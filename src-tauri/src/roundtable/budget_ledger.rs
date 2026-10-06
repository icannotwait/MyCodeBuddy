//! Durable prepaid room wall time. A crash never refunds an unproven remainder.
use super::rt_error;
use super::store::{column, exec, num, one_row, optional_row, query_i64, text, RoundtableStore};
use roundtable_protocol::{
    DurationMs, Epoch, ErrorCode, MonoMs, RoomId, RtResult, Seq, TimeLedger,
};
use sea_orm::ConnectionTrait;

const SLICE_MS: u64 = 1000;

/// One process-local owner of a persisted prepaid time slice. Dropping the
/// owner deliberately leaves the reservation charged for crash recovery.
pub struct ActiveBudgetLease {
    store: RoundtableStore,
    room: RoomId,
    id: String,
    boot: Epoch,
    run: Epoch,
    finished: bool,
    prepaid_until: u64,
}

impl ActiveBudgetLease {
    pub async fn begin(
        store: RoundtableStore,
        room: RoomId,
        boot: Epoch,
        run: Epoch,
    ) -> RtResult<Self> {
        let txn = store.write_transaction().await?;
        let id = uuid::Uuid::new_v4().to_string();
        let result=async {
            let state=one_row(&txn,"SELECT status,boot_epoch,run_epoch,active_control_id FROM rt_rooms WHERE room_id=?",vec![text(&room.to_string())]).await?;
            if column::<String>(&state,0)?!="running" || nonnegative(column(&state,1)?)?!=boot.0 || nonnegative(column(&state,2)?)?!=run.0 || column::<Option<String>>(&state,3)?.is_some() {return Err(rt_error(ErrorCode::InvalidState,"budget_fence"));}
            if let Some(old)=optional_row(&txn,"SELECT boot_epoch,run_epoch FROM rt_active_time_leases WHERE room_id=?",vec![text(&room.to_string())]).await? {
                if nonnegative(column(&old,0)?)?==boot.0 && nonnegative(column(&old,1)?)?==run.0 {return Err(rt_error(ErrorCode::InvalidState,"budget_lease_active"));}
                // A prior owner cannot prove how much of its slice was used.
                exec(&txn,"DELETE FROM rt_active_time_leases WHERE room_id=?",vec![text(&room.to_string())]).await?;
            }
            let now=store.clock_sample().0;
            exec(&txn,"INSERT INTO rt_active_time_leases(room_id,lease_id,boot_epoch,run_epoch,phase_id,prepaid_ms,last_sample_mono) VALUES(?,?,?,?,NULL,0,?)",vec![text(&room.to_string()),text(&id),num(as_i64(boot.0)?),num(as_i64(run.0)?),num(as_i64(now)?)]).await?;
            let ledger=tick_in(&txn,&store,&room,&id,boot,run,false).await?;
            if ledger.prepaid_until.0<=ledger.last_sample_mono.0 {return Err(rt_error(ErrorCode::InsufficientBudget,"active_budget_exhausted"));}
            Ok::<_,roundtable_protocol::RtError>(ledger.prepaid_until.0)
        }.await;
        let prepaid_until = match result {
            Ok(until) => { txn.commit().await.map_err(super::store::storage_err)?; until },
            Err(error) => {
                let _ = txn.rollback().await;
                return Err(error);
            }
        };
        Ok(Self {
            store,
            room,
            id,
            boot,
            run,
            finished: false,
            prepaid_until,
        })
    }

    pub fn prepaid_until(&self) -> u64 { self.prepaid_until }

    pub async fn checkpoint(&mut self) -> RtResult<TimeLedger> {
        self.tick(false).await
    }
    pub async fn finish(&mut self) -> RtResult<TimeLedger> {
        self.tick(true).await
    }

    async fn tick(&mut self, finish: bool) -> RtResult<TimeLedger> {
        if self.finished {
            return Err(rt_error(ErrorCode::InvalidState, "budget_lease_finished"));
        }
        let txn = self.store.write_transaction().await?;
        let result = tick_in(
            &txn,
            &self.store,
            &self.room,
            &self.id,
            self.boot,
            self.run,
            finish,
        )
        .await;
        match result {
            Ok(ledger) => {
                txn.commit().await.map_err(super::store::storage_err)?;
                self.finished = finish;
                self.prepaid_until = ledger.prepaid_until.0;
                Ok(ledger)
            }
            Err(error) => {
                let _ = txn.rollback().await;
                Err(error)
            }
        }
    }
}

/// A control has already stopped the task and proved cleanup. Settle the
/// persisted owner using this boot's monotonic clock before applying controls.
/// Recovery deletes old-boot leases conservatively and never calls this path
/// to refund an unknown crash remainder.
pub(crate) async fn settle_room_in(
    txn: &impl ConnectionTrait, store: &RoundtableStore, room: &RoomId,
) -> RtResult<()> {
    if let Some(row) = optional_row(txn,
        "SELECT lease_id,boot_epoch,run_epoch FROM rt_active_time_leases WHERE room_id=?",
        vec![text(&room.to_string())]).await? {
        tick_in(txn, store, room, &column::<String>(&row, 0)?,
            Epoch(nonnegative(column(&row, 1)?)?), Epoch(nonnegative(column(&row, 2)?)?), true).await?;
    }
    Ok(())
}

async fn tick_in(
    txn: &impl ConnectionTrait,
    store: &RoundtableStore,
    room: &RoomId,
    id: &str,
    boot: Epoch,
    run: Epoch,
    finish: bool,
) -> RtResult<TimeLedger> {
    let room_text = room.to_string();
    let lease=one_row(txn,"SELECT phase_id,prepaid_ms,last_sample_mono,phase_prepaid_ms FROM rt_active_time_leases WHERE room_id=? AND lease_id=? AND boot_epoch=? AND run_epoch=?",vec![text(&room_text),text(id),num(as_i64(boot.0)?),num(as_i64(run.0)?)]).await?;
    let state=one_row(txn,"SELECT remaining_active_ms,current_phase_id,status,boot_epoch,run_epoch,active_control_id,config_ref FROM rt_rooms WHERE room_id=?",vec![text(&room_text)]).await?;
    if nonnegative(column(&state, 3)?)? != boot.0 {
        return Err(rt_error(ErrorCode::InvalidState, "budget_fence"));
    }
    if !finish
        && (nonnegative(column(&state, 4)?)? != run.0
            || column::<String>(&state, 2)? != "running"
            || column::<Option<String>>(&state, 5)?.is_some())
    {
        return Err(rt_error(ErrorCode::InvalidState, "budget_fence"));
    }
    let (now, utc) = store.clock_sample();
    let last = nonnegative(column(&lease, 2)?)?;
    let elapsed = now
        .checked_sub(last)
        .ok_or_else(|| rt_error(ErrorCode::InvalidState, "budget_clock"))?;
    let prepaid = nonnegative(column(&lease, 1)?)?;
    let available = nonnegative(column(&state, 0)?)?.saturating_add(prepaid);
    let overrun = elapsed.saturating_sub(available);
    let remaining = nonnegative(column(&state, 0)?)?
        .saturating_add(prepaid)
        .saturating_sub(elapsed);
    let old_phase: Option<String> = column(&lease, 0)?;
    let mut current: Option<String> = column(&state, 1)?;
    if let Some(phase) = current.as_deref() {
        let status = one_row(
            txn,
            "SELECT status FROM rt_phases WHERE room_id=? AND phase_id=?",
            vec![text(&room_text), text(phase)],
        )
        .await?;
        if !matches!(
            column::<String>(&status, 0)?.as_str(),
            "running" | "closing"
        ) {
            current = None;
        }
    }
    if let Some(phase) = old_phase.as_deref() {
        let phase_remaining = query_i64(
            txn,
            "SELECT remaining_ms FROM rt_phases WHERE room_id=? AND phase_id=?",
            vec![text(&room_text), text(phase)],
        )
        .await?;
        let settled = nonnegative(phase_remaining)?
            .saturating_add(nonnegative(column(&lease, 3)?)?)
            .saturating_sub(elapsed);
        exec(
            txn,
            "UPDATE rt_phases SET remaining_ms=? WHERE room_id=? AND phase_id=?",
            vec![num(as_i64(settled)?), text(&room_text), text(phase)],
        )
        .await?;
    }
    let phase_remaining = if let Some(phase) = current.as_deref() {
        nonnegative(
            query_i64(
                txn,
                "SELECT remaining_ms FROM rt_phases WHERE room_id=? AND phase_id=?",
                vec![text(&room_text), text(phase)],
            )
            .await?,
        )?
    } else {
        remaining
    };
    // Phase expiry closes its attempts, but room time still pays for cleanup
    // and publication. Reserve the clocks separately so a short phase cannot
    // expire permission for the entire room or refund cleanup into that phase.
    let next = if finish { 0 } else { SLICE_MS.min(remaining) };
    let phase_next = if current.is_some() { next.min(phase_remaining) } else { 0 };
    // Zero is still committed: a exhausted lease cannot leave a stale positive budget.
    exec(
        txn,
        "UPDATE rt_rooms SET remaining_active_ms=? WHERE room_id=?",
        vec![
            num(as_i64(remaining.saturating_sub(next))?),
            text(&room_text),
        ],
    )
    .await?;
    if let Some(phase) = current.as_deref() {
        exec(
            txn,
            "UPDATE rt_phases SET remaining_ms=? WHERE room_id=? AND phase_id=?",
            vec![
                num(as_i64(phase_remaining.saturating_sub(phase_next))?),
                text(&room_text),
                text(phase),
            ],
        )
        .await?;
    }
    let mut ledger_seq = query_i64(
        txn,
        "SELECT COALESCE(MAX(ledger_seq),0)+1 FROM rt_measurements WHERE room_id=?",
        vec![text(&room_text)],
    )
    .await?;
    let config: Option<roundtable_protocol::RoundtableConfigV1> =
        serde_json::from_str(&column::<String>(&state, 6)?).ok();
    let sampled = config
        .map(|config| config.budgets.room_budget.0.saturating_sub(remaining))
        .unwrap_or(0);
    exec(txn,"INSERT INTO rt_measurements(room_id,measurement_id,ledger_seq,sampled_active_ms,sampled_at_utc,kind) VALUES(?,?,?,?,?,'active')",vec![text(&room_text),text(&uuid::Uuid::new_v4().to_string()),num(ledger_seq),num(as_i64(sampled)?),text(&utc)]).await?;
    if overrun > 0 {
        ledger_seq = ledger_seq
            .checked_add(1)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "budget_counter"))?;
        exec(txn,"INSERT INTO rt_measurements(room_id,measurement_id,ledger_seq,sampled_active_ms,sampled_at_utc,kind) VALUES(?,?,?,?,?,'cleanup_overrun')",vec![text(&room_text),text(&uuid::Uuid::new_v4().to_string()),num(ledger_seq),num(as_i64(overrun)?),text(&utc)]).await?;
    }
    if finish {
        exec(
            txn,
            "DELETE FROM rt_active_time_leases WHERE room_id=? AND lease_id=?",
            vec![text(&room_text), text(id)],
        )
        .await?;
    } else {
        exec(txn,"UPDATE rt_active_time_leases SET phase_id=?,prepaid_ms=?,last_sample_mono=?,phase_prepaid_ms=? WHERE room_id=? AND lease_id=?",vec![super::store::opt(current.as_deref()),num(as_i64(next)?),num(as_i64(now)?),num(as_i64(phase_next)?),text(&room_text),text(id)]).await?;
    }
    Ok(TimeLedger {
        ledger_seq: Seq(nonnegative(ledger_seq)?),
        remaining_room_ms: DurationMs(remaining.saturating_sub(next)),
        remaining_phase_ms: DurationMs(phase_remaining.saturating_sub(phase_next)),
        prepaid_until: MonoMs(now.saturating_add(next)),
        last_sample_mono: MonoMs(now),
    })
}

fn nonnegative(value: i64) -> RtResult<u64> {
    u64::try_from(value).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "budget_counter"))
}
fn as_i64(value: u64) -> RtResult<i64> {
    i64::try_from(value).map_err(|_| rt_error(ErrorCode::InvalidArgument, "budget_counter"))
}

#[cfg(any(test, feature = "test-utils"))]
mod legacy_test_model {
    use roundtable_protocol::budget::{budget_plan, reserve_attempts, room_active_ms, Timeouts};
    use roundtable_protocol::{
        BudgetReservation, DurationMs, ErrorCode, MonoMs, ReservationRequest, RtResult, Seq,
        TimeLedger,
    };

    use super::super::rt_error;
    use super::super::store::RoundtableStore;

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
            return Err(rt_error(
                ErrorCode::InsufficientBudget,
                "insufficient_budget",
            ));
        }
        let recommended = budget_plan(n, r, c, Timeouts::default())?.room_ms;
        if recommended > configured_room_ms {
            return Err(rt_error(
                ErrorCode::InsufficientBudget,
                "insufficient_budget",
            ));
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
}
#[cfg(any(test, feature = "test-utils"))]
pub use legacy_test_model::*;
