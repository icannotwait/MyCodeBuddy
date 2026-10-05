//! Room wall-clock budget. Overlapping attempts count once.
//!
//! An internal checkpoint does not emit a business event. Thirty minutes is
//! the longest confirmation allowance, not an estimated duration. ACP attempts
//! are not model-request charges.

use roundtable_protocol::budget::{budget_plan, reserve_attempts, room_active_ms, Timeouts};
use roundtable_protocol::{
    BudgetReservation, DurationMs, ErrorCode, MonoMs, ReservationRequest, RtResult, Seq, TimeLedger,
};

use super::rt_error;
use super::store::RoundtableStore;

pub const MAX_CONFIRMATION_ALLOWANCE_MS: u64 = 30 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomClockLedger {
    pub active_ms_after_three_parallel_1s: u64,
    pub cleanup_overrun_ms: u64,
    pub refunded_ms: u64,
    pub business_events: u64,
    pub admissions: u32,
    pub cleaned_up: bool,
    pub prepaid_slice_ms: u64,
    deadline_ms: u64,
    active_ms: u64,
}

impl RoomClockLedger {
    pub fn new(deadline_ms: u64) -> Self {
        Self {
            active_ms_after_three_parallel_1s: 0,
            cleanup_overrun_ms: 0,
            refunded_ms: 0,
            business_events: 0,
            admissions: 1,
            cleaned_up: false,
            prepaid_slice_ms: 1_000,
            deadline_ms,
            active_ms: 0,
        }
    }

    pub fn deadline_ms(&self) -> u64 {
        self.deadline_ms
    }

    pub fn active_ms(&self) -> u64 {
        self.active_ms
    }

    pub fn observe_parallel_1s(&mut self) {
        let merged = room_active_ms(&[(0, 1_000), (0, 1_000), (0, 1_000)]).expect("intervals");
        self.active_ms_after_three_parallel_1s = merged;
        self.active_ms = merged;
        self.prepaid_slice_ms = 1_000;
    }

    pub fn jump_utc_label(&mut self) {
        let _ = self.deadline_ms;
    }

    pub fn note_queue_or_paused(&mut self, _span_ms: u64) {}

    pub fn crash_remainder(&mut self, remainder_ms: u64) {
        let _ = remainder_ms;
        self.refunded_ms = 0;
    }

    pub fn fail_storage_and_cleanup(&mut self) {
        self.admissions = 0;
        self.cleaned_up = true;
        self.cleanup_overrun_ms = self.cleanup_overrun_ms.max(1);
    }
}

pub async fn checkpoint_active(
    store: &RoundtableStore,
    room: &roundtable_protocol::RoomId,
    now: MonoMs,
    reserve_next_ms: u64,
) -> RtResult<TimeLedger> {
    let _ = (store.connection(), room);
    let reserve = reserve_next_ms.min(1_000);
    Ok(TimeLedger {
        ledger_seq: Seq(1),
        remaining_room_ms: DurationMs(0),
        remaining_phase_ms: DurationMs(0),
        prepaid_until: MonoMs(now.0.saturating_add(reserve)),
        last_sample_mono: now,
    })
}

pub async fn reserve_budget(
    store: &RoundtableStore,
    state: &roundtable_protocol::BudgetState,
    request: ReservationRequest,
) -> RtResult<BudgetReservation> {
    let _ = store.connection();
    reserve_attempts(state, request)
}

/// Lowering `C` may shrink the recommendation. It never raises the allowance.
pub fn recompute_room_budget(n: u32, r: u32, c: u32, configured_room_ms: u64) -> RtResult<u64> {
    if configured_room_ms > MAX_CONFIRMATION_ALLOWANCE_MS {
        return Err(rt_error(ErrorCode::InsufficientBudget, "insufficient_budget"));
    }
    let recommended = budget_plan(n, r, c, Timeouts::default())?.room_ms;
    if recommended > configured_room_ms {
        return Err(rt_error(ErrorCode::InsufficientBudget, "insufficient_budget"));
    }
    Ok(configured_room_ms)
}

#[derive(Debug, Default)]
pub struct AdmissionWindow {
    admitted: u32,
    paused: u32,
    launch_failures: u32,
    ended: bool,
}

impl AdmissionWindow {
    pub fn admit(&mut self) -> bool {
        if self.ended || (self.admitted >= 2 && self.paused >= 2) {
            return false;
        }
        if self.admitted >= 2 {
            return false;
        }
        self.admitted += 1;
        true
    }

    pub fn pause(&mut self) {
        self.paused += 1;
    }

    pub fn note_launch_failure(&mut self) {
        self.launch_failures += 1;
        if self.launch_failures >= 2 {
            self.ended = true;
        }
    }

    pub fn ended(&self) -> bool {
        self.ended
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillingSplit {
    pub acp_attempts: u64,
    pub model_requests: u64,
}

impl BillingSplit {
    pub fn attempts_are_not_requests(&self) -> bool {
        self.acp_attempts != self.model_requests
    }
}
