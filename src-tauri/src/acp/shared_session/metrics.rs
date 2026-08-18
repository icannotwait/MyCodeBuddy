use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex as StdMutex,
    },
};

use serde::Serialize;

#[derive(Default)]
pub struct SharedSessionMetrics {
    created_total: AtomicU64,
    attached_total: AtomicU64,
    live_sessions: AtomicU64,
    active_leases: AtomicU64,
    bootstrap_ready_total: AtomicU64,
    bootstrap_failed_total: StdMutex<BTreeMap<String, u64>>,
    bootstrap_duration_ms_total: AtomicU64,
    bootstrap_duration_samples: AtomicU64,
    waiting_prompts: AtomicU64,
    waiting_bytes: AtomicU64,
    enqueue_total: AtomicU64,
    cancel_total: AtomicU64,
    dispatch_total: AtomicU64,
    capacity_rejected_total: AtomicU64,
    queue_item_failed_total: AtomicU64,
    interaction_winner_total: AtomicU64,
    interaction_stale_total: AtomicU64,
    stale_stop_total: AtomicU64,
    lease_expired_total: AtomicU64,
    lease_released_total: AtomicU64,
    idle_candidate_total: AtomicU64,
    idle_cas_lost_total: AtomicU64,
    idle_reclaimed_total: AtomicU64,
    cleanup_duration_ms_total: AtomicU64,
    cleanup_duration_samples: AtomicU64,
    cleanup_incomplete_total: AtomicU64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SharedSessionMetricsSnapshot {
    pub created_total: u64,
    pub attached_total: u64,
    pub live_sessions: u64,
    pub active_leases: u64,
    pub bootstrap_ready_total: u64,
    pub bootstrap_failed_total: BTreeMap<String, u64>,
    pub bootstrap_duration_ms_total: u64,
    pub bootstrap_duration_samples: u64,
    pub waiting_prompts: u64,
    pub waiting_bytes: u64,
    pub enqueue_total: u64,
    pub cancel_total: u64,
    pub dispatch_total: u64,
    pub capacity_rejected_total: u64,
    pub queue_item_failed_total: u64,
    pub interaction_winner_total: u64,
    pub interaction_stale_total: u64,
    pub stale_stop_total: u64,
    pub lease_expired_total: u64,
    pub lease_released_total: u64,
    pub idle_candidate_total: u64,
    pub idle_cas_lost_total: u64,
    pub idle_reclaimed_total: u64,
    pub cleanup_duration_ms_total: u64,
    pub cleanup_duration_samples: u64,
    pub cleanup_incomplete_total: u64,
}

impl SharedSessionMetrics {
    pub fn snapshot(&self) -> SharedSessionMetricsSnapshot {
        let bootstrap_failed_total = self
            .bootstrap_failed_total
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        SharedSessionMetricsSnapshot {
            created_total: load(&self.created_total),
            attached_total: load(&self.attached_total),
            live_sessions: load(&self.live_sessions),
            active_leases: load(&self.active_leases),
            bootstrap_ready_total: load(&self.bootstrap_ready_total),
            bootstrap_failed_total,
            bootstrap_duration_ms_total: load(&self.bootstrap_duration_ms_total),
            bootstrap_duration_samples: load(&self.bootstrap_duration_samples),
            waiting_prompts: load(&self.waiting_prompts),
            waiting_bytes: load(&self.waiting_bytes),
            enqueue_total: load(&self.enqueue_total),
            cancel_total: load(&self.cancel_total),
            dispatch_total: load(&self.dispatch_total),
            capacity_rejected_total: load(&self.capacity_rejected_total),
            queue_item_failed_total: load(&self.queue_item_failed_total),
            interaction_winner_total: load(&self.interaction_winner_total),
            interaction_stale_total: load(&self.interaction_stale_total),
            stale_stop_total: load(&self.stale_stop_total),
            lease_expired_total: load(&self.lease_expired_total),
            lease_released_total: load(&self.lease_released_total),
            idle_candidate_total: load(&self.idle_candidate_total),
            idle_cas_lost_total: load(&self.idle_cas_lost_total),
            idle_reclaimed_total: load(&self.idle_reclaimed_total),
            cleanup_duration_ms_total: load(&self.cleanup_duration_ms_total),
            cleanup_duration_samples: load(&self.cleanup_duration_samples),
            cleanup_incomplete_total: load(&self.cleanup_incomplete_total),
        }
    }

    pub(super) fn record_connect(&self, created: bool) {
        if created {
            self.created_total.fetch_add(1, Ordering::Relaxed);
        } else {
            self.attached_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn add_live_session(&self) {
        self.live_sessions.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn remove_live_session(&self) {
        let _ = self
            .live_sessions
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(1))
            });
    }

    pub(super) fn add_active_leases(&self, count: usize) {
        self.active_leases
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub(super) fn remove_active_leases(&self, count: usize) {
        let count = count as u64;
        let _ = self
            .active_leases
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(count))
            });
    }

    pub(super) fn record_bootstrap_ready(&self, elapsed: std::time::Duration) {
        self.bootstrap_ready_total.fetch_add(1, Ordering::Relaxed);
        self.record_bootstrap_duration(elapsed);
    }

    pub(super) fn record_bootstrap_failed(
        &self,
        agent_category: &str,
        route_capability: &str,
        error_code: &str,
        elapsed: std::time::Duration,
    ) {
        let key = format!("{agent_category}|{route_capability}|{error_code}");
        let mut failures = self
            .bootstrap_failed_total
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *failures.entry(key).or_default() += 1;
        drop(failures);
        self.record_bootstrap_duration(elapsed);
    }

    fn record_bootstrap_duration(&self, elapsed: std::time::Duration) {
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.bootstrap_duration_ms_total
            .fetch_add(elapsed_ms, Ordering::Relaxed);
        self.bootstrap_duration_samples
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_enqueue(&self, waiting_bytes: usize) {
        self.enqueue_total.fetch_add(1, Ordering::Relaxed);
        self.waiting_prompts.fetch_add(1, Ordering::Relaxed);
        self.waiting_bytes
            .fetch_add(waiting_bytes as u64, Ordering::Relaxed);
    }

    pub(super) fn record_cancel(&self, waiting_bytes: usize) {
        self.cancel_total.fetch_add(1, Ordering::Relaxed);
        self.remove_waiting(1, waiting_bytes);
    }

    pub(super) fn remove_waiting(&self, count: usize, waiting_bytes: usize) {
        saturating_sub(&self.waiting_prompts, count as u64);
        saturating_sub(&self.waiting_bytes, waiting_bytes as u64);
    }

    pub(super) fn record_queue_items_failed(&self, count: usize) {
        self.queue_item_failed_total
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub(super) fn record_interaction_winner(&self) {
        self.interaction_winner_total
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_interaction_stale(&self) {
        self.interaction_stale_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_stale_stop(&self) {
        self.stale_stop_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_lease_expired(&self, count: usize) {
        self.lease_expired_total
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub(super) fn record_lease_released(&self) {
        self.lease_released_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_capacity_rejection(&self) {
        self.capacity_rejected_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_dispatch(&self) {
        self.dispatch_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_idle_candidate(&self) {
        self.idle_candidate_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_idle_cas_lost(&self) {
        self.idle_cas_lost_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_idle_reclaimed(&self) {
        self.idle_reclaimed_total.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_cleanup_duration(&self, elapsed: std::time::Duration) {
        let elapsed_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        self.cleanup_duration_ms_total
            .fetch_add(elapsed_ms, Ordering::Relaxed);
        self.cleanup_duration_samples
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_cleanup_incomplete(&self) {
        self.cleanup_incomplete_total
            .fetch_add(1, Ordering::Relaxed);
    }
}

fn saturating_sub(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_sub(amount))
    });
}
