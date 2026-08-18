use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use axum::{extract::Extension, Json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::acp::opencode_plugins::PluginCheckSummary;
use crate::acp::preflight::PreflightResult;
use crate::acp::shared_session::{
    PromptEnqueueResult, SharedDisposition, SharedInteractionRequest, SharedLaunchIdentity,
    SharedMutationGuard, SharedPromptRequest, SharedRouteCapability, SharedSessionBroker,
    SharedSessionError, SharedSessionKey, SharedSessionPhase, SharedStopRequest,
};
use crate::acp::termination::AcpDisconnectOrigin;
use crate::acp::types::{
    AcpAgentInfo, AcpAgentStatus, AgentDiagnosticsReport, AgentSkillContent, AgentSkillLayout,
    AgentSkillScope, AgentSkillsListResult, ConnectionInfo, ForkResultInfo,
};
use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::acp as acp_commands;
use crate::commands::custom_agents as custom_agent_commands;
use crate::models::agent::AgentType;

pub async fn acp_get_shared_session_diagnostics(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<crate::acp::shared_session::SharedSessionDiagnostic>>, AppCommandError> {
    Ok(Json(
        acp_commands::acp_get_shared_session_diagnostics_core(&state.connection_manager).await,
    ))
}

/// Constructs a process-free registered shared root for HTTP integration
/// tests. The feature gate keeps the fake driver surface out of production
/// builds while allowing `tests/shared_session_http.rs` to exercise the real
/// router and manager registration path.
#[cfg(any(test, feature = "test-utils"))]
pub async fn registered_shared_spawn_attempt_for_http_test(
    connection_id: String,
    connection_incarnation: String,
    launch: crate::acp::manager::SharedConnectLaunch,
    existing_public_state: Option<
        Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>>,
    >,
    route_bootstrap_rx: tokio::sync::oneshot::Receiver<
        crate::acp::connection::RouteBootstrapOutcome,
    >,
    agent_stderr: Option<String>,
) -> crate::acp::connection::RegisteredSpawnAttempt {
    let state = match existing_public_state {
        Some(state) => {
            let mut replacement = crate::acp::session_state::SessionState::new(
                connection_id.clone(),
                launch.agent_type,
                launch.working_dir.clone().map(std::path::PathBuf::from),
                "shared-server".into(),
                launch.folder_id,
            );
            replacement.connection_incarnation = connection_incarnation.clone();
            replacement.set_route_plan_snapshot(&launch.launch_inputs.route_plan);
            state
                .write()
                .await
                .prepare_registered_replacement(replacement);
            state
        }
        None => {
            let mut state = crate::acp::session_state::SessionState::new(
                connection_id.clone(),
                launch.agent_type,
                launch.working_dir.clone().map(std::path::PathBuf::from),
                "shared-server".into(),
                launch.folder_id,
            );
            state.connection_incarnation = connection_incarnation.clone();
            state.set_route_plan_snapshot(&launch.launch_inputs.route_plan);
            Arc::new(tokio::sync::RwLock::new(state))
        }
    };
    if let Some(agent_stderr) = agent_stderr {
        state
            .write()
            .await
            .apply_event(&crate::acp::types::AcpEvent::Error {
                message: agent_stderr,
                agent_type: launch.agent_type.as_wire().into_owned(),
                code: Some("agent_stderr".into()),
                details: None,
                terminal: false,
            });
    }
    let (_session_started_tx, session_started_rx) = tokio::sync::oneshot::channel();
    crate::acp::connection::RegisteredSpawnAttempt {
        connection_id,
        connection_incarnation,
        state,
        emitter: crate::web::event_bridge::EventEmitter::Noop,
        handshake: crate::acp::connection::SpawnHandshake {
            session_started_rx,
            route_bootstrap_rx,
        },
        route_plan: launch.launch_inputs.route_plan,
        driver_start_tx: None,
        child_pid: Arc::new(std::sync::atomic::AtomicU32::new(0)),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTypeParams {
    pub agent_type: AgentType,
}

pub async fn acp_get_agent_status(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AgentTypeParams>,
) -> Result<Json<AcpAgentStatus>, AppCommandError> {
    let db = &state.db;
    let result = acp_commands::acp_get_agent_status_core(params.agent_type, db)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

pub async fn acp_list_agents(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<AcpAgentInfo>>, AppCommandError> {
    let db = &state.db;
    let result = acp_commands::acp_list_agents_core(db)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpEnvDiagnosticsParams {
    #[serde(default)]
    pub agent_type: Option<AgentType>,
}

pub async fn acp_env_diagnostics(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpEnvDiagnosticsParams>,
) -> Result<Json<AgentDiagnosticsReport>, AppCommandError> {
    let db = &state.db;
    let result = acp_commands::acp_env_diagnostics_core(db, params.agent_type)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConnectParams {
    pub agent_type: AgentType,
    pub working_dir: Option<String>,
    pub session_id: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<i32>,
    #[serde(default)]
    pub delegation_route_override: Option<crate::acp::delegation::route::DelegationRoutePolicy>,
    #[serde(default)]
    pub preferred_mode_id: Option<String>,
    #[serde(default)]
    pub preferred_config_values: Option<BTreeMap<String, String>>,
    /// Detached pop-out incarnation (desktop cold connect). Web keeps None.
    #[serde(default)]
    pub owner_operation_id: Option<String>,
}

pub async fn acp_connect(
    Extension(_state): Extension<Arc<AppState>>,
    Json(_params): Json<AcpConnectParams>,
) -> Result<Json<String>, AppCommandError> {
    Err(map_acp_error(crate::acp::error::AcpError::Shared(
        SharedSessionError::ProtocolRequired,
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConnectOrAttachRequest {
    pub conversation_id: Option<i32>,
    pub agent_type: AgentType,
    pub working_dir: Option<String>,
    pub external_session_id: Option<String>,
    pub delegation_route_override: Option<crate::acp::delegation::route::DelegationRoutePolicy>,
    pub preferred_mode_id: Option<String>,
    #[serde(default)]
    pub preferred_config_values: BTreeMap<String, String>,
    pub device_id: String,
    pub client_instance_id: String,
    pub request_id: String,
    pub retry_failed_generation: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedPublicPhase {
    Bootstrapping,
    Ready,
    Failed,
    Closing,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcpConnectOrAttachFailure {
    pub code: String,
    pub retryable: bool,
    pub cleanup_complete: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConnectOrAttachResponse {
    pub connection_id: String,
    pub generation: u64,
    pub lease_id: String,
    pub lease_expires_at: chrono::DateTime<chrono::Utc>,
    pub disposition: SharedDisposition,
    pub phase: SharedPublicPhase,
    pub event_seq: u64,
    pub error: Option<AcpConnectOrAttachFailure>,
}

struct SharedConnectTarget {
    key: SharedSessionKey,
    conversation_id: Option<i32>,
    folder_id: Option<i32>,
    working_dir: Option<String>,
    external_session_id: Option<String>,
}

impl SharedSessionBroker {
    pub fn ephemeral_key(
        device_id: &str,
        client_instance_id: &str,
        request_id: &str,
    ) -> Result<SharedSessionKey, SharedSessionError> {
        crate::acp::shared_session::validate_client_label("device_id", device_id)?;
        crate::acp::shared_session::validate_client_label(
            "client_instance_id",
            client_instance_id,
        )?;
        crate::acp::shared_session::validate_client_label("request_id", request_id)?;

        static STARTUP_NONCE: OnceLock<[u8; 16]> = OnceLock::new();
        let startup_nonce = STARTUP_NONCE.get_or_init(|| *uuid::Uuid::new_v4().as_bytes());
        let mut digest = Sha256::new();
        digest.update(startup_nonce);
        for label in [device_id, client_instance_id, request_id] {
            let bytes = label.as_bytes();
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
        Ok(SharedSessionKey::Ephemeral(format!(
            "{:x}",
            digest.finalize()
        )))
    }
}

fn invalid_shared_field(field: &'static str) -> AppCommandError {
    map_acp_error(crate::acp::error::AcpError::Shared(
        SharedSessionError::InvalidField { field },
    ))
}

async fn resolve_shared_connect_target(
    state: &AppState,
    params: &AcpConnectOrAttachRequest,
) -> Result<SharedConnectTarget, AppCommandError> {
    for (field, value) in [
        ("device_id", params.device_id.as_str()),
        ("client_instance_id", params.client_instance_id.as_str()),
        ("request_id", params.request_id.as_str()),
    ] {
        crate::acp::shared_session::validate_client_label(field, value)
            .map_err(|error| map_acp_error(error.into()))?;
    }

    if params.conversation_id.is_some_and(|id| id <= 0) {
        return Err(invalid_shared_field("conversation_id"));
    }

    crate::commands::delegate_access::ensure_connect_delegate_interactive(
        &state.db,
        &state.connection_manager,
        params.agent_type,
        params.external_session_id.as_deref(),
        params.conversation_id,
    )
    .await
    .map_err(map_acp_error)?;

    if let Some(conversation_id) = params.conversation_id {
        let conversation =
            crate::db::service::conversation_service::get_by_id(&state.db.conn, conversation_id)
                .await
                .map_err(|_| invalid_shared_field("conversation_id"))?;
        if conversation.agent_type != params.agent_type {
            return Err(invalid_shared_field("agent_type"));
        }
        let folder = crate::db::service::folder_service::get_folder_by_id(
            &state.db.conn,
            conversation.folder_id,
        )
        .await
        .map_err(|_| invalid_shared_field("folder_id"))?
        .ok_or_else(|| invalid_shared_field("folder_id"))?;
        if params.working_dir.as_deref().is_some_and(|working_dir| {
            crate::parsers::normalize_path_for_matching(working_dir)
                != crate::parsers::normalize_path_for_matching(&folder.path)
        }) {
            return Err(invalid_shared_field("working_dir"));
        }
        if let (Some(persisted), Some(requested)) = (
            conversation.external_id.as_deref(),
            params.external_session_id.as_deref(),
        ) {
            if persisted != requested {
                return Err(invalid_shared_field("external_session_id"));
            }
        }
        crate::commands::delegate_access::ensure_delegate_interactive(
            &state.db,
            &state.connection_manager,
            conversation_id,
        )
        .await
        .map_err(map_acp_error)?;
        return Ok(SharedConnectTarget {
            key: SharedSessionKey::Conversation(conversation_id),
            conversation_id: Some(conversation_id),
            folder_id: Some(conversation.folder_id),
            working_dir: Some(folder.path),
            external_session_id: params
                .external_session_id
                .clone()
                .or(conversation.external_id),
        });
    }

    let normalized_working_dir = params
        .working_dir
        .as_deref()
        .map(crate::parsers::normalize_path_for_matching)
        .filter(|value| !value.is_empty());
    let external_session_id = params
        .external_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let key = match (&normalized_working_dir, &external_session_id) {
        (Some(normalized_working_dir), Some(external_session_id)) => {
            SharedSessionKey::ExternalSession {
                agent_type: params.agent_type,
                normalized_working_dir: normalized_working_dir.clone(),
                external_session_id: external_session_id.clone(),
            }
        }
        _ => SharedSessionBroker::ephemeral_key(
            &params.device_id,
            &params.client_instance_id,
            &params.request_id,
        )
        .map_err(|error| map_acp_error(error.into()))?,
    };
    Ok(SharedConnectTarget {
        key,
        conversation_id: None,
        folder_id: None,
        working_dir: params.working_dir.clone(),
        external_session_id,
    })
}

fn public_shared_phase(
    phase: &SharedSessionPhase,
) -> Result<(SharedPublicPhase, Option<AcpConnectOrAttachFailure>), AppCommandError> {
    match phase {
        SharedSessionPhase::Reserved => {
            Err(map_acp_error(SharedSessionError::SessionUnavailable.into()))
        }
        SharedSessionPhase::Bootstrapping => Ok((SharedPublicPhase::Bootstrapping, None)),
        SharedSessionPhase::Ready => Ok((SharedPublicPhase::Ready, None)),
        SharedSessionPhase::Closing => Ok((SharedPublicPhase::Closing, None)),
        SharedSessionPhase::Failed {
            error_code,
            cleanup_complete,
        } => Ok((
            SharedPublicPhase::Failed,
            Some(AcpConnectOrAttachFailure {
                code: error_code.clone(),
                retryable: *cleanup_complete
                    && matches!(
                        error_code.as_str(),
                        "companion_initialization_failed" | "session_unavailable"
                    ),
                cleanup_complete: *cleanup_complete,
            }),
        )),
    }
}

fn select_shared_connect_response_state(
    attachment_generation: u64,
    attachment_phase: &SharedSessionPhase,
    authoritative_snapshot: Option<(u64, &SharedSessionPhase, u64)>,
) -> (SharedSessionPhase, u64) {
    match authoritative_snapshot {
        Some((generation, phase, event_seq)) if generation == attachment_generation => {
            (phase.clone(), event_seq)
        }
        _ => (attachment_phase.clone(), 0),
    }
}

pub async fn acp_connect_or_attach(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpConnectOrAttachRequest>,
) -> Result<Json<AcpConnectOrAttachResponse>, AppCommandError> {
    let target = resolve_shared_connect_target(&state, &params).await?;
    let runtime = state.delegation_runtime_settings.snapshot();
    let launch_inputs = crate::acp::terminal_context::build_acp_launch_inputs(
        &state.db,
        params.agent_type,
        target.external_session_id.as_deref(),
        &state.data_dir,
        crate::acp::terminal_context::AcpRouteRequest::root(
            target.conversation_id,
            params.delegation_route_override,
        ),
        &runtime,
    )
    .await
    .map_err(map_acp_error)?;
    let launch_identity = SharedLaunchIdentity {
        agent_type: params.agent_type,
        working_dir_fingerprint: crate::parsers::normalize_path_for_matching(
            target.working_dir.as_deref().unwrap_or_default(),
        ),
        external_session_id: target.external_session_id.clone(),
        attach_mode: crate::acp::session_attach::SessionAttachMode::Default,
        route_fingerprint: launch_inputs.route_plan.fingerprint.clone(),
        route_capability: SharedRouteCapability::from_route_plan(&launch_inputs.route_plan),
        terminal_shell_fingerprint: crate::terminal::shell::terminal_shell_selection_key(
            &launch_inputs.terminal_settings,
        ),
        purpose: crate::auto_title::ConnectionPurpose::User,
    };
    let launch_context = crate::auto_title::user_launch_context_from_db(&state.db.conn).await;
    let attachment = state
        .connection_manager
        .connect_or_attach_shared(crate::acp::manager::SharedConnectLaunch {
            database: state.db.conn.clone(),
            key: target.key,
            conversation_id: target.conversation_id,
            folder_id: target.folder_id,
            launch_identity,
            agent_type: params.agent_type,
            working_dir: target.working_dir,
            external_session_id: target.external_session_id,
            launch_inputs,
            emitter: state.emitter.clone(),
            preferred_mode_id: params.preferred_mode_id,
            preferred_config_values: params.preferred_config_values,
            launch_context,
            session_attach_mode: crate::acp::session_attach::SessionAttachMode::Default,
            device_id: params.device_id,
            client_instance_id: params.client_instance_id,
            request_id: params.request_id,
            retry_failed_generation: params.retry_failed_generation,
        })
        .await
        .map_err(map_acp_error)?;

    // Registration is the response latency boundary. One cooperative yield
    // lets already-resolved bootstrap failures settle without delaying a live
    // bootstrap that is still waiting on its companion handshake.
    tokio::task::yield_now().await;
    let snapshot = state
        .connection_manager
        .shared_session_broker()
        .authoritative_snapshot(&attachment.connection_id)
        .await
        .ok();
    let (phase, event_seq) = select_shared_connect_response_state(
        attachment.generation,
        &attachment.phase,
        snapshot
            .as_ref()
            .map(|snapshot| (snapshot.generation, &snapshot.phase, snapshot.event_seq)),
    );
    let (phase, error) = public_shared_phase(&phase)?;
    Ok(Json(AcpConnectOrAttachResponse {
        connection_id: attachment.connection_id,
        generation: attachment.generation,
        lease_id: attachment.lease_id,
        lease_expires_at: attachment.lease_expires_at,
        disposition: attachment.disposition,
        phase,
        event_seq,
        error,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDisconnectParams {
    pub connection_id: String,
    pub origin: AcpDisconnectOrigin,
    #[serde(default)]
    pub expected_owner_window: Option<String>,
    #[serde(default)]
    pub expected_operation_id: Option<String>,
    #[serde(default)]
    pub expected_ownership_generation: Option<u64>,
}

pub async fn acp_disconnect(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpDisconnectParams>,
) -> Result<Json<()>, AppCommandError> {
    let manager = &state.connection_manager;
    if manager
        .is_broker_managed_connection(&params.connection_id)
        .await
    {
        return Err(map_acp_error(crate::acp::error::AcpError::Shared(
            SharedSessionError::ProtocolRequired,
        )));
    }
    manager
        .disconnect_if_owner(
            &params.connection_id,
            params.expected_owner_window.as_deref(),
            params.expected_operation_id.as_deref(),
            params.expected_ownership_generation,
            params.origin,
        )
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpTouchConnectionParams {
    pub connection_id: String,
}

pub async fn acp_touch_connection(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpTouchConnectionParams>,
) -> Result<Json<bool>, AppCommandError> {
    let touched = state.connection_manager.touch(&params.connection_id).await;
    Ok(Json(touched))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPromptParams {
    pub connection_id: String,
    pub blocks: Vec<crate::acp::types::PromptInputBlock>,
    pub folder_id: Option<i32>,
    pub conversation_id: Option<i32>,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<String>,
    #[serde(default)]
    pub client_instance_id: Option<String>,
    #[serde(default)]
    pub client_request_id: Option<String>,
    #[serde(default)]
    pub client_message_id: Option<String>,
    /// Optional composer-visible text for title capture. `Some("")` is
    /// authoritative; absent falls back to ACP block projection.
    #[serde(default)]
    pub visible_text: Option<String>,
    /// Optional wire locale (`en`, `zh_cn`, …). Deserialized as `String` so
    /// unknown older-client values are accepted; lossy parse falls back.
    #[serde(default)]
    pub locale: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum AcpPromptResponse {
    Shared(PromptEnqueueResult),
    Legacy(()),
}

async fn validate_and_bind_shared_prompt_target(
    state: &AppState,
    guard: &SharedMutationGuard,
    conversation_id: Option<i32>,
    folder_id: Option<i32>,
) -> Result<(), AppCommandError> {
    if conversation_id.is_some() && folder_id.is_none() {
        return Err(invalid_shared_field("folder_id"));
    }
    let broker = state.connection_manager.shared_session_broker();
    broker
        .validate_guard(guard)
        .await
        .map_err(|error| map_acp_error(error.into()))?;
    let snapshot = broker
        .authoritative_snapshot(&guard.connection_id)
        .await
        .map_err(|error| map_acp_error(error.into()))?;
    if snapshot.generation != guard.generation {
        return Err(map_acp_error(SharedSessionError::GenerationStale.into()));
    }
    let current_conversation = snapshot.canonical_conversation_id;
    let current_folder = snapshot.folder_id;
    let agent_type = snapshot.agent_type;
    if let Some(current) = current_conversation {
        if conversation_id != Some(current) {
            return Err(map_acp_error(
                SharedSessionError::ConversationKeyConflict.into(),
            ));
        }
    }
    if let Some(current) = current_folder {
        if folder_id != Some(current) {
            return Err(invalid_shared_field("folder_id"));
        }
    }

    let Some(conversation_id) = conversation_id else {
        let folder_id = folder_id.ok_or_else(|| invalid_shared_field("folder_id"))?;
        crate::db::service::folder_service::get_folder_by_id(&state.db.conn, folder_id)
            .await
            .map_err(|_| invalid_shared_field("folder_id"))?
            .ok_or_else(|| invalid_shared_field("folder_id"))?;
        return Ok(());
    };
    let conversation =
        crate::db::service::conversation_service::get_by_id(&state.db.conn, conversation_id)
            .await
            .map_err(|_| invalid_shared_field("conversation_id"))?;
    if conversation.agent_type != agent_type || Some(conversation.folder_id) != folder_id {
        return Err(invalid_shared_field("conversation_id"));
    }
    crate::acp::delegation::workflow::require_writable_conversation_workflow(
        &state.db.conn,
        conversation_id,
    )
    .await
    .map_err(|error| map_acp_error(error.into()))?;

    if current_conversation.is_none() {
        broker
            .bind_conversation_key_guarded(
                guard,
                conversation_id,
                folder_id.expect("conversation target requires a folder"),
            )
            .await
            .map_err(|error| map_acp_error(error.into()))?;
    }
    Ok(())
}

pub async fn acp_prompt(
    Extension(state): Extension<Arc<AppState>>,
    Json(mut params): Json<AcpPromptParams>,
) -> Result<Json<AcpPromptResponse>, AppCommandError> {
    crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
        &state.db,
        &state.connection_manager,
        &params.connection_id,
        params.conversation_id,
    )
    .await
    .map_err(|error| {
        error
            .app_command_error()
            .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
    })?;
    if state
        .connection_manager
        .is_broker_managed_connection(&params.connection_id)
        .await
    {
        let guard = shared_mutation_guard(
            &params.connection_id,
            params.generation,
            params.lease_id.take(),
        )?;
        let client_instance_id = params
            .client_instance_id
            .take()
            .ok_or_else(|| map_acp_error(SharedSessionError::ProtocolRequired.into()))?;
        let client_request_id = params
            .client_request_id
            .take()
            .ok_or_else(|| map_acp_error(SharedSessionError::ProtocolRequired.into()))?;
        let client_message_id = params
            .client_message_id
            .take()
            .ok_or_else(|| map_acp_error(SharedSessionError::ProtocolRequired.into()))?;
        for (field, value) in [
            ("client_instance_id", client_instance_id.as_str()),
            ("client_request_id", client_request_id.as_str()),
        ] {
            crate::acp::shared_session::validate_client_label(field, value)
                .map_err(|error| map_acp_error(error.into()))?;
        }
        if client_message_id.is_empty() {
            return Err(invalid_shared_field("client_message_id"));
        }
        if params.blocks.is_empty() {
            return Err(invalid_shared_field("blocks"));
        }
        crate::acp::prompt_hydration::hydrate_prompt_blocks(
            &mut params.blocks,
            &crate::paths::codeg_uploads_root(),
        )
        .await
        .map_err(map_acp_error)?;
        validate_and_bind_shared_prompt_target(
            &state,
            &guard,
            params.conversation_id,
            params.folder_id,
        )
        .await?;
        let capture =
            crate::auto_title::prompt_capture_from_wire(params.visible_text, params.locale);
        let result = state
            .connection_manager
            .enqueue_shared_prompt(SharedPromptRequest {
                guard,
                client_instance_id,
                client_request_id,
                blocks: params.blocks,
                folder_id: params.folder_id,
                conversation_id: params.conversation_id,
                client_message_id,
                capture,
                submitted_at: chrono::Utc::now(),
            })
            .await
            .map_err(map_acp_error)?;
        return Ok(Json(AcpPromptResponse::Shared(result)));
    }
    let capture = crate::auto_title::prompt_capture_from_wire(params.visible_text, params.locale);
    state
        .connection_manager
        .send_prompt_linked_with_message_id(
            &state.db,
            &params.connection_id,
            params.blocks,
            params.folder_id,
            params.conversation_id,
            None,
            params.client_message_id,
            capture,
        )
        .await
        .map_err(|error| {
            error
                .app_command_error()
                .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
        })?;
    Ok(Json(AcpPromptResponse::Legacy(())))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSharedLeaseParams {
    pub connection_id: String,
    pub generation: u64,
    pub lease_id: String,
}

pub async fn acp_release_lease(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpSharedLeaseParams>,
) -> Result<Json<()>, AppCommandError> {
    crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
        &state.db,
        &state.connection_manager,
        &params.connection_id,
        None,
    )
    .await
    .map_err(map_acp_error)?;
    state
        .connection_manager
        .shared_session_broker()
        .release_lease(&SharedMutationGuard {
            connection_id: params.connection_id,
            generation: params.generation,
            lease_id: params.lease_id,
        })
        .await
        .map_err(|error| map_acp_error(error.into()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpCancelQueuedPromptParams {
    pub connection_id: String,
    pub generation: u64,
    pub lease_id: String,
    pub queue_item_id: String,
}

pub async fn acp_cancel_queued_prompt(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpCancelQueuedPromptParams>,
) -> Result<Json<()>, AppCommandError> {
    crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
        &state.db,
        &state.connection_manager,
        &params.connection_id,
        None,
    )
    .await
    .map_err(map_acp_error)?;
    state
        .connection_manager
        .cancel_shared_queued_prompt(
            SharedMutationGuard {
                connection_id: params.connection_id,
                generation: params.generation,
                lease_id: params.lease_id,
            },
            &params.queue_item_id,
        )
        .await
        .map_err(map_acp_error)?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpTerminateSharedSessionParams {
    pub connection_id: String,
    pub generation: u64,
}

pub async fn acp_terminate_shared_session(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpTerminateSharedSessionParams>,
) -> Result<Json<()>, AppCommandError> {
    crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
        &state.db,
        &state.connection_manager,
        &params.connection_id,
        None,
    )
    .await
    .map_err(map_acp_error)?;
    state
        .connection_manager
        .terminate_shared_session(&params.connection_id, params.generation)
        .await
        .map_err(map_acp_error)?;
    Ok(Json(()))
}

// --- Pattern A: Pure function handlers ---

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPreflightParams {
    pub agent_type: AgentType,
    pub force_refresh: Option<bool>,
}

pub async fn acp_preflight(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpPreflightParams>,
) -> Result<Json<PreflightResult>, AppCommandError> {
    let result =
        acp_commands::acp_preflight_core(params.agent_type, params.force_refresh, &state.db)
            .await
            .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

pub async fn acp_clear_binary_cache(
    Json(params): Json<AgentTypeParams>,
) -> Result<Json<()>, AppCommandError> {
    acp_commands::acp_clear_binary_cache(params.agent_type)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpListAgentSkillsParams {
    pub agent_type: AgentType,
    pub workspace_path: Option<String>,
}

pub async fn acp_list_agent_skills(
    Json(params): Json<AcpListAgentSkillsParams>,
) -> Result<Json<AgentSkillsListResult>, AppCommandError> {
    let result = acp_commands::acp_list_agent_skills(params.agent_type, params.workspace_path)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpReadAgentSkillParams {
    pub agent_type: AgentType,
    pub scope: AgentSkillScope,
    pub skill_id: String,
    pub workspace_path: Option<String>,
}

pub async fn acp_read_agent_skill(
    Json(params): Json<AcpReadAgentSkillParams>,
) -> Result<Json<AgentSkillContent>, AppCommandError> {
    let result = acp_commands::acp_read_agent_skill(
        params.agent_type,
        params.scope,
        params.skill_id,
        params.workspace_path,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSaveAgentSkillParams {
    pub agent_type: AgentType,
    pub scope: AgentSkillScope,
    pub skill_id: String,
    pub content: String,
    pub workspace_path: Option<String>,
    pub layout: Option<AgentSkillLayout>,
}

pub async fn acp_save_agent_skill(
    Json(params): Json<AcpSaveAgentSkillParams>,
) -> Result<Json<()>, AppCommandError> {
    acp_commands::acp_save_agent_skill(
        params.agent_type,
        params.scope,
        params.skill_id,
        params.content,
        params.workspace_path,
        params.layout,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDeleteAgentSkillParams {
    pub agent_type: AgentType,
    pub scope: AgentSkillScope,
    pub skill_id: String,
    pub workspace_path: Option<String>,
}

pub async fn acp_delete_agent_skill(
    Json(params): Json<AcpDeleteAgentSkillParams>,
) -> Result<Json<()>, AppCommandError> {
    acp_commands::acp_delete_agent_skill(
        params.agent_type,
        params.scope,
        params.skill_id,
        params.workspace_path,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

// --- Pattern C: ConnectionManager handlers ---

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpConnectionIdParams {
    pub connection_id: String,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<String>,
    #[serde(default)]
    pub turn_id: Option<String>,
}

fn shared_mutation_guard(
    connection_id: &str,
    generation: Option<u64>,
    lease_id: Option<String>,
) -> Result<SharedMutationGuard, AppCommandError> {
    let (Some(generation), Some(lease_id)) = (generation, lease_id) else {
        let error = crate::acp::error::AcpError::Shared(SharedSessionError::ProtocolRequired);
        return Err(error
            .app_command_error()
            .expect("shared protocol errors have structured mappings"));
    };
    Ok(SharedMutationGuard {
        connection_id: connection_id.to_string(),
        generation,
        lease_id,
    })
}

async fn validate_shared_mutation_if_managed(
    manager: &crate::acp::manager::ConnectionManager,
    connection_id: &str,
    generation: Option<u64>,
    lease_id: Option<String>,
) -> Result<(), AppCommandError> {
    if !manager.is_broker_managed_connection(connection_id).await {
        return Ok(());
    }
    let guard = shared_mutation_guard(connection_id, generation, lease_id)?;
    manager
        .shared_session_broker()
        .validate_guard(&guard)
        .await
        .map_err(|error| map_acp_error(error.into()))
}

fn map_acp_error(error: crate::acp::error::AcpError) -> AppCommandError {
    error
        .app_command_error()
        .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
}

async fn interaction_is_broker_managed(
    manager: &crate::acp::manager::ConnectionManager,
    supplied_connection_id: &str,
    authoritative_owner: Option<String>,
) -> Result<bool, AppCommandError> {
    let routing_connection_id = match authoritative_owner {
        Some(owner) if owner != supplied_connection_id => {
            // Broker-owned interactions treat a mismatched caller as already
            // resolved so a local spoof cannot fence the lease. Legacy
            // delegated children keep routing through the owner so viewer-only
            // admission can reject without consuming the pending card.
            if manager.is_broker_managed_connection(&owner).await
                || manager
                    .is_broker_managed_connection(supplied_connection_id)
                    .await
            {
                return Err(map_acp_error(crate::acp::error::AcpError::Shared(
                    SharedSessionError::InteractionAlreadyResolved,
                )));
            }
            owner
        }
        Some(owner) => owner,
        None => supplied_connection_id.to_string(),
    };
    Ok(manager
        .is_broker_managed_connection(&routing_connection_id)
        .await)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpForkParams {
    pub connection_id: String,
    /// Caller-supplied linkage for a resumed historical conversation whose row
    /// isn't bound to the connection yet (fork-send forks before the first
    /// prompt links it). Both are ignored once the connection is already
    /// linked. See `ConnectionManager::fork_session`.
    #[serde(default)]
    pub conversation_id: Option<i32>,
    #[serde(default)]
    pub folder_id: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSetModeParams {
    pub connection_id: String,
    pub mode_id: String,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<String>,
}

pub async fn acp_set_mode(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpSetModeParams>,
) -> Result<Json<()>, AppCommandError> {
    crate::commands::delegate_access::ensure_connection_delegate_interactive(
        &state.db,
        &state.connection_manager,
        &params.connection_id,
    )
    .await
    .map_err(|error| {
        error
            .app_command_error()
            .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
    })?;
    let manager = &state.connection_manager;
    validate_shared_mutation_if_managed(
        manager,
        &params.connection_id,
        params.generation,
        params.lease_id,
    )
    .await?;
    manager
        .set_mode(&params.connection_id, params.mode_id)
        .await
        .map_err(|error| {
            error
                .app_command_error()
                .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
        })?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpSetConfigOptionParams {
    pub connection_id: String,
    pub config_id: String,
    pub value_id: String,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<String>,
}

pub async fn acp_set_config_option(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpSetConfigOptionParams>,
) -> Result<Json<()>, AppCommandError> {
    crate::commands::delegate_access::ensure_connection_delegate_interactive(
        &state.db,
        &state.connection_manager,
        &params.connection_id,
    )
    .await
    .map_err(|error| {
        error
            .app_command_error()
            .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
    })?;
    let manager = &state.connection_manager;
    validate_shared_mutation_if_managed(
        manager,
        &params.connection_id,
        params.generation,
        params.lease_id,
    )
    .await?;
    manager
        .set_config_option(&params.connection_id, params.config_id, params.value_id)
        .await
        .map_err(|error| {
            error
                .app_command_error()
                .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
        })?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpGoalControlParams {
    pub connection_id: String,
    pub action: crate::acp::connection::GoalControlAction,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<String>,
}

pub async fn acp_goal_control(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpGoalControlParams>,
) -> Result<Json<()>, AppCommandError> {
    let manager = &state.connection_manager;
    validate_shared_mutation_if_managed(
        manager,
        &params.connection_id,
        params.generation,
        params.lease_id,
    )
    .await?;
    manager
        .goal_control(&state.db.conn, &params.connection_id, params.action)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDescribeAgentOptionsParams {
    pub agent_type: crate::models::AgentType,
    #[serde(default)]
    pub working_dir: Option<String>,
}

pub async fn acp_describe_agent_options(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpDescribeAgentOptionsParams>,
) -> Result<Json<crate::acp::types::AgentOptionsSnapshot>, AppCommandError> {
    let runtime = state.delegation_runtime_settings.snapshot();
    let snapshot = crate::commands::acp::acp_describe_agent_options_core(
        &state.connection_manager,
        &state.db,
        &state.data_dir,
        params.agent_type,
        params.working_dir,
        &runtime,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(snapshot))
}

pub async fn acp_cancel(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpConnectionIdParams>,
) -> Result<Json<()>, AppCommandError> {
    let manager = &state.connection_manager;
    crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
        &state.db,
        manager,
        &params.connection_id,
        None,
    )
    .await
    .map_err(map_acp_error)?;
    if manager
        .is_broker_managed_connection(&params.connection_id)
        .await
    {
        let guard =
            shared_mutation_guard(&params.connection_id, params.generation, params.lease_id)?;
        let turn_id = params.turn_id.ok_or_else(|| {
            map_acp_error(crate::acp::error::AcpError::Shared(
                SharedSessionError::ProtocolRequired,
            ))
        })?;
        manager
            .stop_shared_turn(&state.db.conn, SharedStopRequest { guard, turn_id })
            .await
            .map_err(map_acp_error)?;
        return Ok(Json(()));
    }
    manager
        .cancel(&state.db.conn, &params.connection_id)
        .await
        .map_err(|error| {
            error
                .app_command_error()
                .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
        })?;
    Ok(Json(()))
}

pub async fn acp_fork(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpForkParams>,
) -> Result<Json<ForkResultInfo>, AppCommandError> {
    crate::commands::delegate_access::ensure_effective_delegate_interactive(
        &state.db,
        &state.connection_manager,
        &params.connection_id,
        params.conversation_id,
    )
    .await
    .map_err(|error| {
        error
            .app_command_error()
            .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
    })?;
    let manager = &state.connection_manager;
    if manager
        .is_broker_managed_connection(&params.connection_id)
        .await
    {
        return Err(map_acp_error(crate::acp::error::AcpError::Shared(
            SharedSessionError::ProtocolRequired,
        )));
    }
    let result = manager
        .fork_session(
            &state.db,
            &params.connection_id,
            params.conversation_id,
            params.folder_id,
        )
        .await
        .map_err(|error| {
            // Prefer structured mapping (TurnInProgress, DelegateViewerOnly, …)
            // so expected client conditions stay 409 rather than 500.
            error
                .app_command_error()
                .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
        })?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpRespondPermissionParams {
    pub connection_id: String,
    pub request_id: String,
    pub option_id: String,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<String>,
}

pub async fn acp_respond_permission(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpRespondPermissionParams>,
) -> Result<Json<()>, AppCommandError> {
    let manager = &state.connection_manager;
    if manager
        .is_broker_managed_connection(&params.connection_id)
        .await
    {
        crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
            &state.db,
            manager,
            &params.connection_id,
            None,
        )
        .await
        .map_err(map_acp_error)?;
        let guard =
            shared_mutation_guard(&params.connection_id, params.generation, params.lease_id)?;
        manager
            .respond_shared_permission(SharedInteractionRequest {
                guard,
                interaction_id: params.request_id,
                answer: params.option_id,
            })
            .await
            .map_err(map_acp_error)?;
        return Ok(Json(()));
    }
    manager
        .respond_permission(&params.connection_id, &params.request_id, &params.option_id)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAnswerQuestionParams {
    pub connection_id: String,
    pub question_id: String,
    pub answer: crate::acp::question::QuestionAnswer,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<String>,
}

pub async fn acp_answer_question(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpAnswerQuestionParams>,
) -> Result<Json<()>, AppCommandError> {
    let manager = &state.connection_manager;
    let authoritative_owner = manager
        .pending_question_parent_connection_id(&params.question_id)
        .await;
    if interaction_is_broker_managed(manager, &params.connection_id, authoritative_owner).await? {
        crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
            &state.db,
            manager,
            &params.connection_id,
            None,
        )
        .await
        .map_err(map_acp_error)?;
        let guard =
            shared_mutation_guard(&params.connection_id, params.generation, params.lease_id)?;
        manager
            .answer_shared_question(SharedInteractionRequest {
                guard,
                interaction_id: params.question_id,
                answer: params.answer,
            })
            .await
            .map_err(map_acp_error)?;
        return Ok(Json(()));
    }
    // Guard the connection that owns question_id, not the caller-supplied id —
    // answer_question routes by question_id and ignores connection_id.
    crate::commands::delegate_access::ensure_pending_question_delegate_interactive(
        &state.db,
        &state.connection_manager,
        &params.question_id,
    )
    .await
    .map_err(|error| {
        error
            .app_command_error()
            .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
    })?;
    manager
        .answer_question(&params.connection_id, &params.question_id, params.answer)
        .await
        .map_err(|error| {
            error
                .app_command_error()
                .unwrap_or_else(|| AppCommandError::task_execution_failed(error.to_string()))
        })?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAnswerPlanApprovalParams {
    pub connection_id: String,
    pub approval_id: String,
    pub answer: crate::acp::plan_approval::PlanApprovalAnswer,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<String>,
}

pub async fn acp_answer_plan_approval(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpAnswerPlanApprovalParams>,
) -> Result<Json<()>, AppCommandError> {
    let manager = &state.connection_manager;
    let authoritative_owner = manager
        .pending_plan_approval_parent_connection_id(&params.approval_id)
        .await;
    if interaction_is_broker_managed(manager, &params.connection_id, authoritative_owner).await? {
        crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
            &state.db,
            manager,
            &params.connection_id,
            None,
        )
        .await
        .map_err(map_acp_error)?;
        let guard =
            shared_mutation_guard(&params.connection_id, params.generation, params.lease_id)?;
        manager
            .answer_shared_plan_approval(SharedInteractionRequest {
                guard,
                interaction_id: params.approval_id,
                answer: params.answer,
            })
            .await
            .map_err(map_acp_error)?;
        return Ok(Json(()));
    }
    crate::commands::delegate_access::ensure_web_shared_or_delegate_interactive(
        &state.db,
        manager,
        &params.connection_id,
        None,
    )
    .await
    .map_err(map_acp_error)?;
    manager
        .answer_plan_approval(&params.connection_id, &params.approval_id, params.answer)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

pub async fn acp_list_connections(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<ConnectionInfo>>, AppCommandError> {
    let manager = &state.connection_manager;
    let result = manager.list_connections().await;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpGetSessionSnapshotParams {
    pub connection_id: String,
}

pub async fn acp_get_session_snapshot(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpGetSessionSnapshotParams>,
) -> Result<Json<Option<crate::acp::LiveSessionSnapshot>>, AppCommandError> {
    let snap = acp_commands::acp_get_session_snapshot_core(
        &state.connection_manager,
        &params.connection_id,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(snap))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpGetSessionSnapshotByConversationParams {
    pub conversation_id: i32,
}

pub async fn acp_get_session_snapshot_by_conversation(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpGetSessionSnapshotByConversationParams>,
) -> Result<Json<Option<crate::acp::LiveSessionSnapshot>>, AppCommandError> {
    let snap = acp_commands::acp_get_session_snapshot_by_conversation_core(
        &state.connection_manager,
        params.conversation_id,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(snap))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpFindConnectionForConversationParams {
    pub conversation_id: i32,
    /// Optional session id (`external_id`) fallback, matched (with `agent_type`)
    /// when no live connection is bound to `conversation_id` yet (pre-first-
    /// prompt window).
    #[serde(default)]
    pub session_id: Option<String>,
    pub agent_type: AgentType,
}

pub async fn acp_find_connection_for_conversation(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpFindConnectionForConversationParams>,
) -> Result<Json<Option<crate::acp::ConversationConnectionInfo>>, AppCommandError> {
    let info = acp_commands::acp_find_connection_for_conversation_core(
        &state.connection_manager,
        params.conversation_id,
        params.session_id.as_deref(),
        params.agent_type,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(info))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDelegateAccessParams {
    pub conversation_id: i32,
}

pub async fn get_delegate_access(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<GetDelegateAccessParams>,
) -> Result<Json<crate::models::DelegateAccessState>, AppCommandError> {
    Ok(Json(
        crate::commands::delegate_access::get_delegate_access_core(
            &state.db,
            &state.connection_manager,
            params.conversation_id,
        )
        .await,
    ))
}

// --- Pattern B+: Core function handlers ---

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUpdateAgentPreferencesParams {
    pub agent_type: AgentType,
    pub enabled: bool,
    pub env: BTreeMap<String, String>,
    pub config_json: Option<String>,
    pub opencode_auth_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
}

pub async fn acp_update_agent_preferences(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUpdateAgentPreferencesParams>,
) -> Result<Json<usize>, AppCommandError> {
    let db = &state.db;
    let emitter = state.emitter.clone();
    let affected = acp_commands::acp_update_agent_preferences_and_refresh(
        params.agent_type,
        params.enabled,
        params.env,
        params.config_json,
        params.opencode_auth_json,
        params.codex_auth_json,
        params.codex_config_toml,
        db,
        &state.connection_manager,
        &state.data_dir,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(affected))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUpdateAgentDisplayPreferencesParams {
    pub agent_type: AgentType,
    pub show_thinking: bool,
}

pub async fn acp_update_agent_display_preferences(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUpdateAgentDisplayPreferencesParams>,
) -> Result<Json<()>, AppCommandError> {
    acp_commands::acp_update_agent_display_preferences_core(
        params.agent_type,
        params.show_thinking,
        &state.db,
        &state.emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUpdateAgentEnvParams {
    pub agent_type: AgentType,
    pub enabled: bool,
    pub env: BTreeMap<String, String>,
    pub model_provider_id: Option<i32>,
}

pub async fn acp_update_agent_env(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUpdateAgentEnvParams>,
) -> Result<Json<usize>, AppCommandError> {
    let db = &state.db;
    let emitter = state.emitter.clone();
    let affected = acp_commands::acp_update_agent_env_and_refresh(
        params.agent_type,
        params.enabled,
        params.env,
        params.model_provider_id,
        db,
        &state.connection_manager,
        &state.data_dir,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(affected))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUpdateAgentConfigParams {
    pub agent_type: AgentType,
    pub config_json: Option<String>,
    pub opencode_auth_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
    pub codex_model_catalog: Option<String>,
    pub codex_sandbox: Option<crate::acp::types::CodexSandboxStructuredConfig>,
    pub grok_config_toml: Option<String>,
    pub grok_structured: Option<crate::acp::types::GrokStructuredConfig>,
    pub cursor_cli_config_json: Option<String>,
    pub cursor_structured: Option<crate::acp::types::CursorStructuredConfig>,
}

pub async fn acp_update_agent_config(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUpdateAgentConfigParams>,
) -> Result<Json<usize>, AppCommandError> {
    let emitter = state.emitter.clone();
    let affected = acp_commands::acp_update_agent_config_and_refresh(
        params.agent_type,
        params.config_json,
        params.opencode_auth_json,
        params.codex_auth_json,
        params.codex_config_toml,
        params.codex_model_catalog,
        params.codex_sandbox,
        params.grok_config_toml,
        params.grok_structured,
        params.cursor_cli_config_json,
        params.cursor_structured,
        &state.db,
        &state.connection_manager,
        &state.data_dir,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(affected))
}

/// Optional live API key from the Cursor settings form, forwarded so the
/// `status` / `models` probes test what's on screen (empty ⇒ browser-login).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CursorProbeParams {
    pub api_key: Option<String>,
}

pub async fn acp_cursor_auth_status(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<CursorProbeParams>,
) -> Result<Json<crate::acp::types::CursorAuthStatus>, AppCommandError> {
    Ok(Json(
        acp_commands::acp_cursor_auth_status_core(&state.db, params.api_key).await,
    ))
}

pub async fn acp_cursor_list_models(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<CursorProbeParams>,
) -> Result<Json<crate::acp::types::CursorModelsResult>, AppCommandError> {
    Ok(Json(
        acp_commands::acp_cursor_list_models_core(&state.db, params.api_key).await,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUpdateHermesConfigParams {
    pub provider: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub raw_config_yaml: Option<String>,
}

pub async fn acp_update_hermes_config(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUpdateHermesConfigParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    acp_commands::acp_update_hermes_config_core(
        acp_commands::HermesConfigUpdate {
            provider: params.provider,
            api_key: params.api_key,
            model: params.model,
            base_url: params.base_url,
            raw_config_yaml: params.raw_config_yaml,
        },
        &emitter,
    )
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUpdateKimiCodeConfigParams {
    pub mode: String,
    #[serde(default)]
    pub interface_type: Option<String>,
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub max_context_size: Option<i64>,
    #[serde(default)]
    pub vertex_project: Option<String>,
    #[serde(default)]
    pub vertex_location: Option<String>,
    #[serde(default)]
    pub raw_config_toml: Option<String>,
    #[serde(default)]
    pub reasoning_enabled: Option<bool>,
    #[serde(default)]
    pub always_thinking: Option<bool>,
    #[serde(default)]
    pub support_efforts: Option<Vec<String>>,
    #[serde(default)]
    pub default_effort: Option<String>,
}

pub async fn acp_update_kimi_code_config(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUpdateKimiCodeConfigParams>,
) -> Result<Json<usize>, AppCommandError> {
    let emitter = state.emitter.clone();
    let affected = acp_commands::acp_update_kimi_code_config_and_refresh(
        acp_commands::KimiCodeConfigUpdate {
            mode: params.mode,
            interface_type: params.interface_type,
            auth_type: params.auth_type,
            base_url: params.base_url,
            api_key: params.api_key,
            model: params.model,
            max_context_size: params.max_context_size,
            vertex_project: params.vertex_project,
            vertex_location: params.vertex_location,
            raw_config_toml: params.raw_config_toml,
            reasoning_enabled: params.reasoning_enabled,
            always_thinking: params.always_thinking,
            support_efforts: params.support_efforts,
            default_effort: params.default_effort,
        },
        &state.db,
        &state.connection_manager,
        &state.data_dir,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(affected))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpFetchKimiModelsParams {
    pub base_url: String,
    pub api_key: String,
}

pub async fn acp_fetch_kimi_models(
    Json(params): Json<AcpFetchKimiModelsParams>,
) -> Result<Json<Vec<String>>, AppCommandError> {
    let models = acp_commands::acp_fetch_kimi_models_core(&params.base_url, &params.api_key)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(models))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUpdatePiConfigParams {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub custom_base_url: Option<String>,
    #[serde(default)]
    pub custom_api: Option<String>,
}

pub async fn acp_update_pi_config(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUpdatePiConfigParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    acp_commands::acp_update_pi_config_core(
        acp_commands::PiConfigUpdate {
            provider: params.provider,
            model: params.model,
            thinking_level: params.thinking_level,
            api_key: params.api_key,
            custom_base_url: params.custom_base_url,
            custom_api: params.custom_api,
        },
        &state.db,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

pub async fn acp_load_pi_config() -> Result<Json<acp_commands::PiConfigProjection>, AppCommandError>
{
    Ok(Json(acp_commands::load_pi_config_core()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpValidatePiCommandParams {
    pub command: String,
}

pub async fn acp_validate_pi_command(
    Json(params): Json<AcpValidatePiCommandParams>,
) -> Result<Json<acp_commands::PiCommandValidation>, AppCommandError> {
    Ok(Json(acp_commands::acp_validate_pi_command_core(
        params.command,
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPiProjectTrustStateParams {
    pub workspace: String,
}

pub async fn acp_pi_project_trust_state(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpPiProjectTrustStateParams>,
) -> Result<Json<acp_commands::PiProjectTrustState>, AppCommandError> {
    let result = acp_commands::acp_pi_project_trust_state_core(&state.db, params.workspace)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPiSetProjectTrustParams {
    pub workspace: String,
    #[serde(default)]
    pub trusted: Option<bool>,
}

pub async fn acp_pi_set_project_trust(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpPiSetProjectTrustParams>,
) -> Result<Json<()>, AppCommandError> {
    acp_commands::acp_pi_set_project_trust_core(&state.db, params.workspace, params.trusted)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPiAcknowledgeProjectTrustParams {
    pub workspace: String,
}

pub async fn acp_pi_acknowledge_project_trust(
    Json(params): Json<AcpPiAcknowledgeProjectTrustParams>,
) -> Result<Json<()>, AppCommandError> {
    acp_commands::acp_pi_acknowledge_project_trust_core(params.workspace)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

pub async fn acp_pi_list_trust_entries(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<acp_commands::PiTrustEntry>>, AppCommandError> {
    let result = acp_commands::acp_pi_list_trust_entries_core(&state.db)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDownloadAgentBinaryParams {
    pub agent_type: AgentType,
    #[serde(default)]
    pub version: Option<String>,
    pub task_id: String,
}

pub async fn acp_download_agent_binary(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpDownloadAgentBinaryParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    acp_commands::acp_download_agent_binary_core(
        params.agent_type,
        params.version,
        params.task_id,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpInstallUvToolParams {
    pub task_id: String,
}

pub async fn acp_install_uv_tool(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpInstallUvToolParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    acp_commands::acp_install_uv_tool_core(params.task_id, &emitter)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

pub async fn acp_detect_agent_local_version(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AgentTypeParams>,
) -> Result<Json<Option<String>>, AppCommandError> {
    let db = &state.db;
    let emitter = state.emitter.clone();
    let result =
        acp_commands::acp_detect_agent_local_version_core(params.agent_type, &db.conn, &emitter)
            .await
            .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPrepareNpxAgentParams {
    pub agent_type: AgentType,
    pub registry_version: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub clean_first: bool,
    pub task_id: String,
}

pub async fn acp_prepare_npx_agent(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpPrepareNpxAgentParams>,
) -> Result<Json<String>, AppCommandError> {
    let db = &state.db;
    let emitter = state.emitter.clone();
    let result = acp_commands::acp_prepare_npx_agent_core(
        params.agent_type,
        params.registry_version,
        params.version,
        params.clean_first,
        params.task_id,
        db,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpUninstallAgentParams {
    pub agent_type: AgentType,
    pub task_id: String,
}

pub async fn acp_uninstall_agent(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpUninstallAgentParams>,
) -> Result<Json<()>, AppCommandError> {
    let db = &state.db;
    let emitter = state.emitter.clone();
    acp_commands::acp_uninstall_agent_core(params.agent_type, params.task_id, db, &emitter)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpPiBinaryParams {
    pub task_id: String,
}

pub async fn acp_install_pi_binary(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpPiBinaryParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    acp_commands::acp_install_pi_binary_core(params.task_id, &emitter)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

pub async fn acp_uninstall_pi_binary(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpPiBinaryParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    acp_commands::acp_uninstall_pi_binary_core(params.task_id, &emitter)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpReorderAgentsParams {
    pub agent_types: Vec<AgentType>,
}

pub async fn acp_reorder_agents(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpReorderAgentsParams>,
) -> Result<Json<()>, AppCommandError> {
    let db = &state.db;
    let emitter = state.emitter.clone();
    acp_commands::acp_reorder_agents_core(&params.agent_types, db, &emitter)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

pub async fn opencode_list_plugins() -> Result<Json<PluginCheckSummary>, AppCommandError> {
    let result = acp_commands::opencode_list_plugins_core()
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpencodeProviderCatalogParams {
    #[serde(default)]
    pub force_refresh: Option<bool>,
}

pub async fn opencode_provider_catalog(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<OpencodeProviderCatalogParams>,
) -> Result<Json<Vec<crate::acp::opencode_catalog::CatalogProvider>>, AppCommandError> {
    let catalog = acp_commands::opencode_provider_catalog_core(
        &state.data_dir,
        params.force_refresh.unwrap_or(false),
    )
    .await;
    Ok(Json(catalog))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexBundledCatalogParams {
    #[serde(default)]
    pub force_refresh: Option<bool>,
}

pub async fn codex_bundled_catalog(
    Extension(_state): Extension<Arc<AppState>>,
    Json(params): Json<CodexBundledCatalogParams>,
) -> Result<Json<Vec<serde_json::Value>>, AppCommandError> {
    Ok(Json(
        acp_commands::codex_bundled_catalog_core(params.force_refresh.unwrap_or(false)).await,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpencodeInstallPluginsParams {
    pub names: Option<Vec<String>>,
    pub task_id: String,
}

pub async fn opencode_install_plugins(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<OpencodeInstallPluginsParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = crate::web::event_bridge::EventEmitter::web_only(
        state.event_broadcaster.clone(),
        state.acp_event_bus.clone(),
    );
    acp_commands::opencode_install_plugins_core(params.names, params.task_id, &emitter)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpencodeUninstallPluginParams {
    pub name: String,
}

pub async fn opencode_uninstall_plugin(
    Json(params): Json<OpencodeUninstallPluginParams>,
) -> Result<Json<PluginCheckSummary>, AppCommandError> {
    let result = acp_commands::opencode_uninstall_plugin_core(params.name)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

pub async fn codex_request_device_code(
) -> Result<Json<acp_commands::CodexDeviceCodeResponse>, AppCommandError> {
    let result = acp_commands::codex_request_device_code_core()
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexPollDeviceCodeParams {
    pub device_auth_id: String,
    pub user_code: String,
}

pub async fn codex_poll_device_code(
    Json(params): Json<CodexPollDeviceCodeParams>,
) -> Result<Json<acp_commands::CodexDeviceCodePollResult>, AppCommandError> {
    let result = acp_commands::codex_poll_device_code_core(params.device_auth_id, params.user_code)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::session_attach::SessionAttachMode;
    use crate::acp::shared_session::{
        SharedLaunchIdentity, SharedReserveRequest, SharedSessionKey,
    };
    use crate::app_error::AppErrorCode;
    use crate::auto_title::{parse_supported_app_locale, prompt_capture_from_wire};
    use crate::web::event_bridge::EventEmitter;

    #[test]
    fn connect_response_does_not_mix_a_replacement_generation_snapshot() {
        let selected = select_shared_connect_response_state(
            1,
            &SharedSessionPhase::Failed {
                error_code: "session_unavailable".into(),
                cleanup_complete: true,
            },
            Some((2, &SharedSessionPhase::Ready, 42)),
        );

        assert_eq!(
            selected,
            (
                SharedSessionPhase::Failed {
                    error_code: "session_unavailable".into(),
                    cleanup_complete: true,
                },
                0,
            )
        );
    }

    async fn broker_owned_interaction_state(
        connection_id: &str,
        conversation_id: i32,
    ) -> (Arc<AppState>, tempfile::TempDir) {
        let (state, dir, _) = broker_owned_mutation_state(
            connection_id,
            conversation_id,
            crate::auto_title::ConnectionPurpose::User,
        )
        .await;
        (state, dir)
    }

    async fn broker_owned_mutation_state(
        connection_id: &str,
        conversation_id: i32,
        purpose: crate::auto_title::ConnectionPurpose,
    ) -> (
        Arc<AppState>,
        tempfile::TempDir,
        crate::acp::shared_session::SharedSessionAttachment,
    ) {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new_for_test(db, dir.path().to_path_buf()));
        state
            .connection_manager
            .insert_test_connection(connection_id, AgentType::Codex, None, EventEmitter::Noop)
            .await;
        let attachment = state
            .connection_manager
            .shared_session_broker()
            .reserve_or_attach(SharedReserveRequest {
                key: SharedSessionKey::Ephemeral(format!("handler-owner-{conversation_id}")),
                connection_id: connection_id.into(),
                launch_identity: SharedLaunchIdentity {
                    agent_type: AgentType::Codex,
                    working_dir_fingerprint: "handler-owner-cwd".into(),
                    external_session_id: None,
                    attach_mode: SessionAttachMode::Default,
                    route_fingerprint: "handler-owner-route".into(),
                    route_capability: SharedRouteCapability::Standard,
                    terminal_shell_fingerprint: "handler-owner-shell".into(),
                    purpose,
                },
                client_instance_id: "handler-owner-client".into(),
                device_id: "handler-owner-device".into(),
                request_id: "handler-owner-connect".into(),
                retry_failed_generation: None,
                now: tokio::time::Instant::now(),
                now_utc: chrono::Utc::now(),
            })
            .await
            .unwrap()
            .attachment;
        (state, dir, attachment)
    }

    #[tokio::test]
    async fn broker_snapshot_retains_authoritative_identity_and_public_sequence() {
        let (state, _dir, attachment) = broker_owned_mutation_state(
            "retained-snapshot",
            1965,
            crate::auto_title::ConnectionPurpose::User,
        )
        .await;
        let public_state = state
            .connection_manager
            .get_state(&attachment.connection_id)
            .await
            .expect("registered test state");
        {
            let mut public_state = public_state.write().await;
            public_state.conversation_id = None;
            public_state.event_seq = 41;
        }
        let broker = state.connection_manager.shared_session_broker();
        broker
            .bind_conversation_key(&attachment.connection_id, attachment.generation, 1965)
            .await
            .expect("bind authoritative conversation key");
        broker
            .install_registered(
                &attachment.connection_id,
                attachment.generation,
                "retained-snapshot-driver".into(),
                public_state,
                EventEmitter::Noop,
                Arc::new(std::sync::atomic::AtomicU32::new(0)),
            )
            .await
            .expect("install broker-retained state");

        let snapshot = broker
            .authoritative_snapshot(&attachment.connection_id)
            .await
            .expect("authoritative snapshot");
        assert_eq!(snapshot.purpose, crate::auto_title::ConnectionPurpose::User);
        assert_eq!(snapshot.canonical_conversation_id, Some(1965));
        assert_eq!(snapshot.generation, attachment.generation);
        assert_eq!(snapshot.phase, SharedSessionPhase::Bootstrapping);
        assert_eq!(snapshot.event_seq, 41);
    }

    #[tokio::test]
    async fn public_release_rejects_a_non_user_broker_root() {
        let (state, _dir, attachment) = broker_owned_mutation_state(
            "delegation-release",
            1963,
            crate::auto_title::ConnectionPurpose::Delegation,
        )
        .await;

        let error = acp_release_lease(
            Extension(state.clone()),
            Json(AcpSharedLeaseParams {
                connection_id: attachment.connection_id.clone(),
                generation: attachment.generation,
                lease_id: attachment.lease_id.clone(),
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, AppErrorCode::SharedSessionProtocolRequired);
        state
            .connection_manager
            .shared_session_broker()
            .release_lease(&SharedMutationGuard {
                connection_id: attachment.connection_id,
                generation: attachment.generation,
                lease_id: attachment.lease_id,
            })
            .await
            .expect("rejected public release leaves the lease active");
    }

    #[tokio::test]
    async fn public_termination_rejects_a_non_user_broker_root() {
        let (state, _dir, attachment) = broker_owned_mutation_state(
            "delegation-terminate",
            1964,
            crate::auto_title::ConnectionPurpose::Delegation,
        )
        .await;

        let error = acp_terminate_shared_session(
            Extension(state.clone()),
            Json(AcpTerminateSharedSessionParams {
                connection_id: attachment.connection_id.clone(),
                generation: attachment.generation,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, AppErrorCode::SharedSessionProtocolRequired);
        assert!(
            state
                .connection_manager
                .shared_session_broker()
                .is_managed_connection(&attachment.connection_id)
                .await
        );
    }

    #[test]
    fn acp_prompt_params_accept_optional_visible_text_and_lossy_locale() {
        let with_fields = r#"{
            "connectionId": "c1",
            "blocks": [{"type":"text","text":"hi"}],
            "visibleText": "composer visible",
            "locale": "zh_cn"
        }"#;
        let params: AcpPromptParams =
            serde_json::from_str(with_fields).expect("deserialize with optional fields");
        assert_eq!(params.visible_text.as_deref(), Some("composer visible"));
        assert_eq!(params.locale.as_deref(), Some("zh_cn"));
        let capture = prompt_capture_from_wire(params.visible_text, params.locale).unwrap();
        assert_eq!(capture.visible_text.as_deref(), Some("composer visible"));
        assert_eq!(capture.locale, parse_supported_app_locale(Some("zh_cn")));

        // Older clients omit the fields entirely.
        let legacy = r#"{"connectionId":"c1","blocks":[]}"#;
        let legacy_params: AcpPromptParams =
            serde_json::from_str(legacy).expect("legacy payload without optional fields");
        assert!(legacy_params.visible_text.is_none());
        assert!(legacy_params.locale.is_none());
        assert!(
            prompt_capture_from_wire(legacy_params.visible_text, legacy_params.locale).is_none()
        );

        // Unknown locale must deserialize (not reject) then lossy-parse to None.
        let unknown = r#"{
            "connectionId": "c1",
            "blocks": [],
            "locale": "Klingon"
        }"#;
        let unknown_params: AcpPromptParams =
            serde_json::from_str(unknown).expect("unknown locale must not reject request");
        assert_eq!(unknown_params.locale.as_deref(), Some("Klingon"));
        let capture = prompt_capture_from_wire(None, unknown_params.locale).unwrap();
        assert_eq!(capture.locale, None);
    }

    #[test]
    fn shared_mutation_fields_are_optional_for_legacy_payloads_but_fence_shared_calls() {
        let legacy: AcpConnectionIdParams =
            serde_json::from_str(r#"{"connectionId":"local"}"#).unwrap();
        assert!(legacy.generation.is_none());
        assert!(legacy.lease_id.is_none());
        assert!(legacy.turn_id.is_none());

        let error = shared_mutation_guard("shared", None, None).unwrap_err();
        assert_eq!(error.code, AppErrorCode::SharedSessionProtocolRequired);

        let permission: AcpRespondPermissionParams = serde_json::from_str(
            r#"{
                "connectionId":"shared",
                "requestId":"permission-1",
                "optionId":"allow",
                "generation":7,
                "leaseId":"lease-7"
            }"#,
        )
        .unwrap();
        let guard = shared_mutation_guard(
            &permission.connection_id,
            permission.generation,
            permission.lease_id,
        )
        .unwrap();
        assert_eq!(guard.connection_id, "shared");
        assert_eq!(guard.generation, 7);
    }

    #[tokio::test]
    async fn spoofed_local_connection_cannot_answer_broker_owned_question() {
        let (state, _dir) = broker_owned_interaction_state("question-owner", 1961).await;
        let registered = state
            .connection_manager
            .register_question(
                "question-owner",
                vec![crate::acp::question::QuestionSpec {
                    id: "choice".into(),
                    question: "Choose".into(),
                    header: "Choice".into(),
                    multi_select: false,
                    options: vec![],
                    is_secret: false,
                    recovery: None,
                }],
            )
            .await
            .expect("question registered");

        let spoofed = acp_answer_question(
            Extension(state.clone()),
            Json(AcpAnswerQuestionParams {
                connection_id: "local-spoof".into(),
                question_id: registered.question_id.clone(),
                answer: Default::default(),
                generation: None,
                lease_id: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(spoofed.code, AppErrorCode::InteractionAlreadyResolved);
        assert!(state
            .connection_manager
            .pending_question_parent_connection_id(&registered.question_id)
            .await
            .is_some());

        let unfenced_owner = acp_answer_question(
            Extension(state),
            Json(AcpAnswerQuestionParams {
                connection_id: "question-owner".into(),
                question_id: registered.question_id,
                answer: Default::default(),
                generation: None,
                lease_id: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            unfenced_owner.code,
            AppErrorCode::SharedSessionProtocolRequired
        );
    }

    #[tokio::test]
    async fn spoofed_local_connection_cannot_answer_broker_owned_plan_approval() {
        let (state, _dir) = broker_owned_interaction_state("plan-owner", 1962).await;
        let registered = state
            .connection_manager
            .register_plan_approval("plan-owner", "tool-call".into(), "plan".into())
            .await
            .expect("plan approval registered");

        let spoofed = acp_answer_plan_approval(
            Extension(state.clone()),
            Json(AcpAnswerPlanApprovalParams {
                connection_id: "local-spoof".into(),
                approval_id: registered.approval_id.clone(),
                answer: crate::acp::plan_approval::PlanApprovalAnswer {
                    decision: crate::acp::plan_approval::PlanApprovalDecision::Approve,
                    feedback: None,
                },
                generation: None,
                lease_id: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(spoofed.code, AppErrorCode::InteractionAlreadyResolved);
        assert_eq!(
            state
                .connection_manager
                .pending_plan_approval_parent_connection_id(&registered.approval_id)
                .await
                .as_deref(),
            Some("plan-owner")
        );

        let unfenced_owner = acp_answer_plan_approval(
            Extension(state),
            Json(AcpAnswerPlanApprovalParams {
                connection_id: "plan-owner".into(),
                approval_id: registered.approval_id,
                answer: crate::acp::plan_approval::PlanApprovalAnswer {
                    decision: crate::acp::plan_approval::PlanApprovalDecision::Approve,
                    feedback: None,
                },
                generation: None,
                lease_id: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(
            unfenced_owner.code,
            AppErrorCode::SharedSessionProtocolRequired
        );
    }
}

// ---------------------------------------------------------------------------
// Custom ACP agents (user-registered). See `commands::custom_agents`.
// ---------------------------------------------------------------------------

pub async fn acp_list_custom_agents(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<custom_agent_commands::CustomAgentInfo>>, AppCommandError> {
    let result = custom_agent_commands::acp_list_custom_agents_core(&state.db)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

/// Wrapper matching the Tauri command's single `params` argument — the shared
/// frontend client sends the same body to both runtimes.
#[derive(Deserialize)]
pub struct AcpSaveCustomAgentBody {
    pub params: custom_agent_commands::SaveCustomAgentParams,
}

pub async fn acp_save_custom_agent(
    Extension(state): Extension<Arc<AppState>>,
    Json(body): Json<AcpSaveCustomAgentBody>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    custom_agent_commands::acp_save_custom_agent_params_core(body.params, &state.db, &emitter)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpDeleteCustomAgentParams {
    pub registry_id: String,
    #[serde(default)]
    pub delete_transcripts: bool,
}

pub async fn acp_delete_custom_agent(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpDeleteCustomAgentParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    custom_agent_commands::acp_delete_custom_agent_core(
        params.registry_id,
        params.delete_transcripts,
        &state.db,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

pub async fn acp_fetch_registry_catalog(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<crate::acp::remote_registry::RegistryCatalogAgent>>, AppCommandError> {
    let result = custom_agent_commands::acp_fetch_registry_catalog_core(&state.db)
        .await
        .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpAddRegistryAgentParams {
    pub registry_id: String,
    #[serde(default)]
    pub distribution_kind: Option<String>,
}

pub async fn acp_add_registry_agent(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<AcpAddRegistryAgentParams>,
) -> Result<Json<()>, AppCommandError> {
    let emitter = state.emitter.clone();
    custom_agent_commands::acp_add_registry_agent_core(
        params.registry_id,
        params.distribution_kind,
        &state.db,
        &emitter,
    )
    .await
    .map_err(|e| AppCommandError::task_execution_failed(e.to_string()))?;
    Ok(Json(()))
}

pub async fn acp_current_platform() -> Json<String> {
    Json(custom_agent_commands::acp_current_platform_core())
}
