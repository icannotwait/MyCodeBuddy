use crate::models::agent::AgentType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomousActivityPolicy {
    ClaudeTranscript,
    GrokIdleWire,
    CodexGoalTranscript,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutonomousCapabilities {
    pub goal_version: Option<u32>,
    pub load_session: bool,
}

impl AutonomousActivityPolicy {
    pub fn for_connection(agent: AgentType, caps: &AutonomousCapabilities) -> Self {
        match agent {
            AgentType::ClaudeCode => Self::ClaudeTranscript,
            AgentType::Grok => Self::GrokIdleWire,
            AgentType::Codex if caps.goal_version == Some(1) && caps.load_session => {
                Self::CodexGoalTranscript
            }
            _ => Self::Unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AutonomousActivityPolicy, AutonomousCapabilities};
    use crate::models::agent::AgentType;

    #[test]
    fn claude_maps_to_transcript() {
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::ClaudeCode,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::ClaudeTranscript
        );
    }

    #[test]
    fn grok_maps_to_idle_wire() {
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Grok,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::GrokIdleWire
        );
    }

    #[test]
    fn codex_requires_goal_v1_and_load_session() {
        let qualified = AutonomousCapabilities {
            goal_version: Some(1),
            load_session: true,
        };
        assert_eq!(
            AutonomousActivityPolicy::for_connection(AgentType::Codex, &qualified),
            AutonomousActivityPolicy::CodexGoalTranscript
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities {
                    goal_version: Some(1),
                    load_session: false,
                }
            ),
            AutonomousActivityPolicy::Unsupported
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities {
                    goal_version: Some(2),
                    load_session: true,
                }
            ),
            AutonomousActivityPolicy::Unsupported
        );
        assert_eq!(
            AutonomousActivityPolicy::for_connection(
                AgentType::Codex,
                &AutonomousCapabilities::default()
            ),
            AutonomousActivityPolicy::Unsupported
        );
    }

    #[test]
    fn custom_codex_and_other_builtins_are_unsupported() {
        let qualified = AutonomousCapabilities {
            goal_version: Some(1),
            load_session: true,
        };
        for agent in [
            AgentType::Cursor,
            AgentType::OpenCode,
            AgentType::Gemini,
            AgentType::Cline,
            AgentType::Hermes,
            AgentType::CodeBuddy,
            AgentType::KimiCode,
            AgentType::Pi,
            AgentType::DeepSeek,
            AgentType::Custom("codex"),
        ] {
            assert_eq!(
                AutonomousActivityPolicy::for_connection(agent, &qualified),
                AutonomousActivityPolicy::Unsupported,
                "{agent:?}"
            );
        }
    }
}
