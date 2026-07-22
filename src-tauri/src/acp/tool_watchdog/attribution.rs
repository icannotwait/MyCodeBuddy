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
        self.record_terminal_offset_inner(turn, tool_call_id, None, next_offset, at)
            .await
    }

    /// Record progress for a specific associated terminal. Multi-terminal tools
    /// must use this so a lower-offset peer can still renew the lease.
    pub async fn record_terminal_offset_for(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
        terminal_id: &str,
        next_offset: u64,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        let hash = {
            let mut hasher = DefaultHasher::new();
            terminal_id.hash(&mut hasher);
            hasher.finish()
        };
        self.record_terminal_offset_inner(turn, tool_call_id, Some(hash), next_offset, at)
            .await
    }

    async fn record_terminal_offset_inner(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
        terminal_id_hash: Option<u64>,
        next_offset: u64,
        at: WatchdogInstant,
    ) -> Option<LeaseStamp> {
        self.registry
            .record_tool_progress_at(
                tool_progress_key(turn, tool_call_id),
                SemanticProgress::TerminalOffset {
                    terminal_id_hash,
                    next_offset,
                },
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
    ///
    /// Callers must pass a stamp that is still current (for example the fresh
    /// stamp returned by status progress). Prefer
    /// [`Self::sync_terminal_association`] when binding from the accumulated
    /// host association map.
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

    /// Derive cancellation capability from the **accumulated** terminal
    /// association for a tool (not just the current frame):
    /// - exact singleton → `Terminal`
    /// - multi-id (ambiguous) → force `Turn` (downgrade if previously Terminal)
    /// - empty → no capability mutation
    ///
    /// Looks up the current lease stamp so status progress that advanced the
    /// version does not cause a stale bind.
    pub async fn sync_terminal_association(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
        session_id: &str,
        terminal_ids: &[String],
    ) -> Option<LeaseStamp> {
        if terminal_ids.is_empty() {
            return None;
        }
        let stamp = self
            .registry
            .tool_stamp(&tool_lease_key(turn, tool_call_id))
            .await?;
        let capability = match unambiguous_terminal_id(terminal_ids) {
            Some(terminal_id) => CancellationCapability::Terminal {
                session_id: session_id.to_string(),
                terminal_id: terminal_id.to_string(),
            },
            None => CancellationCapability::Turn,
        };
        self.registry
            .bind_capability(&stamp, capability)
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

    /// Prefer request-scoped wait cancel over child cancel for multi-task waits.
    pub async fn bind_delegation_wait(
        &self,
        stamp: &LeaseStamp,
        wait_id: &str,
    ) -> Option<LeaseStamp> {
        self.registry
            .bind_capability(
                stamp,
                CancellationCapability::DelegationWait {
                    wait_id: wait_id.to_string(),
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

    pub async fn complete_tool(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
    ) -> Option<crate::acp::tool_watchdog::ToolWatchdogProjection> {
        self.registry
            .complete_tool(&tool_lease_key(turn, tool_call_id))
            .await
    }

    /// Acknowledged background handoff: complete the exact foreground lease for
    /// `tool_call_id` on `turn`, and only then mark verified background work.
    ///
    /// A mismatched turn (delayed ack after the next prompt) or unknown tool id
    /// is a no-op — it must not suppress the current turn's untracked fallback.
    pub async fn background_handoff(&self, turn: &TurnStamp, tool_call_id: &str) -> bool {
        let completed = self
            .registry
            .complete_tool(&tool_lease_key(turn, tool_call_id))
            .await;
        if completed.is_some() {
            self.registry
                .set_verified_background_work(turn, true)
                .await;
            true
        } else {
            false
        }
    }

    pub async fn complete_turn(
        &self,
        turn: &TurnStamp,
    ) -> Vec<crate::acp::tool_watchdog::ToolWatchdogProjection> {
        self.registry.complete_turn(turn).await
    }

    /// Close lease admission for an incarnation (before clear / map remove).
    pub async fn fence_connection(&self, connection_id: &str, incarnation: &str) {
        self.registry
            .fence_connection(connection_id, incarnation)
            .await;
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
        let version_before = stamp.version;
        let phase_before = attr
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
            Some(phase_before)
        );
        let old_stamp_after = attr
            .registry()
            .lease_stamp(&stamp.lease_id)
            .await
            .expect("old lease stamp");
        assert_eq!(
            old_stamp_after.version, version_before,
            "new incarnation must not bump old lease version"
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
            attr.registry().lease_capability(&stamp.lease_id).await,
            Some(CancellationCapability::Turn)
        );
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
        let b_version_before = b.version;

        let a_after = attr
            .record_status(&turn, "tool-a", "in_progress", t0.advanced(5))
            .await
            .expect("renew a");
        assert_eq!(a_after.lease_id, a.lease_id);
        assert!(a_after.version > a.version);

        // B unchanged (still version 1) — prove via version, not only phase.
        assert_eq!(
            attr.registry().lease_phase(&b.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
        let b_stamp = attr
            .registry()
            .lease_stamp(&b.lease_id)
            .await
            .expect("b stamp");
        assert_eq!(
            b_stamp.version, b_version_before,
            "parallel sibling must not have its version advanced"
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
        assert!(b_first.version > b_version_before);
    }

    #[tokio::test]
    async fn tool_watchdog_attribution_terminal_offsets_renew_across_truncation() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        // Live ToolCall order: register → status progress → bind with FRESH stamp.
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-term", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        let after_status = attr
            .record_status(&turn, "tool-term", "inprogress", t0.advanced(1))
            .await
            .expect("status bumps version");
        assert!(after_status.version > stamp.version);
        // Stale pre-status stamp must fail closed.
        assert!(
            attr.bind_terminal_if_unambiguous(&stamp, "sess-1", &["term-1".into()])
                .await
                .is_none(),
            "bind with pre-status stamp must be rejected"
        );
        let bound = attr
            .bind_terminal_if_unambiguous(&after_status, "sess-1", &["term-1".into()])
            .await
            .expect("unambiguous bind with fresh stamp");
        assert!(bound.version > after_status.version);
        assert_eq!(
            attr.registry().lease_capability(&bound.lease_id).await,
            Some(CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "term-1".into(),
            })
        );

        let p1 = attr
            .record_terminal_offset(&turn, "tool-term", 100, t0.advanced(2))
            .await
            .expect("first offset");
        // Truncation can reset the buffer window but next_offset is still
        // monotonic for the host association — a higher offset renews.
        let p2 = attr
            .record_terminal_offset(&turn, "tool-term", 250, t0.advanced(3))
            .await
            .expect("post-truncation advance");
        assert!(p2.version > p1.version);

        // Unchanged offset does not renew.
        assert!(attr
            .record_terminal_offset(&turn, "tool-term", 250, t0.advanced(4))
            .await
            .is_none());
        // Capability survives offset renewals.
        assert_eq!(
            attr.registry().lease_capability(&p2.lease_id).await,
            Some(CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "term-1".into(),
            })
        );
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
        let tool_version = tool.version;
        assert!(!attr.registry().has_fallback(&turn).await);
        attr.record_agent_activity(&turn, "more thinking", t0.advanced(12))
            .await;
        // Tool lease version unchanged (agent activity is turn-level only).
        assert_eq!(
            attr.registry().lease_phase(&tool.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
        assert_eq!(
            attr.registry()
                .lease_stamp(&tool.lease_id)
                .await
                .map(|s| s.version),
            Some(tool_version),
            "generic agent activity must not renew tracked tool version"
        );
        // Recording status on tool still works independently.
        let status_renewed = attr
            .record_status(&turn, "tool-x", "inprogress", t0.advanced(13))
            .await
            .expect("status renews tool");
        assert!(status_renewed.version > tool_version);
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

        assert!(
            attr.background_handoff(&turn, "tool-bg").await,
            "exact lease handoff must succeed"
        );

        // Foreground lease gone.
        assert!(attr.registry().lease_phase(&stamp.lease_id).await.is_none());
        // Fallback must not re-arm while background accounts for the turn.
        assert!(!attr.registry().has_fallback(&turn).await);
    }

    /// Delayed Claude ack after the next prompt must not suppress gen N+1 fallback.
    #[tokio::test]
    async fn tool_watchdog_attribution_delayed_handoff_does_not_touch_next_turn() {
        let attr = attribution();
        let t0 = clock_base();
        let gen1 = turn_a();
        attr.start_turn(gen1.clone(), t0).await;
        let old = attr
            .register_or_touch_tool(&gen1, "tool-bg", ToolCategory::Other, t0)
            .await
            .unwrap();

        // Generation ends; next prompt starts with its own untracked fallback.
        attr.complete_turn(&gen1).await;
        let gen2 = turn_stamp("conn-1", "inc-a", "sess-1", 2);
        attr.start_turn(gen2.clone(), t0.advanced(1)).await;
        assert!(attr.registry().has_fallback(&gen2).await);
        assert!(attr.registry().lease_phase(&old.lease_id).await.is_none());

        // Bug pattern: apply delayed ack against *current* turn with unmatched id.
        assert!(
            !attr.background_handoff(&gen2, "tool-bg").await,
            "handoff without exact live lease must fail closed"
        );
        assert!(
            attr.registry().has_fallback(&gen2).await,
            "unmatched handoff must not set verified_background on the new turn"
        );

        // Originating-turn stamp after complete is also a no-op (lease already gone).
        assert!(!attr.background_handoff(&gen1, "tool-bg").await);
        assert!(attr.registry().has_fallback(&gen2).await);
    }

    /// Live multi-terminal progress: lower-offset peer advances must renew.
    #[tokio::test]
    async fn tool_watchdog_attribution_multi_terminal_peer_offset_renews() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-multi", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        // Accumulated association is multi-id → Turn capability.
        attr.sync_terminal_association(
            &turn,
            "tool-multi",
            "sess-1",
            &["term-a".into(), "term-b".into()],
        )
        .await
        .expect("multi-id sync");

        let a_high = attr
            .record_terminal_offset_for(&turn, "tool-multi", "term-a", 1000, t0.advanced(1))
            .await
            .expect("terminal A high offset");
        assert!(a_high.version > stamp.version);

        // Terminal B advances under A's max — must still renew the lease.
        let b_low = attr
            .record_terminal_offset_for(&turn, "tool-multi", "term-b", 10, t0.advanced(2))
            .await
            .expect("terminal B first offset");
        assert!(b_low.version > a_high.version);
        let b_adv = attr
            .record_terminal_offset_for(&turn, "tool-multi", "term-b", 20, t0.advanced(3))
            .await
            .expect("terminal B advance under A max");
        assert!(b_adv.version > b_low.version);

        // Unchanged B does not renew.
        assert!(attr
            .record_terminal_offset_for(&turn, "tool-multi", "term-b", 20, t0.advanced(4))
            .await
            .is_none());
    }

    /// Live path must not bind capability from a single frame's terminal ids.
    /// Frame-only B after A was tracked is still Turn until accumulated sync.
    #[tokio::test]
    async fn tool_watchdog_attribution_no_frame_only_terminal_bind() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;

        // ToolCall frame with terminal A: register+status only (no bind).
        let mut stamp = attr
            .register_or_touch_tool(&turn, "tool-shell", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        if let Some(fresh) = attr
            .record_status(&turn, "tool-shell", "inprogress", t0.advanced(1))
            .await
        {
            stamp = fresh;
        }
        // Live event path no longer calls bind_terminal_if_unambiguous with
        // frame-only ids — capability stays Turn until accumulated sync.
        assert_eq!(
            attr.registry().lease_capability(&stamp.lease_id).await,
            Some(CancellationCapability::Turn)
        );

        // Accumulated after first frame: [A] → Terminal(A).
        let after_a = attr
            .sync_terminal_association(&turn, "tool-shell", "sess-1", &["term-a".into()])
            .await
            .expect("singleton accumulated sync");
        assert_eq!(
            attr.registry().lease_capability(&after_a.lease_id).await,
            Some(CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "term-a".into(),
            })
        );

        // ToolCallUpdate frame with only B: host must register/status then
        // immediately sync the *accumulated* association [A,B] (not frame-only
        // B) before any frontend await. Wrong path would bind Terminal(B) or
        // leave Terminal(A) while association is already multi.
        let _ = attr
            .register_or_touch_tool(&turn, "tool-shell", ToolCategory::Terminal, t0.advanced(2))
            .await;
        let _ = attr
            .record_status(&turn, "tool-shell", "inprogress", t0.advanced(2))
            .await;
        let multi = attr
            .sync_terminal_association(
                &turn,
                "tool-shell",
                "sess-1",
                &["term-a".into(), "term-b".into()],
            )
            .await
            .expect("multi accumulated sync immediately after admission");
        assert_eq!(
            attr.registry().lease_capability(&multi.lease_id).await,
            Some(CancellationCapability::Turn),
            "ambiguous association must be Turn before any concurrent claim/scan"
        );
    }

    /// Task 5 r3 I2: live ToolCallUpdate path must sync accumulated multi
    /// association *before* any frontend-style await. Models the host sequence
    /// register → status → sync_terminal_association([A,B]) and asserts a
    /// concurrent capability observer never samples Terminal after the host
    /// marks the multi transition complete (sync returns).
    ///
    /// Before the fix, production deferred sync until after `emit_with_state`,
    /// so a scan could copy Terminal(A) while association was already multi.
    #[tokio::test]
    async fn tool_watchdog_attribution_multi_association_claim_never_sees_terminal() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-shell", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        let after_a = attr
            .sync_terminal_association(&turn, "tool-shell", "sess-1", &["term-a".into()])
            .await
            .expect("singleton");
        assert_eq!(
            attr.registry().lease_capability(&after_a.lease_id).await,
            Some(CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "term-a".into(),
            })
        );

        let multi_applied = Arc::new(AtomicBool::new(false));
        let violated = Arc::new(AtomicBool::new(false));
        let reg = Arc::clone(attr.registry());
        let lease_id = stamp.lease_id.clone();
        let applied = Arc::clone(&multi_applied);
        let flag = Arc::clone(&violated);
        let scanner = tokio::spawn(async move {
            for _ in 0..50_000 {
                if applied.load(Ordering::SeqCst) {
                    if let Some(cap) = reg.lease_capability(&lease_id).await {
                        if matches!(cap, CancellationCapability::Terminal { .. }) {
                            flag.store(true, Ordering::SeqCst);
                            break;
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
        });

        // Fixed host path: admit tool progress then sync multi *before* the
        // simulated frontend await (yield/sleep below).
        let _ = attr
            .register_or_touch_tool(&turn, "tool-shell", ToolCategory::Terminal, t0.advanced(1))
            .await;
        let _ = attr
            .record_status(&turn, "tool-shell", "inprogress", t0.advanced(1))
            .await;
        let multi = attr
            .sync_terminal_association(
                &turn,
                "tool-shell",
                "sess-1",
                &["term-a".into(), "term-b".into()],
            )
            .await
            .expect("multi sync before frontend await");
        assert_eq!(
            attr.registry().lease_capability(&multi.lease_id).await,
            Some(CancellationCapability::Turn)
        );
        // Multi transition complete — concurrent scan must only see Turn.
        multi_applied.store(true, Ordering::SeqCst);

        // Simulated frontend await window (emit_with_state).
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;

        scanner.abort();
        let _ = scanner.await;
        assert!(
            !violated.load(Ordering::SeqCst),
            "after multi sync completes, scan must never observe Terminal capability"
        );
        assert_eq!(
            attr.registry().lease_capability(&multi.lease_id).await,
            Some(CancellationCapability::Turn)
        );
    }

    /// Fence + remove: late register_or_touch is a no-op.
    #[tokio::test]
    async fn tool_watchdog_attribution_fence_blocks_late_register() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-1", ToolCategory::Other, t0)
            .await
            .unwrap();
        attr.fence_connection("conn-1", "inc-a").await;
        attr.remove_connection("conn-1", "inc-a").await;
        assert!(attr.registry().lease_phase(&stamp.lease_id).await.is_none());
        assert!(
            attr.register_or_touch_tool(&turn, "tool-1", ToolCategory::Other, t0.advanced(1))
                .await
                .is_none(),
            "late tool event after fence must not recreate a lease"
        );
        assert!(
            attr.register_or_touch_tool(&turn, "tool-new", ToolCategory::Other, t0.advanced(1))
                .await
                .is_none()
        );
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
        assert_eq!(
            attr.registry().lease_capability(&stamp.lease_id).await,
            Some(CancellationCapability::Turn)
        );
    }

    /// Live ToolCall order: register → status → bind with post-status stamp.
    #[tokio::test]
    async fn tool_watchdog_attribution_live_toolcall_status_then_terminal_bind() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;

        let mut stamp = attr
            .register_or_touch_tool(&turn, "tool-shell", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        if let Some(fresh) = attr
            .record_status(&turn, "tool-shell", "inprogress", t0.advanced(1))
            .await
        {
            stamp = fresh;
        }
        let bound = attr
            .bind_terminal_if_unambiguous(&stamp, "sess-1", &["term-live".into()])
            .await
            .expect("Terminal capability after status progress");
        assert_eq!(
            attr.registry().lease_capability(&bound.lease_id).await,
            Some(CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "term-live".into(),
            })
        );
    }

    /// Singleton bind, then multi-id association must downgrade to Turn.
    #[tokio::test]
    async fn tool_watchdog_attribution_singleton_to_multi_downgrades_to_turn() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        attr.register_or_touch_tool(&turn, "tool-term", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        let bound = attr
            .sync_terminal_association(
                &turn,
                "tool-term",
                "sess-1",
                &["term-1".into()],
            )
            .await
            .expect("singleton Terminal");
        assert_eq!(
            attr.registry().lease_capability(&bound.lease_id).await,
            Some(CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "term-1".into(),
            })
        );

        // Accumulated association becomes multi-id across frames.
        let downgraded = attr
            .sync_terminal_association(
                &turn,
                "tool-term",
                "sess-1",
                &["term-1".into(), "term-2".into()],
            )
            .await
            .expect("multi-id forces Turn");
        assert_eq!(
            attr.registry().lease_capability(&downgraded.lease_id).await,
            Some(CancellationCapability::Turn)
        );
        assert!(downgraded.version > bound.version);
    }

    /// Fallback association path binds Terminal for a verified singleton.
    #[tokio::test]
    async fn tool_watchdog_attribution_fallback_bind_sets_terminal_capability() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        // Tool registered without content-level terminal ids (wire frame empty).
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-fallback", ToolCategory::Terminal, t0)
            .await
            .unwrap();
        assert_eq!(
            attr.registry().lease_capability(&stamp.lease_id).await,
            Some(CancellationCapability::Turn)
        );
        // Later fallback association supplies the terminal id.
        let bound = attr
            .sync_terminal_association(
                &turn,
                "tool-fallback",
                "sess-1",
                &["term-fallback".into()],
            )
            .await
            .expect("fallback singleton bind");
        assert_eq!(
            attr.registry().lease_capability(&bound.lease_id).await,
            Some(CancellationCapability::Terminal {
                session_id: "sess-1".into(),
                terminal_id: "term-fallback".into(),
            })
        );
    }

    /// Suspension-equivalent: complete_turn clears generation leases so a later
    /// resumed prompt cannot see old-generation foreground work.
    #[tokio::test]
    async fn tool_watchdog_attribution_complete_turn_before_resume_clears_old_gen() {
        let attr = attribution();
        let t0 = clock_base();
        let gen1 = turn_a();
        attr.start_turn(gen1.clone(), t0).await;
        let old = attr
            .register_or_touch_tool(&gen1, "tool-suspend", ToolCategory::Other, t0)
            .await
            .unwrap();
        // Suspension success path: complete old turn BEFORE generation clear.
        attr.complete_turn(&gen1).await;
        assert!(
            attr.registry().lease_phase(&old.lease_id).await.is_none(),
            "old-generation lease must not survive suspension handoff"
        );

        let gen2 = turn_stamp("conn-1", "inc-a", "sess-1", 2);
        attr.start_turn(gen2.clone(), t0.advanced(1)).await;
        assert!(attr.registry().has_fallback(&gen2).await);
        assert!(
            attr.registry().lease_phase(&old.lease_id).await.is_none(),
            "resumed prompt must not see old-generation lease"
        );
    }

    /// Disconnect clear is visible to scan immediately (no orphaned lease).
    #[tokio::test]
    async fn tool_watchdog_attribution_disconnect_clears_before_scan() {
        let attr = attribution();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let stamp = attr
            .register_or_touch_tool(&turn, "tool-disc", ToolCategory::Other, t0)
            .await
            .unwrap();
        // Manager path: clear incarnation, then a scan must see no actionable lease.
        attr.remove_connection("conn-1", "inc-a").await;
        assert!(attr.registry().lease_phase(&stamp.lease_id).await.is_none());
        let actions = attr.registry().scan(t0.advanced(10_000)).await;
        assert!(
            actions.is_empty(),
            "scan after disconnect clear must not act on cleared incarnation"
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
        let sibling_version = sibling.version;

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

        // Sibling untouched — version and phase.
        assert_eq!(
            attr.registry().lease_phase(&sibling.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
        assert_eq!(
            attr.registry()
                .lease_stamp(&sibling.lease_id)
                .await
                .map(|s| s.version),
            Some(sibling_version)
        );
    }
}
