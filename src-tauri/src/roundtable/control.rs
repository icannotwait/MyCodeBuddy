//! Idempotent fenced controls. A lost ack resumes the original operation.
//!
//! Stop can cover another control. Nothing else can cover stop. A second
//! restart is `control_in_progress`. The canonical hash excludes credentials.

use super::rt_error;
use roundtable_protocol::{
    ControlKind, Epoch, ErrorCode, MutationAck, OperationId, RequestId, Revision, RoomId, RtResult,
    Seq,
};

/// Check only work that still needs a paid attempt. Frozen close/publication
/// and retained accepted slots do not acquire another attempt or time slot.
pub(crate) async fn validate_resume_in(
    conn: &impl sea_orm::ConnectionTrait,
    room: &RoomId,
    config: &roundtable_protocol::RoundtableConfigV1,
) -> RtResult<()> {
    use super::store::{column, num, one_row, optional_row, query_i64, rows, text};
    let room = room.to_string();
    let n = u32::try_from(config.participants.len())
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "participant_count"))?;
    let plan = roundtable_protocol::budget_plan(
        n,
        config.strategy.critique_rounds,
        config.concurrency,
        roundtable_protocol::Timeouts {
            launch_ms: 0,
            prompt_ms: config.timeouts.attempt_timeout.0,
            validation_ms: 0,
            cleanup_ms: 0,
        },
    )?;
    let nonnegative = |value: i64| {
        u64::try_from(value).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "budget_counter"))
    };
    let overflow = || rt_error(ErrorCode::InvalidArgument, "overflow");
    let budget_error = || rt_error(ErrorCode::InsufficientBudget, "insufficient_budget");
    let state = one_row(
        conn,
        "SELECT current_phase_id,remaining_active_ms FROM rt_rooms WHERE room_id=?",
        vec![text(&room)],
    )
    .await?;
    let current: Option<String> = column(&state, 0)?;
    let remaining_room = nonnegative(column(&state, 1)?)?;
    let start = if let Some(current) = current.as_deref() {
        nonnegative(
            query_i64(
                conn,
                "SELECT phase_index FROM rt_phases WHERE room_id=? AND phase_id=?",
                vec![text(&room), text(current)],
            )
            .await?,
        )?
    } else {
        nonnegative(query_i64(conn,
            "SELECT COALESCE(MAX(phase_index)+1,0) FROM rt_phases WHERE room_id=? AND status='published'",
            vec![text(&room)]).await?)?
    };
    let synthesis = u64::from(config.strategy.critique_rounds) + 1;
    if start > synthesis {
        return Err(rt_error(ErrorCode::InvalidState, "phase_index"));
    }
    let mut needed_attempts = 0u64;
    let mut needed_ms = 0u64;
    for index in start..=synthesis {
        let phase = optional_row(conn,
            "SELECT phase_id,status,remaining_ms,expected,quorum FROM rt_phases WHERE room_id=? AND phase_index=? AND status<>'superseded' ORDER BY revision DESC LIMIT 1",
            vec![text(&room),num(i64::try_from(index).map_err(|_|overflow())?)]).await?;
        let (executable, remaining_phase) =
            if let Some(phase) = phase {
                let status: String = column(&phase, 1)?;
                if matches!(status.as_str(), "closing" | "published") {
                    continue;
                }
                let phase_id: String = column(&phase, 0)?;
                let expected = nonnegative(column(&phase, 3)?)?;
                let quorum = nonnegative(column(&phase, 4)?)?;
                let turns = rows(conn,
                "SELECT status,admitted_attempt_count FROM rt_turns WHERE room_id=? AND phase_id=?",
                vec![text(&room),text(&phase_id)]).await?;
                let mut executable = expected
                    .checked_sub(turns.len() as u64)
                    .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "phase_slots"))?;
                let mut accepted = 0u64;
                for turn in turns {
                    let status: String = column(&turn, 0)?;
                    match status.as_str() {
                        "valid" | "accepted" => accepted += 1,
                        "abstained" => {}
                        _ if nonnegative(column(&turn, 1)?)? < 2 => executable += 1,
                        _ => {}
                    }
                }
                if accepted + executable < quorum {
                    return Err(rt_error(
                        ErrorCode::CannotReachQuorum,
                        "cannot_reach_quorum",
                    ));
                }
                (executable, nonnegative(column(&phase, 2)?)?)
            } else {
                (
                    if index == synthesis { 1 } else { u64::from(n) },
                    config.budgets.phase_budget.0,
                )
            };
        let concurrency = if index == synthesis {
            1
        } else {
            u64::from(config.concurrency)
        };
        let phase_ms = executable
            .div_ceil(concurrency)
            .checked_mul(plan.slot_ms)
            .ok_or_else(overflow)?;
        if phase_ms > remaining_phase {
            return Err(budget_error());
        }
        needed_ms = needed_ms.checked_add(phase_ms).ok_or_else(overflow)?;
        needed_attempts = needed_attempts
            .checked_add(executable)
            .ok_or_else(overflow)?;
    }
    let consumed = nonnegative(query_i64(conn,
        "SELECT COUNT(*) FROM rt_attempts WHERE room_id=? AND dispatch_state IN('sent','unknown')",
        vec![text(&room)]).await?)?;
    if needed_ms > remaining_room
        || consumed
            .checked_add(needed_attempts)
            .is_none_or(|attempts| attempts > plan.max_attempts)
    {
        return Err(budget_error());
    }
    Ok(())
}

// New revisions allocate a new phase allowance from the existing room balance.
// Returns the reserved minimum time, replacement phase cap and required attempts.
fn replacement_budget(
    config: &roundtable_protocol::RoundtableConfigV1,
    index: u64,
    room_remaining: u64,
) -> RtResult<(u64, u64, u64)> {
    let overflow = || rt_error(ErrorCode::InvalidArgument, "overflow");
    let n = config.participants.len() as u64;
    let c = u64::from(config.concurrency);
    if c == 0 || c > n {
        return Err(rt_error(ErrorCode::InvalidArgument, "invalid_budget"));
    }
    let discussion = (u64::from(config.strategy.critique_rounds) + 1)
        .checked_sub(index)
        .ok_or_else(|| rt_error(ErrorCode::InvalidState, "phase_index"))?;
    let current_waves = if discussion == 0 { 1 } else { n.div_ceil(c) };
    let waves = discussion
        .checked_mul(n.div_ceil(c))
        .and_then(|value| value.checked_add(1))
        .ok_or_else(overflow)?;
    let minimum_ms = waves
        .checked_mul(config.timeouts.attempt_timeout.0)
        .ok_or_else(overflow)?;
    let current_minimum = current_waves
        .checked_mul(config.timeouts.attempt_timeout.0)
        .ok_or_else(overflow)?;
    if room_remaining < minimum_ms || config.budgets.phase_budget.0 < current_minimum {
        return Err(rt_error(
            ErrorCode::InsufficientBudget,
            "insufficient_budget",
        ));
    }
    let phase_ms = config
        .budgets
        .phase_budget
        .0
        .min(room_remaining - (minimum_ms - current_minimum));
    let attempts = discussion
        .checked_mul(n)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(overflow)?;
    Ok((minimum_ms, phase_ms, attempts))
}

#[cfg(feature = "test-utils")]
mod legacy_test_model {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    use roundtable_protocol::{
        ControlKind, ControlOperationV1, ControlStep, Epoch, ErrorCode, MutationAck, OperationId,
        PhaseId, PhaseKind, PrincipalId, RequestId, Revision, RoomId, RoomState, RtResult, Seq,
    };

    use super::super::actor::RoomActor;
    use super::super::rt_error;

    #[derive(Clone, Debug)]
    pub struct MutationCommandV1 {
        pub room_id: RoomId,
        pub principal: PrincipalId,
        pub api_major: u32,
        pub request_id: RequestId,
        pub canonical_hash: String,
        pub kind: ControlKind,
        pub recovery_consent: bool,
        pub budget_ok: bool,
        pub hash_changed: bool,
        pub requested_phase: PhaseKind,
        pub actual_phase: PhaseId,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum MatrixRow {
        NoControl,
        Pause,
        Restart,
        Stop,
        RunningCrash,
        Published,
        Stopping,
        LostAck,
    }

    impl MatrixRow {
        pub const ALL: [Self; 8] = [
            Self::NoControl,
            Self::Pause,
            Self::Restart,
            Self::Stop,
            Self::RunningCrash,
            Self::Published,
            Self::Stopping,
            Self::LostAck,
        ];
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct MatrixOutcome {
        pub publish_frozen: bool,
        pub fill_slot: bool,
        pub status: RoomState,
        pub continues_cleanup: bool,
        pub published_repeats: u32,
        pub resumed_operation: Option<OperationId>,
    }

    struct SavedCommand {
        hash: String,
        ack: MutationAck,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ActiveKind {
        Restart,
        Stop,
        Other,
    }

    pub struct ControlBook {
        room_id: RoomId,
        operation_id: OperationId,
        phase_id: PhaseId,
        step: ControlStep,
        saved: BTreeMap<String, SavedCommand>,
        active: Option<ActiveKind>,
        prompts: u32,
        pauses: u32,
        pub successor_count: u32,
        pub published_old_revision_after_stop: u32,
        pub snapshot: String,
        pub admitted_speakers: Vec<String>,
        operations: u32,
    }

    impl ControlBook {
        pub fn new(room_id: RoomId, operation_id: OperationId, phase_id: PhaseId) -> Self {
            Self {
                room_id,
                operation_id,
                phase_id,
                step: ControlStep::Requested,
                saved: BTreeMap::new(),
                active: None,
                prompts: 0,
                pauses: 0,
                successor_count: 0,
                published_old_revision_after_stop: 0,
                snapshot: "before".to_string(),
                admitted_speakers: Vec::new(),
                operations: 1,
            }
        }

        pub fn operation_id(&self) -> &OperationId {
            &self.operation_id
        }

        pub fn prompts(&self) -> u32 {
            self.prompts
        }

        pub fn operations(&self) -> u32 {
            self.operations
        }

        /// A crash reopens the same step. It does not skip to the next one.
        pub fn reopen(&mut self, row: MatrixRow, step: ControlStep) -> MatrixOutcome {
            self.step = step;
            match row {
                MatrixRow::NoControl => MatrixOutcome {
                    publish_frozen: true,
                    fill_slot: false,
                    status: RoomState::Running,
                    continues_cleanup: false,
                    published_repeats: 0,
                    resumed_operation: None,
                },
                MatrixRow::Pause => MatrixOutcome {
                    publish_frozen: false,
                    fill_slot: false,
                    status: RoomState::Paused,
                    continues_cleanup: false,
                    published_repeats: 0,
                    resumed_operation: None,
                },
                MatrixRow::Restart => {
                    self.successor_count = 1;
                    MatrixOutcome {
                        publish_frozen: false,
                        fill_slot: false,
                        status: RoomState::Running,
                        continues_cleanup: false,
                        published_repeats: 0,
                        resumed_operation: None,
                    }
                }
                MatrixRow::Stop => MatrixOutcome {
                    publish_frozen: false,
                    fill_slot: false,
                    status: RoomState::Stopped,
                    continues_cleanup: false,
                    published_repeats: 0,
                    resumed_operation: None,
                },
                MatrixRow::RunningCrash => MatrixOutcome {
                    publish_frozen: false,
                    fill_slot: false,
                    status: RoomState::Paused,
                    continues_cleanup: false,
                    published_repeats: 0,
                    resumed_operation: None,
                },
                MatrixRow::Published => MatrixOutcome {
                    publish_frozen: false,
                    fill_slot: false,
                    status: RoomState::Completed,
                    continues_cleanup: false,
                    published_repeats: 0,
                    resumed_operation: None,
                },
                MatrixRow::Stopping => MatrixOutcome {
                    publish_frozen: false,
                    fill_slot: false,
                    status: RoomState::Stopping,
                    continues_cleanup: true,
                    published_repeats: 0,
                    resumed_operation: None,
                },
                MatrixRow::LostAck => MatrixOutcome {
                    publish_frozen: false,
                    fill_slot: false,
                    status: RoomState::Recovering,
                    continues_cleanup: false,
                    published_repeats: 0,
                    resumed_operation: Some(self.operation_id),
                },
            }
        }

        pub fn apply(
            &mut self,
            _actor: &RoomActor,
            command: MutationCommandV1,
        ) -> RtResult<MutationAck> {
            if self.active == Some(ActiveKind::Stop) && command.kind != ControlKind::Stop {
                return Err(rt_error(
                    ErrorCode::ControlInProgress,
                    "control_in_progress",
                ));
            }
            if self.active == Some(ActiveKind::Restart)
                && command.kind == ControlKind::RestartCurrent
            {
                return Err(rt_error(
                    ErrorCode::ControlInProgress,
                    "control_in_progress",
                ));
            }
            let key = scope_key(&command);
            if let Some(saved) = self.saved.get(&key) {
                if saved.hash == command.canonical_hash {
                    return Ok(saved.ack.clone());
                }
                return Err(rt_error(
                    ErrorCode::IdempotencyConflict,
                    "idempotency_conflict",
                ));
            }
            if command.kind == ControlKind::RestartCurrent && !command.budget_ok {
                return Err(rt_error(
                    ErrorCode::InsufficientBudget,
                    "insufficient_budget",
                ));
            }
            if command.kind == ControlKind::Stop {
                self.active = Some(ActiveKind::Stop);
                self.published_old_revision_after_stop = 0;
            } else if command.kind == ControlKind::RestartCurrent {
                self.active = Some(ActiveKind::Restart);
                self.successor_count = 1;
                self.snapshot = "restarted".to_string();
            } else {
                self.active = Some(ActiveKind::Other);
            }
            if command.recovery_consent {
                self.prompts = self.prompts.saturating_add(1);
            }
            let ack = MutationAck {
                request_id: command.request_id,
                accepted: true,
                operation_id: Some(self.operation_id),
                room_id: self.room_id,
                revision: Revision(1),
                run_epoch: Epoch(1),
                last_seq: Seq(0),
                status: RoomState::Running,
            };
            self.saved.insert(
                key,
                SavedCommand {
                    hash: command.canonical_hash,
                    ack: ack.clone(),
                },
            );
            Ok(ack)
        }

        pub fn note_uncommitted_processing(&mut self) {
            self.step = ControlStep::Requested;
        }

        pub fn advance(&mut self, operation: OperationId) -> RtResult<ControlOperationV1> {
            if operation != self.operation_id {
                return Err(rt_error(ErrorCode::InvalidState, "unknown_operation"));
            }
            self.step = match self.step {
                ControlStep::Requested => ControlStep::Revoking,
                ControlStep::Revoking => ControlStep::Cleaning,
                ControlStep::Cleaning => ControlStep::Applying,
                ControlStep::Applying => ControlStep::Done,
                ControlStep::Done | ControlStep::Blocked => self.step,
            };
            Ok(ControlOperationV1 {
                operation_id: self.operation_id,
                kind: ControlKind::Recover,
                step: self.step,
                room_id: self.room_id,
                target_phase_id: self.phase_id,
                target_revision: Revision(1),
                run_epoch: Epoch(1),
                successor_phase_id: None,
            })
        }

        pub fn next_phase(&self, requested: PhaseKind) -> RtResult<PhaseId> {
            if requested == PhaseKind::Synthesis {
                return Err(rt_error(ErrorCode::NoNextPhase, "no_next_phase"));
            }
            Ok(self.phase_id)
        }

        pub fn pause_again(&mut self) -> RtResult<()> {
            self.pauses = self.pauses.saturating_add(1);
            if self.pauses >= 2 {
                return Err(rt_error(
                    ErrorCode::CannotReachQuorum,
                    "cannot_reach_quorum",
                ));
            }
            Ok(())
        }

        pub fn retry_synthesis(&mut self, hash_changed: bool) -> RtResult<Vec<String>> {
            if hash_changed {
                return Err(rt_error(ErrorCode::InvalidState, "restart_required"));
            }
            self.admitted_speakers = vec!["moderator".to_string()];
            Ok(self.admitted_speakers.clone())
        }
    }

    fn scope_key(command: &MutationCommandV1) -> String {
        format!(
            "{}:{}:{}",
            command.principal, command.api_major, command.request_id
        )
    }

    fn books() -> &'static Mutex<BTreeMap<String, ControlBook>> {
        static BOOKS: OnceLock<Mutex<BTreeMap<String, ControlBook>>> = OnceLock::new();
        BOOKS.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    pub async fn apply_command(
        actor: &RoomActor,
        _principal: PrincipalId,
        command: MutationCommandV1,
    ) -> RtResult<MutationAck> {
        let mut books = books().lock().expect("controls");
        let book = books.entry(command.room_id.to_string()).or_insert_with(|| {
            ControlBook::new(
                command.room_id,
                "00000000-0000-4000-8000-0000000000c1"
                    .parse()
                    .expect("operation"),
                "00000000-0000-4000-8000-0000000000c2"
                    .parse()
                    .expect("phase"),
            )
        });
        book.apply(actor, command)
    }

    pub async fn advance_control(
        actor: &RoomActor,
        operation: OperationId,
    ) -> RtResult<ControlOperationV1> {
        let mut books = books().lock().expect("controls");
        let book = books
            .get_mut(&actor.room_id().to_string())
            .ok_or_else(|| rt_error(ErrorCode::InvalidState, "unknown_operation"))?;
        book.advance(operation)
    }
}
#[cfg(feature = "test-utils")]
pub use legacy_test_model::*;

/// Persisted control request. The authenticated actor, not the body, owns identity.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ControlRequest {
    pub room_id: RoomId,
    pub request_id: RequestId,
    pub expected_revision: Revision,
    pub kind: ControlKind,
    pub input: serde_json::Value,
    pub force_latest: bool,
}

async fn control_replay<C: sea_orm::ConnectionTrait>(
    conn: &C,
    actor: &roundtable_protocol::ActorContext,
    request: &ControlRequest,
) -> RtResult<Option<MutationAck>> {
    use super::store::{column, optional_row, text};
    let saved = optional_row(conn,"SELECT canonical_request_hash,method,status,immutable_response FROM rt_commands WHERE principal_id=? AND api_major=1 AND request_id=?",vec![text(&actor.principal_id().to_string()),text(&request.request_id.to_string())]).await?;
    let Some(saved) = saved else { return Ok(None) };
    let kind = serde_json::to_value(request.kind)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "control_kind"))?;
    if column::<String>(&saved, 0)? != roundtable_protocol::canonical_hash(request)?.to_hex()
        || Some(column::<String>(&saved, 1)?.as_str()) != kind.as_str()
    {
        return Err(rt_error(
            ErrorCode::IdempotencyConflict,
            "idempotency_conflict",
        ));
    }
    let ack: Option<String> = column(&saved, 3)?;
    if column::<String>(&saved, 2)? != "completed" || ack.is_none() {
        return Err(rt_error(
            ErrorCode::CommandInProgress,
            "command_in_progress",
        ));
    }
    serde_json::from_str(ack.as_deref().unwrap_or_default())
        .map(Some)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "command_response"))
}

impl super::store::RoundtableStore {
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn validate_resume_for_test(
        &self,
        room: &RoomId,
        config: &roundtable_protocol::RoundtableConfigV1,
    ) -> RtResult<()> {
        validate_resume_in(self.connection(), room, config).await
    }
    /// A completed command is a read, even when this coordinator cannot mutate.
    pub(crate) async fn replay_control(
        &self,
        actor: &roundtable_protocol::ActorContext,
        request: &ControlRequest,
    ) -> RtResult<Option<MutationAck>> {
        use super::store::{optional_row, text};
        if optional_row(
            self.connection(),
            "SELECT room_id FROM rt_rooms WHERE room_id=? AND principal_id=?",
            vec![
                text(&request.room_id.to_string()),
                text(&actor.principal_id().to_string()),
            ],
        )
        .await?
        .is_none()
        {
            return Err(rt_error(ErrorCode::Forbidden, "room_not_found"));
        }
        control_replay(self.connection(), actor, request).await
    }

    pub async fn request_control(
        &self,
        actor: &roundtable_protocol::ActorContext,
        request: &ControlRequest,
    ) -> RtResult<MutationAck> {
        use sea_orm::TransactionTrait;
        let txn = self
            .connection()
            .begin()
            .await
            .map_err(super::store::storage_err)?;
        let result = self.request_control_in(&txn, actor, request).await;
        match result {
            Ok(ack) => {
                txn.commit().await.map_err(super::store::storage_err)?;
                Ok(ack)
            }
            Err(err) => {
                let _ = txn.rollback().await;
                Err(err)
            }
        }
    }

    async fn request_control_in(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        actor: &roundtable_protocol::ActorContext,
        request: &ControlRequest,
    ) -> RtResult<MutationAck> {
        use super::store::{column, exec, num, one_row, opt, optional_row, text};
        let room = request.room_id.to_string();
        let principal = actor.principal_id().to_string();
        let row=optional_row(txn,"SELECT revision,status,current_phase_id,active_control_id,remaining_active_ms FROM rt_rooms WHERE room_id=? AND principal_id=?",vec![text(&room),text(&principal)]).await?
            .ok_or_else(||rt_error(ErrorCode::Forbidden,"room_not_found"))?;
        let hash = roundtable_protocol::canonical_hash(request)?.to_hex();
        if let Some(ack) = control_replay(txn, actor, request).await? {
            return Ok(ack);
        }
        let revision: i64 = column(&row, 0)?;
        let status: String = column(&row, 1)?;
        let phase: Option<String> = column(&row, 2)?;
        let active: Option<String> = column(&row, 3)?;
        if !(request.force_latest && request.kind == ControlKind::Stop)
            && u64::try_from(revision).ok() != Some(request.expected_revision.0)
        {
            let mut err = rt_error(ErrorCode::RevisionConflict, "revision_conflict");
            err.current_revision = Some(Revision(revision as u64));
            return Err(err);
        }
        if matches!(status.as_str(), "stopped" | "completed") {
            return Err(rt_error(ErrorCode::InvalidState, "room_terminal"));
        }
        if active.is_some() && request.kind != ControlKind::Stop {
            return Err(rt_error(
                ErrorCode::ControlInProgress,
                "control_in_progress",
            ));
        }
        if request.kind == ControlKind::Pause && status != "running" {
            return Err(rt_error(ErrorCode::InvalidState, "room_not_running"));
        }
        if request.kind == ControlKind::RetrySynthesis && status != "paused" {
            return Err(rt_error(ErrorCode::InvalidState, "room_not_paused"));
        }
        let phase_row=match phase.as_deref(){Some(id)=>Some(one_row(txn,"SELECT revision,status,snapshot_ref,remaining_ms FROM rt_phases WHERE room_id=? AND phase_id=?",vec![text(&room),text(id)]).await?),None=>None};
        let phase_revision = phase_row
            .as_ref()
            .map(|p| column::<i64>(p, 0))
            .transpose()?;
        let mut replacement_minimum_ms = None;
        if matches!(
            request.kind,
            ControlKind::RestartCurrent | ControlKind::RetrySynthesis
        ) {
            let p = phase_row
                .as_ref()
                .ok_or_else(|| rt_error(ErrorCode::NoNextPhase, "no_next_phase"))?;
            if request.kind == ControlKind::RetrySynthesis && column::<String>(p, 2)? != "synthesis"
            {
                return Err(rt_error(ErrorCode::InvalidState, "not_synthesis"));
            }
            if column::<String>(p, 1)? == "published" {
                return Err(rt_error(ErrorCode::InvalidState, "phase_published"));
            }
        }
        if matches!(
            request.kind,
            ControlKind::RestartCurrent | ControlKind::RetrySynthesis
        ) {
            let config_row = one_row(
                txn,
                "SELECT config_ref FROM rt_rooms WHERE room_id=?",
                vec![text(&room)],
            )
            .await?;
            let config: roundtable_protocol::RoundtableConfigV1 =
                serde_json::from_str(&column::<String>(&config_row, 0)?)
                    .map_err(|_| rt_error(ErrorCode::InvalidState, "room_config"))?;
            let current = phase
                .as_deref()
                .ok_or_else(|| rt_error(ErrorCode::NoNextPhase, "no_next_phase"))?;
            let index = super::store::query_i64(
                txn,
                "SELECT phase_index FROM rt_phases WHERE room_id=? AND phase_id=?",
                vec![text(&room), text(current)],
            )
            .await?;
            let n = config.participants.len() as u64;
            let (minimum_ms, _, needed) = replacement_budget(
                &config,
                u64::try_from(index)
                    .map_err(|_| rt_error(ErrorCode::InvalidState, "phase_index"))?,
                u64::try_from(column::<i64>(&row, 4)?)
                    .map_err(|_| rt_error(ErrorCode::InvalidState, "room_budget"))?,
            )?;
            replacement_minimum_ms = Some(minimum_ms);
            let plan = roundtable_protocol::budget_plan(
                n as u32,
                config.strategy.critique_rounds,
                config.concurrency,
                roundtable_protocol::Timeouts {
                    launch_ms: 0,
                    prompt_ms: config.timeouts.attempt_timeout.0,
                    validation_ms: 0,
                    cleanup_ms: 0,
                },
            )?;
            let consumed=super::store::query_i64(txn,"SELECT COUNT(*) FROM rt_attempts WHERE room_id=? AND dispatch_state IN('sent','unknown')",vec![text(&room)]).await? as u64;
            if consumed
                .checked_add(needed)
                .is_none_or(|total| total > plan.max_attempts)
            {
                return Err(rt_error(
                    ErrorCode::InsufficientBudget,
                    "insufficient_budget",
                ));
            }
        }
        if request.kind == ControlKind::RestartCurrent
            && request
                .input
                .get("text")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.trim().is_empty() || value.len() > 8192)
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "input_text"));
        }
        if request.kind == ControlKind::RestartCurrent {
            let config_row = one_row(
                txn,
                "SELECT config_ref FROM rt_rooms WHERE room_id=?",
                vec![text(&room)],
            )
            .await?;
            let config: roundtable_protocol::RoundtableConfigV1 =
                serde_json::from_str(&column::<String>(&config_row, 0)?)
                    .map_err(|_| rt_error(ErrorCode::InvalidState, "room_config"))?;
            let used=super::store::query_i64(txn,"SELECT COALESCE(SUM(length(CAST(text AS BLOB))),0) FROM rt_user_inputs WHERE room_id=?",vec![text(&room)]).await?;
            let bytes = request.input["text"].as_str().map_or(0, str::len) as u64;
            if u64::try_from(used)
                .ok()
                .and_then(|used| used.checked_add(bytes))
                .is_none_or(|total| total > config.quotas.interjection_byte_limit.0)
            {
                return Err(rt_error(
                    ErrorCode::InsufficientBudget,
                    "interjection_bytes",
                ));
            }
        }
        let operation = uuid::Uuid::new_v4().to_string();
        if let Some(active) = active.as_deref() {
            exec(txn,"UPDATE rt_control_operations SET step='done',status='superseded' WHERE room_id=? AND operation_id=?",vec![text(&room),text(active)]).await?;
        }
        let next_status = if request.kind == ControlKind::Stop {
            "stopping"
        } else {
            "pausing"
        };
        exec(txn,"UPDATE rt_rooms SET run_epoch=run_epoch+1,revision=revision+1,last_seq=last_seq+1,status=?,active_control_id=? WHERE room_id=?",vec![text(next_status),text(&operation),text(&room)]).await?;
        let epoch = super::store::query_i64(
            txn,
            "SELECT run_epoch FROM rt_rooms WHERE room_id=?",
            vec![text(&room)],
        )
        .await?;
        let kind = serde_json::to_value(request.kind)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "control_kind"))?;
        exec(txn,"INSERT INTO rt_control_operations(room_id,operation_id,kind,target_phase_id,target_revision,requested_epoch,step,status) VALUES(?,?,?,?,?,?,'cleaning','open')",vec![text(&room),text(&operation),text(kind.as_str().unwrap_or("")),opt(phase.as_deref()),phase_revision.into(),num(epoch)]).await?;
        if let Some(active) = active.as_deref() {
            exec(txn,"UPDATE rt_control_operations SET superseded_by=? WHERE room_id=? AND operation_id=?",vec![text(&operation),text(&room),text(active)]).await?;
            exec(txn,"UPDATE rt_budget_reservations SET state='released' WHERE room_id=? AND reservation_id=(SELECT budget_reservation_id FROM rt_control_operations WHERE room_id=? AND operation_id=?) AND state='reserved'",vec![text(&room),text(&room),text(active)]).await?;
        }
        exec(
            txn,
            "INSERT INTO rt_control_inputs(room_id,operation_id,input_json) VALUES(?,?,?)",
            vec![
                text(&room),
                text(&operation),
                text(&request.input.to_string()),
            ],
        )
        .await?;
        if matches!(
            request.kind,
            ControlKind::RestartCurrent | ControlKind::RetrySynthesis
        ) {
            let amount = i64::try_from(
                replacement_minimum_ms
                    .ok_or_else(|| rt_error(ErrorCode::InvalidState, "replacement_budget"))?,
            )
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
            let reservation = uuid::Uuid::new_v4().to_string();
            exec(txn,"INSERT INTO rt_budget_reservations(room_id,reservation_id,principal_id,purpose,amount_ms,amount_bytes,state) VALUES(?,?,?,'restart',?,0,'reserved')",vec![text(&room),text(&reservation),text(&principal),num(amount)]).await?;
            exec(txn,"UPDATE rt_control_operations SET budget_reservation_id=? WHERE room_id=? AND operation_id=?",vec![text(&reservation),text(&room),text(&operation)]).await?;
        }
        exec(txn,"UPDATE rt_attempts SET state=CASE WHEN state='admitting' THEN 'uncertain' ELSE 'interrupted' END,cleanup_state='cleaning' WHERE room_id=? AND state IN ('reserved','launching','admitting','admitted','streaming','validating','active')",vec![text(&room)]).await?;
        exec(
            txn,
            "DELETE FROM rt_active_time_leases WHERE room_id=?",
            vec![text(&room)],
        )
        .await?;
        self.emit_current_in(txn, &room, "control").await?;
        let ack = control_ack(
            txn,
            &room,
            request.request_id,
            Some(
                operation
                    .parse()
                    .map_err(|_| rt_error(ErrorCode::InvalidState, "operation_id"))?,
            ),
        )
        .await?;
        exec(txn,"INSERT INTO rt_commands(principal_id,api_major,request_id,canonical_request_hash,method,room_id,status,immutable_response,operation_id) VALUES(?,1,?,?,?,?,'completed',?,?)",vec![text(&principal),text(&request.request_id.to_string()),text(&hash),text(kind.as_str().unwrap_or("")),text(&room),text(&serde_json::to_string(&ack).map_err(|_|rt_error(ErrorCode::InvalidState,"ack"))?),text(&operation)]).await?;
        Ok(ack)
    }

    pub async fn advance_durable_control(
        &self,
        actor: &roundtable_protocol::ActorContext,
        room_id: RoomId,
        operation_id: OperationId,
        cleanup_confirmed: bool,
    ) -> RtResult<MutationAck> {
        use super::store::{column, exec, num, one_row, opt, query_i64, text};
        use sea_orm::TransactionTrait;
        let txn = self
            .connection()
            .begin()
            .await
            .map_err(super::store::storage_err)?;
        let result=async {
            let room=room_id.to_string();let operation=operation_id.to_string();
            one_row(&txn,"SELECT room_id FROM rt_rooms WHERE room_id=? AND principal_id=?",vec![text(&room),text(&actor.principal_id().to_string())]).await?;
            let saved=one_row(&txn,"SELECT kind,step,target_phase_id,target_revision,successor_phase_id FROM rt_control_operations WHERE room_id=? AND operation_id=?",vec![text(&room),text(&operation)]).await?;
            let request=one_row(&txn,"SELECT request_id FROM rt_commands WHERE room_id=? AND operation_id=?",vec![text(&room),text(&operation)]).await?;
            let request_id=column::<String>(&request,0)?.parse().map_err(|_|rt_error(ErrorCode::InvalidState,"request_id"))?;
            if column::<String>(&saved,1)?=="done" {return control_ack(&txn,&room,request_id,Some(operation_id)).await;}
            let active=one_row(&txn,"SELECT active_control_id,run_epoch FROM rt_rooms WHERE room_id=?",vec![text(&room)]).await?;
            if column::<Option<String>>(&active,0)?.as_deref()!=Some(&operation) {return Err(rt_error(ErrorCode::ControlInProgress,"control_superseded"));}
            let pending=query_i64(&txn,"SELECT COUNT(*) FROM rt_attempts WHERE room_id=? AND cleanup_state<>'confirmed'",vec![text(&room)]).await?;
            let live=query_i64(&txn,"SELECT COUNT(*) FROM rt_bindings b JOIN rt_launch_intents l ON l.incarnation=b.incarnation WHERE b.room_id=? AND l.reaped=0",vec![text(&room)]).await?;
            if live>0 || (pending>0 && !cleanup_confirmed) {return Err(rt_error(ErrorCode::InvalidState,"cleanup_incomplete"));}
            if cleanup_confirmed {
                exec(&txn,"UPDATE rt_attempts SET cleanup_state='confirmed' WHERE room_id=?",vec![text(&room)]).await?;
                exec(&txn,"UPDATE rt_bindings SET state='retired',retire_reason='control' WHERE room_id=?",vec![text(&room)]).await?;
            }
            let kind:String=column(&saved,0)?;let phase:Option<String>=column(&saved,2)?;
            let mut successor:Option<String>=column(&saved,4)?;
            if matches!(kind.as_str(),"restart_current"|"retry_synthesis") && successor.is_none() {
                let phase=phase.as_deref().ok_or_else(||rt_error(ErrorCode::NoNextPhase,"no_next_phase"))?;
                let budget_row=one_row(&txn,"SELECT r.config_ref,r.remaining_active_ms,p.phase_index FROM rt_rooms r JOIN rt_phases p ON p.room_id=r.room_id AND p.phase_id=? WHERE r.room_id=?",vec![text(phase),text(&room)]).await?;
                let config:roundtable_protocol::RoundtableConfigV1=serde_json::from_str(&column::<String>(&budget_row,0)?).map_err(|_|rt_error(ErrorCode::InvalidState,"room_config"))?;
                let (_,phase_ms,_)=replacement_budget(&config,
                    u64::try_from(column::<i64>(&budget_row,2)?).map_err(|_|rt_error(ErrorCode::InvalidState,"phase_index"))?,
                    u64::try_from(column::<i64>(&budget_row,1)?).map_err(|_|rt_error(ErrorCode::InvalidState,"room_budget"))?)?;
                let phase_ms=i64::try_from(phase_ms).map_err(|_|rt_error(ErrorCode::InvalidArgument,"overflow"))?;
                let new_id=uuid::Uuid::new_v4().to_string();
                exec(&txn,"INSERT INTO rt_phases(room_id,phase_id,phase_index,revision,status,snapshot_ref,snapshot_hash,expected,quorum,remaining_ms,manifest_id) SELECT room_id,?,phase_index,revision+1,'ready',snapshot_ref,snapshot_hash,expected,quorum,?,manifest_id FROM rt_phases WHERE room_id=? AND phase_id=? AND status<>'published'",vec![text(&new_id),num(phase_ms),text(&room),text(phase)]).await?;
                exec(&txn,"UPDATE rt_phases SET status='superseded' WHERE room_id=? AND phase_id=? AND status<>'published'",vec![text(&room),text(phase)]).await?;
                exec(&txn,"UPDATE rt_rooms SET current_phase_id=? WHERE room_id=?",vec![text(&new_id),text(&room)]).await?;
                if kind=="restart_current" {
                    let input=one_row(&txn,"SELECT input_json FROM rt_control_inputs WHERE room_id=? AND operation_id=?",vec![text(&room),text(&operation)]).await?;
                    let raw:String=column(&input,0)?;
                    let body:serde_json::Value=serde_json::from_str(&raw).map_err(|_|rt_error(ErrorCode::InvalidState,"control_input"))?;
                    let value=body.get("text").and_then(serde_json::Value::as_str).ok_or_else(||rt_error(ErrorCode::InvalidState,"control_input"))?;
                    exec(&txn,"INSERT INTO rt_user_inputs(room_id,input_id,text,mode,accepted_seq,target_phase_index,applied_phase_id,applied_seq,state) SELECT room_id,?,?,'restart_current',last_seq,(SELECT phase_index FROM rt_phases WHERE room_id=? AND phase_id=?),NULL,NULL,'queued' FROM rt_rooms WHERE room_id=?",vec![text(&uuid::Uuid::new_v4().to_string()),text(value),text(&room),text(&new_id),text(&room)]).await?;
                }
                successor=Some(new_id);
            }
            // The control reserves admission capacity only. Creating a ready
            // successor spends no wall time; the explicit run owns its lease.
            exec(&txn,"UPDATE rt_budget_reservations SET state='released' WHERE room_id=? AND reservation_id=(SELECT budget_reservation_id FROM rt_control_operations WHERE room_id=? AND operation_id=?) AND state='reserved'",vec![text(&room),text(&room),text(&operation)]).await?;
            let status=if kind=="stop" {"stopped"} else {"paused"};
            exec(&txn,"UPDATE rt_control_operations SET step='done',status='completed',successor_phase_id=? WHERE room_id=? AND operation_id=?",vec![opt(successor.as_deref()),text(&room),text(&operation)]).await?;
            exec(&txn,"UPDATE rt_rooms SET status=?,active_control_id=NULL,revision=revision+1,last_seq=last_seq+1 WHERE room_id=?",vec![text(status),text(&room)]).await?;
            self.emit_current_in(&txn,&room,"control").await?;
            control_ack(&txn,&room,request_id,Some(operation_id)).await
        }.await;
        match result {
            Ok(ack) => {
                txn.commit().await.map_err(super::store::storage_err)?;
                Ok(ack)
            }
            Err(err) => {
                let _ = txn.rollback().await;
                Err(err)
            }
        }
    }

    /// Called only after owned processes/mailboxes have been reconciled. It never restarts a paid prompt.
    pub async fn recover_durable(&self, boot_epoch: u64) -> RtResult<()> {
        use super::store::{column, exec, num, query_i64, rows, text};
        use sea_orm::TransactionTrait;
        let txn = self
            .connection()
            .begin()
            .await
            .map_err(super::store::storage_err)?;
        let mut controls = Vec::new();
        let result=async {
            let boot=i64::try_from(boot_epoch).map_err(|_|rt_error(ErrorCode::InvalidArgument,"boot_epoch"))?;
            let rooms=rows(&txn,"SELECT room_id,principal_id,active_control_id,status FROM rt_rooms WHERE (status IN ('running','pausing','stopping','recovering','paused') OR active_control_id IS NOT NULL) AND boot_epoch<>?",vec![num(boot)]).await?;
            for row in rooms {
                let room:String=column(&row,0)?;
                if query_i64(&txn,"SELECT COUNT(*) FROM rt_bindings b JOIN rt_launch_intents l ON l.incarnation=b.incarnation WHERE b.room_id=? AND l.reaped=0",vec![text(&room)]).await?!=0 {return Err(rt_error(ErrorCode::InvalidState,"cleanup_incomplete"));}
                exec(&txn,"UPDATE rt_attempts SET state=CASE WHEN state IN ('admitting','uncertain') THEN 'uncertain' ELSE 'interrupted' END WHERE room_id=? AND state IN ('reserved','launching','admitting','admitted','streaming','validating','active','uncertain')",vec![text(&room)]).await?;
                exec(&txn,"UPDATE rt_attempts SET cleanup_state='confirmed' WHERE room_id=?",vec![text(&room)]).await?;
                exec(&txn,"UPDATE rt_bindings SET state='retired',retire_reason='recovery' WHERE room_id=?",vec![text(&room)]).await?;
                exec(&txn,"UPDATE rt_rooms SET blocked_reason=CASE WHEN status='running' THEN 'recovery_required' ELSE blocked_reason END,status=CASE WHEN status='stopping' THEN 'stopped' ELSE 'paused' END,boot_epoch=?,run_epoch=run_epoch+1,revision=revision+1,last_seq=last_seq+1 WHERE room_id=?",vec![num(boot),text(&room)]).await?;
                exec(&txn,"DELETE FROM rt_active_time_leases WHERE room_id=?",vec![text(&room)]).await?;
                self.emit_current_in(&txn,&room,"recovery").await?;
                if column::<String>(&row,3)?=="running" && column::<Option<String>>(&row,2)?.is_none() {self.recover_frozen_in(&txn,&room).await?;}
                if let Some(operation)=column::<Option<String>>(&row,2)? { controls.push((room,column::<String>(&row,1)?,operation)); }
            }
            Ok::<(),roundtable_protocol::RtError>(())
        }.await;
        match result {
            Ok(()) => txn.commit().await.map_err(super::store::storage_err)?,
            Err(err) => {
                let _ = txn.rollback().await;
                return Err(err);
            }
        }
        for (room, principal, operation) in controls {
            use roundtable_protocol::{ActorContext, ClientIdentity, ClientKind, OperatorScope};
            let parse_error = || rt_error(ErrorCode::StorageUnavailable, "control_identity");
            let actor = ActorContext::from_trusted_entry(
                principal.parse().map_err(|_| parse_error())?,
                OperatorScope::SingleOperator,
                ClientIdentity {
                    kind: ClientKind::Desktop,
                    session_ref: "recovery".into(),
                },
            );
            self.advance_durable_control(
                &actor,
                room.parse().map_err(|_| parse_error())?,
                operation.parse().map_err(|_| parse_error())?,
                false,
            )
            .await?;
        }
        Ok(())
    }
}

async fn control_ack(
    txn: &impl sea_orm::ConnectionTrait,
    room: &str,
    request_id: RequestId,
    operation_id: Option<OperationId>,
) -> RtResult<MutationAck> {
    use super::store::{column, one_row, text};
    let row = one_row(
        txn,
        "SELECT revision,run_epoch,last_seq,status FROM rt_rooms WHERE room_id=?",
        vec![text(room)],
    )
    .await?;
    let status: String = column(&row, 3)?;
    Ok(MutationAck {
        request_id,
        accepted: true,
        operation_id,
        room_id: room
            .parse()
            .map_err(|_| rt_error(ErrorCode::InvalidState, "room_id"))?,
        revision: Revision(column::<i64>(&row, 0)? as u64),
        run_epoch: Epoch(column::<i64>(&row, 1)? as u64),
        last_seq: Seq(column::<i64>(&row, 2)? as u64),
        status: serde_json::from_value(serde_json::Value::String(status))
            .map_err(|_| rt_error(ErrorCode::InvalidState, "room_status"))?,
    })
}
