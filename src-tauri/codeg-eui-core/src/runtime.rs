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
use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary, SharedModel};
use crate::{
    EuiBootstrap, CODEG_EUI_COMMAND_QUEUE_CAPACITY, CODEG_EUI_ERR_INTERNAL,
    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_QUEUE_FULL,
};

pub(crate) type CoreFuture = Pin<Box<dyn Future<Output = Result<CoreResult, String>> + Send>>;

pub(crate) struct CoreResult {
    payload: Vec<u8>,
    update: Option<ModelUpdate>,
}

impl CoreResult {
    fn json(payload: Vec<u8>) -> Self {
        Self {
            payload,
            update: None,
        }
    }
}

pub(crate) trait CoreOps: Send + Sync {
    fn begin_selection(&self, selection_epoch: u64, op: Operation);
    fn set_workspace(&self, selection_epoch: u64, path: Vec<u8>) -> CoreFuture;
    fn create_session(&self, selection_epoch: u64, agent: Vec<u8>) -> CoreFuture;
    fn select_session(&self, selection_epoch: u64, conversation_id: i32) -> CoreFuture;
    fn send_user_message(&self, text: Vec<u8>) -> CoreFuture;
    fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture;
    fn set_agent_settings(&self, agent: Vec<u8>, json: Vec<u8>) -> CoreFuture;
    fn probe_agent(&self, agent: Vec<u8>) -> CoreFuture;
}

struct AppCoreOps {
    state: Arc<codeg_lib::app_state::AppState>,
    context: Arc<Mutex<AppCommandContext>>,
}

#[derive(Default)]
struct AppCommandContext {
    selection_epoch: u64,
    workspace: Option<codeg_lib::commands::eui_facade::EuiWorkspace>,
    selection: Option<codeg_lib::commands::eui_facade::EuiSessionSelection>,
}

impl CoreOps for AppCoreOps {
    fn begin_selection(&self, selection_epoch: u64, op: Operation) {
        let mut context = self
            .context
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        context.selection_epoch = selection_epoch;
        context.selection = None;
        if op == Operation::SetWorkspace {
            context.workspace = None;
        }
    }

    fn set_workspace(&self, selection_epoch: u64, path: Vec<u8>) -> CoreFuture {
        let state = Arc::clone(&self.state);
        let context = Arc::clone(&self.context);
        Box::pin(async move {
            let path = String::from_utf8(path).map_err(|_| "workspace is not UTF-8".to_string())?;
            let workspace = codeg_lib::commands::eui_facade::set_eui_workspace(
                &state,
                std::path::PathBuf::from(path),
            )
            .await
            .map_err(|error| error.to_string())?;
            let payload = serde_json::to_vec(&workspace).map_err(|error| error.to_string())?;
            let sessions = owned_sessions(&workspace.sessions);
            let mut current = context.lock().unwrap_or_else(|error| error.into_inner());
            if selection_epoch == current.selection_epoch {
                current.selection_epoch = selection_epoch;
                current.workspace = Some(workspace);
                current.selection = None;
            }
            Ok(CoreResult {
                payload,
                update: Some(ModelUpdate::Workspace { sessions }),
            })
        })
    }

    fn create_session(&self, selection_epoch: u64, agent: Vec<u8>) -> CoreFuture {
        let state = Arc::clone(&self.state);
        let context = Arc::clone(&self.context);
        Box::pin(async move {
            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
                .map_err(|error| error.to_string())?;
            let workspace = context
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .workspace
                .clone()
                .ok_or_else(|| "no EUI workspace is selected".to_string())?;
            let selection =
                codeg_lib::commands::eui_facade::create_eui_session(&state, &workspace, agent)
                    .await
                    .map_err(|error| error.to_string())?;
            selection_result(context, selection_epoch, workspace, selection)
        })
    }

    fn select_session(&self, selection_epoch: u64, conversation_id: i32) -> CoreFuture {
        let state = Arc::clone(&self.state);
        let context = Arc::clone(&self.context);
        Box::pin(async move {
            let workspace = context
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .workspace
                .clone()
                .ok_or_else(|| "no EUI workspace is selected".to_string())?;
            let selection = codeg_lib::commands::eui_facade::select_eui_session(
                &state,
                &workspace,
                conversation_id,
            )
            .await
            .map_err(|error| error.to_string())?;
            selection_result(context, selection_epoch, workspace, selection)
        })
    }

    fn send_user_message(&self, text: Vec<u8>) -> CoreFuture {
        let state = Arc::clone(&self.state);
        let context = Arc::clone(&self.context);
        Box::pin(async move {
            let text = String::from_utf8(text).map_err(|_| "message is not UTF-8".to_string())?;
            let selection = context
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .selection
                .clone()
                .ok_or_else(|| "no EUI session is selected".to_string())?;
            codeg_lib::commands::eui_facade::send_eui_message(&state, &selection, text)
                .await
                .map_err(|error| error.to_string())?;
            Ok(CoreResult::json(Vec::new()))
        })
    }

    fn get_agent_settings(&self, agent: Vec<u8>) -> CoreFuture {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
                .map_err(|error| error.to_string())?;
            let settings = codeg_lib::commands::eui_facade::get_eui_agent_settings(&state, agent)
                .await
                .map_err(|error| error.to_string())?;
            serde_json::to_vec(&settings)
                .map(CoreResult::json)
                .map_err(|error| error.to_string())
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
            serde_json::to_vec(&settings)
                .map(CoreResult::json)
                .map_err(|error| error.to_string())
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
            serde_json::to_vec(&probe)
                .map(CoreResult::json)
                .map_err(|error| error.to_string())
        })
    }
}

fn selection_result(
    context: Arc<Mutex<AppCommandContext>>,
    selection_epoch: u64,
    mut workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
    selection: codeg_lib::commands::eui_facade::EuiSessionSelection,
) -> Result<CoreResult, String> {
    let summary = codeg_lib::commands::eui_facade::EuiSessionSummary {
        conversation_id: selection.conversation_id,
        title: selection.title.clone(),
        agent_type: selection.agent_type,
        status: selection.status.clone(),
        external_session_id: selection.external_session_id.clone(),
        updated_at_ms: selection.updated_at_ms,
    };
    if let Some(existing) = workspace
        .sessions
        .iter_mut()
        .find(|item| item.conversation_id == summary.conversation_id)
    {
        *existing = summary;
    } else {
        workspace.sessions.insert(0, summary);
    }
    let payload = serde_json::to_vec(&selection).map_err(|error| error.to_string())?;
    let transcript_json =
        serde_json::to_vec(&selection.transcript).map_err(|error| error.to_string())?;
    let sessions = owned_sessions(&workspace.sessions);
    let connection_id = selection.connection_id.as_bytes().to_vec();
    let mut current = context.lock().unwrap_or_else(|error| error.into_inner());
    if selection_epoch == current.selection_epoch {
        current.selection_epoch = selection_epoch;
        current.workspace = Some(workspace);
        current.selection = Some(selection);
    }
    Ok(CoreResult {
        payload,
        update: Some(ModelUpdate::Selection {
            sessions,
            connection_id,
            transcript_json,
        }),
    })
}

fn owned_sessions(
    sessions: &[codeg_lib::commands::eui_facade::EuiSessionSummary],
) -> Vec<OwnedSessionSummary> {
    sessions
        .iter()
        .map(|session| OwnedSessionSummary {
            conversation_id: session.conversation_id,
            title: session.title.clone().unwrap_or_default().into_bytes(),
            agent: session.agent_type.as_wire().as_bytes().to_vec(),
            updated_at_ms: session.updated_at_ms,
        })
        .collect()
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
    core_ops: Arc<dyn CoreOps>,
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
            context: Arc::new(Mutex::new(AppCommandContext::default())),
        });
        let worker = bootstrap.runtime_handle().spawn(run_worker(
            command_rx,
            shutdown_rx,
            model.clone(),
            connections,
            Arc::clone(&admission),
            Arc::clone(&quiesced),
            Arc::clone(&core_ops),
        ));

        Self {
            bootstrap,
            model,
            command_tx: Some(command_tx),
            core_ops,
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
        let selection_epoch = model.selection_epoch();
        if op.changes_selection() {
            self.core_ops.begin_selection(selection_epoch, op);
        }
        permit.send(RuntimeCommand {
            request_id,
            selection_epoch,
            op,
            payload,
        });
        if op == Operation::SendUserMessage {
            model.record_send_accepted(native_timestamp_ns());
        }
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

fn native_timestamp_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
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
                let abort = tasks.spawn(execute_command(
                    command.selection_epoch,
                    command.op,
                    command.payload,
                    Arc::clone(&core_ops),
                ));
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
    completed: Option<Result<(Id, Result<CoreResult, String>), tokio::task::JoinError>>,
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
    match result {
        Ok(result) => model.terminalize_with_update(
            command.selection_epoch,
            OwnedCompletion::ok(command.request_id, command.op, result.payload),
            result.update,
        ),
        Err(error) => model.terminalize(
            command.selection_epoch,
            OwnedCompletion::error(command.request_id, command.op, error),
        ),
    }
}

async fn execute_command(
    selection_epoch: u64,
    op: Operation,
    payload: CommandPayload,
    core_ops: Arc<dyn CoreOps>,
) -> Result<CoreResult, String> {
    match payload {
        #[cfg(feature = "ffi-test-hooks")]
        CommandPayload::Blocked => pending().await,
        #[cfg(test)]
        CommandPayload::Error(error) => Err(error),
        #[cfg(test)]
        CommandPayload::Panic => panic!("test worker panic"),
        CommandPayload::Empty => Err("operation is not implemented in Task 5".to_string()),
        CommandPayload::Utf8(value) => match op {
            Operation::SetWorkspace => core_ops.set_workspace(selection_epoch, value).await,
            Operation::CreateSession => core_ops.create_session(selection_epoch, value).await,
            Operation::SendUserMessage => core_ops.send_user_message(value).await,
            Operation::GetAgentSettings => core_ops.get_agent_settings(value).await,
            Operation::ProbeAgent => core_ops.probe_agent(value).await,
            _ => Err("invalid UTF-8 command payload".to_string()),
        },
        CommandPayload::SelectSession(conversation_id) => {
            core_ops
                .select_session(selection_epoch, conversation_id)
                .await
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
        CoreResult,
    };
    use crate::commands::{CommandPayload, Operation, RuntimeCommand};
    use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary};
    use crate::{CompletionStatus, LifecycleState, SharedModel};

    struct ErrorOps;

    impl CoreOps for ErrorOps {
        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}

        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected workspace".to_string()) })
        }

        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected create".to_string()) })
        }

        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
            Box::pin(async { Err("unexpected select".to_string()) })
        }

        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected send".to_string()) })
        }

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
        let error = execute_command(
            0,
            Operation::SendUserMessage,
            CommandPayload::Error("expected".to_string()),
            Arc::new(ErrorOps),
        )
        .await
        .err();
        assert_eq!(error.as_deref(), Some("expected"));
    }

    #[tokio::test]
    async fn worker_panics_are_visible_to_the_join_boundary() {
        let joined = tokio::spawn(execute_command(
            0,
            Operation::SendUserMessage,
            CommandPayload::Panic,
            Arc::new(ErrorOps),
        ))
        .await;
        let error = match joined {
            Err(error) => error,
            Ok(_) => panic!("worker panic must be caught by join"),
        };
        assert!(error.is_panic());
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
                0,
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
        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}

        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected workspace".to_string()) })
        }

        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected create".to_string()) })
        }

        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
            Box::pin(async { Err("unexpected select".to_string()) })
        }

        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected send".to_string()) })
        }

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
                Ok(CoreResult::json(br#"{"launchable":true}"#.to_vec()))
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

    struct SlowCreateOps {
        started: Arc<Notify>,
        gate: Arc<Notify>,
    }

    impl CoreOps for SlowCreateOps {
        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}

        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected workspace".to_string()) })
        }

        fn create_session(&self, _selection_epoch: u64, _agent: Vec<u8>) -> CoreFuture {
            let started = Arc::clone(&self.started);
            let gate = Arc::clone(&self.gate);
            Box::pin(async move {
                started.notify_one();
                gate.notified().await;
                Ok(CoreResult {
                    payload: br#"{"conversationId":7,"connectionId":"old"}"#.to_vec(),
                    update: Some(ModelUpdate::Selection {
                        sessions: vec![OwnedSessionSummary {
                            conversation_id: 7,
                            title: b"Old".to_vec(),
                            agent: b"codex".to_vec(),
                            updated_at_ms: 1,
                        }],
                        connection_id: b"old".to_vec(),
                        transcript_json: b"[]".to_vec(),
                    }),
                })
            })
        }

        fn select_session(&self, _selection_epoch: u64, _conversation_id: i32) -> CoreFuture {
            Box::pin(async { Err("unexpected select".to_string()) })
        }

        fn send_user_message(&self, _text: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected send".to_string()) })
        }

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
    async fn selection_change_marks_slow_create_stale_once_without_applying_it() {
        let started = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());
        let model = SharedModel::new();
        let create_id = NonZeroU64::new(51).unwrap();
        model
            .reserve(create_id, Operation::CreateSession, 0)
            .unwrap();
        let create_epoch = model.selection_epoch();
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
            Arc::new(SlowCreateOps {
                started: Arc::clone(&started),
                gate: Arc::clone(&gate),
            }),
        ));
        command_tx
            .send(RuntimeCommand {
                request_id: create_id,
                selection_epoch: create_epoch,
                op: Operation::CreateSession,
                payload: CommandPayload::Utf8(b"codex".to_vec()),
            })
            .await
            .unwrap();
        started.notified().await;

        let newer_id = NonZeroU64::new(52).unwrap();
        model
            .reserve(newer_id, Operation::SelectSession, model.selection_epoch())
            .unwrap();
        let newer_epoch = model.selection_epoch();
        gate.notify_one();

        let mut create_completions = 0;
        for generation in 1..=100 {
            tokio::task::yield_now().await;
            let (frame, _) = model.build_frame(false, &quiesced);
            let abi = frame.as_abi(LifecycleState::Running, generation, false);
            assert_eq!(abi.connection_id.len, 0);
            assert_eq!(abi.transcript_json.len, 0);
            let completions = if abi.completions_len == 0 {
                &[][..]
            } else {
                unsafe { std::slice::from_raw_parts(abi.completions, abi.completions_len) }
            };
            for completion in completions {
                if completion.request_id == create_id.get() {
                    create_completions += 1;
                    assert_eq!(completion.status, CompletionStatus::Stale as u32);
                }
            }
            if create_completions == 1 {
                break;
            }
        }
        assert_eq!(create_completions, 1);

        model.terminalize(
            newer_epoch,
            OwnedCompletion::error(
                newer_id,
                Operation::SelectSession,
                "test cleanup".to_string(),
            ),
        );
        let _ = model.build_frame(false, &quiesced);
        shutdown_tx.send(true).unwrap();
        worker.await.unwrap();
    }
}
