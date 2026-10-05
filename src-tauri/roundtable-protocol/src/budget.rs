//! Pure attempt and time budgets.
//!
//! Wall-clock room time is the merged monotonic intervals of the room, never
//! the sum of overlapping attempts. Lowering `C` changes the recommended
//! plan; it does not rewrite a smaller configured budget upward.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{
    DurationMs, ErrorCode, ErrorDetails, InternalReason, MonoMs, RtError, RtResult, Seq, TimeLedger,
};

/// Launch, prompt, validation, and cleanup. Their sum is the slot `T`.
/// Cleanup already includes the 5s cancel grace, so it is not added again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub launch_ms: u64,
    pub prompt_ms: u64,
    pub validation_ms: u64,
    pub cleanup_ms: u64,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            launch_ms: 30_000,
            prompt_ms: 180_000,
            validation_ms: 5_000,
            cleanup_ms: 10_000,
        }
    }
}

impl Timeouts {
    pub fn slot_ms(self) -> RtResult<u64> {
        self.launch_ms
            .checked_add(self.prompt_ms)
            .and_then(|sum| sum.checked_add(self.validation_ms))
            .and_then(|sum| sum.checked_add(self.cleanup_ms))
            .ok_or_else(|| invalid("overflow"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetPlan {
    pub base_attempts: u64,
    pub max_attempts: u64,
    pub slot_ms: u64,
    /// Discussion-phase budget `D = 2 × ceil(N/C) × T`.
    pub discussion_ms: u64,
    pub room_ms: u64,
}

pub fn budget_plan(n: u32, r: u32, c: u32, timeouts: Timeouts) -> RtResult<BudgetPlan> {
    if n < 2 || c == 0 || c > n {
        return Err(invalid("invalid_budget"));
    }
    let participants = u64::from(n);
    let phases = u64::from(r)
        .checked_add(1)
        .ok_or_else(|| invalid("overflow"))?;
    let base = participants
        .checked_mul(phases)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| invalid("overflow"))?;
    let repair_budget = base
        .checked_mul(2)
        .and_then(|value| value.checked_div(5))
        .ok_or_else(|| invalid("overflow"))?;
    let max_attempts = base
        .checked_add(repair_budget)
        .ok_or_else(|| invalid("overflow"))?;
    let slot_ms = timeouts.slot_ms()?;
    let waves = ceil_div(participants, u64::from(c))?;
    let discussion_ms = waves
        .checked_mul(2)
        .and_then(|value| value.checked_mul(slot_ms))
        .ok_or_else(|| invalid("overflow"))?;
    let synthesis_ms = slot_ms.checked_mul(2).ok_or_else(|| invalid("overflow"))?;
    let room_ms = phases
        .checked_mul(discussion_ms)
        .and_then(|value| value.checked_add(synthesis_ms))
        .ok_or_else(|| invalid("overflow"))?;
    Ok(BudgetPlan {
        base_attempts: base,
        max_attempts,
        slot_ms,
        discussion_ms,
        room_ms,
    })
}

fn ceil_div(numerator: u64, denominator: u64) -> RtResult<u64> {
    if denominator == 0 {
        return Err(invalid("overflow"));
    }
    numerator
        .checked_add(denominator - 1)
        .and_then(|value| value.checked_div(denominator))
        .ok_or_else(|| invalid("overflow"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AttemptSlot {
    Member { phase_index: u32, ordinal: u32 },
    Moderator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationKind {
    FirstLaunch,
    OptionalRetry,
}

/// What the caller can prove about dispatch. A reserved prompt that never
/// reached the queue does not consume a prompt attempt. Cancel after enqueue
/// and uncertain admitting do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchObservation {
    ReservedNotEnqueued,
    Admitted,
    Cancelled { enqueued: bool },
    Uncertain,
}

impl DispatchObservation {
    fn consumes_prompt(self) -> bool {
        match self {
            Self::ReservedNotEnqueued => false,
            Self::Admitted => true,
            Self::Cancelled { enqueued } => enqueued,
            Self::Uncertain => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReservationRequest {
    pub kind: ReservationKind,
    pub revision: u64,
    pub slot: AttemptSlot,
    pub observation: DispatchObservation,
    pub now: MonoMs,
    pub deadline: MonoMs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetReservation {
    pub state: BudgetState,
    pub consumed_prompt: bool,
    pub launch_counted: bool,
    pub admission_counted: bool,
    pub reserved_first_launches: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetState {
    pub n: u32,
    pub r: u32,
    pub c: u32,
    pub plan: BudgetPlan,
    pub current_phase_index: u32,
    pub consumed_prompts: u64,
    spent: BTreeSet<(u64, AttemptSlot)>,
    revisions: BTreeMap<(u64, AttemptSlot), TurnAccounting>,
    phase_revisions: BTreeMap<u32, u64>,
    moderator_revision: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct TurnAccounting {
    launch_starts: u32,
    admissions: u32,
}

impl BudgetState {
    pub fn fresh(n: u32, r: u32, c: u32, plan: BudgetPlan) -> Self {
        Self {
            n,
            r,
            c,
            plan,
            current_phase_index: 0,
            consumed_prompts: 0,
            spent: BTreeSet::new(),
            revisions: BTreeMap::new(),
            phase_revisions: BTreeMap::new(),
            moderator_revision: 1,
        }
    }

    pub fn launch_starts(&self, revision: u64) -> u32 {
        self.revisions
            .iter()
            .filter(|((rev, _), _)| *rev == revision)
            .map(|(_, turn)| turn.launch_starts)
            .sum()
    }

    pub fn turn_launch_starts(&self, revision: u64, slot: AttemptSlot) -> u32 {
        self.revisions
            .get(&(revision, slot))
            .map(|turn| turn.launch_starts)
            .unwrap_or(0)
    }

    pub fn admissions(&self, revision: u64) -> u32 {
        self.revisions
            .iter()
            .filter(|((rev, _), _)| *rev == revision)
            .map(|(_, turn)| turn.admissions)
            .sum()
    }

    pub fn turn_admissions(&self, revision: u64, slot: AttemptSlot) -> u32 {
        self.revisions
            .get(&(revision, slot))
            .map(|turn| turn.admissions)
            .unwrap_or(0)
    }

    /// First launches still required: unstarted slots in the current discussion
    /// phase, `N` per future discussion phase, and the moderator once.
    pub fn reserved_first_launches(&self) -> u64 {
        let mut reserved = 0u64;
        if self.current_phase_index <= self.r {
            for phase in self.current_phase_index..=self.r {
                for ordinal in 0..self.n {
                    let slot = AttemptSlot::Member {
                        phase_index: phase,
                        ordinal,
                    };
                    let revision = self.phase_revisions.get(&phase).copied().unwrap_or(1);
                    if !self.spent.contains(&(revision, slot)) {
                        reserved += 1;
                    }
                }
            }
        }
        if !self
            .spent
            .contains(&(self.moderator_revision, AttemptSlot::Moderator))
        {
            reserved += 1;
        }
        reserved
    }
}

/// `now == deadline` is already a timeout. Strictly earlier is still inside.
pub fn deadline_still_open(now: MonoMs, deadline: MonoMs) -> bool {
    now.0 < deadline.0
}

pub fn reserve_attempts(
    state: &BudgetState,
    request: ReservationRequest,
) -> RtResult<BudgetReservation> {
    if !deadline_still_open(request.now, request.deadline) {
        return Err(invalid_state("deadline_reached"));
    }
    validate_slot(state, request.slot)?;
    let current_revision = match request.slot {
        AttemptSlot::Member { phase_index, .. } => state.phase_revisions.get(&phase_index).copied(),
        AttemptSlot::Moderator => Some(state.moderator_revision),
    };
    if current_revision.is_some_and(|revision| request.revision < revision) {
        return Err(invalid_state("stale_revision"));
    }
    let spent = state.spent.contains(&(request.revision, request.slot));
    match request.kind {
        ReservationKind::OptionalRetry if !spent => {
            return Err(invalid_state("retry_before_first_launch"));
        }
        ReservationKind::FirstLaunch if spent => {
            return Err(invalid_state("first_launch_spent"));
        }
        ReservationKind::OptionalRetry | ReservationKind::FirstLaunch => {}
    }
    let consumes_prompt = request.observation.consumes_prompt();
    if let Some(turn) = state.revisions.get(&(request.revision, request.slot)) {
        if turn.launch_starts >= 2 {
            return Err(invalid_state("launch_limit"));
        }
        if consumes_prompt && turn.admissions >= 2 {
            return Err(invalid_state("admission_limit"));
        }
    }
    let consumed_prompts = if consumes_prompt {
        let next_consumed = state
            .consumed_prompts
            .checked_add(1)
            .ok_or_else(|| invalid("overflow"))?;
        if next_consumed > state.plan.max_attempts {
            return Err(insufficient("insufficient_budget"));
        }
        if request.kind == ReservationKind::OptionalRetry {
            let needed = next_consumed
                .checked_add(state.reserved_first_launches())
                .ok_or_else(|| invalid("overflow"))?;
            if needed > state.plan.max_attempts {
                return Err(insufficient("insufficient_budget"));
            }
        }
        next_consumed
    } else {
        state.consumed_prompts
    };

    let mut next = state.clone();
    let launch_counted = true;
    let admission_counted = consumes_prompt;
    {
        let turn = next
            .revisions
            .entry((request.revision, request.slot))
            .or_default();
        turn.launch_starts = turn
            .launch_starts
            .checked_add(1)
            .ok_or_else(|| invalid("overflow"))?;
        if admission_counted {
            turn.admissions = turn
                .admissions
                .checked_add(1)
                .ok_or_else(|| invalid("overflow"))?;
        }
    }
    next.consumed_prompts = consumed_prompts;
    if consumes_prompt && request.kind == ReservationKind::FirstLaunch {
        next.spent.insert((request.revision, request.slot));
    }
    match request.slot {
        AttemptSlot::Member { phase_index, .. } => {
            next.phase_revisions.insert(phase_index, request.revision);
        }
        AttemptSlot::Moderator => next.moderator_revision = request.revision,
    }
    let reserved_first_launches = next.reserved_first_launches();
    Ok(BudgetReservation {
        state: next,
        consumed_prompt: consumes_prompt,
        launch_counted,
        admission_counted,
        reserved_first_launches,
    })
}

fn validate_slot(state: &BudgetState, slot: AttemptSlot) -> RtResult<()> {
    match slot {
        AttemptSlot::Moderator => Ok(()),
        AttemptSlot::Member {
            phase_index,
            ordinal,
        } => {
            if phase_index > state.r || ordinal >= state.n {
                Err(invalid("slot"))
            } else {
                Ok(())
            }
        }
    }
}

/// Merge overlapping room intervals. Concurrent attempts do not add.
pub fn room_active_ms(intervals: &[(u64, u64)]) -> RtResult<u64> {
    if intervals.iter().any(|(start, end)| end < start) {
        return Err(invalid("invalid_budget"));
    }
    let mut ordered = intervals.to_vec();
    ordered.sort_unstable();
    let mut total = 0u64;
    let mut current: Option<(u64, u64)> = None;
    for (start, end) in ordered {
        match current {
            Some((open, close)) if start <= close => {
                current = Some((open, close.max(end)));
            }
            Some((open, close)) => {
                total = total
                    .checked_add(
                        close
                            .checked_sub(open)
                            .ok_or_else(|| invalid("underflow"))?,
                    )
                    .ok_or_else(|| invalid("overflow"))?;
                current = Some((start, end));
            }
            None => current = Some((start, end)),
        }
    }
    if let Some((open, close)) = current {
        total = total
            .checked_add(
                close
                    .checked_sub(open)
                    .ok_or_else(|| invalid("underflow"))?,
            )
            .ok_or_else(|| invalid("overflow"))?;
    }
    Ok(total)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairCoverage {
    pub configured_room_ms: u64,
    pub minimum_room_ms: u64,
    pub funded_repair_slots: u64,
    pub unfunded_repair_slots: u64,
}

/// Fund repair waves only from time the caller already configured.
/// A shorter budget is rejected, not raised.
pub fn repair_coverage(
    n: u32,
    r: u32,
    c: u32,
    plan: &BudgetPlan,
    configured_room_ms: u64,
) -> RtResult<RepairCoverage> {
    if n < 2 || c == 0 || c > n {
        return Err(invalid("invalid_budget"));
    }
    let waves = ceil_div(u64::from(n), u64::from(c))?;
    let phases = u64::from(r)
        .checked_add(1)
        .ok_or_else(|| invalid("overflow"))?;
    let minimum_room_ms = phases
        .checked_mul(waves)
        .and_then(|value| value.checked_mul(plan.slot_ms))
        .and_then(|value| value.checked_add(plan.slot_ms))
        .ok_or_else(|| invalid("overflow"))?;
    if configured_room_ms < minimum_room_ms {
        return Err(insufficient("insufficient_budget"));
    }
    let mut extra = configured_room_ms
        .checked_sub(minimum_room_ms)
        .ok_or_else(|| invalid("underflow"))?;
    let wave_cost = waves
        .checked_mul(plan.slot_ms)
        .ok_or_else(|| invalid("overflow"))?;
    let mut funded = 0u64;
    for _ in 0..phases {
        if extra >= wave_cost {
            extra = extra
                .checked_sub(wave_cost)
                .ok_or_else(|| invalid("underflow"))?;
            funded = funded
                .checked_add(u64::from(n))
                .ok_or_else(|| invalid("overflow"))?;
        }
    }
    if extra >= plan.slot_ms {
        funded = funded.checked_add(1).ok_or_else(|| invalid("overflow"))?;
    }
    if funded > plan.base_attempts {
        funded = plan.base_attempts;
    }
    let unfunded = plan
        .base_attempts
        .checked_sub(funded)
        .ok_or_else(|| invalid("underflow"))?;
    Ok(RepairCoverage {
        configured_room_ms,
        minimum_room_ms,
        funded_repair_slots: funded,
        unfunded_repair_slots: unfunded,
    })
}

pub const MAX_PREPAID_MS: u64 = 1_000;
pub const BUSINESS_EVENT_CAP: u64 = 10_000;
pub const TERMINAL_EVENT_RESERVE: u64 = 128;
pub const PROJECTION_MAX_BYTES: u64 = 64 * 1024;
pub const OBJECT_METADATA_BYTES: u64 = 256;
pub const ATTEMPT_PROJECTION_CAP: u64 = 8;
pub const PHASE_PROJECTION_CAP: u64 = 6;
pub const CONTROL_PROJECTION_CAP: u64 = 8;

/// Prepaid checkpoint. This updates the internal ledger only.
pub fn apply_time_checkpoint(
    ledger: &TimeLedger,
    now: MonoMs,
    prepaid_ms: u64,
) -> RtResult<TimeLedger> {
    if prepaid_ms == 0 || prepaid_ms > MAX_PREPAID_MS {
        return Err(invalid("prepaid_window"));
    }
    let room = ledger
        .remaining_room_ms
        .0
        .checked_sub(prepaid_ms)
        .ok_or_else(|| invalid("underflow"))?;
    let phase = ledger
        .remaining_phase_ms
        .0
        .checked_sub(prepaid_ms)
        .ok_or_else(|| invalid("underflow"))?;
    let seq = ledger
        .ledger_seq
        .0
        .checked_add(1)
        .ok_or_else(|| invalid("overflow"))?;
    let prepaid_until = now
        .0
        .checked_add(prepaid_ms)
        .ok_or_else(|| invalid("overflow"))?;
    Ok(TimeLedger {
        ledger_seq: Seq(seq),
        remaining_room_ms: DurationMs(room),
        remaining_phase_ms: DurationMs(phase),
        prepaid_until: MonoMs(prepaid_until),
        last_sample_mono: now,
    })
}

pub fn checkpoint_business_projections(_ledger: &TimeLedger) -> u64 {
    0
}

/// Refund only a known unused fragment of the last prepaid slice.
pub fn refund_known_unused(ledger: &TimeLedger, unused_ms: u64) -> RtResult<TimeLedger> {
    if unused_ms > MAX_PREPAID_MS {
        return Err(invalid("prepaid_window"));
    }
    let room = ledger
        .remaining_room_ms
        .0
        .checked_add(unused_ms)
        .ok_or_else(|| invalid("overflow"))?;
    let phase = ledger
        .remaining_phase_ms
        .0
        .checked_add(unused_ms)
        .ok_or_else(|| invalid("overflow"))?;
    Ok(TimeLedger {
        remaining_room_ms: DurationMs(room),
        remaining_phase_ms: DurationMs(phase),
        ..ledger.clone()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventReserve {
    pub ordinary_events: u64,
    pub terminal_events: u64,
    pub total_events: u64,
}

/// `12 + 8×max_attempts + 6×(R+2) + 8×32 + 64`, plus 128 terminal events.
pub fn event_reserve(max_attempts: u64, critique_rounds: u64) -> RtResult<EventReserve> {
    let attempt_events = max_attempts
        .checked_mul(ATTEMPT_PROJECTION_CAP)
        .ok_or_else(|| invalid("overflow"))?;
    let phase_events = critique_rounds
        .checked_add(2)
        .and_then(|value| value.checked_mul(PHASE_PROJECTION_CAP))
        .ok_or_else(|| invalid("overflow"))?;
    let control_events = CONTROL_PROJECTION_CAP
        .checked_mul(32)
        .ok_or_else(|| invalid("overflow"))?;
    let ordinary_events = 12u64
        .checked_add(attempt_events)
        .and_then(|value| value.checked_add(phase_events))
        .and_then(|value| value.checked_add(control_events))
        .and_then(|value| value.checked_add(64))
        .ok_or_else(|| invalid("overflow"))?;
    let total_events = ordinary_events
        .checked_add(TERMINAL_EVENT_RESERVE)
        .ok_or_else(|| invalid("overflow"))?;
    if total_events > BUSINESS_EVENT_CAP {
        return Err(too_large("event_cap"));
    }
    Ok(EventReserve {
        ordinary_events,
        terminal_events: TERMINAL_EVENT_RESERVE,
        total_events,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageReserve {
    pub projection_bytes: u64,
    pub diagnostic_bytes: u64,
    pub metadata_bytes: u64,
    pub total_bytes: u64,
}

/// Byte reserve for the same data directory. Counts alone are not a reserve.
/// `active_bindings` is the concurrency cap: a slot permit is held through cleanup.
pub fn storage_reserve(events: &EventReserve, active_bindings: u64) -> RtResult<StorageReserve> {
    let projection_bytes = events
        .total_events
        .checked_mul(PROJECTION_MAX_BYTES)
        .ok_or_else(|| invalid("overflow"))?;
    let diagnostic_bytes = active_bindings
        .checked_mul(PROJECTION_MAX_BYTES)
        .ok_or_else(|| invalid("overflow"))?;
    let metadata_bytes = events
        .total_events
        .checked_add(active_bindings)
        .and_then(|objects| objects.checked_mul(OBJECT_METADATA_BYTES))
        .ok_or_else(|| invalid("overflow"))?;
    let total_bytes = projection_bytes
        .checked_add(diagnostic_bytes)
        .and_then(|value| value.checked_add(metadata_bytes))
        .ok_or_else(|| invalid("overflow"))?;
    if total_bytes > crate::profile_suggestions::DATA_DIR_QUOTA_BYTES {
        return Err(insufficient("insufficient_budget"));
    }
    Ok(StorageReserve {
        projection_bytes,
        diagnostic_bytes,
        metadata_bytes,
        total_bytes,
    })
}

pub fn check_business_projection_caps(attempt: u64, phase: u64, control: u64) -> RtResult<()> {
    if attempt > ATTEMPT_PROJECTION_CAP
        || phase > PHASE_PROJECTION_CAP
        || control > CONTROL_PROJECTION_CAP
    {
        return Err(RtError::from_reason(
            InternalReason::ContextContractViolation,
        ));
    }
    Ok(())
}

pub(crate) fn invalid(reason: &str) -> RtError {
    error(
        ErrorCode::InvalidArgument,
        "The request is invalid.",
        reason,
    )
}

pub(crate) fn invalid_state(reason: &str) -> RtError {
    error(
        ErrorCode::InvalidState,
        "The room cannot accept this command.",
        reason,
    )
}

pub(crate) fn insufficient(reason: &str) -> RtError {
    error(
        ErrorCode::InsufficientBudget,
        "The budget is insufficient.",
        reason,
    )
}

pub(crate) fn unknown(reason: &str) -> RtError {
    error(
        ErrorCode::CapacityUnknown,
        "The context capacity is unknown.",
        reason,
    )
}

pub(crate) fn too_large(reason: &str) -> RtError {
    error(
        ErrorCode::ContextTooLarge,
        "The context is too large.",
        reason,
    )
}

fn error(code: ErrorCode, message: &str, reason: &str) -> RtError {
    RtError {
        code,
        message: message.to_string(),
        retryable: code.retryable(),
        current_revision: None,
        details: ErrorDetails {
            reason: Some(reason.to_string()),
            field_errors: Vec::new(),
        },
    }
}
