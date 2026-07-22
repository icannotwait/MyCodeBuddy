//! Durable run-identity registration and settlement fence.
//!
//! Lifecycle and broker terminal paths settle by active run `task_id`, never by
//! the conversation's immutable root `delegation_call_id`. A late event from an
//! earlier connection incarnation cannot settle a newer generation.

use serde::{Deserialize, Serialize};

/// Live registration linking a child connection incarnation to a durable run.
///
/// Registered **before** prompt enqueue (admission window). Settlement requires
/// matching `(task_id, generation, child_connection_id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRunRegistration {
    pub task_id: String,
    pub generation: i64,
    pub child_connection_id: String,
    pub child_conversation_id: i32,
}

/// Whether a terminal event may settle the registered (or cold-loaded) run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementFenceDecision {
    /// All three fence keys match.
    Allow,
    /// Generation or connection incarnation mismatch — ignore the event.
    IgnoreStale,
    /// No candidate run / registration.
    NoCandidate,
}

/// Fence check for a live registration vs an incoming terminal event.
pub fn fence_allows_settlement(
    registered: Option<&LiveRunRegistration>,
    task_id: &str,
    generation: i64,
    child_connection_id: &str,
) -> SettlementFenceDecision {
    let Some(reg) = registered else {
        return SettlementFenceDecision::NoCandidate;
    };
    if reg.task_id != task_id {
        return SettlementFenceDecision::IgnoreStale;
    }
    if reg.generation != generation {
        return SettlementFenceDecision::IgnoreStale;
    }
    if reg.child_connection_id != child_connection_id {
        return SettlementFenceDecision::IgnoreStale;
    }
    SettlementFenceDecision::Allow
}

/// Cold-path: when live registration is absent, settle only a non-terminal run
/// whose persisted `child_connection_id` matches the event's connection id.
/// Never resolve by conversation root `delegation_call_id`.
pub fn cold_resolve_allows(
    _run_task_id: &str,
    run_connection_id: Option<&str>,
    run_is_non_terminal: bool,
    event_connection_id: &str,
) -> SettlementFenceDecision {
    if !run_is_non_terminal {
        return SettlementFenceDecision::NoCandidate;
    }
    match run_connection_id {
        Some(id) if id == event_connection_id => SettlementFenceDecision::Allow,
        Some(_) => SettlementFenceDecision::IgnoreStale,
        None => SettlementFenceDecision::NoCandidate,
    }
}

/// Buffered terminal source observed during the admission window (status still
/// `reserving` after connection registration, before `promote_running`).
///
/// Prefer [`AdmissionWindowTerminal::Outcome`] for any typed
/// [`crate::acp::delegation::types::DelegationOutcome`] (Ok, child_refusal,
/// max_tokens, unresumable, canceled, …) so drain preserves wire codes.
/// Bare disconnect (no typed code) uses [`AdmissionWindowTerminal::Disconnect`].
#[derive(Debug, Clone)]
pub enum AdmissionWindowTerminal {
    /// Full outcome from `complete_call_for_connection` / lifecycle TurnComplete.
    Outcome(crate::acp::delegation::types::DelegationOutcome),
    /// Bare disconnect without a typed error code.
    Disconnect { detail: Option<String> },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(task: &str, gen: i64, conn: &str, child: i32) -> LiveRunRegistration {
        LiveRunRegistration {
            task_id: task.into(),
            generation: gen,
            child_connection_id: conn.into(),
            child_conversation_id: child,
        }
    }

    #[test]
    fn fence_allows_exact_match() {
        let r = reg("task-b", 2, "conn-2", 10);
        assert_eq!(
            fence_allows_settlement(Some(&r), "task-b", 2, "conn-2"),
            SettlementFenceDecision::Allow
        );
    }

    #[test]
    fn late_old_connection_ignored() {
        let r = reg("task-b", 2, "conn-2", 10);
        assert_eq!(
            fence_allows_settlement(Some(&r), "task-b", 2, "conn-1"),
            SettlementFenceDecision::IgnoreStale
        );
    }

    #[test]
    fn late_old_generation_ignored() {
        let r = reg("task-b", 2, "conn-2", 10);
        assert_eq!(
            fence_allows_settlement(Some(&r), "task-a", 1, "conn-1"),
            SettlementFenceDecision::IgnoreStale
        );
    }

    #[test]
    fn no_registration_is_no_candidate() {
        assert_eq!(
            fence_allows_settlement(None, "task-b", 2, "conn-2"),
            SettlementFenceDecision::NoCandidate
        );
    }

    #[test]
    fn cold_resolve_match() {
        assert_eq!(
            cold_resolve_allows("task-b", Some("conn-2"), true, "conn-2"),
            SettlementFenceDecision::Allow
        );
    }

    #[test]
    fn cold_resolve_mismatch_noop() {
        assert_eq!(
            cold_resolve_allows("task-b", Some("conn-1"), true, "conn-2"),
            SettlementFenceDecision::IgnoreStale
        );
    }

    #[test]
    fn cold_resolve_terminal_or_missing_conn_noop() {
        assert_eq!(
            cold_resolve_allows("task-b", None, true, "conn-2"),
            SettlementFenceDecision::NoCandidate
        );
        assert_eq!(
            cold_resolve_allows("task-b", Some("conn-2"), false, "conn-2"),
            SettlementFenceDecision::NoCandidate
        );
    }
}
