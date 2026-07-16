//! Desktop ACP event batching for the local Tauri webview leg.
//!
//! Bounded queue + single worker that flushes on the first of:
//! 16 ms, 128 envelopes, 64 KiB estimated bytes, a flush-sensitive control
//! event, or shutdown. Server/WebOnly paths remain per-envelope.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::acp::internal_bus::EventBusMetrics;
use crate::acp::streaming_performance::{
    DesktopDeliveryCapabilities, DesktopDeliveryMode, StreamingPerformanceFlags,
};
use crate::acp::types::{AcpEvent, EventEnvelope};

/// Spawn a future on the current Tokio handle when present (unit tests),
/// otherwise on Tauri's global async runtime (desktop `.setup` has no
/// current handle — plain `tokio::spawn` panics there).
fn spawn_on_available_runtime<F>(future: F) -> JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle.spawn(future),
        Err(_) => {
            #[cfg(feature = "tauri-runtime")]
            {
                tauri::async_runtime::handle().inner().spawn(future)
            }
            #[cfg(not(feature = "tauri-runtime"))]
            {
                // Server/lib builds never construct the desktop batcher; this
                // arm exists only so the helper type-checks without tauri.
                tokio::spawn(future)
            }
        }
    }
}

/// Bounded Tokio channel capacity for queued envelopes.
pub(crate) const QUEUE_CAPACITY: usize = 1_024;
/// Max envelopes per batch before a count-triggered flush.
pub(crate) const MAX_BATCH_EVENTS: usize = 128;
/// Max estimated serialized bytes per batch before a byte-triggered flush.
pub(crate) const MAX_BATCH_BYTES: usize = 64 * 1024;
/// Max age of the first pending envelope before a timer-triggered flush.
pub(crate) const MAX_BATCH_DELAY: Duration = Duration::from_millis(16);

/// Event name for batched desktop delivery.
pub const EVENT_BATCH: &str = "acp://event-batch";
/// Event name for legacy single-envelope desktop delivery.
pub const EVENT_LEGACY: &str = "acp://event";
/// Event name for terminal desktop delivery failure.
pub const EVENT_DELIVERY_FAILED: &str = "acp://delivery-failed";

/// Wire shape: one flushed batch of unmodified envelopes.
#[derive(Debug, Clone, Serialize)]
pub struct DesktopAcpEventBatch {
    pub batch_id: u64,
    pub events: Vec<EventEnvelope>,
}

/// Wire shape: inclusive outstanding seq range for one connection after failure.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DesktopConnectionSeqRange {
    pub connection_id: String,
    pub first_seq: u64,
    pub last_seq: u64,
}

/// Wire shape: terminal delivery failure (ranges + reason only; no content).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DesktopDeliveryFailure {
    pub generation: u64,
    pub reason: &'static str,
    pub affected: Vec<DesktopConnectionSeqRange>,
}

/// Errors from desktop enqueue / deliver / shutdown.
#[derive(Debug, thiserror::Error)]
pub enum DesktopDeliveryError {
    #[error("desktop ACP delivery stopped")]
    Stopped,
}

/// Sink that actually reaches the webview (or a test double).
#[async_trait]
pub trait DesktopBatchSink: Send + Sync {
    async fn emit_batch(&self, batch: &DesktopAcpEventBatch) -> Result<(), String>;
    async fn emit_failure(&self, failure: &DesktopDeliveryFailure) -> Result<(), String>;
}

enum QueueMessage {
    Event(QueuedEnvelope),
    Shutdown(oneshot::Sender<()>),
}

struct QueuedEnvelope {
    envelope: Arc<EventEnvelope>,
    estimated_bytes: usize,
    queued_at: Instant,
}

/// Tracks every envelope accepted into the desktop path until successfully
/// emitted — used only for snapshot-recovery ranges after terminal failure.
#[derive(Default)]
struct OutstandingEnvelopes(Mutex<BTreeMap<String, BTreeSet<u64>>>);

impl OutstandingEnvelopes {
    fn insert(&self, connection_id: &str, seq: u64) {
        self.0
            .lock()
            .unwrap()
            .entry(connection_id.to_owned())
            .or_default()
            .insert(seq);
    }

    fn remove(&self, connection_id: &str, seq: u64) {
        let mut entries = self.0.lock().unwrap();
        if let Some(sequences) = entries.get_mut(connection_id) {
            sequences.remove(&seq);
            if sequences.is_empty() {
                entries.remove(connection_id);
            }
        }
    }

    fn remove_emitted(&self, events: &[EventEnvelope]) {
        for event in events {
            self.remove(&event.connection_id, event.seq);
        }
    }

    fn connection_seq_ranges(&self) -> Vec<DesktopConnectionSeqRange> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(connection_id, sequences)| {
                Some(DesktopConnectionSeqRange {
                    connection_id: connection_id.clone(),
                    first_seq: *sequences.first()?,
                    last_seq: *sequences.last()?,
                })
            })
            .collect()
    }
}

/// Permission / question / completion / error force an immediate flush.
/// Tool calls and tool updates are intentionally excluded.
pub(crate) fn is_flush_sensitive(event: &AcpEvent) -> bool {
    matches!(
        event,
        AcpEvent::PermissionRequest { .. }
            | AcpEvent::QuestionRequest { .. }
            | AcpEvent::TurnComplete { .. }
            | AcpEvent::Error { .. }
            | AcpEvent::SessionLoadFailed { .. }
    )
}

/// Single-worker batcher with a bounded queue and flush policy.
pub struct DesktopAcpEventBatcher {
    sender: mpsc::Sender<QueueMessage>,
    metrics: Arc<EventBusMetrics>,
    failed: Arc<AtomicBool>,
    outstanding: Arc<OutstandingEnvelopes>,
    orderly_shutdown: Arc<AtomicBool>,
    /// Supervisor task (waits on the worker). Joined once during shutdown.
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl DesktopAcpEventBatcher {
    /// Spawn the batch worker + supervisor against `sink`.
    pub fn start(sink: Arc<dyn DesktopBatchSink>, metrics: Arc<EventBusMetrics>) -> Self {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let failed = Arc::new(AtomicBool::new(false));
        let outstanding = Arc::new(OutstandingEnvelopes::default());
        let orderly_shutdown = Arc::new(AtomicBool::new(false));
        let failure_signaled = Arc::new(AtomicBool::new(false));

        let worker_sink = Arc::clone(&sink);
        let worker_metrics = Arc::clone(&metrics);
        let worker_failed = Arc::clone(&failed);
        let worker_outstanding = Arc::clone(&outstanding);
        let worker_failure_signaled = Arc::clone(&failure_signaled);

        // Tauri `.setup` runs outside a current Tokio handle; unit tests
        // (#[tokio::test]) already have one. Prefer the current handle so
        // paused-clock tests stay on the same runtime.
        let worker = spawn_on_available_runtime(async move {
            run_batcher(
                receiver,
                worker_sink,
                worker_metrics,
                worker_failed,
                worker_outstanding,
                worker_failure_signaled,
            )
            .await;
        });

        let sup_sink = Arc::clone(&sink);
        let sup_metrics = Arc::clone(&metrics);
        let sup_failed = Arc::clone(&failed);
        let sup_outstanding = Arc::clone(&outstanding);
        let sup_orderly = Arc::clone(&orderly_shutdown);
        let sup_failure_signaled = Arc::clone(&failure_signaled);

        let supervisor = spawn_on_available_runtime(async move {
            // Worker result is only used to distinguish panic vs clean exit.
            let worker_result = worker.await;
            if sup_orderly.load(Ordering::Acquire) {
                return;
            }
            // Already signaled by a failed flush (batch_emit_failed).
            if sup_failure_signaled.swap(true, Ordering::AcqRel) {
                return;
            }
            sup_failed.store(true, Ordering::Release);
            sup_metrics
                .desktop_runtime_failure_count
                .fetch_add(1, Ordering::Relaxed);
            let _ = worker_result;
            let failure = DesktopDeliveryFailure {
                generation: 0,
                reason: "batch_task_stopped",
                affected: sup_outstanding.connection_seq_ranges(),
            };
            tracing::error!(
                "[ACP] desktop batch task stopped: reason={} affected_connections={}",
                failure.reason,
                failure.affected.len()
            );
            let _ = sup_sink.emit_failure(&failure).await;
        });

        // `failure_signaled` is owned by the worker + supervisor only.
        let _ = failure_signaled;
        Self {
            sender,
            metrics,
            failed,
            outstanding,
            orderly_shutdown,
            supervisor: Mutex::new(Some(supervisor)),
        }
    }

    pub fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    pub async fn enqueue(
        &self,
        envelope: Arc<EventEnvelope>,
        estimated_bytes: usize,
    ) -> Result<(), DesktopDeliveryError> {
        if self.failed.load(Ordering::Acquire) {
            return Err(DesktopDeliveryError::Stopped);
        }
        let connection_id = envelope.connection_id.clone();
        let seq = envelope.seq;
        self.outstanding.insert(&connection_id, seq);
        let message = QueueMessage::Event(QueuedEnvelope {
            envelope,
            estimated_bytes,
            queued_at: Instant::now(),
        });
        match self.sender.try_send(message) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(message)) => {
                self.metrics
                    .desktop_queue_full_count
                    .fetch_add(1, Ordering::Relaxed);
                if self.sender.send(message).await.is_ok() {
                    Ok(())
                } else {
                    self.outstanding.remove(&connection_id, seq);
                    Err(DesktopDeliveryError::Stopped)
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.outstanding.remove(&connection_id, seq);
                Err(DesktopDeliveryError::Stopped)
            }
        }
    }

    /// Drain pending events, stop the worker, and join the supervisor.
    /// Idempotent: concurrent/duplicate callers wait for the same drain.
    pub async fn shutdown(&self) -> Result<(), DesktopDeliveryError> {
        // Mark orderly first so the supervisor never emits batch_task_stopped.
        self.orderly_shutdown.store(true, Ordering::Release);

        let (done_tx, done_rx) = oneshot::channel();
        match self.sender.send(QueueMessage::Shutdown(done_tx)).await {
            Ok(()) => {
                let _ = done_rx.await;
            }
            Err(_) => {
                // Channel already closed (worker exited). Fall through to join.
            }
        }

        let handle = self.supervisor.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
        Ok(())
    }
}

async fn run_batcher(
    mut receiver: mpsc::Receiver<QueueMessage>,
    sink: Arc<dyn DesktopBatchSink>,
    metrics: Arc<EventBusMetrics>,
    failed: Arc<AtomicBool>,
    outstanding: Arc<OutstandingEnvelopes>,
    failure_signaled: Arc<AtomicBool>,
) {
    let mut pending: Vec<QueuedEnvelope> = Vec::with_capacity(MAX_BATCH_EVENTS);
    let mut pending_bytes = 0usize;
    let mut next_batch_id = 0u64;

    loop {
        let message = if pending.is_empty() {
            receiver.recv().await
        } else {
            let deadline = pending[0].queued_at + MAX_BATCH_DELAY;
            tokio::select! {
                value = receiver.recv() => value,
                _ = sleep_until(deadline) => {
                    if flush(
                        &mut pending,
                        &mut pending_bytes,
                        &mut next_batch_id,
                        sink.as_ref(),
                        &metrics,
                        &outstanding,
                        &failed,
                        &failure_signaled,
                        &mut receiver,
                    ).await.is_err() {
                        return;
                    }
                    continue;
                }
            }
        };

        match message {
            Some(QueueMessage::Event(queued)) => {
                let flush_sensitive = is_flush_sensitive(&queued.envelope.payload);
                pending_bytes = pending_bytes.saturating_add(queued.estimated_bytes);
                pending.push(queued);
                let should_flush = pending.len() >= MAX_BATCH_EVENTS
                    || pending_bytes >= MAX_BATCH_BYTES
                    || flush_sensitive;
                if should_flush
                    && flush(
                        &mut pending,
                        &mut pending_bytes,
                        &mut next_batch_id,
                        sink.as_ref(),
                        &metrics,
                        &outstanding,
                        &failed,
                        &failure_signaled,
                        &mut receiver,
                    )
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Some(QueueMessage::Shutdown(done)) => {
                let _ = flush(
                    &mut pending,
                    &mut pending_bytes,
                    &mut next_batch_id,
                    sink.as_ref(),
                    &metrics,
                    &outstanding,
                    &failed,
                    &failure_signaled,
                    &mut receiver,
                )
                .await;
                let _ = done.send(());
                return;
            }
            None => {
                let _ = flush(
                    &mut pending,
                    &mut pending_bytes,
                    &mut next_batch_id,
                    sink.as_ref(),
                    &metrics,
                    &outstanding,
                    &failed,
                    &failure_signaled,
                    &mut receiver,
                )
                .await;
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn flush(
    pending: &mut Vec<QueuedEnvelope>,
    pending_bytes: &mut usize,
    next_batch_id: &mut u64,
    sink: &dyn DesktopBatchSink,
    metrics: &EventBusMetrics,
    outstanding: &OutstandingEnvelopes,
    failed: &AtomicBool,
    failure_signaled: &AtomicBool,
    receiver: &mut mpsc::Receiver<QueueMessage>,
) -> Result<(), String> {
    if pending.is_empty() {
        return Ok(());
    }
    let latency = pending[0].queued_at.elapsed();
    let events = pending
        .drain(..)
        .map(|queued| queued.envelope.as_ref().clone())
        .collect::<Vec<_>>();
    let bytes = std::mem::take(pending_bytes);
    *next_batch_id += 1;
    let batch = DesktopAcpEventBatch {
        batch_id: *next_batch_id,
        events,
    };
    metrics
        .desktop_emit_attempt_count
        .fetch_add(1, Ordering::Relaxed);
    match sink.emit_batch(&batch).await {
        Ok(()) => {
            metrics.record_desktop_batch(batch.events.len(), bytes, latency);
            outstanding.remove_emitted(&batch.events);
            Ok(())
        }
        Err(error) => {
            metrics
                .desktop_emit_failure_count
                .fetch_add(1, Ordering::Relaxed);
            metrics
                .desktop_runtime_failure_count
                .fetch_add(1, Ordering::Relaxed);
            // Reject new producers *before* snapshotting recovery ranges so a
            // concurrent enqueue cannot insert into outstanding after the
            // snapshot and then be dropped when the receiver is closed.
            failed.store(true, Ordering::Release);
            receiver.close();
            // Claim the one-shot failure signal before emit so the supervisor
            // cannot double-report.
            failure_signaled.store(true, Ordering::Release);
            // Snapshot after fail-closed; re-snapshot immediately before emit
            // to include any insert that raced past the failed load but has
            // not yet observed Closed and removed itself.
            let _ = outstanding.connection_seq_ranges();
            let failure = DesktopDeliveryFailure {
                generation: *next_batch_id,
                reason: "batch_emit_failed",
                affected: outstanding.connection_seq_ranges(),
            };
            tracing::error!(
                "[ACP] desktop batch emit failed: reason={} generation={} affected_connections={}",
                failure.reason,
                failure.generation,
                failure.affected.len()
            );
            let _ = sink.emit_failure(&failure).await;
            Err(error)
        }
    }
}

// ---------------------------------------------------------------------------
// Desktop delivery owner (legacy vs batched, selected once at startup)
// ---------------------------------------------------------------------------

/// Owns the desktop ACP delivery mode for the process lifetime.
///
/// Mode is chosen once at startup from normalized flags; never hot-switched.
pub struct DesktopAcpDelivery {
    mode: DesktopDeliveryMode,
    flags: StreamingPerformanceFlags,
    metrics: Arc<EventBusMetrics>,
    #[cfg(feature = "tauri-runtime")]
    app: tauri::AppHandle,
    batcher: Option<Arc<DesktopAcpEventBatcher>>,
    shut_down: AtomicBool,
}

impl DesktopAcpDelivery {
    /// Start delivery with the given flags. Falls back to legacy if batching
    /// is requested but the worker cannot be started.
    #[cfg(feature = "tauri-runtime")]
    pub fn start(
        app: tauri::AppHandle,
        metrics: Arc<EventBusMetrics>,
        flags: StreamingPerformanceFlags,
    ) -> Arc<Self> {
        let flags = flags.normalized();
        if flags.desktop_acp_event_batching {
            let sink: Arc<dyn DesktopBatchSink> = Arc::new(TauriBatchSink {
                app: app.clone(),
            });
            let batcher = Arc::new(DesktopAcpEventBatcher::start(sink, Arc::clone(&metrics)));
            return Arc::new(Self {
                mode: DesktopDeliveryMode::Batched,
                flags,
                metrics,
                app,
                batcher: Some(batcher),
                shut_down: AtomicBool::new(false),
            });
        }
        Arc::new(Self::legacy_inner(app, metrics, flags))
    }

    /// Test/helper path: force legacy mode and record a startup fallback.
    /// Dependent flags are forced off so capabilities never advertise batching.
    #[cfg(feature = "tauri-runtime")]
    pub fn start_legacy_fallback(
        app: tauri::AppHandle,
        metrics: Arc<EventBusMetrics>,
        requested: StreamingPerformanceFlags,
    ) -> Arc<Self> {
        metrics
            .desktop_startup_fallback_count
            .fetch_add(1, Ordering::Relaxed);
        let _ = requested;
        Arc::new(Self::legacy_inner(
            app,
            metrics,
            StreamingPerformanceFlags::legacy(),
        ))
    }

    #[cfg(feature = "tauri-runtime")]
    fn legacy_inner(
        app: tauri::AppHandle,
        metrics: Arc<EventBusMetrics>,
        flags: StreamingPerformanceFlags,
    ) -> Self {
        // Guarantee dependents cannot outrank a legacy mode selection.
        let flags = StreamingPerformanceFlags {
            desktop_acp_event_batching: false,
            ..flags.normalized()
        }
        .normalized();
        Self {
            mode: DesktopDeliveryMode::Legacy,
            flags,
            metrics,
            app,
            batcher: None,
            shut_down: AtomicBool::new(false),
        }
    }

    /// Pure capability snapshot for the active mode (no AppHandle required).
    pub fn capabilities_for(
        mode: DesktopDeliveryMode,
        flags: StreamingPerformanceFlags,
    ) -> DesktopDeliveryCapabilities {
        let flags = match mode {
            DesktopDeliveryMode::Legacy => StreamingPerformanceFlags {
                desktop_acp_event_batching: false,
                ..flags.normalized()
            }
            .normalized(),
            DesktopDeliveryMode::Batched => flags.normalized(),
        };
        DesktopDeliveryCapabilities {
            mode,
            flags,
            perf_replay_available: cfg!(feature = "test-utils"),
            failure_event: DesktopDeliveryCapabilities::FAILURE_EVENT,
        }
    }

    pub fn capabilities(&self) -> DesktopDeliveryCapabilities {
        Self::capabilities_for(self.mode, self.flags.clone())
    }

    pub fn mode(&self) -> DesktopDeliveryMode {
        self.mode
    }

    /// Deliver one envelope on the desktop path. Does **not** record offer —
    /// the caller records `record_desktop_offer` exactly once before calling.
    pub async fn deliver(
        &self,
        envelope: Arc<EventEnvelope>,
        estimated_bytes: usize,
    ) -> Result<(), DesktopDeliveryError> {
        match self.mode {
            DesktopDeliveryMode::Batched => {
                let Some(batcher) = self.batcher.as_ref() else {
                    return Err(DesktopDeliveryError::Stopped);
                };
                batcher.enqueue(envelope, estimated_bytes).await
            }
            DesktopDeliveryMode::Legacy => {
                #[cfg(feature = "tauri-runtime")]
                {
                    emit_legacy(&self.app, envelope.as_ref(), Some(Arc::clone(&self.metrics)));
                    Ok(())
                }
                #[cfg(not(feature = "tauri-runtime"))]
                {
                    let _ = (envelope, estimated_bytes);
                    Err(DesktopDeliveryError::Stopped)
                }
            }
        }
    }

    /// Drain the batcher (if any). Idempotent.
    pub async fn shutdown(&self) -> Result<(), DesktopDeliveryError> {
        if self.shut_down.swap(true, Ordering::AcqRel) {
            // Already shut down (or another caller is finishing). If a batcher
            // is present, wait for its supervisor join via a second shutdown
            // call which is itself idempotent on the join handle.
            if let Some(batcher) = self.batcher.as_ref() {
                return batcher.shutdown().await;
            }
            return Ok(());
        }
        if let Some(batcher) = self.batcher.as_ref() {
            batcher.shutdown().await?;
        }
        Ok(())
    }
}

/// Legacy single-event `app.emit("acp://event", …)`. Increments attempt /
/// success / failure counters only — never records a desktop offer.
#[cfg(feature = "tauri-runtime")]
pub fn emit_legacy(
    app: &tauri::AppHandle,
    envelope: &EventEnvelope,
    metrics: Option<Arc<EventBusMetrics>>,
) {
    use tauri::Emitter;
    if let Some(ref m) = metrics {
        m.desktop_emit_attempt_count
            .fetch_add(1, Ordering::Relaxed);
    }
    match app.emit(EVENT_LEGACY, envelope) {
        Ok(()) => {
            if let Some(m) = metrics {
                m.desktop_legacy_emit_count
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        Err(error) => {
            tracing::error!("[ACP] desktop legacy emit failed: {error}");
            if let Some(m) = metrics {
                m.desktop_emit_failure_count
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(feature = "tauri-runtime")]
struct TauriBatchSink {
    app: tauri::AppHandle,
}

#[cfg(feature = "tauri-runtime")]
#[async_trait]
impl DesktopBatchSink for TauriBatchSink {
    async fn emit_batch(&self, batch: &DesktopAcpEventBatch) -> Result<(), String> {
        use tauri::Emitter;
        self.app
            .emit(EVENT_BATCH, batch)
            .map_err(|e| e.to_string())
    }

    async fn emit_failure(&self, failure: &DesktopDeliveryFailure) -> Result<(), String> {
        use tauri::Emitter;
        self.app
            .emit(EVENT_DELIVERY_FAILED, failure)
            .map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::internal_bus::EventBusMetricsSnapshot;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::Notify;

    #[derive(Default)]
    struct RecordingSink {
        batches: Mutex<Vec<DesktopAcpEventBatch>>,
        failures: Mutex<Vec<DesktopDeliveryFailure>>,
        fail: AtomicBool,
        block: AtomicBool,
        entered: Notify,
        release: Notify,
    }

    #[async_trait]
    impl DesktopBatchSink for RecordingSink {
        async fn emit_batch(&self, batch: &DesktopAcpEventBatch) -> Result<(), String> {
            if self.block.load(Ordering::Acquire) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            if self.fail.load(Ordering::Relaxed) {
                return Err("injected emit failure".into());
            }
            self.batches.lock().unwrap().push(batch.clone());
            Ok(())
        }

        async fn emit_failure(&self, failure: &DesktopDeliveryFailure) -> Result<(), String> {
            self.failures.lock().unwrap().push(failure.clone());
            Ok(())
        }
    }

    struct TestEnvelope {
        envelope: Arc<EventEnvelope>,
        estimated_bytes: usize,
    }

    #[derive(Clone)]
    struct BatcherHarness {
        batcher: Arc<DesktopAcpEventBatcher>,
        sink: Arc<RecordingSink>,
        metrics: Arc<EventBusMetrics>,
    }

    impl BatcherHarness {
        fn new() -> Self {
            Self::with_sink(RecordingSink::default())
        }

        fn with_blocked_sink() -> Self {
            let sink = RecordingSink::default();
            sink.block.store(true, Ordering::Release);
            Self::with_sink(sink)
        }

        fn with_sink(sink: RecordingSink) -> Self {
            let sink = Arc::new(sink);
            let metrics = Arc::new(EventBusMetrics::default());
            let batcher = Arc::new(DesktopAcpEventBatcher::start(
                sink.clone(),
                metrics.clone(),
            ));
            Self {
                batcher,
                sink,
                metrics,
            }
        }

        async fn enqueue(&self, item: TestEnvelope) -> Result<(), DesktopDeliveryError> {
            self.batcher
                .enqueue(item.envelope, item.estimated_bytes)
                .await
        }

        async fn shutdown(&self) -> Result<(), DesktopDeliveryError> {
            self.batcher.shutdown().await
        }

        async fn yield_task(&self) {
            for _ in 0..32 {
                tokio::task::yield_now().await;
            }
        }

        async fn wait_for_batches(&self, expected: usize) {
            for _ in 0..1_000 {
                if self.batches().len() >= expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("batcher did not emit {expected} batch(es)");
        }

        async fn wait_for_failure(&self) {
            for _ in 0..1_000 {
                if self.is_failed() && !self.failures().is_empty() {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!("batcher did not enter failed state");
        }

        async fn wait_until_sink_blocked(&self) {
            self.sink.entered.notified().await;
        }

        fn release_sink(&self) {
            self.sink.block.store(false, Ordering::Release);
            self.sink.release.notify_one();
        }

        fn fail_sink(&self) {
            self.sink.fail.store(true, Ordering::Release);
        }

        fn batches(&self) -> Vec<DesktopAcpEventBatch> {
            self.sink.batches.lock().unwrap().clone()
        }

        fn batch_seqs(&self) -> Vec<Vec<u64>> {
            self.batches()
                .into_iter()
                .map(|batch| batch.events.into_iter().map(|event| event.seq).collect())
                .collect()
        }

        fn flattened_seqs(&self, connection_id: &str) -> Vec<u64> {
            self.batches()
                .into_iter()
                .flat_map(|batch| batch.events)
                .filter(|event| event.connection_id == connection_id)
                .map(|event| event.seq)
                .collect()
        }

        fn failures(&self) -> Vec<DesktopDeliveryFailure> {
            self.sink.failures.lock().unwrap().clone()
        }

        fn metrics(&self) -> EventBusMetricsSnapshot {
            self.metrics.snapshot()
        }

        fn is_failed(&self) -> bool {
            self.batcher.is_failed()
        }
    }

    fn test_envelope(
        connection_id: &str,
        seq: u64,
        payload: AcpEvent,
        estimated_bytes: usize,
    ) -> TestEnvelope {
        TestEnvelope {
            envelope: Arc::new(EventEnvelope {
                seq,
                connection_id: connection_id.to_owned(),
                payload,
            }),
            estimated_bytes,
        }
    }

    fn content(seq: u64, estimated_bytes: usize) -> TestEnvelope {
        content_for("c1", seq, estimated_bytes)
    }

    fn content_for(connection_id: &str, seq: u64, estimated_bytes: usize) -> TestEnvelope {
        test_envelope(
            connection_id,
            seq,
            AcpEvent::ContentDelta { text: "x".into() },
            estimated_bytes,
        )
    }

    fn permission(seq: u64) -> TestEnvelope {
        test_envelope(
            "c1",
            seq,
            AcpEvent::PermissionRequest {
                request_id: "permission-1".into(),
                tool_call: serde_json::json!({}),
                options: vec![],
            },
            1,
        )
    }

    fn question(seq: u64) -> TestEnvelope {
        test_envelope(
            "c1",
            seq,
            AcpEvent::QuestionRequest {
                question_id: "question-1".into(),
                questions: vec![],
            },
            1,
        )
    }

    fn completion(seq: u64) -> TestEnvelope {
        test_envelope(
            "c1",
            seq,
            AcpEvent::TurnComplete {
                session_id: "session-1".into(),
                stop_reason: "end_turn".into(),
                agent_type: "grok".into(),
            mark_awaiting_reply: false,
            },
            1,
        )
    }

    fn error(connection_id: &str, seq: u64) -> TestEnvelope {
        test_envelope(
            connection_id,
            seq,
            AcpEvent::Error {
                message: "synthetic".into(),
                agent_type: "grok".into(),
                code: Some("synthetic".into()),
                terminal: false,
            },
            1,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn timer_flushes_16ms_after_first_event() {
        let harness = BatcherHarness::new();
        harness.enqueue(content(1, 8)).await.unwrap();
        tokio::time::advance(Duration::from_millis(15)).await;
        assert!(harness.batches().is_empty());
        tokio::time::advance(Duration::from_millis(1)).await;
        harness.yield_task().await;
        assert_eq!(harness.batch_seqs(), vec![vec![1]]);
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn count_byte_control_and_shutdown_flush_in_order() {
        assert_flushes_at_count(128).await;
        assert_flushes_at_bytes(64 * 1024).await;
        assert_control_flushes_with_preceding_events(permission(3)).await;
        assert_control_flushes_with_preceding_events(question(3)).await;
        assert_control_flushes_with_preceding_events(completion(3)).await;
        assert_control_flushes_with_preceding_events(error("c1", 3)).await;
        assert_shutdown_drains(vec![content(1, 1), content(2, 1)]).await;
    }

    async fn assert_flushes_at_count(count: usize) {
        let harness = BatcherHarness::new();
        for seq in 1..=count as u64 {
            harness.enqueue(content(seq, 1)).await.unwrap();
        }
        harness.wait_for_batches(1).await;
        assert_eq!(harness.batches()[0].events.len(), count);
        harness.shutdown().await.unwrap();
    }

    async fn assert_flushes_at_bytes(bytes: usize) {
        let harness = BatcherHarness::new();
        harness.enqueue(content(1, bytes / 2)).await.unwrap();
        harness
            .enqueue(content(2, bytes - bytes / 2))
            .await
            .unwrap();
        harness.wait_for_batches(1).await;
        assert_eq!(harness.batch_seqs(), vec![vec![1, 2]]);
        harness.shutdown().await.unwrap();
    }

    async fn assert_control_flushes_with_preceding_events(control: TestEnvelope) {
        let harness = BatcherHarness::new();
        harness.enqueue(content(1, 1)).await.unwrap();
        harness.enqueue(content(2, 1)).await.unwrap();
        harness.enqueue(control).await.unwrap();
        harness.wait_for_batches(1).await;
        assert_eq!(harness.batch_seqs(), vec![vec![1, 2, 3]]);
        harness.shutdown().await.unwrap();
    }

    async fn assert_shutdown_drains(events: Vec<TestEnvelope>) {
        let harness = BatcherHarness::new();
        for event in events {
            harness.enqueue(event).await.unwrap();
        }
        harness.shutdown().await.unwrap();
        assert_eq!(harness.batch_seqs(), vec![vec![1, 2]]);
    }

    #[tokio::test]
    async fn full_queue_applies_backpressure_without_loss() {
        let harness = BatcherHarness::with_blocked_sink();
        for seq in 1..=MAX_BATCH_EVENTS as u64 {
            harness
                .enqueue(content_for("c1", seq, 1))
                .await
                .unwrap();
        }
        harness.wait_until_sink_blocked().await;
        for seq in (MAX_BATCH_EVENTS as u64 + 1)..=(MAX_BATCH_EVENTS + QUEUE_CAPACITY) as u64 {
            harness
                .enqueue(content_for("c1", seq, 1))
                .await
                .unwrap();
        }
        let blocked = tokio::spawn({
            let harness = harness.clone();
            async move {
                harness
                    .enqueue(content_for(
                        "c1",
                        (MAX_BATCH_EVENTS + QUEUE_CAPACITY + 1) as u64,
                        1,
                    ))
                    .await
            }
        });
        harness.yield_task().await;
        assert!(!blocked.is_finished());
        harness.release_sink();
        blocked.await.unwrap().unwrap();
        harness.shutdown().await.unwrap();
        assert_eq!(
            harness.flattened_seqs("c1"),
            (1..=(MAX_BATCH_EVENTS + QUEUE_CAPACITY + 1) as u64).collect::<Vec<_>>()
        );
        assert!(harness.metrics().desktop_queue_full_count > 0);
    }

    #[tokio::test]
    async fn failed_emit_stops_delivery_and_reports_affected_ranges_once() {
        let harness = BatcherHarness::with_blocked_sink();
        for seq in 1..=MAX_BATCH_EVENTS as u64 {
            harness.enqueue(content_for("a", seq, 1)).await.unwrap();
        }
        harness.wait_until_sink_blocked().await;
        harness.enqueue(content_for("b", 3, 1)).await.unwrap();
        harness.enqueue(content_for("c", 9, 1)).await.unwrap();
        harness.fail_sink();
        harness.release_sink();
        harness.wait_for_failure().await;
        assert_eq!(harness.failures().len(), 1);
        assert_eq!(harness.failures()[0].reason, "batch_emit_failed");
        assert_eq!(
            harness.failures()[0].affected,
            vec![
                DesktopConnectionSeqRange {
                    connection_id: "a".into(),
                    first_seq: 1,
                    last_seq: MAX_BATCH_EVENTS as u64,
                },
                DesktopConnectionSeqRange {
                    connection_id: "b".into(),
                    first_seq: 3,
                    last_seq: 3,
                },
                DesktopConnectionSeqRange {
                    connection_id: "c".into(),
                    first_seq: 9,
                    last_seq: 9,
                },
            ]
        );
        // Failure payload is ranges + reason only (no content/tool fields).
        let encoded = serde_json::to_value(&harness.failures()[0]).unwrap();
        assert!(encoded.get("reason").is_some());
        assert!(encoded.get("affected").is_some());
        assert!(encoded.get("generation").is_some());
        assert!(encoded.get("message").is_none());
        assert!(encoded.get("tool_call").is_none());
        assert!(encoded.get("events").is_none());

        assert!(harness.enqueue(content_for("a", 129, 1)).await.is_err());
        assert_eq!(harness.metrics().desktop_runtime_failure_count, 1);
        // One failure signal only.
        harness.yield_task().await;
        assert_eq!(harness.failures().len(), 1);
    }

    /// Sink whose `emit_batch` panics so the worker JoinHandle exits with a
    /// panic — supervisor must emit `batch_task_stopped` once with outstanding
    /// ranges and never include content fields.
    struct PanicBatchSink {
        failures: Mutex<Vec<DesktopDeliveryFailure>>,
    }

    #[async_trait]
    impl DesktopBatchSink for PanicBatchSink {
        async fn emit_batch(&self, _batch: &DesktopAcpEventBatch) -> Result<(), String> {
            panic!("injected batch worker panic");
        }

        async fn emit_failure(&self, failure: &DesktopDeliveryFailure) -> Result<(), String> {
            self.failures.lock().unwrap().push(failure.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn worker_panic_emits_batch_task_stopped_once() {
        let sink = Arc::new(PanicBatchSink {
            failures: Mutex::new(Vec::new()),
        });
        let metrics = Arc::new(EventBusMetrics::default());
        let batcher = Arc::new(DesktopAcpEventBatcher::start(
            sink.clone() as Arc<dyn DesktopBatchSink>,
            metrics.clone(),
        ));
        batcher
            .enqueue(
                Arc::new(EventEnvelope {
                    seq: 7,
                    connection_id: "c1".into(),
                    payload: AcpEvent::ContentDelta { text: "secret".into() },
                }),
                1,
            )
            .await
            .unwrap();
        // Force an immediate flush via a control event so the panic path runs.
        batcher
            .enqueue(
                Arc::new(EventEnvelope {
                    seq: 8,
                    connection_id: "c1".into(),
                    payload: AcpEvent::TurnComplete {
                        session_id: "s".into(),
                        stop_reason: "end_turn".into(),
                        agent_type: "grok".into(),
                    mark_awaiting_reply: false,
                    },
                }),
                1,
            )
            .await
            .unwrap();

        for _ in 0..2_000 {
            if !sink.failures.lock().unwrap().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let failures = sink.failures.lock().unwrap().clone();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].reason, "batch_task_stopped");
        assert_eq!(
            failures[0].affected,
            vec![DesktopConnectionSeqRange {
                connection_id: "c1".into(),
                first_seq: 7,
                last_seq: 8,
            }]
        );
        let encoded = serde_json::to_value(&failures[0]).unwrap();
        assert!(encoded.get("message").is_none());
        assert!(encoded.get("events").is_none());
        assert!(encoded.get("text").is_none());
        assert_eq!(
            metrics
                .desktop_runtime_failure_count
                .load(Ordering::Relaxed),
            1
        );
        // Keep batcher alive until supervisor finishes so the channel is not
        // closed for unrelated reasons mid-panic.
        drop(batcher);
    }

    #[tokio::test]
    async fn orderly_shutdown_does_not_emit_failure() {
        let harness = BatcherHarness::new();
        harness.enqueue(content(1, 1)).await.unwrap();
        harness.shutdown().await.unwrap();
        assert!(harness.failures().is_empty());
        assert_eq!(harness.batch_seqs(), vec![vec![1]]);
    }

    #[test]
    fn tool_calls_are_not_flush_sensitive() {
        assert!(!is_flush_sensitive(&AcpEvent::ToolCall {
            tool_call_id: "t".into(),
            title: "t".into(),
            kind: "read".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        }));
        assert!(!is_flush_sensitive(&AcpEvent::ToolCallUpdate {
            tool_call_id: "t".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        }));
        assert!(!is_flush_sensitive(&AcpEvent::ContentDelta {
            text: "x".into()
        }));
        assert!(is_flush_sensitive(&AcpEvent::SessionLoadFailed {
            session_id: "s".into(),
            message: "missing".into(),
            code: "resource_not_found".into(),
        }));
    }

    #[test]
    fn legacy_capabilities_disable_dependent_flags_and_exclusive_mode() {
        let requested = StreamingPerformanceFlags {
            desktop_acp_event_batching: true,
            incremental_live_transcript: true,
            deferred_streaming_rich_content: true,
        };
        // Startup fallback / legacy mode forces batching off and dependents off.
        let caps = DesktopAcpDelivery::capabilities_for(
            DesktopDeliveryMode::Legacy,
            requested,
        );
        assert_eq!(caps.mode, DesktopDeliveryMode::Legacy);
        assert!(!caps.flags.desktop_acp_event_batching);
        assert!(!caps.flags.incremental_live_transcript);
        assert!(!caps.flags.deferred_streaming_rich_content);
        assert_eq!(caps.failure_event, "acp://delivery-failed");
        // Mode is exclusive: legacy never advertises batch event name as active.
        // (Frontend selects one subscription from mode; both names are never live.)
        assert_ne!(EVENT_LEGACY, EVENT_BATCH);
    }

    #[test]
    fn batched_capabilities_preserve_normalized_flags() {
        let flags = StreamingPerformanceFlags {
            desktop_acp_event_batching: true,
            incremental_live_transcript: true,
            deferred_streaming_rich_content: false,
        };
        let caps =
            DesktopAcpDelivery::capabilities_for(DesktopDeliveryMode::Batched, flags.clone());
        assert_eq!(caps.mode, DesktopDeliveryMode::Batched);
        assert_eq!(caps.flags, flags.normalized());
    }

    #[test]
    fn from_env_flags_default_complete_path_after_p3() {
        let flags = StreamingPerformanceFlags::from_lookup(|_| None);
        assert_eq!(flags, StreamingPerformanceFlags::phase_default());
        assert!(flags.desktop_acp_event_batching);
        assert!(flags.incremental_live_transcript);
        assert!(flags.deferred_streaming_rich_content);
        let caps =
            DesktopAcpDelivery::capabilities_for(DesktopDeliveryMode::Batched, flags);
        assert_eq!(caps.mode, DesktopDeliveryMode::Batched);
    }
}
