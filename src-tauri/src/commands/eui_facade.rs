use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::acp::preflight::CheckStatus;
use crate::acp::types::{
    AcpAgentInfo, CodexSandboxSettings, CodexSandboxStructuredConfig, GrokSettings,
    GrokStructuredConfig,
};
use crate::app_state::AppState;
use crate::commands::acp::{
    acp_list_agents_core, acp_preflight_core, acp_update_agent_config_and_refresh,
    acp_update_agent_env_and_refresh,
};
use crate::models::agent::AgentType;

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
    #[error("ACP settings operation failed: {0}")]
    Acp(#[from] crate::acp::error::AcpError),
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
    use std::collections::BTreeMap;

    use super::{
        ensure_supported, parse_supported_agent, project_agent_settings, EuiAgentSettingsPatch,
        EuiFacadeError,
    };
    use crate::acp::types::AcpAgentInfo;
    use crate::models::agent::AgentType;

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
