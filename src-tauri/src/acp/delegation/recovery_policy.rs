use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::acp::termination::{
    AcpDisconnectOrigin, AcpTerminationClassification, AcpTerminationReason, AcpTerminationSource,
    DelegationTerminationAuditV1, ParsedDelegationTermination,
};
use crate::db::entities::delegation_task_run::{AdmissionClass, DelegationRunStatus};

const FINGERPRINT_VERSION: &str = "delegation_recovery_v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySourceSnapshot {
    pub source_task_id: String,
    pub lineage_root_task_id: String,
    pub generation: i64,
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub workspace_path: Option<String>,
    pub route_fingerprint: Option<String>,
    pub work_unit_key: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub child_connection_id: Option<String>,
    pub history_only: bool,
    pub is_latest: bool,
    pub has_active_run: bool,
    pub child_superseded: bool,
    pub child_ownership_valid: bool,
    pub agent_type_matches: bool,
    pub run_status: DelegationRunStatus,
    pub error_code: Option<String>,
    pub admission_class: AdmissionClass,
    pub parsed_termination: ParsedDelegationTermination,
    pub reached_running: bool,
    pub launch_snapshot_complete: bool,
    pub launch_snapshot_version: Option<String>,
    pub mode_id: Option<String>,
    pub config_values_json: Option<String>,
    /// SHA-256 of the external session identity. Raw session ids never enter
    /// the policy snapshot or its canonical fingerprint input.
    pub external_session_identity_hash: Option<String>,
    pub replaced_task_id: Option<String>,
    pub replacement_reason: Option<ReplacementReason>,
    pub recovery_authorization_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRailSnapshot {
    pub agent_supports_reuse: bool,
    pub unexpected_continue_budget_available: bool,
    pub replacement_budget_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryDecision {
    pub source_task_id: String,
    pub source_state_fingerprint: String,
    pub disposition: RecoveryDisposition,
    pub confirmation: RecoveryConfirmation,
    pub cause_code: RecoveryCauseCode,
    pub risk_class: RecoveryRiskClass,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDisposition {
    Continue {
        admission_class: AdmissionClass,
    },
    FreshDispatch,
    Replace {
        replacement_reason: ReplacementReason,
    },
    Stop {
        code: RecoveryStopCode,
    },
    InconsistentDurableState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestedRecoveryOperation {
    Inspect,
    Continue,
    FreshDispatch,
    Replace {
        replacement_reason: ReplacementReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementReason {
    Unresumable,
    BudgetExhaustedContinue,
    NotSupported,
    AdmissionFailed,
    AdmissionUnknown,
}

impl ReplacementReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unresumable => "unresumable",
            Self::BudgetExhaustedContinue => "budget_exhausted_continue",
            Self::NotSupported => "not_supported",
            Self::AdmissionFailed => "admission_failed",
            Self::AdmissionUnknown => "admission_unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "unresumable" => Self::Unresumable,
            "budget_exhausted_continue" => Self::BudgetExhaustedContinue,
            "not_supported" => Self::NotSupported,
            "admission_failed" => Self::AdmissionFailed,
            "admission_unknown" => Self::AdmissionUnknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRiskClass {
    Normal,
    ExecutionMayHaveOccurred,
    ExplicitUserStop,
    LegacyUnknownOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryConfirmation {
    NotRequired,
    Required,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCauseCode {
    Completed,
    RevisionEligibleFailure,
    UnexpectedTransportLoss,
    UnexpectedProcessLoss,
    UnexpectedSessionLoss,
    UnexpectedHostRestart,
    UnexpectedChildConnectionLoss,
    ParentCanceled,
    ParentTurnFailed,
    JoinAbandoned,
    UserCancelled,
    ToolStalledTimeout,
    LegacyParentDisconnect,
    IntentionalParentDisconnect,
    MalformedTerminationAudit,
    PreAdmissionRetry,
    PreAdmissionAbort,
    AdmissionFailed,
    AdmissionUnknown,
    MissingResumeIdentity,
    UnsupportedReuse,
    PersistedUnresumable,
    ContinueBudgetExhausted,
    ReplacementBudgetExhausted,
    RouteRejected,
    StaleSource,
    BusySource,
    StructuralFence,
    ContradictoryEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStopCode {
    BusyThread,
    StaleTaskId,
    NotContinuable,
    RouteRejected,
    ReplacementBudgetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAction {
    Continue {
        admission_class: AdmissionClass,
    },
    FreshDispatch,
    Replace {
        replacement_reason: ReplacementReason,
    },
}

impl RecoveryDecision {
    pub fn requires_authorization(&self) -> bool {
        self.confirmation == RecoveryConfirmation::Required
            && matches!(
                self.disposition,
                RecoveryDisposition::Continue { .. }
                    | RecoveryDisposition::FreshDispatch
                    | RecoveryDisposition::Replace { .. }
            )
    }

    pub fn operation_matches(&self, operation: RequestedRecoveryOperation) -> bool {
        match (&self.disposition, operation) {
            (_, RequestedRecoveryOperation::Inspect)
            | (RecoveryDisposition::Continue { .. }, RequestedRecoveryOperation::Continue)
            | (RecoveryDisposition::FreshDispatch, RequestedRecoveryOperation::FreshDispatch) => {
                true
            }
            (
                RecoveryDisposition::Replace {
                    replacement_reason: decided,
                },
                RequestedRecoveryOperation::Replace {
                    replacement_reason: requested,
                },
            ) => decided == &requested,
            _ => false,
        }
    }

    pub fn proposed_action(&self) -> Option<RecoveryAction> {
        match &self.disposition {
            RecoveryDisposition::Continue { admission_class } => Some(RecoveryAction::Continue {
                admission_class: admission_class.clone(),
            }),
            RecoveryDisposition::FreshDispatch => Some(RecoveryAction::FreshDispatch),
            RecoveryDisposition::Replace { replacement_reason } => Some(RecoveryAction::Replace {
                replacement_reason: replacement_reason.clone(),
            }),
            RecoveryDisposition::Stop { .. } | RecoveryDisposition::InconsistentDurableState => {
                None
            }
        }
    }

    pub fn stop_code(&self) -> Option<RecoveryStopCode> {
        match &self.disposition {
            RecoveryDisposition::Stop { code } => Some(code.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct CauseAssessment {
    cause_code: RecoveryCauseCode,
    confirmation: RecoveryConfirmation,
    risk_class: RecoveryRiskClass,
    admission_class: AdmissionClass,
}

impl CauseAssessment {
    fn normal(cause_code: RecoveryCauseCode, admission_class: AdmissionClass) -> Self {
        Self {
            cause_code,
            confirmation: RecoveryConfirmation::NotRequired,
            risk_class: RecoveryRiskClass::Normal,
            admission_class,
        }
    }

    fn confirmation_required(cause_code: RecoveryCauseCode, risk_class: RecoveryRiskClass) -> Self {
        Self {
            cause_code,
            confirmation: RecoveryConfirmation::Required,
            risk_class,
            admission_class: AdmissionClass::UnexpectedContinue,
        }
    }
}

pub fn hash_external_session_identity(external_session_id: &str) -> String {
    hex_lower(&Sha256::digest(external_session_id.as_bytes()))
}

pub fn decide_delegation_recovery(
    source: &RecoverySourceSnapshot,
    rails: &RecoveryRailSnapshot,
    _operation: RequestedRecoveryOperation,
) -> RecoveryDecision {
    let fingerprint = source_state_fingerprint(source, rails);
    let decision = |disposition, confirmation, cause_code, risk_class| RecoveryDecision {
        source_task_id: source.source_task_id.clone(),
        source_state_fingerprint: fingerprint.clone(),
        disposition,
        confirmation,
        cause_code,
        risk_class,
    };

    // Lifecycle fences precede every cause/authorization path.
    if source.has_active_run
        || matches!(
            source.run_status,
            DelegationRunStatus::Reserving | DelegationRunStatus::Running
        )
    {
        return decision(
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::BusyThread,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::BusySource,
            RecoveryRiskClass::Normal,
        );
    }
    if !source.is_latest {
        return decision(
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::StaleTaskId,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::StaleSource,
            RecoveryRiskClass::Normal,
        );
    }
    if inconsistent_durable_state(source) {
        return decision(
            RecoveryDisposition::InconsistentDurableState,
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::ContradictoryEvidence,
            RecoveryRiskClass::LegacyUnknownOrigin,
        );
    }
    if source.history_only
        || source.child_superseded
        || !source.child_ownership_valid
        || !source.agent_type_matches
    {
        return decision(
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::NotContinuable,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::StructuralFence,
            RecoveryRiskClass::Normal,
        );
    }
    if source.error_code.as_deref() == Some("route_policy_rejected") {
        return decision(
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::RouteRejected,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::RouteRejected,
            RecoveryRiskClass::Normal,
        );
    }

    let assessment = assess_cause(source);
    let base_disposition = base_disposition(source, &assessment);
    let (disposition, cause_code) = apply_rails(source, rails, base_disposition, &assessment);
    decision(
        disposition,
        assessment.confirmation,
        cause_code,
        assessment.risk_class,
    )
}

fn inconsistent_durable_state(source: &RecoverySourceSnapshot) -> bool {
    if matches!(
        source.error_code.as_deref(),
        Some("admission_failed") | Some("admission_unknown")
    ) && (source.run_status != DelegationRunStatus::Failed || source.reached_running)
    {
        return true;
    }
    let typed_termination = match &source.parsed_termination {
        ParsedDelegationTermination::Typed(audit) => Some(&audit.termination),
        _ => None,
    };
    let protected_error = protected_cancellation_error(source.error_code.as_deref());
    let typed_protected = typed_termination
        .is_some_and(|termination| protected_cancellation_reason(termination.reason));
    if source.run_status != DelegationRunStatus::Canceled
        && (protected_error.is_some() || typed_protected)
    {
        return true;
    }
    if source.error_code.as_deref() == Some("unresumable") && typed_protected {
        return true;
    }
    if let (Some(expected), Some(termination)) = (protected_error, typed_termination) {
        if (termination.source, termination.reason) != expected.0 {
            return true;
        }
    }
    if let ParsedDelegationTermination::Typed(audit) = &source.parsed_termination {
        if (audit.prior_status == DelegationRunStatus::Running && !source.reached_running)
            || (audit.prior_status == DelegationRunStatus::Reserving && source.reached_running)
        {
            return true;
        }
    }
    false
}

fn protected_cancellation_error(
    error_code: Option<&str>,
) -> Option<(
    (AcpTerminationSource, AcpTerminationReason),
    RecoveryCauseCode,
    RecoveryRiskClass,
)> {
    Some(match error_code? {
        "parent_disconnected" => (
            (
                AcpTerminationSource::Frontend,
                AcpTerminationReason::FrontendDisconnected,
            ),
            RecoveryCauseCode::LegacyParentDisconnect,
            RecoveryRiskClass::LegacyUnknownOrigin,
        ),
        "parent_canceled" => (
            (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::ParentCanceled,
            ),
            RecoveryCauseCode::ParentCanceled,
            RecoveryRiskClass::ExplicitUserStop,
        ),
        "parent_turn_failed" => (
            (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::ParentTurnFailed,
            ),
            RecoveryCauseCode::ParentTurnFailed,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        ),
        "join_abandoned" => (
            (
                AcpTerminationSource::ParentTurn,
                AcpTerminationReason::JoinAbandoned,
            ),
            RecoveryCauseCode::JoinAbandoned,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        ),
        "user_cancelled" => (
            (
                AcpTerminationSource::Frontend,
                AcpTerminationReason::UserCancelled,
            ),
            RecoveryCauseCode::UserCancelled,
            RecoveryRiskClass::ExplicitUserStop,
        ),
        "tool_stalled_timeout" => (
            (
                AcpTerminationSource::Watchdog,
                AcpTerminationReason::ToolStalledTimeout,
            ),
            RecoveryCauseCode::ToolStalledTimeout,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        ),
        _ => return None,
    })
}

fn protected_cancellation_reason(reason: AcpTerminationReason) -> bool {
    matches!(
        reason,
        AcpTerminationReason::FrontendDisconnected
            | AcpTerminationReason::ParentCanceled
            | AcpTerminationReason::ParentTurnFailed
            | AcpTerminationReason::JoinAbandoned
            | AcpTerminationReason::UserCancelled
            | AcpTerminationReason::ToolStalledTimeout
    )
}

fn assess_cause(source: &RecoverySourceSnapshot) -> CauseAssessment {
    match source.error_code.as_deref() {
        Some("admission_unknown") => {
            return CauseAssessment::confirmation_required(
                RecoveryCauseCode::AdmissionUnknown,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            )
        }
        Some("admission_failed") => {
            return CauseAssessment::normal(
                RecoveryCauseCode::AdmissionFailed,
                AdmissionClass::Replacement,
            )
        }
        Some("unresumable") => {
            if source.admission_class == AdmissionClass::UnexpectedContinue {
                return CauseAssessment {
                    cause_code: RecoveryCauseCode::PersistedUnresumable,
                    confirmation: if source.recovery_authorization_id.is_some() {
                        RecoveryConfirmation::NotRequired
                    } else {
                        RecoveryConfirmation::Required
                    },
                    risk_class: RecoveryRiskClass::ExecutionMayHaveOccurred,
                    admission_class: AdmissionClass::Replacement,
                };
            }
            return CauseAssessment::normal(
                RecoveryCauseCode::PersistedUnresumable,
                AdmissionClass::Replacement,
            );
        }
        Some("not_supported") => {
            return CauseAssessment::normal(
                RecoveryCauseCode::UnsupportedReuse,
                AdmissionClass::Replacement,
            )
        }
        _ => {}
    }

    match &source.parsed_termination {
        ParsedDelegationTermination::LegacyParentDisconnect => {
            CauseAssessment::confirmation_required(
                RecoveryCauseCode::LegacyParentDisconnect,
                RecoveryRiskClass::LegacyUnknownOrigin,
            )
        }
        ParsedDelegationTermination::Malformed { .. } => CauseAssessment::confirmation_required(
            RecoveryCauseCode::MalformedTerminationAudit,
            RecoveryRiskClass::LegacyUnknownOrigin,
        ),
        ParsedDelegationTermination::LegacyUnspecified
            if !source.reached_running
                && (source.child_connection_id.is_some()
                    || source.external_session_identity_hash.is_some()) =>
        {
            CauseAssessment::confirmation_required(
                RecoveryCauseCode::AdmissionUnknown,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            )
        }
        ParsedDelegationTermination::LegacyUnspecified
            if source.run_status == DelegationRunStatus::Canceled =>
        {
            let (_, cause_code, risk_class) =
                protected_cancellation_error(source.error_code.as_deref()).unwrap_or((
                    (
                        AcpTerminationSource::Legacy,
                        AcpTerminationReason::LegacyUnspecified,
                    ),
                    RecoveryCauseCode::LegacyParentDisconnect,
                    RecoveryRiskClass::LegacyUnknownOrigin,
                ));
            CauseAssessment::confirmation_required(cause_code, risk_class)
        }
        ParsedDelegationTermination::LegacyUnspecified => match source.run_status {
            DelegationRunStatus::Completed => CauseAssessment::normal(
                RecoveryCauseCode::Completed,
                AdmissionClass::NormalRevision,
            ),
            DelegationRunStatus::Failed if source.reached_running => CauseAssessment::normal(
                RecoveryCauseCode::RevisionEligibleFailure,
                AdmissionClass::NormalRevision,
            ),
            _ => CauseAssessment::normal(
                RecoveryCauseCode::PreAdmissionAbort,
                source.admission_class.clone(),
            ),
        },
        ParsedDelegationTermination::Typed(audit) => assess_typed_termination(audit, source),
    }
}

fn assess_typed_termination(
    audit: &DelegationTerminationAuditV1,
    source: &RecoverySourceSnapshot,
) -> CauseAssessment {
    let termination = &audit.termination;
    let automatic_running_loss = source.reached_running
        && audit.prior_status == DelegationRunStatus::Running
        && termination.prompt_may_have_executed
        && termination.classification == AcpTerminationClassification::Unexpected;
    let automatic_cause = match (termination.source, termination.reason) {
        (AcpTerminationSource::Transport, AcpTerminationReason::TransportDisconnected) => {
            Some(RecoveryCauseCode::UnexpectedTransportLoss)
        }
        (AcpTerminationSource::Process, AcpTerminationReason::ProcessExited) => {
            Some(RecoveryCauseCode::UnexpectedProcessLoss)
        }
        (AcpTerminationSource::Session, AcpTerminationReason::SessionLost) => {
            Some(RecoveryCauseCode::UnexpectedSessionLoss)
        }
        (AcpTerminationSource::HostRestart, AcpTerminationReason::HostRestarted) => {
            Some(RecoveryCauseCode::UnexpectedHostRestart)
        }
        (AcpTerminationSource::ChildConnection, AcpTerminationReason::ChildTerminal) => {
            Some(RecoveryCauseCode::UnexpectedChildConnectionLoss)
        }
        _ => None,
    };
    if automatic_running_loss {
        if let Some(cause_code) = automatic_cause {
            return CauseAssessment {
                cause_code,
                confirmation: RecoveryConfirmation::NotRequired,
                risk_class: RecoveryRiskClass::ExecutionMayHaveOccurred,
                admission_class: AdmissionClass::UnexpectedContinue,
            };
        }
    }

    match termination.reason {
        AcpTerminationReason::ParentCanceled => CauseAssessment::confirmation_required(
            RecoveryCauseCode::ParentCanceled,
            RecoveryRiskClass::ExplicitUserStop,
        ),
        AcpTerminationReason::ParentTurnFailed => CauseAssessment::confirmation_required(
            RecoveryCauseCode::ParentTurnFailed,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        ),
        AcpTerminationReason::JoinAbandoned => CauseAssessment::confirmation_required(
            RecoveryCauseCode::JoinAbandoned,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        ),
        AcpTerminationReason::UserCancelled => CauseAssessment::confirmation_required(
            RecoveryCauseCode::UserCancelled,
            RecoveryRiskClass::ExplicitUserStop,
        ),
        AcpTerminationReason::FrontendDisconnected => {
            let (cause, risk) = match termination.frontend_origin {
                Some(AcpDisconnectOrigin::ExplicitUser) => (
                    RecoveryCauseCode::UserCancelled,
                    RecoveryRiskClass::ExplicitUserStop,
                ),
                Some(AcpDisconnectOrigin::LegacyUnspecified) | None => (
                    RecoveryCauseCode::LegacyParentDisconnect,
                    RecoveryRiskClass::LegacyUnknownOrigin,
                ),
                Some(AcpDisconnectOrigin::ProviderUnmount)
                | Some(AcpDisconnectOrigin::DisconnectAll)
                | Some(AcpDisconnectOrigin::ApplicationShutdown)
                | Some(AcpDisconnectOrigin::ConnectionSuperseded)
                | Some(AcpDisconnectOrigin::IdleTimeout)
                | Some(AcpDisconnectOrigin::ConfigReapply)
                | Some(AcpDisconnectOrigin::DraftRetarget)
                | Some(AcpDisconnectOrigin::AbandonedConnect)
                | Some(AcpDisconnectOrigin::InternalJobComplete) => (
                    RecoveryCauseCode::IntentionalParentDisconnect,
                    RecoveryRiskClass::ExecutionMayHaveOccurred,
                ),
            };
            CauseAssessment::confirmation_required(cause, risk)
        }
        AcpTerminationReason::ToolStalledTimeout => CauseAssessment::confirmation_required(
            RecoveryCauseCode::ToolStalledTimeout,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        ),
        AcpTerminationReason::AdmissionUnknown => CauseAssessment::confirmation_required(
            RecoveryCauseCode::AdmissionUnknown,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        ),
        AcpTerminationReason::AdmissionFailed => CauseAssessment::normal(
            RecoveryCauseCode::PreAdmissionAbort,
            source.admission_class.clone(),
        ),
        AcpTerminationReason::HostRestarted
            if audit.prior_status == DelegationRunStatus::Reserving
                && !termination.prompt_may_have_executed =>
        {
            CauseAssessment::normal(
                RecoveryCauseCode::PreAdmissionRetry,
                source.admission_class.clone(),
            )
        }
        _ if source.run_status == DelegationRunStatus::Completed => {
            CauseAssessment::normal(RecoveryCauseCode::Completed, AdmissionClass::NormalRevision)
        }
        _ if source.run_status == DelegationRunStatus::Failed && source.reached_running => {
            CauseAssessment::normal(
                RecoveryCauseCode::RevisionEligibleFailure,
                AdmissionClass::NormalRevision,
            )
        }
        _ => CauseAssessment::confirmation_required(
            RecoveryCauseCode::LegacyParentDisconnect,
            RecoveryRiskClass::LegacyUnknownOrigin,
        ),
    }
}

fn base_disposition(
    source: &RecoverySourceSnapshot,
    assessment: &CauseAssessment,
) -> RecoveryDisposition {
    if source.error_code.as_deref() == Some("admission_unknown")
        || assessment.cause_code == RecoveryCauseCode::AdmissionUnknown
    {
        return RecoveryDisposition::Replace {
            replacement_reason: ReplacementReason::AdmissionUnknown,
        };
    }
    if source.error_code.as_deref() == Some("admission_failed") {
        return RecoveryDisposition::Replace {
            replacement_reason: ReplacementReason::AdmissionFailed,
        };
    }
    if source.error_code.as_deref() == Some("unresumable") {
        return RecoveryDisposition::Replace {
            replacement_reason: ReplacementReason::Unresumable,
        };
    }
    if source.error_code.as_deref() == Some("not_supported") {
        return RecoveryDisposition::Replace {
            replacement_reason: ReplacementReason::NotSupported,
        };
    }

    let pre_admission = !source.reached_running;
    let prompt_may_have_executed = matches!(
        &source.parsed_termination,
        ParsedDelegationTermination::Typed(audit) if audit.termination.prompt_may_have_executed
    );
    if pre_admission && source.admission_class == AdmissionClass::Replacement {
        if prompt_may_have_executed || source.child_connection_id.is_some() {
            return RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::AdmissionUnknown,
            };
        }
        return RecoveryDisposition::Replace {
            replacement_reason: source
                .replacement_reason
                .clone()
                .unwrap_or(ReplacementReason::AdmissionFailed),
        };
    }

    if pre_admission && (prompt_may_have_executed || source.child_connection_id.is_some()) {
        return RecoveryDisposition::Replace {
            replacement_reason: ReplacementReason::AdmissionUnknown,
        };
    }

    let has_resume_identity =
        source.launch_snapshot_complete && source.external_session_identity_hash.is_some();
    if pre_admission && has_resume_identity {
        return RecoveryDisposition::Continue {
            admission_class: source.admission_class.clone(),
        };
    }
    if pre_admission
        && source.generation == 1
        && !prompt_may_have_executed
        && source.child_connection_id.is_none()
        && source.external_session_identity_hash.is_none()
    {
        return RecoveryDisposition::FreshDispatch;
    }

    RecoveryDisposition::Continue {
        admission_class: assessment.admission_class.clone(),
    }
}

fn apply_rails(
    source: &RecoverySourceSnapshot,
    rails: &RecoveryRailSnapshot,
    disposition: RecoveryDisposition,
    assessment: &CauseAssessment,
) -> (RecoveryDisposition, RecoveryCauseCode) {
    match disposition {
        RecoveryDisposition::Continue { admission_class } => {
            if !rails.agent_supports_reuse {
                return replacement_or_exhausted(
                    rails,
                    ReplacementReason::NotSupported,
                    RecoveryCauseCode::UnsupportedReuse,
                );
            }
            if !source.launch_snapshot_complete || source.external_session_identity_hash.is_none() {
                return replacement_or_exhausted(
                    rails,
                    ReplacementReason::NotSupported,
                    assessment.cause_code.clone().then_missing_resume(),
                );
            }
            if admission_class == AdmissionClass::UnexpectedContinue
                && !rails.unexpected_continue_budget_available
            {
                return replacement_or_exhausted(
                    rails,
                    ReplacementReason::BudgetExhaustedContinue,
                    RecoveryCauseCode::ContinueBudgetExhausted,
                );
            }
            (
                RecoveryDisposition::Continue { admission_class },
                assessment.cause_code.clone(),
            )
        }
        RecoveryDisposition::Replace { replacement_reason } => {
            replacement_or_exhausted(rails, replacement_reason, assessment.cause_code.clone())
        }
        other => (other, assessment.cause_code.clone()),
    }
}

trait MissingResumeCause {
    fn then_missing_resume(self) -> RecoveryCauseCode;
}

impl MissingResumeCause for RecoveryCauseCode {
    fn then_missing_resume(self) -> RecoveryCauseCode {
        match self {
            RecoveryCauseCode::Completed
            | RecoveryCauseCode::RevisionEligibleFailure
            | RecoveryCauseCode::PreAdmissionRetry
            | RecoveryCauseCode::PreAdmissionAbort => RecoveryCauseCode::MissingResumeIdentity,
            confirmation_cause => confirmation_cause,
        }
    }
}

fn replacement_or_exhausted(
    rails: &RecoveryRailSnapshot,
    replacement_reason: ReplacementReason,
    cause_code: RecoveryCauseCode,
) -> (RecoveryDisposition, RecoveryCauseCode) {
    if rails.replacement_budget_available {
        (
            RecoveryDisposition::Replace { replacement_reason },
            cause_code,
        )
    } else {
        (
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::ReplacementBudgetExhausted,
            },
            RecoveryCauseCode::ReplacementBudgetExhausted,
        )
    }
}

#[derive(Serialize)]
struct CanonicalRecoverySource<'a> {
    version: &'static str,
    source_task_id: &'a str,
    lineage_root_task_id: &'a str,
    generation: i64,
    parent_conversation_id: i32,
    child_conversation_id: i32,
    agent_type: &'a str,
    profile_id: &'a Option<String>,
    workspace_path: &'a Option<String>,
    route_fingerprint: &'a Option<String>,
    work_unit_key: &'a Option<String>,
    parent_tool_use_id: &'a Option<String>,
    child_connection_id: &'a Option<String>,
    history_only: bool,
    is_latest: bool,
    has_active_run: bool,
    child_superseded: bool,
    child_ownership_valid: bool,
    agent_type_matches: bool,
    run_status: &'a DelegationRunStatus,
    error_code: &'a Option<String>,
    admission_class: &'a AdmissionClass,
    parsed_termination: serde_json::Value,
    reached_running: bool,
    launch_snapshot_complete: bool,
    launch_snapshot_version: &'a Option<String>,
    mode_id: &'a Option<String>,
    config_values: serde_json::Value,
    external_session_identity_hash: &'a Option<String>,
    replaced_task_id: &'a Option<String>,
    replacement_reason: &'a Option<ReplacementReason>,
    recovery_authorization_id: &'a Option<String>,
    agent_supports_reuse: bool,
}

fn source_state_fingerprint(
    source: &RecoverySourceSnapshot,
    rails: &RecoveryRailSnapshot,
) -> String {
    let canonical = CanonicalRecoverySource {
        version: FINGERPRINT_VERSION,
        source_task_id: &source.source_task_id,
        lineage_root_task_id: &source.lineage_root_task_id,
        generation: source.generation,
        parent_conversation_id: source.parent_conversation_id,
        child_conversation_id: source.child_conversation_id,
        agent_type: &source.agent_type,
        profile_id: &source.profile_id,
        workspace_path: &source.workspace_path,
        route_fingerprint: &source.route_fingerprint,
        work_unit_key: &source.work_unit_key,
        parent_tool_use_id: &source.parent_tool_use_id,
        child_connection_id: &source.child_connection_id,
        history_only: source.history_only,
        is_latest: source.is_latest,
        has_active_run: source.has_active_run,
        child_superseded: source.child_superseded,
        child_ownership_valid: source.child_ownership_valid,
        agent_type_matches: source.agent_type_matches,
        run_status: &source.run_status,
        error_code: &source.error_code,
        admission_class: &source.admission_class,
        parsed_termination: canonical_termination(&source.parsed_termination),
        reached_running: source.reached_running,
        launch_snapshot_complete: source.launch_snapshot_complete,
        launch_snapshot_version: &source.launch_snapshot_version,
        mode_id: &source.mode_id,
        config_values: canonical_config_values(&source.config_values_json),
        external_session_identity_hash: &source.external_session_identity_hash,
        replaced_task_id: &source.replaced_task_id,
        replacement_reason: &source.replacement_reason,
        recovery_authorization_id: &source.recovery_authorization_id,
        agent_supports_reuse: rails.agent_supports_reuse,
    };
    let bytes = serde_json::to_vec(&canonical).expect("recovery fingerprint input serializes");
    format!(
        "{FINGERPRINT_VERSION}:{}",
        hex_lower(&Sha256::digest(bytes))
    )
}

fn canonical_config_values(raw: &Option<String>) -> serde_json::Value {
    match raw {
        None => serde_json::Value::Null,
        Some(raw) => match serde_json::from_str(raw) {
            Ok(value) => serde_json::json!({
                "kind": "structured",
                "value": canonical_json(value),
            }),
            Err(_) => serde_json::json!({
                "kind": "malformed",
                "raw_sha256": hex_lower(&Sha256::digest(raw.as_bytes())),
            }),
        },
    }
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(values) => {
            let mut entries: Vec<_> = values.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut canonical = serde_json::Map::new();
            for (key, value) in entries {
                canonical.insert(key, canonical_json(value));
            }
            serde_json::Value::Object(canonical)
        }
        scalar => scalar,
    }
}

fn canonical_termination(parsed: &ParsedDelegationTermination) -> serde_json::Value {
    match parsed {
        ParsedDelegationTermination::Typed(audit) => serde_json::json!({
            "kind": "typed",
            "termination": audit.termination,
            "prior_status": audit.prior_status,
            "admission_class": audit.admission_class,
            "parent_tool_use_id": audit.parent_tool_use_id,
            "child_connection_id": audit.child_connection_id,
        }),
        ParsedDelegationTermination::LegacyParentDisconnect => {
            serde_json::json!({ "kind": "legacy_parent_disconnect" })
        }
        ParsedDelegationTermination::LegacyUnspecified => {
            serde_json::json!({ "kind": "legacy_unspecified" })
        }
        ParsedDelegationTermination::Malformed { raw_sha256 } => {
            serde_json::json!({ "kind": "malformed", "raw_sha256": raw_sha256 })
        }
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod delegation_recovery_policy {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::acp::termination::{
        AcpDisconnectOrigin, AcpTerminationClassification, AcpTerminationReason,
        AcpTerminationSource, AcpTerminationSummaryV1, DelegationTerminationAuditV1,
        ParsedDelegationTermination, TERMINATION_AUDIT_VERSION,
    };
    use crate::db::entities::delegation_task_run::{AdmissionClass, DelegationRunStatus};

    fn observed_at() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 30, 12, 0, 0)
            .single()
            .expect("valid fixture timestamp")
    }

    fn typed_termination(
        source: AcpTerminationSource,
        reason: AcpTerminationReason,
        classification: AcpTerminationClassification,
        prior_status: DelegationRunStatus,
        prompt_may_have_executed: bool,
    ) -> ParsedDelegationTermination {
        ParsedDelegationTermination::Typed(DelegationTerminationAuditV1 {
            termination: AcpTerminationSummaryV1 {
                version: TERMINATION_AUDIT_VERSION,
                source,
                reason,
                classification,
                frontend_origin: None,
                prompt_may_have_executed,
                requested_at: None,
                observed_at: observed_at(),
            },
            prior_status,
            admission_class: AdmissionClass::NormalRevision,
            parent_tool_use_id: Some("parent-tool-1".into()),
            child_connection_id: Some("child-connection-1".into()),
        })
    }

    fn source() -> RecoverySourceSnapshot {
        RecoverySourceSnapshot {
            source_task_id: "task-1".into(),
            lineage_root_task_id: "lineage-1".into(),
            generation: 1,
            parent_conversation_id: 10,
            child_conversation_id: 20,
            agent_type: "codex".into(),
            profile_id: Some("profile-1".into()),
            workspace_path: Some("C:/workspace".into()),
            route_fingerprint: Some("route-1".into()),
            work_unit_key: Some("work-unit-1".into()),
            parent_tool_use_id: Some("parent-tool-1".into()),
            child_connection_id: Some("child-connection-1".into()),
            history_only: false,
            is_latest: true,
            has_active_run: false,
            child_superseded: false,
            child_ownership_valid: true,
            agent_type_matches: true,
            run_status: DelegationRunStatus::Completed,
            error_code: None,
            admission_class: AdmissionClass::NormalRevision,
            parsed_termination: ParsedDelegationTermination::LegacyUnspecified,
            reached_running: true,
            launch_snapshot_complete: true,
            launch_snapshot_version: Some("v1".into()),
            mode_id: Some("default".into()),
            config_values_json: Some(r#"{"effort":"high","model":"gpt-5"}"#.into()),
            external_session_identity_hash: Some(hash_external_session_identity("session-1")),
            replaced_task_id: None,
            replacement_reason: None,
            recovery_authorization_id: None,
        }
    }

    fn rails() -> RecoveryRailSnapshot {
        RecoveryRailSnapshot {
            agent_supports_reuse: true,
            unexpected_continue_budget_available: true,
            replacement_budget_available: true,
        }
    }

    fn assert_decision(
        source: RecoverySourceSnapshot,
        rails: RecoveryRailSnapshot,
        disposition: RecoveryDisposition,
        confirmation: RecoveryConfirmation,
        cause_code: RecoveryCauseCode,
        risk_class: RecoveryRiskClass,
    ) {
        let decision =
            decide_delegation_recovery(&source, &rails, RequestedRecoveryOperation::Inspect);
        assert_eq!(decision.source_task_id, source.source_task_id);
        assert_eq!(decision.disposition, disposition);
        assert_eq!(decision.confirmation, confirmation);
        assert_eq!(decision.cause_code, cause_code);
        assert_eq!(decision.risk_class, risk_class);
        let expected_action = match &decision.disposition {
            RecoveryDisposition::Continue { admission_class } => Some(RecoveryAction::Continue {
                admission_class: admission_class.clone(),
            }),
            RecoveryDisposition::FreshDispatch => Some(RecoveryAction::FreshDispatch),
            RecoveryDisposition::Replace { replacement_reason } => Some(RecoveryAction::Replace {
                replacement_reason: replacement_reason.clone(),
            }),
            RecoveryDisposition::Stop { .. } | RecoveryDisposition::InconsistentDurableState => {
                None
            }
        };
        assert_eq!(decision.proposed_action(), expected_action);
        let expected_operation = match &decision.disposition {
            RecoveryDisposition::Continue { .. } => RequestedRecoveryOperation::Continue,
            RecoveryDisposition::FreshDispatch => RequestedRecoveryOperation::FreshDispatch,
            RecoveryDisposition::Replace { replacement_reason } => {
                RequestedRecoveryOperation::Replace {
                    replacement_reason: replacement_reason.clone(),
                }
            }
            RecoveryDisposition::Stop { .. } | RecoveryDisposition::InconsistentDurableState => {
                RequestedRecoveryOperation::Continue
            }
        };
        assert_eq!(
            decision.operation_matches(expected_operation),
            decision.proposed_action().is_some()
        );
        assert_eq!(
            decision.requires_authorization(),
            confirmation == RecoveryConfirmation::Required && decision.proposed_action().is_some()
        );
        assert_eq!(
            decision.stop_code(),
            match decision.disposition {
                RecoveryDisposition::Stop { ref code } => Some(code.clone()),
                _ => None,
            }
        );
    }

    #[test]
    fn delegation_recovery_decision_matrix() {
        let normal_rail = rails();

        assert_decision(
            source(),
            normal_rail.clone(),
            RecoveryDisposition::Continue {
                admission_class: AdmissionClass::NormalRevision,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::Completed,
            RecoveryRiskClass::Normal,
        );

        let mut revision_failure = source();
        revision_failure.run_status = DelegationRunStatus::Failed;
        revision_failure.error_code = Some("child_refusal".into());
        assert_decision(
            revision_failure,
            normal_rail.clone(),
            RecoveryDisposition::Continue {
                admission_class: AdmissionClass::NormalRevision,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::RevisionEligibleFailure,
            RecoveryRiskClass::Normal,
        );

        for (termination_source, reason, cause) in [
            (
                AcpTerminationSource::Transport,
                AcpTerminationReason::TransportDisconnected,
                RecoveryCauseCode::UnexpectedTransportLoss,
            ),
            (
                AcpTerminationSource::Process,
                AcpTerminationReason::ProcessExited,
                RecoveryCauseCode::UnexpectedProcessLoss,
            ),
            (
                AcpTerminationSource::Session,
                AcpTerminationReason::SessionLost,
                RecoveryCauseCode::UnexpectedSessionLoss,
            ),
            (
                AcpTerminationSource::HostRestart,
                AcpTerminationReason::HostRestarted,
                RecoveryCauseCode::UnexpectedHostRestart,
            ),
            (
                AcpTerminationSource::ChildConnection,
                AcpTerminationReason::ChildTerminal,
                RecoveryCauseCode::UnexpectedChildConnectionLoss,
            ),
        ] {
            let mut unexpected = source();
            unexpected.run_status = DelegationRunStatus::Canceled;
            unexpected.error_code = Some("interrupted".into());
            unexpected.parsed_termination = typed_termination(
                termination_source,
                reason,
                AcpTerminationClassification::Unexpected,
                DelegationRunStatus::Running,
                true,
            );
            assert_decision(
                unexpected,
                normal_rail.clone(),
                RecoveryDisposition::Continue {
                    admission_class: AdmissionClass::UnexpectedContinue,
                },
                RecoveryConfirmation::NotRequired,
                cause,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            );
        }

        let mut legacy_null = source();
        legacy_null.run_status = DelegationRunStatus::Canceled;
        legacy_null.error_code = Some("parent_disconnected".into());
        legacy_null.parsed_termination = ParsedDelegationTermination::LegacyParentDisconnect;
        assert_decision(
            legacy_null,
            normal_rail.clone(),
            RecoveryDisposition::Continue {
                admission_class: AdmissionClass::UnexpectedContinue,
            },
            RecoveryConfirmation::Required,
            RecoveryCauseCode::LegacyParentDisconnect,
            RecoveryRiskClass::LegacyUnknownOrigin,
        );

        let mut malformed = source();
        malformed.run_status = DelegationRunStatus::Canceled;
        malformed.error_code = Some("parent_disconnected".into());
        malformed.parsed_termination = ParsedDelegationTermination::Malformed {
            raw_sha256: "a".repeat(64),
        };
        assert_decision(
            malformed,
            normal_rail.clone(),
            RecoveryDisposition::Continue {
                admission_class: AdmissionClass::UnexpectedContinue,
            },
            RecoveryConfirmation::Required,
            RecoveryCauseCode::MalformedTerminationAudit,
            RecoveryRiskClass::LegacyUnknownOrigin,
        );

        for (reason, classification, cause, risk) in [
            (
                AcpTerminationReason::ParentCanceled,
                AcpTerminationClassification::Explicit,
                RecoveryCauseCode::ParentCanceled,
                RecoveryRiskClass::ExplicitUserStop,
            ),
            (
                AcpTerminationReason::ParentTurnFailed,
                AcpTerminationClassification::Unexpected,
                RecoveryCauseCode::ParentTurnFailed,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            ),
            (
                AcpTerminationReason::JoinAbandoned,
                AcpTerminationClassification::Intentional,
                RecoveryCauseCode::JoinAbandoned,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            ),
        ] {
            let mut parent_end = source();
            parent_end.run_status = DelegationRunStatus::Canceled;
            parent_end.error_code = Some(
                match reason {
                    AcpTerminationReason::ParentCanceled => "parent_canceled",
                    AcpTerminationReason::ParentTurnFailed => "parent_turn_failed",
                    AcpTerminationReason::JoinAbandoned => "join_abandoned",
                    _ => unreachable!(),
                }
                .into(),
            );
            parent_end.parsed_termination = typed_termination(
                AcpTerminationSource::ParentTurn,
                reason,
                classification,
                DelegationRunStatus::Running,
                true,
            );
            assert_decision(
                parent_end,
                normal_rail.clone(),
                RecoveryDisposition::Continue {
                    admission_class: AdmissionClass::UnexpectedContinue,
                },
                RecoveryConfirmation::Required,
                cause,
                risk,
            );
        }

        let mut explicit_cancel = source();
        explicit_cancel.run_status = DelegationRunStatus::Canceled;
        explicit_cancel.error_code = Some("user_cancelled".into());
        explicit_cancel.parsed_termination = typed_termination(
            AcpTerminationSource::Frontend,
            AcpTerminationReason::UserCancelled,
            AcpTerminationClassification::Explicit,
            DelegationRunStatus::Running,
            true,
        );
        if let ParsedDelegationTermination::Typed(audit) = &mut explicit_cancel.parsed_termination {
            audit.termination.frontend_origin = Some(AcpDisconnectOrigin::ExplicitUser);
            audit.termination.requested_at = Some(observed_at());
        }
        assert_decision(
            explicit_cancel,
            normal_rail.clone(),
            RecoveryDisposition::Continue {
                admission_class: AdmissionClass::UnexpectedContinue,
            },
            RecoveryConfirmation::Required,
            RecoveryCauseCode::UserCancelled,
            RecoveryRiskClass::ExplicitUserStop,
        );

        let mut stall = source();
        stall.run_status = DelegationRunStatus::Canceled;
        stall.error_code = Some("tool_stalled_timeout".into());
        stall.parsed_termination = typed_termination(
            AcpTerminationSource::Watchdog,
            AcpTerminationReason::ToolStalledTimeout,
            AcpTerminationClassification::AutomatedAmbiguous,
            DelegationRunStatus::Running,
            true,
        );
        assert_decision(
            stall,
            normal_rail.clone(),
            RecoveryDisposition::Continue {
                admission_class: AdmissionClass::UnexpectedContinue,
            },
            RecoveryConfirmation::Required,
            RecoveryCauseCode::ToolStalledTimeout,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        );

        let mut pure_abort = source();
        pure_abort.run_status = DelegationRunStatus::Failed;
        pure_abort.error_code = Some("spawn_failed".into());
        pure_abort.reached_running = false;
        pure_abort.child_connection_id = None;
        pure_abort.external_session_identity_hash = None;
        pure_abort.parsed_termination = typed_termination(
            AcpTerminationSource::Admission,
            AcpTerminationReason::AdmissionFailed,
            AcpTerminationClassification::Intentional,
            DelegationRunStatus::Reserving,
            false,
        );
        assert_decision(
            pure_abort,
            normal_rail.clone(),
            RecoveryDisposition::FreshDispatch,
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::PreAdmissionAbort,
            RecoveryRiskClass::Normal,
        );

        let mut explicit_pre_admission_abort = source();
        explicit_pre_admission_abort.run_status = DelegationRunStatus::Canceled;
        explicit_pre_admission_abort.error_code = Some("user_cancelled".into());
        explicit_pre_admission_abort.reached_running = false;
        explicit_pre_admission_abort.child_connection_id = None;
        explicit_pre_admission_abort.external_session_identity_hash = None;
        explicit_pre_admission_abort.parsed_termination = typed_termination(
            AcpTerminationSource::Frontend,
            AcpTerminationReason::UserCancelled,
            AcpTerminationClassification::Explicit,
            DelegationRunStatus::Reserving,
            false,
        );
        assert_decision(
            explicit_pre_admission_abort,
            normal_rail.clone(),
            RecoveryDisposition::FreshDispatch,
            RecoveryConfirmation::Required,
            RecoveryCauseCode::UserCancelled,
            RecoveryRiskClass::ExplicitUserStop,
        );

        let mut admission_failed = source();
        admission_failed.run_status = DelegationRunStatus::Failed;
        admission_failed.error_code = Some("admission_failed".into());
        admission_failed.reached_running = false;
        admission_failed.external_session_identity_hash = None;
        assert_decision(
            admission_failed,
            normal_rail.clone(),
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::AdmissionFailed,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::AdmissionFailed,
            RecoveryRiskClass::Normal,
        );

        let mut admission_unknown = source();
        admission_unknown.run_status = DelegationRunStatus::Failed;
        admission_unknown.error_code = Some("admission_unknown".into());
        admission_unknown.reached_running = false;
        assert_decision(
            admission_unknown,
            normal_rail.clone(),
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::AdmissionUnknown,
            },
            RecoveryConfirmation::Required,
            RecoveryCauseCode::AdmissionUnknown,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        );

        let mut missing_resume = source();
        missing_resume.external_session_identity_hash = None;
        assert_decision(
            missing_resume,
            normal_rail.clone(),
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::NotSupported,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::MissingResumeIdentity,
            RecoveryRiskClass::Normal,
        );

        for (authorization, confirmation) in [
            (None, RecoveryConfirmation::Required),
            (
                Some("authorization-1".to_string()),
                RecoveryConfirmation::NotRequired,
            ),
        ] {
            let mut unresumable = source();
            unresumable.run_status = DelegationRunStatus::Failed;
            unresumable.error_code = Some("unresumable".into());
            unresumable.admission_class = AdmissionClass::UnexpectedContinue;
            unresumable.recovery_authorization_id = authorization;
            assert_decision(
                unresumable,
                normal_rail.clone(),
                RecoveryDisposition::Replace {
                    replacement_reason: ReplacementReason::Unresumable,
                },
                confirmation,
                RecoveryCauseCode::PersistedUnresumable,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            );
        }

        let mut unexpected = source();
        unexpected.run_status = DelegationRunStatus::Canceled;
        unexpected.parsed_termination = typed_termination(
            AcpTerminationSource::Transport,
            AcpTerminationReason::TransportDisconnected,
            AcpTerminationClassification::Unexpected,
            DelegationRunStatus::Running,
            true,
        );
        let mut exhausted_continue = normal_rail.clone();
        exhausted_continue.unexpected_continue_budget_available = false;
        assert_decision(
            unexpected,
            exhausted_continue,
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::BudgetExhaustedContinue,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::ContinueBudgetExhausted,
            RecoveryRiskClass::ExecutionMayHaveOccurred,
        );

        let mut unsupported = normal_rail.clone();
        unsupported.agent_supports_reuse = false;
        assert_decision(
            source(),
            unsupported,
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::NotSupported,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::UnsupportedReuse,
            RecoveryRiskClass::Normal,
        );

        let mut no_replacement = normal_rail.clone();
        no_replacement.agent_supports_reuse = false;
        no_replacement.replacement_budget_available = false;
        assert_decision(
            source(),
            no_replacement,
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::ReplacementBudgetExhausted,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::ReplacementBudgetExhausted,
            RecoveryRiskClass::Normal,
        );

        let mut stale = source();
        stale.is_latest = false;
        assert_decision(
            stale,
            normal_rail.clone(),
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::StaleTaskId,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::StaleSource,
            RecoveryRiskClass::Normal,
        );

        let mut busy = source();
        busy.has_active_run = true;
        assert_decision(
            busy,
            normal_rail.clone(),
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::BusyThread,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::BusySource,
            RecoveryRiskClass::Normal,
        );

        let mut route_rejected = source();
        route_rejected.run_status = DelegationRunStatus::Failed;
        route_rejected.error_code = Some("route_policy_rejected".into());
        assert_decision(
            route_rejected,
            normal_rail.clone(),
            RecoveryDisposition::Stop {
                code: RecoveryStopCode::RouteRejected,
            },
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::RouteRejected,
            RecoveryRiskClass::Normal,
        );

        let mut contradictory = source();
        contradictory.run_status = DelegationRunStatus::Failed;
        contradictory.error_code = Some("parent_disconnected".into());
        contradictory.parsed_termination = ParsedDelegationTermination::LegacyUnspecified;
        assert_decision(
            contradictory,
            normal_rail,
            RecoveryDisposition::InconsistentDurableState,
            RecoveryConfirmation::NotRequired,
            RecoveryCauseCode::ContradictoryEvidence,
            RecoveryRiskClass::LegacyUnknownOrigin,
        );
    }

    #[test]
    fn post_running_and_pre_admission_host_restart_use_distinct_rails() {
        let mut post_running = source();
        post_running.run_status = DelegationRunStatus::Canceled;
        post_running.error_code = Some("host_restarted".into());
        post_running.parsed_termination = typed_termination(
            AcpTerminationSource::HostRestart,
            AcpTerminationReason::HostRestarted,
            AcpTerminationClassification::Unexpected,
            DelegationRunStatus::Running,
            true,
        );
        let decision = decide_delegation_recovery(
            &post_running,
            &rails(),
            RequestedRecoveryOperation::Inspect,
        );
        assert_eq!(
            decision.disposition,
            RecoveryDisposition::Continue {
                admission_class: AdmissionClass::UnexpectedContinue
            }
        );
        assert_eq!(decision.confirmation, RecoveryConfirmation::NotRequired);

        for class in [
            AdmissionClass::NormalRevision,
            AdmissionClass::UnexpectedContinue,
        ] {
            let mut pre_admission = source();
            pre_admission.generation = 2;
            pre_admission.run_status = DelegationRunStatus::Failed;
            pre_admission.error_code = Some("host_restarted".into());
            pre_admission.admission_class = class.clone();
            pre_admission.reached_running = false;
            pre_admission.child_connection_id = None;
            pre_admission.parsed_termination = typed_termination(
                AcpTerminationSource::HostRestart,
                AcpTerminationReason::HostRestarted,
                AcpTerminationClassification::Unexpected,
                DelegationRunStatus::Reserving,
                false,
            );
            let decision = decide_delegation_recovery(
                &pre_admission,
                &rails(),
                RequestedRecoveryOperation::Inspect,
            );
            assert_eq!(
                decision.disposition,
                RecoveryDisposition::Continue {
                    admission_class: class
                }
            );
        }

        let mut ambiguous = source();
        ambiguous.generation = 2;
        ambiguous.run_status = DelegationRunStatus::Failed;
        ambiguous.error_code = Some("host_restarted".into());
        ambiguous.reached_running = false;
        ambiguous.child_connection_id = Some("bound-before-crash".into());
        ambiguous.external_session_identity_hash = None;
        ambiguous.parsed_termination = ParsedDelegationTermination::LegacyUnspecified;
        let decision =
            decide_delegation_recovery(&ambiguous, &rails(), RequestedRecoveryOperation::Inspect);
        assert_eq!(
            decision.disposition,
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::AdmissionUnknown
            }
        );
        assert_eq!(decision.confirmation, RecoveryConfirmation::Required);
    }

    #[test]
    fn established_pre_admission_continue_retry_preserves_rail_and_confirmation() {
        for (class, confirmation) in [
            (
                AdmissionClass::NormalRevision,
                RecoveryConfirmation::NotRequired,
            ),
            (
                AdmissionClass::UnexpectedContinue,
                RecoveryConfirmation::Required,
            ),
        ] {
            let mut retry = source();
            retry.generation = 3;
            retry.run_status = DelegationRunStatus::Failed;
            retry.error_code = Some("host_restarted".into());
            retry.admission_class = class.clone();
            retry.reached_running = false;
            retry.child_connection_id = None;
            retry.parsed_termination = if class == AdmissionClass::UnexpectedContinue {
                ParsedDelegationTermination::Malformed {
                    raw_sha256: "c".repeat(64),
                }
            } else {
                typed_termination(
                    AcpTerminationSource::HostRestart,
                    AcpTerminationReason::HostRestarted,
                    AcpTerminationClassification::Unexpected,
                    DelegationRunStatus::Reserving,
                    false,
                )
            };
            let rail_snapshot = rails();
            let decision = decide_delegation_recovery(
                &retry,
                &rail_snapshot,
                RequestedRecoveryOperation::Inspect,
            );
            assert_eq!(
                decision.disposition,
                RecoveryDisposition::Continue {
                    admission_class: class
                }
            );
            assert_eq!(decision.confirmation, confirmation);
            assert!(rail_snapshot.unexpected_continue_budget_available);
            assert!(rail_snapshot.replacement_budget_available);
        }

        let mut missing_resume = source();
        missing_resume.generation = 3;
        missing_resume.run_status = DelegationRunStatus::Failed;
        missing_resume.error_code = Some("host_restarted".into());
        missing_resume.reached_running = false;
        missing_resume.child_connection_id = None;
        missing_resume.external_session_identity_hash = None;
        missing_resume.parsed_termination = typed_termination(
            AcpTerminationSource::HostRestart,
            AcpTerminationReason::HostRestarted,
            AcpTerminationClassification::Unexpected,
            DelegationRunStatus::Reserving,
            false,
        );
        let decision = decide_delegation_recovery(
            &missing_resume,
            &rails(),
            RequestedRecoveryOperation::Inspect,
        );
        assert_eq!(
            decision.disposition,
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::NotSupported
            },
            "established lineage must never fall back to generation-1 fresh dispatch"
        );
    }

    #[test]
    fn established_pre_admission_replacement_retry_never_switches_to_continue() {
        let mut retry = source();
        retry.run_status = DelegationRunStatus::Failed;
        retry.error_code = Some("host_restarted".into());
        retry.admission_class = AdmissionClass::Replacement;
        retry.reached_running = false;
        retry.child_connection_id = None;
        retry.replaced_task_id = Some("replaced-task".into());
        retry.replacement_reason = Some(ReplacementReason::Unresumable);
        retry.recovery_authorization_id = Some("authorization-1".into());
        retry.parsed_termination = typed_termination(
            AcpTerminationSource::HostRestart,
            AcpTerminationReason::HostRestarted,
            AcpTerminationClassification::Unexpected,
            DelegationRunStatus::Reserving,
            false,
        );
        let decision =
            decide_delegation_recovery(&retry, &rails(), RequestedRecoveryOperation::Inspect);
        assert_eq!(
            decision.disposition,
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::Unresumable
            }
        );
        assert_eq!(decision.confirmation, RecoveryConfirmation::NotRequired);

        retry.error_code = Some("admission_unknown".into());
        retry.child_connection_id = Some("bound-before-crash".into());
        let ambiguous =
            decide_delegation_recovery(&retry, &rails(), RequestedRecoveryOperation::Inspect);
        assert_eq!(
            ambiguous.disposition,
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::AdmissionUnknown
            }
        );
        assert_eq!(ambiguous.confirmation, RecoveryConfirmation::Required);
    }

    #[test]
    fn busy_precedes_every_authorization_and_has_no_detach_action() {
        for status in [DelegationRunStatus::Reserving, DelegationRunStatus::Running] {
            let mut busy = source();
            busy.run_status = status;
            busy.has_active_run = true;
            busy.recovery_authorization_id = Some("matching-authorization".into());
            let decision =
                decide_delegation_recovery(&busy, &rails(), RequestedRecoveryOperation::Continue);
            assert_eq!(
                decision.disposition,
                RecoveryDisposition::Stop {
                    code: RecoveryStopCode::BusyThread
                }
            );
            assert!(!decision.requires_authorization());
            assert_eq!(decision.proposed_action(), None);
        }
    }

    #[test]
    fn parent_cancel_with_missing_resume_identity_still_requires_confirmation_for_replace() {
        let mut canceled = source();
        canceled.run_status = DelegationRunStatus::Canceled;
        canceled.error_code = Some("parent_canceled".into());
        canceled.external_session_identity_hash = None;
        canceled.parsed_termination = typed_termination(
            AcpTerminationSource::ParentTurn,
            AcpTerminationReason::ParentCanceled,
            AcpTerminationClassification::Explicit,
            DelegationRunStatus::Running,
            true,
        );
        let decision =
            decide_delegation_recovery(&canceled, &rails(), RequestedRecoveryOperation::Inspect);
        assert_eq!(
            decision.disposition,
            RecoveryDisposition::Replace {
                replacement_reason: ReplacementReason::NotSupported
            }
        );
        assert_eq!(decision.confirmation, RecoveryConfirmation::Required);
        assert_eq!(decision.cause_code, RecoveryCauseCode::ParentCanceled);
        assert_eq!(decision.risk_class, RecoveryRiskClass::ExplicitUserStop);
    }

    #[test]
    fn failed_parent_disconnected_is_inconsistent_not_legacy_compatible() {
        for parsed in [
            ParsedDelegationTermination::LegacyUnspecified,
            ParsedDelegationTermination::Malformed {
                raw_sha256: "f".repeat(64),
            },
        ] {
            let mut source = source();
            source.run_status = DelegationRunStatus::Failed;
            source.error_code = Some("parent_disconnected".into());
            source.parsed_termination = parsed;
            let decision =
                decide_delegation_recovery(&source, &rails(), RequestedRecoveryOperation::Inspect);
            assert_eq!(
                decision.disposition,
                RecoveryDisposition::InconsistentDurableState
            );
            assert_eq!(
                decision.cause_code,
                RecoveryCauseCode::ContradictoryEvidence
            );
            assert_eq!(decision.proposed_action(), None);
        }
    }

    #[test]
    fn completed_protected_cancellation_errors_are_inconsistent() {
        for error_code in [
            "parent_disconnected",
            "parent_canceled",
            "parent_turn_failed",
            "join_abandoned",
            "user_cancelled",
            "tool_stalled_timeout",
        ] {
            let mut completed = source();
            completed.run_status = DelegationRunStatus::Completed;
            completed.error_code = Some(error_code.into());
            completed.parsed_termination = ParsedDelegationTermination::LegacyUnspecified;

            let decision = decide_delegation_recovery(
                &completed,
                &rails(),
                RequestedRecoveryOperation::Inspect,
            );

            assert_eq!(
                decision.disposition,
                RecoveryDisposition::InconsistentDurableState,
                "completed + {error_code} must be inconsistent"
            );
            assert_eq!(decision.confirmation, RecoveryConfirmation::NotRequired);
            assert_eq!(decision.proposed_action(), None);
            assert!(!decision.requires_authorization());
            assert!(!decision.operation_matches(RequestedRecoveryOperation::Continue));
            assert!(
                !decision.operation_matches(RequestedRecoveryOperation::Replace {
                    replacement_reason: ReplacementReason::Unresumable,
                })
            );
        }
    }

    #[test]
    fn protected_status_error_and_audit_reconciliation_matrix() {
        let protected = [
            (
                "parent_disconnected",
                RecoveryCauseCode::LegacyParentDisconnect,
                RecoveryRiskClass::LegacyUnknownOrigin,
            ),
            (
                "parent_canceled",
                RecoveryCauseCode::ParentCanceled,
                RecoveryRiskClass::ExplicitUserStop,
            ),
            (
                "parent_turn_failed",
                RecoveryCauseCode::ParentTurnFailed,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            ),
            (
                "join_abandoned",
                RecoveryCauseCode::JoinAbandoned,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            ),
            (
                "user_cancelled",
                RecoveryCauseCode::UserCancelled,
                RecoveryRiskClass::ExplicitUserStop,
            ),
            (
                "tool_stalled_timeout",
                RecoveryCauseCode::ToolStalledTimeout,
                RecoveryRiskClass::ExecutionMayHaveOccurred,
            ),
        ];

        for (error_code, cause_code, risk_class) in protected {
            let mut failed = source();
            failed.run_status = DelegationRunStatus::Failed;
            failed.error_code = Some(error_code.into());
            failed.parsed_termination = ParsedDelegationTermination::LegacyUnspecified;
            let decision =
                decide_delegation_recovery(&failed, &rails(), RequestedRecoveryOperation::Inspect);
            assert_eq!(
                decision.disposition,
                RecoveryDisposition::InconsistentDurableState,
                "failed + {error_code} must be inconsistent"
            );
            assert_eq!(decision.proposed_action(), None);

            let mut canceled = source();
            canceled.run_status = DelegationRunStatus::Canceled;
            canceled.error_code = Some(error_code.into());
            canceled.parsed_termination = if error_code == "parent_disconnected" {
                ParsedDelegationTermination::LegacyParentDisconnect
            } else {
                ParsedDelegationTermination::LegacyUnspecified
            };
            assert_decision(
                canceled,
                rails(),
                RecoveryDisposition::Continue {
                    admission_class: AdmissionClass::UnexpectedContinue,
                },
                RecoveryConfirmation::Required,
                cause_code,
                risk_class,
            );
        }

        for (reason, source_kind) in [
            (
                AcpTerminationReason::ParentCanceled,
                AcpTerminationSource::ParentTurn,
            ),
            (
                AcpTerminationReason::ParentTurnFailed,
                AcpTerminationSource::ParentTurn,
            ),
            (
                AcpTerminationReason::JoinAbandoned,
                AcpTerminationSource::ParentTurn,
            ),
            (
                AcpTerminationReason::UserCancelled,
                AcpTerminationSource::Frontend,
            ),
            (
                AcpTerminationReason::ToolStalledTimeout,
                AcpTerminationSource::Watchdog,
            ),
        ] {
            let mut contradictory = source();
            contradictory.run_status = DelegationRunStatus::Failed;
            contradictory.error_code = Some("unresumable".into());
            contradictory.parsed_termination = typed_termination(
                source_kind,
                reason,
                AcpTerminationClassification::Intentional,
                DelegationRunStatus::Running,
                true,
            );
            let decision = decide_delegation_recovery(
                &contradictory,
                &rails(),
                RequestedRecoveryOperation::Inspect,
            );
            assert_eq!(
                decision.disposition,
                RecoveryDisposition::InconsistentDurableState,
                "unresumable + {reason:?} must be inconsistent"
            );
        }

        let mut mismatched = source();
        mismatched.run_status = DelegationRunStatus::Canceled;
        mismatched.error_code = Some("parent_canceled".into());
        mismatched.parsed_termination = typed_termination(
            AcpTerminationSource::Watchdog,
            AcpTerminationReason::ToolStalledTimeout,
            AcpTerminationClassification::AutomatedAmbiguous,
            DelegationRunStatus::Running,
            true,
        );
        assert_eq!(
            decide_delegation_recovery(&mismatched, &rails(), RequestedRecoveryOperation::Inspect,)
                .disposition,
            RecoveryDisposition::InconsistentDurableState,
        );
    }

    #[test]
    fn automatic_continue_requires_durable_reached_running() {
        let mut missing_durable_running = source();
        missing_durable_running.run_status = DelegationRunStatus::Canceled;
        missing_durable_running.error_code = Some("transport_disconnected".into());
        missing_durable_running.reached_running = false;
        missing_durable_running.parsed_termination = typed_termination(
            AcpTerminationSource::Transport,
            AcpTerminationReason::TransportDisconnected,
            AcpTerminationClassification::Unexpected,
            DelegationRunStatus::Running,
            true,
        );
        let decision = decide_delegation_recovery(
            &missing_durable_running,
            &rails(),
            RequestedRecoveryOperation::Inspect,
        );
        assert_eq!(
            decision.disposition,
            RecoveryDisposition::InconsistentDurableState,
        );
        assert_eq!(decision.proposed_action(), None);

        let mut stale_reserving_phase = source();
        stale_reserving_phase.run_status = DelegationRunStatus::Failed;
        stale_reserving_phase.error_code = Some("spawn_failed".into());
        stale_reserving_phase.reached_running = true;
        stale_reserving_phase.parsed_termination = typed_termination(
            AcpTerminationSource::Admission,
            AcpTerminationReason::AdmissionFailed,
            AcpTerminationClassification::Intentional,
            DelegationRunStatus::Reserving,
            false,
        );
        let decision = decide_delegation_recovery(
            &stale_reserving_phase,
            &rails(),
            RequestedRecoveryOperation::Inspect,
        );
        assert_eq!(
            decision.disposition,
            RecoveryDisposition::InconsistentDurableState,
        );
        assert_eq!(decision.proposed_action(), None);
    }

    #[test]
    fn intentional_frontend_disconnect_origins_require_confirmation() {
        let decision_for = |origin: Option<AcpDisconnectOrigin>| {
            let mut disconnected = source();
            disconnected.run_status = DelegationRunStatus::Canceled;
            disconnected.error_code = Some("parent_disconnected".into());
            disconnected.parsed_termination = typed_termination(
                AcpTerminationSource::Frontend,
                AcpTerminationReason::FrontendDisconnected,
                AcpTerminationClassification::Intentional,
                DelegationRunStatus::Running,
                true,
            );
            let ParsedDelegationTermination::Typed(audit) = &mut disconnected.parsed_termination
            else {
                unreachable!()
            };
            audit.termination.frontend_origin = origin;
            decide_delegation_recovery(&disconnected, &rails(), RequestedRecoveryOperation::Inspect)
        };

        let explicit = decision_for(Some(AcpDisconnectOrigin::ExplicitUser));
        assert_eq!(explicit.confirmation, RecoveryConfirmation::Required);
        assert_eq!(explicit.cause_code, RecoveryCauseCode::UserCancelled);
        assert_eq!(explicit.risk_class, RecoveryRiskClass::ExplicitUserStop);

        for origin in [None, Some(AcpDisconnectOrigin::LegacyUnspecified)] {
            let legacy = decision_for(origin);
            assert_eq!(legacy.confirmation, RecoveryConfirmation::Required);
            assert_eq!(legacy.cause_code, RecoveryCauseCode::LegacyParentDisconnect);
            assert_eq!(legacy.risk_class, RecoveryRiskClass::LegacyUnknownOrigin);
        }

        for origin in [
            AcpDisconnectOrigin::ProviderUnmount,
            AcpDisconnectOrigin::DisconnectAll,
            AcpDisconnectOrigin::ApplicationShutdown,
            AcpDisconnectOrigin::ConnectionSuperseded,
            AcpDisconnectOrigin::IdleTimeout,
            AcpDisconnectOrigin::ConfigReapply,
            AcpDisconnectOrigin::DraftRetarget,
            AcpDisconnectOrigin::AbandonedConnect,
            AcpDisconnectOrigin::InternalJobComplete,
        ] {
            let intentional = decision_for(Some(origin));
            assert_eq!(
                intentional.disposition,
                RecoveryDisposition::Continue {
                    admission_class: AdmissionClass::UnexpectedContinue,
                }
            );
            assert_eq!(intentional.confirmation, RecoveryConfirmation::Required);
            assert_eq!(
                intentional.cause_code,
                RecoveryCauseCode::IntentionalParentDisconnect,
                "intentional origin {origin:?} must not become user cancellation"
            );
            assert_eq!(
                intentional.risk_class,
                RecoveryRiskClass::ExecutionMayHaveOccurred
            );
        }
    }

    #[test]
    fn launch_snapshot_identity_fields_are_fingerprinted() {
        let source = source();
        let baseline =
            decide_delegation_recovery(&source, &rails(), RequestedRecoveryOperation::Inspect)
                .source_state_fingerprint;

        let mut changed_version = source.clone();
        changed_version.launch_snapshot_version = Some("v2".into());
        let mut changed_mode = source.clone();
        changed_mode.mode_id = Some("review".into());
        let mut changed_config = source.clone();
        changed_config.config_values_json = Some(r#"{"model":"gpt-5.1","effort":"high"}"#.into());

        for mutation in [changed_version, changed_mode, changed_config] {
            let fingerprint = decide_delegation_recovery(
                &mutation,
                &rails(),
                RequestedRecoveryOperation::Inspect,
            )
            .source_state_fingerprint;
            assert_ne!(baseline, fingerprint);
            assert!(fingerprint.starts_with("delegation_recovery_v1:"));
            assert_eq!(fingerprint.len(), 87);
            assert!(fingerprint[23..]
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));
        }

        let mut reordered_config = source.clone();
        reordered_config.config_values_json =
            Some(r#"{ "model": "gpt-5", "effort": "high" }"#.into());
        let reordered = decide_delegation_recovery(
            &reordered_config,
            &rails(),
            RequestedRecoveryOperation::Inspect,
        )
        .source_state_fingerprint;
        assert_eq!(
            baseline, reordered,
            "JSON object order is not snapshot identity"
        );
    }

    #[test]
    fn fingerprints_exclude_prompt_raw_external_session_id_and_budgets() {
        let source = source();
        let base_rails = rails();
        let base =
            decide_delegation_recovery(&source, &base_rails, RequestedRecoveryOperation::Inspect);
        assert_eq!(base.source_state_fingerprint.len(), 87);
        assert!(base
            .source_state_fingerprint
            .starts_with("delegation_recovery_v1:"));
        assert!(base.source_state_fingerprint[23..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)));

        // These values cannot enter RecoverySourceSnapshot. Changing them while
        // retaining the already-derived session identity hash is therefore inert.
        let excluded_before = ("secret prompt", "running nicely", "session-1");
        let excluded_after = ("different prompt", "different prose", "different raw id");
        assert_ne!(excluded_before, excluded_after);
        let unchanged = decide_delegation_recovery(
            &source,
            &RecoveryRailSnapshot {
                unexpected_continue_budget_available: false,
                replacement_budget_available: false,
                ..base_rails.clone()
            },
            RequestedRecoveryOperation::Inspect,
        );
        assert_eq!(
            base.source_state_fingerprint,
            unchanged.source_state_fingerprint
        );

        let mut mutations = Vec::new();
        macro_rules! mutated {
            ($field:ident = $value:expr) => {{
                let mut value = source.clone();
                value.$field = $value;
                mutations.push(value);
            }};
        }
        mutated!(source_task_id = "task-2".into());
        mutated!(lineage_root_task_id = "lineage-2".into());
        mutated!(generation = 2);
        mutated!(parent_conversation_id = 11);
        mutated!(child_conversation_id = 21);
        mutated!(agent_type = "claude".into());
        mutated!(profile_id = Some("profile-2".into()));
        mutated!(workspace_path = Some("D:/workspace".into()));
        mutated!(route_fingerprint = Some("route-2".into()));
        mutated!(work_unit_key = Some("work-unit-2".into()));
        mutated!(parent_tool_use_id = Some("parent-tool-2".into()));
        mutated!(child_connection_id = Some("child-connection-2".into()));
        mutated!(history_only = true);
        mutated!(is_latest = false);
        mutated!(has_active_run = true);
        mutated!(child_superseded = true);
        mutated!(child_ownership_valid = false);
        mutated!(agent_type_matches = false);
        mutated!(run_status = DelegationRunStatus::Failed);
        mutated!(error_code = Some("different".into()));
        mutated!(admission_class = AdmissionClass::UnexpectedContinue);
        mutated!(
            parsed_termination = typed_termination(
                AcpTerminationSource::Transport,
                AcpTerminationReason::TransportDisconnected,
                AcpTerminationClassification::Unexpected,
                DelegationRunStatus::Running,
                true,
            )
        );
        mutated!(reached_running = false);
        mutated!(launch_snapshot_complete = false);
        mutated!(launch_snapshot_version = Some("v2".into()));
        mutated!(mode_id = Some("review".into()));
        mutated!(config_values_json = Some(r#"{"model":"gpt-5.1"}"#.into()));
        mutated!(
            external_session_identity_hash = Some(hash_external_session_identity("session-2"))
        );
        mutated!(replaced_task_id = Some("replaced-task".into()));
        mutated!(replacement_reason = Some(ReplacementReason::Unresumable));
        mutated!(recovery_authorization_id = Some("authorization-1".into()));
        for mutation in mutations {
            let changed = decide_delegation_recovery(
                &mutation,
                &base_rails,
                RequestedRecoveryOperation::Inspect,
            );
            assert_ne!(
                base.source_state_fingerprint,
                changed.source_state_fingerprint
            );
        }

        let changed_capability = decide_delegation_recovery(
            &source,
            &RecoveryRailSnapshot {
                agent_supports_reuse: false,
                ..base_rails.clone()
            },
            RequestedRecoveryOperation::Inspect,
        );
        assert_ne!(
            base.source_state_fingerprint,
            changed_capability.source_state_fingerprint
        );
        let mut budget_source = source.clone();
        budget_source.run_status = DelegationRunStatus::Canceled;
        budget_source.parsed_termination = typed_termination(
            AcpTerminationSource::Transport,
            AcpTerminationReason::TransportDisconnected,
            AcpTerminationClassification::Unexpected,
            DelegationRunStatus::Running,
            true,
        );
        let budget_available = decide_delegation_recovery(
            &budget_source,
            &base_rails,
            RequestedRecoveryOperation::Inspect,
        );
        let budget_exhausted = decide_delegation_recovery(
            &budget_source,
            &RecoveryRailSnapshot {
                unexpected_continue_budget_available: false,
                ..base_rails
            },
            RequestedRecoveryOperation::Inspect,
        );
        assert_eq!(
            budget_available.source_state_fingerprint,
            budget_exhausted.source_state_fingerprint
        );
        assert_ne!(budget_available.disposition, budget_exhausted.disposition);
    }
}
