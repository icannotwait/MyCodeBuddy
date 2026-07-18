//! Capability-limited soft supervisor for running delegation tasks.
//!
//! Observes only: derives [`TaskObservation`] from child agent activity and
//! waiting-input state, publishes snapshots through
//! [`DelegationObservationSink`], and never receives cancel / disconnect /
//! settle / route capabilities.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::{mpsc, watch};

use super::types::{ObservationSnapshot, TaskObservation};

/// Periodic scan cadence for the soft watchdog.
pub const SUPERVISOR_SCAN_INTERVAL: Duration = Duration::from_secs(15);

/// Read-only health snapshot for one running Broker task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningTaskHealth {
    pub task_id: String,
    pub child_connection_id: String,
    pub last_agent_activity_at: DateTime<Utc>,
    pub waiting_input: bool,
}

/// Read-only view of running-task health. No cancel / disconnect / settle.
#[async_trait]
pub trait DelegationObservationSource: Send + Sync {
    async fn running_task_health(&self) -> Vec<RunningTaskHealth>;
}

/// Event/cache-only sink for observation snapshots. No terminal writes.
#[async_trait]
pub trait DelegationObservationSink: Send + Sync {
    async fn publish_observation(&self, task_id: &str, observation: ObservationSnapshot);
    async fn clear_observation(&self, task_id: &str);
}

/// Clock abstraction so tests drive threshold crossings without sleeping.
pub trait SupervisorClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Production wall-clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl SupervisorClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Coalescing wake handle for accepted/terminal/activity/permission changes.
/// `try_send` ignores a full or closed channel so producers stay non-blocking.
#[derive(Clone, Default)]
pub struct SupervisorWake {
    tx: Option<mpsc::Sender<()>>,
}

impl std::fmt::Debug for SupervisorWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupervisorWake")
            .field("armed", &self.tx.is_some())
            .finish()
    }
}

impl SupervisorWake {
    pub fn new(tx: mpsc::Sender<()>) -> Self {
        Self { tx: Some(tx) }
    }

    pub fn noop() -> Self {
        Self { tx: None }
    }

    pub fn notify(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(());
        }
    }
}

/// Derive observation for a running task.
///
/// Precedence: `waiting_input` wins over stall. `stalled_since` is exactly
/// `last_agent_activity_at + threshold`, never the scan time.
pub fn derive_observation(
    now: DateTime<Utc>,
    last_agent_activity_at: DateTime<Utc>,
    waiting_input: bool,
    threshold_secs: u32,
) -> ObservationSnapshot {
    if waiting_input {
        return ObservationSnapshot {
            observation: TaskObservation::WaitingInput,
            last_agent_activity_at,
            stalled_since: None,
        };
    }
    let threshold = chrono::Duration::seconds(i64::from(threshold_secs));
    let stalled_at = last_agent_activity_at + threshold;
    if now >= stalled_at {
        ObservationSnapshot {
            observation: TaskObservation::Stalled,
            last_agent_activity_at,
            stalled_since: Some(stalled_at),
        }
    } else {
        ObservationSnapshot {
            observation: TaskObservation::Active,
            last_agent_activity_at,
            stalled_since: None,
        }
    }
}

/// Soft supervisor: 15s scan + immediate bounded wakeups. Observe only.
pub struct DelegationSupervisor {
    source: Arc<dyn DelegationObservationSource>,
    sink: Arc<dyn DelegationObservationSink>,
    clock: Arc<dyn SupervisorClock>,
    threshold_rx: watch::Receiver<u32>,
    wake_rx: mpsc::Receiver<()>,
    last_emitted: Mutex<HashMap<String, ObservationSnapshot>>,
    metrics: Arc<super::metrics::DelegationMetrics>,
}

impl DelegationSupervisor {
    /// The sole constructor (production and tests). No cancel/store/spawner.
    pub fn new(
        source: Arc<dyn DelegationObservationSource>,
        sink: Arc<dyn DelegationObservationSink>,
        clock: Arc<dyn SupervisorClock>,
        threshold_rx: watch::Receiver<u32>,
        wake_rx: mpsc::Receiver<()>,
    ) -> Self {
        Self::with_metrics(
            source,
            sink,
            clock,
            threshold_rx,
            wake_rx,
            Arc::new(super::metrics::DelegationMetrics::default()),
        )
    }

    /// Production constructor with shared reliability metrics.
    pub fn with_metrics(
        source: Arc<dyn DelegationObservationSource>,
        sink: Arc<dyn DelegationObservationSink>,
        clock: Arc<dyn SupervisorClock>,
        threshold_rx: watch::Receiver<u32>,
        wake_rx: mpsc::Receiver<()>,
        metrics: Arc<super::metrics::DelegationMetrics>,
    ) -> Self {
        Self {
            source,
            sink,
            clock,
            threshold_rx,
            wake_rx,
            last_emitted: Mutex::new(HashMap::new()),
            metrics,
        }
    }

    /// One scan pass: emit on observation/timestamp change; drop terminals.
    pub async fn scan_once(&self) {
        let now = self.clock.now();
        let threshold = *self.threshold_rx.borrow();
        let health = self.source.running_task_health().await;
        let live_ids: std::collections::HashSet<String> =
            health.iter().map(|h| h.task_id.clone()).collect();

        // Remove observations for tasks no longer logical Running
        // (absent from health = not in running ∪ settling).
        let stale: Vec<String> = {
            let last = self.last_emitted.lock().expect("last_emitted lock");
            last.keys()
                .filter(|id| !live_ids.contains(*id))
                .cloned()
                .collect()
        };
        for task_id in stale {
            self.sink.clear_observation(&task_id).await;
            self.last_emitted
                .lock()
                .expect("last_emitted lock")
                .remove(&task_id);
        }

        for h in health {
            let snap =
                derive_observation(now, h.last_agent_activity_at, h.waiting_input, threshold);
            let prev_snap = {
                let last = self.last_emitted.lock().expect("last_emitted lock");
                last.get(&h.task_id).cloned()
            };
            let should_emit = match &prev_snap {
                Some(prev) => prev != &snap,
                None => true,
            };
            if should_emit {
                // Metrics only on actual supervisor-emitted observation-enum
                // transitions (not first publish; timestamp-only changes still
                // emit events but do not bump stalled episode counters).
                if let Some(prev) = &prev_snap {
                    if prev.observation != snap.observation {
                        self.metrics
                            .record_observation_transition(prev.observation, snap.observation);
                        super::metrics::DelegationAuditRecord::observation(
                            &h.task_id,
                            prev.observation,
                            snap.observation,
                        )
                        .emit_observation();
                    }
                }
                self.sink
                    .publish_observation(&h.task_id, snap.clone())
                    .await;
                self.last_emitted
                    .lock()
                    .expect("last_emitted lock")
                    .insert(h.task_id, snap);
            }
        }
    }

    /// Run until the wake channel closes (process shutdown).
    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(SUPERVISOR_SCAN_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Consume the immediate first tick so startup does not double-scan
        // with an explicit first wake.
        interval.tick().await;
        // Initial scan so accepted tasks present at boot get an observation.
        self.scan_once().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.scan_once().await;
                }
                wake = self.wake_rx.recv() => {
                    if wake.is_none() {
                        break;
                    }
                    // Drain coalesced wakes so a burst becomes one scan.
                    while self.wake_rx.try_recv().is_ok() {}
                    self.scan_once().await;
                }
                changed = self.threshold_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    self.scan_once().await;
                }
            }
        }
    }
}

// ── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::types::TaskStatus;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Mutex as AsyncMutex;

    struct FakeClock {
        now: Mutex<DateTime<Utc>>,
    }

    impl FakeClock {
        fn at(rfc3339: &str) -> Self {
            let now = DateTime::parse_from_rfc3339(rfc3339)
                .expect("rfc3339")
                .with_timezone(&Utc);
            Self {
                now: Mutex::new(now),
            }
        }

        fn advance_seconds(&self, secs: i64) {
            let mut n = self.now.lock().expect("clock");
            *n += chrono::Duration::seconds(secs);
        }

        fn now_value(&self) -> DateTime<Utc> {
            *self.now.lock().expect("clock")
        }
    }

    impl SupervisorClock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            self.now_value()
        }
    }

    struct MockObservationSource {
        health: AsyncMutex<Vec<RunningTaskHealth>>,
        task_status: AsyncMutex<TaskStatus>,
        disconnect_count: AtomicUsize,
        cancel_count: AtomicUsize,
        route_change_count: AtomicUsize,
    }

    impl MockObservationSource {
        fn running(task_id: &str, last: DateTime<Utc>) -> Self {
            Self {
                health: AsyncMutex::new(vec![RunningTaskHealth {
                    task_id: task_id.to_string(),
                    child_connection_id: format!("child-{task_id}"),
                    last_agent_activity_at: last,
                    waiting_input: false,
                }]),
                task_status: AsyncMutex::new(TaskStatus::Running),
                disconnect_count: AtomicUsize::new(0),
                cancel_count: AtomicUsize::new(0),
                route_change_count: AtomicUsize::new(0),
            }
        }

        async fn mark_activity(&self, at: DateTime<Utc>) {
            let mut h = self.health.lock().await;
            if let Some(entry) = h.first_mut() {
                entry.last_agent_activity_at = at;
                entry.waiting_input = false;
            }
        }

        /// Drop from health as if the task left logical Running (true terminal).
        async fn leave_logical_running(&self) {
            self.health.lock().await.clear();
            *self.task_status.lock().await = TaskStatus::Completed;
        }

        async fn task_status(&self) -> TaskStatus {
            *self.task_status.lock().await
        }

        fn disconnect_count(&self) -> usize {
            self.disconnect_count.load(Ordering::SeqCst)
        }

        fn cancel_count(&self) -> usize {
            self.cancel_count.load(Ordering::SeqCst)
        }

        fn route_change_count(&self) -> usize {
            self.route_change_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl DelegationObservationSource for MockObservationSource {
        async fn running_task_health(&self) -> Vec<RunningTaskHealth> {
            self.health.lock().await.clone()
        }
    }

    #[derive(Default)]
    struct MockObservationSink {
        transitions: AsyncMutex<Vec<(String, TaskObservation)>>,
        snapshots: AsyncMutex<Vec<(String, ObservationSnapshot)>>,
        last: AsyncMutex<Option<ObservationSnapshot>>,
        clears: AsyncMutex<Vec<String>>,
    }

    impl MockObservationSink {
        async fn transitions(&self) -> Vec<(String, TaskObservation)> {
            self.transitions.lock().await.clone()
        }

        async fn snapshots(&self) -> Vec<(String, ObservationSnapshot)> {
            self.snapshots.lock().await.clone()
        }

        async fn clears(&self) -> Vec<String> {
            self.clears.lock().await.clone()
        }

        async fn last(&self) -> ObservationSnapshot {
            self.last
                .lock()
                .await
                .clone()
                .expect("observation published")
        }
    }

    #[async_trait]
    impl DelegationObservationSink for MockObservationSink {
        async fn publish_observation(&self, task_id: &str, observation: ObservationSnapshot) {
            self.transitions
                .lock()
                .await
                .push((task_id.to_string(), observation.observation));
            self.snapshots
                .lock()
                .await
                .push((task_id.to_string(), observation.clone()));
            *self.last.lock().await = Some(observation);
        }

        async fn clear_observation(&self, task_id: &str) {
            self.clears.lock().await.push(task_id.to_string());
            *self.last.lock().await = None;
        }
    }

    fn supervisor_with(
        source: Arc<MockObservationSource>,
        sink: Arc<MockObservationSink>,
        clock: Arc<FakeClock>,
        threshold: u32,
    ) -> DelegationSupervisor {
        let (_thresh_tx, thresh_rx) = watch::channel(threshold);
        let (_wake_tx, wake_rx) = mpsc::channel(8);
        DelegationSupervisor::new(source, sink, clock, thresh_rx, wake_rx)
    }

    #[test]
    fn observation_precedence_and_threshold_timestamp_are_exact() {
        let last = Utc.with_ymd_and_hms(2026, 7, 16, 10, 0, 0).unwrap();
        let before = derive_observation(last + chrono::Duration::seconds(299), last, false, 300);
        assert_eq!(before.observation, TaskObservation::Active);

        let stalled = derive_observation(last + chrono::Duration::seconds(301), last, false, 300);
        assert_eq!(stalled.observation, TaskObservation::Stalled);
        assert_eq!(
            stalled.stalled_since,
            Some(last + chrono::Duration::seconds(300))
        );

        let waiting = derive_observation(last + chrono::Duration::seconds(900), last, true, 300);
        assert_eq!(waiting.observation, TaskObservation::WaitingInput);
        assert_eq!(waiting.stalled_since, None);

        // Exactly at threshold is stalled (silence at least N seconds).
        let exact = derive_observation(last + chrono::Duration::seconds(300), last, false, 300);
        assert_eq!(exact.observation, TaskObservation::Stalled);
        assert_eq!(
            exact.stalled_since,
            Some(last + chrono::Duration::seconds(300))
        );
    }

    #[tokio::test]
    async fn supervisor_emits_once_per_transition_and_activity_recovers_stall() {
        let clock = Arc::new(FakeClock::at("2026-07-16T10:00:00Z"));
        let source = Arc::new(MockObservationSource::running("task-1", clock.now_value()));
        let sink = Arc::new(MockObservationSink::default());
        let supervisor = supervisor_with(source.clone(), sink.clone(), clock.clone(), 300);

        supervisor.scan_once().await;
        supervisor.scan_once().await;
        assert_eq!(
            sink.transitions().await,
            vec![("task-1".into(), TaskObservation::Active)]
        );

        clock.advance_seconds(301);
        supervisor.scan_once().await;
        supervisor.scan_once().await;
        source.mark_activity(clock.now_value()).await;
        supervisor.scan_once().await;
        assert_eq!(
            sink.transitions()
                .await
                .iter()
                .map(|(_, o)| *o)
                .collect::<Vec<_>>(),
            vec![
                TaskObservation::Active,
                TaskObservation::Stalled,
                TaskObservation::Active
            ]
        );
    }

    struct SupervisorFixture {
        supervisor: DelegationSupervisor,
        source: Arc<MockObservationSource>,
        sink: Arc<MockObservationSink>,
    }

    impl SupervisorFixture {
        fn stalled() -> Self {
            let last = Utc.with_ymd_and_hms(2026, 7, 16, 10, 0, 0).unwrap();
            let clock = Arc::new(FakeClock::at("2026-07-16T10:05:01Z")); // 301s later
            let source = Arc::new(MockObservationSource::running("task-stalled", last));
            let sink = Arc::new(MockObservationSink::default());
            let supervisor = supervisor_with(source.clone(), sink.clone(), clock, 300);
            Self {
                supervisor,
                source,
                sink,
            }
        }
    }

    #[tokio::test]
    async fn stalled_scan_has_no_terminal_or_connection_side_effect() {
        let fixture = SupervisorFixture::stalled();
        fixture.supervisor.scan_once().await;
        assert_eq!(fixture.source.task_status().await, TaskStatus::Running);
        assert_eq!(fixture.source.disconnect_count(), 0);
        assert_eq!(fixture.source.cancel_count(), 0);
        assert_eq!(fixture.source.route_change_count(), 0);
        assert_eq!(
            fixture.sink.last().await.observation,
            TaskObservation::Stalled
        );
    }

    #[tokio::test]
    async fn live_threshold_change_recalculates_observation() {
        let clock = Arc::new(FakeClock::at("2026-07-16T10:00:00Z"));
        let last = clock.now_value();
        let source = Arc::new(MockObservationSource::running("task-1", last));
        let sink = Arc::new(MockObservationSink::default());
        let (thresh_tx, thresh_rx) = watch::channel(600u32);
        let (_wake_tx, wake_rx) = mpsc::channel(8);
        let supervisor = DelegationSupervisor::new(
            source.clone(),
            sink.clone(),
            clock.clone(),
            thresh_rx,
            wake_rx,
        );

        // At 301s with threshold 600 → still Active.
        clock.advance_seconds(301);
        supervisor.scan_once().await;
        assert_eq!(sink.last().await.observation, TaskObservation::Active);

        // Lower threshold to 300 → becomes Stalled without time advance.
        thresh_tx.send(300).expect("send threshold");
        // Manual scan_once (run() would react to changed()); assert derivation.
        supervisor.scan_once().await;
        assert_eq!(sink.last().await.observation, TaskObservation::Stalled);
        assert_eq!(
            sink.last().await.stalled_since,
            Some(last + chrono::Duration::seconds(300))
        );
    }

    /// Settling is still logical Running: health continues to include the task,
    /// so a mid-settle scan must retain the observation. Cache/`last_emitted`
    /// clear only after the task leaves health (true terminal), once.
    #[tokio::test]
    async fn observation_remains_through_settling_then_clears_once_at_terminal() {
        let clock = Arc::new(FakeClock::at("2026-07-16T10:00:00Z"));
        let source = Arc::new(MockObservationSource::running("task-1", clock.now_value()));
        let sink = Arc::new(MockObservationSink::default());
        let supervisor = supervisor_with(source.clone(), sink.clone(), clock.clone(), 300);

        supervisor.scan_once().await;
        assert_eq!(
            sink.transitions().await,
            vec![("task-1".into(), TaskObservation::Active)]
        );
        assert!(sink.clears().await.is_empty());

        // Mid-settle scan: source still reports the task (running ∪ settling).
        supervisor.scan_once().await;
        assert_eq!(
            sink.transitions().await,
            vec![("task-1".into(), TaskObservation::Active)],
            "identical Active snapshot must not re-emit during settling"
        );
        assert!(
            sink.clears().await.is_empty(),
            "settling must not clear observation as stale/terminal"
        );
        assert_eq!(sink.last().await.observation, TaskObservation::Active);

        // True terminal: leave both running and settling.
        source.leave_logical_running().await;
        supervisor.scan_once().await;
        assert_eq!(sink.clears().await, vec!["task-1".to_string()]);
        assert!(
            sink.last.lock().await.is_none(),
            "terminal clear drops the cached last snapshot"
        );

        // Second scan after terminal: no duplicate clear.
        supervisor.scan_once().await;
        assert_eq!(
            sink.clears().await,
            vec!["task-1".to_string()],
            "terminal clear must be once"
        );
    }

    /// Production de-dup uses full `ObservationSnapshot` equality (enum +
    /// timestamps). An activity refresh that stays `Active` must re-emit.
    #[tokio::test]
    async fn active_snapshot_re_emits_when_last_agent_activity_at_changes() {
        let clock = Arc::new(FakeClock::at("2026-07-16T10:00:00Z"));
        let source = Arc::new(MockObservationSource::running("task-1", clock.now_value()));
        let sink = Arc::new(MockObservationSink::default());
        let supervisor = supervisor_with(source.clone(), sink.clone(), clock.clone(), 300);

        supervisor.scan_once().await;
        let first_at = sink.last().await.last_agent_activity_at;

        clock.advance_seconds(10);
        source.mark_activity(clock.now_value()).await;
        supervisor.scan_once().await;

        let snaps = sink.snapshots().await;
        assert_eq!(snaps.len(), 2, "timestamp-only change must re-emit");
        assert_eq!(snaps[0].1.observation, TaskObservation::Active);
        assert_eq!(snaps[1].1.observation, TaskObservation::Active);
        assert_eq!(snaps[0].1.last_agent_activity_at, first_at);
        assert_eq!(snaps[1].1.last_agent_activity_at, clock.now_value());
        assert_ne!(
            snaps[0].1.last_agent_activity_at,
            snaps[1].1.last_agent_activity_at
        );
        assert!(sink.clears().await.is_empty());
    }
}
