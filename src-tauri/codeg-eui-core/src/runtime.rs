use std::collections::HashMap;
#[cfg(feature = "ffi-test-hooks")]
use std::future::pending;
use std::future::Future;
use std::num::NonZeroU64;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use codeg_lib::acp::manager::ConnectionManager;
use codeg_lib::acp::termination::AcpDisconnectOrigin;
use tokio::sync::{mpsc, watch};
use tokio::task::{Id, JoinHandle, JoinSet};

use crate::commands::{CommandPayload, Operation, RuntimeCommand};
use crate::model::{OwnedCompletion, SharedModel};
use crate::{
    EuiBootstrap, CODEG_EUI_COMMAND_QUEUE_CAPACITY, CODEG_EUI_ERR_INTERNAL,
    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_QUEUE_FULL,
};

pub(crate) type CoreFuture = Pin<Box<dyn Future<Output = Result<Vec<u8>, String>> + Send>>;

pub(crate) trait CoreOps: Send + Sync {
    fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture;
    fn set_agent_settings(&self, agent: Vec<u8>, json: Vec<u8>) -> CoreFuture;
    fn probe_agent(&self, agent: Vec<u8>) -> CoreFuture;
}

struct AppCoreOps {
    state: Arc<codeg_lib::app_state::AppState>,
}

impl CoreOps for AppCoreOps {
    fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
                .map_err(|error| error.to_string())?;
            let settings = codeg_lib::commands::eui_facade::get_eui_agent_settings(&state, agent)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_vec(&settings).map_err(|error| error.to_string())
        })
    }

    fn set_agent_settings(&self, agent: Vec<u8>, json: Vec<u8>) -> CoreFuture {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
                .map_err(|error| error.to_string())?;
            let patch = serde_json::from_slice::<
                codeg_lib::commands::eui_facade::EuiAgentSettingsPatch,
            >(&json)
            .map_err(|error| format!("invalid agent settings patch: {error}"))?;
            let settings =
                codeg_lib::commands::eui_facade::set_eui_agent_settings(&state, agent, patch)
                    .await
                    .map_err(|error| error.to_string())?;
            serde_json::to_vec(&settings).map_err(|error| error.to_string())
        })
    }

    fn probe_agent(&self, agent: Vec<u8>) -> CoreFuture {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
                .map_err(|error| error.to_string())?;
            let probe = codeg_lib::commands::eui_facade::probe_eui_agent(&state, agent)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_vec(&probe).map_err(|error| error.to_string())
        })
    }
}

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy)]
struct CommandMetadata {
    request_id: NonZeroU64,
    selection_epoch: u64,
    op: Operation,
}

struct WorkerExitGuard {
    model: SharedModel,
    admission: Arc<Mutex<()>>,
    quiesced: Arc<AtomicBool>,
}

impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _ = catch_unwind(AssertUnwindSafe(|| self.model.cancel_all()));
        self.quiesced.store(true, Ordering::Release);
    }
}

pub(crate) struct RuntimeOwner {
    bootstrap: EuiBootstrap,
    model: SharedModel,
    command_tx: Option<mpsc::Sender<RuntimeCommand>>,
    shutdown_tx: Option<watch::Sender<bool>>,
    worker: JoinHandle<()>,
    admission: Arc<Mutex<()>>,
    quiesced: Arc<AtomicBool>,
}

impl RuntimeOwner {
    pub(crate) fn start(bootstrap: EuiBootstrap, model: SharedModel) -> Self {
        let (command_tx, command_rx) = mpsc::channel(CODEG_EUI_COMMAND_QUEUE_CAPACITY);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let admission = Arc::new(Mutex::new(()));
        let quiesced = Arc::new(AtomicBool::new(false));
        let connections = bootstrap.state.connection_manager.clone_ref();
        let core_ops: Arc<dyn CoreOps> = Arc::new(AppCoreOps {
            state: Arc::clone(&bootstrap.state),
        });
        let worker = bootstrap.runtime_handle().spawn(run_worker(
            command_rx,
            shutdown_rx,
            model.clone(),
            connections,
            Arc::clone(&admission),
            Arc::clone(&quiesced),
            core_ops,
        ));

        Self {
            bootstrap,
            model,
            command_tx: Some(command_tx),
            shutdown_tx: Some(shutdown_tx),
            worker,
            admission,
            quiesced,
        }
    }

    pub(crate) fn enqueue(
        &self,
        model: &SharedModel,
        op: Operation,
        payload: CommandPayload,
    ) -> Result<NonZeroU64, i32> {
        let _admission = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.quiesced.load(Ordering::Acquire) {
            return Err(CODEG_EUI_ERR_INTERNAL);
        }
        if self.worker.is_finished() {
            return Err(CODEG_EUI_ERR_INTERNAL);
        }
        let sender = self
            .command_tx
            .as_ref()
            .ok_or(CODEG_EUI_ERR_INVALID_STATE)?;
        let permit = sender.try_reserve().map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => CODEG_EUI_ERR_QUEUE_FULL,
            mpsc::error::TrySendError::Closed(_) => CODEG_EUI_ERR_INVALID_STATE,
        })?;
        let request_id = next_request_id()?;
        let selection_epoch = model.selection_epoch();
        model.reserve(request_id, op, selection_epoch)?;
        permit.send(RuntimeCommand {
            request_id,
            selection_epoch,
            op,
            payload,
        });
        Ok(request_id)
    }

    pub(crate) fn begin_shutdown(&mut self) {
        self.command_tx.take();
        if self
            .shutdown_tx
            .take()
            .is_some_and(|shutdown| shutdown.send(true).is_err())
        {
            self.model.cancel_all();
            self.quiesced.store(true, Ordering::Release);
        }
    }

    pub(crate) fn quiesced_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.quiesced)
    }

    pub(crate) fn join(self) {
        let Self {
            bootstrap, worker, ..
        } = self;
        drop(worker);
        bootstrap.shutdown();
    }
}

fn next_request_id() -> Result<NonZeroU64, i32> {
    let value = NEXT_REQUEST_ID
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(1)
        })
        .map_err(|_| CODEG_EUI_ERR_INTERNAL)?;
    NonZeroU64::new(value).ok_or(CODEG_EUI_ERR_INTERNAL)
}

async fn run_worker(
    mut commands: mpsc::Receiver<RuntimeCommand>,
    mut shutdown: watch::Receiver<bool>,
    model: SharedModel,
    connections: ConnectionManager,
    admission: Arc<Mutex<()>>,
    quiesced: Arc<AtomicBool>,
    core_ops: Arc<dyn CoreOps>,
) {
    let _exit_guard = WorkerExitGuard {
        model: model.clone(),
        admission,
        quiesced,
    };
    let mut tasks = JoinSet::new();
    let mut metadata = HashMap::<Id, CommandMetadata>::new();

    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            completed = tasks.join_next_with_id(), if !tasks.is_empty() => {
                terminalize_task(&model, &mut metadata, completed);
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                let command_metadata = CommandMetadata {
                    request_id: command.request_id,
                    selection_epoch: command.selection_epoch,
                    op: command.op,
                };
                let abort = tasks.spawn(execute_command(command.op, command.payload, Arc::clone(&core_ops)));
                metadata.insert(abort.id(), command_metadata);
            }
        }
    }

    commands.close();
    tasks.abort_all();
    while let Some(completed) = tasks.join_next_with_id().await {
        if let Ok((id, _)) = &completed {
            metadata.remove(id);
        } else if let Err(error) = &completed {
            metadata.remove(&error.id());
        }
    }
    metadata.clear();
    while commands.try_recv().is_ok() {}
    model.cancel_all();
    connections
        .disconnect_all(AcpDisconnectOrigin::ApplicationShutdown)
        .await;
}

fn terminalize_task(
    model: &SharedModel,
    metadata: &mut HashMap<Id, CommandMetadata>,
    completed: Option<Result<(Id, Result<Vec<u8>, String>), tokio::task::JoinError>>,
) {
    let Some(completed) = completed else {
        return;
    };
    let (task_id, result) = match completed {
        Ok((task_id, result)) => (task_id, result),
        Err(error) => {
            let task_id = error.id();
            (task_id, Err(format!("worker panic: {error}")))
        }
    };
    let command = metadata
        .remove(&task_id)
        .expect("metadata exists for every worker task");
    let completion = match result {
        Ok(payload) => OwnedCompletion::ok(command.request_id, command.op, payload),
        Err(error) => OwnedCompletion::error(command.request_id, command.op, error),
    };
    model.terminalize(command.selection_epoch, completion);
}

async fn execute_command(
    op: Operation,
    payload: CommandPayload,
    core_ops: Arc<dyn CoreOps>,
) -> Result<Vec<u8>, String> {
    match payload {
        #[cfg(feature = "ffi-test-hooks")]
        CommandPayload::Blocked => pending().await,
        #[cfg(test)]
        CommandPayload::Error(error) => Err(error),
        #[cfg(test)]
        CommandPayload::Panic => panic!("test worker panic"),
        CommandPayload::Empty => Err("operation is not implemented in Task 3".to_string()),
        CommandPayload::Utf8(value) => match op {
            Operation::GetAgentSettings => core_ops.get_agent_settings(value).await,
            Operation::ProbeAgent => core_ops.probe_agent(value).await,
            _ => Err("operation is not implemented in Task 3".to_string()),
        },
        CommandPayload::SelectSession(conversation_id) => {
            let _ = conversation_id;
            Err("operation is not implemented in Task 3".to_string())
        }
        CommandPayload::AgentSettings { agent, json } => {
            if op != Operation::SetAgentSettings {
                return Err("invalid settings operation".to_string());
            }
            core_ops.set_agent_settings(agent, json).await
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::num::NonZeroU64;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use tokio::sync::{mpsc, watch, Notify};
    use tokio::task::JoinSet;

    use super::{
        execute_command, run_worker, terminalize_task, CommandMetadata, CoreFuture, CoreOps,
    };
    use crate::commands::{CommandPayload, Operation, RuntimeCommand};
    use crate::{CompletionStatus, LifecycleState, SharedModel};

    struct ErrorOps;

    impl CoreOps for ErrorOps {
        fn get_agent_settings(&self, _agent: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected get".to_string()) })
        }

        fn set_agent_settings(&self, _agent: Vec<u8>, _json: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected set".to_string()) })
        }

        fn probe_agent(&self, _agent: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected probe".to_string()) })
        }
    }

    #[tokio::test]
    async fn worker_errors_are_terminal_results() {
        assert_eq!(
            execute_command(
                Operation::SendUserMessage,
                CommandPayload::Error("expected".to_string()),
                Arc::new(ErrorOps),
            )
            .await,
            Err("expected".to_string())
        );
    }

    #[tokio::test]
    async fn worker_panics_are_visible_to_the_join_boundary() {
        let joined = tokio::spawn(execute_command(
            Operation::SendUserMessage,
            CommandPayload::Panic,
            Arc::new(ErrorOps),
        ))
        .await;
        assert!(joined
            .expect_err("worker panic must be caught by join")
            .is_panic());
    }

    #[tokio::test]
    async fn worker_error_and_panic_each_terminalize_once() {
        let model = SharedModel::new();
        let mut metadata = HashMap::new();
        let cases = [
            (
                NonZeroU64::new(1).unwrap(),
                CommandPayload::Error("expected error".to_string()),
            ),
            (NonZeroU64::new(2).unwrap(), CommandPayload::Panic),
        ];

        for (request_id, payload) in cases {
            model
                .reserve(request_id, Operation::SendUserMessage, 0)
                .unwrap();
            let mut tasks = JoinSet::new();
            let abort = tasks.spawn(execute_command(
                Operation::SendUserMessage,
                payload,
                Arc::new(ErrorOps),
            ));
            metadata.insert(
                abort.id(),
                CommandMetadata {
                    request_id,
                    selection_epoch: 0,
                    op: Operation::SendUserMessage,
                },
            );
            terminalize_task(&model, &mut metadata, tasks.join_next_with_id().await);
        }

        let (owned, ready) = model.build_frame(false, &AtomicBool::new(false));
        assert!(!ready);
        let frame = owned.as_abi(LifecycleState::Running, 1, false);
        let completions =
            unsafe { std::slice::from_raw_parts(frame.completions, frame.completions_len) };
        assert_eq!(completions.len(), 2);
        assert!(completions
            .iter()
            .all(|completion| completion.status == CompletionStatus::Error as u32));
        assert!(completions
            .iter()
            .all(|completion| completion.error.len > 0));
    }

    struct SlowProbeOps {
        gate: Arc<Notify>,
    }

    impl CoreOps for SlowProbeOps {
        fn get_agent_settings(&self, _agent: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected get".to_string()) })
        }

        fn set_agent_settings(&self, _agent: Vec<u8>, _json: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected set".to_string()) })
        }

        fn probe_agent(&self, _agent: Vec<u8>) -> CoreFuture {
            let gate = Arc::clone(&self.gate);
            Box::pin(async move {
                gate.notified().await;
                Ok(br#"{"launchable":true}"#.to_vec())
            })
        }
    }

    #[tokio::test]
    async fn slow_probe_does_not_block_frame_build_and_completes_once() {
        let gate = Arc::new(Notify::new());
        let model = SharedModel::new();
        let request_id = NonZeroU64::new(41).unwrap();
        model.reserve(request_id, Operation::ProbeAgent, 0).unwrap();
        let (command_tx, command_rx) = mpsc::channel(4);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let quiesced = Arc::new(AtomicBool::new(false));
        let worker = tokio::spawn(run_worker(
            command_rx,
            shutdown_rx,
            model.clone(),
            codeg_lib::acp::manager::ConnectionManager::new(),
            Arc::new(std::sync::Mutex::new(())),
            Arc::clone(&quiesced),
            Arc::new(SlowProbeOps {
                gate: Arc::clone(&gate),
            }),
        ));
        command_tx
            .send(RuntimeCommand {
                request_id,
                selection_epoch: 0,
                op: Operation::ProbeAgent,
                payload: CommandPayload::Utf8(b"codex".to_vec()),
            })
            .await
            .unwrap();

        let (first, _) = model.build_frame(false, &quiesced);
        let first_abi = first.as_abi(LifecycleState::Running, 1, false);
        assert_eq!(first_abi.completions_len, 0);

        gate.notify_one();
        let mut completions_seen = 0;
        for generation in 2..=100 {
            tokio::task::yield_now().await;
            let (frame, _) = model.build_frame(false, &quiesced);
            let abi = frame.as_abi(LifecycleState::Running, generation, false);
            completions_seen += abi.completions_len;
            if abi.completions_len == 1 {
                let completion = unsafe { &*abi.completions };
                assert_eq!(completion.request_id, request_id.get());
                assert_eq!(completion.op, Operation::ProbeAgent as u32);
                assert_eq!(completion.status, CompletionStatus::Ok as u32);
                break;
            }
        }
        assert_eq!(completions_seen, 1);
        shutdown_tx.send(true).unwrap();
        worker.await.unwrap();
    }
}
