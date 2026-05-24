//! `ConnectionSpawner` trait — the subset of `ConnectionManager` capabilities
//! that the delegation broker needs. Defined as a trait so:
//!
//! 1. The broker can be unit-tested with a `MockSpawner` (no real ACP
//!    processes, no DB writes).
//! 2. Future cross-host / remote-agent work (v3+) can plug in a different
//!    backend without touching the broker.
//!
//! The concrete impl on `Arc<ConnectionManager>` lives in
//! `acp::manager` next to the existing `ConnectionManager` methods to keep
//! the manager's surface area contiguous.

use async_trait::async_trait;

use crate::models::agent::AgentType;

/// Identifies a delegation call across the broker, the ACP layer, and the DB.
///
/// `parent_conversation_id` is the **DB** id (i32) of the parent's conversation
/// row, not the ACP-side external session id. The child's new conversation
/// row will carry this as `parent_id` plus `parent_tool_use_id` (the MCP
/// tool_use_id from the parent's LLM-issued ToolUse) and `delegation_call_id`
/// (broker-internal UUID).
#[derive(Debug, Clone)]
pub struct DelegationLink {
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: String,
    pub delegation_call_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnerError {
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("send prompt failed: {0}")]
    Send(String),
    #[error("disconnect failed: {0}")]
    Disconnect(String),
    #[error("cancel failed: {0}")]
    Cancel(String),
}

/// Capabilities the delegation broker needs from whatever owns the ACP
/// connections. v1 production impl is `Arc<ConnectionManager>` (see
/// `acp/manager.rs`); tests use `mock::MockSpawner`.
///
/// All methods are `async` because the production impl drives a Tokio runtime
/// and DB; the mock returns immediately.
#[async_trait]
pub trait ConnectionSpawner: Send + Sync {
    /// Spawn a fresh child ACP connection of `agent_type` in `working_dir`.
    /// No session resume, no preferred mode, no special env — delegation
    /// children are always brand-new sessions.
    ///
    /// `parent_connection_id` identifies the parent ACP connection so the
    /// production impl can inherit the parent's `EventEmitter` and
    /// `owner_window_label` (both required by `ConnectionManager::spawn_agent`)
    /// without leaking those types into the broker. If `working_dir` is
    /// `None`, the impl may fall back to the parent connection's `working_dir`.
    ///
    /// Returns the new connection id (codeg-internal UUID, not the ACP
    /// session id assigned by the agent).
    async fn spawn(
        &self,
        parent_connection_id: &str,
        agent_type: AgentType,
        working_dir: Option<String>,
    ) -> Result<String, SpawnerError>;

    /// Send the delegation task as the child's first prompt. The
    /// `DelegationLink` is persisted onto the new conversation row so the
    /// lifecycle subscriber can later notify the broker on `TurnComplete`.
    ///
    /// Returns the new child conversation row id (i32).
    async fn send_prompt_linked_for_delegation(
        &self,
        conn_id: &str,
        task: String,
        link: DelegationLink,
    ) -> Result<i32, SpawnerError>;

    /// Cancel any in-flight prompt on the child connection. Idempotent:
    /// calling on a connection with nothing in flight is a no-op success.
    async fn cancel(&self, conn_id: &str) -> Result<(), SpawnerError>;

    /// Tear down the child connection. Always called after the broker has
    /// resolved (or failed) the pending call, to enforce v1's one-shot
    /// semantics.
    async fn disconnect(&self, conn_id: &str) -> Result<(), SpawnerError>;
}

#[cfg(any(test, feature = "test-utils"))]
pub mod mock {
    use super::*;
    use std::collections::VecDeque;
    use tokio::sync::Mutex;

    /// In-memory spawner that returns pre-queued results and records every
    /// `cancel` / `disconnect` it sees. Use `queue_spawn` / `queue_send` to
    /// stage the next return value; calls without queued results fail loudly.
    #[derive(Default)]
    pub struct MockSpawner {
        pub spawn_results: Mutex<VecDeque<Result<String, SpawnerError>>>,
        pub send_results: Mutex<VecDeque<Result<i32, SpawnerError>>>,
        pub cancels: Mutex<Vec<String>>,
        pub disconnects: Mutex<Vec<String>>,
    }

    impl MockSpawner {
        pub fn new() -> Self {
            Self::default()
        }

        pub async fn queue_spawn(&self, r: Result<String, SpawnerError>) {
            self.spawn_results.lock().await.push_back(r);
        }

        pub async fn queue_send(&self, r: Result<i32, SpawnerError>) {
            self.send_results.lock().await.push_back(r);
        }
    }

    #[async_trait]
    impl ConnectionSpawner for MockSpawner {
        async fn spawn(
            &self,
            _parent_connection_id: &str,
            _agent_type: AgentType,
            _working_dir: Option<String>,
        ) -> Result<String, SpawnerError> {
            self.spawn_results
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| Err(SpawnerError::Spawn("no queued spawn result".into())))
        }

        async fn send_prompt_linked_for_delegation(
            &self,
            _conn_id: &str,
            _task: String,
            _link: DelegationLink,
        ) -> Result<i32, SpawnerError> {
            self.send_results
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| Err(SpawnerError::Send("no queued send result".into())))
        }

        async fn cancel(&self, conn_id: &str) -> Result<(), SpawnerError> {
            self.cancels.lock().await.push(conn_id.to_string());
            Ok(())
        }

        async fn disconnect(&self, conn_id: &str) -> Result<(), SpawnerError> {
            self.disconnects.lock().await.push(conn_id.to_string());
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[tokio::test]
        async fn mock_records_cancel_and_disconnect() {
            let m = MockSpawner::new();
            m.cancel("c1").await.unwrap();
            m.disconnect("c2").await.unwrap();
            assert_eq!(m.cancels.lock().await.as_slice(), &["c1".to_string()]);
            assert_eq!(m.disconnects.lock().await.as_slice(), &["c2".to_string()]);
        }

        #[tokio::test]
        async fn mock_consumes_queued_spawn_results() {
            let m = MockSpawner::new();
            m.queue_spawn(Ok("child-1".into())).await;
            m.queue_spawn(Err(SpawnerError::Spawn("oh no".into())))
                .await;
            let r1 = m
                .spawn("parent-1", AgentType::ClaudeCode, Some("/tmp".into()))
                .await
                .unwrap();
            assert_eq!(r1, "child-1");
            let r2 = m
                .spawn("parent-1", AgentType::Codex, None)
                .await
                .unwrap_err();
            assert!(matches!(r2, SpawnerError::Spawn(_)));
        }

        #[tokio::test]
        async fn mock_unqueued_spawn_fails_loudly() {
            let m = MockSpawner::new();
            let r = m
                .spawn("parent-1", AgentType::ClaudeCode, None)
                .await
                .unwrap_err();
            match r {
                SpawnerError::Spawn(msg) => assert!(msg.contains("no queued")),
                other => panic!("expected SpawnerError::Spawn, got {other:?}"),
            }
        }
    }
}
