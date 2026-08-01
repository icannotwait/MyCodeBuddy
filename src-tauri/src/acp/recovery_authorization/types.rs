use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::entities::recovery_authorization::{self, RecoveryAuthorizationStatus};

pub const APPROVAL_TTL: Duration = Duration::minutes(10);
pub const TERMINAL_AUTHORIZATION_RETENTION_DAYS: i64 = 30;
pub const RECOVERY_APPROVE_LABEL: &str = "approve";
pub const RECOVERY_DECLINE_LABEL: &str = "decline";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoverySubjectKind {
    DelegationTask,
    Workflow,
}

impl RecoverySubjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DelegationTask => "delegation_task",
            Self::Workflow => "workflow",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAllowedAction {
    Continue,
    FreshDispatch,
    Replace,
    RecoverWorkflow,
    ResetPlanLineage,
}

impl RecoveryAllowedAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::FreshDispatch => "fresh_dispatch",
            Self::Replace => "replace",
            Self::RecoverWorkflow => "recover_workflow",
            Self::ResetPlanLineage => "reset_plan_lineage",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "continue" => Self::Continue,
            "fresh_dispatch" => Self::FreshDispatch,
            "replace" => Self::Replace,
            "recover_workflow" => Self::RecoverWorkflow,
            "reset_plan_lineage" => Self::ResetPlanLineage,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryActionMetadata<'a> {
    pub target_code: &'static str,
    pub replacement_reason: Option<&'a str>,
    pub target_state: Option<&'a str>,
}

pub fn derive_recovery_action_metadata<'a>(
    action: RecoveryAllowedAction,
    payload: &'a Value,
) -> Option<RecoveryActionMetadata<'a>> {
    let payload_action = payload.get("action").and_then(Value::as_str);
    let (target_code, replacement_reason, target_state) = match action {
        RecoveryAllowedAction::Continue if payload_action == Some("continue") => {
            ("existing_session", None, None)
        }
        RecoveryAllowedAction::FreshDispatch if payload_action == Some("fresh_dispatch") => {
            ("fresh_task", None, None)
        }
        RecoveryAllowedAction::Replace if payload_action == Some("replace") => {
            let reason = payload.get("replacement_reason")?.as_str()?;
            let target = match reason {
                "unresumable" => "replace_unresumable",
                "budget_exhausted_continue" => "replace_budget_exhausted_continue",
                "not_supported" => "replace_not_supported",
                "admission_failed" => "replace_admission_failed",
                "admission_unknown" => "replace_admission_unknown",
                _ => return None,
            };
            (target, Some(reason), None)
        }
        RecoveryAllowedAction::RecoverWorkflow => {
            let state = payload.get("target_state")?.as_str()?;
            let target = match state {
                "skeleton" => "workflow_skeleton",
                "estimated" => "workflow_estimated",
                "approved" => "workflow_approved",
                _ => return None,
            };
            (target, None, Some(state))
        }
        RecoveryAllowedAction::ResetPlanLineage
            if payload
                .get("displayed_reason_sha256")
                .and_then(Value::as_str)
                .is_some_and(|hash| !hash.is_empty()) =>
        {
            ("plan_lineage", None, None)
        }
        _ => return None,
    };
    Some(RecoveryActionMetadata {
        target_code,
        replacement_reason,
        target_state,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryConsumerKind {
    DelegationTaskRun,
    WorkflowManifestRevision,
}

impl RecoveryConsumerKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DelegationTaskRun => "delegation_task_run",
            Self::WorkflowManifestRevision => "workflow_manifest_revision",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationAuthorizationIdentity {
    pub source_task_id: String,
    pub child_conversation_id: Option<i32>,
    pub lineage_root_task_id: String,
    pub work_unit_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryChallenge {
    pub parent_conversation_id: i32,
    pub subject_kind: RecoverySubjectKind,
    pub subject_id: String,
    pub delegation_identity: Option<DelegationAuthorizationIdentity>,
    pub source_state_fingerprint: String,
    pub allowed_action: RecoveryAllowedAction,
    pub action_payload: Value,
    pub cause_code: String,
    pub risk_class: String,
    pub display_reason: Option<String>,
}

pub struct AuthorizationConsumeExpectation<'a> {
    pub parent_conversation_id: i32,
    pub subject_kind: RecoverySubjectKind,
    pub subject_id: &'a str,
    pub source_state_fingerprint: &'a str,
    pub allowed_action: RecoveryAllowedAction,
    pub action_payload: &'a Value,
    pub consumer_kind: RecoveryConsumerKind,
    pub consumer_id: &'a str,
    pub consumer_correlation_id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryAuthorizationResult {
    pub authorization_id: String,
    pub status: RecoveryAuthorizationStatus,
    pub subject_kind: String,
    pub subject_id: String,
    pub allowed_action: String,
    pub action_payload: Value,
    pub cause_code: String,
    pub display_reason: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<&recovery_authorization::Model> for RecoveryAuthorizationResult {
    fn from(row: &recovery_authorization::Model) -> Self {
        Self {
            authorization_id: row.authorization_id.clone(),
            status: row.status.clone(),
            subject_kind: row.subject_kind.clone(),
            subject_id: row.subject_id.clone(),
            allowed_action: row.allowed_action.clone(),
            action_payload: serde_json::from_str(&row.action_payload_json).unwrap_or(Value::Null),
            cause_code: row.cause_code.clone(),
            display_reason: row.display_reason.clone(),
            approved_at: row.approved_at,
            expires_at: row.expires_at,
        }
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Pending preserves the durable row used by question resolution.
pub enum PreparedAuthorization {
    NotRequired {
        action: RecoveryAllowedAction,
    },
    HardStop {
        code: String,
    },
    ExistingApproved(RecoveryAuthorizationResult),
    Pending {
        row: recovery_authorization::Model,
        newly_created: bool,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryAuthorizationError {
    #[error("recovery authorization database error: {0}")]
    Database(String),
    #[error("recovery authorization not found")]
    NotFound,
    #[error("recovery authorization request is blocked")]
    Blocked,
    #[error("recovery authorization wait was cancelled")]
    Cancelled,
    #[error("active recovery challenge differs from the requested challenge")]
    ChallengeConflict,
    #[error("recovery question is already bound to a different card")]
    QuestionBindingConflict,
    #[error("recovery authorization parent mismatch")]
    ParentMismatch,
    #[error("recovery authorization subject kind mismatch")]
    SubjectKindMismatch,
    #[error("recovery authorization subject id mismatch")]
    SubjectIdMismatch,
    #[error("recovery authorization fingerprint mismatch")]
    FingerprintMismatch,
    #[error("recovery authorization action mismatch")]
    ActionMismatch,
    #[error("recovery authorization payload mismatch")]
    PayloadMismatch,
    #[error("recovery authorization is pending")]
    Pending,
    #[error("recovery authorization was declined")]
    Declined,
    #[error("recovery authorization expired")]
    Expired,
    #[error("recovery authorization was abandoned")]
    Abandoned,
    #[error("recovery authorization was already consumed by another expectation")]
    ConsumedConflict,
}

impl RecoveryAuthorizationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Database(_) => "recovery_authorization_database_error",
            Self::NotFound => "recovery_authorization_not_found",
            Self::Blocked => "recovery_authorization_blocked",
            Self::Cancelled => "recovery_authorization_cancelled",
            Self::ChallengeConflict => "recovery_authorization_challenge_conflict",
            Self::QuestionBindingConflict => "recovery_authorization_question_binding_conflict",
            Self::ParentMismatch => "recovery_authorization_parent_mismatch",
            Self::SubjectKindMismatch => "recovery_authorization_subject_kind_mismatch",
            Self::SubjectIdMismatch => "recovery_authorization_subject_id_mismatch",
            Self::FingerprintMismatch => "recovery_authorization_fingerprint_mismatch",
            Self::ActionMismatch => "recovery_authorization_action_mismatch",
            Self::PayloadMismatch => "recovery_authorization_payload_mismatch",
            Self::Pending => "recovery_authorization_pending",
            Self::Declined => "recovery_authorization_declined",
            Self::Expired => "recovery_authorization_expired",
            Self::Abandoned => "recovery_authorization_abandoned",
            Self::ConsumedConflict => "recovery_authorization_consumed_conflict",
        }
    }
}

impl From<sea_orm::DbErr> for RecoveryAuthorizationError {
    fn from(error: sea_orm::DbErr) -> Self {
        Self::Database(error.to_string())
    }
}
