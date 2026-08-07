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
    RegisterTool, SemanticProgress, ToolExecutionLeaseRegistry, ToolLeaseKey, ToolProgressApply,
    ToolProgressKey, TurnStamp, WatchdogInstant,
};
use super::types::{
    CancellationCapability, LeaseStamp, PauseReason, ToolCategory, ToolWatchdogProjection,
};

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
    ///
    /// On first admission, [`RegisterToolOutcome::cleared`] may carry a Cleared
    /// projection for a retired Grace/Warning fallback that hosts must emit.
    pub async fn register_or_touch_tool(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
        category: ToolCategory,
        at: WatchdogInstant,
    ) -> Option<crate::acp::tool_watchdog::RegisterToolOutcome> {
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
    ) -> Option<ToolProgressApply> {
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
    ) -> Option<ToolProgressApply> {
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
    ) -> Option<ToolProgressApply> {
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
    ) -> Option<ToolProgressApply> {
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
    ) -> Option<ToolProgressApply> {
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
        self.registry.bind_capability(&stamp, capability).await.ok()
    }

    pub async fn bind_delegation(&self, stamp: &LeaseStamp, task_id: &str) -> Option<LeaseStamp> {
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
    ///
    /// Progress tokens are allocated per-lease (monotonic sequence), never from
    /// wall-clock milliseconds — clock rollback must not reject renewals.
    pub async fn record_delegation_activity(
        &self,
        turn: &TurnStamp,
        parent_tool_use_id: &str,
        at: WatchdogInstant,
    ) -> Option<ToolProgressApply> {
        self.registry
            .record_delegation_activity(tool_progress_key(turn, parent_tool_use_id), at)
            .await
    }

    /// Verified child activity: renew a **live** launch lease (if any) and every
    /// exact-match wait lease for `task_id`.
    ///
    /// Never calls `register_or_touch_tool` or `bind_delegation` — a completed
    /// launch tool must not be resurrected or re-armed with
    /// [`CancellationCapability::Delegation`] from observation alone.
    ///
    /// Returns Cleared projections for any Warning/Grace demotion so hosts can
    /// drop them from the attach replay map.
    pub async fn renew_from_verified_child_activity(
        &self,
        wait_cancel: &crate::acp::delegation::wait_cancel::WaitCancelRegistry,
        turn: &TurnStamp,
        launch_tool_call_id: &str,
        task_id: &str,
        at: WatchdogInstant,
    ) -> Vec<ToolWatchdogProjection> {
        let mut cleared = Vec::new();

        // Live launch only (read-only stamp check). No resurrection.
        if self
            .registry
            .tool_stamp(&tool_lease_key(turn, launch_tool_call_id))
            .await
            .is_some()
        {
            if let Some(apply) = self
                .record_delegation_activity(turn, launch_tool_call_id, at)
                .await
            {
                if let Some(projection) = apply.cleared {
                    cleared.push(projection);
                }
            }
        }

        let targets = wait_cancel
            .exact_match_progress_targets(
                task_id,
                &turn.connection_id,
                &turn.connection_incarnation,
                turn.turn_generation,
            )
            .await;
        for target in targets {
            if let Some(apply) = self
                .record_delegation_activity(turn, &target.wait_tool_call_id, at)
                .await
            {
                if let Some(projection) = apply.cleared {
                    cleared.push(projection);
                }
            }
        }

        cleared
    }

    /// Generic transcript/thinking renews only the untracked fallback.
    ///
    /// Returns a Cleared projection when the fallback was demoted from
    /// Warning/Grace so hosts can drop it from the attach replay map.
    pub async fn record_agent_activity(
        &self,
        turn: &TurnStamp,
        content: &str,
        at: WatchdogInstant,
    ) -> Option<ToolWatchdogProjection> {
        self.registry
            .record_turn_progress_at(
                turn,
                SemanticProgress::AgentActivity {
                    content_hash: content_hash(content),
                },
                at,
            )
            .await
    }

    /// Pause for permission; returns Cleared projections for demoted
    /// Warning/Grace leases.
    pub async fn pause_permission(
        &self,
        turn: &TurnStamp,
    ) -> Vec<crate::acp::tool_watchdog::ToolWatchdogProjection> {
        self.registry
            .pause_turn(turn, PauseReason::Permission)
            .await
    }

    /// Pause for agent question; returns Cleared projections for demoted
    /// Warning/Grace leases.
    pub async fn pause_question(
        &self,
        turn: &TurnStamp,
    ) -> Vec<crate::acp::tool_watchdog::ToolWatchdogProjection> {
        self.registry
            .pause_turn(turn, PauseReason::AgentQuestion)
            .await
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
    /// Returns the complete projection (Cleared / TimedOut) when a lease was
    /// settled so hosts can update the attach replay map. A mismatched turn
    /// (delayed ack after the next prompt) or unknown tool id is a no-op — it
    /// must not suppress the current turn's untracked fallback.
    pub async fn background_handoff(
        &self,
        turn: &TurnStamp,
        tool_call_id: &str,
    ) -> Option<ToolWatchdogProjection> {
        let completed = self
            .registry
            .complete_tool(&tool_lease_key(turn, tool_call_id))
            .await;
        if completed.is_some() {
            self.registry.set_verified_background_work(turn, true).await;
        }
        completed
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
        assert_eq!(unambiguous_terminal_id(&["t1".into()]), Some("t1"));
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
        let mut foreign = stamp.stamp.clone();
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
            attr.background_handoff(&turn, "tool-bg").await.is_some(),
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
            attr.background_handoff(&gen2, "tool-bg").await.is_none(),
            "handoff without exact live lease must fail closed"
        );
        assert!(
            attr.registry().has_fallback(&gen2).await,
            "unmatched handoff must not set verified_background on the new turn"
        );

        // Originating-turn stamp after complete is also a no-op (lease already gone).
        assert!(attr.background_handoff(&gen1, "tool-bg").await.is_none());
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
            .unwrap()
            .stamp;
        if let Some(fresh) = attr
            .record_status(&turn, "tool-shell", "inprogress", t0.advanced(1))
            .await
        {
            stamp = fresh.stamp;
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
        assert!(attr
            .register_or_touch_tool(&turn, "tool-new", ToolCategory::Other, t0.advanced(1))
            .await
            .is_none());
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
            .bind_terminal_if_unambiguous(&stamp, "sess-1", &["t1".into(), "t2".into()],)
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
            .unwrap()
            .stamp;
        if let Some(fresh) = attr
            .record_status(&turn, "tool-shell", "inprogress", t0.advanced(1))
            .await
        {
            stamp = fresh.stamp;
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
            .sync_terminal_association(&turn, "tool-term", "sess-1", &["term-1".into()])
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
            .sync_terminal_association(&turn, "tool-fallback", "sess-1", &["term-fallback".into()])
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
            .register_or_touch_tool(&turn, "parent-tool-use", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        let sibling = attr
            .register_or_touch_tool(&turn, "other-tool", ToolCategory::Other, t0)
            .await
            .unwrap();
        let sibling_version = sibling.version;

        let renewed = attr
            .record_delegation_activity(&turn, "parent-tool-use", t0.advanced(3))
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

    /// Child-activity production path: Grace parent + verified child progress
    /// must yield Cleared that drops the lease from SessionState attach map.
    #[tokio::test]
    async fn tool_watchdog_attribution_child_activity_clears_grace_attach_map() {
        use crate::acp::session_state::SessionState;
        use crate::acp::tool_watchdog::{RegistryAction, ToolWatchdogPhase, ToolWatchdogSettings};
        use crate::acp::types::AcpEvent;

        let attr = attribution();
        // Short thresholds so we can enter Grace without long waits.
        attr.registry()
            .apply_settings(ToolWatchdogSettings {
                enabled: true,
                warning_after_seconds: 60,
                grace_seconds: 60,
            })
            .await;
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;
        let parent = attr
            .register_or_touch_tool(&turn, "parent-tool-use", ToolCategory::Delegation, t0)
            .await
            .unwrap();

        let warn_at = t0.advanced(60);
        let actions = attr.registry().scan(warn_at).await;
        let RegistryAction::PublishWarning {
            stamp: w,
            projection,
        } = &actions[0]
        else {
            panic!("expected warning for silent parent: {actions:?}");
        };
        assert_eq!(w.lease_id, parent.lease_id);
        let grace = attr
            .registry()
            .warning_published(&w.lease_id, w.version, warn_at)
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);

        // Attach map holds Grace (as production emit_tool_watchdog_changed would).
        let mut session = SessionState::new(
            "conn-1".into(),
            crate::models::AgentType::ClaudeCode,
            None,
            "main".into(),
            None,
        );
        session.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: projection.clone(),
        });
        session.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: grace.clone(),
        });
        assert_eq!(
            session.to_snapshot().tool_watchdog_projections.len(),
            1,
            "Grace must be attach-replayable before child activity"
        );
        assert!(session
            .to_snapshot()
            .tool_watchdog_projections
            .contains_key(&parent.lease_id));

        // Same registry call the event-emitter path uses for verified child activity.
        let apply = attr
            .record_delegation_activity(&turn, "parent-tool-use", warn_at.advanced(1))
            .await
            .expect("child activity renews parent");
        let cleared = apply
            .cleared
            .expect("Grace→Running must produce Cleared for attach map");
        assert_eq!(cleared.phase, ToolWatchdogPhase::Cleared);
        assert_eq!(cleared.lease_id, parent.lease_id);

        // Host emits ToolWatchdogChanged { Cleared } (event_emitter path).
        session.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: cleared,
        });
        assert!(
            session.to_snapshot().tool_watchdog_projections.is_empty(),
            "child activity clear must drop Grace so attach cannot replay stale Stop/Extend"
        );
        assert_eq!(
            attr.registry().lease_phase(&parent.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    // --- Task 3: wait-lease attribution; never resurrect completed launch ---

    use crate::acp::delegation::wait_cancel::{new_wait_cancel_channel, WaitCancelRegistry};
    use crate::acp::tool_watchdog::{
        RegistryAction, ToolWatchdogPhase, WaitCancelHandle, WaitOwner, WaitStamp,
    };

    fn wait_handle(
        wait_id: &str,
        turn: &TurnStamp,
        wait_tool_id: &str,
        task_ids: Vec<String>,
    ) -> WaitCancelHandle {
        let (tx, _rx) = new_wait_cancel_channel();
        WaitCancelHandle {
            stamp: WaitStamp {
                wait_id: wait_id.into(),
                connection_id: turn.connection_id.clone(),
                connection_incarnation: turn.connection_incarnation.clone(),
                turn_generation: turn.turn_generation,
                parent_conversation_id: 42,
                parent_tool_use_id: Some(wait_tool_id.into()),
            },
            owner: WaitOwner::Listener,
            cancel: tx,
            task_ids,
        }
    }

    /// Distinct completed launch A + live wait B: activity renews B only.
    #[tokio::test]
    async fn attribution_activity_renews_wait_only_not_completed_launch() {
        let attr = attribution();
        let wait_cancel = WaitCancelRegistry::new();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;

        // Launch A: admitted, bound Delegation, then completed (tombstoned).
        let launch = attr
            .register_or_touch_tool(&turn, "launch-A", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        let _ = attr.bind_delegation(&launch.stamp, "task-1").await;
        let launch_id = launch.lease_id.clone();
        attr.complete_tool(&turn, "launch-A").await;
        assert!(attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, "launch-A"))
            .await
            .is_none());
        assert!(
            attr.registry()
                .has_completed_tool_tombstone(&tool_lease_key(&turn, "launch-A"))
                .await
        );

        // Wait B: live status lease + exact-match registration for task-1.
        let wait = attr
            .register_or_touch_tool(&turn, "wait-B", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        let wait_version = wait.version;
        wait_cancel
            .register(wait_handle(
                "wait-1",
                &turn,
                "wait-B",
                vec!["task-1".into()],
            ))
            .await
            .unwrap();

        let cleared = attr
            .renew_from_verified_child_activity(
                &wait_cancel,
                &turn,
                "launch-A",
                "task-1",
                t0.advanced(5),
            )
            .await;
        assert!(cleared.is_empty(), "Running wait must not emit Cleared");

        // B renewed.
        let wait_after = attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, "wait-B"))
            .await
            .expect("wait lease remains live");
        assert!(
            wait_after.version > wait_version,
            "child activity must renew wait-B"
        );
        assert_eq!(
            attr.registry().lease_phase(&wait.lease_id).await,
            Some(ToolLeasePhase::Running)
        );

        // A not resurrected: still no live stamp, tombstone intact, no capability.
        assert!(
            attr.registry()
                .tool_stamp(&tool_lease_key(&turn, "launch-A"))
                .await
                .is_none(),
            "completed launch must not regain a live lease"
        );
        assert!(
            attr.registry()
                .has_completed_tool_tombstone(&tool_lease_key(&turn, "launch-A"))
                .await
        );
        assert!(
            attr.registry().lease_capability(&launch_id).await.is_none(),
            "completed launch must not re-arm Delegation capability"
        );
        assert_eq!(
            attr.registry().lease_phase(&launch_id).await,
            None,
            "completed launch lease must stay gone"
        );
    }

    /// Unrelated task_id must not renew a wait registered for another task.
    #[tokio::test]
    async fn attribution_unrelated_task_cannot_renew_wait() {
        let attr = attribution();
        let wait_cancel = WaitCancelRegistry::new();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;

        let wait = attr
            .register_or_touch_tool(&turn, "wait-B", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        let wait_version = wait.version;
        wait_cancel
            .register(wait_handle(
                "wait-1",
                &turn,
                "wait-B",
                vec!["task-1".into()],
            ))
            .await
            .unwrap();

        let _ = attr
            .renew_from_verified_child_activity(
                &wait_cancel,
                &turn,
                "launch-other",
                "task-unrelated",
                t0.advanced(5),
            )
            .await;

        let wait_after = attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, "wait-B"))
            .await
            .expect("wait still live");
        assert_eq!(
            wait_after.version, wait_version,
            "unrelated task_id must not renew wait-B"
        );
    }

    /// Activity at t+590s resets the silence clock so scan at t+600s is quiet.
    #[tokio::test]
    async fn attribution_activity_at_590s_prevents_warning_at_600s_on_wait() {
        let attr = attribution();
        let wait_cancel = WaitCancelRegistry::new();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;

        let wait = attr
            .register_or_touch_tool(&turn, "wait-B", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        wait_cancel
            .register(wait_handle(
                "wait-1",
                &turn,
                "wait-B",
                vec!["task-1".into()],
            ))
            .await
            .unwrap();

        let _ = attr
            .renew_from_verified_child_activity(
                &wait_cancel,
                &turn,
                "launch-A",
                "task-1",
                t0.advanced(590),
            )
            .await;

        let actions = attr.registry().scan(t0.advanced(600)).await;
        assert!(
            actions.iter().all(|a| !matches!(
                a,
                RegistryAction::PublishWarning { stamp, .. } if stamp.lease_id == wait.lease_id
            )),
            "renewed wait must not warn at t+600s; actions={actions:?}"
        );
        assert_eq!(
            attr.registry().lease_phase(&wait.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    /// Grace→Running on the wait lease emits exactly one Cleared.
    #[tokio::test]
    async fn attribution_activity_clears_grace_on_wait_exactly_once() {
        let attr = attribution();
        let wait_cancel = WaitCancelRegistry::new();
        attr.registry()
            .apply_settings(ToolWatchdogSettings {
                enabled: true,
                warning_after_seconds: 60,
                grace_seconds: 60,
            })
            .await;
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;

        let wait = attr
            .register_or_touch_tool(&turn, "wait-B", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        wait_cancel
            .register(wait_handle(
                "wait-1",
                &turn,
                "wait-B",
                vec!["task-1".into()],
            ))
            .await
            .unwrap();

        let warn_at = t0.advanced(60);
        let actions = attr.registry().scan(warn_at).await;
        let RegistryAction::PublishWarning { stamp: w, .. } = &actions[0] else {
            panic!("expected warning: {actions:?}");
        };
        assert_eq!(w.lease_id, wait.lease_id);
        let grace = attr
            .registry()
            .warning_published(&w.lease_id, w.version, warn_at)
            .await
            .unwrap();
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);

        let cleared = attr
            .renew_from_verified_child_activity(
                &wait_cancel,
                &turn,
                "launch-A",
                "task-1",
                warn_at.advanced(1),
            )
            .await;
        assert_eq!(cleared.len(), 1, "exactly one Cleared: {cleared:?}");
        assert_eq!(cleared[0].phase, ToolWatchdogPhase::Cleared);
        assert_eq!(cleared[0].lease_id, wait.lease_id);
        assert_eq!(
            attr.registry().lease_phase(&wait.lease_id).await,
            Some(ToolLeasePhase::Running)
        );
    }

    /// Stale turn generation or incarnation must not renew a wait.
    #[tokio::test]
    async fn attribution_stale_turn_or_incarnation_does_not_renew_wait() {
        let attr = attribution();
        let wait_cancel = WaitCancelRegistry::new();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;

        let wait = attr
            .register_or_touch_tool(&turn, "wait-B", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        let wait_version = wait.version;
        wait_cancel
            .register(wait_handle(
                "wait-1",
                &turn,
                "wait-B",
                vec!["task-1".into()],
            ))
            .await
            .unwrap();

        // Stale turn generation on the same incarnation.
        let stale_gen = turn_stamp("conn-1", "inc-a", "sess-1", 99);
        attr.start_turn(stale_gen.clone(), t0.advanced(1)).await;
        let _ = attr
            .renew_from_verified_child_activity(
                &wait_cancel,
                &stale_gen,
                "launch-A",
                "task-1",
                t0.advanced(5),
            )
            .await;

        // Stale incarnation on the same generation number.
        let stale_inc = turn_b_new_incarnation();
        attr.start_turn(stale_inc.clone(), t0.advanced(2)).await;
        let _ = attr
            .renew_from_verified_child_activity(
                &wait_cancel,
                &stale_inc,
                "launch-A",
                "task-1",
                t0.advanced(6),
            )
            .await;

        let wait_after = attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, "wait-B"))
            .await
            .expect("original wait still live under original turn");
        assert_eq!(
            wait_after.version, wait_version,
            "stale turn/incarnation must not renew wait-B"
        );
    }

    // --- Task 6 / Conversation 1570: controlled-clock wait correlation pack ---
    //
    // Layers:
    // - companion `_meta.tool_use_id` → BrokerStatusRequest.parent_tool_use_id
    //   (companion::tests::incident_1570_*)
    // - listener parks using that production field (listener::tests::incident_1570_*)
    // - this suite: launch A vs wait B, activity renews B past 1200s wall, silence
    //   → warn + grace → wait-only ClaimCancel; A never resurrected.

    /// Controlled-clock 1570 shape: activity keeps wait-B Running past the
    /// original 1,200s absolute wall; silence then warn + full grace cancel.
    /// Launch-A stays tombstoned (no resurrection / no Delegation re-arm).
    #[tokio::test]
    async fn conversation_1570_activity_keeps_wait_b_past_1200s_then_silence_timeouts() {
        let attr = attribution();
        let wait_cancel = WaitCancelRegistry::new();
        let t0 = clock_base();
        let turn = turn_a();
        attr.start_turn(turn.clone(), t0).await;

        // Launch tool A: admitted + bound Delegation, then completed (tombstone).
        let launch = attr
            .register_or_touch_tool(&turn, "launch-A", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        let launch_id = launch.lease_id.clone();
        let _ = attr.bind_delegation(&launch.stamp, "task-1").await;
        attr.complete_tool(&turn, "launch-A").await;
        assert!(attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, "launch-A"))
            .await
            .is_none());
        assert!(
            attr.registry()
                .has_completed_tool_tombstone(&tool_lease_key(&turn, "launch-A"))
                .await
        );

        // Status wait tool B: live lease + DelegationWait + exact-match registry.
        let wait = attr
            .register_or_touch_tool(&turn, "wait-B", ToolCategory::Delegation, t0)
            .await
            .unwrap();
        let wait_stamp = attr
            .bind_delegation_wait(&wait.stamp, "wait-1570")
            .await
            .expect("bind DelegationWait on wait-B");
        wait_cancel
            .register(wait_handle(
                "wait-1570",
                &turn,
                "wait-B",
                vec!["task-1".into()],
            ))
            .await
            .unwrap();

        // Publish child activity past the original 1,200s total duration while
        // each renewal is within the 600s silence window. Scan after every
        // historical deadline: wait-B must remain Running.
        let activity_points = [590u64, 1_190, 1_790];
        for secs in activity_points {
            let at = t0.advanced(secs);
            let cleared = attr
                .renew_from_verified_child_activity(&wait_cancel, &turn, "launch-A", "task-1", at)
                .await;
            assert!(
                cleared.is_empty(),
                "Running wait should not emit Cleared at t+{secs}s; cleared={cleared:?}"
            );

            // Scan at next "original wall" checkpoints while activity is fresh.
            let scan_at = t0.advanced(secs + 10);
            let actions = attr.registry().scan(scan_at).await;
            let wait_actionable = actions.iter().any(|a| match a {
                RegistryAction::PublishWarning { stamp, .. } => {
                    stamp.lease_id == wait_stamp.lease_id
                }
                RegistryAction::ClaimCancel { claim, .. } => {
                    claim.stamp.lease_id == wait_stamp.lease_id
                }
                _ => false,
            });
            assert!(
                !wait_actionable,
                "wait-B must stay Running while activity is newer than 600s silence; \
                 t+{secs}+10 actions={actions:?}"
            );
            assert_eq!(
                attr.registry().lease_phase(&wait_stamp.lease_id).await,
                Some(ToolLeasePhase::Running),
                "wait-B phase after activity at t+{secs}s"
            );

            // Launch A never resurrected across the whole activity window.
            assert!(
                attr.registry()
                    .tool_stamp(&tool_lease_key(&turn, "launch-A"))
                    .await
                    .is_none(),
                "completed launch-A must not regain a live lease at t+{secs}s"
            );
            assert!(
                attr.registry()
                    .has_completed_tool_tombstone(&tool_lease_key(&turn, "launch-A"))
                    .await,
                "launch-A tombstone must remain at t+{secs}s"
            );
            assert!(
                attr.registry().lease_capability(&launch_id).await.is_none(),
                "launch-A must not re-arm Delegation capability at t+{secs}s"
            );
        }

        // Explicit past-1200s wall check: last activity at 1790 keeps B Running
        // through t+1800 (original warn+grace absolute window from t0).
        let past_wall = t0.advanced(1_800);
        let wall_actions = attr.registry().scan(past_wall).await;
        assert!(
            wall_actions.is_empty(),
            "activity at 1790 must keep wait-B quiet at t+1800; actions={wall_actions:?}"
        );
        assert_eq!(
            attr.registry().lease_phase(&wait_stamp.lease_id).await,
            Some(ToolLeasePhase::Running)
        );

        // Stop publishing activity. Silence clock starts at last progress (1790).
        let last_progress = t0.advanced(1_790);
        let warn_at = last_progress.advanced(600);
        let warn_actions = attr.registry().scan(warn_at).await;
        assert_eq!(
            warn_actions.len(),
            1,
            "exactly one action at 600s silence: {warn_actions:?}"
        );
        let RegistryAction::PublishWarning { stamp: w, .. } = &warn_actions[0] else {
            panic!("expected PublishWarning at silence+600s, got {warn_actions:?}");
        };
        assert_eq!(w.lease_id, wait_stamp.lease_id);
        assert!(
            !warn_actions
                .iter()
                .any(|a| matches!(a, RegistryAction::ClaimCancel { .. })),
            "must not ClaimCancel on the warning pass"
        );
        let grace = attr
            .registry()
            .warning_published(&w.lease_id, w.version, warn_at)
            .await
            .expect("enter grace");
        assert_eq!(grace.phase, ToolWatchdogPhase::Grace);

        // Mid-grace quiet.
        assert!(
            attr.registry().scan(warn_at.advanced(599)).await.is_empty(),
            "no cancel before full grace"
        );

        // Full 600s grace → wait-only ClaimCancel (DelegationWait, not Delegation).
        let cancel_at = warn_at.advanced(600);
        let end = attr.registry().scan(cancel_at).await;
        assert_eq!(end.len(), 1, "exactly one action at silence+1200s: {end:?}");
        let RegistryAction::ClaimCancel { claim, projection } = &end[0] else {
            panic!("expected ClaimCancel after grace, got {end:?}");
        };
        assert_eq!(claim.stamp.lease_id, wait_stamp.lease_id);
        assert_eq!(
            claim.cause,
            crate::acp::tool_watchdog::CancelCause::AutoTimeout
        );
        assert_eq!(
            claim.capability,
            crate::acp::tool_watchdog::CancellationCapability::DelegationWait {
                wait_id: "wait-1570".into(),
            }
        );
        assert_eq!(projection.phase, ToolWatchdogPhase::Cancelling);

        // Still no launch resurrection after wait timeout claim.
        assert!(attr
            .registry()
            .tool_stamp(&tool_lease_key(&turn, "launch-A"))
            .await
            .is_none());
        assert!(
            attr.registry()
                .has_completed_tool_tombstone(&tool_lease_key(&turn, "launch-A"))
                .await
        );
    }
}
