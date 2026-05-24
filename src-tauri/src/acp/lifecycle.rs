//! Background subscriber that watches the in-process `InternalEventBus` for
//! ACP events that need cross-connection DB persistence (e.g. binding the
//! agent's external session id onto a conversation row when SessionStarted
//! fires). Decoupled from `emit_with_state` so the emit hot path stays
//! lock-tight.
//!
//! Phase 5: migrated from `WebEventBroadcaster` (JSON-shape) to
//! `InternalEventBus` (typed `Arc<EventEnvelope>`). Eliminates the
//! per-event `serde_json::from_value` reparse and lets us drop the
//! `acp://event` channel from the global firehose entirely.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use sea_orm::DatabaseConnection;
use tokio::sync::{broadcast, mpsc};

use crate::acp::delegation::broker::DelegationBroker;
use crate::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationSuccess};
use crate::acp::internal_bus::InternalEventBus;
use crate::acp::manager::ConnectionManager;
use crate::acp::session_state::SessionState;
use crate::acp::types::{AcpEvent, ConnectionStatus, EventEnvelope};
use crate::db::entities::conversation::ConversationStatus;
use crate::db::error::DbError;
use crate::db::service::conversation_service;
use crate::web::event_bridge::{emit_with_state, EventEmitter};
use tokio::sync::RwLock;

/// Per-connection worker queue depth. Sized for the **filtered** event set
/// only (see `is_lifecycle_relevant`) — high-frequency events (ContentDelta,
/// ToolCall*, PermissionRequest) are dropped at the dispatcher and never
/// enter the queue. The remaining 5 event types arrive at most a handful
/// of times per turn, so 64 slots is comfortable headroom for a sustained
/// SQLite stall without forcing the dispatcher to block on `send`.
const WORKER_QUEUE_CAPACITY: usize = 64;

/// Whether an event needs to reach the per-connection worker. Mirrors the
/// match arms in `connection_worker_loop` — keep in sync so the dispatcher
/// doesn't filter out an event a future worker arm starts caring about.
///
/// Filtering at the dispatcher (rather than letting the worker no-op on
/// uninteresting events) means ContentDelta floods can't crowd out a
/// TurnComplete in the worker mailbox: only events that may write the DB
/// or update the per-connection cache enter the queue.
///
/// `ToolCall` is in the accept list because the worker's ToolCall arm
/// captures `delegate_to_agent` invocations for the broker's pending
/// tool_call_id queue. ToolCall fires a handful of times per turn (not
/// per-token like ContentDelta), so the queue pressure is bounded.
fn is_lifecycle_relevant(event: &AcpEvent) -> bool {
    matches!(
        event,
        AcpEvent::SessionStarted { .. }
            | AcpEvent::TurnComplete { .. }
            | AcpEvent::ConversationLinked { .. }
            | AcpEvent::ToolCall { .. }
            | AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected
            }
            | AcpEvent::Error { .. }
    )
}

/// Per-connection state that survives `ConnectionCleanupGuard::drop` so
/// `Disconnected` / `Error` handlers can still emit a derived
/// `ConversationStatusChanged` after the manager entry has been removed.
///
/// Captured on `ConversationLinked` (the earliest point a connection is bound
/// to a conversation row) and consulted on terminal status events. Without
/// this cache, `manager.get_state_and_emitter(connection_id)` races the
/// cleanup guard: `emit_with_state(StatusChanged{Disconnected})` writes to the
/// broadcaster *before* the guard drops, but the subscriber's async receive
/// can wake up after the entry is already gone.
struct CachedConn {
    conversation_id: i32,
    state: Arc<RwLock<SessionState>>,
    emitter: EventEmitter,
}

/// Backoff schedule for `handle_event` DB writes. Most transient
/// SQLite contention clears within the first retry; the third gives a
/// final chance before we fall back to "log loudly and move on".
const HANDLE_EVENT_RETRY_BACKOFFS: &[Duration] =
    &[Duration::from_millis(100), Duration::from_millis(500)];

/// Wrap `handle_event` with a small backoff retry. Most failures here
/// are transient SQLite "database is locked" errors that clear within a
/// few hundred milliseconds; without a retry the conversation row would
/// silently miss its `pending_review` write and the sidebar would stay
/// stuck on `in_progress` until the next prompt's `in_progress` write.
///
/// Final failure is logged at ERROR — this is the only signal the
/// subscriber is dropping correctness on the floor, so it must be noisy.
async fn handle_event_with_retry(
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    envelope: &EventEnvelope,
    broker: Option<&Arc<DelegationBroker>>,
) {
    match handle_event(db_conn, manager, envelope, broker).await {
        Ok(()) => return,
        Err(e) => {
            eprintln!(
                "[lifecycle][WARN] handle_event failed (attempt 1, will retry) for {:?}: {e}",
                envelope.payload
            );
        }
    }
    for (attempt, backoff) in HANDLE_EVENT_RETRY_BACKOFFS.iter().enumerate() {
        tokio::time::sleep(*backoff).await;
        match handle_event(db_conn, manager, envelope, broker).await {
            Ok(()) => return,
            Err(e) => {
                let attempt_num = attempt + 2;
                let is_last = attempt + 1 == HANDLE_EVENT_RETRY_BACKOFFS.len();
                let level = if is_last { "ERROR" } else { "WARN" };
                eprintln!(
                    "[lifecycle][{level}] handle_event failed (attempt {attempt_num}{}) \
                     for {:?}: {e}",
                    if is_last {
                        ", giving up"
                    } else {
                        ", will retry"
                    },
                    envelope.payload
                );
            }
        }
    }
}

pub(crate) async fn handle_event(
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    envelope: &EventEnvelope,
    broker: Option<&Arc<DelegationBroker>>,
) -> Result<(), DbError> {
    match &envelope.payload {
        AcpEvent::ToolCall {
            tool_call_id,
            title,
            raw_input,
            ..
        } => {
            // MCP clients don't reliably populate `_meta.tool_use_id`, so we
            // capture every parent-side `delegate_to_agent` tool_call_id
            // here. The broker pops the most recent one when the matching
            // MCP round-trip arrives. See [`DelegationBroker::register_pending_tool_call`].
            //
            // ACP `title` is a free-form human-readable string the agent
            // composes from the tool name (Codex emits the bare MCP method,
            // Claude Code emits "Run <method>", others phrase it as
            // "Delegate to <agent>"). Pair the title match with a raw_input
            // shape check so we don't miss a delegation just because the
            // host re-phrased the title.
            if let Some(b) = broker {
                if is_delegation_invocation(title, raw_input.as_deref()) {
                    b.register_pending_tool_call(&envelope.connection_id, tool_call_id.clone())
                        .await;
                }
            }
            Ok(())
        }
        AcpEvent::SessionStarted { session_id } => {
            // Look up conversation_id from the live state.
            let Some(state_arc) = manager.get_state(&envelope.connection_id).await else {
                return Ok(());
            };
            let conversation_id = state_arc.read().await.conversation_id;
            if let Some(cid) = conversation_id {
                conversation_service::update_external_id(db_conn, cid, session_id.clone()).await?;
            }
            Ok(())
        }
        AcpEvent::TurnComplete { stop_reason, .. } => {
            // Centralized status transition: when the agent reports the turn
            // is done, flip the conversation row and re-broadcast the change
            // as `ConversationStatusChanged`. This lives in the lifecycle
            // subscriber (rather than at the original emit site in
            // `acp/connection.rs`) so the write is decoupled from the
            // protocol-event hot path AND survives a frontend refresh
            // mid-turn — the row gets the correct status even if no
            // browser is connected to react to TurnComplete itself.
            //
            // The target status depends on the stop reason: `end_turn` is the
            // only success case and goes to `PendingReview`. `refusal`,
            // `max_tokens`, `max_turn_requests`, `unknown`, and `empty`
            // indicate the turn failed (often a backend/gateway error
            // masquerading as `Refusal` per the ACP spec gap, or — common
            // with OpenCode — a silent EndTurn that produced no output), so
            // we flip to `Cancelled` and pair the transition with an
            // `AcpEvent::Error` toast emitted upstream by `connection.rs`.
            // `cancelled` is already written by `manager.cancel()` (eager
            // CAS InProgress → Cancelled at the user-cancel entry point), so
            // we leave it alone here. `completed` transitions remain
            // frontend-driven.
            let target_status = match stop_reason.as_str() {
                "end_turn" => Some(ConversationStatus::PendingReview),
                "refusal" | "max_tokens" | "max_turn_requests" | "unknown" | "empty" => {
                    Some(ConversationStatus::Cancelled)
                }
                // `cancelled` and any future reason: don't write here.
                _ => None,
            };
            let Some((state_arc, emitter)) =
                manager.get_state_and_emitter(&envelope.connection_id).await
            else {
                return Ok(());
            };
            let (conversation_id, last_text) = {
                let snap = state_arc.read().await;
                (snap.conversation_id, snap.last_assistant_text.clone())
            };
            // No conversation row bound (defensive — should never happen in
            // practice since `send_prompt_linked` runs before TurnComplete can
            // fire). Nothing to update.
            let Some(cid) = conversation_id else {
                return Ok(());
            };
            if let Some(ts) = target_status.clone() {
                // DB write before emit so any downstream subscriber that observes
                // the ConversationStatusChanged event can assume the row is
                // already at the target status.
                conversation_service::update_status(db_conn, cid, ts.clone()).await?;
                emit_with_state(
                    &state_arc,
                    &emitter,
                    AcpEvent::ConversationStatusChanged {
                        conversation_id: cid,
                        status: ts,
                    },
                )
                .await;
            }

            // If this conversation was spawned by a delegation, resolve the
            // pending broker call. The broker maps the outcome onto the
            // parent's `tool_use_id` via the registered `call_id`.
            if let Some(b) = broker {
                forward_turn_complete_to_broker(
                    db_conn,
                    b.as_ref(),
                    cid,
                    stop_reason.as_str(),
                    last_text,
                )
                .await;
            }
            Ok(())
        }
        // Other events don't need cross-connection DB persistence today; extend
        // this dispatcher with new arms as the lifecycle scope grows.
        _ => Ok(()),
    }
}

/// On TurnComplete for a delegation child, resolve the pending broker call
/// and let the broker drive the rest of the lifecycle (meta write, the
/// `AcpEvent::DelegationCompleted` emit against the parent stream, child
/// disconnect, tx.send). Keeping the emit responsibility inside
/// `broker.complete_call` is what guarantees the broker's other terminal
/// paths (`timeout` / `cancel_by_child_connection` / `cancel_by_parent`)
/// also surface the event — see
/// `.docs/issues/2026-05-24-delegation-termination-cascade.md`.
async fn forward_turn_complete_to_broker(
    db_conn: &DatabaseConnection,
    broker: &DelegationBroker,
    conversation_id: i32,
    stop_reason: &str,
    last_text: Option<String>,
) {
    let row = match conversation_service::get_by_id(db_conn, conversation_id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "[delegation][lifecycle] couldn't fetch child conversation \
                 {conversation_id} for outcome routing: {e}"
            );
            return;
        }
    };
    let call_id = match row.delegation_call_id.clone() {
        Some(id) => id,
        None => return, // not a delegation child; nothing to do.
    };
    if row.parent_tool_use_id.is_none() {
        eprintln!(
            "[delegation][lifecycle] conversation {conversation_id} has \
             delegation_call_id but no parent_tool_use_id; dropping"
        );
        return;
    }
    let agent_type = row.agent_type;
    let outcome = match stop_reason {
        "end_turn" => DelegationOutcome::Ok(DelegationSuccess {
            text: last_text.unwrap_or_default(),
            child_conversation_id: conversation_id,
            child_agent_type: agent_type,
            turn_count: 1,
            duration_ms: 0,
            token_usage: None,
        }),
        "cancelled" => DelegationOutcome::from_err(
            DelegationError::Canceled {
                reason: "child session was cancelled".into(),
            },
            Some(conversation_id),
        ),
        // Each child turn-failure reason gets a distinct wire code so the
        // parent UI can show a more useful error label than a generic
        // "subagent error". Mirrors the parent's own
        // `turn_failure_error_event` mapping in `connection.rs`.
        "refusal" => {
            DelegationOutcome::from_err(DelegationError::ChildRefusal, Some(conversation_id))
        }
        "max_tokens" => {
            DelegationOutcome::from_err(DelegationError::ChildMaxTokens, Some(conversation_id))
        }
        "max_turn_requests" => DelegationOutcome::from_err(
            DelegationError::ChildMaxTurnRequests,
            Some(conversation_id),
        ),
        "empty" => DelegationOutcome::from_err(DelegationError::ChildEmpty, Some(conversation_id)),
        other => DelegationOutcome::from_err(
            DelegationError::ChildUnknown(other.to_string()),
            Some(conversation_id),
        ),
    };
    broker.complete_call(&call_id, outcome).await;
}

/// Snapshot the connection's `(state, emitter)` into the lifecycle cache when
/// `ConversationLinked` arrives. Idempotent on repeat calls (re-link on the
/// already-bound path is a no-op so we don't churn the cached refs).
async fn try_cache_link(
    cache: &mut HashMap<String, CachedConn>,
    manager: &ConnectionManager,
    connection_id: &str,
    conversation_id: i32,
) {
    if cache.contains_key(connection_id) {
        return;
    }
    // The connection is necessarily still in the manager at this point —
    // `ConversationLinked` is emitted by `send_prompt_linked` from the
    // connection's own send path, well before any disconnect.
    let Some((state, emitter)) = manager.get_state_and_emitter(connection_id).await else {
        eprintln!(
            "[lifecycle][WARN] ConversationLinked for unknown connection {connection_id}; \
             skipping cache (terminal-status hand-off will no-op)"
        );
        return;
    };
    cache.insert(
        connection_id.to_string(),
        CachedConn {
            conversation_id,
            state,
            emitter,
        },
    );
}

/// Handle `StatusChanged{Disconnected}` / `Error` for a cached connection:
/// CAS the row from `InProgress` → `Cancelled` (preserves any prior
/// `PendingReview` from `TurnComplete` and any user-driven `Completed`),
/// re-emit `ConversationStatusChanged` if the write took effect.
///
/// Removing the cache entry on first terminal event handles the
/// `Error` → `Disconnected` sequence that `connection.rs` emits on the error
/// path: the second event finds an empty cache and is a clean no-op, so we
/// don't pay a redundant DB read.
async fn handle_terminal_event(
    db_conn: &DatabaseConnection,
    cache: &mut HashMap<String, CachedConn>,
    connection_id: &str,
) -> Result<(), DbError> {
    let Some(entry) = cache.remove(connection_id) else {
        return Ok(());
    };
    let cid = entry.conversation_id;
    let changed = conversation_service::update_status_if(
        db_conn,
        cid,
        ConversationStatus::InProgress,
        ConversationStatus::Cancelled,
    )
    .await?;
    if !changed {
        return Ok(());
    }
    emit_with_state(
        &entry.state,
        &entry.emitter,
        AcpEvent::ConversationStatusChanged {
            conversation_id: cid,
            status: ConversationStatus::Cancelled,
        },
    )
    .await;
    Ok(())
}

/// On a non-TurnComplete terminal event (Disconnected / Error) for a
/// delegation child, surface a `canceled` outcome to the broker. The
/// child's DB row may already be marked `Cancelled` by `handle_terminal_event`
/// above; this separately wakes the parent's pending `delegate_to_agent`
/// tool_use_id. Match-by-`child_connection_id` is O(pending), bounded by
/// active delegations.
async fn forward_disconnect_to_broker(broker: &DelegationBroker, connection_id: &str) {
    broker.cancel_by_child_connection(connection_id).await;
}

/// True when the ACP `tool_call` smells like an invocation of the
/// `delegate_to_agent` MCP tool. Defensive on both inputs because the host
/// agent gets to decide both fields:
///
/// * `title` is a free-form human-readable string the host composes. Some
///   hosts copy the MCP method verbatim (`mcp__codeg-delegate__delegate_to_agent`),
///   some prefix it with a verb (`Run mcp__…__delegate_to_agent`), some
///   rephrase it (`Delegate to codex`). We match by substring so any
///   form containing `delegate_to_agent` is captured.
/// * `raw_input` is the JSON arg blob the agent sent to the MCP server. The
///   `delegate_to_agent` schema requires `agent_type` AND `task`; presence
///   of both is a near-zero false-positive shape check that catches any
///   host that mangles the title beyond recognition.
fn is_delegation_invocation(title: &str, raw_input: Option<&str>) -> bool {
    let normalized_title = title.to_ascii_lowercase().replace([' ', '-'], "_");
    if normalized_title.contains("delegate_to_agent") {
        return true;
    }
    if let Some(raw) = raw_input {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
            if let Some(obj) = v.as_object() {
                let has_task = obj.get("task").and_then(|t| t.as_str()).is_some();
                let has_agent_type = obj.get("agent_type").and_then(|a| a.as_str()).is_some();
                if has_task && has_agent_type {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod delegation_title_tests {
    use super::is_delegation_invocation;

    #[test]
    fn matches_bare_method_in_title() {
        assert!(is_delegation_invocation("delegate_to_agent", None));
        assert!(is_delegation_invocation("Delegate To Agent", None));
        assert!(is_delegation_invocation("delegate-to-agent", None));
    }

    #[test]
    fn matches_mcp_prefixed_method_in_title() {
        assert!(is_delegation_invocation(
            "mcp__codeg-delegate__delegate_to_agent",
            None
        ));
        assert!(is_delegation_invocation(
            "Run mcp__codeg__delegate_to_agent",
            None
        ));
    }

    #[test]
    fn matches_via_raw_input_shape_when_title_is_unrecognized() {
        let raw = r#"{"agent_type":"codex","task":"smoke test"}"#;
        assert!(is_delegation_invocation("Delegate to codex", Some(raw)));
        assert!(is_delegation_invocation("anything", Some(raw)));
    }

    #[test]
    fn rejects_unrelated_tools() {
        assert!(!is_delegation_invocation("write", None));
        assert!(!is_delegation_invocation("agent", None));
        assert!(!is_delegation_invocation("delegate_other_thing", None));
        assert!(!is_delegation_invocation(
            "write",
            Some(r#"{"path":"/tmp/x","content":"y"}"#)
        ));
    }
}

/// Per-connection worker that owns the cache for one connection and
/// serializes its DB writes. Multiple connections run in parallel; within a
/// connection, ordering is preserved by the mpsc FIFO. Decouples the bus
/// receiver from DB-write latency — a slow SQLite write on connection A no
/// longer blocks events for connection B from being drained off the
/// broadcast buffer (the prior failure mode that pushed `lagged_count`).
async fn connection_worker_loop(
    connection_id: String,
    db: DatabaseConnection,
    manager: ConnectionManager,
    broker: Option<Arc<DelegationBroker>>,
    mut rx: mpsc::Receiver<Arc<EventEnvelope>>,
) {
    // 1-entry HashMap so we can reuse `handle_terminal_event` (also keeps the
    // existing test surface intact — tests still drive a `&mut HashMap`).
    let mut cache: HashMap<String, CachedConn> = HashMap::new();
    while let Some(envelope_arc) = rx.recv().await {
        let envelope: &EventEnvelope = envelope_arc.as_ref();
        match &envelope.payload {
            AcpEvent::ConversationLinked {
                conversation_id, ..
            } => {
                try_cache_link(&mut cache, &manager, &connection_id, *conversation_id).await;
            }
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected,
            }
            | AcpEvent::Error { .. } => {
                if let Err(e) = handle_terminal_event(&db, &mut cache, &connection_id).await {
                    eprintln!("[lifecycle][ERROR] terminal event for {connection_id}: {e}");
                }
                // If this connection owned a delegation child, surface a
                // terminal outcome to the broker so the parent's pending
                // tool_use_id doesn't dangle.
                if let Some(b) = broker.as_ref() {
                    forward_disconnect_to_broker(b.as_ref(), &connection_id).await;
                }
            }
            _ => {
                handle_event_with_retry(&db, &manager, envelope, broker.as_ref()).await;
            }
        }
    }
}

/// Subscribe to the in-process bus synchronously and return the dispatcher
/// loop future. Filters out events the lifecycle worker doesn't care about
/// (high-frequency ContentDelta / ToolCall / PermissionRequest etc.) and
/// fans the rest out to per-connection worker tasks. Within a single
/// connection, ordering is preserved by the per-worker mpsc; across
/// connections, workers run independently so a slow SQLite write on one
/// connection doesn't backpressure the others.
///
/// All forwarded events (the 5 types in `is_lifecycle_relevant`) use
/// blocking `send().await` to guarantee delivery even when the worker
/// mailbox is full — `SessionStarted` (writes external_id) and
/// `TurnComplete` (writes terminal status) are correctness-critical and
/// silently dropping either leaves the conversation row in a permanently
/// wrong state. Filtering keeps the queue from filling on noise traffic
/// so the blocking path is rarely exercised.
///
/// The `subscribe()` call happens here, before the future is returned, so any
/// events emitted between this call and the first poll are buffered by the
/// broadcast channel rather than dropped.
pub fn lifecycle_subscriber_task(
    db_conn: DatabaseConnection,
    manager: ConnectionManager,
    bus: Arc<InternalEventBus>,
    broker: Option<Arc<DelegationBroker>>,
) -> impl Future<Output = ()> + Send + 'static {
    let mut rx = bus.subscribe();
    let metrics = Arc::clone(bus.metrics());
    async move {
        // connection_id → worker mailbox. Workers are spawned lazily on the
        // connection's first relevant event and torn down after a terminal
        // event by dropping the sender (worker drains its queue and exits).
        let mut workers: HashMap<String, mpsc::Sender<Arc<EventEnvelope>>> = HashMap::new();
        loop {
            match rx.recv().await {
                Ok(envelope_arc) => {
                    // Fast-path filter: skip events the worker would no-op.
                    // Avoids spawning a worker for connections that only emit
                    // high-frequency noise and avoids crowding existing
                    // workers' mailboxes.
                    if !is_lifecycle_relevant(&envelope_arc.payload) {
                        continue;
                    }

                    let conn_id = envelope_arc.connection_id.clone();
                    let is_terminal = matches!(
                        &envelope_arc.payload,
                        AcpEvent::StatusChanged {
                            status: ConnectionStatus::Disconnected
                        } | AcpEvent::Error { .. }
                    );

                    let tx = workers.entry(conn_id.clone()).or_insert_with(|| {
                        let (tx, worker_rx) =
                            mpsc::channel::<Arc<EventEnvelope>>(WORKER_QUEUE_CAPACITY);
                        let db_clone = db_conn.clone();
                        let mgr_clone = manager.clone_ref();
                        let broker_clone = broker.clone();
                        let id_clone = conn_id.clone();
                        tokio::spawn(connection_worker_loop(
                            id_clone,
                            db_clone,
                            mgr_clone,
                            broker_clone,
                            worker_rx,
                        ));
                        tx
                    });

                    // Two-phase send: try non-blocking first (the common
                    // case), only `await` when the mailbox is actually full.
                    // Counts queue-full as back-pressure observation rather
                    // than a drop event — nothing is dropped, the dispatcher
                    // just waits for the worker to make room.
                    let send_result = match tx.try_send(envelope_arc) {
                        Ok(()) => Ok(()),
                        Err(mpsc::error::TrySendError::Full(env)) => {
                            metrics
                                .worker_queue_full_count
                                .fetch_add(1, Ordering::Relaxed);
                            eprintln!(
                                "[lifecycle][WARN] worker queue full for \
                                 {conn_id}, awaiting drain (back-pressure)"
                            );
                            tx.send(env).await.map_err(|_| ())
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
                    };

                    if send_result.is_err() {
                        // Worker exited unexpectedly (panic). Clean up the
                        // stale entry so the next relevant event respawns.
                        workers.remove(&conn_id);
                        continue;
                    }

                    if is_terminal {
                        // Drop the sender; worker drains the queue then
                        // exits. Releases the per-connection `CachedConn`
                        // (state Arc + emitter) the worker was holding.
                        workers.remove(&conn_id);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // Lagged at the bus level. Now that the dispatcher
                    // filters and only blocks on the rare relevant events,
                    // this should only fire under genuine emit-rate spikes
                    // exceeding the 4096 broadcast capacity.
                    eprintln!(
                        "[lifecycle][WARN] internal bus lagged, dropped {skipped} events \
                         (emit rate exceeded broadcast capacity)"
                    );
                    metrics.lagged_count.fetch_add(skipped, Ordering::Relaxed);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    eprintln!("[lifecycle] internal bus closed; dispatcher exiting");
                    // Drop all worker senders; workers drain & exit on their own.
                    drop(workers);
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::session_state::SessionState;
    use crate::db::test_helpers;
    use crate::models::agent::AgentType;
    use crate::web::event_bridge::EventEmitter;
    use std::sync::Arc;
    use tokio::sync::{mpsc, RwLock};

    fn fake_connection_with_state(
        id: &str,
        conv_id: Option<i32>,
    ) -> crate::acp::connection::AgentConnection {
        let (tx, _rx) = mpsc::channel(1);
        let mut state = SessionState::new(
            id.to_string(),
            AgentType::ClaudeCode,
            None,
            "test-window".to_string(),
            None,
        );
        state.conversation_id = conv_id;
        crate::acp::connection::AgentConnection {
            id: id.to_string(),
            agent_type: AgentType::ClaudeCode,
            status: crate::acp::types::ConnectionStatus::Connected,
            owner_window_label: "test-window".to_string(),
            cmd_tx: tx,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    #[tokio::test]
    async fn handle_event_writes_external_id_when_conversation_bound() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/test").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::SessionStarted {
                session_id: "ext-99".into(),
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        let reloaded = conversation_service::get_by_id(&db.conn, conv.id)
            .await
            .unwrap();
        assert_eq!(reloaded.external_id.as_deref(), Some("ext-99"));
    }

    #[tokio::test]
    async fn handle_event_is_noop_when_no_conversation_bound() {
        let db = test_helpers::fresh_in_memory_db().await;
        // Seed a sentinel conversation row that should remain untouched.
        let folder_id = test_helpers::seed_folder(&db, "/tmp/test-noop").await;
        let sentinel =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c1".to_string(), fake_connection_with_state("c1", None));
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::SessionStarted {
                session_id: "should-not-write".into(),
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();

        // Sentinel row must still have no external_id — dispatcher correctly
        // skipped the write because the connection had no conversation_id.
        let reloaded = conversation_service::get_by_id(&db.conn, sentinel.id)
            .await
            .unwrap();
        assert!(
            reloaded.external_id.is_none(),
            "sentinel row should be untouched"
        );
    }

    /// Helper: read the raw `status` column off the conversation entity
    /// (the `conversation_service::get_by_id` summary type stringifies status,
    /// which loses round-trip parity with the `ConversationStatus` enum).
    async fn read_row_status(
        db: &crate::db::AppDatabase,
        conversation_id: i32,
    ) -> ConversationStatus {
        use crate::db::entities::conversation;
        use sea_orm::EntityTrait;
        conversation::Entity::find_by_id(conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("conversation row exists")
            .status
    }

    #[tokio::test]
    async fn handle_event_writes_pending_review_on_turn_complete() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/turn-complete").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        // Sanity precondition: row was created in InProgress (the
        // conversation_service::create default).
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::InProgress
        );

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "ext-1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::PendingReview
        );
    }

    #[tokio::test]
    async fn handle_event_writes_cancelled_on_turn_failure_stop_reasons() {
        // OpenCode (and similar agents) maps backend errors to `Refusal`.
        // The lifecycle subscriber must flip the conversation to Cancelled
        // for refusal/max_tokens/max_turn_requests/unknown so the user sees
        // a terminal state instead of a misleading PendingReview ("待审查").
        let cases = [
            "refusal",
            "max_tokens",
            "max_turn_requests",
            "unknown",
            "empty",
        ];
        for stop_reason in cases {
            let db = test_helpers::fresh_in_memory_db().await;
            let folder_id =
                test_helpers::seed_folder(&db, &format!("/tmp/turn-fail-{stop_reason}")).await;
            let conv =
                conversation_service::create(&db.conn, folder_id, AgentType::OpenCode, None, None)
                    .await
                    .unwrap();

            let mgr = ConnectionManager::new();
            {
                let mut map = mgr.connections.lock().await;
                map.insert(
                    "c1".to_string(),
                    fake_connection_with_state("c1", Some(conv.id)),
                );
            }
            let env = EventEnvelope {
                seq: 1,
                connection_id: "c1".to_string(),
                payload: AcpEvent::TurnComplete {
                    session_id: "ext-1".into(),
                    stop_reason: stop_reason.into(),
                    agent_type: "open_code".into(),
                },
            };
            handle_event(&db.conn, &mgr, &env, None).await.unwrap();
            assert_eq!(
                read_row_status(&db, conv.id).await,
                ConversationStatus::Cancelled,
                "stop_reason={stop_reason} must flip the row to Cancelled"
            );
        }
    }

    #[tokio::test]
    async fn handle_event_skips_write_on_cancelled_stop_reason() {
        // `cancelled` is already written by `manager.cancel()` (eager CAS
        // InProgress → Cancelled at the user-cancel entry point), so the
        // TurnComplete arm must not double-write.
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/turn-cancelled").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "ext-1".into(),
                stop_reason: "cancelled".into(),
                agent_type: "claude_code".into(),
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::InProgress,
            "TurnComplete{{cancelled}} must not overwrite the row — user-cancel path owns it"
        );
    }

    #[tokio::test]
    async fn handle_event_pending_review_is_noop_when_no_conversation_bound() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/no-conv").await;
        // Sentinel row: must remain in its initial status (InProgress) since
        // the connection is unbound and the dispatcher should skip the write.
        let sentinel =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        assert_eq!(sentinel.status, ConversationStatus::InProgress);

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c1".to_string(), fake_connection_with_state("c1", None));
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "ext-1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, sentinel.id).await,
            ConversationStatus::InProgress,
            "row must be untouched when no conversation_id is bound to the connection"
        );
    }

    /// Helper: install one cache entry seeded from a manager-registered
    /// connection. Mirrors the runtime path where `try_cache_link` populates
    /// the cache on `ConversationLinked`.
    async fn seed_cache(
        cache: &mut HashMap<String, CachedConn>,
        manager: &ConnectionManager,
        connection_id: &str,
        conversation_id: i32,
    ) {
        try_cache_link(cache, manager, connection_id, conversation_id).await;
    }

    #[tokio::test]
    async fn handle_terminal_event_writes_cancelled_when_in_progress() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/term-cancel").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        // Default-creates as InProgress.
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::InProgress
        );

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }
        let mut cache: HashMap<String, CachedConn> = HashMap::new();
        seed_cache(&mut cache, &mgr, "c1", conv.id).await;
        assert!(
            cache.contains_key("c1"),
            "ConversationLinked should populate cache"
        );

        handle_terminal_event(&db.conn, &mut cache, "c1")
            .await
            .unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::Cancelled,
            "in_progress row must be flipped to cancelled"
        );
        assert!(
            !cache.contains_key("c1"),
            "cache entry must be drained after first terminal event"
        );
    }

    #[tokio::test]
    async fn handle_terminal_event_preserves_pending_review() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/term-pr").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        // Simulate a TurnComplete-driven row already at PendingReview.
        conversation_service::update_status(&db.conn, conv.id, ConversationStatus::PendingReview)
            .await
            .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }
        let mut cache: HashMap<String, CachedConn> = HashMap::new();
        seed_cache(&mut cache, &mgr, "c1", conv.id).await;

        handle_terminal_event(&db.conn, &mut cache, "c1")
            .await
            .unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::PendingReview,
            "CAS must not overwrite PendingReview when subscriber sees terminal event \
             after TurnComplete"
        );
    }

    #[tokio::test]
    async fn handle_terminal_event_preserves_user_completed() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/term-completed").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        // User manually marked the conversation completed before the
        // disconnect arrived.
        conversation_service::update_status(&db.conn, conv.id, ConversationStatus::Completed)
            .await
            .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }
        let mut cache: HashMap<String, CachedConn> = HashMap::new();
        seed_cache(&mut cache, &mgr, "c1", conv.id).await;

        handle_terminal_event(&db.conn, &mut cache, "c1")
            .await
            .unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::Completed,
            "user-driven completed must survive the lifecycle terminal-event handler"
        );
    }

    #[tokio::test]
    async fn handle_terminal_event_drains_cache_on_error_then_disconnected() {
        // connection.rs emits `Error` → `Disconnected` on failure. The first
        // event drains the cache so the second is a clean no-op (no extra DB
        // read, no second CAS attempt).
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/term-pair").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }
        let mut cache: HashMap<String, CachedConn> = HashMap::new();
        seed_cache(&mut cache, &mgr, "c1", conv.id).await;

        // First terminal event: cancels, drains.
        handle_terminal_event(&db.conn, &mut cache, "c1")
            .await
            .unwrap();
        assert!(!cache.contains_key("c1"));
        // Second terminal event: empty cache, returns Ok with no DB writes.
        handle_terminal_event(&db.conn, &mut cache, "c1")
            .await
            .unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn handle_terminal_event_noop_when_connection_unknown() {
        // Defensive: a terminal event for a connection that never linked a
        // conversation (cache miss) must not error out or touch any row.
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/term-unknown").await;
        let sentinel =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        assert_eq!(sentinel.status, ConversationStatus::InProgress);

        let mut cache: HashMap<String, CachedConn> = HashMap::new();
        handle_terminal_event(&db.conn, &mut cache, "ghost-connection")
            .await
            .unwrap();
        assert_eq!(
            read_row_status(&db, sentinel.id).await,
            ConversationStatus::InProgress,
            "no conversation should have been touched"
        );
    }

    #[tokio::test]
    async fn handle_event_is_noop_for_unrelated_events() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/test-unrelated").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }
        // ContentDelta should be a no-op even though the connection IS bound.
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::ContentDelta { text: "hi".into() },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();

        let reloaded = conversation_service::get_by_id(&db.conn, conv.id)
            .await
            .unwrap();
        assert!(
            reloaded.external_id.is_none(),
            "non-SessionStarted events must not write external_id"
        );
    }

    // ── Dispatcher-level regression coverage ─────────────────────────────
    //
    // These exercise the full `lifecycle_subscriber_task` (bus → filter →
    // dispatcher → per-conn worker → DB) so the integration between the
    // filter predicate and the worker's match arms cannot silently drift.

    use crate::acp::internal_bus::{EventBusMetrics, InternalEventBus};
    use std::time::Duration;

    /// Predicate must accept exactly the event types the worker handles.
    /// If a future worker arm starts caring about a new event type without
    /// updating `is_lifecycle_relevant`, this test catches the drift.
    #[test]
    fn is_lifecycle_relevant_matches_worker_arms() {
        // Accepted (worker has dedicated handling):
        assert!(is_lifecycle_relevant(&AcpEvent::SessionStarted {
            session_id: "s".into(),
        }));
        assert!(is_lifecycle_relevant(&AcpEvent::TurnComplete {
            session_id: "s".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
        }));
        assert!(is_lifecycle_relevant(&AcpEvent::ConversationLinked {
            conversation_id: 1,
            folder_id: 1,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        }));
        // ToolCall must enter the queue so the delegation broker's
        // pending tool_call_id capture (see `handle_event`'s ToolCall
        // arm) actually runs.
        assert!(is_lifecycle_relevant(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "delegate_to_agent".into(),
            kind: "other".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        }));
        assert!(is_lifecycle_relevant(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Disconnected,
        }));
        assert!(is_lifecycle_relevant(&AcpEvent::Error {
            message: "boom".into(),
            agent_type: "claude_code".into(),
            code: None,
        }));

        // Rejected (worker no-ops on these — must not enter the queue):
        assert!(!is_lifecycle_relevant(&AcpEvent::ContentDelta {
            text: "x".into(),
        }));
        assert!(!is_lifecycle_relevant(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Connected,
        }));
        assert!(!is_lifecycle_relevant(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        }));
    }

    /// Poll the conversation row's status until it matches `expected` or
    /// the timeout elapses. Used because the dispatcher exits as soon as
    /// the bus closes, but its workers may still be draining queued events
    /// when `dispatcher.await` returns.
    async fn poll_status(
        db: &crate::db::AppDatabase,
        conversation_id: i32,
        expected: ConversationStatus,
        timeout: Duration,
    ) -> ConversationStatus {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let observed = read_row_status(db, conversation_id).await;
            if observed == expected || std::time::Instant::now() >= deadline {
                return observed;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn poll_external_id(
        db: &crate::db::AppDatabase,
        conversation_id: i32,
        expected: &str,
        timeout: Duration,
    ) -> Option<String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let observed = conversation_service::get_by_id(&db.conn, conversation_id)
                .await
                .unwrap()
                .external_id;
            if observed.as_deref() == Some(expected) || std::time::Instant::now() >= deadline {
                return observed;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Filter must keep high-frequency events from spawning a worker or
    /// reaching `handle_event_with_retry`. Verified by emitting only
    /// ContentDelta and asserting the conversation row stays untouched.
    #[tokio::test]
    async fn dispatcher_filter_drops_high_frequency_events_at_source() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/disp-filter").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics.clone()));

        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            None,
        ));

        // Subscribe AFTER spawn would race; the bus's broadcast channel
        // requires a receiver to count. The dispatcher subscribes
        // synchronously inside `lifecycle_subscriber_task`, so by the time
        // `tokio::spawn` returns, the receiver IS registered.
        for i in 0..50 {
            bus.send(Arc::new(EventEnvelope {
                seq: i,
                connection_id: "c1".to_string(),
                payload: AcpEvent::ContentDelta {
                    text: format!("delta {i}"),
                },
            }));
        }

        // Close the bus to drain the dispatcher.
        drop(bus);
        let _ = dispatcher.await;
        // Brief settle for any worker that might have spawned.
        tokio::time::sleep(Duration::from_millis(20)).await;

        let row = conversation_service::get_by_id(&db.conn, conv.id)
            .await
            .unwrap();
        assert!(
            row.external_id.is_none(),
            "filter must keep ContentDelta from triggering DB writes"
        );
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::InProgress,
            "row must be untouched"
        );
    }

    /// Happy-path through the full dispatcher → worker → DB chain.
    /// SessionStarted writes external_id; TurnComplete{end_turn} flips the
    /// row to PendingReview. Both must land.
    #[tokio::test]
    async fn dispatcher_delivers_session_started_and_turn_complete_to_db() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/disp-happy").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics));
        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            None,
        ));

        bus.send(Arc::new(EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::SessionStarted {
                session_id: "ext-final".into(),
            },
        }));
        bus.send(Arc::new(EventEnvelope {
            seq: 2,
            connection_id: "c1".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "ext-final".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        }));

        let observed_external =
            poll_external_id(&db, conv.id, "ext-final", Duration::from_millis(500)).await;
        let observed_status = poll_status(
            &db,
            conv.id,
            ConversationStatus::PendingReview,
            Duration::from_millis(500),
        )
        .await;

        drop(bus);
        let _ = dispatcher.await;

        assert_eq!(observed_external.as_deref(), Some("ext-final"));
        assert_eq!(observed_status, ConversationStatus::PendingReview);
    }

    /// Burst: emit a long sequence of relevant events followed by a
    /// `TurnComplete{end_turn}`. With the prior `try_send` + drop logic,
    /// any sufficiently-long burst could push the TurnComplete off the
    /// worker mailbox, leaving the row at InProgress. With the blocking
    /// `send().await` fallback the dispatcher waits for the worker to
    /// drain — so the TurnComplete MUST land regardless of burst size.
    ///
    /// The N=200 burst exceeds `WORKER_QUEUE_CAPACITY` (64) so the
    /// dispatcher exercises the `try_send → send.await` fallback path.
    /// Even if SQLite serves writes quickly enough to keep the queue
    /// shallow most of the time, exceeding capacity at any instant
    /// triggers the back-pressure code path that we're regressing on.
    #[tokio::test]
    async fn dispatcher_delivers_turn_complete_after_relevant_event_burst() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/disp-burst").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(conv.id)),
            );
        }

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics));
        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            None,
        ));

        // Burst of 200 SessionStarted events (each writes external_id).
        // 200 > WORKER_QUEUE_CAPACITY (64) ensures the back-pressure path
        // is exercised.
        for i in 1..=200u64 {
            bus.send(Arc::new(EventEnvelope {
                seq: i,
                connection_id: "c1".to_string(),
                payload: AcpEvent::SessionStarted {
                    session_id: format!("ext-{i:03}"),
                },
            }));
        }

        // The critical event: TurnComplete that MUST flip the row.
        bus.send(Arc::new(EventEnvelope {
            seq: 201,
            connection_id: "c1".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "ext-200".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
            },
        }));

        // Wait for the worker to fully drain. The TurnComplete is at the
        // tail of the queue, so observing PendingReview proves nothing
        // before it was dropped.
        let observed = poll_status(
            &db,
            conv.id,
            ConversationStatus::PendingReview,
            Duration::from_secs(2),
        )
        .await;

        drop(bus);
        let _ = dispatcher.await;

        assert_eq!(
            observed,
            ConversationStatus::PendingReview,
            "TurnComplete at the tail of a 200-event burst MUST be delivered \
             (regression test for `try_send` drop bug)"
        );
    }
}
