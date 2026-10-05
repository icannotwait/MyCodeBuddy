//! Closing recovery. Ready waits until quarantine is still and the supervisor is up.

use roundtable_protocol::{ControlStep, ErrorCode, RtResult};

use super::control::MatrixRow;
use super::rt_error;
use super::service::{RoundtableService, ServiceReadiness};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    PublishFrozen,
    KeepSet,
    RestartSuccessor,
    StopOnly,
    PauseForRecovery,
    DoNotRepublish,
    ContinueCleanup,
    ResumeOperation,
}

#[derive(Clone, Debug)]
pub struct RecoveryState {
    pub row: MatrixRow,
    pub step: ControlStep,
    pub supervisor_online: bool,
    pub quarantine_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryReport {
    pub ready: bool,
}

pub fn recovery_action(state: &RecoveryState) -> RtResult<RecoveryAction> {
    let _ = state.step;
    Ok(match state.row {
        MatrixRow::NoControl => RecoveryAction::PublishFrozen,
        MatrixRow::Pause => RecoveryAction::KeepSet,
        MatrixRow::Restart => RecoveryAction::RestartSuccessor,
        MatrixRow::Stop => RecoveryAction::StopOnly,
        MatrixRow::RunningCrash => RecoveryAction::PauseForRecovery,
        MatrixRow::Published => RecoveryAction::DoNotRepublish,
        MatrixRow::Stopping => RecoveryAction::ContinueCleanup,
        MatrixRow::LostAck => RecoveryAction::ResumeOperation,
    })
}

pub async fn recover_service(service: &RoundtableService) -> RtResult<RecoveryReport> {
    if !matches!(service.readiness(), ServiceReadiness::Recovering) {
        return Err(rt_error(ErrorCode::InvalidState, "not_recovering"));
    }
    if service.quarantine_pending() || !service.supervisor_online() {
        return Ok(RecoveryReport { ready: false });
    }
    service.mark_ready_after_recovery()?;
    Ok(RecoveryReport { ready: true })
}
