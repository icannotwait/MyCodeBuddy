//! Store and validation errors for the workflow graph.

use thiserror::Error;

use super::plan_review::PlanReviewError;
pub use super::types::WorkflowError;

/// Errors from publish / settle / get_workflow_state core paths.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowStoreError {
    #[error(transparent)]
    Validation(#[from] WorkflowError),

    #[error(transparent)]
    PlanReview(#[from] PlanReviewError),

    #[error("workflow not found: {0}")]
    NotFound(String),

    #[error("cross-parent ownership violation for workflow {workflow_id}")]
    CrossParent {
        workflow_id: String,
        expected_parent: i32,
        actual_parent: i32,
    },

    #[error("stale manifest revision: expected {expected}, current {current}")]
    StaleManifestRevision { expected: u64, current: u64 },

    #[error("stale graph revision: expected {expected}, current {current}")]
    StaleGraphRevision { expected: u64, current: u64 },

    #[error("publication token mismatch: same token has a different document digest")]
    PublicationTokenMismatch {
        publication_token: String,
        workflow_id: String,
    },

    #[error("publication token conflict: parent already has workflow {existing_workflow_id}")]
    PublicationTokenConflict { existing_workflow_id: String },

    #[error("admitted-node identity mutation rejected for node {node_id}")]
    AdmittedNodeIdentityMutation { node_id: String },

    #[error("cannot drop frozen/unobserved Task partner node {node_id} (cohort_frozen)")]
    FrozenPartnerDrop { node_id: String },

    #[error("gate not ready: {0}")]
    GateNotReady(String),

    #[error("gate cycle conflict: {0}")]
    GateCycleConflict(String),

    #[error("document gate only: settle rejects Task/Final execution gates ({0})")]
    ExecutionGateSettleRejected(String),

    #[error(
        "approval rejected while Critical/Important findings remain (critical={critical}, important={important})"
    )]
    ApprovalWithOpenFindings { critical: i64, important: i64 },

    #[error(
        "approval rejected: required reviewer {node_id} is failed/canceled without legal recovery"
    )]
    ApprovalRejectedFailedReviewer { node_id: String },

    #[error("adjudication summary exceeds 4 KiB bound")]
    SummaryTooLarge,

    #[error(
        "finding counts must be non-negative (critical={critical}, important={important}, minor={minor})"
    )]
    NegativeFindingCounts {
        critical: i64,
        important: i64,
        minor: i64,
    },

    #[error("parent conversation {0} not found")]
    ParentNotFound(i32),

    /// Transient contention (e.g. publication_token race winner not yet visible).
    /// Callers may safely retry the same publish request.
    #[error("busy (retryable): {0}")]
    Busy(String),

    #[error("persistence failure: {0}")]
    Persistence(String),
}

impl WorkflowStoreError {
    /// True when the client may retry the same operation after a short delay.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy(_) | Self::Persistence(_))
    }
}
