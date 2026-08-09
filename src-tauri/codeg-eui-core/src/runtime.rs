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
use crate::live::{AppLiveBackend, LiveBackend, LiveError, LiveProjector};
use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary, SharedModel};
use crate::perf::native_timestamp_ns;
use crate::{
    EuiBootstrap, CODEG_EUI_COMMAND_QUEUE_CAPACITY, CODEG_EUI_ERR_INTERNAL,
    CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_QUEUE_FULL,
};

pub(crate) type CoreFuture = Pin<Box<dyn Future<Output = Result<CoreResult, String>> + Send>>;

pub(crate) struct CoreResult {
    payload: Vec<u8>,
    update: Option<ModelUpdate>,
    live_connection_id: Option<String>,
    sent_user_text: Option<Vec<u8>>,
}

impl CoreResult {
    fn json(payload: Vec<u8>) -> Self {
        Self {
            payload,
            update: None,
            live_connection_id: None,
            sent_user_text: None,
        }
    }
}

pub(crate) trait CoreOps: Send + Sync {
    fn capture_context(&self, _selection_epoch: u64, _op: Operation) -> CommandContext {
        CommandContext::None
    }
    fn begin_selection(&self, selection_epoch: u64, op: Operation);
    fn set_workspace(&self, selection_epoch: u64, path: Vec<u8>) -> CoreFuture;
    fn create_session(
        &self,
        selection_epoch: u64,
        workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
        agent: Vec<u8>,
    ) -> CoreFuture;
    fn select_session(
        &self,
        selection_epoch: u64,
        workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
        conversation_id: i32,
    ) -> CoreFuture;
    fn send_user_message(
        &self,
        selection: codeg_lib::commands::eui_facade::EuiSessionSelection,
        text: Vec<u8>,
    ) -> CoreFuture;
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

pub(crate) enum CommandContext {
    None,
    Workspace(codeg_lib::commands::eui_facade::EuiWorkspace),
    Selection(codeg_lib::commands::eui_facade::EuiSessionSelection),
    Unavailable(String),
}

fn capture_command_context(
    context: &Arc<Mutex<AppCommandContext>>,
    selection_epoch: u64,
    op: Operation,
) -> CommandContext {
    let current = context.lock().unwrap_or_else(|error| error.into_inner());
    if current.selection_epoch != selection_epoch {
        return CommandContext::Unavailable(
            "EUI selection changed before command context was captured".to_string(),
        );
    }
    match op {
        Operation::CreateSession | Operation::SelectSession => current
            .workspace
            .clone()
            .map(CommandContext::Workspace)
            .unwrap_or_else(|| {
                CommandContext::Unavailable("no EUI workspace is selected".to_string())
            }),
        Operation::SendUserMessage => current
            .selection
            .clone()
            .map(CommandContext::Selection)
            .unwrap_or_else(|| {
                CommandContext::Unavailable("no EUI session is selected".to_string())
            }),
        _ => CommandContext::None,
    }
}

impl CoreOps for AppCoreOps {
    fn capture_context(&self, selection_epoch: u64, op: Operation) -> CommandContext {
        capture_command_context(&self.context, selection_epoch, op)
    }

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
                live_connection_id: None,
                sent_user_text: None,
            })
        })
    }

    fn create_session(
        &self,
        selection_epoch: u64,
        workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
        agent: Vec<u8>,
    ) -> CoreFuture {
        let state = Arc::clone(&self.state);
        let context = Arc::clone(&self.context);
        Box::pin(async move {
            let wire = String::from_utf8(agent).map_err(|_| "agent is not UTF-8".to_string())?;
            let agent = codeg_lib::commands::eui_facade::parse_supported_agent(&wire)
                .map_err(|error| error.to_string())?;
            let selection =
                codeg_lib::commands::eui_facade::create_eui_session(&state, &workspace, agent)
                    .await
                    .map_err(|error| error.to_string())?;
            selection_result(context, selection_epoch, workspace, selection)
        })
    }

    fn select_session(
        &self,
        selection_epoch: u64,
        workspace: codeg_lib::commands::eui_facade::EuiWorkspace,
        conversation_id: i32,
    ) -> CoreFuture {
        let state = Arc::clone(&self.state);
        let context = Arc::clone(&self.context);
        Box::pin(async move {
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

    fn send_user_message(
        &self,
        selection: codeg_lib::commands::eui_facade::EuiSessionSelection,
        text: Vec<u8>,
    ) -> CoreFuture {
        let state = Arc::clone(&self.state);
        Box::pin(async move {
            let text = String::from_utf8(text).map_err(|_| "message is not UTF-8".to_string())?;
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
    let live_connection_id = selection.connection_id.clone();
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
        live_connection_id: Some(live_connection_id),
        sent_user_text: None,
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
        let live_backend: Arc<dyn LiveBackend> =
            Arc::new(AppLiveBackend::new(Arc::clone(&bootstrap.state)));
        let worker = bootstrap.runtime_handle().spawn(run_worker(
            command_rx,
            shutdown_rx,
            model.clone(),
            connections,
            Arc::clone(&admission),
            Arc::clone(&quiesced),
            Arc::clone(&core_ops),
            Some(live_backend),
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
        let context = self.core_ops.capture_context(selection_epoch, op);
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
            context,
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
    live_backend: Option<Arc<dyn LiveBackend>>,
) {
    let _exit_guard = WorkerExitGuard {
        model: model.clone(),
        admission,
        quiesced,
    };
    let mut tasks = JoinSet::new();
    let mut metadata = HashMap::<Id, CommandMetadata>::new();
    let mut live_task: Option<JoinHandle<()>> = None;

    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            completed = tasks.join_next_with_id(), if !tasks.is_empty() => {
                if let Some(selection) = terminalize_task(&model, &mut metadata, completed) {
                    if let Some(task) = live_task.take() {
                        task.abort();
                    }
                    if let Some(backend) = live_backend.as_ref() {
                        let backend = Arc::clone(backend);
                        let live_model = model.clone();
                        live_task = Some(tokio::spawn(async move {
                            let projector = LiveProjector::new(backend, live_model.clone());
                            match projector
                                .attach(&selection.connection_id, selection.selection_epoch)
                                .await
                            {
                                Ok(attachment) => attachment.run().await,
                                Err(LiveError::SelectionChanged) => {}
                                Err(error) => {
                                    let _ = live_model.set_live_error(
                                        selection.selection_epoch,
                                        &selection.connection_id,
                                        error.to_string(),
                                        native_timestamp_ns(),
                                    );
                                }
                            }
                        }));
                    }
                }
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
                    command.context,
                    Arc::clone(&core_ops),
                ));
                metadata.insert(abort.id(), command_metadata);
            }
        }
    }

    commands.close();
    if let Some(task) = live_task {
        task.abort();
        let _ = task.await;
    }
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

struct LiveSelection {
    connection_id: String,
    selection_epoch: u64,
}

fn terminalize_task(
    model: &SharedModel,
    metadata: &mut HashMap<Id, CommandMetadata>,
    completed: Option<Result<(Id, Result<CoreResult, String>), tokio::task::JoinError>>,
) -> Option<LiveSelection> {
    let Some(completed) = completed else {
        return None;
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
        Ok(result) => {
            if let Some(text) = result.sent_user_text.clone() {
                let _ =
                    model.record_sent_user_turn(command.selection_epoch, command.request_id, text);
            }
            let live_connection_id = result.live_connection_id;
            let is_current = model.terminalize_with_update(
                command.selection_epoch,
                OwnedCompletion::ok(command.request_id, command.op, result.payload),
                result.update,
            );
            if is_current {
                live_connection_id.map(|connection_id| LiveSelection {
                    connection_id,
                    selection_epoch: command.selection_epoch,
                })
            } else {
                None
            }
        }
        Err(error) => {
            model.terminalize(
                command.selection_epoch,
                OwnedCompletion::error(command.request_id, command.op, error),
            );
            None
        }
    }
}

async fn execute_command(
    selection_epoch: u64,
    op: Operation,
    payload: CommandPayload,
    context: CommandContext,
    core_ops: Arc<dyn CoreOps>,
) -> Result<CoreResult, String> {
    let context = match context {
        CommandContext::Unavailable(error) => return Err(error),
        context => context,
    };
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
            Operation::CreateSession => {
                let CommandContext::Workspace(workspace) = context else {
                    return Err("create session is missing its admitted workspace".to_string());
                };
                core_ops
                    .create_session(selection_epoch, workspace, value)
                    .await
            }
            Operation::SendUserMessage => {
                let CommandContext::Selection(selection) = context else {
                    return Err("send is missing its admitted session".to_string());
                };
                let sent_user_text = value.clone();
                let mut result = core_ops.send_user_message(selection, value).await?;
                result.sent_user_text = Some(sent_user_text);
                Ok(result)
            }
            Operation::GetAgentSettings => core_ops.get_agent_settings(value).await,
            Operation::ProbeAgent => core_ops.probe_agent(value).await,
            _ => Err("invalid UTF-8 command payload".to_string()),
        },
        CommandPayload::SelectSession(conversation_id) => {
            let CommandContext::Workspace(workspace) = context else {
                return Err("select session is missing its admitted workspace".to_string());
            };
            core_ops
                .select_session(selection_epoch, workspace, conversation_id)
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
    use std::sync::{Arc, Mutex};

    use tokio::sync::{mpsc, watch, Notify};
    use tokio::task::JoinSet;

    use super::{
        capture_command_context, execute_command, run_worker, selection_result, terminalize_task,
        AppCommandContext, CommandContext, CommandMetadata, CoreFuture, CoreOps, CoreResult,
    };
    use crate::commands::{CommandPayload, Operation, RuntimeCommand};
    use crate::model::{ModelUpdate, OwnedCompletion, OwnedSessionSummary};
    use crate::{CompletionStatus, LifecycleState, SharedModel};
    use codeg_lib::commands::eui_facade::{EuiSessionSelection, EuiWorkspace};
    use codeg_lib::models::AgentType;

    fn test_workspace(folder_id: i32, path: &str) -> EuiWorkspace {
        EuiWorkspace {
            folder_id,
            path: std::path::PathBuf::from(path),
            sessions: Vec::new(),
        }
    }

    fn test_selection(
        workspace: &EuiWorkspace,
        conversation_id: i32,
        connection_id: &str,
    ) -> EuiSessionSelection {
        EuiSessionSelection {
            folder_id: workspace.folder_id,
            path: workspace.path.clone(),
            conversation_id,
            title: Some(format!("Session {conversation_id}")),
            agent_type: AgentType::Codex,
            status: "active".to_string(),
            external_session_id: None,
            updated_at_ms: 1,
            connection_id: connection_id.to_string(),
            transcript: Vec::new(),
        }
    }

    #[test]
    fn successful_selection_requests_live_attachment_for_its_connection() {
        let workspace = test_workspace(11, "/workspace");
        let selection = test_selection(&workspace, 101, "connection-live");
        let context = Arc::new(Mutex::new(AppCommandContext {
            selection_epoch: 7,
            workspace: Some(workspace.clone()),
            selection: None,
        }));

        let result = selection_result(context, 7, workspace, selection).unwrap();

        assert_eq!(
            result.live_connection_id.as_deref(),
            Some("connection-live")
        );
    }

    #[test]
    fn accepted_commands_keep_their_original_workspace_and_selection() {
        let workspace_a = test_workspace(11, "/workspace-a");
        let selection_a = test_selection(&workspace_a, 101, "connection-a");
        let context = Arc::new(Mutex::new(AppCommandContext {
            selection_epoch: 7,
            workspace: Some(workspace_a.clone()),
            selection: Some(selection_a),
        }));

        let create_context = capture_command_context(&context, 7, Operation::CreateSession);
        let send_context = capture_command_context(&context, 7, Operation::SendUserMessage);

        let workspace_b = test_workspace(22, "/workspace-b");
        let selection_b = test_selection(&workspace_b, 202, "connection-b");
        *context.lock().unwrap() = AppCommandContext {
            selection_epoch: 8,
            workspace: Some(workspace_b),
            selection: Some(selection_b),
        };

        let CommandContext::Workspace(captured_workspace) = create_context else {
            panic!("create must capture a workspace");
        };
        assert_eq!(captured_workspace.folder_id, 11);
        assert_eq!(
            captured_workspace.path,
            std::path::PathBuf::from("/workspace-a")
        );

        let CommandContext::Selection(captured_selection) = send_context else {
            panic!("send must capture a selection");
        };
        assert_eq!(captured_selection.folder_id, 11);
        assert_eq!(captured_selection.conversation_id, 101);
        assert_eq!(captured_selection.connection_id, "connection-a");
    }

    struct ErrorOps;

    impl CoreOps for ErrorOps {
        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}

        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected workspace".to_string()) })
        }

        fn create_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _agent: Vec<u8>,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected create".to_string()) })
        }

        fn select_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _conversation_id: i32,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected select".to_string()) })
        }

        fn send_user_message(&self, _selection: EuiSessionSelection, _text: Vec<u8>) -> CoreFuture {
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

    struct SuccessSendOps;

    impl CoreOps for SuccessSendOps {
        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}

        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected workspace".to_string()) })
        }

        fn create_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _agent: Vec<u8>,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected create".to_string()) })
        }

        fn select_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _conversation_id: i32,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected select".to_string()) })
        }

        fn send_user_message(&self, _selection: EuiSessionSelection, _text: Vec<u8>) -> CoreFuture {
            Box::pin(async { Ok(CoreResult::json(Vec::new())) })
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
            CommandContext::None,
            Arc::new(ErrorOps),
        )
        .await
        .err();
        assert_eq!(error.as_deref(), Some("expected"));
    }

    #[tokio::test]
    async fn successful_send_projects_user_text_into_the_transcript() {
        let model = SharedModel::new();
        let select_id = NonZeroU64::new(90).unwrap();
        model
            .reserve(select_id, Operation::SelectSession, 0)
            .unwrap();
        let selection_epoch = model.selection_epoch();
        model.terminalize_with_update(
            selection_epoch,
            OwnedCompletion::ok(select_id, Operation::SelectSession, Vec::new()),
            Some(ModelUpdate::Selection {
                sessions: Vec::new(),
                connection_id: b"send-transcript".to_vec(),
                transcript_json: b"[]".to_vec(),
            }),
        );
        let send_id = NonZeroU64::new(91).unwrap();
        model
            .reserve(send_id, Operation::SendUserMessage, selection_epoch)
            .unwrap();
        let mut tasks = JoinSet::new();
        let abort = tasks.spawn(execute_command(
            selection_epoch,
            Operation::SendUserMessage,
            CommandPayload::Utf8(b"kept through lag".to_vec()),
            CommandContext::Selection(test_selection(
                &test_workspace(1, "/workspace"),
                1,
                "send-transcript",
            )),
            Arc::new(SuccessSendOps),
        ));
        let mut metadata = HashMap::from([(
            abort.id(),
            CommandMetadata {
                request_id: send_id,
                selection_epoch,
                op: Operation::SendUserMessage,
            },
        )]);
        terminalize_task(&model, &mut metadata, tasks.join_next_with_id().await);

        let (frame, _) = model.build_frame(false, &AtomicBool::new(false));
        let abi = frame.as_abi(LifecycleState::Running, 1, false);
        let transcript =
            unsafe { std::slice::from_raw_parts(abi.transcript_json.ptr, abi.transcript_json.len) };
        let transcript: serde_json::Value = serde_json::from_slice(transcript).unwrap();
        assert_eq!(transcript[0]["blocks"][0]["text"], "kept through lag");
    }

    #[tokio::test]
    async fn worker_panics_are_visible_to_the_join_boundary() {
        let joined = tokio::spawn(execute_command(
            0,
            Operation::SendUserMessage,
            CommandPayload::Panic,
            CommandContext::None,
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
                CommandContext::None,
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

        fn create_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _agent: Vec<u8>,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected create".to_string()) })
        }

        fn select_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _conversation_id: i32,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected select".to_string()) })
        }

        fn send_user_message(&self, _selection: EuiSessionSelection, _text: Vec<u8>) -> CoreFuture {
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
            None,
        ));
        command_tx
            .send(RuntimeCommand {
                request_id,
                selection_epoch: 0,
                op: Operation::ProbeAgent,
                payload: CommandPayload::Utf8(b"codex".to_vec()),
                context: CommandContext::None,
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

        fn create_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _agent: Vec<u8>,
        ) -> CoreFuture {
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
                    live_connection_id: Some("old".to_string()),
                    sent_user_text: None,
                })
            })
        }

        fn select_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _conversation_id: i32,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected select".to_string()) })
        }

        fn send_user_message(&self, _selection: EuiSessionSelection, _text: Vec<u8>) -> CoreFuture {
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
            None,
        ));
        command_tx
            .send(RuntimeCommand {
                request_id: create_id,
                selection_epoch: create_epoch,
                op: Operation::CreateSession,
                payload: CommandPayload::Utf8(b"codex".to_vec()),
                context: CommandContext::Workspace(test_workspace(1, "/workspace")),
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

    struct SlowBoundSendOps {
        started: Arc<Notify>,
        gate: Arc<Notify>,
        linked: Arc<Mutex<Vec<(String, i32, i32, String)>>>,
    }

    impl CoreOps for SlowBoundSendOps {
        fn begin_selection(&self, _selection_epoch: u64, _op: Operation) {}

        fn set_workspace(&self, _selection_epoch: u64, _path: Vec<u8>) -> CoreFuture {
            Box::pin(async { Err("unexpected workspace".to_string()) })
        }

        fn create_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _agent: Vec<u8>,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected create".to_string()) })
        }

        fn select_session(
            &self,
            _selection_epoch: u64,
            _workspace: EuiWorkspace,
            _conversation_id: i32,
        ) -> CoreFuture {
            Box::pin(async { Err("unexpected select".to_string()) })
        }

        fn send_user_message(&self, selection: EuiSessionSelection, text: Vec<u8>) -> CoreFuture {
            let started = Arc::clone(&self.started);
            let gate = Arc::clone(&self.gate);
            let linked = Arc::clone(&self.linked);
            Box::pin(async move {
                started.notify_one();
                gate.notified().await;
                linked.lock().unwrap().push((
                    selection.connection_id,
                    selection.folder_id,
                    selection.conversation_id,
                    String::from_utf8(text).unwrap(),
                ));
                Ok(CoreResult::json(Vec::new()))
            })
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
    async fn admitted_send_keeps_original_ids_and_terminalizes_stale_once() {
        let started = Arc::new(Notify::new());
        let gate = Arc::new(Notify::new());
        let linked = Arc::new(Mutex::new(Vec::new()));
        let workspace_a = test_workspace(11, "/workspace-a");
        let selection_a = test_selection(&workspace_a, 101, "connection-a");
        let command_context = CommandContext::Selection(selection_a);
        let model = SharedModel::new();
        let send_id = NonZeroU64::new(61).unwrap();
        model
            .reserve(send_id, Operation::SendUserMessage, 0)
            .unwrap();
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
            Arc::new(SlowBoundSendOps {
                started: Arc::clone(&started),
                gate: Arc::clone(&gate),
                linked: Arc::clone(&linked),
            }),
            None,
        ));
        command_tx
            .send(RuntimeCommand {
                request_id: send_id,
                selection_epoch: 0,
                op: Operation::SendUserMessage,
                payload: CommandPayload::Utf8(b"hello".to_vec()),
                context: command_context,
            })
            .await
            .unwrap();
        started.notified().await;

        let newer_id = NonZeroU64::new(62).unwrap();
        model
            .reserve(newer_id, Operation::SelectSession, model.selection_epoch())
            .unwrap();
        let newer_epoch = model.selection_epoch();
        gate.notify_one();

        let mut send_completions = 0;
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
                if completion.request_id == send_id.get() {
                    send_completions += 1;
                    assert_eq!(completion.status, CompletionStatus::Stale as u32);
                }
            }
            if send_completions == 1 {
                break;
            }
        }
        assert_eq!(send_completions, 1);
        assert_eq!(
            linked.lock().unwrap().as_slice(),
            &[("connection-a".to_string(), 11, 101, "hello".to_string())]
        );

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
