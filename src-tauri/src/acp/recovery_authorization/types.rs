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
    pub approved_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl From<&recovery_authorization::Model> for RecoveryAuthorizationResult {
    fn from(row: &recovery_authorization::Model) -> Self {
        Self {
            authorization_id: row.authorization_id.clone(),
            status: row.status.clone(),
            approved_at: row.approved_at,
            expires_at: row.expires_at,
        }
    }
}

#[derive(Debug)]
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
