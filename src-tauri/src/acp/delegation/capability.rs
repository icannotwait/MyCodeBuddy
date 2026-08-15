//! Per-agent capability gate for child session reuse (`continue_delegation`).
//!
//! Initial `delegate_to_agent` is **never** gated by this check. Only the
//! continue / resume path consults [`agent_supports_session_reuse`]. When the
//! agent type is not enabled, callers return wire code `not_supported` without
//! attempting `session/resume` or `session/load`.

use crate::models::AgentType;

/// Whether this agent type is eligible to attempt durable child session reuse
/// (same external id resume/load after disconnect).
///
/// This is the coarse rollout gate. The spawned agent's initialize response is
/// still authoritative: `ResumeExistingOnly` refuses `unresumable` when neither
/// `session/resume` nor `loadSession` is advertised and never falls through to
/// `session/new`. This keeps older cached or PATH-installed CLIs fail-closed.
/// All agent types outside the rollout return `false`, so continue paths stop
/// earlier with `not_supported`.
pub fn agent_supports_session_reuse(agent_type: AgentType) -> bool {
    matches!(
        agent_type,
        AgentType::Codex
            | AgentType::ClaudeCode
            | AgentType::CodeBuddy
            | AgentType::Grok
            | AgentType::Cursor
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
        assert!(!agent_supports_session_reuse(AgentType::OpenCode));
        assert!(!agent_supports_session_reuse(AgentType::Gemini));
        assert!(!agent_supports_session_reuse(AgentType::Cline));
        assert!(!agent_supports_session_reuse(AgentType::Hermes));
        assert!(!agent_supports_session_reuse(AgentType::KimiCode));
        assert!(!agent_supports_session_reuse(AgentType::Pi));
    }

    #[test]
    fn capability_true_for_reuse_rollout_agents() {
        assert!(agent_supports_session_reuse(AgentType::Codex));
        assert!(agent_supports_session_reuse(AgentType::ClaudeCode));
        assert!(agent_supports_session_reuse(AgentType::CodeBuddy));
        assert!(agent_supports_session_reuse(AgentType::Grok));
        assert!(agent_supports_session_reuse(AgentType::Cursor));
    }

    #[test]
    fn continue_gate_allows_cursor_and_rejects_unsupported_agents() {
        assert_eq!(
            gate_continue_session_reuse(AgentType::Cursor),
            ContinueCapabilityDecision::Allowed
        );
        assert_eq!(
            gate_continue_session_reuse(AgentType::OpenCode),
            ContinueCapabilityDecision::NotSupported
        );
    }
}
