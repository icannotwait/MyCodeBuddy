//! `DelegationBroker` — the coordination unit for multi-agent delegation.
//!
//! Lifecycle of a single call:
//!
//! 1. `handle_request` is the broker's only entry point. The MCP listener
//!    feeds it the LLM-issued `delegate_to_agent` payload.
//! 2. Pre-checks: feature enabled? depth limit ok? Both failures return
//!    immediately, no child session created.
//! 3. Spawn the child via [`ConnectionSpawner::spawn`].
//! 4. Send the delegation task as the first prompt via
//!    [`ConnectionSpawner::send_prompt_linked_for_delegation`]. The trailing
//!    [`DelegationLink`] carries the parent's `tool_use_id` and a
//!    broker-internal `call_id` (UUID) — these get persisted onto the new
//!    conversation row.
//! 5. Park a `oneshot::Sender` keyed by `call_id`. The race is between:
//!       - the listener calling [`DelegationBroker::complete_call`] on
//!         `TurnComplete`, and
//!       - the broker's own `tokio::time::timeout`.
//! 6. On any resolution, the child connection is disconnected. v1 is
//!    explicitly one-shot — no session reuse.
//!
//! Cancellation cascade: when a parent session goes away (user-initiated
//! cancel, parent disconnect), the lifecycle subscriber calls
//! [`DelegationBroker::cancel_by_parent`] which fans out cancel + disconnect
//! to every pending child of that parent.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{oneshot, Mutex};

use crate::acp::delegation::spawner::{ConnectionSpawner, DelegationLink};
use crate::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationRequest};

/// Lookup the `parent_id` for a conversation. Abstracted so the broker can be
/// unit-tested against an in-memory chain without touching SeaORM.
#[async_trait]
pub trait ConversationDepthLookup: Send + Sync {
    async fn parent_of(&self, conversation_id: i32) -> Result<Option<i32>, DelegationError>;
}

#[derive(Debug, Clone)]
pub struct DelegationConfig {
    pub enabled: bool,
    /// Max chain depth a *new* delegation may exist at. With `depth_limit = 2`
    /// the chain root → child → grandchild is allowed; the grandchild trying
    /// to spawn a great-grandchild is rejected. See spec §5.
    pub depth_limit: u32,
    pub default_timeout: Duration,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            depth_limit: 2,
            default_timeout: Duration::from_secs(600),
        }
    }
}

struct PendingCall {
    child_connection_id: String,
    child_conversation_id: i32,
    parent_connection_id: String,
    #[allow(dead_code)] // surfaced via accessors and listener payloads in later phases
    parent_tool_use_id: String,
    tx: oneshot::Sender<DelegationOutcome>,
}

#[derive(Default)]
struct PendingCalls {
    inner: Mutex<HashMap<String, PendingCall>>,
}

/// The broker is intentionally `Clone` (cheap — only `Arc`s inside) so
/// listener/handler code can hand copies to spawned tasks without lifetime
/// gymnastics.
#[derive(Clone)]
pub struct DelegationBroker {
    spawner: Arc<dyn ConnectionSpawner>,
    depth_lookup: Arc<dyn ConversationDepthLookup>,
    pending: Arc<PendingCalls>,
    config: Arc<Mutex<DelegationConfig>>,
}

impl DelegationBroker {
    pub fn new(
        spawner: Arc<dyn ConnectionSpawner>,
        depth_lookup: Arc<dyn ConversationDepthLookup>,
    ) -> Self {
        Self {
            spawner,
            depth_lookup,
            pending: Arc::new(PendingCalls::default()),
            config: Arc::new(Mutex::new(DelegationConfig::default())),
        }
    }

    pub async fn set_config(&self, cfg: DelegationConfig) {
        *self.config.lock().await = cfg;
    }

    pub async fn config_snapshot(&self) -> DelegationConfig {
        self.config.lock().await.clone()
    }

    /// Entry point. Drives the full lifecycle and returns whatever the parent
    /// LLM should see as the `delegate_to_agent` tool_result.
    pub async fn handle_request(&self, req: DelegationRequest) -> DelegationOutcome {
        let cfg = self.config_snapshot().await;
        if !cfg.enabled {
            return DelegationOutcome::from_err(
                DelegationError::Canceled {
                    reason: "delegation disabled".into(),
                },
                None,
            );
        }

        // --- Depth pre-check ----------------------------------------------------
        // We walk up to `limit + 1` so we know whether the *new* child would
        // sit at >= limit. Cycles/dead chains saturate at the cap.
        let lookup = self.depth_lookup.clone();
        let parent_depth = match crate::acp::delegation::depth::compute_depth(
            req.parent_conversation_id,
            |id| {
                let lookup = lookup.clone();
                async move { lookup.parent_of(id).await }
            },
            cfg.depth_limit + 1,
        )
        .await
        {
            Ok(d) => d,
            Err(e) => return DelegationOutcome::from_err(e, None),
        };
        // The child the broker is about to create would sit at `parent_depth + 1`.
        // Reject when the *child* depth would equal or exceed the limit.
        if parent_depth + 1 > cfg.depth_limit {
            return DelegationOutcome::from_err(
                DelegationError::DepthLimitExceeded {
                    current_depth: parent_depth,
                    limit: cfg.depth_limit,
                },
                None,
            );
        }

        let timeout = req
            .timeout_seconds
            .map(Duration::from_secs)
            .unwrap_or(cfg.default_timeout);
        let started_at = Instant::now();

        // --- Spawn child connection --------------------------------------------
        let child_connection_id = match self
            .spawner
            .spawn(
                &req.parent_connection_id,
                req.agent_type,
                req.working_dir.clone(),
            )
            .await
        {
            Ok(id) => id,
            Err(e) => {
                return DelegationOutcome::from_err(
                    DelegationError::SpawnFailed(e.to_string()),
                    None,
                );
            }
        };

        // --- Send linked prompt ------------------------------------------------
        let call_id = uuid::Uuid::new_v4().to_string();
        let link = DelegationLink {
            parent_conversation_id: req.parent_conversation_id,
            parent_tool_use_id: req.parent_tool_use_id.clone(),
            delegation_call_id: call_id.clone(),
        };
        let child_conversation_id = match self
            .spawner
            .send_prompt_linked_for_delegation(&child_connection_id, req.task.clone(), link)
            .await
        {
            Ok(cid) => cid,
            Err(e) => {
                let _ = self.spawner.disconnect(&child_connection_id).await;
                return DelegationOutcome::from_err(
                    DelegationError::SpawnFailed(e.to_string()),
                    None,
                );
            }
        };

        // --- Register pending + race timeout vs completion --------------------
        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.inner.lock().await;
            map.insert(
                call_id.clone(),
                PendingCall {
                    child_connection_id: child_connection_id.clone(),
                    child_conversation_id,
                    parent_connection_id: req.parent_connection_id.clone(),
                    parent_tool_use_id: req.parent_tool_use_id.clone(),
                    tx,
                },
            );
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(outcome)) => {
                // complete_call already removed from `pending` and disconnected;
                // belt-and-braces idempotent prune.
                self.pending.inner.lock().await.remove(&call_id);
                outcome
            }
            Ok(Err(_)) => {
                // The sender was dropped before sending — should not happen in
                // practice (complete_call always sends before drop), but be defensive.
                self.pending.inner.lock().await.remove(&call_id);
                let _ = self.spawner.disconnect(&child_connection_id).await;
                DelegationOutcome::from_err(
                    DelegationError::Canceled {
                        reason: "completion channel dropped".into(),
                    },
                    Some(child_conversation_id),
                )
            }
            Err(_) => {
                // Timeout: cancel in-flight, then disconnect, then return.
                let _ = self.spawner.cancel(&child_connection_id).await;
                let _ = self.spawner.disconnect(&child_connection_id).await;
                self.pending.inner.lock().await.remove(&call_id);
                DelegationOutcome::from_err(
                    DelegationError::Timeout {
                        elapsed_ms: started_at.elapsed().as_millis() as u64,
                    },
                    Some(child_conversation_id),
                )
            }
        }
    }

    /// Called by the child-session lifecycle subscriber on `TurnComplete`
    /// (success path) or by error mappers (failure path). Idempotent —
    /// calls on unknown `call_id` are silent no-ops.
    pub async fn complete_call(&self, call_id: &str, outcome: DelegationOutcome) {
        let entry = self.pending.inner.lock().await.remove(call_id);
        if let Some(PendingCall {
            child_connection_id,
            tx,
            ..
        }) = entry
        {
            // v1 one-shot: always tear down the child.
            let _ = self.spawner.disconnect(&child_connection_id).await;
            let _ = tx.send(outcome);
        }
    }

    /// Cascade-cancel every pending delegation owned by `parent_connection_id`.
    /// Used when a parent session disconnects or the user cancels the parent's
    /// active prompt.
    pub async fn cancel_by_parent(&self, parent_connection_id: &str) {
        let drained: Vec<PendingCall> = {
            let mut map = self.pending.inner.lock().await;
            let keys: Vec<String> = map
                .iter()
                .filter(|(_, v)| v.parent_connection_id == parent_connection_id)
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .map(|k| map.remove(&k).expect("key just observed"))
                .collect()
        };
        for entry in drained {
            let _ = self.spawner.cancel(&entry.child_connection_id).await;
            let _ = self.spawner.disconnect(&entry.child_connection_id).await;
            let _ = entry.tx.send(DelegationOutcome::from_err(
                DelegationError::Canceled {
                    reason: "parent canceled".into(),
                },
                Some(entry.child_conversation_id),
            ));
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn peek_first_pending_call_id(&self) -> Option<String> {
        self.pending.inner.lock().await.keys().next().cloned()
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn pending_count(&self) -> usize {
        self.pending.inner.lock().await.len()
    }
}

/// `ConversationDepthLookup` over the live `AppDatabase`. Used by the
/// production wiring; tests use the in-module `MockDepth`.
pub struct DbDepthLookup {
    pub db: Arc<crate::db::AppDatabase>,
}

#[async_trait]
impl ConversationDepthLookup for DbDepthLookup {
    async fn parent_of(&self, conversation_id: i32) -> Result<Option<i32>, DelegationError> {
        use sea_orm::EntityTrait;
        let row = crate::db::entities::conversation::Entity::find_by_id(conversation_id)
            .one(&self.db.conn)
            .await
            .map_err(|e| DelegationError::SubagentRuntimeError(format!("db: {e}")))?;
        Ok(row.and_then(|r| r.parent_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::spawner::{mock::MockSpawner, SpawnerError};
    use crate::acp::delegation::types::DelegationSuccess;
    use crate::models::AgentType;

    /// Test-only `ConversationDepthLookup` that resolves against a flat
    /// (id, parent_id) table. Unknown ids return `Ok(None)` to keep test
    /// setup small.
    struct MockDepth(Vec<(i32, Option<i32>)>);

    #[async_trait]
    impl ConversationDepthLookup for MockDepth {
        async fn parent_of(&self, id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(self
                .0
                .iter()
                .find(|(c, _)| *c == id)
                .and_then(|(_, p)| *p))
        }
    }

    fn shallow_lookup() -> Arc<dyn ConversationDepthLookup> {
        // parent conversation is the root — depth = 0, no rejection.
        Arc::new(MockDepth(vec![(1, None)])) as Arc<dyn ConversationDepthLookup>
    }

    fn request(parent_conv: i32, tool_use: &str) -> DelegationRequest {
        DelegationRequest {
            parent_connection_id: "parent-conn".into(),
            parent_conversation_id: parent_conv,
            parent_tool_use_id: tool_use.into(),
            agent_type: AgentType::ClaudeCode,
            task: "do x".into(),
            working_dir: None,
            timeout_seconds: Some(30),
        }
    }

    // -- Task 4.3 -----------------------------------------------------------

    #[tokio::test]
    async fn config_round_trip() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .set_config(DelegationConfig {
                enabled: false,
                depth_limit: 5,
                default_timeout: Duration::from_secs(120),
            })
            .await;
        let got = broker.config_snapshot().await;
        assert!(!got.enabled);
        assert_eq!(got.depth_limit, 5);
        assert_eq!(got.default_timeout, Duration::from_secs(120));
    }

    #[tokio::test]
    async fn disabled_returns_canceled_without_touching_spawner() {
        let mock = Arc::new(MockSpawner::new());
        let broker = DelegationBroker::new(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .set_config(DelegationConfig {
                enabled: false,
                depth_limit: 2,
                default_timeout: Duration::from_secs(60),
            })
            .await;
        let outcome = broker.handle_request(request(1, "pt-1")).await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
            _ => panic!("expected Err"),
        }
        assert!(mock.disconnects.lock().await.is_empty());
    }

    // -- Task 4.4: happy path ----------------------------------------------

    #[tokio::test]
    async fn happy_path_returns_ok_after_complete_call() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let broker = DelegationBroker::new(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-1")).await })
        };

        // Spin until the broker has registered the pending call so the test
        // doesn't race the spawn/send awaits.
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };

        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "4".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 50,
                    token_usage: None,
                }),
            )
            .await;

        let outcome = driver.await.unwrap();
        match outcome {
            DelegationOutcome::Ok(s) => {
                assert_eq!(s.text, "4");
                assert_eq!(s.child_conversation_id, 42);
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        assert_eq!(broker.pending_count().await, 0);
        // complete_call disconnects the child once.
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["child-conn-1"]);
    }

    // -- Task 4.5: error paths ---------------------------------------------

    #[tokio::test]
    async fn spawn_failure_maps_to_spawn_failed() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Err(SpawnerError::Spawn("nope".into()))).await;
        let broker = DelegationBroker::new(
            mock as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        let outcome = broker.handle_request(request(1, "pt-1")).await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "spawn_failed"),
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_failure_after_spawn_disconnects_child() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Err(SpawnerError::Send("agent rejected prompt".into())))
            .await;
        let broker = DelegationBroker::new(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        let outcome = broker.handle_request(request(1, "pt-1")).await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "spawn_failed"),
            other => panic!("expected Err, got {other:?}"),
        }
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c1"]);
    }

    #[tokio::test]
    async fn timeout_cancels_and_disconnects() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(99)).await;
        let broker = DelegationBroker::new(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        let mut req = request(1, "pt-1");
        req.timeout_seconds = Some(1);
        let outcome = broker.handle_request(req).await;
        match outcome {
            DelegationOutcome::Err {
                code,
                child_conversation_id,
                ..
            } => {
                assert_eq!(code, "timeout");
                assert_eq!(child_conversation_id, Some(99));
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
        assert_eq!(mock.cancels.lock().await.as_slice(), &["c1"]);
        assert_eq!(mock.disconnects.lock().await.as_slice(), &["c1"]);
        assert_eq!(broker.pending_count().await, 0);
    }

    // -- Task 4.6: parent-cancel cascade -----------------------------------

    #[tokio::test]
    async fn parent_cancel_cancels_all_pending_children() {
        let mock = Arc::new(MockSpawner::new());
        for i in 0..3 {
            mock.queue_spawn(Ok(format!("c{i}"))).await;
            mock.queue_send(Ok(100 + i)).await;
        }
        let broker = DelegationBroker::new(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );

        let mut handles = Vec::new();
        for i in 0..3 {
            let broker = broker.clone();
            handles.push(tokio::spawn(async move {
                broker.handle_request(request(1, &format!("pt-{i}"))).await
            }));
        }

        // Wait until all three are parked.
        while broker.pending_count().await < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        broker.cancel_by_parent("parent-conn").await;
        for h in handles {
            let outcome = h.await.unwrap();
            match outcome {
                DelegationOutcome::Err { code, .. } => assert_eq!(code, "canceled"),
                other => panic!("expected canceled, got {other:?}"),
            }
        }
        assert_eq!(mock.cancels.lock().await.len(), 3);
        // Each child disconnects exactly once via cancel_by_parent.
        assert_eq!(mock.disconnects.lock().await.len(), 3);
    }

    #[tokio::test]
    async fn cancel_by_parent_ignores_other_parents() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(200)).await;
        let broker = DelegationBroker::new(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-1")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        broker.cancel_by_parent("other-parent").await;
        // No effect — pending entry still there.
        assert_eq!(broker.pending_count().await, 1);

        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "done".into(),
                    child_conversation_id: 200,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 10,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    // -- Task 4.7: depth limit ---------------------------------------------

    #[tokio::test]
    async fn depth_limit_rejects_before_spawn() {
        let mock = Arc::new(MockSpawner::new());
        // No queued spawn results — if the broker tries to spawn, it errors loudly.
        // chain: 1 (root, None) <- 2 (child of 1) <- 3 (grandchild of 2).
        // Parent = grandchild (id 3): parent_depth = 2. With limit = 2, child
        // would sit at depth 3 → reject.
        let lookup = Arc::new(MockDepth(vec![
            (1, None),
            (2, Some(1)),
            (3, Some(2)),
        ])) as Arc<dyn ConversationDepthLookup>;
        let broker = DelegationBroker::new(mock as Arc<dyn ConnectionSpawner>, lookup);
        broker
            .set_config(DelegationConfig {
                enabled: true,
                depth_limit: 2,
                default_timeout: Duration::from_secs(60),
            })
            .await;
        let outcome = broker.handle_request(request(3, "pt-1")).await;
        match outcome {
            DelegationOutcome::Err { code, .. } => assert_eq!(code, "depth_limit"),
            other => panic!("expected depth_limit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn depth_limit_allows_root() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(7)).await;
        let lookup = Arc::new(MockDepth(vec![(1, None)])) as Arc<dyn ConversationDepthLookup>;
        let broker = DelegationBroker::new(
            mock.clone() as Arc<dyn ConnectionSpawner>,
            lookup,
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                depth_limit: 2,
                default_timeout: Duration::from_secs(60),
            })
            .await;

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-1")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }
}
