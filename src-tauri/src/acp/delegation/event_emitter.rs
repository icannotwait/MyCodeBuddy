//! `DelegationEventEmitter` — broker capability for surfacing parent-stream
//! operational delegation events (`DelegationStarted` / runtime / attention /
//! `DelegationCompleted`) to the parent's event stream.
//!
//! Parallel to [`crate::acp::delegation::meta_writer::DelegationMetaWriter`]:
//! both abstract over the broker's access to the parent connection's
//! `(state, emitter)` pair so the broker can be unit-tested without spinning
//! up a `ConnectionManager`. Together they form the broker's two-output
//! capability surface — meta writes patch the persisted `ToolCallState`,
//! event emits drive the live frontend `DelegationContext`.
//!
//! The broker calls `emit_started` once from the start path — right after the
//! child is accepted and start publication is allowed — and `emit_completed`
//! from every terminal path. Runtime and attention replacements publish only
//! after `started_published=true` and while `terminal=false`.
//!
//! Emits are skipped when the broker is operating on a synthetic
//! `parent_tool_use_id` (the `"delegation-*"` UUID fallback) because no
//! `tool_call_id`-keyed UI exists to receive them — same guard as the meta
//! writer. The frontend's snapshot path will still recover state from the
//! broker's meta write.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;

use crate::acp::delegation::attention::AttentionRequestSummary;
use crate::acp::delegation::card_summary::CardSummary;
use crate::acp::delegation::runtime_stats::DelegationRuntimeStats;
use crate::acp::delegation::types::TaskObservation;
use crate::acp::manager::ConnectionManager;
use crate::acp::types::{AcpEvent, DelegationResultSummary};
use crate::db::entities::conversation::ConversationStatus;
use crate::models::{AgentType, ConversationStatePatch};
use crate::web::event_bridge::emit_with_state;

/// Wire string for [`ConversationStatus`] on `ConversationStatePatch.status`
/// (must match root lifecycle / conversation_service patches).
fn conversation_status_wire(status: &ConversationStatus) -> String {
    match status {
        ConversationStatus::InProgress => "in_progress".into(),
        ConversationStatus::PendingReview => "pending_review".into(),
        ConversationStatus::Completed => "completed".into(),
        ConversationStatus::Cancelled => "cancelled".into(),
    }
}

/// Sidebar fan-out for a durable child status transition. Shared by the
/// production emitter so settle wins and tests that exercise the real path
/// stay aligned with root `emit_conversation_state` callers.
fn emit_sidebar_conversation_state(
    emitter: &crate::web::event_bridge::EventEmitter,
    conversation_id: i32,
    status: ConversationStatus,
    updated_at: DateTime<Utc>,
) {
    crate::commands::conversations::emit_conversation_state(
        emitter,
        ConversationStatePatch {
            id: conversation_id,
            status: conversation_status_wire(&status),
            // Delegate rows never mint awaiting-reply tokens (lifecycle
            // fail-closed); clear on terminal so a stale token cannot linger.
            awaiting_reply_token: None,
            updated_at,
        },
    );
}

/// Capability the broker uses to publish parent-stream operational
/// delegation events.
///
/// Errors are swallowed at the impl boundary — same rationale as
/// `DelegationMetaWriter`. The broker must finish its pending-table
/// cleanup regardless of whether the parent connection is still around to
/// observe the event.
#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait DelegationEventEmitter: Send + Sync {
    /// Publish `AcpEvent::DelegationStarted` on the parent's stream once the
    /// child is accepted and start publication is allowed. Carries the full
    /// authoritative start snapshot (task id, rebased started_at, runtime,
    /// open attention). The task preview labels identity-less Cursor calls.
    async fn emit_started(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
        task_preview: &str,
        task_id: &str,
        started_at: DateTime<Utc>,
        runtime_stats: DelegationRuntimeStats,
        attention_request: Option<AttentionRequestSummary>,
    );

    async fn emit_completed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
        task_id: &str,
        runtime_stats: DelegationRuntimeStats,
        result: DelegationResultSummary,
        card_summary: Option<CardSummary>,
    );

    async fn emit_runtime_stats_changed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        task_id: &str,
        runtime_stats: DelegationRuntimeStats,
    );

    async fn emit_attention_changed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        task_id: &str,
        attention_request: Option<AttentionRequestSummary>,
    );

    /// Publish `AcpEvent::ConversationStatusChanged` for the child conversation
    /// after a winning durable CAS so live sidebars match persisted status.
    ///
    /// Production also emits a global `conversation://changed` State patch on
    /// the same win — sub-session sidebars subscribe only to that channel
    /// (`useSubsessionSync`); the per-connection ACP event alone never updates
    /// the left-hand list. Losers must not call this (one emit per terminal
    /// winner).
    async fn emit_conversation_status_changed(
        &self,
        parent_connection_id: &str,
        conversation_id: i32,
        status: ConversationStatus,
    );

    /// Publish `AcpEvent::DelegationObservationChanged` when soft-supervisor
    /// health transitions. Observe-only — never completes or cancels a task.
    async fn emit_observation_changed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        task_id: &str,
        observation: TaskObservation,
        last_agent_activity_at: DateTime<Utc>,
        stalled_since: Option<DateTime<Utc>>,
    );
}

/// Default emitter used when the broker is constructed via the short-form
/// `DelegationBroker::new`. Silently drops every emit — most broker tests
/// observe behavior via outcomes + pending accounting + meta writes, not
/// event fanout. Tests that DO assert on the event lifecycle wire
/// `MockEventEmitter` via `with_writers`.
#[derive(Default, Clone)]
pub struct NoopEventEmitter;

#[async_trait]
#[allow(clippy::too_many_arguments)]
impl DelegationEventEmitter for NoopEventEmitter {
    #[allow(clippy::too_many_arguments)]
    async fn emit_started(
        &self,
        _parent_connection_id: &str,
        _parent_tool_use_id: &str,
        _child_connection_id: &str,
        _child_conversation_id: i32,
        _agent_type: AgentType,
        _task_preview: &str,
        _task_id: &str,
        _started_at: DateTime<Utc>,
        _runtime_stats: DelegationRuntimeStats,
        _attention_request: Option<AttentionRequestSummary>,
    ) {
    }

    async fn emit_completed(
        &self,
        _parent_connection_id: &str,
        _parent_tool_use_id: &str,
        _child_connection_id: &str,
        _child_conversation_id: i32,
        _agent_type: AgentType,
        _task_id: &str,
        _runtime_stats: DelegationRuntimeStats,
        _result: DelegationResultSummary,
        _card_summary: Option<CardSummary>,
    ) {
    }

    async fn emit_runtime_stats_changed(
        &self,
        _parent_connection_id: &str,
        _parent_tool_use_id: &str,
        _task_id: &str,
        _runtime_stats: DelegationRuntimeStats,
    ) {
    }

    async fn emit_attention_changed(
        &self,
        _parent_connection_id: &str,
        _parent_tool_use_id: &str,
        _task_id: &str,
        _attention_request: Option<AttentionRequestSummary>,
    ) {
    }

    async fn emit_conversation_status_changed(
        &self,
        _parent_connection_id: &str,
        _conversation_id: i32,
        _status: ConversationStatus,
    ) {
    }

    async fn emit_observation_changed(
        &self,
        _parent_connection_id: &str,
        _parent_tool_use_id: &str,
        _task_id: &str,
        _observation: TaskObservation,
        _last_agent_activity_at: DateTime<Utc>,
        _stalled_since: Option<DateTime<Utc>>,
    ) {
    }
}

/// Production impl backed by `ConnectionManager`. Resolves the parent
/// connection's `(state, emitter)` and routes events through
/// `emit_with_state` so they land on the same fanout path as every other
/// ACP event from that connection.
///
/// A missing parent connection (user disconnected mid-delegation, parent
/// already torn down by another path) becomes a silent no-op — the broker
/// still needs to drain its pending table even when no one is listening.
#[derive(Clone)]
pub struct ConnectionManagerEventEmitter {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait]
#[allow(clippy::too_many_arguments)]
impl DelegationEventEmitter for ConnectionManagerEventEmitter {
    #[allow(clippy::too_many_arguments)]
    async fn emit_started(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
        task_preview: &str,
        task_id: &str,
        started_at: DateTime<Utc>,
        runtime_stats: DelegationRuntimeStats,
        attention_request: Option<AttentionRequestSummary>,
    ) {
        let Some((state_arc, emitter)) = self
            .manager
            .get_state_and_emitter(parent_connection_id)
            .await
        else {
            return;
        };
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::DelegationStarted {
                parent_connection_id: parent_connection_id.to_string(),
                parent_tool_use_id: parent_tool_use_id.to_string(),
                child_connection_id: child_connection_id.to_string(),
                child_conversation_id,
                agent_type,
                task_preview: task_preview.to_string(),
                task_id: task_id.to_string(),
                started_at,
                runtime_stats,
                attention_request,
            },
        )
        .await;
    }

    async fn emit_completed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
        task_id: &str,
        runtime_stats: DelegationRuntimeStats,
        result: DelegationResultSummary,
        card_summary: Option<CardSummary>,
    ) {
        let Some((state_arc, emitter)) = self
            .manager
            .get_state_and_emitter(parent_connection_id)
            .await
        else {
            return;
        };
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::DelegationCompleted {
                parent_connection_id: parent_connection_id.to_string(),
                parent_tool_use_id: parent_tool_use_id.to_string(),
                child_connection_id: child_connection_id.to_string(),
                child_conversation_id,
                agent_type,
                task_id: task_id.to_string(),
                runtime_stats,
                result,
                card_summary,
            },
        )
        .await;
    }

    async fn emit_runtime_stats_changed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        task_id: &str,
        runtime_stats: DelegationRuntimeStats,
    ) {
        let Some((state_arc, emitter)) = self
            .manager
            .get_state_and_emitter(parent_connection_id)
            .await
        else {
            return;
        };
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::DelegationRuntimeStatsChanged {
                parent_tool_use_id: parent_tool_use_id.to_string(),
                task_id: task_id.to_string(),
                runtime_stats,
            },
        )
        .await;
    }

    async fn emit_attention_changed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        task_id: &str,
        attention_request: Option<AttentionRequestSummary>,
    ) {
        let Some((state_arc, emitter)) = self
            .manager
            .get_state_and_emitter(parent_connection_id)
            .await
        else {
            return;
        };
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::DelegationAttentionChanged {
                parent_tool_use_id: parent_tool_use_id.to_string(),
                task_id: task_id.to_string(),
                attention_request,
            },
        )
        .await;
    }

    async fn emit_conversation_status_changed(
        &self,
        parent_connection_id: &str,
        conversation_id: i32,
        status: ConversationStatus,
    ) {
        let Some((state_arc, emitter)) = self
            .manager
            .get_state_and_emitter(parent_connection_id)
            .await
        else {
            return;
        };
        // Capture wall time once so the ACP event and the global sidebar patch
        // share the same `updated_at` (children apply State by identity, not
        // CAS on the timestamp).
        let updated_at = Utc::now();
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status: status.clone(),
            },
        )
        .await;
        // Sub-session sidebars (`useSubsessionSync`) ignore the per-connection
        // ACP event and only patch from `conversation://changed`. Without this
        // fan-out, settled children stay `in_progress` (spinning) in the left
        // list while cards already show terminal via meta / task status.
        emit_sidebar_conversation_state(&emitter, conversation_id, status, updated_at);
    }

    async fn emit_observation_changed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        task_id: &str,
        observation: TaskObservation,
        last_agent_activity_at: DateTime<Utc>,
        stalled_since: Option<DateTime<Utc>>,
    ) {
        let Some((state_arc, emitter)) = self
            .manager
            .get_state_and_emitter(parent_connection_id)
            .await
        else {
            return;
        };
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::DelegationObservationChanged {
                parent_tool_use_id: parent_tool_use_id.to_string(),
                task_id: task_id.to_string(),
                observation,
                last_agent_activity_at,
                stalled_since,
            },
        )
        .await;

        // Feed exact parent-tool / wait leases only through verified
        // parent_tool_use_id -> task_id binding. Does not change the 300s
        // soft-supervisor observation calculation.
        match observation {
            TaskObservation::Active => {
                tool_watchdog_on_verified_child_activity(
                    &state_arc,
                    &emitter,
                    self.manager.wait_cancel_registry(),
                    parent_tool_use_id,
                    task_id,
                    last_agent_activity_at,
                )
                .await;
            }
            TaskObservation::WaitingInput => {
                tool_watchdog_pause_delegation_waiting(
                    &state_arc,
                    &emitter,
                    parent_tool_use_id,
                    task_id,
                )
                .await;
            }
            TaskObservation::Stalled => {}
        }
    }
}

/// Publish Cleared/TimedOut so attach maps drop demoted actionable leases.
async fn emit_tool_watchdog_clear(
    state: &std::sync::Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>>,
    emitter: &crate::web::event_bridge::EventEmitter,
    projection: crate::acp::tool_watchdog::ToolWatchdogProjection,
) {
    use crate::acp::tool_watchdog::ToolWatchdogPhase;
    if matches!(
        projection.phase,
        ToolWatchdogPhase::Cleared | ToolWatchdogPhase::TimedOut
    ) {
        emit_with_state(state, emitter, AcpEvent::ToolWatchdogChanged { projection }).await;
    }
}

/// Renew live launch + exact-match wait leases for a verified Broker child.
///
/// Never resurrects a completed launch tool or re-arms
/// `CancellationCapability::Delegation` from observation.
async fn tool_watchdog_on_verified_child_activity(
    state: &std::sync::Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>>,
    emitter: &crate::web::event_bridge::EventEmitter,
    wait_cancel: std::sync::Arc<crate::acp::delegation::wait_cancel::WaitCancelRegistry>,
    parent_tool_use_id: &str,
    task_id: &str,
    last_agent_activity_at: DateTime<Utc>,
) {
    use crate::acp::tool_watchdog::WatchdogInstant;

    let (attr, turn, binding_ok) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        let binding_ok = s
            .active_delegations
            .get(parent_tool_use_id)
            .is_some_and(|d| d.task_id == task_id);
        (s.lease_attribution(), turn, binding_ok)
    };
    if !binding_ok {
        return;
    }
    let at = WatchdogInstant {
        mono: tokio::time::Instant::now(),
        wall: last_agent_activity_at,
    };
    // Live launch (if any) + exact-match wait leases only. No register/bind.
    // Progress tokens are per-lease monotonic sequences (not wall-clock ms).
    let cleared = attr
        .renew_from_verified_child_activity(
            wait_cancel.as_ref(),
            &turn,
            parent_tool_use_id,
            task_id,
            at,
        )
        .await;
    for projection in cleared {
        emit_tool_watchdog_clear(state, emitter, projection).await;
    }
}

async fn tool_watchdog_pause_delegation_waiting(
    state: &std::sync::Arc<tokio::sync::RwLock<crate::acp::session_state::SessionState>>,
    emitter: &crate::web::event_bridge::EventEmitter,
    parent_tool_use_id: &str,
    task_id: &str,
) {
    use crate::acp::tool_watchdog::PauseReason;

    let (attr, turn, binding_ok) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        let binding_ok = s
            .active_delegations
            .get(parent_tool_use_id)
            .is_some_and(|d| d.task_id == task_id);
        (s.lease_attribution(), turn, binding_ok)
    };
    if !binding_ok {
        return;
    }
    let cleared = attr
        .registry()
        .pause_turn(&turn, PauseReason::DelegationWaitingInput)
        .await;
    for projection in cleared {
        emit_tool_watchdog_clear(state, emitter, projection).await;
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod mock {
    use super::*;
    use tokio::sync::Mutex;

    /// Ordered projection emit for terminal-flush / publication ordering tests.
    #[derive(Debug, Clone)]
    pub enum ProjectionEmit {
        Started(EmitStartedCall),
        RuntimeStatsChanged(EmitRuntimeCall),
        AttentionChanged(EmitAttentionCall),
        Completed(EmitCall),
    }

    /// Records every emit so broker tests can assert the event lifecycle
    /// (one emit per drained pending entry, never doubled, correct
    /// `result_summary` per terminal path). No-op on the publishing side —
    /// the broker is the unit under test, not the event fanout.
    #[derive(Default)]
    pub struct MockEventEmitter {
        pub calls: Mutex<Vec<EmitCall>>,
        pub started_calls: Mutex<Vec<EmitStartedCall>>,
        pub runtime_calls: Mutex<Vec<EmitRuntimeCall>>,
        pub attention_calls: Mutex<Vec<EmitAttentionCall>>,
        pub status_changed_calls: Mutex<Vec<StatusChangedCall>>,
        pub observation_calls: Mutex<Vec<ObservationChangedCall>>,
        /// Single ordered log covering started/runtime/attention/completed.
        pub ordered: Mutex<Vec<ProjectionEmit>>,
    }

    #[derive(Debug, Clone)]
    pub struct EmitCall {
        pub parent_connection_id: String,
        pub parent_tool_use_id: String,
        pub child_connection_id: String,
        pub child_conversation_id: i32,
        pub agent_type: AgentType,
        pub task_id: String,
        pub runtime_stats: DelegationRuntimeStats,
        pub result: DelegationResultSummary,
        pub card_summary: Option<CardSummary>,
    }

    #[derive(Debug, Clone)]
    pub struct EmitStartedCall {
        pub parent_connection_id: String,
        pub parent_tool_use_id: String,
        pub child_connection_id: String,
        pub child_conversation_id: i32,
        pub agent_type: AgentType,
        pub task_preview: String,
        pub task_id: String,
        pub started_at: DateTime<Utc>,
        pub runtime_stats: DelegationRuntimeStats,
        pub attention_request: Option<AttentionRequestSummary>,
    }

    #[derive(Debug, Clone)]
    pub struct EmitRuntimeCall {
        pub parent_connection_id: String,
        pub parent_tool_use_id: String,
        pub task_id: String,
        pub runtime_stats: DelegationRuntimeStats,
    }

    #[derive(Debug, Clone)]
    pub struct EmitAttentionCall {
        pub parent_connection_id: String,
        pub parent_tool_use_id: String,
        pub task_id: String,
        pub attention_request: Option<AttentionRequestSummary>,
    }

    #[derive(Debug, Clone)]
    pub struct StatusChangedCall {
        pub parent_connection_id: String,
        pub conversation_id: i32,
        pub status: ConversationStatus,
    }

    #[derive(Debug, Clone)]
    pub struct ObservationChangedCall {
        pub parent_connection_id: String,
        pub parent_tool_use_id: String,
        pub task_id: String,
        pub observation: TaskObservation,
        pub last_agent_activity_at: DateTime<Utc>,
        pub stalled_since: Option<DateTime<Utc>>,
    }

    impl MockEventEmitter {
        pub fn new() -> Self {
            Self::default()
        }

        pub async fn snapshot(&self) -> Vec<EmitCall> {
            self.calls.lock().await.clone()
        }

        pub async fn count(&self) -> usize {
            self.calls.lock().await.len()
        }

        pub async fn started_snapshot(&self) -> Vec<EmitStartedCall> {
            self.started_calls.lock().await.clone()
        }

        pub async fn started_count(&self) -> usize {
            self.started_calls.lock().await.len()
        }

        pub async fn runtime_snapshot(&self) -> Vec<EmitRuntimeCall> {
            self.runtime_calls.lock().await.clone()
        }

        pub async fn attention_snapshot(&self) -> Vec<EmitAttentionCall> {
            self.attention_calls.lock().await.clone()
        }

        pub async fn ordered_snapshot(&self) -> Vec<ProjectionEmit> {
            self.ordered.lock().await.clone()
        }

        pub async fn status_changed_snapshot(&self) -> Vec<StatusChangedCall> {
            self.status_changed_calls.lock().await.clone()
        }

        pub async fn status_changed_count(&self) -> usize {
            self.status_changed_calls.lock().await.len()
        }

        pub async fn observation_snapshot(&self) -> Vec<ObservationChangedCall> {
            self.observation_calls.lock().await.clone()
        }

        pub async fn observation_count(&self) -> usize {
            self.observation_calls.lock().await.len()
        }
    }

    #[async_trait]
    #[allow(clippy::too_many_arguments)]
    impl DelegationEventEmitter for MockEventEmitter {
        #[allow(clippy::too_many_arguments)]
        async fn emit_started(
            &self,
            parent_connection_id: &str,
            parent_tool_use_id: &str,
            child_connection_id: &str,
            child_conversation_id: i32,
            agent_type: AgentType,
            task_preview: &str,
            task_id: &str,
            started_at: DateTime<Utc>,
            runtime_stats: DelegationRuntimeStats,
            attention_request: Option<AttentionRequestSummary>,
        ) {
            let call = EmitStartedCall {
                parent_connection_id: parent_connection_id.to_string(),
                parent_tool_use_id: parent_tool_use_id.to_string(),
                child_connection_id: child_connection_id.to_string(),
                child_conversation_id,
                agent_type,
                task_preview: task_preview.to_string(),
                task_id: task_id.to_string(),
                started_at,
                runtime_stats,
                attention_request,
            };
            self.started_calls.lock().await.push(call.clone());
            self.ordered
                .lock()
                .await
                .push(ProjectionEmit::Started(call));
        }

        async fn emit_completed(
            &self,
            parent_connection_id: &str,
            parent_tool_use_id: &str,
            child_connection_id: &str,
            child_conversation_id: i32,
            agent_type: AgentType,
            task_id: &str,
            runtime_stats: DelegationRuntimeStats,
            result: DelegationResultSummary,
            card_summary: Option<CardSummary>,
        ) {
            let call = EmitCall {
                parent_connection_id: parent_connection_id.to_string(),
                parent_tool_use_id: parent_tool_use_id.to_string(),
                child_connection_id: child_connection_id.to_string(),
                child_conversation_id,
                agent_type,
                task_id: task_id.to_string(),
                runtime_stats,
                result,
                card_summary,
            };
            self.calls.lock().await.push(call.clone());
            self.ordered
                .lock()
                .await
                .push(ProjectionEmit::Completed(call));
        }

        async fn emit_runtime_stats_changed(
            &self,
            parent_connection_id: &str,
            parent_tool_use_id: &str,
            task_id: &str,
            runtime_stats: DelegationRuntimeStats,
        ) {
            let call = EmitRuntimeCall {
                parent_connection_id: parent_connection_id.to_string(),
                parent_tool_use_id: parent_tool_use_id.to_string(),
                task_id: task_id.to_string(),
                runtime_stats,
            };
            self.runtime_calls.lock().await.push(call.clone());
            self.ordered
                .lock()
                .await
                .push(ProjectionEmit::RuntimeStatsChanged(call));
        }

        async fn emit_attention_changed(
            &self,
            parent_connection_id: &str,
            parent_tool_use_id: &str,
            task_id: &str,
            attention_request: Option<AttentionRequestSummary>,
        ) {
            let call = EmitAttentionCall {
                parent_connection_id: parent_connection_id.to_string(),
                parent_tool_use_id: parent_tool_use_id.to_string(),
                task_id: task_id.to_string(),
                attention_request,
            };
            self.attention_calls.lock().await.push(call.clone());
            self.ordered
                .lock()
                .await
                .push(ProjectionEmit::AttentionChanged(call));
        }

        async fn emit_conversation_status_changed(
            &self,
            parent_connection_id: &str,
            conversation_id: i32,
            status: ConversationStatus,
        ) {
            self.status_changed_calls
                .lock()
                .await
                .push(StatusChangedCall {
                    parent_connection_id: parent_connection_id.to_string(),
                    conversation_id,
                    status,
                });
        }

        async fn emit_observation_changed(
            &self,
            parent_connection_id: &str,
            parent_tool_use_id: &str,
            task_id: &str,
            observation: TaskObservation,
            last_agent_activity_at: DateTime<Utc>,
            stalled_since: Option<DateTime<Utc>>,
        ) {
            self.observation_calls
                .lock()
                .await
                .push(ObservationChangedCall {
                    parent_connection_id: parent_connection_id.to_string(),
                    parent_tool_use_id: parent_tool_use_id.to_string(),
                    task_id: task_id.to_string(),
                    observation,
                    last_agent_activity_at,
                    stalled_since,
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::runtime_stats::DelegationRuntimeStats;
    use crate::acp::session_state::{ActiveDelegationState, SessionState};
    use crate::acp::tool_watchdog::{ToolCategory, WatchdogInstant};
    use crate::models::AgentType;
    use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster, CONVERSATION_CHANGED_EVENT};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[tokio::test]
    async fn verified_child_activity_requires_the_exact_durable_task_binding() {
        let state = Arc::new(RwLock::new(SessionState::new(
            "parent-conn".into(),
            AgentType::ClaudeCode,
            None,
            "test-window".into(),
            None,
        )));
        let (attribution, turn) = {
            let mut state = state.write().await;
            state.external_id = Some("parent-session".into());
            state.active_turn_generation = Some(1);
            let turn = state.tool_watchdog_turn_stamp().expect("active turn stamp");
            (state.lease_attribution(), turn)
        };
        let started_at = WatchdogInstant::now();
        attribution.start_turn(turn.clone(), started_at).await;
        let parent = attribution
            .register_or_touch_tool(&turn, "parent-tool", ToolCategory::Delegation, started_at)
            .await
            .expect("parent lease")
            .stamp;
        let sibling = attribution
            .register_or_touch_tool(&turn, "sibling-tool", ToolCategory::Delegation, started_at)
            .await
            .expect("sibling lease")
            .stamp;
        let now = Utc::now();
        state.write().await.active_delegations.insert(
            "parent-tool".into(),
            ActiveDelegationState {
                parent_tool_use_id: "parent-tool".into(),
                child_connection_id: "child-conn".into(),
                child_conversation_id: 42,
                agent_type: AgentType::Codex,
                task_preview: "work".into(),
                task_id: "task-exact".into(),
                started_at: now,
                runtime_stats: DelegationRuntimeStats::empty(now),
                attention_request: None,
                observation: None,
                last_agent_activity_at: None,
                stalled_since: None,
            },
        );
        let registry = attribution.registry().clone();
        let wait_cancel = crate::acp::delegation::wait_cancel::WaitCancelRegistry::new_shared();

        tool_watchdog_on_verified_child_activity(
            &state,
            &EventEmitter::Noop,
            wait_cancel.clone(),
            "parent-tool",
            "task-other",
            now,
        )
        .await;
        assert_eq!(
            registry
                .lease_stamp(&parent.lease_id)
                .await
                .unwrap()
                .version,
            parent.version,
        );

        tool_watchdog_on_verified_child_activity(
            &state,
            &EventEmitter::Noop,
            wait_cancel,
            "parent-tool",
            "task-exact",
            now + chrono::Duration::seconds(1),
        )
        .await;
        assert!(
            registry
                .lease_stamp(&parent.lease_id)
                .await
                .unwrap()
                .version
                > parent.version
        );
        assert_eq!(
            registry
                .lease_stamp(&sibling.lease_id)
                .await
                .unwrap()
                .version,
            sibling.version,
        );
    }

    #[tokio::test]
    async fn production_status_emit_fans_out_global_state_for_sidebar() {
        let mgr = ConnectionManager::new();
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut global_rx = broadcaster.subscribe();
        let parent_id = "parent-sidebar-status";
        mgr.insert_test_connection(
            parent_id,
            AgentType::Grok,
            Some(PathBuf::from("/tmp/sidebar-status")),
            EventEmitter::test_web_only(broadcaster.clone()),
        )
        .await;

        let emitter = ConnectionManagerEventEmitter {
            manager: Arc::new(mgr),
        };
        emitter
            .emit_conversation_status_changed(parent_id, 42, ConversationStatus::PendingReview)
            .await;

        let evt = global_rx
            .try_recv()
            .expect("settle win must emit conversation://changed for sidebar");
        assert_eq!(evt.channel, CONVERSATION_CHANGED_EVENT);
        let p = &*evt.payload;
        assert_eq!(p["kind"], "state");
        assert_eq!(p["patch"]["id"], 42);
        assert_eq!(p["patch"]["status"], "pending_review");
        assert!(p["patch"]["awaiting_reply_token"].is_null());
        assert!(p["patch"]["updated_at"].is_string());
    }

    #[tokio::test]
    async fn production_status_emit_noop_without_parent_connection() {
        let mgr = ConnectionManager::new();
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut global_rx = broadcaster.subscribe();
        // No parent connection registered — nothing to fan out.
        let emitter = ConnectionManagerEventEmitter {
            manager: Arc::new(mgr),
        };
        emitter
            .emit_conversation_status_changed("missing", 7, ConversationStatus::Cancelled)
            .await;
        assert!(
            global_rx.try_recv().is_err(),
            "missing parent must not invent a sidebar event"
        );
    }
}
