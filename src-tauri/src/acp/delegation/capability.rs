//! Per-agent capability gate for child session reuse (`continue_delegation`).
//!
//! Initial `delegate_to_agent` is **never** gated by this check. Only the
//! continue / resume path consults [`agent_supports_session_reuse`]. When the
//! agent type is not enabled, callers return wire code `not_supported` without
//! attempting `session/resume` or `session/load`.

use crate::models::AgentType;

/// Whether this agent type is capability-enabled for durable child session
/// reuse (same external id resume/load after disconnect).
///
/// Initial rollout is conservative: only agent types with a managed route
/// contract and known same-id resume/load support are enabled. All others
/// return `false` so continue paths fail closed with `not_supported`.
pub fn agent_supports_session_reuse(agent_type: AgentType) -> bool {
    matches!(
        agent_type,
        AgentType::Codex | AgentType::ClaudeCode | AgentType::CodeBuddy | AgentType::Grok
    )
}

/// Continue-path gate result. Distinct from ownership/busy checks — call only
/// after the target run is loaded and parent ownership is confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinueCapabilityDecision {
    Allowed,
    /// Wire code: `not_supported`.
    NotSupported,
}

/// Apply the session-reuse capability gate for `continue_delegation`.
///
/// Must not be used on the initial `delegate_to_agent` path.
pub fn gate_continue_session_reuse(agent_type: AgentType) -> ContinueCapabilityDecision {
    if agent_supports_session_reuse(agent_type) {
        ContinueCapabilityDecision::Allowed
    } else {
        ContinueCapabilityDecision::NotSupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_false_for_agents_without_reuse_rollout() {
        assert!(!agent_supports_session_reuse(AgentType::Cursor));
        assert!(!agent_supports_session_reuse(AgentType::OpenCode));
        assert!(!agent_supports_session_reuse(AgentType::Gemini));
        assert!(!agent_supports_session_reuse(AgentType::Cline));
        assert!(!agent_supports_session_reuse(AgentType::Hermes));
        assert!(!agent_supports_session_reuse(AgentType::KimiCode));
        assert!(!agent_supports_session_reuse(AgentType::Pi));
    }

    #[test]
    fn capability_true_for_initial_reuse_rollout_agents() {
        assert!(agent_supports_session_reuse(AgentType::Codex));
        assert!(agent_supports_session_reuse(AgentType::ClaudeCode));
        assert!(agent_supports_session_reuse(AgentType::CodeBuddy));
        assert!(agent_supports_session_reuse(AgentType::Grok));
    }

    #[test]
    fn continue_gate_maps_false_to_not_supported_only() {
        assert_eq!(
            gate_continue_session_reuse(AgentType::Cursor),
            ContinueCapabilityDecision::NotSupported
        );
        assert_eq!(
            gate_continue_session_reuse(AgentType::Codex),
            ContinueCapabilityDecision::Allowed
        );
    }

    #[test]
    fn initial_delegate_is_not_gated_by_capability() {
        // Documented contract: even agents with capability=false may still
        // receive gen-1 `delegate_to_agent`. There is no gate helper for that
        // path — callers must not invoke `gate_continue_session_reuse` there.
        for agent in [
            AgentType::Cursor,
            AgentType::OpenCode,
            AgentType::Codex,
            AgentType::ClaudeCode,
        ] {
            let _ = agent_supports_session_reuse(agent); // may be true or false
                                                         // No assertion that would block gen-1 — presence of this test pins
                                                         // the API surface used only by continue.
        }
    }
}
