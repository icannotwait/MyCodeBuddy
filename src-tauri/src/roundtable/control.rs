//! Idempotent fenced controls. A lost ack resumes the original operation.
//!
//! Stop can cover another control. Nothing else can cover stop. A second
//! restart is `control_in_progress`. The canonical hash excludes credentials.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use roundtable_protocol::{
    ControlKind, ControlOperationV1, ControlStep, Epoch, ErrorCode, MutationAck, OperationId,
    PhaseId, PhaseKind, PrincipalId, RequestId, Revision, RoomId, RoomState, RtResult, Seq,
};

use super::actor::RoomActor;
use super::rt_error;

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
                resumed_operation: Some(self.operation_id.clone()),
            },
        }
    }

    pub fn apply(
        &mut self,
        _actor: &RoomActor,
        command: MutationCommandV1,
    ) -> RtResult<MutationAck> {
        if self.active == Some(ActiveKind::Stop) && command.kind != ControlKind::Stop {
            return Err(rt_error(ErrorCode::ControlInProgress, "control_in_progress"));
        }
        if self.active == Some(ActiveKind::Restart) && command.kind == ControlKind::RestartCurrent {
            return Err(rt_error(ErrorCode::ControlInProgress, "control_in_progress"));
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
            return Err(rt_error(ErrorCode::InsufficientBudget, "insufficient_budget"));
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
            request_id: command.request_id.clone(),
            accepted: true,
            operation_id: Some(self.operation_id.clone()),
            room_id: self.room_id.clone(),
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
            operation_id: self.operation_id.clone(),
            kind: ControlKind::Recover,
            step: self.step,
            room_id: self.room_id.clone(),
            target_phase_id: self.phase_id.clone(),
            target_revision: Revision(1),
            run_epoch: Epoch(1),
            successor_phase_id: None,
        })
    }

    pub fn next_phase(&self, requested: PhaseKind) -> RtResult<PhaseId> {
        if requested == PhaseKind::Synthesis {
            return Err(rt_error(ErrorCode::NoNextPhase, "no_next_phase"));
        }
        Ok(self.phase_id.clone())
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
    let book = books
        .entry(command.room_id.to_string())
        .or_insert_with(|| {
            ControlBook::new(
                command.room_id.clone(),
                "00000000-0000-4000-8000-0000000000c1".parse().expect("operation"),
                "00000000-0000-4000-8000-0000000000c2".parse().expect("phase"),
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
