//! Host-side semantic progress attribution for tool-execution leases.
//!
//! Call sites feed already-normalized facts through these helpers so:
//! - parallel tools renew only themselves;
//! - terminal association upgrades capability only when unambiguous;
//! - generic agent content renews only the untracked-turn fallback;
//! - background handoff ends foreground ownership immediately.
//!
//! Full cancel supervisor (Task 6) is out of scope; this module only
//! register / progress / pause / complete / clear leases.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use super::registry::{
    RegisterTool, SemanticProgress, ToolExecutionLeaseRegistry, ToolLeaseKey, ToolProgressKey,
    TurnStamp, WatchdogInstant,
};
use super::types::{CancellationCapability, LeaseStamp, PauseReason, ToolCategory};

/// Coarse host category for a live tool call (never free-form provider titles).
pub fn classify_tool_category(kind: &str, title: Option<&str>) -> ToolCategory {
    let kind_l = kind.to_ascii_lowercase();
    let title_l = title.unwrap_or("").to_ascii_lowercase();

    // Terminal / shell family.
    if kind_l.contains("terminal")
        || kind_l == "execute"
        || kind_l == "shell"
        || title_l == "bash"
        || title_l == "shell"
        || title_l == "terminal"
        || title_l.starts_with("run_terminal")
        || title_l.starts_with("run_command")
    {
        return ToolCategory::Terminal;
    }

    // Delegation companions (codeg-mcp) before generic MCP.
    if title_l.contains("delegat")
        || title_l.contains("codeg_delegate")
        || title_l.contains("get_delegation")
        || title_l.contains("wait_for_delegation")
        || title_l.contains("cancel_delegation")
        || kind_l.contains("delegat")
    {
        return ToolCategory::Delegation;
    }

    if kind_l.contains("mcp")
        || title_l.starts_with("mcp")
        || title_l.starts_with("mcp__")
        || title_l == "use_tool"
    {
        return ToolCategory::Mcp;
    }

    ToolCategory::Other
}

/// Stable fingerprint for a tool-call status string.
pub fn status_fingerprint(status: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    status.hash(&mut hasher);
    hasher.finish()
}

/// Stable fingerprint for generic agent content / thought text.
pub fn content_hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub fn turn_stamp(
    connection_id: impl Into<String>,
    connection_incarnation: impl Into<String>,
    session_id: impl Into<String>,
    turn_generation: u64,
) -> TurnStamp {
    TurnStamp {
        connection_id: connection_id.into(),
        connection_incarnation: connection_incarnation.into(),
        session_id: session_id.into(),
        turn_generation,
    }
}

pub fn tool_lease_key(turn: &TurnStamp, tool_call_id: impl Into<String>) -> ToolLeaseKey {
    ToolLeaseKey {
        connection_id: turn.connection_id.clone(),
        connection_incarnation: turn.connection_incarnation.clone(),
        turn_generation: turn.turn_generation,
        tool_call_id: tool_call_id.into(),
    }
}

pub fn tool_progress_key(turn: &TurnStamp, tool_call_id: impl Into<String>) -> ToolProgressKey {
    ToolProgressKey {
        connection_id: turn.connection_id.clone(),
        connection_incarnation: turn.connection_incarnation.clone(),
        turn_generation: turn.turn_generation,
        tool_call_id: tool_call_id.into(),
    }
}

/// Exact one terminal id → bind Terminal capability; otherwise keep Turn only.
pub fn unambiguous_terminal_id(terminal_ids: &[String]) -> Option<&str> {
    match terminal_ids {
        [only] => Some(only.as_str()),
        _ => None,
    }
}

/// Host facade used by ConnectionManager / connection loop / delegation paths.
#[derive(Clone)]
pub struct LeaseAttribution {
    registry: Arc<ToolExecutionLeaseRegistry>,
}

impl LeaseAttribution {
    pub fn new(registry: Arc<ToolExecutionLeaseRegistry>) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> &Arc<ToolExecutionLeaseRegistry> {
        &self.registry
    }

    pub async fn start_turn(&self, turn: TurnStamp, at: WatchdogInstant) {
        self.registry.start_turn(turn, at).await;
    }

    /// Register or refresh an in-progress tool lease (exact tool call id).
    pub async fn register_or_touch_tool(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
        category: ToolCategory,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        self.registry
            .register_tool(RegisterTool {
                turn: turn.clone(),
                tool_call_id: tool_call_id.to_string(),
                category,
                at,
            })
            .await
            .ok()
    }

    pub async fn record_status(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
        status: &str,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        self.registry
            .record_tool_progress_at(
                tool_progress_key(turn, tool_call_id),
                SemanticProgress::ToolStatusChanged {
                    status_fingerprint: status_fingerprint(status),
                },
                at,
            )
            .await
    }

    pub async fn record_terminal_offset(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
        next_offset: u64,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        self.registry
            .record_tool_progress_at(
                tool_progress_key(turn, tool_call_id),
                SemanticProgress::TerminalOffset { next_offset },
                at,
            )
            .await
    }

    pub async fn record_terminal_exit(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        self.registry
            .record_tool_progress_at(
                tool_progress_key(turn, tool_call_id),
                SemanticProgress::TerminalExit,
                at,
            )
            .await
    }

    /// Bind Terminal capability only when association is a singleton.
    /// Ambiguous multi-terminal association retains Turn capability only.
    pub async fn bind_terminal_if_unambiguous(
        &self,
        stamp: &LeaseStamp,
        session_id: &str,
        terminal_ids: &[String],
    ) -> Option<LeaseStamp> {
        let terminal_id = unambiguous_terminal_id(terminal_ids)?;
        self.registry
            .bind_capability(
                stamp,
                CancellationCapability::Terminal {
                    session_id: session_id.to_string(),
                    terminal_id: terminal_id.to_string(),
                },
            )
            .await
            .ok()
    }

    pub async fn bind_delegation(
        &self,
        stamp: &LeaseStamp,
        task_id: &str,
    ) -> Option<LeaseStamp> {
        self.registry
            .bind_capability(
                stamp,
                CancellationCapability::Delegation {
                    task_id: task_id.to_string(),
                },
            )
            .await
            .ok()
    }

    /// Child activity renews the verified parent tool lease only.
    pub async fn record_delegation_activity(
        &self,
        turn: &TurnStamp,
        parent_tool_use_id: &str,
        at_mono_ms: u64,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        self.registry
            .record_tool_progress_at(
                tool_progress_key(turn, parent_tool_use_id),
                SemanticProgress::DelegationActivity { at_mono_ms },
                at,
            )
            .await
    }

    /// Generic transcript/thinking renews only the untracked fallback.
    pub async fn record_agent_activity(
        &self,
        turn: &TurnStamp,
        content: &str,
        at: WatchdogInstant,
    ) {
        self.registry
            .record_turn_progress_at(
                turn,
                SemanticProgress::AgentActivity {
                    content_hash: content_hash(content),
                },
                at,
            )
            .await;
    }

    pub async fn pause_permission(&self, turn: &TurnStamp) {
        self.registry
            .pause_turn(turn, PauseReason::Permission)
            .await;
    }

    pub async fn pause_question(&self, turn: &TurnStamp) {
        self.registry
            .pause_turn(turn, PauseReason::AgentQuestion)
            .await;
    }

    pub async fn resume(&self, turn: &TurnStamp, at: WatchdogInstant) {
        self.registry.resume_turn(turn, at).await;
    }

    pub async fn complete_tool(&self, turn: &TurnStamp, tool_call_id: &str) {
        let _ = self
            .registry
            .complete_tool(&tool_lease_key(turn, tool_call_id))
            .await;
    }

    /// Acknowledged background handoff: drop foreground ownership immediately.
    pub async fn background_handoff(&self, turn: &TurnStamp, tool_call_id: &str) {
        self.registry
            .set_verified_background_work(turn, true)
            .await;
        let _ = self
            .registry
            .complete_tool(&tool_lease_key(turn, tool_call_id))
            .await;
    }

    pub async fn complete_turn(&self, turn: &TurnStamp) {
        let _ = self.registry.complete_turn(turn).await;
    }

    pub async fn remove_connection(&self, connection_id: &str, incarnation: &str) {
        let _ = self
            .registry
            .remove_connection(connection_id, incarnation)
            .await;
    }
}

#[cfg(test)]
mod tool_watchdog_attribution_tests {
    use super::*;
    use crate::acp::tool_watchdog::types::{ToolLeasePhase, ToolWatchdogSettings};
    use chrono::{DateTime, Utc};
    use tokio::time::Instant;

    fn clock_base() -> WatchdogInstant {
        WatchdogInstant {
            mono: Instant::now(),
            wall: DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    fn turn_a() -> TurnStamp {
        turn_stamp("conn-1", "inc-a", "sess-1", 1)
    }

    fn turn_b_new_incarnation() -> TurnStamp {
        turn_stamp("conn-1", "inc-b", "sess-1", 1)
    }

    fn attribution() -> LeaseAttribution {
        LeaseAttribution::new(Arc::new(ToolExecutionLeaseRegistry::new(
            ToolWatchdogSettings::default(),
        )))
    }

    #[test]
    fn tool_watchdog_attribution_classifies_categories() {
        assert_eq!(
            classify_tool_category("terminal", Some("bash")),
            ToolCategory::Terminal
        );
        assert_eq!(
            classify_tool_category("execute", Some("shell")),
            ToolCategory::Terminal
        );
        assert_eq!(
            classify_tool_category("other", Some("mcp__codeg__delegate_task")),
            ToolCategory::Delegation
        );
        assert_eq!(
            classify_tool_category("other", Some("use_tool")),
            ToolCategory::Mcp
        );
        assert_eq!(
            classify_tool_category("read", Some("Read")),
            ToolCategory::Other
        );
    }

    #[test]
    fn tool_watchdog_attribution_unambiguous_terminal_only() {
        assert_eq!(
            unambiguous_terminal_id(&["t1".into()]),
            Some("t1")
        );
        assert_eq!(
            unambiguous_terminal_id(&["t1".into(), "t2".into()]),
            None,
            "ambiguous association stays Turn only"
        );
        assert_eq!(unambiguous_terminal_id(&[]), None);
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_new_incarnation_cannot_mutate_old_lease() {
        let attr = attribution();
        let t0 = clock_base();
        let old = turn_a();
        attr.start_turn(old.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&old, "tool-same", ToolCategory::Other, t0)
            .await
            .expect("register on old incarnation");
        let last_before = attr
            .registry()
            .lease_phase(&stamp.lease_id)
            .await
            .expect("live");

        // Same tool id on a replacement incarnation must not touch the old lease.
        let neu = turn_b_new_incarnation();
        attr.start_turn(neu.clone(), t0.advanced(1)).await;
        let _ = attr
            .register_or_touch_tool(&neu, "tool-same", ToolCategory::Other, t0.advanced(1))
            .await;
        let renewed = attr
            .record_status(&neu, "tool-same", "inprogress", t0.advanced(2))
            .await;
        assert!(renewed.is_some());

        // Old lease still Running and not advanced by the new incarnation's progress.
        assert_eq!(
            attr.registry().lease_phase(&stamp.lease_id).await,
            Some(last_before)
        );
        // Progress with the OLD stamp key from a mismatched incarnation path is a no-op:
        // the new turn uses a different incarnation key.
        let mut foreign = stamp.clone();
        foreign.connection_incarnation = "inc-b".into();
        // bind with wrong stamp version/incarnation must fail closed.
        assert!(attr
            .bind_terminal_if_unambiguous(&foreign, "sess-1", &["term-x".into()])
            .await
            .is_none());
        assert_eq!(
            attr.registry().lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_parallel_tools_renew_only_themselves() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let a = attr
            .register_or_touch_tool(&turn, "tool-a", ToolCategory::Other, t0)
            .await
            .unwrap();
        let b = attr
            .register_or_touch_tool(&turn, "tool-b", ToolCategory::Other, t0)
            .await
            .unwrap();

        let a_after = attr
            .record_status(&turn, "tool-a", "in_progress", t0.advanced(5))
            .await
            .expect("renew a");
        assert_eq!(a_after.lease_id, a.lease_id);
        assert!(a_after.version > a.version);

        // B unchanged (still version 1).
        assert_eq!(
            attr.registry().lease_phase(&b.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
        // Re-record same status on B does not bump when never progressed with that fact —
        // first status does bump; use a second identical status to prove no double renew.
        let b_first = attr
            .record_status(&turn, "tool-b", "running", t0.advanced(6))
            .await
            .unwrap();
        let b_dup = attr
            .record_status(&turn, "tool-b", "running", t0.advanced(7))
            .await;
        assert!(b_dup.is_none(), "status-only duplicate must not renew");
        assert_eq!(b_first.lease_id, b.lease_id);
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_terminal_offsets_renew_across_truncation() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-term", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        let bound = attr
            .bind_terminal_if_unambiguous(&stamp, "sess-1", &["term-1".into()])
            .await
            .expect("unambiguous bind");
        assert!(bound.version > stamp.version);

        let p1 = attr
            .record_terminal_offset(&turn, "tool-term", 100, t0.advanced(1))
            .await
            .expect("first offset");
        // Truncation can reset the buffer window but next_offset is still
        // monotonic for the host association — a higher offset renews.
        let p2 = attr
            .record_terminal_offset(&turn, "tool-term", 250, t0.advanced(2))
            .await
            .expect("post-truncation advance");
        assert!(p2.version > p1.version);

        // Unchanged offset does not renew.
        assert!(attr
            .record_terminal_offset(&turn, "tool-term", 250, t0.advanced(3))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_status_only_duplicates_do_not_renew() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        attr.register_or_touch_tool(&turn, "tool-1", ToolCategory::Other, t0)
            .await
            .unwrap();
        assert!(attr
            .record_status(&turn, "tool-1", "inprogress", t0.advanced(1))
            .await
            .is_some());
        assert!(attr
            .record_status(&turn, "tool-1", "inprogress", t0.advanced(2))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_generic_content_renews_only_untracked_turn() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        assert!(attr.registry().has_fallback(&turn).await);

        let fb = attr
            .registry()
            .fallback_stamp(&turn)
            .await
            .expect("fallback");
        attr.record_agent_activity(&turn, "thinking about architecture", t0.advanced(10))
            .await;
        // Fallback version should advance on new content hash.
        let fb2 = attr
            .registry()
            .fallback_stamp(&turn)
            .await
            .expect("fallback still");
        assert_eq!(fb2.lease_id, fb.lease_id);
        assert!(fb2.version > fb.version);

        // With a tracked tool present, agent activity must not create/renew a tool lease.
        let tool = attr
            .register_or_touch_tool(&turn, "tool-x", ToolCategory::Other, t0.advanced(11))
            .await
            .unwrap();
        assert!(!attr.registry().has_fallback(&turn).await);
        attr.record_agent_activity(&turn, "more thinking", t0.advanced(12))
            .await;
        // Tool lease version unchanged (agent activity is turn-level only).
        assert_eq!(
            attr.registry().lease_phase(&tool.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
        // Recording status on tool still works independently.
        assert!(attr
            .record_status(&turn, "tool-x", "inprogress", t0.advanced(13))
            .await
            .is_some());
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_permission_and_question_pause() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-1", ToolCategory::Other, t0)
            .await
            .unwrap();

        attr.pause_permission(&turn).await;
        assert!(matches!(
            attr.registry().lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Paused {
                reason: PauseReason::Permission
            })
        ));

        attr.resume(&turn, t0.advanced(5)).await;
        assert_eq!(
            attr.registry().lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );

        attr.pause_question(&turn).await;
        assert!(matches!(
            attr.registry().lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Paused {
                reason: PauseReason::AgentQuestion
            })
        ));
        attr.resume(&turn, t0.advanced(6)).await;
        assert_eq!(
            attr.registry().lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_background_handoff_removes_foreground() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-bg", ToolCategory::Other, t0)
            .await
            .unwrap();
        assert!(!attr.registry().has_fallback(&turn).await);

        attr.background_handoff(&turn, "tool-bg").await;

        // Foreground lease gone.
        assert!(attr.registry().lease_phase(&stamp.lease_id).await.is_none());
        // Fallback must not re-arm while background accounts for the turn.
        assert!(!attr.registry().has_fallback(&turn).await);
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_turn_complete_and_disconnect_clear_leases() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-1", ToolCategory::Other, t0)
            .await
            .unwrap();

        attr.complete_turn(&turn).await;
        assert!(attr.registry().lease_phase(&stamp.lease_id).await.is_none());
        assert!(!attr.registry().has_fallback(&turn).await);

        // Replacement/disconnect clears all leases for the incarnation.
        let turn2 = turn_stamp("conn-1", "inc-a", "sess-1", 2);
        attr.start_turn(turn2.clone(), t0.advanced(1)).await;
        let s2 = attr
            .register_or_touch_tool(&turn2, "tool-2", ToolCategory::Other, t0.advanced(1))
            .await
            .unwrap();
        attr.remove_connection("conn-1", "inc-a").await;
        assert!(attr.registry().lease_phase(&s2.lease_id).await.is_none());
        assert!(!attr.registry().has_fallback(&turn2).await);

        // New incarnation after replacement starts clean.
        let replacement = turn_b_new_incarnation();
        attr.start_turn(replacement.clone(), t0.advanced(2)).await;
        assert!(attr.registry().has_fallback(&replacement).await);
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_ambiguous_terminal_stays_turn_capability() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-term", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        assert!(attr
            .bind_terminal_if_unambiguous(
                &stamp,
                "sess-1",
                &["t1".into(), "t2".into()],
            )
            .await
            .is_none());
        // Lease still live (capability remains default Turn).
        assert_eq!(
            attr.registry().lease_phase(&stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_delegation_child_activity_hits_parent_tool() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let parent = attr
            .register_or_touch_tool(
                &turn,
                "parent-tool-use",
                ToolCategory::Delegation,
                t0,
            )
            .await
            .unwrap();
        let sibling = attr
            .register_or_touch_tool(&turn, "other-tool", ToolCategory::Other, t0)
            .await
            .unwrap();

        let renewed = attr
            .record_delegation_activity(
                &turn,
                "parent-tool-use",
                1_700_000_000_000,
                t0.advanced(3),
            )
            .await
            .expect("parent renews");
        assert_eq!(renewed.lease_id, parent.lease_id);
        assert!(renewed.version > parent.version);

        // Sibling untouched.
        assert_eq!(
            attr.registry().lease_phase(&sibling.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }
}
