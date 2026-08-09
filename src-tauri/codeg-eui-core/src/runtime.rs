use std::collections::HashMap;
#[cfg(feature = "ffi-test-hooks")]
use std::future::pending;
use std::num::NonZeroU64;
use std::panic::{catch_unwind, AssertUnwindSafe};
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
        let worker = bootstrap.runtime_handle().spawn(run_worker(
            command_rx,
            shutdown_rx,
            model.clone(),
            connections,
            Arc::clone(&admission),
            Arc::clone(&quiesced),
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
                let abort = tasks.spawn(execute_command(command.payload));
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

async fn execute_command(payload: CommandPayload) -> Result<Vec<u8>, String> {
    match payload {
        #[cfg(feature = "ffi-test-hooks")]
        CommandPayload::Blocked => pending().await,
        #[cfg(test)]
        CommandPayload::Error(error) => Err(error),
        #[cfg(test)]
        CommandPayload::Panic => panic!("test worker panic"),
        CommandPayload::Empty => Err("operation is not implemented in Task 3".to_string()),
        CommandPayload::Utf8(value) => {
            let _ = value;
            Err("operation is not implemented in Task 3".to_string())
        }
        CommandPayload::SelectSession(conversation_id) => {
            let _ = conversation_id;
            Err("operation is not implemented in Task 3".to_string())
        }
        CommandPayload::AgentSettings { agent, json } => {
            let _ = (agent, json);
            Err("operation is not implemented in Task 3".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::num::NonZeroU64;
    use std::sync::atomic::AtomicBool;

    use tokio::task::JoinSet;

    use super::{execute_command, terminalize_task, CommandMetadata};
    use crate::commands::{CommandPayload, Operation};
    use crate::{CompletionStatus, LifecycleState, SharedModel};

    #[tokio::test]
    async fn worker_errors_are_terminal_results() {
        assert_eq!(
            execute_command(CommandPayload::Error("expected".to_string())).await,
            Err("expected".to_string())
        );
    }

    #[tokio::test]
    async fn worker_panics_are_visible_to_the_join_boundary() {
        let joined = tokio::spawn(execute_command(CommandPayload::Panic)).await;
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
            let abort = tasks.spawn(execute_command(payload));
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
}
