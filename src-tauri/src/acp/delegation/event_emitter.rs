//! `DelegationEventEmitter` — broker capability for surfacing
//! `AcpEvent::DelegationCompleted` to the parent's event stream.
//!
//! Parallel to [`crate::acp::delegation::meta_writer::DelegationMetaWriter`]:
//! both abstract over the broker's access to the parent connection's
//! `(state, emitter)` pair so the broker can be unit-tested without spinning
//! up a `ConnectionManager`. Together they form the broker's two-output
//! capability surface — meta writes patch the persisted `ToolCallState`,
//! event emits drive the live frontend `DelegationContext`.
//!
//! The broker calls this from every terminal path:
//!
//! 1. `complete_call` — happy path (kind=ok) and error completions
//!    (kind=err) propagated by the listener/lifecycle.
//! 2. `handle_request` broker-side timeout — emits `Err{error_code: "timeout"}`.
//! 3. `handle_request` completion-channel-dropped — emits `Err{error_code: "canceled"}`.
//! 4. `cancel_by_child_connection` — emits `Err{error_code: "canceled"}` for
//!    every drained pending entry whose child matches.
//! 5. `cancel_by_parent` — emits `Err{error_code: "canceled"}` for every
//!    drained pending entry whose parent matches.
//!
//! Emits are skipped when the broker is operating on a synthetic
//! `parent_tool_use_id` (the `"delegation-*"` UUID fallback) because no
//! `tool_call_id`-keyed UI exists to receive them — same guard as the meta
//! writer. The frontend's snapshot path will still recover state from the
//! broker's meta write.

use async_trait::async_trait;
use std::sync::Arc;

use crate::acp::manager::ConnectionManager;
use crate::acp::types::{AcpEvent, DelegationResultSummary};
use crate::web::event_bridge::emit_with_state;

/// Capability the broker uses to publish `AcpEvent::DelegationCompleted`
/// against the parent connection's event stream.
///
/// Errors are swallowed at the impl boundary — same rationale as
/// `DelegationMetaWriter`. The broker must finish its pending-table
/// cleanup regardless of whether the parent connection is still around to
/// observe the event.
#[async_trait]
pub trait DelegationEventEmitter: Send + Sync {
    async fn emit_completed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        result: DelegationResultSummary,
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
impl DelegationEventEmitter for NoopEventEmitter {
    async fn emit_completed(
        &self,
        _parent_connection_id: &str,
        _parent_tool_use_id: &str,
        _child_connection_id: &str,
        _child_conversation_id: i32,
        _result: DelegationResultSummary,
    ) {
    }
}

/// Production impl backed by `ConnectionManager`. Resolves the parent
/// connection's `(state, emitter)` and routes the `DelegationCompleted`
/// event through `emit_with_state` so it lands on the same fanout path
/// as every other ACP event from that connection.
///
/// A missing parent connection (user disconnected mid-delegation, parent
/// already torn down by another path) becomes a silent no-op — the broker
/// still needs to drain its pending table even when no one is listening.
#[derive(Clone)]
pub struct ConnectionManagerEventEmitter {
    pub manager: Arc<ConnectionManager>,
}

#[async_trait]
impl DelegationEventEmitter for ConnectionManagerEventEmitter {
    async fn emit_completed(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
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
                result,
            },
        )
        .await;
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod mock {
    use super::*;
    use tokio::sync::Mutex;

    /// Records every emit so broker tests can assert the event lifecycle
    /// (one emit per drained pending entry, never doubled, correct
    /// `result_summary` per terminal path). No-op on the publishing side —
    /// the broker is the unit under test, not the event fanout.
    #[derive(Default)]
    pub struct MockEventEmitter {
        pub calls: Mutex<Vec<EmitCall>>,
    }

    #[derive(Debug, Clone)]
    pub struct EmitCall {
        pub parent_connection_id: String,
        pub parent_tool_use_id: String,
        pub child_connection_id: String,
        pub child_conversation_id: i32,
        pub result: DelegationResultSummary,
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
    }

    #[async_trait]
    impl DelegationEventEmitter for MockEventEmitter {
        async fn emit_completed(
            &self,
            parent_connection_id: &str,
            parent_tool_use_id: &str,
            child_connection_id: &str,
            child_conversation_id: i32,
            result: DelegationResultSummary,
        ) {
            self.calls.lock().await.push(EmitCall {
                parent_connection_id: parent_connection_id.to_string(),
                parent_tool_use_id: parent_tool_use_id.to_string(),
                child_connection_id: child_connection_id.to_string(),
                child_conversation_id,
                result,
            });
        }
    }
}
