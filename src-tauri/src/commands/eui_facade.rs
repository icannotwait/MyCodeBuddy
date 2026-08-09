use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::acp::preflight::CheckStatus;
use crate::acp::terminal_context::{build_acp_launch_inputs, AcpRouteRequest};
use crate::acp::types::{
    AcpAgentInfo, AcpEvent, CodexSandboxSettings, CodexSandboxStructuredConfig, GrokSettings,
    GrokStructuredConfig, PromptInputBlock,
};
use crate::app_state::AppState;
use crate::commands::acp::{
    acp_list_agents_core, acp_preflight_core, acp_update_agent_config_and_refresh,
    acp_update_agent_env_and_refresh, verify_agent_installed,
};
use crate::commands::conversations::{
    create_project_conversation_core, get_folder_conversation_with_live_core,
};
use crate::commands::folders::open_folder_core;
use crate::commands::history_window::HistoryLoadOpts;
use crate::db::entities::conversation::ConversationKind;
use crate::db::service::conversation_service;
use crate::models::{AgentType, DbConversationSummary, MessageTurn};
use crate::web::event_bridge::emit_with_state_gated;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EuiWorkspace {
    pub folder_id: i32,
    pub path: PathBuf,
    pub sessions: Vec<EuiSessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EuiSessionSummary {
    pub conversation_id: i32,
    pub title: Option<String>,
    pub agent_type: AgentType,
    pub status: String,
    pub external_session_id: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EuiSessionSelection {
    pub folder_id: i32,
    pub path: PathBuf,
    pub conversation_id: i32,
    pub title: Option<String>,
    pub agent_type: AgentType,
    pub status: String,
    pub external_session_id: Option<String>,
    pub updated_at_ms: i64,
    pub connection_id: String,
    pub transcript: Vec<MessageTurn>,
}

#[derive(Debug)]
pub(crate) struct LoadedEuiSession {
    pub summary: EuiSessionSummary,
    pub transcript: Vec<MessageTurn>,
}

#[async_trait::async_trait]
pub(crate) trait EuiSessionOps: Send + Sync {
    type LaunchInputs: Send;

    async fn verify_installed(&self, agent_type: AgentType) -> Result<(), EuiFacadeError>;

    async fn build_launch_inputs(
        &self,
        state: &AppState,
        agent_type: AgentType,
        external_session_id: Option<&str>,
        conversation_id: i32,
    ) -> Result<Self::LaunchInputs, EuiFacadeError>;

    #[allow(clippy::too_many_arguments)]
    async fn spawn_agent(
        &self,
        state: &AppState,
        agent_type: AgentType,
        workspace_path: &std::path::Path,
        external_session_id: Option<String>,
        conversation_id: i32,
        launch_inputs: Self::LaunchInputs,
        owner: &str,
    ) -> Result<String, EuiFacadeError>;

    async fn find_connection(&self, state: &AppState, conversation_id: i32) -> Option<String>;

    async fn bind_connection(
        &self,
        state: &AppState,
        connection_id: &str,
        folder_id: i32,
        conversation_id: i32,
    ) -> Result<(), EuiFacadeError>;

    #[allow(clippy::too_many_arguments)]
    async fn send_linked(
        &self,
        state: &AppState,
        connection_id: &str,
        blocks: Vec<PromptInputBlock>,
        folder_id: i32,
        conversation_id: i32,
        client_message_id: String,
    ) -> Result<(), EuiFacadeError>;
}

struct ProductionEuiSessionOps;

#[async_trait::async_trait]
impl EuiSessionOps for ProductionEuiSessionOps {
    type LaunchInputs = crate::acp::terminal_context::AcpLaunchInputs;

    async fn verify_installed(&self, agent_type: AgentType) -> Result<(), EuiFacadeError> {
        verify_agent_installed(agent_type)
            .await
            .map_err(EuiFacadeError::from)
    }

    async fn build_launch_inputs(
        &self,
        state: &AppState,
        agent_type: AgentType,
        external_session_id: Option<&str>,
        conversation_id: i32,
    ) -> Result<Self::LaunchInputs, EuiFacadeError> {
        build_acp_launch_inputs(
            &state.db,
            agent_type,
            external_session_id,
            &state.data_dir,
            AcpRouteRequest::root(Some(conversation_id), None),
            &state.delegation_runtime_settings.snapshot(),
        )
        .await
        .map_err(EuiFacadeError::from)
    }

    async fn spawn_agent(
        &self,
        state: &AppState,
        agent_type: AgentType,
        workspace_path: &std::path::Path,
        external_session_id: Option<String>,
        _conversation_id: i32,
        launch_inputs: Self::LaunchInputs,
        owner: &str,
    ) -> Result<String, EuiFacadeError> {
        let launch_context = crate::auto_title::user_launch_context_from_db(&state.db.conn).await;
        state
            .connection_manager
            .spawn_agent(
                agent_type,
                Some(workspace_path.to_string_lossy().into_owned()),
                external_session_id,
                launch_inputs,
                owner.to_string(),
                state.emitter.clone(),
                None,
                BTreeMap::new(),
                launch_context,
                None,
                None,
            )
            .await
            .map_err(EuiFacadeError::from)
    }

    async fn find_connection(&self, state: &AppState, conversation_id: i32) -> Option<String> {
        state
            .connection_manager
            .find_connection_by_conversation_id(conversation_id)
            .await
    }

    async fn bind_connection(
        &self,
        state: &AppState,
        connection_id: &str,
        folder_id: i32,
        conversation_id: i32,
    ) -> Result<(), EuiFacadeError> {
        bind_eui_connection(state, connection_id, folder_id, conversation_id).await
    }

    async fn send_linked(
        &self,
        state: &AppState,
        connection_id: &str,
        blocks: Vec<PromptInputBlock>,
        folder_id: i32,
        conversation_id: i32,
        client_message_id: String,
    ) -> Result<(), EuiFacadeError> {
        state
            .connection_manager
            .send_prompt_linked_with_message_id(
                &state.db,
                connection_id,
                blocks,
                Some(folder_id),
                Some(conversation_id),
                None,
                Some(client_message_id),
                None,
            )
            .await?;
        Ok(())
    }
}

/// The native EUI settings contract intentionally contains only fields owned
/// by the existing Grok/Codex ACP settings paths.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EuiAgentSettings {
    pub agent_type: AgentType,
    pub available: bool,
    pub enabled: bool,
    pub installed_version: Option<String>,
    pub env: BTreeMap<String, String>,
    pub config_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
    pub codex_model_catalog: Option<String>,
    pub codex_sandbox: Option<CodexSandboxSettings>,
    pub grok_config_toml: Option<String>,
    pub grok_settings: Option<GrokSettings>,
    pub model_provider_id: Option<i32>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EuiAgentSettingsPatch {
    pub enabled: Option<bool>,
    pub env: Option<BTreeMap<String, String>>,
    pub model_provider_id: Option<i32>,
    pub config_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
    pub codex_model_catalog: Option<String>,
    pub codex_sandbox: Option<CodexSandboxStructuredConfig>,
    pub grok_config_toml: Option<String>,
    pub grok_structured: Option<GrokStructuredConfig>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EuiAgentProbe {
    pub launchable: bool,
    pub installed_version: Option<String>,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum EuiFacadeError {
    #[error("unsupported EUI agent: {0}")]
    UnsupportedAgent(String),
    #[error("settings field is not valid for {agent}: {field}")]
    AgentFieldConflict {
        agent: AgentType,
        field: &'static str,
    },
    #[error("agent settings row was not found for {0}")]
    AgentNotFound(AgentType),
    #[error("invalid agent settings patch: {0}")]
    InvalidPatch(String),
    #[error("invalid EUI workspace {path}: {reason}")]
    InvalidWorkspace { path: String, reason: String },
    #[error("conversation {conversation_id} does not belong to workspace folder {folder_id}")]
    ConversationOutsideWorkspace {
        conversation_id: i32,
        folder_id: i32,
    },
    #[error("conversation {conversation_id} is not an eligible EUI session")]
    IneligibleConversation { conversation_id: i32 },
    #[error("could not bind EUI connection {connection_id}: {reason}")]
    ConnectionBinding {
        connection_id: String,
        reason: String,
    },
    #[error("EUI application operation failed: {0}")]
    App(#[from] crate::app_error::AppCommandError),
    #[error("EUI database operation failed: {0}")]
    Database(#[from] crate::db::error::DbError),
    #[error("ACP settings operation failed: {0}")]
    Acp(#[from] crate::acp::error::AcpError),
}

pub async fn set_eui_workspace(
    state: &AppState,
    requested_path: PathBuf,
) -> Result<EuiWorkspace, EuiFacadeError> {
    let path = std::fs::canonicalize(&requested_path).map_err(|error| {
        EuiFacadeError::InvalidWorkspace {
            path: requested_path.display().to_string(),
            reason: error.to_string(),
        }
    })?;
    if !path.is_dir() {
        return Err(EuiFacadeError::InvalidWorkspace {
            path: path.display().to_string(),
            reason: "path is not a directory".to_string(),
        });
    }
    let wire_path = path
        .to_str()
        .ok_or_else(|| EuiFacadeError::InvalidWorkspace {
            path: path.display().to_string(),
            reason: "canonical path is not valid UTF-8".to_string(),
        })?
        .to_string();
    let folder = open_folder_core(&state.db, wire_path).await?;
    let sessions =
        conversation_service::list_by_folder(&state.db.conn, folder.id, None, None, None, None)
            .await?
            .into_iter()
            .filter(is_eui_session_eligible)
            .map(project_session_summary)
            .collect();
    Ok(EuiWorkspace {
        folder_id: folder.id,
        path,
        sessions,
    })
}

pub async fn create_eui_conversation(
    state: &AppState,
    folder_id: i32,
    agent_type: AgentType,
) -> Result<EuiSessionSummary, EuiFacadeError> {
    ensure_supported(agent_type)?;
    let created =
        create_project_conversation_core(&state.db.conn, folder_id, agent_type, None, None).await?;
    let row = conversation_service::get_by_id(&state.db.conn, created.conversation_id).await?;
    Ok(project_session_summary(row))
}

pub async fn create_eui_session(
    state: &AppState,
    workspace: &EuiWorkspace,
    agent_type: AgentType,
) -> Result<EuiSessionSelection, EuiFacadeError> {
    create_eui_session_with_ops(state, workspace, agent_type, &ProductionEuiSessionOps).await
}

pub(crate) async fn create_eui_session_with_ops<O: EuiSessionOps>(
    state: &AppState,
    workspace: &EuiWorkspace,
    agent_type: AgentType,
    ops: &O,
) -> Result<EuiSessionSelection, EuiFacadeError> {
    ensure_supported(agent_type)?;
    ops.verify_installed(agent_type).await?;
    let summary = create_eui_conversation(state, workspace.folder_id, agent_type).await?;
    let launch_inputs = ops
        .build_launch_inputs(
            state,
            summary.agent_type,
            summary.external_session_id.as_deref(),
            summary.conversation_id,
        )
        .await?;
    let connection_id = ops
        .spawn_agent(
            state,
            summary.agent_type,
            &workspace.path,
            summary.external_session_id.clone(),
            summary.conversation_id,
            launch_inputs,
            "eui",
        )
        .await?;
    ops.bind_connection(
        state,
        &connection_id,
        workspace.folder_id,
        summary.conversation_id,
    )
    .await?;
    Ok(selection_from_parts(
        workspace,
        summary,
        connection_id,
        Vec::new(),
    ))
}

pub async fn select_eui_session(
    state: &AppState,
    workspace: &EuiWorkspace,
    conversation_id: i32,
) -> Result<EuiSessionSelection, EuiFacadeError> {
    select_eui_session_with_ops(state, workspace, conversation_id, &ProductionEuiSessionOps).await
}

pub(crate) async fn select_eui_session_with_ops<O: EuiSessionOps>(
    state: &AppState,
    workspace: &EuiWorkspace,
    conversation_id: i32,
    ops: &O,
) -> Result<EuiSessionSelection, EuiFacadeError> {
    let loaded = load_eui_session(state, workspace, conversation_id).await?;
    let connection_id = match ops.find_connection(state, conversation_id).await {
        Some(connection_id) => connection_id,
        None => {
            ensure_supported(loaded.summary.agent_type)?;
            ops.verify_installed(loaded.summary.agent_type).await?;
            let launch_inputs = ops
                .build_launch_inputs(
                    state,
                    loaded.summary.agent_type,
                    loaded.summary.external_session_id.as_deref(),
                    loaded.summary.conversation_id,
                )
                .await?;
            let connection_id = ops
                .spawn_agent(
                    state,
                    loaded.summary.agent_type,
                    &workspace.path,
                    loaded.summary.external_session_id.clone(),
                    loaded.summary.conversation_id,
                    launch_inputs,
                    "eui",
                )
                .await?;
            ops.bind_connection(
                state,
                &connection_id,
                workspace.folder_id,
                loaded.summary.conversation_id,
            )
            .await?;
            connection_id
        }
    };
    Ok(selection_from_parts(
        workspace,
        loaded.summary,
        connection_id,
        loaded.transcript,
    ))
}

pub async fn send_eui_message(
    state: &AppState,
    selection: &EuiSessionSelection,
    text: String,
) -> Result<(), EuiFacadeError> {
    send_eui_message_with_ops(state, selection, text, &ProductionEuiSessionOps).await
}

pub(crate) async fn send_eui_message_with_ops<O: EuiSessionOps>(
    state: &AppState,
    selection: &EuiSessionSelection,
    text: String,
    ops: &O,
) -> Result<(), EuiFacadeError> {
    let blocks = vec![PromptInputBlock::Text { text }];
    ops.send_linked(
        state,
        &selection.connection_id,
        blocks,
        selection.folder_id,
        selection.conversation_id,
        uuid::Uuid::new_v4().to_string(),
    )
    .await
}

pub(crate) async fn load_eui_session(
    state: &AppState,
    workspace: &EuiWorkspace,
    conversation_id: i32,
) -> Result<LoadedEuiSession, EuiFacadeError> {
    let row = conversation_service::get_by_id(&state.db.conn, conversation_id).await?;
    if row.folder_id != workspace.folder_id {
        return Err(EuiFacadeError::ConversationOutsideWorkspace {
            conversation_id,
            folder_id: workspace.folder_id,
        });
    }
    if !is_eui_session_eligible(&row) {
        return Err(EuiFacadeError::IneligibleConversation { conversation_id });
    }
    let detail = get_folder_conversation_with_live_core(
        &state.db.conn,
        &state.connection_manager,
        &state.chat_channel_manager,
        &state.emitter,
        state.internal_sessions.as_ref(),
        conversation_id,
        HistoryLoadOpts {
            user_turn_limit: Some(100),
            before_turn_id: None,
        },
    )
    .await?;
    if detail.summary.folder_id != workspace.folder_id {
        return Err(EuiFacadeError::ConversationOutsideWorkspace {
            conversation_id,
            folder_id: workspace.folder_id,
        });
    }
    Ok(LoadedEuiSession {
        summary: project_session_summary(detail.summary),
        transcript: detail.turns,
    })
}

fn selection_from_parts(
    workspace: &EuiWorkspace,
    summary: EuiSessionSummary,
    connection_id: String,
    transcript: Vec<MessageTurn>,
) -> EuiSessionSelection {
    EuiSessionSelection {
        folder_id: workspace.folder_id,
        path: workspace.path.clone(),
        conversation_id: summary.conversation_id,
        title: summary.title,
        agent_type: summary.agent_type,
        status: summary.status,
        external_session_id: summary.external_session_id,
        updated_at_ms: summary.updated_at_ms,
        connection_id,
        transcript,
    }
}

fn project_session_summary(row: DbConversationSummary) -> EuiSessionSummary {
    EuiSessionSummary {
        conversation_id: row.id,
        title: row.title,
        agent_type: row.agent_type,
        status: row.status,
        external_session_id: row.external_id,
        updated_at_ms: row.updated_at.timestamp_millis(),
    }
}

fn is_eui_session_eligible(row: &DbConversationSummary) -> bool {
    row.kind == ConversationKind::Regular
        && matches!(row.agent_type, AgentType::Codex | AgentType::Grok)
}

async fn bind_eui_connection(
    state: &AppState,
    connection_id: &str,
    folder_id: i32,
    conversation_id: i32,
) -> Result<(), EuiFacadeError> {
    let (session, emitter) = state
        .connection_manager
        .get_state_and_emitter(connection_id)
        .await
        .ok_or_else(|| EuiFacadeError::ConnectionBinding {
            connection_id: connection_id.to_string(),
            reason: "connection is no longer live".to_string(),
        })?;
    {
        let current = session.read().await;
        match (current.conversation_id, current.folder_id) {
            (Some(current_conversation), Some(current_folder))
                if current_conversation == conversation_id && current_folder == folder_id =>
            {
                return Ok(())
            }
            (Some(current_conversation), _) => {
                return Err(EuiFacadeError::ConnectionBinding {
                    connection_id: connection_id.to_string(),
                    reason: format!("already belongs to conversation {current_conversation}"),
                })
            }
            _ => {}
        }
    }

    let applied = emit_with_state_gated(
        &session,
        &emitter,
        AcpEvent::ConversationLinked {
            conversation_id,
            folder_id,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        },
        |current| current.conversation_id.is_none(),
    )
    .await;
    if applied {
        return Ok(());
    }

    let current = session.read().await;
    if current.conversation_id == Some(conversation_id) && current.folder_id == Some(folder_id) {
        Ok(())
    } else {
        Err(EuiFacadeError::ConnectionBinding {
            connection_id: connection_id.to_string(),
            reason: "a concurrent operation bound it to another conversation".to_string(),
        })
    }
}

impl EuiAgentSettingsPatch {
    pub(crate) fn validate_for(&self, agent: AgentType) -> Result<(), EuiFacadeError> {
        match agent {
            AgentType::Codex => {
                if self.grok_config_toml.is_some() {
                    return Err(EuiFacadeError::AgentFieldConflict {
                        agent,
                        field: "grokConfigToml",
                    });
                }
                if self.grok_structured.is_some() {
                    return Err(EuiFacadeError::AgentFieldConflict {
                        agent,
                        field: "grokStructured",
                    });
                }
            }
            AgentType::Grok => {
                if self.codex_auth_json.is_some() {
                    return Err(EuiFacadeError::AgentFieldConflict {
                        agent,
                        field: "codexAuthJson",
                    });
                }
                if self.codex_config_toml.is_some() {
                    return Err(EuiFacadeError::AgentFieldConflict {
                        agent,
                        field: "codexConfigToml",
                    });
                }
                if self.codex_model_catalog.is_some() {
                    return Err(EuiFacadeError::AgentFieldConflict {
                        agent,
                        field: "codexModelCatalog",
                    });
                }
                if self.codex_sandbox.is_some() {
                    return Err(EuiFacadeError::AgentFieldConflict {
                        agent,
                        field: "codexSandbox",
                    });
                }
            }
            _ => {
                return Err(EuiFacadeError::UnsupportedAgent(
                    agent.as_wire().into_owned(),
                ))
            }
        }
        Ok(())
    }
}

/// Parse the intentionally smaller EUI wire vocabulary. This is called before
/// touching an AppState so unsupported agents fail closed without DB or config
/// access.
pub fn parse_supported_agent(wire: &str) -> Result<AgentType, EuiFacadeError> {
    match wire {
        "codex" => Ok(AgentType::Codex),
        "grok" => Ok(AgentType::Grok),
        other => Err(EuiFacadeError::UnsupportedAgent(other.to_string())),
    }
}

pub(crate) fn project_agent_settings(info: AcpAgentInfo) -> EuiAgentSettings {
    let is_codex = info.agent_type == AgentType::Codex;
    let is_grok = info.agent_type == AgentType::Grok;
    EuiAgentSettings {
        agent_type: info.agent_type,
        available: info.available,
        enabled: info.enabled,
        installed_version: info.installed_version,
        env: info.env,
        config_json: info.config_json,
        codex_auth_json: is_codex.then_some(info.codex_auth_json).flatten(),
        codex_config_toml: is_codex.then_some(info.codex_config_toml).flatten(),
        codex_model_catalog: is_codex.then_some(info.codex_model_catalog).flatten(),
        codex_sandbox: is_codex.then_some(info.codex_sandbox_settings).flatten(),
        grok_config_toml: is_grok.then_some(info.grok_config_toml).flatten(),
        grok_settings: is_grok.then_some(info.grok_settings).flatten(),
        model_provider_id: info.model_provider_id,
    }
}

pub async fn get_eui_agent_settings(
    state: &AppState,
    agent: AgentType,
) -> Result<EuiAgentSettings, EuiFacadeError> {
    ensure_supported(agent)?;
    let agents = acp_list_agents_core(&state.db).await?;
    let info = agents
        .into_iter()
        .find(|candidate| candidate.agent_type == agent)
        .ok_or(EuiFacadeError::AgentNotFound(agent))?;
    Ok(project_agent_settings(info))
}

pub async fn set_eui_agent_settings(
    state: &AppState,
    agent: AgentType,
    patch: EuiAgentSettingsPatch,
) -> Result<EuiAgentSettings, EuiFacadeError> {
    ensure_supported(agent)?;
    patch.validate_for(agent)?;
    let current = get_eui_agent_settings(state, agent).await?;

    let enabled = patch.enabled.unwrap_or(current.enabled);
    let env = patch.env.clone().unwrap_or_else(|| current.env.clone());
    let model_provider_id = patch.model_provider_id.or(current.model_provider_id);
    if patch.enabled.is_some() || patch.env.is_some() || patch.model_provider_id.is_some() {
        update_env(state, agent, enabled, env, model_provider_id).await?;
    }

    let has_native_config = patch.config_json.is_some()
        || patch.codex_auth_json.is_some()
        || patch.codex_config_toml.is_some()
        || patch.codex_model_catalog.is_some()
        || patch.codex_sandbox.is_some()
        || patch.grok_config_toml.is_some()
        || patch.grok_structured.is_some();
    if has_native_config {
        acp_update_agent_config_and_refresh(
            agent,
            patch.config_json,
            None,
            patch.codex_auth_json,
            patch.codex_config_toml,
            patch.codex_model_catalog,
            patch.codex_sandbox,
            patch.grok_config_toml,
            patch.grok_structured,
            None,
            None,
            &state.db,
            &state.connection_manager,
            &state.data_dir,
            &state.emitter,
        )
        .await?;
    }

    get_eui_agent_settings(state, agent).await
}

async fn update_env(
    state: &AppState,
    agent: AgentType,
    enabled: bool,
    env: BTreeMap<String, String>,
    model_provider_id: Option<i32>,
) -> Result<(), EuiFacadeError> {
    acp_update_agent_env_and_refresh(
        agent,
        enabled,
        env,
        model_provider_id,
        &state.db,
        &state.connection_manager,
        &state.data_dir,
        &state.emitter,
    )
    .await?;
    Ok(())
}

pub async fn probe_eui_agent(
    state: &AppState,
    agent: AgentType,
) -> Result<EuiAgentProbe, EuiFacadeError> {
    ensure_supported(agent)?;
    let preflight = acp_preflight_core(agent, Some(true), &state.db).await?;
    let installed_version = get_eui_agent_settings(state, agent)
        .await?
        .installed_version;
    let message = preflight
        .checks
        .iter()
        .filter(|check| !matches!(check.status, CheckStatus::Pass))
        .map(|check| check.message.clone())
        .collect::<Vec<_>>()
        .join("; ");
    Ok(EuiAgentProbe {
        launchable: preflight.passed,
        installed_version,
        message,
    })
}

fn ensure_supported(agent: AgentType) -> Result<(), EuiFacadeError> {
    match agent {
        AgentType::Codex | AgentType::Grok => Ok(()),
        _ => Err(EuiFacadeError::UnsupportedAgent(
            agent.as_wire().into_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex};

    use super::{
        bind_eui_connection, create_eui_conversation, create_eui_session_with_ops,
        ensure_supported, load_eui_session, parse_supported_agent, project_agent_settings,
        select_eui_session_with_ops, send_eui_message, send_eui_message_with_ops,
        set_eui_workspace, EuiAgentSettingsPatch, EuiFacadeError, EuiSessionOps,
        EuiSessionSelection,
    };
    use crate::acp::connection::ConnectionCommand;
    use crate::acp::types::AcpAgentInfo;
    use crate::app_state::AppState;
    use crate::db::service::{conversation_service, folder_service};
    use crate::db::test_helpers::fresh_disk_db;
    use crate::models::agent::AgentType;

    async fn eui_test_state(root: &std::path::Path) -> AppState {
        let db = fresh_disk_db(root).await;
        AppState::new_for_test(db, root.to_path_buf())
    }

    #[derive(Clone, Default)]
    struct RecordingSessionOps {
        calls: Arc<Mutex<Vec<&'static str>>>,
        last_send: Arc<Mutex<Option<(String, i32, i32, String, String)>>>,
        live_connections: Arc<Mutex<HashMap<i32, String>>>,
    }

    impl RecordingSessionOps {
        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: &'static str) {
            self.calls.lock().unwrap().push(call);
        }
    }

    #[async_trait::async_trait]
    impl EuiSessionOps for RecordingSessionOps {
        type LaunchInputs = (AgentType, Option<String>, i32);

        async fn verify_installed(&self, agent_type: AgentType) -> Result<(), EuiFacadeError> {
            assert!(matches!(agent_type, AgentType::Codex | AgentType::Grok));
            self.record("verify_installed");
            Ok(())
        }

        async fn build_launch_inputs(
            &self,
            _state: &AppState,
            agent_type: AgentType,
            external_session_id: Option<&str>,
            conversation_id: i32,
        ) -> Result<Self::LaunchInputs, EuiFacadeError> {
            assert!(conversation_id > 0);
            self.record("build_launch_inputs");
            Ok((
                agent_type,
                external_session_id.map(str::to_string),
                conversation_id,
            ))
        }

        async fn spawn_agent(
            &self,
            _state: &AppState,
            agent_type: AgentType,
            workspace_path: &std::path::Path,
            external_session_id: Option<String>,
            conversation_id: i32,
            launch_inputs: Self::LaunchInputs,
            owner: &str,
        ) -> Result<String, EuiFacadeError> {
            assert!(workspace_path.is_absolute());
            assert_eq!(owner, "eui");
            assert_eq!(
                launch_inputs,
                (agent_type, external_session_id, conversation_id)
            );
            self.record("spawn_agent");
            Ok("recorded-connection".to_string())
        }

        async fn find_connection(&self, _state: &AppState, conversation_id: i32) -> Option<String> {
            self.record("find_connection");
            self.live_connections
                .lock()
                .unwrap()
                .get(&conversation_id)
                .cloned()
        }

        async fn bind_connection(
            &self,
            _state: &AppState,
            connection_id: &str,
            _folder_id: i32,
            conversation_id: i32,
        ) -> Result<(), EuiFacadeError> {
            self.record("bind_connection");
            self.live_connections
                .lock()
                .unwrap()
                .insert(conversation_id, connection_id.to_string());
            Ok(())
        }

        async fn send_linked(
            &self,
            _state: &AppState,
            connection_id: &str,
            blocks: Vec<crate::acp::types::PromptInputBlock>,
            folder_id: i32,
            conversation_id: i32,
            client_message_id: String,
        ) -> Result<(), EuiFacadeError> {
            let [crate::acp::types::PromptInputBlock::Text { text }] = blocks.as_slice() else {
                panic!("EUI send must contain exactly one text block");
            };
            self.record("send_prompt_linked");
            *self.last_send.lock().unwrap() = Some((
                connection_id.to_string(),
                folder_id,
                conversation_id,
                client_message_id,
                text.clone(),
            ));
            Ok(())
        }
    }

    #[tokio::test]
    async fn create_session_verifies_builds_then_spawns_with_eui_ownership() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;
        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
        let ops = RecordingSessionOps::default();

        let selection = create_eui_session_with_ops(&state, &workspace, AgentType::Codex, &ops)
            .await
            .unwrap();

        assert_eq!(
            ops.calls(),
            [
                "verify_installed",
                "build_launch_inputs",
                "spawn_agent",
                "bind_connection"
            ]
        );
        assert!(selection.conversation_id > 0);
        assert_eq!(selection.connection_id, "recorded-connection");

        ops.calls.lock().unwrap().clear();
        send_eui_message_with_ops(&state, &selection, "hello".to_string(), &ops)
            .await
            .unwrap();
        assert_eq!(ops.calls(), ["send_prompt_linked"]);
        let send = ops.last_send.lock().unwrap().clone().unwrap();
        assert_eq!(send.0, selection.connection_id);
        assert_eq!(send.1, selection.folder_id);
        assert_eq!(send.2, selection.conversation_id);
        assert!(uuid::Uuid::parse_str(&send.3).is_ok());
        assert_eq!(send.4, "hello");
    }

    #[tokio::test]
    async fn workspace_and_conversation_reuse_existing_database_cores() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;

        let workspace = set_eui_workspace(&state, workspace_dir.clone())
            .await
            .unwrap();
        assert_eq!(
            workspace.path,
            std::fs::canonicalize(&workspace_dir).unwrap()
        );
        let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Grok)
            .await
            .unwrap();
        assert!(row.conversation_id > 0);
        assert_eq!(row.agent_type, AgentType::Grok);

        let rows = conversation_service::list_by_folder(
            &state.db.conn,
            workspace.folder_id,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[tokio::test]
    async fn workspace_list_contains_only_supported_regular_sessions() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;
        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();

        let eligible = conversation_service::create(
            &state.db.conn,
            workspace.folder_id,
            AgentType::Codex,
            None,
            None,
        )
        .await
        .unwrap();
        conversation_service::create(
            &state.db.conn,
            workspace.folder_id,
            AgentType::ClaudeCode,
            None,
            None,
        )
        .await
        .unwrap();
        conversation_service::create_chat(
            &state.db.conn,
            workspace.folder_id,
            AgentType::Grok,
            None,
            None,
        )
        .await
        .unwrap();

        let workspace = set_eui_workspace(&state, workspace.path).await.unwrap();
        assert_eq!(
            workspace
                .sessions
                .iter()
                .map(|session| session.conversation_id)
                .collect::<Vec<_>>(),
            [eligible.id]
        );
    }

    #[tokio::test]
    async fn invalid_workspace_does_not_create_a_folder_row() {
        let root = tempfile::tempdir().unwrap();
        let state = eui_test_state(root.path()).await;
        let file = root.path().join("file.txt");
        std::fs::write(&file, b"not a directory").unwrap();

        assert!(matches!(
            set_eui_workspace(&state, file).await,
            Err(EuiFacadeError::InvalidWorkspace { .. })
        ));
        assert!(matches!(
            set_eui_workspace(&state, root.path().join("missing")).await,
            Err(EuiFacadeError::InvalidWorkspace { .. })
        ));
        assert!(folder_service::list_folders(&state.db.conn)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn only_codex_and_grok_conversations_are_created() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;
        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();

        for agent in [AgentType::Codex, AgentType::Grok] {
            assert_eq!(
                create_eui_conversation(&state, workspace.folder_id, agent)
                    .await
                    .unwrap()
                    .agent_type,
                agent
            );
        }
        assert!(matches!(
            create_eui_conversation(&state, workspace.folder_id, AgentType::ClaudeCode).await,
            Err(EuiFacadeError::UnsupportedAgent(_))
        ));
        let rows = conversation_service::list_by_folder(
            &state.db.conn,
            workspace.folder_id,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn selection_rejects_ineligible_rows_before_connection_lookup() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;
        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
        let unsupported = conversation_service::create(
            &state.db.conn,
            workspace.folder_id,
            AgentType::ClaudeCode,
            None,
            None,
        )
        .await
        .unwrap();
        let non_regular = conversation_service::create_chat(
            &state.db.conn,
            workspace.folder_id,
            AgentType::Codex,
            None,
            None,
        )
        .await
        .unwrap();
        let ops = RecordingSessionOps::default();
        ops.live_connections
            .lock()
            .unwrap()
            .insert(unsupported.id, "unsupported-live-connection".to_string());

        assert!(matches!(
            select_eui_session_with_ops(&state, &workspace, unsupported.id, &ops).await,
            Err(EuiFacadeError::IneligibleConversation { conversation_id })
                if conversation_id == unsupported.id
        ));
        assert!(ops.calls().is_empty());

        assert!(matches!(
            select_eui_session_with_ops(&state, &workspace, non_regular.id, &ops).await,
            Err(EuiFacadeError::IneligibleConversation { conversation_id })
                if conversation_id == non_regular.id
        ));
        assert!(ops.calls().is_empty());
    }

    #[tokio::test]
    async fn create_then_select_before_send_reuses_the_spawned_connection() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;
        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
        let ops = RecordingSessionOps::default();

        let created = create_eui_session_with_ops(&state, &workspace, AgentType::Codex, &ops)
            .await
            .unwrap();
        ops.calls.lock().unwrap().clear();

        let selected =
            select_eui_session_with_ops(&state, &workspace, created.conversation_id, &ops)
                .await
                .unwrap();

        assert_eq!(selected.connection_id, created.connection_id);
        assert_eq!(ops.calls(), ["find_connection"]);
    }

    #[tokio::test]
    async fn connection_binding_makes_a_spawn_discoverable_before_send() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;
        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
        let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Codex)
            .await
            .unwrap();
        let _commands = state
            .connection_manager
            .insert_test_connection_live(
                "eui-pre-send-connection",
                AgentType::Codex,
                Some(workspace.path),
                state.emitter.clone(),
            )
            .await;

        bind_eui_connection(
            &state,
            "eui-pre-send-connection",
            workspace.folder_id,
            row.conversation_id,
        )
        .await
        .unwrap();

        assert_eq!(
            state
                .connection_manager
                .find_connection_by_conversation_id(row.conversation_id)
                .await
                .as_deref(),
            Some("eui-pre-send-connection")
        );
    }

    #[tokio::test]
    async fn history_projection_is_backend_message_turn_json() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;
        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
        let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Codex)
            .await
            .unwrap();

        let loaded = load_eui_session(&state, &workspace, row.conversation_id)
            .await
            .unwrap();
        assert_eq!(loaded.summary, row);
        assert_eq!(
            serde_json::to_value(&loaded.transcript).unwrap(),
            serde_json::json!([])
        );
    }

    #[tokio::test]
    async fn send_uses_one_text_block_and_binds_the_selected_ids() {
        let root = tempfile::tempdir().unwrap();
        let workspace_dir = root.path().join("workspace");
        std::fs::create_dir(&workspace_dir).unwrap();
        let state = eui_test_state(root.path()).await;
        let workspace = set_eui_workspace(&state, workspace_dir).await.unwrap();
        let row = create_eui_conversation(&state, workspace.folder_id, AgentType::Codex)
            .await
            .unwrap();
        let mut commands = state
            .connection_manager
            .insert_test_connection_live(
                "eui-test-connection",
                AgentType::Codex,
                Some(workspace.path.clone()),
                state.emitter.clone(),
            )
            .await;
        let selection = EuiSessionSelection {
            folder_id: workspace.folder_id,
            path: workspace.path,
            conversation_id: row.conversation_id,
            title: row.title,
            agent_type: row.agent_type,
            status: row.status,
            external_session_id: row.external_session_id,
            updated_at_ms: row.updated_at_ms,
            connection_id: "eui-test-connection".to_string(),
            transcript: Vec::new(),
        };

        send_eui_message(&state, &selection, "hello".to_string())
            .await
            .unwrap();

        let command = commands.recv().await.expect("one prompt command");
        let ConnectionCommand::Prompt {
            blocks,
            user_message,
            ..
        } = command
        else {
            panic!("expected prompt command");
        };
        assert!(matches!(
            blocks.as_slice(),
            [crate::acp::types::PromptInputBlock::Text { text }] if text == "hello"
        ));
        let message_id = user_message.expect("linked user message").0;
        assert!(uuid::Uuid::parse_str(&message_id).is_ok());
        assert_eq!(
            state
                .connection_manager
                .get_state("eui-test-connection")
                .await
                .unwrap()
                .read()
                .await
                .conversation_id,
            Some(selection.conversation_id)
        );
    }

    #[test]
    fn only_codex_and_grok_wire_values_are_supported() {
        assert_eq!(parse_supported_agent("codex").unwrap(), AgentType::Codex);
        assert_eq!(parse_supported_agent("grok").unwrap(), AgentType::Grok);
        assert!(matches!(
            parse_supported_agent("claude"),
            Err(EuiFacadeError::UnsupportedAgent(_))
        ));
    }

    #[test]
    fn unsupported_typed_agent_is_rejected_by_the_pre_access_guard() {
        assert!(matches!(
            ensure_supported(AgentType::ClaudeCode),
            Err(EuiFacadeError::UnsupportedAgent(_))
        ));
    }

    #[test]
    fn projected_codex_settings_do_not_expose_grok_fields() {
        let info = AcpAgentInfo {
            agent_type: AgentType::Codex,
            skills_capable: false,
            registry_id: "codex".into(),
            registry_version: None,
            name: "Codex".into(),
            description: String::new(),
            available: true,
            distribution_type: "npx".into(),
            custom_source: None,
            enabled: true,
            show_thinking: false,
            sort_order: 0,
            installed_version: Some("1.0.0".into()),
            env: BTreeMap::from([(String::from("OPENAI_API_KEY"), String::from("secret"))]),
            config_json: None,
            config_file_path: None,
            opencode_auth_json: None,
            codex_auth_json: Some(String::from("{}")),
            codex_config_toml: Some(String::from("model = \"gpt-5\"\n")),
            codex_model_catalog: None,
            codex_sandbox_settings: None,
            cline_secrets_json: None,
            hermes_config_yaml: None,
            grok_config_toml: Some(String::from("must be omitted")),
            grok_settings: None,
            cursor_cli_config_json: None,
            cursor_settings: None,
            model_provider_id: Some(7),
            icon_url: None,
        };

        let projected = project_agent_settings(info);
        assert_eq!(projected.agent_type, AgentType::Codex);
        assert_eq!(
            projected.codex_config_toml.as_deref(),
            Some("model = \"gpt-5\"\n")
        );
        assert!(projected.grok_config_toml.is_none());
        assert!(projected.grok_settings.is_none());
    }

    #[test]
    fn patch_rejects_fields_owned_by_the_other_agent() {
        let patch = EuiAgentSettingsPatch {
            grok_config_toml: Some(String::from("[ui]\n")),
            ..Default::default()
        };
        assert!(matches!(
            patch.validate_for(AgentType::Codex),
            Err(EuiFacadeError::AgentFieldConflict { .. })
        ));
    }
}
