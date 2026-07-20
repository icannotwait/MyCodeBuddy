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

use sea_orm::{DatabaseConnection, EntityTrait, TransactionTrait};
use tokio::sync::{broadcast, mpsc};

use crate::acp::delegation::broker::{DelegationBroker, DelegationMatchKey};
use crate::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationSuccess};
use crate::acp::internal_bus::{
    is_lifecycle_critical, EventBusMetrics, InternalEventBus, InternalEventEnvelope,
};
use crate::acp::manager::ConnectionManager;
use crate::acp::session_state::SessionState;
use crate::acp::types::{AcpEvent, ConnectionStatus, EventEnvelope};
use crate::auto_title::{apply_usable_completion, TurnCompletionSnapshot};
use crate::db::entities::conversation::ConversationStatus;
use crate::db::error::DbError;
use crate::db::service::conversation_service;
use crate::models::AgentType;
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
/// `ToolCall`/`ToolCallUpdate` are deliberately NOT in the accept list.
/// Delegation correlation (capturing `delegate_to_agent` tool_call_ids for
/// the broker's pending queue) used to ride the worker's `ToolCall` arm, but
/// that coupled a latency-critical, lossless registration to the DB-stalling
/// worker AND fed every `ToolCall` (including each parallel child's tool
/// stream) into worker mailboxes — pressure that could block the dispatcher
/// and lag the bus into dropping a parent's second delegation `tool_call`.
/// Registration runs on a dedicated off-select broker-tool worker via
/// `register_delegation_tool_call_from_event`, so these high-frequency events
/// never need to reach a lifecycle worker and never park the dispatcher.
fn is_lifecycle_relevant(event: &AcpEvent) -> bool {
    // Keep in sync with `internal_bus::is_lifecycle_critical` (critical lane).
    // Non-terminal `Error` is also worker-relevant for logging paths that still
    // forward it, but the critical lane only carries `terminal: true` Errors
    // (see `is_lifecycle_critical`). Worker still accepts any Error via the
    // broader match used historically for terminal-or-not branching.
    is_lifecycle_critical(event)
        || matches!(
            event,
            AcpEvent::Error {
                terminal: false,
                ..
            }
        )
}

/// Whether the dispatcher should tear down (drop the sender for) the per-
/// connection worker after forwarding this event. Two cases:
///
///   - `Disconnected` — the normal teardown signal, always emitted by
///     `connection.rs` after `run_connection` returns.
///   - `Error { terminal: true }` — defense-in-depth for the case where
///     the bus drops the trailing `Disconnected` (`Lagged`) or the
///     `run_connection` task aborts between emit sites. The worker
///     dispatches terminal work on whichever lands first (P1); without
///     also dropping the sender here, a missed `Disconnected` would leak
///     the worker task + its `CachedConn` for the lifetime of the process.
///
/// Non-terminal `Error` is NOT terminal at the dispatcher level — it also
/// fires mid-turn from `turn_failure_error_event` while the child connection
/// stays alive, and the worker must survive to process the trailing
/// `TurnComplete`. (P2 follow-up in the v0.14.3 post-mortem review.)
fn is_dispatcher_terminal(event: &AcpEvent) -> bool {
    matches!(
        event,
        AcpEvent::StatusChanged {
            status: ConnectionStatus::Disconnected
        } | AcpEvent::Error { terminal: true, .. }
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

/// Wrap `handle_internal_event` with a small backoff retry. Most failures
/// here are transient SQLite "database is locked" errors that clear within
/// a few hundred milliseconds; without a retry the conversation row would
/// silently miss its `pending_review` write and the sidebar would stay
/// stuck on `in_progress` until the next prompt's `in_progress` write.
///
/// Final failure is logged at ERROR — this is the only signal the
/// subscriber is dropping correctness on the floor, so it must be noisy.
async fn handle_internal_event_with_retry(
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    internal: &InternalEventEnvelope,
    broker: Option<&Arc<DelegationBroker>>,
) {
    match handle_internal_event(db_conn, manager, internal, broker).await {
        Ok(()) => return,
        Err(e) => {
            tracing::warn!(
                "[lifecycle][WARN] handle_event failed (attempt 1, will retry) for {:?}: {e}",
                internal.payload
            );
        }
    }
    for (attempt, backoff) in HANDLE_EVENT_RETRY_BACKOFFS.iter().enumerate() {
        tokio::time::sleep(*backoff).await;
        match handle_internal_event(db_conn, manager, internal, broker).await {
            Ok(()) => return,
            Err(e) => {
                let attempt_num = attempt + 2;
                let is_last = attempt + 1 == HANDLE_EVENT_RETRY_BACKOFFS.len();
                let level = if is_last { "ERROR" } else { "WARN" };
                tracing::warn!(
                    "[lifecycle][{level}] handle_event failed (attempt {attempt_num}{}) \
                     for {:?}: {e}",
                    if is_last {
                        ", giving up"
                    } else {
                        ", will retry"
                    },
                    internal.payload
                );
            }
        }
    }
}

/// Production lifecycle entry for bus workers. Consumes the optional
/// completion sidecar on `TurnComplete`; other events delegate to
/// [`handle_event`].
async fn handle_internal_event(
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    internal: &InternalEventEnvelope,
    broker: Option<&Arc<DelegationBroker>>,
) -> Result<(), DbError> {
    match &internal.payload {
        AcpEvent::TurnComplete {
            stop_reason,
            mark_awaiting_reply,
            ..
        } => {
            handle_turn_complete_internal(
                db_conn,
                manager,
                &internal.connection_id,
                stop_reason,
                *mark_awaiting_reply,
                internal.completion.as_ref().map(Arc::clone),
                broker,
            )
            .await
        }
        _ => handle_event(db_conn, manager, internal.event.as_ref(), broker).await,
    }
}

/// Sidecar-aware TurnComplete path. When a completion snapshot is present,
/// status CAS and usable-completion job updates share one transaction and
/// never re-read mutable SessionState for assistant text / locale / token /
/// conversation id. Without a sidecar, status still runs (for direct tests
/// that still call [`handle_event`]); broker result text is empty.
async fn handle_turn_complete_internal(
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    connection_id: &str,
    stop_reason: &str,
    mark_awaiting_reply: bool,
    completion: Option<Arc<TurnCompletionSnapshot>>,
    broker: Option<&Arc<DelegationBroker>>,
) -> Result<(), DbError> {
    let live = manager.get_state_and_emitter(connection_id).await;

    let (conversation_id, broker_text) = if let Some(snapshot) = completion.as_ref() {
        (
            Some(snapshot.conversation_id),
            Some(snapshot.final_text.clone()),
        )
    } else {
        // Production emits always carry a sidecar when active_turn was set.
        // Sidecar-free paths (direct unit tests via handle_event, or turns
        // without active_turn) never fall back to mutable last_assistant_text.
        let cid = match &live {
            Some((state_arc, _)) => state_arc.read().await.conversation_id,
            None => None,
        };
        (cid, None)
    };

    let Some(cid) = conversation_id else {
        tracing::warn!(
            connection_id = %connection_id,
            stop_reason = %stop_reason,
            has_completion_sidecar = completion.is_some(),
            live_state_present = live.is_some(),
            "[lifecycle] TurnComplete skipped: no conversation_id bound \
             (row will stay in_progress if still InProgress)"
        );
        return Ok(());
    };

    tracing::info!(
        connection_id = %connection_id,
        conversation_id = cid,
        stop_reason = %stop_reason,
        mark_awaiting_reply,
        has_completion_sidecar = completion.is_some(),
        "[lifecycle] handling TurnComplete"
    );

    match conversation_is_delegate(db_conn, cid).await {
        Ok(true) => {
            // Delegate ConversationStatus is broker-owned, but auto-title jobs
            // still advance on usable completions from the event-owned sidecar.
            tracing::info!(
                connection_id = %connection_id,
                conversation_id = cid,
                stop_reason = %stop_reason,
                "[lifecycle] TurnComplete on delegate row; broker owns status CAS"
            );
            if let Some(snapshot) = completion.as_ref() {
                let txn = db_conn.begin().await?;
                let transition =
                    apply_usable_completion(&txn, snapshot.as_ref(), stop_reason).await?;
                txn.commit().await?;
                if transition.became_ready {
                    crate::auto_title::notify_live_coordinator_ready();
                }
            }
            // Broker settle (complete_call → durable CAS → parent tool_result)
            // MUST NOT run under the lifecycle worker's 45s timeout. Production
            // saw Codex child TurnComplete log "broker owns status CAS" then
            // hang in complete_call for 5×45s; the first attempt moves the task
            // into `settling`, the timeout cancels mid-settle, and retries
            // no-op — child stays `running`/`in_progress` and the parent never
            // receives the MCP tool_result (Join stuck forever).
            // Auto-title stays on-worker (short DB write); settle is detached.
            spawn_forward_turn_complete_to_broker(
                db_conn,
                broker,
                cid,
                stop_reason,
                broker_text
                    .as_ref()
                    .map(|t| t.as_ref().to_string())
                    .unwrap_or_default(),
            );
            return Ok(());
        }
        Ok(false) => {}
        Err(e) => {
            tracing::error!(
                conversation_id = cid,
                error = %e,
                "[lifecycle] is_delegate probe failed; skipping generic \
                 ConversationStatus write (fail-closed)"
            );
            if let Some(snapshot) = completion.as_ref() {
                let txn = db_conn.begin().await?;
                let transition =
                    apply_usable_completion(&txn, snapshot.as_ref(), stop_reason).await?;
                txn.commit().await?;
                if transition.became_ready {
                    crate::auto_title::notify_live_coordinator_ready();
                }
            }
            spawn_forward_turn_complete_to_broker(
                db_conn,
                broker,
                cid,
                stop_reason,
                broker_text
                    .as_ref()
                    .map(|t| t.as_ref().to_string())
                    .unwrap_or_default(),
            );
            return Ok(());
        }
    }

    // One transaction: status transition + usable completion (when sidecar).
    let txn = db_conn.begin().await?;
    let cas_patch = match stop_reason {
        "end_turn" => {
            conversation_service::finish_end_turn_if_in_progress(&txn, cid, mark_awaiting_reply)
                .await?
        }
        "refusal" | "max_tokens" | "max_turn_requests" | "unknown" | "empty" => {
            conversation_service::update_status_if_with_patch(
                &txn,
                cid,
                ConversationStatus::InProgress,
                ConversationStatus::Cancelled,
            )
            .await?
        }
        _ => {
            tracing::info!(
                connection_id = %connection_id,
                conversation_id = cid,
                stop_reason = %stop_reason,
                "[lifecycle] TurnComplete stop_reason has no status CAS \
                 (e.g. cancelled already written by manager.cancel)"
            );
            None
        }
    };
    let mut became_ready = false;
    if let Some(snapshot) = completion.as_ref() {
        let transition = apply_usable_completion(&txn, snapshot.as_ref(), stop_reason).await?;
        became_ready = transition.became_ready;
    }
    txn.commit().await?;
    // Notify the durable title worker only after the transaction commits.
    if became_ready {
        crate::auto_title::notify_live_coordinator_ready();
    }

    match &cas_patch {
        Some(patch) => {
            tracing::info!(
                connection_id = %connection_id,
                conversation_id = cid,
                stop_reason = %stop_reason,
                new_status = %patch.status,
                became_ready,
                "[lifecycle] TurnComplete status CAS won"
            );
        }
        None => {
            tracing::warn!(
                connection_id = %connection_id,
                conversation_id = cid,
                stop_reason = %stop_reason,
                became_ready,
                "[lifecycle] TurnComplete status CAS lost or no-op \
                 (row may already be terminal / not in_progress)"
            );
        }
    }

    // Immediate post-commit re-read: prove the row still holds the CAS winner.
    // A concurrent unconditional `update_status_with_patch(InProgress)` (the
    // #394-class bug) can land in the same millisecond and leave the sidebar
    // spinning while logs claim "CAS won".
    let expected_after_cas: Option<ConversationStatus> = match stop_reason {
        "end_turn" if cas_patch.is_some() => Some(ConversationStatus::PendingReview),
        "refusal" | "max_tokens" | "max_turn_requests" | "unknown" | "empty"
            if cas_patch.is_some() =>
        {
            Some(ConversationStatus::Cancelled)
        }
        _ => None,
    };
    if let Some(expected) = expected_after_cas {
        match crate::db::entities::conversation::Entity::find_by_id(cid)
            .one(db_conn)
            .await
        {
            Ok(Some(row)) => {
                if row.status != expected {
                    tracing::error!(
                        connection_id = %connection_id,
                        conversation_id = cid,
                        stop_reason = %stop_reason,
                        expected = ?expected,
                        actual = ?row.status,
                        "[lifecycle][ERROR] post-CAS re-read mismatch — status \
                         overwritten after TurnComplete CAS"
                    );
                } else {
                    tracing::info!(
                        connection_id = %connection_id,
                        conversation_id = cid,
                        status = ?row.status,
                        awaiting_reply_token_set = row.awaiting_reply_token.is_some(),
                        "[lifecycle] post-CAS re-read ok"
                    );
                }
            }
            Ok(None) => {
                tracing::warn!(
                    connection_id = %connection_id,
                    conversation_id = cid,
                    "[lifecycle] post-CAS re-read: conversation row missing"
                );
            }
            Err(e) => {
                tracing::warn!(
                    connection_id = %connection_id,
                    conversation_id = cid,
                    error = %e,
                    "[lifecycle] post-CAS re-read failed"
                );
            }
        }
    }

    if let Some(patch) = cas_patch {
        let status = if stop_reason == "end_turn" {
            ConversationStatus::PendingReview
        } else {
            ConversationStatus::Cancelled
        };
        // DB status is already committed. Fan-out to SessionState + desktop
        // must not stall this worker — a hung `app.emit` / delivery path would
        // leave later TurnCompletes sitting in the per-connection mailbox and
        // strand the row at `in_progress` (seen on Grok multi-turn sessions).
        if let Some((state_arc, emitter)) = live.as_ref() {
            let state_arc = Arc::clone(state_arc);
            let emitter = emitter.clone();
            let patch = patch.clone();
            tokio::spawn(async move {
                emit_with_state(
                    &state_arc,
                    &emitter,
                    AcpEvent::ConversationStatusChanged {
                        conversation_id: cid,
                        status,
                    },
                )
                .await;
                crate::commands::conversations::emit_conversation_state(&emitter, patch);
            });
        }

        // Delayed orphan recovery: if something silent rewrote the row back to
        // InProgress after a successful end_turn CAS, and no turn is in flight
        // on this connection, re-apply pending_review and re-emit. Catches the
        // #394/#385 class without racing a legitimate follow-up prompt (those
        // set turn_in_flight before/with the InProgress write).
        if stop_reason == "end_turn" {
            let db_conn = db_conn.clone();
            let manager = manager.clone_ref();
            let connection_id = connection_id.to_string();
            tokio::spawn(async move {
                for delay in [Duration::from_secs(1), Duration::from_secs(5)] {
                    tokio::time::sleep(delay).await;
                    if let Err(e) = reconcile_orphaned_in_progress_after_turn_complete(
                        &db_conn,
                        &manager,
                        &connection_id,
                        cid,
                        mark_awaiting_reply,
                    )
                    .await
                    {
                        tracing::warn!(
                            connection_id = %connection_id,
                            conversation_id = cid,
                            error = %e,
                            "[lifecycle] orphan InProgress reconcile failed"
                        );
                    }
                }
            });
        }
    }

    Ok(())
}

/// If the row is still (or again) `in_progress` after a successful end_turn
/// CAS, and this connection has no turn in flight, re-apply the end-turn CAS
/// and fan out the state patch. No-op when a real follow-up turn owns the
/// status, or when the row is already terminal/pending_review.
async fn reconcile_orphaned_in_progress_after_turn_complete(
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    connection_id: &str,
    conversation_id: i32,
    mark_awaiting_reply: bool,
) -> Result<(), DbError> {
    let turn_in_flight = match manager.get_state_and_emitter(connection_id).await {
        Some((state_arc, _)) => state_arc.read().await.turn_in_flight,
        // Connection gone: nothing can complete a turn for this row — safe to
        // heal a stranded InProgress left after a prior CAS was clobbered.
        None => false,
    };
    if turn_in_flight {
        tracing::debug!(
            connection_id = %connection_id,
            conversation_id,
            "[lifecycle] orphan reconcile skipped: turn still in flight"
        );
        return Ok(());
    }

    let Some(row) = crate::db::entities::conversation::Entity::find_by_id(conversation_id)
        .one(db_conn)
        .await?
    else {
        return Ok(());
    };
    if row.status != ConversationStatus::InProgress {
        return Ok(());
    }

    tracing::error!(
        connection_id = %connection_id,
        conversation_id,
        "[lifecycle][ERROR] orphan InProgress after TurnComplete CAS — \
         re-applying pending_review (no turn in flight)"
    );

    let Some(patch) = conversation_service::finish_end_turn_if_in_progress(
        db_conn,
        conversation_id,
        mark_awaiting_reply,
    )
    .await?
    else {
        return Ok(());
    };

    if let Some((state_arc, emitter)) = manager.get_state_and_emitter(connection_id).await {
        emit_with_state(
            &state_arc,
            &emitter,
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status: ConversationStatus::PendingReview,
            },
        )
        .await;
        crate::commands::conversations::emit_conversation_state(&emitter, patch);
    } else {
        // Connection already torn down: still try the durable state channel if
        // any live connection emitter exists is not available — best-effort
        // log only; next list refresh will load pending_review from DB.
        tracing::info!(
            connection_id = %connection_id,
            conversation_id,
            "[lifecycle] orphan reconcile wrote pending_review; connection gone \
             (UI will converge on next refresh)"
        );
    }
    Ok(())
}

pub(crate) async fn handle_event(
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    envelope: &EventEnvelope,
    broker: Option<&Arc<DelegationBroker>>,
) -> Result<(), DbError> {
    match &envelope.payload {
        // NOTE: parent-side `delegate_to_agent` tool_call_id capture used to
        // live here (a `ToolCall` arm). It now runs in the dispatcher loop via
        // `register_delegation_tool_call_from_event`, off the DB-coupled worker
        // and across both `ToolCall` and `ToolCallUpdate`, so `ToolCall` no
        // longer reaches this worker at all (see `is_lifecycle_relevant`).
        AcpEvent::SessionStarted { session_id } => {
            // Look up conversation_id (and the emitter) from the live state.
            let Some((state_arc, emitter)) =
                manager.get_state_and_emitter(&envelope.connection_id).await
            else {
                return Ok(());
            };
            let conversation_id = state_arc.read().await.conversation_id;
            if let Some(cid) = conversation_id {
                conversation_service::update_external_id(db_conn, cid, session_id.clone()).await?;
                // The external_id just landed on the row. The create-time
                // sidebar upsert carried `external_id: null` (no session yet),
                // so re-broadcast the full summary on `conversation://changed`
                // to converge every client. Root-only (the helper skips
                // delegation children). Best-effort, after the DB write.
                crate::commands::conversations::emit_conversation_upsert(&emitter, db_conn, cid)
                    .await;
            }
            Ok(())
        }
        AcpEvent::TurnComplete {
            stop_reason,
            mark_awaiting_reply,
            ..
        } => {
            // Centralized status transition: when the agent reports the turn
            // is done, flip the conversation row and re-broadcast the change
            // as `ConversationStatusChanged`. This lives in the lifecycle
            // subscriber (rather than at the original emit site in
            // `acp/connection.rs`) so the write is decoupled from the
            // protocol-event hot path AND survives a frontend refresh
            // mid-turn — the row gets the correct status even if no
            // browser is connected to react to TurnComplete itself.
            //
            // CAS helpers from conversation_service own the write:
            // `end_turn` → finish_end_turn_if_in_progress (PendingReview +
            // optional token). Failure reasons → update_status_if_with_patch
            // (InProgress → Cancelled, clears token). Emit the existing
            // per-connection status event only when the CAS wins (`Some`),
            // then the global ConversationChange::State with the returned
            // backend patch. `cancelled` is already written by
            // `manager.cancel()`; leave it alone. `completed` transitions
            // remain frontend-driven.
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
            // Delegate rows: durable task status + sidebar ConversationStatus are
            // owned by the broker store CAS (`settle_task`). A generic
            // ConversationStatus write here would race / obscure the terminal
            // winner. Non-delegate chats keep the awaiting-reply CAS path.
            // DB probe is fail-closed: on error never fall through to generic
            // ConversationStatus mutation (may still route to broker).
            match conversation_is_delegate(db_conn, cid).await {
                Ok(true) => {
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
                    return Ok(());
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(
                        conversation_id = cid,
                        error = %e,
                        "[lifecycle] is_delegate probe failed; skipping generic \
                         ConversationStatus write (fail-closed)"
                    );
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
                    return Ok(());
                }
            }

            let cas_patch = match stop_reason.as_str() {
                "end_turn" => {
                    conversation_service::finish_end_turn_if_in_progress(
                        db_conn,
                        cid,
                        *mark_awaiting_reply,
                    )
                    .await?
                }
                "refusal" | "max_tokens" | "max_turn_requests" | "unknown" | "empty" => {
                    conversation_service::update_status_if_with_patch(
                        db_conn,
                        cid,
                        ConversationStatus::InProgress,
                        ConversationStatus::Cancelled,
                    )
                    .await?
                }
                // `cancelled` and any future reason: don't write here.
                _ => None,
            };
            if let Some(patch) = cas_patch {
                // Map from the CAS arm we just took (not from patch.status string)
                // so the per-connection event stays a typed ConversationStatus.
                let status = if stop_reason.as_str() == "end_turn" {
                    ConversationStatus::PendingReview
                } else {
                    ConversationStatus::Cancelled
                };
                emit_with_state(
                    &state_arc,
                    &emitter,
                    AcpEvent::ConversationStatusChanged {
                        conversation_id: cid,
                        status,
                    },
                )
                .await;
                crate::commands::conversations::emit_conversation_state(&emitter, patch);
            }
            Ok(())
        }
        // Other events don't need cross-connection DB persistence today; extend
        // this dispatcher with new arms as the lifecycle scope grows.
        _ => Ok(()),
    }
}

/// Whether a conversation row is a Codeg delegation child (has
/// `delegation_call_id`). Returns `Err` on DB failure so callers can fail
/// closed instead of treating the probe as "not a delegate".
async fn conversation_is_delegate(
    db_conn: &DatabaseConnection,
    conversation_id: i32,
) -> Result<bool, DbError> {
    let row = conversation_service::get_by_id(db_conn, conversation_id).await?;
    Ok(row.delegation_call_id.is_some())
}

/// Detach broker settlement from the lifecycle worker so a slow
/// `complete_call` cannot be cancelled by the 45s handle timeout (which
/// strands tasks in `settling` and blocks the parent Join forever).
fn spawn_forward_turn_complete_to_broker(
    db_conn: &DatabaseConnection,
    broker: Option<&Arc<DelegationBroker>>,
    conversation_id: i32,
    stop_reason: &str,
    last_text: String,
) {
    let Some(broker) = broker.map(Arc::clone) else {
        tracing::warn!(
            conversation_id,
            "[lifecycle] delegate TurnComplete but no broker installed — \
             child status will not settle"
        );
        return;
    };
    let db_conn = db_conn.clone();
    let stop_reason = stop_reason.to_string();
    tracing::info!(
        conversation_id,
        stop_reason = %stop_reason,
        "[lifecycle] spawning broker complete_call off lifecycle worker"
    );
    tokio::spawn(async move {
        forward_turn_complete_to_broker(
            &db_conn,
            broker.as_ref(),
            conversation_id,
            &stop_reason,
            Some(last_text),
        )
        .await;
    });
}

/// On TurnComplete for a delegation child, resolve the pending broker call
/// and let the broker drive the rest of the lifecycle (meta write, the
/// `AcpEvent::DelegationCompleted` emit against the parent stream, child
/// disconnect, tx.send). Keeping the emit responsibility inside
/// `broker.complete_call` is what guarantees the broker's other terminal
/// paths (`timeout` / `cancel_by_child_connection` / `cancel_by_parent`)
/// also surface the event — see
/// `.docs/issues/2026-05-24-delegation-termination-cascade.md`.
///
/// Prefer [`spawn_forward_turn_complete_to_broker`] from the lifecycle worker
/// so settlement is not cancelled by the worker handle timeout.
async fn forward_turn_complete_to_broker(
    db_conn: &DatabaseConnection,
    broker: &DelegationBroker,
    conversation_id: i32,
    stop_reason: &str,
    last_text: Option<String>,
) {
    let started = std::time::Instant::now();
    tracing::info!(
        conversation_id,
        stop_reason = %stop_reason,
        "[delegation][lifecycle] forward_turn_complete_to_broker begin"
    );
    let row = match conversation_service::get_by_id(db_conn, conversation_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                "[delegation][lifecycle] couldn't fetch child conversation \
                 {conversation_id} for outcome routing: {e}"
            );
            return;
        }
    };
    let call_id = match row.delegation_call_id.clone() {
        Some(id) => id,
        None => {
            tracing::warn!(
                conversation_id,
                "[delegation][lifecycle] no delegation_call_id; cannot complete_call"
            );
            return; // not a delegation child; nothing to do.
        }
    };
    if row.parent_tool_use_id.is_none() {
        tracing::info!(
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
    tracing::info!(
        conversation_id,
        call_id = %call_id,
        stop_reason = %stop_reason,
        "[delegation][lifecycle] invoking broker.complete_call"
    );
    broker.complete_call(&call_id, outcome).await;
    tracing::info!(
        conversation_id,
        call_id = %call_id,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "[delegation][lifecycle] broker.complete_call finished"
    );
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
        tracing::warn!(
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
    // Delegate rows: terminal ConversationStatus is owned by the broker store
    // CAS. Skipping the generic InProgress→Cancelled write prevents racing the
    // durable winner (completed/failed/canceled). Fail-closed on probe error.
    match conversation_is_delegate(db_conn, cid).await {
        Ok(true) => return Ok(()),
        Ok(false) => {}
        Err(e) => {
            tracing::error!(
                conversation_id = cid,
                error = %e,
                "[lifecycle] is_delegate probe failed on terminal event; \
                 skipping generic ConversationStatus write (fail-closed)"
            );
            return Ok(());
        }
    }
    let Some(patch) = conversation_service::update_status_if_with_patch(
        db_conn,
        cid,
        ConversationStatus::InProgress,
        ConversationStatus::Cancelled,
    )
    .await?
    else {
        return Ok(());
    };
    emit_with_state(
        &entry.state,
        &entry.emitter,
        AcpEvent::ConversationStatusChanged {
            conversation_id: cid,
            status: ConversationStatus::Cancelled,
        },
    )
    .await;
    crate::commands::conversations::emit_conversation_state(&entry.emitter, patch);
    Ok(())
}

/// On a non-TurnComplete terminal event (Disconnected / Error) for a
/// delegation child, surface a `canceled` outcome to the broker. The
/// child's DB row may already be marked `Cancelled` by `handle_terminal_event`
/// above; this separately wakes the parent's pending `delegate_to_agent`
/// tool_use_id. Match-by-`child_connection_id` is O(pending), bounded by
/// active delegations.
///
/// `terminal_error` is the formatted `AcpEvent::Error` detail (when the
/// caller arrived via `Error` rather than a bare `Disconnected`). It gets
/// stitched into the broker's canceled reason so the parent's
/// `delegate_to_agent` tool-call result surfaces the real failure cause.
async fn forward_disconnect_to_broker(
    broker: &DelegationBroker,
    connection_id: &str,
    terminal_error: Option<&str>,
) {
    broker
        .cancel_by_child_connection(connection_id, terminal_error)
        .await;
}

/// Build a single-line detail string from an `AcpEvent::Error` payload,
/// preferring the form `"[code] message"` when a stable code is present
/// (so the parent agent sees both the machine-readable bucket and the
/// human-readable text). Trims trailing whitespace; returns `message`
/// verbatim when no code is provided.
fn format_terminal_error(message: &str, code: Option<&str>) -> String {
    let trimmed = message.trim();
    match code {
        Some(c) if !c.trim().is_empty() => format!("[{c}] {trimmed}"),
        _ => trimmed.to_string(),
    }
}

/// Wrapper keys hosts use to nest the real tool arguments. JSON-RPC servers
/// and MCP relays pack the call as `{name, arguments}` or `{params: {...}}`;
/// some agents stash the args under a generic `input`/`payload` next to
/// `_meta`. Mirrors the frontend `ARGS_WRAPPER_KEYS` in
/// `delegated-sub-thread.tsx` so the two sides peel exactly the same shapes.
const ARGS_WRAPPER_KEYS: [&str; 5] = ["arguments", "input", "params", "payload", "_meta"];

/// Walk wrapper layers — and one level of double-encoded JSON-of-JSON — down to
/// the object that actually carries the `delegate_to_agent` arguments, and
/// return a clone of it. A node qualifies the moment it exposes any of
/// `task`/`agent_type`/`working_dir` as a string; otherwise we descend into the
/// known wrapper keys (depth-capped so pathological nesting can't loop).
///
/// Direct port of the frontend `findDelegationArgs` (`delegated-sub-thread.tsx`):
/// same wrapper keys, same depth-4 cap, same "first object with a delegation
/// field wins" rule. Keeping the walkers symmetric means a `raw_input` the card
/// can render into a task line is the same `raw_input` the broker can build a
/// correlation key from — so a host that wraps its ACP tool-call args (e.g.
/// Codex packs them under `params.input`; some relays double-encode the blob)
/// still gets a *keyed* pending entry instead of silently degrading to
/// FIFO/synthetic correlation, which is the exact failure the keyed-retention
/// fix exists to prevent.
fn find_delegation_args(
    value: &serde_json::Value,
    depth: u8,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if depth > 4 {
        return None;
    }
    // Double-encoded: some hosts ship `raw_input` as a JSON string whose
    // contents are themselves the arg blob. Parse one inner layer and recurse.
    if let Some(s) = value.as_str() {
        let inner: serde_json::Value = serde_json::from_str(s).ok()?;
        return find_delegation_args(&inner, depth + 1);
    }
    let obj = value.as_object()?;
    // Direct hit: this object declares a delegation field at its top level.
    if obj.get("task").and_then(|v| v.as_str()).is_some()
        || obj.get("agent_type").and_then(|v| v.as_str()).is_some()
        || obj.get("working_dir").and_then(|v| v.as_str()).is_some()
    {
        return Some(obj.clone());
    }
    // Otherwise peel a known wrapper layer.
    for key in ARGS_WRAPPER_KEYS {
        if let Some(child) = obj.get(key) {
            if let Some(found) = find_delegation_args(child, depth + 1) {
                return Some(found);
            }
        }
    }
    None
}

/// True when the ACP `tool_call` smells like an invocation of the
/// `delegate_to_agent` MCP tool. Defensive on both inputs because the host
/// agent gets to decide both fields:
///
/// * `title` is a free-form human-readable string the host composes. Some
///   hosts copy the MCP method verbatim (`mcp__codeg-mcp__delegate_to_agent`),
///   some prefix it with a verb (`Run mcp__…__delegate_to_agent`), some
///   rephrase it (`Delegate to codex`). We match by substring so any
///   form containing `delegate_to_agent` is captured.
/// * `raw_input` is the JSON arg blob the agent sent to the MCP server. The
///   `delegate_to_agent` schema requires `agent_type` AND `task`; presence
///   of both — after peeling any wrapper layers via [`find_delegation_args`] —
///   is a near-zero false-positive shape check that catches any host that
///   mangles the title beyond recognition, including ones that wrap their
///   tool-call args.
fn is_delegation_invocation(title: &str, raw_input: Option<&str>) -> bool {
    let normalized_title = title.to_ascii_lowercase().replace([' ', '-'], "_");
    if normalized_title.contains("delegate_to_agent") {
        return true;
    }
    if let Some(raw) = raw_input {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
            if let Some(args) = find_delegation_args(&v, 0) {
                let has_task = args.get("task").and_then(|t| t.as_str()).is_some();
                let has_agent_type = args.get("agent_type").and_then(|a| a.as_str()).is_some();
                if has_task && has_agent_type {
                    return true;
                }
            }
        }
    }
    false
}

/// Build the broker's `(agent_type, task, working_dir)` correlation key from
/// a `delegate_to_agent` tool_call's `raw_input` JSON. All three are values
/// the LLM passed identically to the ACP tool call and the MCP `tools/call`,
/// so the triple uniquely identifies the call even when several
/// `delegate_to_agent` invocations are in flight at once (and, unlike `task`
/// alone, doesn't collide when two parallel calls target different agents —
/// or different directories — with the same task text). `working_dir` is the
/// LLM's explicit value (`None` when omitted), matching the broker's
/// `DelegationRequest::requested_working_dir`. The args are located via
/// [`find_delegation_args`], so hosts that wrap or double-encode `raw_input`
/// are keyed identically to hosts that send the fields at the top level.
/// Returns `None` when `raw_input` is absent, not JSON, has no locatable
/// delegation object, or is missing/unparseable for `agent_type`/`task` — the
/// broker then falls back to FIFO ordering.
fn extract_delegation_match_key(raw_input: Option<&str>) -> Option<DelegationMatchKey> {
    let raw = raw_input?;
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let args = find_delegation_args(&parsed, 0)?;
    let task = args.get("task").and_then(|v| v.as_str())?.to_string();
    // Parse `agent_type` through the same serde path the MCP listener uses,
    // so the stored enum equals `DelegationRequest::agent_type`.
    let agent_type: AgentType = serde_json::from_value(args.get("agent_type")?.clone()).ok()?;
    let working_dir = args
        .get("working_dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(DelegationMatchKey {
        agent_type,
        task,
        working_dir,
    })
}

/// True when an ACP `ToolCallUpdate.status` string is terminal for delegation
/// correlation. The live value is `format!("{:?}", ToolCallStatus).to_lowercase()`
/// over the `agent-client-protocol-schema` enum (variants `Pending`,
/// `InProgress`, `Completed`, `Failed`), so terminal == `completed` | `failed`.
/// Cancellation never arrives via this field — it flows through the turn-cancel
/// / teardown path, which already drains pending entries on the broker. The
/// enum is `#[non_exhaustive]`; if a `Cancelled` variant is added upstream,
/// extend this set alongside `acp::connection`'s status mapping.
fn is_terminal_tool_call_status(status: Option<&str>) -> bool {
    matches!(status, Some("completed" | "failed"))
}

/// Synchronously register a parent-side `delegate_to_agent` tool_call_id with
/// the broker, straight off the in-process bus — i.e. NOT via the
/// per-connection worker.
///
/// Called from the dispatcher loop for BOTH `ToolCall` and `ToolCallUpdate`
/// so correlation is robust against the two failure modes that orphaned the
/// second of two parallel delegations to a synthetic id (dead "view session"
/// + stuck "sub-agent running…"):
///
/// 1. **Args arriving late.** Some hosts emit an arg-less initial `ToolCall`
///    (a model-generated `title` that doesn't contain `delegate_to_agent`,
///    `raw_input` still empty) and only ship the `agent_type`/`task` arguments
///    on a following `ToolCallUpdate`. The old code registered solely from the
///    initial `ToolCall` and filtered `ToolCallUpdate` out entirely, so such a
///    call was never registered and its MCP round-trip fell back to a
///    synthetic `delegation-<uuid>`. Handling both variants here registers (or
///    backfills the key onto) the id whenever the args first appear.
/// 2. **Bus lag / worker stall.** Registration used to run inside the
///    DB-coupled per-connection worker. Under the load two parallel children
///    create (each streaming many `ToolCall`s), a worker stalling on a SQLite
///    retry could fill its mailbox, block the dispatcher's `send().await`, and
///    let the broadcast bus lag — dropping the parent's *second* `tool_call`
///    before it was ever registered. Registering here, before the
///    `is_lifecycle_relevant` filter and any worker send, removes that
///    dependency; and because `ToolCall` is no longer forwarded to workers at
///    all, the very mailbox pressure that caused the lag is gone too.
///
/// Cheap on the hot path: the discriminant match plus `is_delegation_invocation`
/// (a substring test on `title`, and a JSON parse only when `raw_input` is
/// present) fast-rejects the high-frequency non-delegation `ToolCallUpdate`
/// flood — those carry streaming `raw_output`, not `raw_input`. The broker's
/// own two-tier dedupe absorbs the repeated registrations a multi-update
/// delegation call produces.
///
/// A TERMINAL tool-call event (status `completed`/`failed`, via EITHER
/// `ToolCall` or `ToolCallUpdate` — some hosts ship status flips on the
/// non-update variant, see `register_pending_tool_call`'s dedupe doc) is handled
/// the opposite way: instead of registering, it tombstones any still-pending
/// entry for that `tool_call_id` via
/// [`DelegationBroker::tombstone_pending_tool_call`], so a `delegate_to_agent`
/// that went terminal without its MCP round-trip ever arriving can't leave a
/// stale keyed entry for a later same-key delegation to mis-claim.
async fn register_delegation_tool_call_from_event(
    broker: &DelegationBroker,
    envelope: &EventEnvelope,
) {
    // Terminal tool-call event (completed/failed) → tombstone by id, don't
    // register. Read BOTH variants, symmetric with the registration path below:
    // some hosts ship status flips on the non-update `ToolCall` variant, not
    // only `ToolCallUpdate` (`register_pending_tool_call`'s dedupe doc). Keyed on
    // `tool_call_id` membership rather than `is_delegation_invocation`: a bare
    // terminal update may carry `title: None` / `raw_input: None`, leaving no
    // derivable key, so we let the broker no-op when the id isn't a pending
    // delegation. This removes a STALE keyed entry (the call failed / the turn
    // was interrupted / its round-trip never reached the broker) so a later
    // identical (agent_type, task, working_dir) call can't claim its dead id and
    // bind to the wrong card.
    let terminal: Option<(&String, &str)> = match &envelope.payload {
        AcpEvent::ToolCall {
            tool_call_id,
            status,
            ..
        } if is_terminal_tool_call_status(Some(status)) => Some((tool_call_id, status.as_str())),
        AcpEvent::ToolCallUpdate {
            tool_call_id,
            status,
            ..
        } if is_terminal_tool_call_status(status.as_deref()) => {
            Some((tool_call_id, status.as_deref().unwrap_or("")))
        }
        _ => None,
    };
    if let Some((tool_call_id, status)) = terminal {
        let removed = broker
            .tombstone_pending_tool_call(&envelope.connection_id, tool_call_id)
            .await;
        if removed {
            tracing::info!(
                "[delegation] tombstoned stale parent tool_call_id={tool_call_id} on conn={} (terminal status={status})",
                envelope.connection_id
            );
        }
        return;
    }

    let (tool_call_id, title, raw_input): (&String, &str, Option<&str>) = match &envelope.payload {
        AcpEvent::ToolCall {
            tool_call_id,
            title,
            raw_input,
            ..
        } => (tool_call_id, title.as_str(), raw_input.as_deref()),
        AcpEvent::ToolCallUpdate {
            tool_call_id,
            title,
            raw_input,
            ..
        } => (
            tool_call_id,
            title.as_deref().unwrap_or(""),
            raw_input.as_deref(),
        ),
        _ => return,
    };
    if !is_delegation_invocation(title, raw_input) {
        return;
    }
    let match_key = extract_delegation_match_key(raw_input);
    tracing::info!(
        "[delegation] registering parent tool_call_id={tool_call_id} on conn={} (keyed={})",
        envelope.connection_id,
        match_key.is_some()
    );
    broker
        .register_pending_tool_call_with_key(
            &envelope.connection_id,
            tool_call_id.clone(),
            match_key,
        )
        .await;
}

#[cfg(test)]
mod delegation_title_tests {
    use super::{extract_delegation_match_key, is_delegation_invocation};
    use crate::models::AgentType;

    #[test]
    fn extract_match_key_pulls_agent_task_and_dir() {
        let raw = r#"{"agent_type":"codex","task":"smoke test","working_dir":"/tmp"}"#;
        let key = extract_delegation_match_key(Some(raw)).expect("key parses");
        assert_eq!(key.agent_type, AgentType::Codex);
        assert_eq!(key.task, "smoke test");
        assert_eq!(key.working_dir.as_deref(), Some("/tmp"));
    }

    #[test]
    fn extract_match_key_working_dir_none_when_omitted() {
        // The common case: the LLM omits working_dir, so the key's working_dir
        // is None — symmetric with the MCP side, where the listener records
        // `requested_working_dir = None` before defaulting it for the spawn.
        let raw = r#"{"agent_type":"codex","task":"smoke test"}"#;
        let key = extract_delegation_match_key(Some(raw)).expect("key parses");
        assert!(key.working_dir.is_none());
    }

    #[test]
    fn extract_match_key_none_when_field_missing_or_unparseable() {
        // Missing task.
        assert!(extract_delegation_match_key(Some(r#"{"agent_type":"codex"}"#)).is_none());
        // Missing agent_type.
        assert!(extract_delegation_match_key(Some(r#"{"task":"x"}"#)).is_none());
        // Unknown agent_type doesn't deserialize to AgentType.
        assert!(
            extract_delegation_match_key(Some(r#"{"agent_type":"garbage","task":"x"}"#)).is_none()
        );
        // Not JSON / absent.
        assert!(extract_delegation_match_key(Some("not json")).is_none());
        assert!(extract_delegation_match_key(None).is_none());
    }

    #[test]
    fn extract_match_key_peels_wrapper_layers() {
        // Codex-style: args nested under `params.input` (mirrors the
        // `findDelegationArgs` walker in delegated-sub-thread.tsx).
        let nested = r#"{"params":{"input":{"agent_type":"codex","task":"t","working_dir":"/w"}}}"#;
        let key = extract_delegation_match_key(Some(nested)).expect("nested key parses");
        assert_eq!(key.agent_type, AgentType::Codex);
        assert_eq!(key.task, "t");
        assert_eq!(key.working_dir.as_deref(), Some("/w"));

        // JSON-RPC `{name, arguments}` envelope.
        let wrapped =
            r#"{"name":"delegate_to_agent","arguments":{"agent_type":"codex","task":"t2"}}"#;
        let key = extract_delegation_match_key(Some(wrapped)).expect("wrapped key parses");
        assert_eq!(key.task, "t2");
        assert!(key.working_dir.is_none());

        // Top-level args alongside a sibling `_meta` block (claude-agent-acp):
        // the direct hit fires at the top level, so `_meta` is never descended.
        let with_meta = r#"{"_meta":{"trace":"abc"},"agent_type":"codex","task":"t3"}"#;
        let key = extract_delegation_match_key(Some(with_meta)).expect("meta key parses");
        assert_eq!(key.task, "t3");
    }

    #[test]
    fn extract_match_key_peels_double_encoded_json() {
        // Some relays ship `raw_input` as a JSON string whose contents are the
        // arg blob (JSON-of-JSON). The walker parses one inner layer.
        let inner = r#"{"agent_type":"codex","task":"double"}"#;
        let double = serde_json::Value::String(inner.to_string()).to_string();
        let key = extract_delegation_match_key(Some(&double)).expect("double-encoded parses");
        assert_eq!(key.agent_type, AgentType::Codex);
        assert_eq!(key.task, "double");
    }

    #[test]
    fn extract_match_key_none_when_nesting_exceeds_cap() {
        // Wrapping deeper than the depth cap degrades to None (FIFO fallback)
        // rather than panicking or looping. Five `params` layers push the args
        // to depth 5, one past the cap.
        let deep = r#"{"params":{"params":{"params":{"params":{"params":{"agent_type":"codex","task":"deep"}}}}}}"#;
        assert!(extract_delegation_match_key(Some(deep)).is_none());
    }

    #[test]
    fn matches_bare_method_in_title() {
        assert!(is_delegation_invocation("delegate_to_agent", None));
        assert!(is_delegation_invocation("Delegate To Agent", None));
        assert!(is_delegation_invocation("delegate-to-agent", None));
    }

    #[test]
    fn matches_mcp_prefixed_method_in_title() {
        assert!(is_delegation_invocation(
            "mcp__codeg-mcp__delegate_to_agent",
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
    fn matches_via_wrapped_raw_input_shape() {
        // A host that BOTH mangles the title AND wraps the args is still
        // recognized via the wrapper-aware shape check (otherwise it would be
        // missed entirely, not just left unkeyed).
        let wrapped = r#"{"params":{"input":{"agent_type":"codex","task":"t"}}}"#;
        assert!(is_delegation_invocation("some custom verb", Some(wrapped)));
        // Double-encoded args are recognized too.
        let inner = r#"{"agent_type":"codex","task":"t"}"#;
        let double = serde_json::Value::String(inner.to_string()).to_string();
        assert!(is_delegation_invocation("custom", Some(&double)));
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

    #[test]
    fn terminal_status_set_is_completed_and_failed_only() {
        use super::is_terminal_tool_call_status as is_terminal;
        assert!(is_terminal(Some("completed")));
        assert!(is_terminal(Some("failed")));
        assert!(!is_terminal(Some("pending")));
        assert!(!is_terminal(Some("in_progress")));
        assert!(!is_terminal(None));
        // Cancellation never arrives via this field (it flows through the
        // turn-cancel path), so it must not be treated as terminal here.
        assert!(!is_terminal(Some("canceled")));
        assert!(!is_terminal(Some("cancelled")));
    }
}

#[cfg(test)]
mod delegation_registration_tests {
    //! Covers `register_delegation_tool_call_from_event` — the dispatcher-side
    //! correlation capture that replaced the worker's `ToolCall` arm. These
    //! exercise the two cases that orphaned a parallel delegation to a
    //! synthetic id before the move: args arriving on a `ToolCallUpdate`, and
    //! a key backfilled by a later update.

    use super::register_delegation_tool_call_from_event;
    use crate::acp::delegation::broker::{
        ConversationDepthLookup, DelegationBroker, DelegationMatchKey,
    };
    use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
    use crate::acp::delegation::types::DelegationError;
    use crate::acp::types::{AcpEvent, EventEnvelope};
    use crate::models::AgentType;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct RootDepth;
    #[async_trait]
    impl ConversationDepthLookup for RootDepth {
        async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(None)
        }
    }

    fn broker() -> DelegationBroker {
        DelegationBroker::new(
            Arc::new(MockSpawner::new()) as Arc<dyn ConnectionSpawner>,
            Arc::new(RootDepth) as Arc<dyn ConversationDepthLookup>,
        )
    }

    fn tool_call_event(tool_call_id: &str, title: &str, raw_input: Option<&str>) -> EventEnvelope {
        EventEnvelope {
            seq: 1,
            connection_id: "parent-conn".into(),
            payload: AcpEvent::ToolCall {
                tool_call_id: tool_call_id.into(),
                title: title.into(),
                kind: "other".into(),
                status: "pending".into(),
                content: None,
                raw_input: raw_input.map(|s| s.to_string()),
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            },
        }
    }

    fn tool_call_update_event(
        tool_call_id: &str,
        title: Option<&str>,
        raw_input: Option<&str>,
    ) -> EventEnvelope {
        EventEnvelope {
            seq: 2,
            connection_id: "parent-conn".into(),
            payload: AcpEvent::ToolCallUpdate {
                tool_call_id: tool_call_id.into(),
                title: title.map(|s| s.to_string()),
                status: None,
                content: None,
                raw_input: raw_input.map(|s| s.to_string()),
                raw_output: None,
                raw_output_append: None,
                locations: None,
                meta: None,
                images: None,
            },
        }
    }

    fn codex_key(task: &str) -> DelegationMatchKey {
        DelegationMatchKey {
            agent_type: AgentType::Codex,
            task: task.to_string(),
            working_dir: None,
        }
    }

    /// `tool_call_update_event` with an explicit `status` (the base helper
    /// hardcodes `None`). Used to drive the terminal-tombstone branch.
    fn tool_call_update_event_with_status(
        tool_call_id: &str,
        status: Option<&str>,
        raw_input: Option<&str>,
    ) -> EventEnvelope {
        EventEnvelope {
            seq: 2,
            connection_id: "parent-conn".into(),
            payload: AcpEvent::ToolCallUpdate {
                tool_call_id: tool_call_id.into(),
                title: None,
                status: status.map(|s| s.to_string()),
                content: None,
                raw_input: raw_input.map(|s| s.to_string()),
                raw_output: None,
                raw_output_append: None,
                locations: None,
                meta: None,
                images: None,
            },
        }
    }

    /// `tool_call_event` with an explicit `status` (the base helper hardcodes
    /// `"pending"`). Some hosts ship terminal status flips on the non-update
    /// `ToolCall` variant, so the tombstone branch must read it too.
    fn tool_call_event_with_status(
        tool_call_id: &str,
        title: &str,
        status: &str,
        raw_input: Option<&str>,
    ) -> EventEnvelope {
        EventEnvelope {
            seq: 1,
            connection_id: "parent-conn".into(),
            payload: AcpEvent::ToolCall {
                tool_call_id: tool_call_id.into(),
                title: title.into(),
                kind: "other".into(),
                status: status.into(),
                content: None,
                raw_input: raw_input.map(|s| s.to_string()),
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            },
        }
    }

    /// A terminal `ToolCallUpdate` (completed) for a registered delegation
    /// tombstones its keyed entry, so a `delegate_to_agent` that went terminal
    /// without its round-trip ever arriving leaves nothing for a later same-key
    /// delegation to mis-claim.
    #[tokio::test]
    async fn terminal_update_tombstones_registered_delegation() {
        let b = broker();
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event(
                "tc-1",
                "delegate_to_agent",
                Some(r#"{"agent_type":"codex","task":"research"}"#),
            ),
        )
        .await;
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_update_event_with_status("tc-1", Some("completed"), None),
        )
        .await;
        assert!(
            b.take_matching_tool_call("parent-conn", &codex_key("research"))
                .await
                .is_none(),
            "a terminal ToolCallUpdate must tombstone the stale keyed entry"
        );
    }

    /// A NON-terminal update must NOT tombstone: this is the serialized
    /// round-trip case (Claude Code runs parallel `delegate_to_agent` calls
    /// one-at-a-time, so the 2nd entry waits `in_progress` for up to ~77s before
    /// its round-trip fires). Evicting it here would reintroduce the dead-card
    /// bug the keyed-retention rule was added to fix.
    #[tokio::test]
    async fn non_terminal_update_does_not_tombstone() {
        let b = broker();
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event(
                "tc-late",
                "delegate_to_agent",
                Some(r#"{"agent_type":"codex","task":"slow"}"#),
            ),
        )
        .await;
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_update_event_with_status("tc-late", Some("in_progress"), None),
        )
        .await;
        assert_eq!(
            b.take_matching_tool_call("parent-conn", &codex_key("slow"))
                .await
                .as_deref(),
            Some("tc-late"),
            "a non-terminal update must leave the waiting entry claimable"
        );
    }

    /// A terminal update for an unrelated (non-delegation) tool call no-ops and
    /// leaves a registered delegation intact — the tombstone runs for every
    /// terminal update but only removes a matching pending delegation id.
    #[tokio::test]
    async fn terminal_update_for_unrelated_tool_is_harmless() {
        let b = broker();
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event(
                "tc-deleg",
                "delegate_to_agent",
                Some(r#"{"agent_type":"codex","task":"research"}"#),
            ),
        )
        .await;
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_update_event_with_status("tc-bash-42", Some("completed"), None),
        )
        .await;
        assert_eq!(
            b.take_matching_tool_call("parent-conn", &codex_key("research"))
                .await
                .as_deref(),
            Some("tc-deleg"),
            "a terminal update for an unrelated tool must leave the delegation intact"
        );
    }

    /// A terminal status shipped via the non-update `ToolCall` variant (some
    /// hosts use it for status flips — see `register_pending_tool_call`'s dedupe
    /// doc) tombstones too, symmetric with the `ToolCallUpdate` path. Without
    /// this, a terminal `ToolCall` still carrying the delegation shape would
    /// RE-REGISTER the stale entry instead of removing it. Uses `failed` to also
    /// drive that terminal value through the dispatcher.
    #[tokio::test]
    async fn terminal_tool_call_variant_tombstones_registered_delegation() {
        let b = broker();
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event(
                "tc-1",
                "delegate_to_agent",
                Some(r#"{"agent_type":"codex","task":"research"}"#),
            ),
        )
        .await;
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event_with_status(
                "tc-1",
                "delegate_to_agent",
                "failed",
                Some(r#"{"agent_type":"codex","task":"research"}"#),
            ),
        )
        .await;
        assert!(
            b.take_matching_tool_call("parent-conn", &codex_key("research"))
                .await
                .is_none(),
            "a terminal ToolCall (status flip via the non-update variant) must tombstone"
        );
    }

    /// A terminal `ToolCall` for an id with no pending entry must NOT register a
    /// fresh one — it short-circuits at the terminal branch before the register
    /// path, so it can't itself create the stale entry it exists to prevent.
    #[tokio::test]
    async fn terminal_tool_call_does_not_register_fresh_entry() {
        let b = broker();
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event_with_status(
                "tc-1",
                "delegate_to_agent",
                "completed",
                Some(r#"{"agent_type":"codex","task":"research"}"#),
            ),
        )
        .await;
        assert!(
            b.take_matching_tool_call("parent-conn", &codex_key("research"))
                .await
                .is_none(),
            "a terminal ToolCall with no prior registration must not create an entry"
        );
    }

    /// The headline regression: a delegation whose `agent_type`/`task` arrive
    /// on a `ToolCallUpdate` (the initial `ToolCall` had a model-generated
    /// title and no `raw_input`) is still registered, keyed, and claimable by
    /// its MCP round-trip. The old `ToolCall`-only path never saw the args, so
    /// this call fell back to a synthetic id → dead "view session".
    #[tokio::test]
    async fn registers_delegation_from_tool_call_update() {
        let b = broker();
        // Arg-less initial ToolCall with a descriptive title → not yet a
        // recognizable delegation, nothing registered.
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event("tc-1", "Delegating research to codex", None),
        )
        .await;
        assert!(
            b.take_matching_tool_call("parent-conn", &codex_key("research"))
                .await
                .is_none(),
            "arg-less descriptive ToolCall must not register"
        );
        // Args land on the following update → now registered with its key.
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_update_event(
                "tc-1",
                Some("Delegating research to codex"),
                Some(r#"{"agent_type":"codex","task":"research"}"#),
            ),
        )
        .await;
        assert_eq!(
            b.take_matching_tool_call("parent-conn", &codex_key("research"))
                .await
                .as_deref(),
            Some("tc-1"),
            "delegation args arriving via ToolCallUpdate must register the id"
        );
    }

    /// An initial `ToolCall` whose title names the tool but carries no args
    /// registers UNKEYED; a later `ToolCallUpdate` with the args must backfill
    /// the key. The in-loop claim binds ONLY by exact key match (unkeyed entries
    /// are never claimed there), so `tc-2` becomes claimable purely because the
    /// backfill landed its key — shown here alongside a parallel keyed sibling
    /// it must not be mixed up with.
    #[tokio::test]
    async fn update_backfills_key_onto_unkeyed_tool_call() {
        let b = broker();
        // tc-2 registers unkeyed (tool-name title, no raw_input yet).
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event("tc-2", "mcp__codeg-mcp__delegate_to_agent", None),
        )
        .await;
        // A parallel keyed sibling sharing the queue (must not be mixed up).
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event(
                "tc-sibling",
                "mcp__codeg-mcp__delegate_to_agent",
                Some(r#"{"agent_type":"codex","task":"sibling"}"#),
            ),
        )
        .await;
        // tc-2's args arrive on an update → backfills its key.
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_update_event(
                "tc-2",
                None,
                Some(r#"{"agent_type":"codex","task":"build"}"#),
            ),
        )
        .await;
        // In-loop claims are exact-match-only, so tc-2 is claimable purely
        // because the backfill landed its key (never via arrival-order FIFO).
        assert_eq!(
            b.take_matching_tool_call("parent-conn", &codex_key("build"))
                .await
                .as_deref(),
            Some("tc-2"),
            "ToolCallUpdate must backfill the key onto the unkeyed entry"
        );
        assert_eq!(
            b.take_matching_tool_call("parent-conn", &codex_key("sibling"))
                .await
                .as_deref(),
            Some("tc-sibling")
        );
    }

    /// The high-frequency non-delegation tool stream (bash/read/write and
    /// their `raw_output` update floods) must never register anything — that's
    /// what keeps the dispatcher-side check cheap and the pending queue clean.
    #[tokio::test]
    async fn ignores_non_delegation_tool_events() {
        let b = broker();
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_event("tc-3", "bash", Some(r#"{"command":"ls"}"#)),
        )
        .await;
        register_delegation_tool_call_from_event(
            &b,
            &tool_call_update_event("tc-3", Some("bash"), None),
        )
        .await;
        assert!(
            b.take_pending_tool_call("parent-conn").await.is_none(),
            "non-delegation tool events must not register"
        );
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
    mut rx: mpsc::Receiver<Arc<InternalEventEnvelope>>,
) {
    // 1-entry HashMap so we can reuse `handle_terminal_event` (also keeps the
    // existing test surface intact — tests still drive a `&mut HashMap`).
    let mut cache: HashMap<String, CachedConn> = HashMap::new();
    // True once we've already invoked `handle_terminal_event` +
    // `forward_disconnect_to_broker` for this connection. Terminal `Error`
    // and `Disconnected` ARE both expected on the genuine teardown path
    // (`connection.rs:493` → `run_connection` unwind → `Disconnected`), and
    // either one alone is also valid: a `Disconnected` without preceding
    // Error fires for clean transport close, and a terminal Error in
    // theory could be the last event if the bus drops the trailing
    // Disconnected (broadcast `Lagged`). Whichever lands first dispatches
    // the terminal work; the second one is a no-op so the broker / DB
    // aren't double-touched.
    let mut terminal_dispatched = false;
    while let Some(envelope_arc) = rx.recv().await {
        let internal: &InternalEventEnvelope = envelope_arc.as_ref();
        match &internal.payload {
            AcpEvent::ConversationLinked {
                conversation_id, ..
            } => {
                try_cache_link(&mut cache, &manager, &connection_id, *conversation_id).await;
            }
            AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected,
            } => {
                if terminal_dispatched {
                    continue;
                }
                if let Err(e) = handle_terminal_event(&db, &mut cache, &connection_id).await {
                    tracing::error!("[lifecycle][ERROR] terminal event for {connection_id}: {e}");
                }
                if let Some(b) = broker.as_ref() {
                    forward_disconnect_to_broker(b.as_ref(), &connection_id, None).await;
                }
                terminal_dispatched = true;
            }
            AcpEvent::Error {
                message,
                code,
                terminal,
                ..
            } => {
                // Non-terminal Errors (`turn_failure_error_event`,
                // `session/load` fallback, empty-prompt rejection, SetMode
                // / SetConfigOption failures) leave the connection alive:
                // - flipping the row InProgress → Cancelled would briefly
                //   show "Cancelled" in the UI before the next TurnComplete
                //   corrects it (cosmetic but jumpy).
                // - draining the broker would race-cancel a pending
                //   delegation that the upcoming `TurnComplete` →
                //   `complete_call` would have mapped to a proper child-side
                //   error code (`ChildRefusal` / `ChildMaxTokens` / …).
                //
                // F2 in the v0.14.3 sub-agent delegation post-mortem.
                if !*terminal {
                    continue;
                }
                if terminal_dispatched {
                    continue;
                }
                // Genuinely terminal (the `run_connection` failure path at
                // `connection.rs:493`). Drain the broker NOW with the error
                // detail instead of waiting for the trailing `Disconnected`.
                // If `Disconnected` never arrives (bus `Lagged`, task
                // abort, a future emit site that forgets to follow up) the
                // parent's `delegate_to_agent` would otherwise block on
                // `rx.await` forever. The drain itself is idempotent
                // (`cancel_by_child_connection` no-ops on empty pending),
                // so the subsequent Disconnected will short-circuit on
                // `terminal_dispatched`.
                if let Err(e) = handle_terminal_event(&db, &mut cache, &connection_id).await {
                    tracing::error!("[lifecycle][ERROR] terminal event for {connection_id}: {e}");
                }
                if let Some(b) = broker.as_ref() {
                    let detail = format_terminal_error(message, code.as_deref());
                    forward_disconnect_to_broker(b.as_ref(), &connection_id, Some(&detail)).await;
                }
                terminal_dispatched = true;
            }
            _ => {
                // Bound worker time so a stuck DB/broker call cannot freeze
                // the per-connection mailbox forever (which used to back-pressure
                // the dispatcher into broadcast Lagged and drop later TurnCompletes).
                //
                // TurnComplete is special: status CAS is correctness-critical.
                // On timeout we retry (at-least-once processing) rather than
                // silently moving on and leaving the row at `in_progress`.
                // Non-TurnComplete events yield after one timeout so a later
                // TurnComplete in the same mailbox can still run.
                const WORKER_HANDLE_TIMEOUT: Duration = Duration::from_secs(45);
                const TURN_COMPLETE_MAX_ATTEMPTS: u32 = 5;
                let is_turn_complete = matches!(internal.payload, AcpEvent::TurnComplete { .. });
                let max_attempts = if is_turn_complete {
                    TURN_COMPLETE_MAX_ATTEMPTS
                } else {
                    1
                };
                let mut attempt = 0u32;
                loop {
                    attempt += 1;
                    match tokio::time::timeout(
                        WORKER_HANDLE_TIMEOUT,
                        handle_internal_event_with_retry(&db, &manager, internal, broker.as_ref()),
                    )
                    .await
                    {
                        Ok(()) => break,
                        Err(_) => {
                            if attempt < max_attempts {
                                tracing::error!(
                                    connection_id = %connection_id,
                                    event = %lifecycle_payload_kind(&internal.payload),
                                    timeout_secs = 45,
                                    attempt,
                                    max_attempts,
                                    "[lifecycle][ERROR] worker handle_event timed out — \
                                     retrying TurnComplete (at-least-once status CAS)"
                                );
                                continue;
                            }
                            tracing::error!(
                                connection_id = %connection_id,
                                event = %lifecycle_payload_kind(&internal.payload),
                                timeout_secs = 45,
                                attempts = attempt,
                                is_turn_complete,
                                "[lifecycle][ERROR] worker handle_event timed out after \
                                 all attempts — moving on so later events can drain \
                                 (TurnComplete may leave row in_progress if this was CAS)"
                            );
                            break;
                        }
                    }
                }
            }
        }
    }
}

/// Label for logs (stable, short).
fn lifecycle_payload_kind(payload: &AcpEvent) -> String {
    match payload {
        AcpEvent::TurnComplete { stop_reason, .. } => {
            format!("TurnComplete({stop_reason})")
        }
        AcpEvent::SessionStarted { .. } => "SessionStarted".into(),
        AcpEvent::ConversationLinked { .. } => "ConversationLinked".into(),
        AcpEvent::StatusChanged { status, .. } => {
            format!("StatusChanged({status:?})")
        }
        AcpEvent::Error { terminal, .. } => format!("Error(terminal={terminal})"),
        other => format!("{other:?}")
            .split_whitespace()
            .next()
            .unwrap_or("Other")
            .to_string(),
    }
}

/// Spawn a per-connection lifecycle worker and return its mailbox sender.
fn spawn_lifecycle_worker(
    conn_id: &str,
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    broker: &Option<Arc<DelegationBroker>>,
) -> mpsc::Sender<Arc<InternalEventEnvelope>> {
    let (tx, worker_rx) = mpsc::channel::<Arc<InternalEventEnvelope>>(WORKER_QUEUE_CAPACITY);
    let db_clone = db_conn.clone();
    let mgr_clone = manager.clone_ref();
    let broker_clone = broker.clone();
    let id_clone = conn_id.to_string();
    tokio::spawn(connection_worker_loop(
        id_clone,
        db_clone,
        mgr_clone,
        broker_clone,
        worker_rx,
    ));
    tx
}

/// Enqueue a lifecycle-relevant envelope onto the per-connection worker.
///
/// **Never awaits** a full mailbox. Blocking the dispatcher here was the
/// production failure mode behind stuck `in_progress` rows: one stalled
/// worker filled its queue, `send().await` froze the bus consumer, the
/// broadcast lagged, and a later `TurnComplete` was dropped before enqueue
/// (Codex #385: 19 emits, 18 CAS — last turn had `TurnComplete emitted` but
/// no `enqueue`).
///
/// Full mailbox → spawn an overflow deliverer with a timeout. Closed →
/// respawn worker and retry (or overflow).
fn enqueue_lifecycle_envelope(
    workers: &mut HashMap<String, mpsc::Sender<Arc<InternalEventEnvelope>>>,
    db_conn: &DatabaseConnection,
    manager: &ConnectionManager,
    broker: &Option<Arc<DelegationBroker>>,
    metrics: &EventBusMetrics,
    envelope_arc: Arc<InternalEventEnvelope>,
    source: &'static str,
) {
    let conn_id = envelope_arc.connection_id.clone();
    let is_terminal = is_dispatcher_terminal(&envelope_arc.payload);
    let payload_kind = lifecycle_payload_kind(&envelope_arc.payload);

    if workers.get(&conn_id).is_some_and(|tx| tx.is_closed()) {
        tracing::warn!(
            connection_id = %conn_id,
            event = %payload_kind,
            source,
            "[lifecycle][WARN] worker channel closed; respawning before enqueue \
             (prior worker likely panicked)"
        );
        workers.remove(&conn_id);
    }

    let tx = workers
        .entry(conn_id.clone())
        .or_insert_with(|| spawn_lifecycle_worker(&conn_id, db_conn, manager, broker));

    if matches!(envelope_arc.payload, AcpEvent::TurnComplete { .. }) {
        tracing::info!(
            connection_id = %conn_id,
            event = %payload_kind,
            source,
            has_completion_sidecar = envelope_arc.completion.is_some(),
            "[lifecycle] enqueue TurnComplete to worker"
        );
    }

    const OVERFLOW_SEND_TIMEOUT: Duration = Duration::from_secs(60);

    let overflow = |tx: mpsc::Sender<Arc<InternalEventEnvelope>>,
                    env: Arc<InternalEventEnvelope>,
                    conn_id: String,
                    payload_kind: String,
                    source: &'static str,
                    why: &'static str| {
        // Critical payloads (TurnComplete etc.) must not be cancelled by a
        // timeout: `timeout` dropping a pending `send` discards the message.
        // Wait indefinitely for capacity. Non-critical still use a 60s cap.
        let critical = is_lifecycle_critical(&env.payload);
        tokio::spawn(async move {
            if critical {
                tracing::warn!(
                    connection_id = %conn_id,
                    event = %payload_kind,
                    source,
                    why,
                    "[lifecycle][WARN] overflow deliver waiting for worker mailbox \
                     (critical — no drop timeout)"
                );
                match tx.send(env).await {
                    Ok(()) => {
                        tracing::info!(
                            connection_id = %conn_id,
                            event = %payload_kind,
                            source,
                            "[lifecycle] overflow deliver succeeded (critical)"
                        );
                    }
                    Err(_) => {
                        tracing::error!(
                            connection_id = %conn_id,
                            event = %payload_kind,
                            source,
                            "[lifecycle][ERROR] DROPPED critical lifecycle event — \
                             worker channel closed during overflow deliver"
                        );
                    }
                }
                return;
            }
            tracing::warn!(
                connection_id = %conn_id,
                event = %payload_kind,
                source,
                why,
                timeout_secs = 60,
                "[lifecycle][WARN] overflow deliver waiting for worker mailbox"
            );
            match tokio::time::timeout(OVERFLOW_SEND_TIMEOUT, tx.send(env)).await {
                Ok(Ok(())) => {
                    tracing::info!(
                        connection_id = %conn_id,
                        event = %payload_kind,
                        source,
                        "[lifecycle] overflow deliver succeeded"
                    );
                }
                Ok(Err(_)) => {
                    tracing::error!(
                        connection_id = %conn_id,
                        event = %payload_kind,
                        source,
                        "[lifecycle][ERROR] DROPPED lifecycle event — worker \
                         channel closed during overflow deliver"
                    );
                }
                Err(_) => {
                    tracing::error!(
                        connection_id = %conn_id,
                        event = %payload_kind,
                        source,
                        "[lifecycle][ERROR] DROPPED non-critical lifecycle event — \
                         worker stuck >60s (mailbox never drained)"
                    );
                }
            }
        });
    };

    match tx.try_send(envelope_arc) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(env)) => {
            metrics
                .worker_queue_full_count
                .fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                connection_id = %conn_id,
                event = %payload_kind,
                source,
                "[lifecycle][WARN] worker queue full — spawning overflow \
                 deliverer (dispatcher stays unblocked)"
            );
            overflow(
                tx.clone(),
                env,
                conn_id.clone(),
                payload_kind.clone(),
                source,
                "queue_full",
            );
        }
        Err(mpsc::error::TrySendError::Closed(env)) => {
            tracing::error!(
                connection_id = %conn_id,
                event = %payload_kind,
                source,
                "[lifecycle][ERROR] worker channel closed mid-send; respawning \
                 and re-enqueueing"
            );
            workers.remove(&conn_id);
            let tx2 = workers
                .entry(conn_id.clone())
                .or_insert_with(|| spawn_lifecycle_worker(&conn_id, db_conn, manager, broker));
            match tx2.try_send(env) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(env2))
                | Err(mpsc::error::TrySendError::Closed(env2)) => {
                    overflow(
                        tx2.clone(),
                        env2,
                        conn_id.clone(),
                        payload_kind.clone(),
                        source,
                        "respawn_retry",
                    );
                }
            }
        }
    }

    if is_terminal {
        // Drop the sender; worker drains the queue then exits. Overflow
        // tasks hold their own Sender clone so they can still complete.
        workers.remove(&conn_id);
    }
}

/// Capacity of the off-select broker tool-effects mailbox. ToolCall floods
/// can be large; the worker serializes register/project so ordering is
/// preserved without ever parking the lifecycle dispatcher.
const BROKER_TOOL_QUEUE_CAPACITY: usize = 1024;

/// Test-only stall injected into the broker tool worker so regression tests
/// can pin the register/project path without blocking the dispatcher.
/// Filtered by `connection_id` so concurrent lifecycle tests are not hung.
#[cfg(test)]
struct TestBrokerToolStall {
    connection_id: String,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
static TEST_BROKER_TOOL_STALL: std::sync::Mutex<Option<TestBrokerToolStall>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn install_test_broker_tool_stall(
    connection_id: impl Into<String>,
    release: tokio::sync::oneshot::Receiver<()>,
) {
    *TEST_BROKER_TOOL_STALL
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(TestBrokerToolStall {
        connection_id: connection_id.into(),
        release,
    });
}

/// Run parent tool_call registration + child runtime projection for one
/// envelope. Always invoked from the dedicated broker-tool worker (never
/// from the lifecycle `select!` branch).
async fn run_broker_tool_side_effects(broker: &DelegationBroker, envelope: &InternalEventEnvelope) {
    #[cfg(test)]
    {
        let stall = {
            let mut guard = TEST_BROKER_TOOL_STALL
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            match guard.as_ref() {
                Some(s) if s.connection_id == envelope.connection_id => guard.take(),
                _ => None,
            }
        };
        if let Some(stall) = stall {
            tracing::info!(
                connection_id = %envelope.connection_id,
                "[lifecycle][test] broker tool worker parked on test stall"
            );
            let _ = stall.release.await;
        }
    }
    register_delegation_tool_call_from_event(broker, envelope.event.as_ref()).await;
    broker
        .project_child_tool_event(&envelope.connection_id, &envelope.payload)
        .await;
}

/// Enqueue a tool event onto the off-select broker worker. Never awaits.
fn enqueue_broker_tool_effect(
    broker_tool_tx: &mpsc::Sender<Arc<InternalEventEnvelope>>,
    envelope: Arc<InternalEventEnvelope>,
) {
    match broker_tool_tx.try_send(envelope) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(env)) => {
            tracing::warn!(
                connection_id = %env.connection_id,
                "[lifecycle][WARN] broker tool queue full — overflow deliver \
                 (dispatcher stays unblocked; TurnComplete path unaffected)"
            );
            let tx = broker_tool_tx.clone();
            tokio::spawn(async move {
                if tx.send(env).await.is_err() {
                    tracing::error!("[lifecycle][ERROR] broker tool worker closed during overflow");
                }
            });
        }
        Err(mpsc::error::TrySendError::Closed(env)) => {
            tracing::error!(
                connection_id = %env.connection_id,
                "[lifecycle][ERROR] broker tool worker closed — \
                 register/project will not run for this ToolCall"
            );
        }
    }
}

/// Subscribe to the in-process bus (and the critical lifecycle lane)
/// synchronously and return the dispatcher loop future.
///
/// Filters out events the lifecycle worker doesn't care about
/// (high-frequency ContentDelta / ToolCall / PermissionRequest etc.) and
/// fans the rest out to per-connection worker tasks. Within a single
/// connection, ordering is preserved by the per-worker mpsc; across
/// connections, workers run independently so a slow SQLite write on one
/// connection doesn't backpressure the others.
///
/// The dispatcher **never blocks** on a full worker mailbox, and **never
/// awaits** broker tool registration/projection inside `select!` (those run
/// on a dedicated serial worker). Critical events arrive only on the
/// dedicated mpsc lane when it is live (single lifecycle source — no
/// broadcast re-enqueue of the same `seq`), so a ContentDelta flood or a
/// stuck `project_child_tool_event` cannot drop or delay `TurnComplete`.
///
/// The `subscribe()` / `take_critical_rx()` calls happen here, before the
/// future is returned, so any events emitted between this call and the first
/// poll are buffered rather than dropped.
pub fn lifecycle_subscriber_task(
    db_conn: DatabaseConnection,
    manager: ConnectionManager,
    bus: Arc<InternalEventBus>,
    broker: Option<Arc<DelegationBroker>>,
) -> impl Future<Output = ()> + Send + 'static {
    let mut rx = bus.subscribe();
    let mut critical_rx = bus.take_critical_rx();
    let critical_lane_live = critical_rx.is_some();
    if !critical_lane_live {
        tracing::error!(
            "[lifecycle][ERROR] critical lifecycle lane already taken or missing — \
             TurnComplete relies on broadcast only (lag-prone)"
        );
    } else {
        tracing::info!(
            "[lifecycle] critical lane attached (TurnComplete sole source; \
             broker tool work off-select)"
        );
    }
    let metrics = Arc::clone(bus.metrics());

    async move {
        // Off-select serial worker for broker tool side-effects. Spawned
        // inside the async body so we are on a Tokio runtime (this function
        // is often constructed before `tauri::async_runtime::spawn`).
        let broker_tool_tx = broker.as_ref().map(|b| {
            let (tx, mut tool_rx) =
                mpsc::channel::<Arc<InternalEventEnvelope>>(BROKER_TOOL_QUEUE_CAPACITY);
            let b = Arc::clone(b);
            tokio::spawn(async move {
                while let Some(env) = tool_rx.recv().await {
                    run_broker_tool_side_effects(b.as_ref(), env.as_ref()).await;
                }
            });
            tx
        });

        // connection_id → worker mailbox. Workers are spawned lazily on the
        // connection's first relevant event and torn down after a terminal
        // event by dropping the sender (worker drains its queue and exits).
        let mut workers: HashMap<String, mpsc::Sender<Arc<InternalEventEnvelope>>> = HashMap::new();
        loop {
            tokio::select! {
                // Prefer critical lane so status CAS is not starved by a
                // ContentDelta flood on the broadcast receiver.
                biased;

                crit = async {
                    match critical_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match crit {
                        Some(envelope_arc) => {
                            let kind = lifecycle_payload_kind(&envelope_arc.payload);
                            if matches!(envelope_arc.payload, AcpEvent::TurnComplete { .. }) {
                                tracing::info!(
                                    connection_id = %envelope_arc.connection_id,
                                    event = %kind,
                                    has_completion_sidecar = envelope_arc.completion.is_some(),
                                    "[lifecycle] critical lane received TurnComplete"
                                );
                            }
                            // Critical lane only carries lifecycle-critical
                            // payloads — no broker tool-correlation needed here.
                            enqueue_lifecycle_envelope(
                                &mut workers,
                                &db_conn,
                                &manager,
                                &broker,
                                metrics.as_ref(),
                                envelope_arc,
                                "critical_lane",
                            );
                        }
                        None => {
                            tracing::error!(
                                "[lifecycle][ERROR] critical lifecycle lane closed"
                            );
                            critical_rx = None;
                        }
                    }
                }

                bus_msg = rx.recv() => {
                    match bus_msg {
                        Ok(envelope_arc) => {
                            if matches!(envelope_arc.payload, AcpEvent::TurnComplete { .. }) {
                                tracing::info!(
                                    connection_id = %envelope_arc.connection_id,
                                    event = %lifecycle_payload_kind(&envelope_arc.payload),
                                    has_completion_sidecar = envelope_arc.completion.is_some(),
                                    critical_lane_live = critical_rx.is_some(),
                                    "[lifecycle] broadcast received TurnComplete"
                                );
                            }

                            // Off-select delegation correlation: never await
                            // register/project here. A stuck projector lock
                            // used to freeze this select branch and starve
                            // the critical lane (Codex #385 / child tool flood).
                            if let Some(ref tool_tx) = broker_tool_tx {
                                let needs_broker = matches!(
                                    envelope_arc.payload,
                                    AcpEvent::ToolCall { .. }
                                        | AcpEvent::ToolCallUpdate { .. }
                                );
                                if needs_broker {
                                    enqueue_broker_tool_effect(
                                        tool_tx,
                                        Arc::clone(&envelope_arc),
                                    );
                                }
                            }

                            if !is_lifecycle_relevant(&envelope_arc.payload) {
                                continue;
                            }

                            // Single lifecycle source for critical payloads:
                            // when the critical lane is live, broadcast must
                            // NOT re-enqueue TurnComplete / SessionStarted /
                            // terminal Error / Disconnected. Duplicates used
                            // to re-spawn workers after terminal teardown and
                            // race CAS / broker drain.
                            if is_lifecycle_critical(&envelope_arc.payload)
                                && critical_rx.is_some()
                            {
                                continue;
                            }

                            enqueue_lifecycle_envelope(
                                &mut workers,
                                &db_conn,
                                &manager,
                                &broker,
                                metrics.as_ref(),
                                envelope_arc,
                                "broadcast",
                            );
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            // Critical lane should still deliver TurnComplete.
                            // Log at ERROR so operators notice residual risk
                            // (SessionStarted on broadcast-only consumers, etc.).
                            tracing::error!(
                                skipped,
                                "[lifecycle][ERROR] internal bus lagged, dropped {skipped} \
                                 broadcast events (critical lane should still carry \
                                 TurnComplete/SessionStarted)"
                            );
                            metrics.lagged_count.fetch_add(skipped, Ordering::Relaxed);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::info!(
                                "[lifecycle] internal bus closed; dispatcher exiting"
                            );
                            drop(workers);
                            break;
                        }
                    }
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
    use tokio::sync::RwLock;

    fn fake_connection_with_state(
        id: &str,
        conv_id: Option<i32>,
    ) -> crate::acp::connection::AgentConnection {
        let (tx, _rx, _cmd_liveness_rx) = crate::acp::connection::connection_channel(1);
        let (control_tx, _control_rx, _control_liveness_rx) =
            crate::acp::connection::connection_channel(1);
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
            control_tx,
            task_abort: None,
            state: Arc::new(RwLock::new(state)),
            emitter: EventEmitter::Noop,
            prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
            spawn_config: {
                let plan = crate::acp::delegation::route::test_empty_route_plan();
                crate::acp::connection::matching_config_pair(
                    String::new(),
                    "system",
                    plan.fingerprint.clone(),
                )
                .0
            },
            observed_config: {
                let plan = crate::acp::delegation::route::test_empty_route_plan();
                crate::acp::connection::matching_config_pair(
                    String::new(),
                    "system",
                    plan.fingerprint,
                )
                .1
            },
            terminal_shell: crate::acp::connection::test_placeholder_terminal_shell(),
            route_plan: crate::acp::delegation::route::test_empty_route_plan(),
            origin: crate::acp::delegation::route::DelegationConnectionOrigin::Root,
            route_preference: None,
            route_capability:
                crate::acp::delegation::route::RouteCapabilitySnapshot::test_supported(),
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
    async fn handle_event_session_started_broadcasts_conversation_upsert() {
        // SessionStarted persists external_id; it must ALSO re-broadcast the
        // full summary on `conversation://changed` so every client's sidebar
        // converges (the create-time upsert carried external_id: null).
        use crate::web::event_bridge::{WebEventBroadcaster, CONVERSATION_CHANGED_EVENT};
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/test-sess-upsert").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut rx = broadcaster.subscribe();
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            let mut conn = fake_connection_with_state("c1", Some(conv.id));
            conn.emitter = EventEmitter::test_web_only(broadcaster.clone());
            map.insert("c1".to_string(), conn);
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::SessionStarted {
                session_id: "ext-99".into(),
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();

        // external_id persisted on the row...
        let reloaded = conversation_service::get_by_id(&db.conn, conv.id)
            .await
            .unwrap();
        assert_eq!(reloaded.external_id.as_deref(), Some("ext-99"));

        // ...and a `conversation://changed` upsert carrying it was broadcast.
        let evt = rx
            .try_recv()
            .expect("SessionStarted should broadcast a conversation upsert");
        assert_eq!(evt.channel, CONVERSATION_CHANGED_EVENT);
        let p = &*evt.payload;
        assert_eq!(p["kind"], "upsert");
        assert_eq!(p["summary"]["id"], conv.id);
        assert_eq!(p["summary"]["external_id"], "ext-99");
    }

    #[tokio::test]
    async fn handle_event_session_started_skips_soft_deleted_conversation() {
        // A fork emits `SessionStarted{S2}`. If the bound conversation was
        // soft-deleted while its ACP connection stayed live (delete only
        // soft-marks the row; it never disconnects the agent), this late write
        // must be a total no-op: `update_external_id` is guarded on
        // `deleted_at IS NULL`, so the deleted row keeps its S1 session id and
        // its `updated_at`, and `emit_conversation_upsert` (which re-fetches via
        // `get_by_id`, itself deleted-filtered) broadcasts nothing. This locks
        // the actual residual path end-to-end, not just the shared helper.
        use crate::db::entities::conversation;
        use crate::web::event_bridge::WebEventBroadcaster;
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/test-sess-deleted").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        conversation_service::update_external_id(&db.conn, conv.id, "session-S1".into())
            .await
            .unwrap();
        conversation_service::soft_delete(&db.conn, conv.id)
            .await
            .unwrap();
        // Snapshot the row AFTER the delete so we can prove the stale event
        // changes nothing (external_id AND updated_at must be untouched).
        let before = conversation::Entity::find_by_id(conv.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("soft-deleted row still exists");

        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut rx = broadcaster.subscribe();
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            let mut conn = fake_connection_with_state("c1", Some(conv.id));
            conn.emitter = EventEmitter::test_web_only(broadcaster.clone());
            map.insert("c1".to_string(), conn);
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::SessionStarted {
                session_id: "session-S2".into(),
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();

        // The deleted row is untouched: still deleted, still S1, same updated_at.
        let after = conversation::Entity::find_by_id(conv.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("row still present");
        assert!(after.deleted_at.is_some(), "row must remain soft-deleted");
        assert_eq!(
            after.external_id.as_deref(),
            Some("session-S1"),
            "a stale SessionStarted must not re-point a soft-deleted row S1 → S2"
        );
        assert_eq!(
            after.updated_at, before.updated_at,
            "a no-op SessionStarted must not bump updated_at on a deleted row"
        );

        // And NO conversation upsert may be broadcast for a deleted row.
        assert!(
            rx.try_recv().is_err(),
            "no conversation upsert should be broadcast when the bound row is deleted"
        );
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
                mark_awaiting_reply: true,
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::PendingReview
        );
    }

    async fn read_row_awaiting_reply_token(
        db: &crate::db::AppDatabase,
        conversation_id: i32,
    ) -> Option<String> {
        use crate::db::entities::conversation;
        use sea_orm::EntityTrait;
        conversation::Entity::find_by_id(conversation_id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("conversation row exists")
            .awaiting_reply_token
    }

    #[tokio::test]
    async fn handle_event_updates_conversation_status_on_turn_complete_marks_awaiting_reply() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/turn-complete-await").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
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
                mark_awaiting_reply: true,
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::PendingReview
        );
        assert!(
            read_row_awaiting_reply_token(&db, conv.id).await.is_some(),
            "eligible root end_turn must mint awaiting_reply_token"
        );
    }

    #[tokio::test]
    async fn turn_complete_emits_exactly_one_state_event_with_backend_token() {
        use crate::db::entities::conversation;
        use crate::web::event_bridge::{WebEventBroadcaster, CONVERSATION_CHANGED_EVENT};
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/turn-complete-state-event").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut global_rx = broadcaster.subscribe();
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            let mut conn = fake_connection_with_state("c1", Some(conv.id));
            conn.emitter = EventEmitter::test_web_only(broadcaster.clone());
            map.insert("c1".to_string(), conn);
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "ext-1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
                mark_awaiting_reply: true,
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();

        let row = conversation::Entity::find_by_id(conv.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, ConversationStatus::PendingReview);
        let token = row
            .awaiting_reply_token
            .clone()
            .expect("eligible end_turn mints token");

        let mut state_events = Vec::new();
        while let Ok(evt) = global_rx.try_recv() {
            if evt.channel == CONVERSATION_CHANGED_EVENT {
                state_events.push(evt);
            }
        }
        assert_eq!(
            state_events.len(),
            1,
            "end_turn CAS must emit exactly one conversation://changed state event"
        );
        let p = &*state_events[0].payload;
        assert_eq!(p["kind"], "state");
        assert_eq!(p["patch"]["id"], conv.id);
        assert_eq!(p["patch"]["status"], "pending_review");
        assert_eq!(p["patch"]["awaiting_reply_token"], token);
        assert_eq!(
            p["patch"]["updated_at"],
            serde_json::to_value(row.updated_at).unwrap()
        );
    }

    #[tokio::test]
    async fn terminal_disconnect_emits_exactly_one_state_event_on_cas_win() {
        use crate::db::entities::conversation;
        use crate::web::event_bridge::{WebEventBroadcaster, CONVERSATION_CHANGED_EVENT};
        use sea_orm::EntityTrait;

        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/term-state-event").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut global_rx = broadcaster.subscribe();
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            let mut conn = fake_connection_with_state("c1", Some(conv.id));
            conn.emitter = EventEmitter::test_web_only(broadcaster.clone());
            map.insert("c1".to_string(), conn);
        }
        let mut cache: HashMap<String, CachedConn> = HashMap::new();
        seed_cache(&mut cache, &mgr, "c1", conv.id).await;

        handle_terminal_event(&db.conn, &mut cache, "c1")
            .await
            .unwrap();

        let row = conversation::Entity::find_by_id(conv.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, ConversationStatus::Cancelled);

        let mut state_events = Vec::new();
        while let Ok(evt) = global_rx.try_recv() {
            if evt.channel == CONVERSATION_CHANGED_EVENT {
                state_events.push(evt);
            }
        }
        assert_eq!(state_events.len(), 1);
        let p = &*state_events[0].payload;
        assert_eq!(p["kind"], "state");
        assert_eq!(p["patch"]["status"], "cancelled");
        assert_eq!(
            p["patch"]["updated_at"],
            serde_json::to_value(row.updated_at).unwrap()
        );
    }

    #[tokio::test]
    async fn handle_event_updates_conversation_status_on_turn_complete_background_no_token() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/turn-complete-bg").await;
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
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
                mark_awaiting_reply: false,
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::PendingReview
        );
        assert!(
            read_row_awaiting_reply_token(&db, conv.id).await.is_none(),
            "background root end_turn must not mint a token"
        );
    }

    #[tokio::test]
    async fn handle_event_updates_conversation_status_on_turn_complete_child_no_token() {
        use crate::acp::delegation::spawner::DelegationLink;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/turn-complete-child").await;
        let parent =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::Codex,
            None,
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "call-1".into(),
            }),
        )
        .await
        .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c1".to_string(),
                fake_connection_with_state("c1", Some(child.id)),
            );
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "ext-1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "codex".into(),
                mark_awaiting_reply: true,
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        // Linked delegates: broker/store CAS owns terminal ConversationStatus.
        // Generic lifecycle must not mint PendingReview or an awaiting-reply token.
        assert_eq!(
            read_row_status(&db, child.id).await,
            ConversationStatus::InProgress
        );
        assert!(
            read_row_awaiting_reply_token(&db, child.id).await.is_none(),
            "delegate end_turn never receives a generation even if mark is true"
        );
    }

    #[tokio::test]
    async fn handle_event_updates_conversation_status_on_turn_complete_respects_completed_cas() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/turn-complete-completed").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
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
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c1".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "ext-1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
                mark_awaiting_reply: true,
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::Completed,
            "Completed row must win over delayed end_turn CAS"
        );
        assert!(read_row_awaiting_reply_token(&db, conv.id).await.is_none());
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
                    mark_awaiting_reply: false,
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
                mark_awaiting_reply: false,
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
                mark_awaiting_reply: false,
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

    #[test]
    fn format_terminal_error_with_code_prefixes_bracketed_label() {
        // The lifecycle worker stitches `[code] message` together so the
        // parent agent's tool-call result reads with both a stable
        // machine-readable bucket and the human-readable detail.
        assert_eq!(
            format_terminal_error("Authentication required", Some("auth_required")),
            "[auth_required] Authentication required"
        );
    }

    #[test]
    fn format_terminal_error_without_code_returns_trimmed_message() {
        assert_eq!(
            format_terminal_error("  transport closed\n", None),
            "transport closed"
        );
    }

    #[test]
    fn format_terminal_error_treats_blank_code_as_absent() {
        // Defensive: a whitespace-only code shouldn't produce a stray `[]` prefix.
        assert_eq!(
            format_terminal_error("agent crashed", Some("   ")),
            "agent crashed"
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
            mark_awaiting_reply: false,
        }));
        assert!(is_lifecycle_relevant(&AcpEvent::ConversationLinked {
            conversation_id: 1,
            folder_id: 1,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        }));
        assert!(is_lifecycle_relevant(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Disconnected,
        }));
        assert!(is_lifecycle_relevant(&AcpEvent::Error {
            message: "boom".into(),
            agent_type: "claude_code".into(),
            code: None,
            terminal: true,
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
        // ToolCall / ToolCallUpdate are NO LONGER worker-relevant: delegation
        // tool_call_id capture moved to the dispatcher loop
        // (`register_delegation_tool_call_from_event`), so neither variant
        // needs to enter a worker mailbox. Keeping them out is what relieves
        // the bus-lag pressure that dropped a parallel delegation's tool_call.
        assert!(!is_lifecycle_relevant(&AcpEvent::ToolCall {
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
        assert!(!is_lifecycle_relevant(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: Some("delegate_to_agent".into()),
            status: None,
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        }));
    }

    /// Dispatcher must drop the per-connection worker sender on either
    /// `Disconnected` or a `terminal: true` Error. Non-terminal Errors and
    /// other ConnectionStatus values must NOT trigger teardown — the
    /// worker is still expected to receive a trailing TurnComplete /
    /// Disconnected. (P2 regression in v0.14.3 post-mortem review:
    /// without the `Error { terminal: true }` arm, the worker that
    /// dispatched terminal work in lifecycle_subscriber_task would leak
    /// when the bus drops the trailing Disconnected.)
    #[test]
    fn is_dispatcher_terminal_drops_worker_on_disconnected_and_terminal_error() {
        assert!(is_dispatcher_terminal(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Disconnected,
        }));
        assert!(is_dispatcher_terminal(&AcpEvent::Error {
            message: "transport closed".into(),
            agent_type: "claude_code".into(),
            code: None,
            terminal: true,
        }));

        // Non-terminal Error: worker must survive.
        assert!(!is_dispatcher_terminal(&AcpEvent::Error {
            message: "turn refusal".into(),
            agent_type: "claude_code".into(),
            code: Some("turn_failed_refusal".into()),
            terminal: false,
        }));

        // Other StatusChanged values: worker must survive.
        assert!(!is_dispatcher_terminal(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Connected,
        }));
        assert!(!is_dispatcher_terminal(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        }));
        assert!(!is_dispatcher_terminal(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Error,
        }));

        // Other event arms must never trigger teardown.
        assert!(!is_dispatcher_terminal(&AcpEvent::TurnComplete {
            session_id: "s".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
            mark_awaiting_reply: false,
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
                mark_awaiting_reply: false,
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
        let conn = fake_connection_with_state("c1", Some(conv.id));
        let mut status_rx = {
            let state = conn.state.read().await;
            state.event_stream().subscribe()
        };
        {
            let mut map = mgr.connections.lock().await;
            map.insert("c1".to_string(), conn);
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
                mark_awaiting_reply: false,
            },
        }));

        // The private-stream acknowledgement is emitted after the worker
        // commits the trailing event. `dispatcher.await` only performs
        // dispatcher cleanup; it does not prove worker drain completion.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let envelope = status_rx
                    .recv()
                    .await
                    .expect("private stream stays open until status acknowledgement");
                if matches!(
                    &envelope.payload,
                    AcpEvent::ConversationStatusChanged {
                        conversation_id,
                        status: ConversationStatus::PendingReview,
                    } if *conversation_id == conv.id
                ) {
                    break;
                }
            }
        })
        .await
        .expect("trailing TurnComplete should commit and emit PendingReview");

        drop(bus);
        let _ = dispatcher.await;

        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::PendingReview,
            "TurnComplete at the tail of a 200-event burst MUST be delivered \
             (regression test for `try_send` drop bug)"
        );
    }

    /// Regression for the #385 failure chain: a stuck broker tool path
    /// (`project_child_tool_event` / register) must **not** park the
    /// lifecycle `select!`, so `TurnComplete` on the critical lane still
    /// flips the row to `pending_review` while the stall is held.
    ///
    /// Without the off-select broker worker, awaiting projection inside the
    /// broadcast branch prevents critical-lane polling entirely.
    #[tokio::test]
    async fn turn_complete_cas_while_broker_tool_path_blocked() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/disp-broker-stall").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "parent-stall".to_string(),
                fake_connection_with_state("parent-stall", Some(conv.id)),
            );
        }

        // Park the broker tool worker before any ToolCall arrives.
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        install_test_broker_tool_stall("child-tool-flood", release_rx);

        // Real broker so the tool worker path is exercised (register/project).
        let mock = Arc::new(MockSpawner::new());
        let broker = Arc::new(DelegationBroker::new(
            mock as Arc<dyn ConnectionSpawner>,
            Arc::new(NoopDepthLookup) as Arc<dyn ConversationDepthLookup>,
        ));

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics.clone()));
        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            Some(broker),
        ));

        // ToolCall lands first and parks the broker tool worker on the stall.
        bus.send(Arc::new(EventEnvelope {
            seq: 1,
            connection_id: "child-tool-flood".to_string(),
            payload: AcpEvent::ToolCall {
                tool_call_id: "tc-stall".into(),
                title: "bash".into(),
                kind: "execute".into(),
                status: "in_progress".into(),
                content: None,
                raw_input: Some(r#"{"command":"sleep"}"#.into()),
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            },
        }));

        // Yield so the broker tool worker dequeues and hits the stall.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        // TurnComplete must CAS while the stall is still held.
        bus.send(Arc::new(EventEnvelope {
            seq: 2,
            connection_id: "parent-stall".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "s-stall".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
                mark_awaiting_reply: false,
            },
        }));

        let observed = poll_status(
            &db,
            conv.id,
            ConversationStatus::PendingReview,
            Duration::from_secs(3),
        )
        .await;

        // Still holding the stall — prove we did not need to release projection.
        assert_eq!(
            observed,
            ConversationStatus::PendingReview,
            "TurnComplete must CAS to pending_review while project_child_tool_event \
             path remains blocked (critical lane must not share select await with broker)"
        );
        assert_eq!(
            metrics.critical_lane_emit_count.load(Ordering::Relaxed),
            1,
            "TurnComplete must have entered the critical lane"
        );
        assert_eq!(
            metrics.worker_queue_full_count.load(Ordering::Relaxed),
            0,
            "failure mode is broker await in select, not worker queue full"
        );

        // Release stall and tear down.
        let _ = release_tx.send(());
        drop(bus);
        let _ = dispatcher.await;
    }

    // ── Broker-cancel routing regression ─────────────────────────────────
    //
    // The lifecycle worker MUST gate `broker.cancel_by_child_connection`
    // on `terminal == true`. `AcpEvent::Error` also fires mid-turn from
    // `turn_failure_error_event` (refusal / max_tokens / empty / unknown)
    // immediately before `TurnComplete`, while the child connection stays
    // alive. Cancelling at Error there would race-drain the pending
    // broker entry before `complete_call` could map the real stop reason
    // — surfacing "canceled" to the parent agent instead of
    // `ChildRefusal` / `ChildMaxTokens` / …. (See F1 in the v0.14.3
    // sub-agent delegation post-mortem.)
    //
    // On the truly terminal path (`connection.rs:493`) the worker drains
    // the broker on Error directly with the detail, then dedupes the
    // trailing Disconnected. This avoids the "Error reaches us but the
    // bus drops Disconnected" hang where `handle_request`'s `rx.await`
    // would block forever.
    //
    // These tests drive `lifecycle_subscriber_task` end-to-end with a real
    // `DelegationBroker` + `MockSpawner` so the dispatcher → worker →
    // broker chain is exercised the same way it runs in production.

    use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationBroker};
    use crate::acp::delegation::spawner::{accepted, mock::MockSpawner, ConnectionSpawner};
    use crate::acp::delegation::types::{DelegationError, DelegationOutcome, DelegationRequest};
    use async_trait::async_trait;
    use chrono::Utc;

    struct NoopDepthLookup;

    #[async_trait]
    impl ConversationDepthLookup for NoopDepthLookup {
        async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
            Ok(None)
        }
    }

    fn delegation_request(child_conn_id: &str) -> DelegationRequest {
        DelegationRequest {
            parent_connection_id: format!("parent-of-{child_conn_id}"),
            parent_conversation_id: 1,
            parent_tool_use_id: format!("tu-{child_conn_id}"),
            agent_type: AgentType::ClaudeCode,
            profile_id: None,
            task: "do x".into(),
            working_dir: None,
            requested_working_dir: None,
            external_handle: None,
        }
    }

    /// Stage a broker with one pending entry whose `child_connection_id`
    /// matches the test connection. The returned join handle resolves
    /// once the broker drains the entry (via cancel or complete).
    async fn stage_pending_delegation(
        child_conn_id: &str,
        child_conv_id: i32,
    ) -> (
        Arc<DelegationBroker>,
        tokio::task::JoinHandle<DelegationOutcome>,
    ) {
        let mock = Arc::new(MockSpawner::new());
        mock.queue_spawn(Ok(child_conn_id.to_string())).await;
        mock.queue_send(Ok(accepted(child_conv_id, Utc::now())))
            .await;
        let broker = Arc::new(DelegationBroker::new(
            mock as Arc<dyn ConnectionSpawner>,
            Arc::new(NoopDepthLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        // Production default is `enabled: false`; lifecycle tests need
        // delegation active so `handle_request` parks the pending entry
        // they're about to assert against.
        broker
            .set_config(crate::acp::delegation::broker::DelegationConfig {
                enabled: true,
                ..crate::acp::delegation::broker::DelegationConfig::default()
            })
            .await;
        let driver = {
            let broker = broker.clone();
            let id = child_conn_id.to_string();
            tokio::spawn(async move { broker.handle_request(delegation_request(&id)).await })
        };
        // Spin until the broker has registered the pending entry so the
        // test doesn't race the spawn/send awaits.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while broker.pending_count().await == 0 {
            if std::time::Instant::now() >= deadline {
                panic!("broker never registered pending entry");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        (broker, driver)
    }

    /// `Error` alone must NOT drain the broker. The pending entry stays
    /// in-flight so an upcoming `TurnComplete` can resolve it via
    /// `complete_call` with the correct child-side error mapping.
    #[tokio::test]
    async fn dispatcher_error_alone_does_not_drain_broker_pending_entry() {
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let (broker, driver) = stage_pending_delegation("c-no-drain", 41).await;

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics));
        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            Some(broker.clone()),
        ));

        bus.send(Arc::new(EventEnvelope {
            seq: 1,
            connection_id: "c-no-drain".to_string(),
            payload: AcpEvent::Error {
                message: "Gemini refused the prompt.".into(),
                agent_type: "gemini".into(),
                code: Some("turn_failed_refusal".into()),
                // turn-failure Error: non-terminal. Worker MUST no-op (the
                // upcoming TurnComplete maps the outcome via complete_call).
                terminal: false,
            },
        }));

        // Give the worker time to process Error. Without the fix it would
        // call `cancel_by_child_connection` and the pending entry would
        // drop to 0 here.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            broker.pending_count().await,
            1,
            "Error-only event must NOT drain the pending delegation — TurnComplete still needs to map it"
        );

        // Cleanup: send Disconnected so the driver resolves, dispatcher exits.
        bus.send(Arc::new(EventEnvelope {
            seq: 2,
            connection_id: "c-no-drain".to_string(),
            payload: AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected,
            },
        }));
        drop(bus);
        let _ = driver.await;
        let _ = dispatcher.await;
    }

    /// Defense-in-depth: a terminal `Error` alone (no trailing
    /// `Disconnected`) must still drain the broker. In production
    /// `Disconnected` always follows, but the in-process bus is a
    /// `broadcast` channel — a `Lagged` event or a task abort between
    /// emit sites would otherwise leave the broker's `rx.await` blocked
    /// forever and hang the parent's `delegate_to_agent` call. (See P1
    /// in the v0.14.3 post-mortem follow-up review.)
    #[tokio::test]
    async fn dispatcher_terminal_error_alone_drains_broker_with_detail() {
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let (broker, driver) = stage_pending_delegation("c-error-alone", 51).await;

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics));
        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            Some(broker.clone()),
        ));

        bus.send(Arc::new(EventEnvelope {
            seq: 1,
            connection_id: "c-error-alone".to_string(),
            payload: AcpEvent::Error {
                message: "transport closed".into(),
                agent_type: "claude_code".into(),
                code: None,
                terminal: true,
            },
        }));
        // Deliberately no Disconnected — simulates the bus dropping it
        // (Lagged) or the run_connection task aborting after Error.

        let outcome = tokio::time::timeout(Duration::from_secs(2), driver)
            .await
            .expect("terminal Error alone must drain the broker (no hang)")
            .unwrap();
        match &outcome {
            DelegationOutcome::Err { code, message, .. } => {
                assert_eq!(code, "canceled");
                assert_eq!(
                    message, "canceled: child session ended without TurnComplete: transport closed",
                    "terminal Error detail must reach the broker without waiting for Disconnected"
                );
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }

        drop(bus);
        let _ = dispatcher.await;
    }

    /// `Error` → `Disconnected` (the genuinely terminal path emitted by
    /// `connection.rs:488` → 514) must drain the broker AND thread the
    /// Error detail into the canceled reason, so the parent agent's
    /// `delegate_to_agent` tool result reads with the real failure cause
    /// instead of the opaque default. The drain happens on Error; the
    /// trailing Disconnected is a no-op (verified by the absence of a
    /// double-emit elsewhere — `cancel_by_child_connection` is
    /// idempotent).
    #[tokio::test]
    async fn dispatcher_error_then_disconnected_threads_buffered_detail_to_broker() {
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let (broker, driver) = stage_pending_delegation("c-auth-fail", 42).await;

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics));
        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            Some(broker.clone()),
        ));

        bus.send(Arc::new(EventEnvelope {
            seq: 1,
            connection_id: "c-auth-fail".to_string(),
            payload: AcpEvent::Error {
                message: "Authentication required".into(),
                agent_type: "gemini".into(),
                code: Some("auth_required".into()),
                // Genuinely terminal: matches `connection.rs:493`, the only
                // emit site where the run_connection task is unwinding.
                terminal: true,
            },
        }));
        bus.send(Arc::new(EventEnvelope {
            seq: 2,
            connection_id: "c-auth-fail".to_string(),
            payload: AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected,
            },
        }));

        let outcome = driver.await.unwrap();
        match &outcome {
            DelegationOutcome::Err { code, message, .. } => {
                assert_eq!(code, "canceled");
                assert_eq!(
                    message,
                    "canceled: child session ended without TurnComplete: \
                     [auth_required] Authentication required",
                    "the buffered Error detail must be stitched into the canceled reason"
                );
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }

        drop(bus);
        let _ = dispatcher.await;
    }

    /// Bare `Disconnected` (no preceding Error — e.g. clean transport close
    /// with a delegation still in flight) must still drain the broker,
    /// but with the default fallback reason since there's nothing buffered.
    #[tokio::test]
    async fn dispatcher_disconnected_alone_drains_broker_with_default_reason() {
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let (broker, driver) = stage_pending_delegation("c-bare-disco", 43).await;

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics));
        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            Some(broker.clone()),
        ));

        bus.send(Arc::new(EventEnvelope {
            seq: 1,
            connection_id: "c-bare-disco".to_string(),
            payload: AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected,
            },
        }));

        let outcome = driver.await.unwrap();
        match &outcome {
            DelegationOutcome::Err { code, message, .. } => {
                assert_eq!(code, "canceled");
                assert_eq!(
                    message,
                    "canceled: child session ended without TurnComplete"
                );
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }

        drop(bus);
        let _ = dispatcher.await;
    }

    /// F2 regression: a non-terminal `Error` (e.g. `session/load` fallback,
    /// `turn_failure_error_event`, idle SetMode failure) must NOT pollute
    /// `last_error`. If it did, an unrelated future `Disconnected` would
    /// stitch a stale detail into the broker's canceled reason. The fix
    /// gates the buffer on `terminal == true` — only the run_connection
    /// failure path qualifies. (Without this fix, the assertion below sees
    /// `…: [session_load_failed] Failed to load session…` instead of the
    /// default.)
    #[tokio::test]
    async fn dispatcher_non_terminal_error_does_not_pollute_disconnected_drain_reason() {
        let db = test_helpers::fresh_in_memory_db().await;
        let mgr = ConnectionManager::new();
        let (broker, driver) = stage_pending_delegation("c-nonterm", 44).await;

        let metrics = Arc::new(EventBusMetrics::default());
        let bus = Arc::new(InternalEventBus::new(metrics));
        let dispatcher = tokio::spawn(lifecycle_subscriber_task(
            db.conn.clone(),
            mgr.clone_ref(),
            bus.clone(),
            Some(broker.clone()),
        ));

        // A non-terminal Error fires first (e.g. recoverable session/load
        // fallback during child setup). The worker MUST ignore it.
        bus.send(Arc::new(EventEnvelope {
            seq: 1,
            connection_id: "c-nonterm".to_string(),
            payload: AcpEvent::Error {
                message: "Failed to load session, starting new: stale id".into(),
                agent_type: "gemini".into(),
                code: None,
                terminal: false,
            },
        }));
        // Then a later, unrelated Disconnected (e.g. the parent disconnects).
        bus.send(Arc::new(EventEnvelope {
            seq: 2,
            connection_id: "c-nonterm".to_string(),
            payload: AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected,
            },
        }));

        let outcome = driver.await.unwrap();
        match &outcome {
            DelegationOutcome::Err { code, message, .. } => {
                assert_eq!(code, "canceled");
                assert_eq!(
                    message, "canceled: child session ended without TurnComplete",
                    "non-terminal Error must NOT be buffered into the broker's cancel reason"
                );
            }
            other => panic!("expected Err{{canceled}}, got {other:?}"),
        }

        drop(bus);
        let _ = dispatcher.await;
    }

    /// F2 row-state regression: a non-terminal `Error` while the
    /// conversation is mid-prompt (status = InProgress) must NOT flip the
    /// row to Cancelled — that would briefly flash "Cancelled" in the
    /// sidebar before the next TurnComplete corrects it. The worker only
    /// runs `handle_terminal_event` when `terminal == true`.
    #[tokio::test]
    async fn dispatcher_non_terminal_error_does_not_flip_conversation_row() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/f2-row-noflip").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::InProgress
        );

        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c-row".to_string(),
                fake_connection_with_state("c-row", Some(conv.id)),
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

        // ConversationLinked first so the cache binds (matches production:
        // try_cache_link runs before any terminal event).
        bus.send(Arc::new(EventEnvelope {
            seq: 1,
            connection_id: "c-row".to_string(),
            payload: AcpEvent::ConversationLinked {
                conversation_id: conv.id,
                folder_id,
                parent_conversation_id: None,
                parent_tool_use_id: None,
            },
        }));
        bus.send(Arc::new(EventEnvelope {
            seq: 2,
            connection_id: "c-row".to_string(),
            payload: AcpEvent::Error {
                message: "Failed to set mode: bad id".into(),
                agent_type: "claude_code".into(),
                code: None,
                terminal: false,
            },
        }));

        // Give the worker time to (NOT) process the row flip.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::InProgress,
            "non-terminal Error must leave the row's InProgress status intact"
        );

        drop(bus);
        let _ = dispatcher.await;
    }

    // ── Task 8 review fix: is_delegate fail-closed ───────────────────────

    #[tokio::test]
    async fn conversation_is_delegate_false_for_regular_chat() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/is-delegate-regular").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        assert!(!conversation_is_delegate(&db.conn, conv.id).await.unwrap());
    }

    #[tokio::test]
    async fn conversation_is_delegate_true_for_linked_child() {
        use crate::acp::delegation::spawner::DelegationLink;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/is-delegate-child").await;
        let parent =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "pt-1".into(),
                delegation_call_id: "call-delegate-1".into(),
            }),
        )
        .await
        .unwrap();
        assert!(conversation_is_delegate(&db.conn, child.id).await.unwrap());
    }

    #[tokio::test]
    async fn turn_complete_nondelegate_writes_pending_review() {
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/tc-nondelegate").await;
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
                session_id: "s".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
                mark_awaiting_reply: false,
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, conv.id).await,
            ConversationStatus::PendingReview
        );
    }

    #[tokio::test]
    async fn turn_complete_delegate_skips_generic_status_write() {
        use crate::acp::delegation::spawner::DelegationLink;
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/tc-delegate").await;
        let parent =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder_id,
            AgentType::ClaudeCode,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "pt-1".into(),
                delegation_call_id: "call-tc-del".into(),
            }),
        )
        .await
        .unwrap();
        // Delegate rows start InProgress; store CAS owns terminal ConversationStatus.
        assert_eq!(
            read_row_status(&db, child.id).await,
            ConversationStatus::InProgress
        );
        let mgr = ConnectionManager::new();
        {
            let mut map = mgr.connections.lock().await;
            map.insert(
                "c-child".to_string(),
                fake_connection_with_state("c-child", Some(child.id)),
            );
        }
        let env = EventEnvelope {
            seq: 1,
            connection_id: "c-child".to_string(),
            payload: AcpEvent::TurnComplete {
                session_id: "s".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
                mark_awaiting_reply: false,
            },
        };
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        assert_eq!(
            read_row_status(&db, child.id).await,
            ConversationStatus::InProgress,
            "delegate TurnComplete must not write generic PendingReview"
        );
    }

    #[tokio::test]
    async fn is_delegate_probe_db_error_skips_generic_status_write() {
        // Soft-deleted rows are filtered by get_by_id → Err, which must fail
        // closed (no generic ConversationStatus mutation) rather than treating
        // the probe as "not a delegate".
        let db = test_helpers::fresh_in_memory_db().await;
        let folder_id = test_helpers::seed_folder(&db, "/tmp/is-delegate-err").await;
        let conv =
            conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
                .await
                .unwrap();
        conversation_service::soft_delete(&db.conn, conv.id)
            .await
            .unwrap();
        assert!(
            conversation_is_delegate(&db.conn, conv.id).await.is_err(),
            "soft-deleted row must surface as probe error"
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
                session_id: "s".into(),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
                mark_awaiting_reply: false,
            },
        };
        // Must not error out of handle_event, and must not resurrect status.
        handle_event(&db.conn, &mgr, &env, None).await.unwrap();
        // Raw entity still InProgress (soft-deleted; no status flip).
        use crate::db::entities::conversation;
        use sea_orm::EntityTrait;
        let row = conversation::Entity::find_by_id(conv.id)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("row present");
        assert_eq!(row.status, ConversationStatus::InProgress);
    }
}
