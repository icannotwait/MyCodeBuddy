//! In-process ACP event bus.
//!
//! Carries `Arc<InternalEventEnvelope>` to back-end consumers (lifecycle,
//! pet state mapper, chat-channel subscribers). The public `EventEnvelope`
//! remains shared with private streams / replay / transport; only this bus
//! may retain an optional completion sidecar. Distinct from
//! `WebEventBroadcaster`, which carries `Arc<serde_json::Value>` for
//! transport-bound JSON delivery to WS clients.
//!
//! Two reasons to split the buses:
//!
//! 1. **No JSON parse on the consumer side.** Every back-end subscriber used
//!    to call `serde_json::from_value(payload.clone())` on the broadcaster's
//!    `WebEvent.payload`, paying the parse cost per event per subscriber.
//!    With a typed bus they receive the envelope directly.
//!
//! 2. **No frontend dedup needed.** Before the split, web/remote-desktop WS
//!    clients received `acp://event` from BOTH the per-connection attach
//!    stream AND the global broadcaster firehose, forcing a receiver-side
//!    dedup `Set<connectionId>` on the client. With ACP events removed from
//!    the global broadcaster, the per-connection stream is the sole path
//!    and the dedup goes away.

use std::ops::Deref;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::acp::types::{AcpEvent, ConnectionStatus, EventEnvelope};
use crate::auto_title::TurnCompletionSnapshot;

/// Capacity of the broadcast channel. Sized to the same headroom as
/// `WebEventBroadcaster` (4096) — they observe the same emit rate so the
/// burst tolerance is identical.
const BUS_CAPACITY: usize = 4096;

/// Dedicated lane for lifecycle-critical events (`TurnComplete`,
/// `SessionStarted`, …). Bounded but large: these fire a handful of times
/// per turn, never at ContentDelta rate. When the lifecycle consumer is
/// briefly busy, the lane absorbs the burst without blocking emitters and
/// without competing with the broadcast buffer that ContentDelta floods.
const CRITICAL_LANE_CAPACITY: usize = 1024;

/// Internal-only wrapper around the public event envelope. May carry an
/// immutable turn-completion sidecar for lifecycle title work. Public
/// transport paths never see this type.
#[derive(Debug, Clone)]
pub struct InternalEventEnvelope {
    pub event: Arc<EventEnvelope>,
    pub completion: Option<Arc<TurnCompletionSnapshot>>,
}

impl Deref for InternalEventEnvelope {
    type Target = EventEnvelope;

    fn deref(&self) -> &EventEnvelope {
        self.event.as_ref()
    }
}

/// Whether this payload must reach the lifecycle worker for correctness
/// (status CAS, external_id bind, terminal teardown). Mirrored by
/// `lifecycle::is_lifecycle_relevant` — keep both in sync.
pub fn is_lifecycle_critical(payload: &AcpEvent) -> bool {
    matches!(
        payload,
        AcpEvent::SessionStarted { .. }
            | AcpEvent::TurnComplete { .. }
            | AcpEvent::ConversationLinked { .. }
            | AcpEvent::StatusChanged {
                status: ConnectionStatus::Disconnected
            }
            | AcpEvent::Error { terminal: true, .. }
    )
}

/// Process-wide bus delivering ACP envelopes to in-process consumers.
///
/// Subscribers (lifecycle / pet / chat-channel) call `subscribe()` once at
/// startup and hold the receiver for the lifetime of the process.
/// `emit_with_state` calls `send_with_completion()` after the per-connection
/// stream so the envelope arrives in lockstep with the WS attach delivery.
///
/// Lifecycle-critical events are **also** pushed onto a dedicated mpsc lane
/// (`take_critical_rx`) so a broadcast `Lagged` (ContentDelta flood while the
/// dispatcher was briefly busy) cannot silently drop `TurnComplete` and leave
/// conversation rows stuck at `in_progress`.
#[derive(Debug)]
pub struct InternalEventBus {
    sender: broadcast::Sender<Arc<InternalEventEnvelope>>,
    critical_tx: mpsc::Sender<Arc<InternalEventEnvelope>>,
    /// Taken once by the lifecycle subscriber. `Mutex` so `new` stays sync
    /// and only one consumer can own the receiver.
    critical_rx: Mutex<Option<mpsc::Receiver<Arc<InternalEventEnvelope>>>>,
    metrics: Arc<EventBusMetrics>,
}

impl InternalEventBus {
    pub fn new(metrics: Arc<EventBusMetrics>) -> Self {
        let (sender, _) = broadcast::channel(BUS_CAPACITY);
        let (critical_tx, critical_rx) = mpsc::channel(CRITICAL_LANE_CAPACITY);
        Self {
            sender,
            critical_tx,
            critical_rx: Mutex::new(Some(critical_rx)),
            metrics,
        }
    }

    /// Broadcast a sidecar-free internal event. Used by direct producers and
    /// tests that still push plain public envelopes onto the bus.
    pub fn send(&self, event: Arc<EventEnvelope>) {
        self.send_with_completion(event, None);
    }

    /// Broadcast a public envelope with an optional completion sidecar.
    /// Used only by the shared event-bridge emit core.
    ///
    /// Lifecycle-critical payloads are always mirrored onto the critical
    /// lane (even when there are currently zero broadcast subscribers) so
    /// status CAS cannot depend solely on the lag-prone broadcast path.
    pub fn send_with_completion(
        &self,
        event: Arc<EventEnvelope>,
        completion: Option<Arc<TurnCompletionSnapshot>>,
    ) {
        let critical = is_lifecycle_critical(&event.payload);
        let connection_id = event.connection_id.clone();
        let payload_label = if critical {
            critical_payload_label(&event.payload)
        } else {
            ""
        };

        let internal = Arc::new(InternalEventEnvelope { event, completion });

        let receivers = self.sender.receiver_count();
        if receivers > 0 {
            // SendError can only fire when receiver_count() == 0, which we just
            // checked under the same lock-free atomic. The race window is narrow
            // (a subscriber dropping between the check and the send) and a
            // dropped envelope in that exact window is benign — there's no one
            // to deliver to anyway.
            let _ = self.sender.send(Arc::clone(&internal));
            self.metrics.emitted_count.fetch_add(1, Ordering::Relaxed);
        } else if critical {
            tracing::warn!(
                connection_id = %connection_id,
                event = %payload_label,
                "[ACP][bus] broadcast has 0 subscribers for critical event; \
                 relying on critical lifecycle lane only"
            );
        }

        if critical {
            match self.critical_tx.try_send(Arc::clone(&internal)) {
                Ok(()) => {
                    self.metrics
                        .critical_lane_emit_count
                        .fetch_add(1, Ordering::Relaxed);
                    if matches!(internal.payload, AcpEvent::TurnComplete { .. }) {
                        tracing::info!(
                            connection_id = %connection_id,
                            event = %payload_label,
                            broadcast_receivers = receivers,
                            has_completion_sidecar = internal.completion.is_some(),
                            "[ACP][bus] TurnComplete on critical lifecycle lane"
                        );
                    }
                }
                Err(mpsc::error::TrySendError::Full(env)) => {
                    // Never drop lifecycle-critical envelopes on Full. A
                    // blocked lifecycle consumer must not lose TurnComplete —
                    // spawn a deliverer that waits (no timeout) for capacity.
                    self.metrics
                        .critical_lane_full_count
                        .fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        connection_id = %connection_id,
                        event = %payload_label,
                        "[ACP][bus][ERROR] critical lifecycle lane FULL — \
                         spawning unbounded overflow deliverer (will not drop \
                         TurnComplete/SessionStarted)"
                    );
                    let tx = self.critical_tx.clone();
                    let conn = connection_id.clone();
                    let label = payload_label;
                    // Must not panic if called outside a runtime (emit paths
                    // are normally on Tokio; this is belt-and-suspenders).
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => {
                            handle.spawn(async move {
                                match tx.send(env).await {
                                    Ok(()) => {
                                        tracing::info!(
                                            connection_id = %conn,
                                            event = %label,
                                            "[ACP][bus] critical lane overflow deliver succeeded"
                                        );
                                    }
                                    Err(_) => {
                                        tracing::error!(
                                            connection_id = %conn,
                                            event = %label,
                                            "[ACP][bus][ERROR] critical lifecycle lane CLOSED \
                                             during overflow deliver — status CAS will not run"
                                        );
                                    }
                                }
                            });
                        }
                        Err(_) => {
                            tracing::error!(
                                connection_id = %connection_id,
                                event = %payload_label,
                                "[ACP][bus][ERROR] critical lane FULL and no Tokio runtime \
                                 for overflow deliver — event may be lost"
                            );
                        }
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    tracing::error!(
                        connection_id = %connection_id,
                        event = %payload_label,
                        "[ACP][bus][ERROR] critical lifecycle lane CLOSED — \
                         no lifecycle consumer; status CAS will not run"
                    );
                }
            }
        }
    }

    /// Subscribe to the bus. The returned receiver buffers up to
    /// `BUS_CAPACITY` events behind the slowest subscriber; if it falls
    /// further behind, the next `recv()` returns `RecvError::Lagged(n)`
    /// and bumps `lagged_count`.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<InternalEventEnvelope>> {
        self.sender.subscribe()
    }

    /// Take the once-only critical-lane receiver. Lifecycle must call this
    /// at subscriber start; subsequent calls return `None`.
    pub fn take_critical_rx(&self) -> Option<mpsc::Receiver<Arc<InternalEventEnvelope>>> {
        self.critical_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    pub fn metrics(&self) -> &Arc<EventBusMetrics> {
        &self.metrics
    }
}

fn critical_payload_label(payload: &AcpEvent) -> &'static str {
    match payload {
        AcpEvent::TurnComplete { .. } => "TurnComplete",
        AcpEvent::SessionStarted { .. } => "SessionStarted",
        AcpEvent::ConversationLinked { .. } => "ConversationLinked",
        AcpEvent::StatusChanged {
            status: ConnectionStatus::Disconnected,
        } => "StatusChanged(Disconnected)",
        AcpEvent::Error { terminal: true, .. } => "Error(terminal)",
        _ => "critical",
    }
}

/// Counters surfaced on the `/debug/event_metrics` HTTP endpoint and via
/// shutdown logs. Kept as plain `AtomicU64` to avoid pulling in a metrics
/// framework — load is low, the only consumers are operators tailing logs
/// or fetching the debug endpoint.
#[derive(Debug, Default)]
pub struct EventBusMetrics {
    /// Envelopes pushed onto `InternalEventBus`. Tracks emit volume.
    pub emitted_count: AtomicU64,
    /// `RecvError::Lagged(n)` occurrences across all subscribers — sum of
    /// dropped-events `n`. Spike means a subscriber is behind on DB writes
    /// or otherwise too slow.
    pub lagged_count: AtomicU64,
    /// Envelopes evicted from a per-connection `RecentEventsBuffer`
    /// (FIFO trim by either count cap or byte cap). Drives the snapshot-vs-
    /// replay decision when an attach-with-cursor lands too late.
    pub ring_buffer_evict_count: AtomicU64,
    /// Attach decisions: client supplied a cursor that fell within the ring
    /// buffer and was small enough to batch. Tracks happy-path resync.
    pub replay_count: AtomicU64,
    /// Sum of envelope counts across all replay batches. Average batch size
    /// = `replay_event_total / replay_count`, useful for sizing
    /// `REPLAY_BATCH_THRESHOLD`.
    pub replay_event_total: AtomicU64,
    /// Attach decisions: client supplied a cursor that fell outside the
    /// ring buffer (or buffer was too large to batch), so the server fell
    /// back to a full snapshot. High rate suggests buffer caps need lifting.
    pub snapshot_fallback_count: AtomicU64,
    /// Attach decisions: client requested a snapshot explicitly (no cursor).
    /// Cold-start frontends + post-disconnect re-attaches with no preserved
    /// state.
    pub snapshot_cold_count: AtomicU64,
    /// Per-attach forwarder tasks that exited with `Lagged`. Each one
    /// triggers a client re-attach (and therefore a snapshot or replay).
    pub forwarder_lagged_count: AtomicU64,
    /// Lifecycle dispatcher observed a per-connection worker mailbox at
    /// capacity. The dispatcher no longer blocks — it spawns an overflow
    /// deliverer — but the counter still marks chronic worker stalls
    /// (typically SQLite contention). Distinct from `lagged_count`.
    pub worker_queue_full_count: AtomicU64,
    /// Critical lifecycle-lane emits (`TurnComplete` / `SessionStarted` / …).
    pub critical_lane_emit_count: AtomicU64,
    /// Critical lane `try_send` found the buffer full.
    pub critical_lane_full_count: AtomicU64,

    // --- Desktop ACP delivery observability (Tauri webview leg only) ---
    /// Envelopes offered to the desktop delivery path (legacy emit or batcher).
    pub desktop_raw_envelope_count: AtomicU64,
    /// Estimated serialized bytes of envelopes offered to desktop delivery.
    pub desktop_raw_bytes: AtomicU64,
    /// Desktop emit attempts (legacy single-event or batch flush).
    pub desktop_emit_attempt_count: AtomicU64,
    /// Failures while estimating payload size (serde); payload contents never logged.
    pub desktop_serialization_failure_count: AtomicU64,
    /// Desktop emit failures (`app.emit` / batch emit errors).
    pub desktop_emit_failure_count: AtomicU64,
    /// Successful single-event legacy `app.emit("acp://event", …)` counts.
    pub desktop_legacy_emit_count: AtomicU64,
    /// Batches flushed to the webview (P1+; remains zero under P0 legacy).
    pub desktop_batch_count: AtomicU64,
    /// Sum of event counts across all flushed batches.
    pub desktop_batch_event_count: AtomicU64,
    /// Sum of estimated bytes across all flushed batches.
    pub desktop_batch_bytes: AtomicU64,
    /// Max events observed in a single batch (`fetch_max`).
    pub desktop_batch_max_events: AtomicU64,
    /// Max estimated bytes observed in a single batch (`fetch_max`).
    pub desktop_batch_max_bytes: AtomicU64,
    /// Sum of batch flush latencies in microseconds.
    pub desktop_batch_latency_total_us: AtomicU64,
    /// Max batch flush latency in microseconds (`fetch_max`).
    pub desktop_batch_latency_max_us: AtomicU64,
    /// Desktop batcher queue full / backpressure observations.
    pub desktop_queue_full_count: AtomicU64,
    /// Startup fell back from batched to legacy delivery.
    pub desktop_startup_fallback_count: AtomicU64,
    /// Runtime delivery failures that forced fallback / failure events.
    pub desktop_runtime_failure_count: AtomicU64,
}

impl EventBusMetrics {
    /// Record one envelope offered to the desktop delivery path.
    pub fn record_desktop_offer(&self, estimated_bytes: usize) {
        self.desktop_raw_envelope_count
            .fetch_add(1, Ordering::Relaxed);
        self.desktop_raw_bytes
            .fetch_add(estimated_bytes as u64, Ordering::Relaxed);
    }

    /// Record one flushed desktop batch (event count, bytes, flush latency).
    pub fn record_desktop_batch(&self, events: usize, bytes: usize, latency: Duration) {
        self.desktop_batch_count.fetch_add(1, Ordering::Relaxed);
        self.desktop_batch_event_count
            .fetch_add(events as u64, Ordering::Relaxed);
        self.desktop_batch_bytes
            .fetch_add(bytes as u64, Ordering::Relaxed);
        self.desktop_batch_max_events
            .fetch_max(events as u64, Ordering::Relaxed);
        self.desktop_batch_max_bytes
            .fetch_max(bytes as u64, Ordering::Relaxed);
        let latency_us = latency.as_micros().min(u64::MAX as u128) as u64;
        self.desktop_batch_latency_total_us
            .fetch_add(latency_us, Ordering::Relaxed);
        self.desktop_batch_latency_max_us
            .fetch_max(latency_us, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> EventBusMetricsSnapshot {
        EventBusMetricsSnapshot {
            emitted_count: self.emitted_count.load(Ordering::Relaxed),
            lagged_count: self.lagged_count.load(Ordering::Relaxed),
            ring_buffer_evict_count: self.ring_buffer_evict_count.load(Ordering::Relaxed),
            replay_count: self.replay_count.load(Ordering::Relaxed),
            replay_event_total: self.replay_event_total.load(Ordering::Relaxed),
            snapshot_fallback_count: self.snapshot_fallback_count.load(Ordering::Relaxed),
            snapshot_cold_count: self.snapshot_cold_count.load(Ordering::Relaxed),
            forwarder_lagged_count: self.forwarder_lagged_count.load(Ordering::Relaxed),
            worker_queue_full_count: self.worker_queue_full_count.load(Ordering::Relaxed),
            critical_lane_emit_count: self.critical_lane_emit_count.load(Ordering::Relaxed),
            critical_lane_full_count: self.critical_lane_full_count.load(Ordering::Relaxed),
            desktop_raw_envelope_count: self.desktop_raw_envelope_count.load(Ordering::Relaxed),
            desktop_raw_bytes: self.desktop_raw_bytes.load(Ordering::Relaxed),
            desktop_emit_attempt_count: self.desktop_emit_attempt_count.load(Ordering::Relaxed),
            desktop_serialization_failure_count: self
                .desktop_serialization_failure_count
                .load(Ordering::Relaxed),
            desktop_emit_failure_count: self.desktop_emit_failure_count.load(Ordering::Relaxed),
            desktop_legacy_emit_count: self.desktop_legacy_emit_count.load(Ordering::Relaxed),
            desktop_batch_count: self.desktop_batch_count.load(Ordering::Relaxed),
            desktop_batch_event_count: self.desktop_batch_event_count.load(Ordering::Relaxed),
            desktop_batch_bytes: self.desktop_batch_bytes.load(Ordering::Relaxed),
            desktop_batch_max_events: self.desktop_batch_max_events.load(Ordering::Relaxed),
            desktop_batch_max_bytes: self.desktop_batch_max_bytes.load(Ordering::Relaxed),
            desktop_batch_latency_total_us: self
                .desktop_batch_latency_total_us
                .load(Ordering::Relaxed),
            desktop_batch_latency_max_us: self.desktop_batch_latency_max_us.load(Ordering::Relaxed),
            desktop_queue_full_count: self.desktop_queue_full_count.load(Ordering::Relaxed),
            desktop_startup_fallback_count: self
                .desktop_startup_fallback_count
                .load(Ordering::Relaxed),
            desktop_runtime_failure_count: self
                .desktop_runtime_failure_count
                .load(Ordering::Relaxed),
        }
    }
}

/// JSON-serializable view of `EventBusMetrics` for the debug HTTP endpoint.
/// Plain `u64` so the response is stable JSON — atomic types serialize
/// erratically across serde-versions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EventBusMetricsSnapshot {
    pub emitted_count: u64,
    pub lagged_count: u64,
    pub ring_buffer_evict_count: u64,
    pub replay_count: u64,
    pub replay_event_total: u64,
    pub snapshot_fallback_count: u64,
    pub snapshot_cold_count: u64,
    pub forwarder_lagged_count: u64,
    pub worker_queue_full_count: u64,
    pub critical_lane_emit_count: u64,
    pub critical_lane_full_count: u64,
    pub desktop_raw_envelope_count: u64,
    pub desktop_raw_bytes: u64,
    pub desktop_emit_attempt_count: u64,
    pub desktop_serialization_failure_count: u64,
    pub desktop_emit_failure_count: u64,
    pub desktop_legacy_emit_count: u64,
    pub desktop_batch_count: u64,
    pub desktop_batch_event_count: u64,
    pub desktop_batch_bytes: u64,
    pub desktop_batch_max_events: u64,
    pub desktop_batch_max_bytes: u64,
    pub desktop_batch_latency_total_us: u64,
    pub desktop_batch_latency_max_us: u64,
    pub desktop_queue_full_count: u64,
    pub desktop_startup_fallback_count: u64,
    pub desktop_runtime_failure_count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::types::AcpEvent;

    fn fake_envelope(seq: u64) -> Arc<EventEnvelope> {
        Arc::new(EventEnvelope {
            seq,
            connection_id: "c1".into(),
            payload: AcpEvent::ContentDelta { text: "x".into() },
        })
    }

    #[tokio::test]
    async fn send_with_no_subscribers_is_noop_and_does_not_count() {
        // No-receiver fast path must not bump emitted_count — the metric
        // tracks delivered emit attempts, not orphaned ones (otherwise a
        // process with no UI would still rack up emits during agent runs).
        let metrics = Arc::new(EventBusMetrics::default());
        let bus = InternalEventBus::new(metrics.clone());
        bus.send(fake_envelope(1));
        assert_eq!(metrics.emitted_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn send_delivers_to_all_subscribers_and_counts_once() {
        let metrics = Arc::new(EventBusMetrics::default());
        let bus = InternalEventBus::new(metrics.clone());
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();

        bus.send(fake_envelope(7));
        let e1 = rx1.recv().await.unwrap();
        let e2 = rx2.recv().await.unwrap();
        assert_eq!(e1.seq, 7);
        assert_eq!(e2.seq, 7);
        // Same Arc — broadcast clones the handle, not the payload.
        assert!(Arc::ptr_eq(&e1, &e2));
        assert_eq!(metrics.emitted_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn turn_complete_hits_critical_lane_even_without_broadcast_subscribers() {
        let metrics = Arc::new(EventBusMetrics::default());
        let bus = InternalEventBus::new(metrics.clone());
        let mut critical = bus.take_critical_rx().expect("critical rx");
        assert!(bus.take_critical_rx().is_none(), "critical rx is once-only");

        let env = Arc::new(EventEnvelope {
            seq: 9,
            connection_id: "c-crit".into(),
            payload: AcpEvent::TurnComplete {
                session_id: "s".into(),
                stop_reason: "end_turn".into(),
                agent_type: "codex".into(),
                mark_awaiting_reply: true,
            },
        });
        // No broadcast subscriber — previous code would no-op entirely.
        bus.send_with_completion(env, None);

        let got = tokio::time::timeout(Duration::from_secs(1), critical.recv())
            .await
            .expect("critical recv timed out")
            .expect("critical closed");
        assert_eq!(got.connection_id, "c-crit");
        assert!(matches!(got.payload, AcpEvent::TurnComplete { .. }));
        assert_eq!(metrics.critical_lane_emit_count.load(Ordering::Relaxed), 1);
        // Broadcast had no receivers — emitted_count stays 0.
        assert_eq!(metrics.emitted_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn content_delta_does_not_use_critical_lane() {
        let metrics = Arc::new(EventBusMetrics::default());
        let bus = InternalEventBus::new(metrics.clone());
        let mut critical = bus.take_critical_rx().expect("critical rx");
        let _broadcast = bus.subscribe();
        bus.send(fake_envelope(1));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), critical.recv())
                .await
                .is_err(),
            "ContentDelta must not enter the critical lane"
        );
        assert_eq!(metrics.critical_lane_emit_count.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn metrics_snapshot_returns_loaded_values() {
        let metrics = Arc::new(EventBusMetrics::default());
        metrics.emitted_count.store(42, Ordering::Relaxed);
        metrics.lagged_count.store(3, Ordering::Relaxed);
        metrics.snapshot_fallback_count.store(1, Ordering::Relaxed);
        let snap = metrics.snapshot();
        assert_eq!(snap.emitted_count, 42);
        assert_eq!(snap.lagged_count, 3);
        assert_eq!(snap.snapshot_fallback_count, 1);
    }

    #[test]
    fn metrics_snapshot_includes_desktop_delivery_counters() {
        let metrics = EventBusMetrics::default();
        metrics
            .desktop_raw_envelope_count
            .store(9, Ordering::Relaxed);
        metrics.desktop_raw_bytes.store(4_096, Ordering::Relaxed);
        metrics
            .desktop_emit_failure_count
            .store(2, Ordering::Relaxed);
        metrics
            .desktop_batch_max_events
            .store(17, Ordering::Relaxed);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.desktop_raw_envelope_count, 9);
        assert_eq!(snapshot.desktop_raw_bytes, 4_096);
        assert_eq!(snapshot.desktop_emit_failure_count, 2);
        assert_eq!(snapshot.desktop_batch_max_events, 17);
    }
}
