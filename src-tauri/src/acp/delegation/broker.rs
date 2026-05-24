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

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::{oneshot, Mutex};

use crate::acp::delegation::event_emitter::{DelegationEventEmitter, NoopEventEmitter};
use crate::acp::delegation::meta_writer::{
    build_delegation_meta, is_synthetic_parent_tool_use_id, DelegationMetaWriter, NoopMetaWriter,
};
use crate::acp::delegation::spawner::{ConnectionSpawner, DelegationLink};
use crate::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationRequest};
use crate::acp::types::DelegationResultSummary;

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

/// FIFO of `tool_call_id`s that the ACP lifecycle observed firing
/// `delegate_to_agent` on a given parent connection but for which the
/// matching broker round-trip has not yet arrived. MCP clients (Codex,
/// Claude Code) generally do NOT populate `_meta.tool_use_id` when
/// invoking an MCP tool, so the broker can't read the LLM-issued
/// `tool_use_id` from the wire — we capture it from the parallel ACP
/// `tool_call` event stream instead.
///
/// ACP `sessionUpdate(tool_call)` almost always lands ahead of the
/// agent's own MCP `tools/call`, so this LRU resolves the race in
/// practice. Stale entries (if any) get evicted on parent disconnect.
#[derive(Default)]
struct PendingToolCalls {
    inner: Mutex<HashMap<String, VecDeque<String>>>,
}

/// The broker is intentionally `Clone` (cheap — only `Arc`s inside) so
/// listener/handler code can hand copies to spawned tasks without lifetime
/// gymnastics.
#[derive(Clone)]
pub struct DelegationBroker {
    spawner: Arc<dyn ConnectionSpawner>,
    depth_lookup: Arc<dyn ConversationDepthLookup>,
    /// Writer for `meta["codeg.delegation"]` on the parent's active
    /// `delegate_to_agent` ToolCallState. Defaults to a no-op so tests
    /// that aren't exercising the meta lifecycle don't need to wire
    /// anything; production constructs the broker with the
    /// `ConnectionManagerMetaWriter` via `with_writers`.
    meta_writer: Arc<dyn DelegationMetaWriter>,
    /// Emitter for `AcpEvent::DelegationCompleted` against the parent
    /// connection's event stream. Same Noop/Mock/Production scheme as
    /// the meta writer — production wires `ConnectionManagerEventEmitter`
    /// via `with_writers`; tests that don't observe the event lifecycle
    /// take the default Noop.
    event_emitter: Arc<dyn DelegationEventEmitter>,
    pending: Arc<PendingCalls>,
    pending_tool_calls: Arc<PendingToolCalls>,
    config: Arc<Mutex<DelegationConfig>>,
}

impl DelegationBroker {
    pub fn new(
        spawner: Arc<dyn ConnectionSpawner>,
        depth_lookup: Arc<dyn ConversationDepthLookup>,
    ) -> Self {
        Self::with_writers(
            spawner,
            depth_lookup,
            Arc::new(NoopMetaWriter) as Arc<dyn DelegationMetaWriter>,
            Arc::new(NoopEventEmitter) as Arc<dyn DelegationEventEmitter>,
        )
    }

    /// Test-only constructor that injects a meta writer but keeps the
    /// default Noop event emitter. Retained so existing meta-focused
    /// tests don't have to mention the emitter parameter. New callsites
    /// (and production wiring) should prefer `with_writers`.
    pub fn with_meta_writer(
        spawner: Arc<dyn ConnectionSpawner>,
        depth_lookup: Arc<dyn ConversationDepthLookup>,
        meta_writer: Arc<dyn DelegationMetaWriter>,
    ) -> Self {
        Self::with_writers(
            spawner,
            depth_lookup,
            meta_writer,
            Arc::new(NoopEventEmitter) as Arc<dyn DelegationEventEmitter>,
        )
    }

    /// Production-grade constructor wiring the broker to both a real
    /// meta writer (`ConnectionManagerMetaWriter`) AND an event emitter
    /// (`ConnectionManagerEventEmitter`). Tests that observe the full
    /// lifecycle (meta writes + DelegationCompleted emits) should use
    /// this with `MockMetaWriter` + `MockEventEmitter`.
    pub fn with_writers(
        spawner: Arc<dyn ConnectionSpawner>,
        depth_lookup: Arc<dyn ConversationDepthLookup>,
        meta_writer: Arc<dyn DelegationMetaWriter>,
        event_emitter: Arc<dyn DelegationEventEmitter>,
    ) -> Self {
        Self {
            spawner,
            depth_lookup,
            meta_writer,
            event_emitter,
            pending: Arc::new(PendingCalls::default()),
            pending_tool_calls: Arc::new(PendingToolCalls::default()),
            config: Arc::new(Mutex::new(DelegationConfig::default())),
        }
    }

    /// Record a parent ACP `tool_call_id` whose title indicates the LLM is
    /// invoking `delegate_to_agent`. The next broker round-trip from the
    /// same `parent_connection_id` will claim this id as its
    /// `parent_tool_use_id`. Bounded FIFO per connection.
    pub async fn register_pending_tool_call(
        &self,
        parent_connection_id: &str,
        tool_call_id: String,
    ) {
        let mut map = self.pending_tool_calls.inner.lock().await;
        let queue = map
            .entry(parent_connection_id.to_string())
            .or_insert_with(VecDeque::new);
        // Defensive cap so an agent that fires many delegations without ever
        // round-tripping can't grow this map without bound.
        if queue.len() >= 32 {
            queue.pop_front();
        }
        queue.push_back(tool_call_id);
    }

    /// Pop the oldest pending `tool_call_id` for the given parent, if any.
    pub async fn take_pending_tool_call(&self, parent_connection_id: &str) -> Option<String> {
        let mut map = self.pending_tool_calls.inner.lock().await;
        let queue = map.get_mut(parent_connection_id)?;
        let id = queue.pop_front();
        if queue.is_empty() {
            map.remove(parent_connection_id);
        }
        id
    }

    /// `take_pending_tool_call` with a brief poll loop. Used by
    /// `handle_request` to absorb the inherent race between two parallel
    /// arrival paths for the parent's `delegate_to_agent` invocation:
    ///
    ///   * ACP `session/update(tool_call)` → in-process bus → lifecycle
    ///     dispatcher → `register_pending_tool_call` (fast)
    ///   * MCP `tools/call` → stdio round-trip → companion server →
    ///     `handle_request` (slower, but not by much)
    ///
    /// In practice the ACP path lands first because it's in-process, but
    /// the order is not contractually guaranteed. Without this wait, a
    /// faster-than-usual MCP delivery would slip past an empty queue and
    /// fall back to the synthetic `delegation-<uuid>` placeholder — which
    /// breaks the parent's UI binding because the frontend keys its
    /// `parent_tool_use_id` map by the agent's real `tool_call_id`.
    ///
    /// 100 ms total polling budget (10 attempts × 10 ms) is generous: the
    /// observed gap on local dev is well under 5 ms, but headroom protects
    /// against busier hosts or slower MCP transports without delaying the
    /// no-ACP-id fallback path materially.
    async fn claim_pending_tool_call_with_brief_wait(
        &self,
        parent_connection_id: &str,
    ) -> Option<String> {
        if let Some(id) = self.take_pending_tool_call(parent_connection_id).await {
            return Some(id);
        }
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if let Some(id) = self.take_pending_tool_call(parent_connection_id).await {
                return Some(id);
            }
        }
        None
    }

    /// Forget every pending tool_call id for the given parent. Called when
    /// the parent connection tears down so stale ids don't bind to a future
    /// reuse of the same connection_id (UUIDs make that unlikely but cheap
    /// to defend against).
    pub async fn drop_pending_tool_calls_for_parent(&self, parent_connection_id: &str) {
        self.pending_tool_calls
            .inner
            .lock()
            .await
            .remove(parent_connection_id);
    }

    pub async fn set_config(&self, cfg: DelegationConfig) {
        *self.config.lock().await = cfg;
    }

    pub async fn config_snapshot(&self) -> DelegationConfig {
        self.config.lock().await.clone()
    }

    /// Entry point. Drives the full lifecycle and returns whatever the parent
    /// LLM should see as the `delegate_to_agent` tool_result.
    pub async fn handle_request(&self, mut req: DelegationRequest) -> DelegationOutcome {
        // MCP clients usually don't populate `_meta.tool_use_id`, so the
        // listener will pass through an empty string. Best-effort claim the
        // most recent ACP-side `tool_call_id` for this parent — with a brief
        // poll loop so an MCP round-trip that out-races the in-process ACP
        // `session/update` doesn't fall back to a synthetic id (which
        // breaks the parent UI's `parent_tool_use_id` binding). Falls back
        // to a UUID placeholder only after the wait budget is exhausted.
        if req.parent_tool_use_id.is_empty() {
            req.parent_tool_use_id = self
                .claim_pending_tool_call_with_brief_wait(&req.parent_connection_id)
                .await
                .unwrap_or_else(|| format!("delegation-{}", uuid::Uuid::new_v4()));
        }
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

        // --- Mark the parent's tool call as in-flight -------------------------
        // The frontend's DelegationContext seeds its `parent_tool_use_id`-keyed
        // binding map from this meta on snapshot replay, so a page refresh
        // mid-delegation can reconstruct the child connection / conversation
        // ids without depending on the live `delegation_started` event having
        // been received.
        self.write_meta_if_real(
            &req.parent_connection_id,
            &req.parent_tool_use_id,
            build_delegation_meta(
                "running",
                Some(&child_connection_id),
                Some(child_conversation_id),
                None,
            ),
        )
        .await;

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
                // complete_call already removed from `pending`, wrote meta,
                // emitted DelegationCompleted, and disconnected; this is a
                // belt-and-braces idempotent prune in case complete_call
                // wasn't reached on this path (it always is in production,
                // but the prune is cheap).
                self.pending.inner.lock().await.remove(&call_id);
                outcome
            }
            Ok(Err(_)) => {
                // The sender was dropped before sending — should not happen in
                // practice (complete_call always sends before drop), but be defensive.
                // Drain pending FIRST so a racing complete_call (from a late
                // lifecycle TurnComplete) finds no entry and silently no-ops
                // instead of double-emitting DelegationCompleted.
                let _ = self.pending.inner.lock().await.remove(&call_id);
                self.write_meta_if_real(
                    &req.parent_connection_id,
                    &req.parent_tool_use_id,
                    build_delegation_meta(
                        "failed",
                        Some(&child_connection_id),
                        Some(child_conversation_id),
                        Some("canceled"),
                    ),
                )
                .await;
                self.emit_completed_if_real(
                    &req.parent_connection_id,
                    &req.parent_tool_use_id,
                    &child_connection_id,
                    child_conversation_id,
                    DelegationResultSummary::Err {
                        error_code: "canceled".to_string(),
                    },
                )
                .await;
                let _ = self.spawner.disconnect(&child_connection_id).await;
                DelegationOutcome::from_err(
                    DelegationError::Canceled {
                        reason: "completion channel dropped".into(),
                    },
                    Some(child_conversation_id),
                )
            }
            Err(_) => {
                // Timeout. Drain pending FIRST so the subsequent
                // spawner.disconnect (which fires the child's
                // StatusChanged{Disconnected} → forward_disconnect_to_broker →
                // cancel_by_child_connection cascade) finds an empty entry
                // and doesn't race us into a double DelegationCompleted emit.
                let _ = self.pending.inner.lock().await.remove(&call_id);
                self.write_meta_if_real(
                    &req.parent_connection_id,
                    &req.parent_tool_use_id,
                    build_delegation_meta(
                        "failed",
                        Some(&child_connection_id),
                        Some(child_conversation_id),
                        Some("timeout"),
                    ),
                )
                .await;
                self.emit_completed_if_real(
                    &req.parent_connection_id,
                    &req.parent_tool_use_id,
                    &child_connection_id,
                    child_conversation_id,
                    DelegationResultSummary::Err {
                        error_code: "timeout".to_string(),
                    },
                )
                .await;
                let _ = self.spawner.cancel(&child_connection_id).await;
                let _ = self.spawner.disconnect(&child_connection_id).await;
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
            child_conversation_id,
            parent_connection_id,
            parent_tool_use_id,
            tx,
        }) = entry
        {
            // Mirror the resolution onto the parent's `delegate_to_agent`
            // ToolCallState meta so snapshot recovery after refresh shows
            // the final state without depending on the broker's live
            // `delegation_completed` event having been received.
            let meta = match &outcome {
                DelegationOutcome::Ok(_) => build_delegation_meta(
                    "completed",
                    Some(&child_connection_id),
                    Some(child_conversation_id),
                    None,
                ),
                DelegationOutcome::Err { code, .. } => build_delegation_meta(
                    "failed",
                    Some(&child_connection_id),
                    Some(child_conversation_id),
                    Some(code),
                ),
            };
            self.write_meta_if_real(&parent_connection_id, &parent_tool_use_id, meta)
                .await;
            self.emit_completed_if_real(
                &parent_connection_id,
                &parent_tool_use_id,
                &child_connection_id,
                child_conversation_id,
                Self::outcome_to_summary(&outcome),
            )
            .await;
            // v1 one-shot: always tear down the child.
            let _ = self.spawner.disconnect(&child_connection_id).await;
            let _ = tx.send(outcome);
        }
    }

    /// Project a `DelegationOutcome` onto the wire-stable
    /// `DelegationResultSummary` carried by `AcpEvent::DelegationCompleted`.
    /// Keeps the mapping (and the `error_code` choice) in one place.
    fn outcome_to_summary(outcome: &DelegationOutcome) -> DelegationResultSummary {
        match outcome {
            DelegationOutcome::Ok(ok) => DelegationResultSummary::Ok {
                duration_ms: ok.duration_ms,
            },
            DelegationOutcome::Err { code, .. } => DelegationResultSummary::Err {
                error_code: code.clone(),
            },
        }
    }

    /// Internal helper — apply the meta write iff the parent's
    /// `tool_use_id` refers to a real ACP `tool_call_id`. The
    /// broker-synthesized `"delegation-<uuid>"` placeholder targets no
    /// ToolCallState, so emitting a `ToolCallUpdate` against it would be
    /// noise that the frontend would route through `apply_tool_call_update`
    /// to a non-existent entry. See `meta_writer::is_synthetic_parent_tool_use_id`.
    async fn write_meta_if_real(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        meta: serde_json::Value,
    ) {
        if is_synthetic_parent_tool_use_id(parent_tool_use_id) {
            return;
        }
        self.meta_writer
            .write_meta(parent_connection_id, parent_tool_use_id, meta)
            .await;
    }

    /// Internal helper — emit `AcpEvent::DelegationCompleted` on the parent's
    /// stream iff the `parent_tool_use_id` refers to a real ACP tool_call.
    /// Synthetic ids (the `"delegation-<uuid>"` UUID fallback) map to no
    /// live UI binding, so the emit would be wasted noise — same skip
    /// criterion as `write_meta_if_real`.
    async fn emit_completed_if_real(
        &self,
        parent_connection_id: &str,
        parent_tool_use_id: &str,
        child_connection_id: &str,
        child_conversation_id: i32,
        result: DelegationResultSummary,
    ) {
        if is_synthetic_parent_tool_use_id(parent_tool_use_id) {
            return;
        }
        self.event_emitter
            .emit_completed(
                parent_connection_id,
                parent_tool_use_id,
                child_connection_id,
                child_conversation_id,
                result,
            )
            .await;
    }

    /// Resolve the pending delegation whose child matches
    /// `child_connection_id` with a `canceled` outcome. Used when a child
    /// session disconnects or errors out without firing a clean
    /// TurnComplete — the parent's `tool_use_id` shouldn't dangle.
    /// No-op when no matching entry exists.
    pub async fn cancel_by_child_connection(&self, child_connection_id: &str) {
        let drained: Vec<PendingCall> = {
            let mut map = self.pending.inner.lock().await;
            let keys: Vec<String> = map
                .iter()
                .filter(|(_, v)| v.child_connection_id == child_connection_id)
                .map(|(k, _)| k.clone())
                .collect();
            keys.into_iter()
                .map(|k| map.remove(&k).expect("key just observed"))
                .collect()
        };
        for entry in drained {
            self.write_meta_if_real(
                &entry.parent_connection_id,
                &entry.parent_tool_use_id,
                build_delegation_meta(
                    "failed",
                    Some(&entry.child_connection_id),
                    Some(entry.child_conversation_id),
                    Some("canceled"),
                ),
            )
            .await;
            self.emit_completed_if_real(
                &entry.parent_connection_id,
                &entry.parent_tool_use_id,
                &entry.child_connection_id,
                entry.child_conversation_id,
                DelegationResultSummary::Err {
                    error_code: "canceled".to_string(),
                },
            )
            .await;
            let _ = self.spawner.disconnect(&entry.child_connection_id).await;
            let _ = entry.tx.send(DelegationOutcome::from_err(
                DelegationError::Canceled {
                    reason: "child session ended without TurnComplete".into(),
                },
                Some(entry.child_conversation_id),
            ));
        }
    }

    /// Cascade-cancel every pending delegation owned by `parent_connection_id`.
    /// Used when a parent session disconnects or the user cancels the parent's
    /// active prompt.
    pub async fn cancel_by_parent(&self, parent_connection_id: &str) {
        // Also drain any tool_call ids that were captured ahead of an MCP
        // round-trip that never arrived — keeps the map bounded across
        // parent reconnects.
        self.drop_pending_tool_calls_for_parent(parent_connection_id)
            .await;
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
            // Best-effort meta patch so a parent-side snapshot post-cancel
            // shows the delegation as failed/canceled rather than stuck
            // on the prior "running" mark.
            self.write_meta_if_real(
                &entry.parent_connection_id,
                &entry.parent_tool_use_id,
                build_delegation_meta(
                    "failed",
                    Some(&entry.child_connection_id),
                    Some(entry.child_conversation_id),
                    Some("canceled"),
                ),
            )
            .await;
            self.emit_completed_if_real(
                &entry.parent_connection_id,
                &entry.parent_tool_use_id,
                &entry.child_connection_id,
                entry.child_conversation_id,
                DelegationResultSummary::Err {
                    error_code: "canceled".to_string(),
                },
            )
            .await;
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
            Ok(self.0.iter().find(|(c, _)| *c == id).and_then(|(_, p)| *p))
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
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
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
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());

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
        mock.queue_spawn(Err(SpawnerError::Spawn("nope".into())))
            .await;
        let broker = DelegationBroker::new(mock as Arc<dyn ConnectionSpawner>, shallow_lookup());
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
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
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
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
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
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());

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
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());

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
        let lookup = Arc::new(MockDepth(vec![(1, None), (2, Some(1)), (3, Some(2))]))
            as Arc<dyn ConversationDepthLookup>;
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

    // -- Pending tool_call_id queue (MCP `_meta.tool_use_id` fallback) ----

    #[tokio::test]
    async fn pending_tool_call_register_and_take_is_fifo() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "tc-a".into()).await;
        broker.register_pending_tool_call("p1", "tc-b".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-a")
        );
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("tc-b")
        );
        assert!(broker.take_pending_tool_call("p1").await.is_none());
    }

    #[tokio::test]
    async fn pending_tool_call_is_isolated_per_parent() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker.register_pending_tool_call("p1", "p1-a".into()).await;
        broker.register_pending_tool_call("p2", "p2-a".into()).await;
        assert_eq!(
            broker.take_pending_tool_call("p1").await.as_deref(),
            Some("p1-a")
        );
        assert_eq!(
            broker.take_pending_tool_call("p2").await.as_deref(),
            Some("p2-a")
        );
        assert!(broker.take_pending_tool_call("p1").await.is_none());
        assert!(broker.take_pending_tool_call("p2").await.is_none());
    }

    #[tokio::test]
    async fn empty_parent_tool_use_id_claims_pending_then_completes() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(7)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        broker
            .register_pending_tool_call("parent-conn", "tu-from-acp".into())
            .await;
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The captured ACP id was consumed.
        assert!(broker.take_pending_tool_call("parent-conn").await.is_none());
        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "ok".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    #[tokio::test]
    async fn empty_parent_tool_use_id_claims_pending_arriving_late() {
        // Regression: when the parent's ACP `session/update(tool_call)`
        // lands at the lifecycle dispatcher AFTER `broker.handle_request`
        // already entered the claim phase, the brief poll loop must still
        // pick it up rather than falling back to the synthetic UUID.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-late".into())).await;
        mock.queue_send(Ok(13)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };

        // Give the driver time to enter the claim wait loop on an empty
        // queue, then register the ACP id (simulates the dispatcher's
        // ToolCall handling landing late).
        tokio::time::sleep(Duration::from_millis(30)).await;
        broker
            .register_pending_tool_call("parent-conn", "tu-late".into())
            .await;

        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The late-arriving ACP id was consumed by the broker — no leftover
        // entry.
        assert!(broker.take_pending_tool_call("parent-conn").await.is_none());
        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "late ok".into(),
                    child_conversation_id: 13,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    #[tokio::test]
    async fn empty_parent_tool_use_id_with_no_pending_falls_back_to_uuid() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(11)).await;
        let broker =
            DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, shallow_lookup());
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let call_id = broker.peek_first_pending_call_id().await.unwrap();
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::Ok(DelegationSuccess {
                    text: "fallback ok".into(),
                    child_conversation_id: 11,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Ok(_)));
    }

    #[tokio::test]
    async fn cancel_by_parent_also_drops_pending_tool_calls() {
        let broker = DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
        );
        broker
            .register_pending_tool_call("parent-conn", "tu-1".into())
            .await;
        broker.cancel_by_parent("parent-conn").await;
        assert!(broker.take_pending_tool_call("parent-conn").await.is_none());
    }

    #[tokio::test]
    async fn depth_limit_allows_root() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c1".into())).await;
        mock.queue_send(Ok(7)).await;
        let lookup = Arc::new(MockDepth(vec![(1, None)])) as Arc<dyn ConversationDepthLookup>;
        let broker = DelegationBroker::new(mock.clone() as Arc<dyn ConnectionSpawner>, lookup);
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

    // -- Meta writer lifecycle --------------------------------------------

    use crate::acp::delegation::meta_writer::mock::MockMetaWriter;
    use crate::acp::delegation::meta_writer::DelegationMetaWriter;

    fn broker_with_meta(mock: Arc<MockSpawner>, writer: Arc<MockMetaWriter>) -> DelegationBroker {
        DelegationBroker::with_meta_writer(
            mock as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            writer as Arc<dyn DelegationMetaWriter>,
        )
    }

    #[tokio::test]
    async fn meta_writer_records_running_then_completed_on_happy_path() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-real")).await })
        };
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
                    text: "done".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = writer.snapshot().await;
        assert_eq!(calls.len(), 2);
        // First write: running, with child connection + conversation ids.
        let first = &calls[0];
        assert_eq!(first.parent_tool_use_id, "pt-real");
        let inner_first = first
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            inner_first.get("status").unwrap().as_str().unwrap(),
            "running"
        );
        assert_eq!(
            inner_first
                .get("child_connection_id")
                .unwrap()
                .as_str()
                .unwrap(),
            "child-conn-1"
        );
        assert_eq!(
            inner_first
                .get("child_conversation_id")
                .unwrap()
                .as_i64()
                .unwrap(),
            42
        );
        // Second write: completed.
        let second = &calls[1];
        let inner_second = second
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            inner_second.get("status").unwrap().as_str().unwrap(),
            "completed"
        );
    }

    #[tokio::test]
    async fn meta_writer_records_failed_on_err_outcome() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-2".into())).await;
        mock.queue_send(Ok(7)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-err")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::from_err(
                    DelegationError::Timeout { elapsed_ms: 5_000 },
                    Some(7),
                ),
            )
            .await;
        driver.await.unwrap();

        let calls = writer.snapshot().await;
        assert_eq!(calls.len(), 2);
        let inner = calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(inner.get("status").unwrap().as_str().unwrap(), "failed");
        assert_eq!(
            inner.get("error_code").unwrap().as_str().unwrap(),
            "timeout"
        );
    }

    #[tokio::test]
    async fn meta_writer_records_failed_on_parent_cancel() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-cancel".into())).await;
        mock.queue_send(Ok(33)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-pcancel")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        let outcome = driver.await.unwrap();
        assert!(matches!(outcome, DelegationOutcome::Err { .. }));

        let calls = writer.snapshot().await;
        // running + canceled
        assert_eq!(calls.len(), 2);
        let inner = calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(inner.get("status").unwrap().as_str().unwrap(), "failed");
        assert_eq!(
            inner.get("error_code").unwrap().as_str().unwrap(),
            "canceled"
        );
    }

    #[tokio::test]
    async fn meta_writer_skipped_for_synthetic_parent_tool_use_id() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-synth".into())).await;
        mock.queue_send(Ok(8)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let broker = broker_with_meta(mock.clone(), writer.clone());

        // Empty `parent_tool_use_id` triggers the broker's UUID fallback —
        // `"delegation-<uuid>"` — which the writer must skip because no
        // matching ACP tool_call_id exists.
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
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
                    text: "ok".into(),
                    child_conversation_id: 8,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = writer.snapshot().await;
        assert!(
            calls.is_empty(),
            "writer should be skipped for synthetic parent_tool_use_id, got {:?}",
            calls
        );
    }

    // -- Event emitter lifecycle ------------------------------------------
    //
    // Issue: `.docs/issues/2026-05-24-delegation-termination-cascade.md`.
    // The broker must emit `AcpEvent::DelegationCompleted` once per drained
    // pending entry, regardless of which terminal path drained it (happy
    // `complete_call`, broker-side timeout, child-disconnect cleanup, or
    // parent-cancel cascade). Without these emits the frontend's live
    // delegation binding stays at "running" forever — see the issue doc
    // for the full path matrix.

    use crate::acp::delegation::event_emitter::mock::MockEventEmitter;
    use crate::acp::delegation::event_emitter::DelegationEventEmitter;
    use crate::acp::types::DelegationResultSummary;

    fn broker_with_emitter(
        mock: Arc<MockSpawner>,
        writer: Arc<MockMetaWriter>,
        emitter: Arc<MockEventEmitter>,
    ) -> DelegationBroker {
        DelegationBroker::with_writers(
            mock as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            writer as Arc<dyn DelegationMetaWriter>,
            emitter as Arc<dyn DelegationEventEmitter>,
        )
    }

    #[tokio::test]
    async fn emitter_records_ok_on_complete_call_happy_path() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-1".into())).await;
        mock.queue_send(Ok(42)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-ok")).await })
        };
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
                    text: "done".into(),
                    child_conversation_id: 42,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 73,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.parent_tool_use_id, "pt-ok");
        assert_eq!(call.child_connection_id, "child-conn-1");
        assert_eq!(call.child_conversation_id, 42);
        assert!(
            matches!(call.result, DelegationResultSummary::Ok { duration_ms: 73 }),
            "expected Ok{{73}}, got {:?}",
            call.result
        );
    }

    #[tokio::test]
    async fn emitter_records_err_on_complete_call_err_outcome() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-err".into())).await;
        mock.queue_send(Ok(11)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-err")).await })
        };
        let call_id = loop {
            if let Some(id) = broker.peek_first_pending_call_id().await {
                break id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        };
        broker
            .complete_call(
                &call_id,
                DelegationOutcome::from_err(
                    DelegationError::SubagentRuntimeError("agent died".into()),
                    Some(11),
                ),
            )
            .await;
        driver.await.unwrap();

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 1);
        match &calls[0].result {
            DelegationResultSummary::Err { error_code } => {
                assert_eq!(error_code, "subagent_error")
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emitter_records_timeout_on_broker_side_timeout() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("child-conn-to".into())).await;
        mock.queue_send(Ok(91)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone());

        let mut req = request(1, "pt-timeout");
        req.timeout_seconds = Some(1);
        let outcome = broker.handle_request(req).await;
        assert!(matches!(
            outcome,
            DelegationOutcome::Err { ref code, .. } if code == "timeout"
        ));

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 1, "expected exactly one emit, got {calls:?}");
        let call = &calls[0];
        assert_eq!(call.parent_tool_use_id, "pt-timeout");
        assert_eq!(call.child_connection_id, "child-conn-to");
        assert_eq!(call.child_conversation_id, 91);
        match &call.result {
            DelegationResultSummary::Err { error_code } => {
                assert_eq!(error_code, "timeout")
            }
            other => panic!("expected Err{{timeout}}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emitter_records_canceled_on_cancel_by_child_connection() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-dropped".into())).await;
        mock.queue_send(Ok(55)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-cbc")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_child_connection("c-dropped").await;
        let outcome = driver.await.unwrap();
        assert!(matches!(
            outcome,
            DelegationOutcome::Err { ref code, .. } if code == "canceled"
        ));

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 1);
        match &calls[0].result {
            DelegationResultSummary::Err { error_code } => {
                assert_eq!(error_code, "canceled")
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn emitter_records_one_event_per_drained_entry_on_cancel_by_parent() {
        let mock = Arc::new(MockSpawner::new());
        for i in 0..3 {
            mock.queue_spawn(Ok(format!("c{i}"))).await;
            mock.queue_send(Ok(100 + i)).await;
        }
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone());

        let mut handles = Vec::new();
        for i in 0..3 {
            let broker = broker.clone();
            handles.push(tokio::spawn(async move {
                broker.handle_request(request(1, &format!("pt-{i}"))).await
            }));
        }
        while broker.pending_count().await < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        for h in handles {
            let _ = h.await.unwrap();
        }

        let calls = emitter.snapshot().await;
        assert_eq!(calls.len(), 3, "expected 3 emits, got {calls:?}");
        let mut parent_tool_use_ids: Vec<String> =
            calls.iter().map(|c| c.parent_tool_use_id.clone()).collect();
        parent_tool_use_ids.sort();
        assert_eq!(
            parent_tool_use_ids,
            vec!["pt-0".to_string(), "pt-1".to_string(), "pt-2".to_string()]
        );
        for call in &calls {
            match &call.result {
                DelegationResultSummary::Err { error_code } => {
                    assert_eq!(error_code, "canceled")
                }
                other => panic!("expected Err{{canceled}}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn emitter_does_not_double_emit_on_repeat_cancel_by_parent() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-once".into())).await;
        mock.queue_send(Ok(42)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-idem")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // First call drains the entry + emits one.
        broker.cancel_by_parent("parent-conn").await;
        // Second call finds the pending map empty — no extra emit.
        broker.cancel_by_parent("parent-conn").await;
        // Cleanup-guard-style triple call also stays bounded.
        broker.cancel_by_parent("parent-conn").await;
        let _ = driver.await.unwrap();

        assert_eq!(emitter.count().await, 1);
    }

    #[tokio::test]
    async fn emitter_skipped_for_synthetic_parent_tool_use_id() {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-synth".into())).await;
        mock.queue_send(Ok(8)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "")).await })
        };
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
                    text: "ok".into(),
                    child_conversation_id: 8,
                    child_agent_type: AgentType::Codex,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let calls = emitter.snapshot().await;
        assert!(
            calls.is_empty(),
            "emitter must skip synthetic parent_tool_use_id (same rule as meta writer); got {calls:?}"
        );
    }

    #[tokio::test]
    async fn emitter_records_after_meta_write_on_complete_call() {
        // Frontend's snapshot-recovery path reads `meta["codeg.delegation"]`
        // first and the live event second; if the emit lands before the
        // meta write, a snapshot taken between them would see "running"
        // meta paired with a "completed" event. Enforce meta-before-emit
        // by checking the MockMetaWriter has at least one call before the
        // emitter records.
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok("c-order".into())).await;
        mock.queue_send(Ok(7)).await;
        let writer = Arc::new(MockMetaWriter::new());
        let emitter = Arc::new(MockEventEmitter::new());
        let broker = broker_with_emitter(mock.clone(), writer.clone(), emitter.clone());

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-order")).await })
        };
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
                    text: "ok".into(),
                    child_conversation_id: 7,
                    child_agent_type: AgentType::ClaudeCode,
                    turn_count: 1,
                    duration_ms: 5,
                    token_usage: None,
                }),
            )
            .await;
        driver.await.unwrap();

        let meta_calls = writer.snapshot().await;
        let event_calls = emitter.snapshot().await;
        // running (from handle_request) + completed (from complete_call) =
        // 2 meta writes. The single event must be the "completed" one,
        // and it must land AFTER the running meta — guaranteed structurally
        // by complete_call's order (write_meta_if_real then emit).
        assert_eq!(meta_calls.len(), 2);
        assert_eq!(event_calls.len(), 1);
        let inner_second = meta_calls[1]
            .meta
            .get("codeg.delegation")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(
            inner_second.get("status").unwrap().as_str().unwrap(),
            "completed"
        );
    }

    // -- Production-path fanout coverage ----------------------------------
    //
    // Every other emitter test in this module uses `MockEventEmitter`. The
    // production wiring goes through `ConnectionManagerEventEmitter`, which
    // resolves `(state, emitter)` against the live `ConnectionManager` and
    // hands the event to `emit_with_state` so it fans out to (1) the parent
    // connection's `ConnectionEventStream` (the WS attach path) and (2) the
    // `InternalEventBus` (the lifecycle/pet/chat-channel subscriber path).
    // These tests exercise that real fanout end-to-end so a regression in
    // `get_state_and_emitter` lookup, `emit_with_state` routing, or the
    // `EventEmitter::WebOnly { bus, .. }` wiring is caught here even when
    // every mock-backed test stays green.

    #[tokio::test]
    async fn real_emitter_fans_out_delegation_completed_to_parent_stream_and_bus() {
        use crate::acp::delegation::event_emitter::ConnectionManagerEventEmitter;
        use crate::acp::manager::ConnectionManager;
        use crate::acp::types::AcpEvent;
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

        // Real ConnectionManager + fake parent wired to a WebOnly emitter so
        // the InternalEventBus gets typed envelopes and we can subscribe to
        // verify the lifecycle-path delivery alongside the per-connection
        // stream delivery.
        let manager = ConnectionManager::new();
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let parent_emitter = EventEmitter::test_web_only(broadcaster);
        let bus = parent_emitter
            .acp_event_bus()
            .expect("WebOnly emitter must expose an InternalEventBus");
        manager
            .insert_test_connection("parent-conn", AgentType::ClaudeCode, None, parent_emitter)
            .await;

        // Subscribe BEFORE triggering events — broadcast channels drop
        // sends that happen with no receivers registered.
        let mut bus_rx = bus.subscribe();
        let (parent_state, _) = manager
            .get_state_and_emitter("parent-conn")
            .await
            .expect("parent just inserted");
        let mut stream_rx = parent_state.read().await.event_stream().subscribe();

        // Build the broker with the PRODUCTION emitter; meta writer can stay
        // noop because this test is asserting the event-fanout invariant.
        let mock_spawner = Arc::new(MockSpawner::new());
        mock_spawner.queue_spawn(Ok("child-conn-real".into())).await;
        mock_spawner.queue_send(Ok(77)).await;
        let real_emitter = Arc::new(ConnectionManagerEventEmitter {
            manager: Arc::new(manager.clone_ref()),
        });
        let broker = DelegationBroker::with_writers(
            mock_spawner.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            Arc::new(crate::acp::delegation::meta_writer::NoopMetaWriter)
                as Arc<dyn crate::acp::delegation::meta_writer::DelegationMetaWriter>,
            real_emitter as Arc<dyn crate::acp::delegation::event_emitter::DelegationEventEmitter>,
        );

        // Park a pending entry then trigger cancel_by_parent to drive the
        // production emit path. `request()` hard-codes parent_connection_id
        // = "parent-conn" which matches the insert above.
        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-fanout")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        let _ = driver.await.unwrap();

        // Per-connection stream (WS attach delivery path) must receive the
        // envelope tagged with the right connection + payload shape.
        let envelope = tokio::time::timeout(Duration::from_millis(500), stream_rx.recv())
            .await
            .expect("per-connection stream should receive DelegationCompleted within 500ms")
            .expect("envelope recv must not error");
        assert_eq!(envelope.connection_id, "parent-conn");
        match &envelope.payload {
            AcpEvent::DelegationCompleted {
                parent_tool_use_id,
                child_connection_id,
                child_conversation_id,
                result,
                ..
            } => {
                assert_eq!(parent_tool_use_id, "pt-fanout");
                assert_eq!(child_connection_id, "child-conn-real");
                assert_eq!(*child_conversation_id, 77);
                match result {
                    DelegationResultSummary::Err { error_code } => {
                        assert_eq!(error_code, "canceled");
                    }
                    other => panic!("expected Err{{canceled}}, got {other:?}"),
                }
            }
            other => panic!("expected DelegationCompleted, got {other:?}"),
        }

        // InternalEventBus (lifecycle/pet/chat-channel subscriber path) must
        // also receive the same envelope — proves the WebOnly emitter's bus
        // arm in `emit_with_state` is reached.
        let bus_envelope = tokio::time::timeout(Duration::from_millis(500), bus_rx.recv())
            .await
            .expect("InternalEventBus should receive DelegationCompleted within 500ms")
            .expect("bus recv must not error");
        assert_eq!(bus_envelope.connection_id, "parent-conn");
        assert!(matches!(
            bus_envelope.payload,
            AcpEvent::DelegationCompleted { .. }
        ));
    }

    #[tokio::test]
    async fn real_emitter_is_silent_no_op_when_parent_already_detached() {
        // Parent torn down mid-delegation: `get_state_and_emitter` returns
        // None, the emit silently drops, BUT the broker still drains its
        // pending table and surfaces the outcome to the awaiting caller.
        // This is the "parent disappeared before terminal" path that the
        // mock-backed tests can't observe.
        use crate::acp::delegation::event_emitter::ConnectionManagerEventEmitter;
        use crate::acp::manager::ConnectionManager;

        let manager = ConnectionManager::new();
        // Intentionally no insert_test_connection — parent is absent.
        let real_emitter = Arc::new(ConnectionManagerEventEmitter {
            manager: Arc::new(manager.clone_ref()),
        });
        let mock_spawner = Arc::new(MockSpawner::new());
        mock_spawner.queue_spawn(Ok("c-orphan".into())).await;
        mock_spawner.queue_send(Ok(1)).await;
        let broker = DelegationBroker::with_writers(
            mock_spawner.clone() as Arc<dyn ConnectionSpawner>,
            shallow_lookup(),
            Arc::new(crate::acp::delegation::meta_writer::NoopMetaWriter)
                as Arc<dyn crate::acp::delegation::meta_writer::DelegationMetaWriter>,
            real_emitter as Arc<dyn crate::acp::delegation::event_emitter::DelegationEventEmitter>,
        );

        let driver = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.handle_request(request(1, "pt-orphan")).await })
        };
        while broker.pending_count().await == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        broker.cancel_by_parent("parent-conn").await;
        let outcome = driver.await.unwrap();

        assert!(matches!(
            outcome,
            DelegationOutcome::Err { ref code, .. } if code == "canceled"
        ));
        assert_eq!(
            broker.pending_count().await,
            0,
            "broker must drain pending even when no parent exists to receive the emit"
        );
    }
}
