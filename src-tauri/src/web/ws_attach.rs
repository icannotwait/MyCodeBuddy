//! WS attach protocol — Phase 1 of the Subscribe-with-Snapshot redesign.
//!
//! Replaces the legacy "subscribe to a global firehose + fetch HTTP snapshot
//! separately" flow. A client expressing interest in a specific connection
//! sends an `attach` message; the server atomically (under the SessionState
//! read lock) decides between a `snapshot` or `replay` response and
//! registers a per-connection broadcast receiver. After the response, live
//! events from that connection are delivered as `event` frames over the
//! same WebSocket.
//!
//! The legacy global `acp://event` channel remains active during Phase 1-3
//! for backward compatibility; Phase 4 retires it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::{fmt, sync::atomic::Ordering};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::acp::internal_bus::EventBusMetrics;
use crate::acp::manager::ConnectionManager;
use crate::acp::session_state::LiveSessionSnapshot;
use crate::acp::shared_session::{LeaseSocketBinding, SharedSessionBroker};
use crate::acp::types::{AcpEvent, EventEnvelope};

/// Maximum number of events delivered in a single `replay` response. Larger
/// gaps fall through to a `snapshot` even when the ring buffer can satisfy
/// them — past this many events, snapshot serialization is comparable in
/// size and avoids forcing the client to apply the events one-by-one.
pub const REPLAY_BATCH_THRESHOLD: usize = 32;

/// Capacity of the per-WS-connection outbound mpsc channel. Backpressure
/// from a slow WS write naturally throttles per-subscription forwarders;
/// 64 in-flight messages is enough for short bursts without making
/// memory blow up if the client stops reading.
pub const OUTBOUND_CAPACITY: usize = 64;

#[derive(Clone, Default)]
pub struct WatchdogEventGate {
    floors: BTreeMap<String, u64>,
    actionable: BTreeSet<String>,
}

impl WatchdogEventGate {
    fn new(floors: BTreeMap<String, u64>, actionable: BTreeSet<String>) -> Self {
        Self { floors, actionable }
    }

    fn allows(&mut self, event: &AcpEvent) -> bool {
        let AcpEvent::ToolWatchdogChanged { projection } = event else {
            return true;
        };
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let floor = self.floors.get(&projection.lease_id).copied().unwrap_or(0);
        match projection.phase {
            ToolWatchdogPhase::Cleared | ToolWatchdogPhase::TimedOut => {
                if projection.version < floor {
                    return false;
                }
                self.floors
                    .insert(projection.lease_id.clone(), projection.version);
                self.actionable.remove(&projection.lease_id);
            }
            ToolWatchdogPhase::Warning
            | ToolWatchdogPhase::Grace
            | ToolWatchdogPhase::Cancelling => {
                let blocked_by_tombstone = projection.version < floor
                    || (projection.version == floor
                        && floor > 0
                        && !self.actionable.contains(&projection.lease_id));
                if blocked_by_tombstone {
                    return false;
                }
                self.floors
                    .insert(projection.lease_id.clone(), projection.version);
                self.actionable.insert(projection.lease_id.clone());
            }
        }
        true
    }
}

#[derive(Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Subscribe this WebSocket to a specific connection's event stream.
    /// `since_seq` allows incremental catch-up after a brief disconnect;
    /// `None` requests a full snapshot.
    Attach {
        subscription_id: String,
        connection_id: String,
        #[serde(default)]
        generation: Option<u64>,
        #[serde(default)]
        lease_id: Option<String>,
        #[serde(default)]
        since_seq: Option<u64>,
    },
    /// Cancel a prior `attach` by `subscription_id`.
    Detach { subscription_id: String },
    /// Liveness check. Server replies with `pong`.
    Ping,
}

impl fmt::Debug for ClientMsg {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attach {
                subscription_id,
                connection_id,
                generation,
                lease_id,
                since_seq,
            } => formatter
                .debug_struct("ClientMsg::Attach")
                .field("subscription_id", subscription_id)
                .field("connection_id", connection_id)
                .field("generation", generation)
                .field("lease_id", &lease_id.as_ref().map(|_| "***"))
                .field("since_seq", since_seq)
                .finish(),
            Self::Detach { subscription_id } => formatter
                .debug_struct("ClientMsg::Detach")
                .field("subscription_id", subscription_id)
                .finish(),
            Self::Ping => formatter.write_str("ClientMsg::Ping"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachErrorCode {
    SnapshotBudgetExceeded,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Initial state for `attach` (cold start, large gap, or cursor invalid).
    /// `event_seq` is the high-water mark; subsequent `event` frames carry
    /// `seq > event_seq`. The snapshot is `Box`'d so the enum variant size
    /// doesn't dominate every other (small) variant.
    Snapshot {
        subscription_id: String,
        connection_id: String,
        snapshot: Box<LiveSessionSnapshot>,
        event_seq: u64,
    },
    /// Batched catch-up for a small gap. `high_water_seq` is the largest
    /// seq in `events`; subsequent `event` frames carry `seq > high_water_seq`.
    Replay {
        subscription_id: String,
        connection_id: String,
        events: Vec<Arc<EventEnvelope>>,
        high_water_seq: u64,
    },
    /// Live event delivered after the initial Snapshot/Replay frame.
    Event {
        subscription_id: String,
        envelope: Arc<EventEnvelope>,
    },
    /// Subscription was terminated by the server. `reason` is a stable code
    /// the client maps to UX (re-attach vs. drop the conversation).
    Detached {
        subscription_id: String,
        reason: DetachReason,
    },
    /// A recoverable attach-protocol error. The socket and live agent
    /// connection remain active so the client can choose a recovery path.
    AttachError {
        subscription_id: String,
        code: AttachErrorCode,
    },
    /// Liveness response.
    Pong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DetachReason {
    /// `connection_id` is unknown to the manager (possibly GC'd, possibly
    /// never existed). Client should treat this as a terminal state for
    /// the conversation.
    ConnectionGone,
    /// The supplied generation does not match the current shared session.
    GenerationStale,
    /// The supplied client lease id is not active on this generation.
    LeaseMissing,
    /// The supplied client lease id was recently expired.
    LeaseExpired,
    /// The public connection was replaced after a failed generation.
    SessionReplaced,
    /// The per-connection broadcast channel dropped events because this
    /// subscriber couldn't keep up. Client must re-attach with its
    /// `lastAppliedSeq` to resync.
    Lagged,
    /// Server is shutting down. Reconnect after the next handshake.
    ServerShutdown,
}

/// Decision returned by the attach handler describing what frame to send
/// back to the client and which `broadcast::Receiver` to forward live
/// events from.
pub struct AttachOutcome {
    pub initial_msg: ServerMsg,
    pub receiver: tokio::sync::broadcast::Receiver<Arc<EventEnvelope>>,
    pub binding: Option<LeaseSocketBinding>,
    pub(crate) watchdog_gate: WatchdogEventGate,
}

/// Decide-and-subscribe under a single `SessionState` read lock. The
/// returned receiver is registered before the lock releases, so any event
/// fired after this function returns is delivered (no race against
/// `emit_with_state`'s write lock).
///
/// `metrics` records which response shape was returned (cold snapshot vs.
/// resumed replay vs. snapshot fallback) so operators can spot when the
/// `REPLAY_BATCH_THRESHOLD` / ring-buffer caps need tuning.
pub async fn handle_attach(
    manager: &ConnectionManager,
    metrics: &EventBusMetrics,
    subscription_id: String,
    connection_id: String,
    generation: Option<u64>,
    lease_id: Option<String>,
    since_seq: Option<u64>,
) -> Result<AttachOutcome, DetachReason> {
    let broker = manager.shared_session_broker();
    let (binding, retained_state) = match broker
        .validate_and_bind_lease_with_state(&connection_id, generation, lease_id.as_deref())
        .await
    {
        Ok((binding, state)) => (Some(binding), Some(state)),
        Err(DetachReason::ConnectionGone) if generation.is_none() && lease_id.is_none() => {
            (None, None)
        }
        Err(reason) => return Err(reason),
    };
    let state_arc = match retained_state {
        Some(state) => state,
        None => manager
            .get_state(&connection_id)
            .await
            .ok_or(DetachReason::ConnectionGone)?,
    };

    let s = state_arc.read().await;

    // Decide response shape. Order of checks matters:
    //   - explicit None → snapshot (fresh attach; client has no state yet)
    //   - cursor at or past head → snapshot anyway, defends against client
    //     bugs where lastAppliedSeq was advanced past an event we never
    //     actually broadcast (not currently possible, but cheap to guard)
    //   - cursor in ring buffer with small gap → replay
    //   - cursor in ring buffer with large gap → snapshot (cheaper)
    //   - cursor older than ring buffer → snapshot (only choice)
    let snapshot_msg = || {
        let mut snapshot = s.to_snapshot();
        if let (Some(shared), Some(binding)) = (snapshot.shared_session.as_mut(), binding.as_ref())
        {
            shared.lease_expires_at = Some(binding.lease_expires_at);
        }
        ServerMsg::Snapshot {
            subscription_id: subscription_id.clone(),
            connection_id: connection_id.clone(),
            snapshot: Box::new(snapshot),
            event_seq: s.event_seq,
        }
    };
    let (watchdog_floors, actionable_watchdogs) = s.watchdog_event_gate_seed();
    let watchdog_gate =
        WatchdogEventGate::new(watchdog_floors, actionable_watchdogs.into_iter().collect());

    let initial_msg = match since_seq {
        None => {
            metrics.snapshot_cold_count.fetch_add(1, Ordering::Relaxed);
            snapshot_msg()
        }
        Some(cursor) if cursor >= s.event_seq => {
            // Cursor at-or-past head — treat as fresh attach. Doesn't bump
            // `snapshot_fallback_count` because there's no gap-too-large
            // semantic; this is just a defensive equivalent of cold start.
            metrics.snapshot_cold_count.fetch_add(1, Ordering::Relaxed);
            snapshot_msg()
        }
        Some(cursor) => match s.recent_events_after(cursor) {
            Some(events) if events.len() <= REPLAY_BATCH_THRESHOLD && !events.is_empty() => {
                let high_water_seq = events.last().expect("non-empty checked above").seq;
                let mut replay_gate = watchdog_gate.clone();
                let events: Vec<_> = events
                    .into_iter()
                    .filter(|event| replay_gate.allows(&event.payload))
                    .collect();
                let event_count = events.len() as u64;
                metrics.replay_count.fetch_add(1, Ordering::Relaxed);
                metrics
                    .replay_event_total
                    .fetch_add(event_count, Ordering::Relaxed);
                ServerMsg::Replay {
                    subscription_id: subscription_id.clone(),
                    connection_id: connection_id.clone(),
                    events,
                    high_water_seq,
                }
            }
            // Either too many to batch, or buffer doesn't cover the cursor.
            _ => {
                metrics
                    .snapshot_fallback_count
                    .fetch_add(1, Ordering::Relaxed);
                snapshot_msg()
            }
        },
    };

    let receiver = s.event_stream().subscribe();
    drop(s);

    Ok(AttachOutcome {
        initial_msg,
        receiver,
        binding,
        watchdog_gate,
    })
}

/// Spawn a forwarding task that drains the per-connection broadcast receiver
/// and sends `Event` frames to the shared outbound channel. Exits on
/// receiver close (connection went away) or `Lagged` (slow consumer); in
/// both cases sends a `Detached` frame so the client knows to re-attach.
/// Lease-bound subscriptions keep their binding in the WS subscription map
/// when the retained state stream closes, allowing the next ping to classify
/// a replacement through the broker's bounded generation tombstone.
///
/// `cleanup_tx` carries `(subscription_id, epoch)` back to the WS main loop
/// on every self-exit path so the loop can drop the now-completed
/// `JoinHandle` from its `subscriptions` map. The `epoch` is critical: a
/// stale signal arriving after the client has re-attached (which replaces
/// the handle) would otherwise orphan the fresh forwarder. The main loop
/// only removes when the stored epoch matches the signal's epoch — re-attach
/// stamps a new epoch so old signals become no-ops. Without epoch matching,
/// `JoinHandle::is_finished()` is racy on multi-threaded runtimes (the
/// runtime may not have updated the JoinHandle slot yet when the cleanup
/// signal is consumed).
///
/// Send is `try_send` so a saturated cleanup channel never blocks the
/// exiting task; the socket-close `subscriptions.drain()` is the safety net.
///
/// `metrics` records `Lagged` exits so operators can correlate attach
/// re-attachment storms with per-connection broadcast pressure.
#[allow(clippy::too_many_arguments)]
pub fn spawn_forwarder(
    subscription_id: String,
    epoch: u64,
    metrics: Arc<EventBusMetrics>,
    mut receiver: tokio::sync::broadcast::Receiver<Arc<EventEnvelope>>,
    mut watchdog_gate: WatchdogEventGate,
    outbound: mpsc::Sender<ServerMsg>,
    cleanup_tx: mpsc::Sender<(String, u64)>,
    broker: SharedSessionBroker,
    binding: Option<LeaseSocketBinding>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let signal_cleanup = || {
            let _ = cleanup_tx.try_send((subscription_id.clone(), epoch));
        };
        loop {
            match receiver.recv().await {
                Ok(envelope) => {
                    if !watchdog_gate.allows(&envelope.payload) {
                        continue;
                    }
                    let msg = ServerMsg::Event {
                        subscription_id: subscription_id.clone(),
                        envelope,
                    };
                    if outbound.send(msg).await.is_err() {
                        // WS closed; nothing to forward to. Map cleanup
                        // is handled by the main loop's drain on exit,
                        // but the signal is harmless either way.
                        signal_cleanup();
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "[WS attach] subscription {} lagged ({} events dropped); detaching",
                        subscription_id,
                        n
                    );
                    metrics
                        .forwarder_lagged_count
                        .fetch_add(1, Ordering::Relaxed);
                    let _ = outbound
                        .send(ServerMsg::Detached {
                            subscription_id: subscription_id.clone(),
                            reason: DetachReason::Lagged,
                        })
                        .await;
                    signal_cleanup();
                    return;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    let reason = match binding.as_ref() {
                        Some(binding) => match broker
                            .validate_and_bind_lease(
                                &binding.connection_id,
                                Some(binding.generation),
                                Some(&binding.lease_id),
                            )
                            .await
                        {
                            Err(DetachReason::SessionReplaced) => return,
                            Err(reason) => reason,
                            Ok(_) => DetachReason::ConnectionGone,
                        },
                        None => DetachReason::ConnectionGone,
                    };
                    let _ = outbound
                        .send(ServerMsg::Detached {
                            subscription_id: subscription_id.clone(),
                            reason,
                        })
                        .await;
                    signal_cleanup();
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::session_state::SessionState;
    use crate::acp::tool_watchdog::{
        CancellationScope, ToolCategory, ToolWatchdogPhase, ToolWatchdogProjection,
    };
    use crate::acp::types::{AcpEvent, ConnectionStatus, SessionFailureRecord, ToolCallImageInfo};
    use crate::models::agent::AgentType;

    #[test]
    fn client_message_debug_redacts_lease_id() {
        let message = ClientMsg::Attach {
            subscription_id: "subscription".into(),
            connection_id: "connection".into(),
            generation: Some(7),
            lease_id: Some("lease-secret".into()),
            since_seq: None,
        };

        let debug = format!("{message:?}");
        assert!(!debug.contains("lease-secret"));
        assert!(debug.contains("***"));
    }

    #[test]
    fn serialized_snapshot_frame_never_exceeds_attach_frame_limit() {
        const MIB: usize = 1024 * 1024;
        let mut state = SessionState::new(
            "snapshot-budget".into(),
            AgentType::ClaudeCode,
            None,
            "window".into(),
            None,
        );
        state.status = ConnectionStatus::Prompting;
        state.apply_event(&AcpEvent::ContentDelta {
            text: "l".repeat(MIB),
            parent_tool_use_id: None,
        });
        for index in 0..300 {
            state.apply_event(&AcpEvent::ToolCall {
                tool_call_id: format!("tool-{index:03}"),
                title: "budget fixture".into(),
                kind: "other".into(),
                status: "in_progress".into(),
                content: None,
                raw_input: (index == 0).then(|| format!(r#"{{"payload":"{}"}}"#, "i".repeat(MIB))),
                raw_input_is_model_authored: None,
                raw_output: (index == 0).then(|| "o".repeat(MIB)),
                locations: None,
                meta: None,
                images: Some(vec![ToolCallImageInfo {
                    data: "a".repeat(256),
                    mime_type: "image/png".into(),
                    uri: None,
                }]),
            });
        }
        for index in 0..1000 {
            state.apply_event(&AcpEvent::SessionFailure {
                record: SessionFailureRecord {
                    id: format!("failure-{index:04}"),
                    revision: 1,
                    category: "service".into(),
                    severity: "warning".into(),
                    title: "retry later".into(),
                    details: None,
                    actions: vec!["retry".into()],
                    resolved: false,
                },
            });
            state.apply_event(&AcpEvent::ToolWatchdogChanged {
                projection: ToolWatchdogProjection {
                    lease_id: format!("tombstone-{index:04}"),
                    version: 1,
                    tool_title: ToolCategory::Terminal,
                    phase: ToolWatchdogPhase::TimedOut,
                    last_progress_at: "2026-08-20T00:00:00Z".into(),
                    transition_at: "2026-08-20T00:01:00Z".into(),
                    transition_seq: index as u64,
                    grace_deadline: None,
                    cancellation_scope: Some(CancellationScope::Terminal),
                    error_code: Some("tool_stalled_timeout".into()),
                },
            });
        }

        let snapshot = state.to_snapshot();
        let frame = ServerMsg::Snapshot {
            subscription_id: "subscription".into(),
            connection_id: "snapshot-budget".into(),
            event_seq: snapshot.event_seq,
            snapshot: Box::new(snapshot.clone()),
        };
        let encoded = serde_json::to_vec(&frame).expect("snapshot frame serializes");
        assert!(
            encoded.len() <= crate::acp::session_state::MAX_ATTACH_FRAME_BYTES,
            "snapshot frame {} exceeds {}",
            encoded.len(),
            crate::acp::session_state::MAX_ATTACH_FRAME_BYTES
        );

        let oversized = ServerMsg::Snapshot {
            subscription_id: "s".repeat(crate::acp::session_state::MAX_ATTACH_FRAME_BYTES + 1),
            connection_id: "snapshot-budget".into(),
            snapshot: Box::new(snapshot),
            event_seq: 0,
        };
        let checked = crate::web::ws::serialize_server_msg(&oversized)
            .expect("fallback serializes")
            .expect("oversized snapshot yields attach error");
        assert!(checked.len() <= crate::acp::session_state::MAX_ATTACH_FRAME_BYTES);
        let error: serde_json::Value =
            serde_json::from_slice(&checked).expect("attach error is JSON");
        assert_eq!(error["type"], "attach_error");
        assert_eq!(error["code"], "snapshot_budget_exceeded");

        let mut watchdog_gate = WatchdogEventGate::new(
            std::collections::BTreeMap::from([("terminal-lease".into(), 5)]),
            std::collections::BTreeSet::new(),
        );
        let stale = AcpEvent::ToolWatchdogChanged {
            projection: ToolWatchdogProjection {
                lease_id: "terminal-lease".into(),
                version: 5,
                tool_title: ToolCategory::Terminal,
                phase: ToolWatchdogPhase::Cancelling,
                last_progress_at: "2026-08-20T00:00:00Z".into(),
                transition_at: "2026-08-20T00:01:00Z".into(),
                transition_seq: 1,
                grace_deadline: None,
                cancellation_scope: Some(CancellationScope::Terminal),
                error_code: None,
            },
        };
        assert!(
            !watchdog_gate.allows(&stale),
            "an omitted terminal tombstone must still block its stale producer"
        );
    }
}
