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
use crate::acp::delegation::runtime_stats::DelegationRuntimeStats;
use crate::acp::delegation::types::TaskObservation;
use crate::acp::manager::ConnectionManager;
use crate::acp::types::{AcpEvent, DelegationResultSummary};
use crate::db::entities::conversation::ConversationStatus;
use crate::models::AgentType;
use crate::web::event_bridge::emit_with_state;

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
    /// open attention).
    async fn emit_started(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
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
    /// Losers must not call this (one emit per terminal winner).
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
    async fn emit_started(
        &self,
        _parent_connection_id: &str,
        _parent_tool_use_id: &str,
        _child_connection_id: &str,
        _child_conversation_id: i32,
        _agent_type: AgentType,
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
    async fn emit_started(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        agent_type: AgentType,
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
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            },
        )
        .await;
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
    }

    #[derive(Debug, Clone)]
    pub struct EmitStartedCall {
        pub parent_connection_id: String,
        pub parent_tool_use_id: String,
        pub child_connection_id: String,
        pub child_conversation_id: i32,
        pub agent_type: AgentType,
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
        async fn emit_started(
            &self,
            parent_connection_id: &str,
            parent_tool_use_id: &str,
            child_connection_id: &str,
            child_conversation_id: i32,
            agent_type: AgentType,
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
