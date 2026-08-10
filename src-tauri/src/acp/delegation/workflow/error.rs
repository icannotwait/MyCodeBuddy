//! Store and validation errors for the workflow graph.

use thiserror::Error;

use crate::db::entities::delegation_workflow::CompletionProtocolMode;

pub use super::artifact_resolver::{ArtifactError, ArtifactFailure};
pub use super::evidence_scope::EvidenceScopeError;
pub use super::plan_material::{PlanMaterialError, PlanMaterialErrorKind};
use super::plan_review::PlanReviewError;
use super::recovery_policy::WorkflowRecoveryProjection;
pub use super::types::WorkflowError;

pub const WORKFLOW_RECOVERY_REQUIRED: &str = "workflow_recovery_required";
pub const WORKFLOW_RECOVERY_NOT_AVAILABLE: &str = "workflow_recovery_not_available";
pub const WORKFLOW_RECOVERY_CONFLICT: &str = "workflow_recovery_conflict";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "completion protocol configuration {variable} was removed; remove the variable and restart"
)]
pub struct CompletionProtocolConfigurationRemoved {
    pub variable: &'static str,
}

impl CompletionProtocolConfigurationRemoved {
    pub const fn code(&self) -> &'static str {
        "completion_protocol_configuration_removed"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CompletionRecoveryFenceError {
    #[error("completion outcome requires a direct decision before recovery")]
    DecisionRequired,
    #[error("completion artifact is unavailable; use the artifact recovery action")]
    ArtifactUnavailable,
}

impl CompletionRecoveryFenceError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::DecisionRequired => "completion_decision_required",
            Self::ArtifactUnavailable => "completion_artifact_unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CompletionEvidenceError {
    #[error("completion terminal state is invalid: {0}")]
    InvalidTerminalState(String),
    #[error("completion attention is invalid: {0}")]
    InvalidAttention(String),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Scope(#[from] EvidenceScopeError),
    #[error("completion decision was superseded")]
    DecisionSuperseded,
    #[error("completion evidence is corrupt: {0}")]
    EvidenceCorrupt(String),
    #[error("{message}")]
    Protocol { code: &'static str, message: String },
    #[error("completion persistence failure: {0}")]
    Persistence(String),
}

impl CompletionEvidenceError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidTerminalState(_) => "completion_terminal_state_invalid",
            Self::InvalidAttention(_) => "completion_attention_invalid",
            Self::Artifact(error) => error.code(),
            Self::Scope(error) => error.code(),
            Self::DecisionSuperseded => "completion_decision_superseded",
            Self::EvidenceCorrupt(_) => "completion_evidence_corrupt",
            Self::Protocol { code, .. } => code,
            Self::Persistence(_) => "completion_persistence_failure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkflowAdmissionRecoveryError {
    pub message: String,
    pub recovery: WorkflowRecoveryProjection,
}

impl WorkflowAdmissionRecoveryError {
    pub fn encode(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn decode(value: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(value)
    }
}

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

    #[error("workflow binding immutable identity conflict for node {node_id}; use a new node id")]
    AdmittedNodeIdentityMutation { node_id: String },

    #[error("admitted workflow binding or Task cohort policy/complete route is immutable at {node_id} (cohort_frozen)")]
    CohortFrozen { node_id: String },

    #[error("gate not ready: reviewed_task_stale: {0}")]
    ReviewedTaskStale(String),

    #[error("gate not ready: artifact_digest_mismatch: {0}")]
    ArtifactDigestMismatch(String),

    #[error("gate not ready: {0}")]
    GateNotReady(String),

    #[error("protocol-v2 settlement rejects caller-supplied legacy evidence fields")]
    V2CallerEvidenceRejected,

    #[error("completion outcome requires a direct decision before gate reduction")]
    CompletionDecisionRequired,

    #[error("completion decision was superseded before gate reduction")]
    CompletionDecisionSuperseded,

    #[error("completion artifact is unavailable before gate reduction")]
    CompletionArtifactUnavailable,

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

    #[error("legacy completion protocol is read-only")]
    LegacyCompletionProtocolReadOnly,

    #[error("unsupported completion protocol pair: version {version}, mode {mode:?}")]
    UnsupportedCompletionProtocol {
        version: i64,
        mode: CompletionProtocolMode,
    },

    #[error("unsupported completion protocol header: {0}")]
    UnsupportedCompletionProtocolHeader(String),

    /// Transient contention (e.g. publication_token race winner not yet visible).
    /// Callers may safely retry the same publish request.
    #[error("busy (retryable): {0}")]
    Busy(String),

    #[error("workflow recovery is not available")]
    WorkflowRecoveryNotAvailable,

    #[error("workflow recovery request conflicts with a committed recovery")]
    WorkflowRecoveryConflict,

    #[error("recovery authorization is required for {action}")]
    RecoveryAuthorizationRequired { action: &'static str },

    #[error("recovery authorization is stale")]
    RecoveryAuthorizationStale,

    #[error("recovery authorization rejected: {code}")]
    RecoveryAuthorizationRejected { code: &'static str },

    #[error("persistence failure: {0}")]
    Persistence(String),
}

impl WorkflowStoreError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::LegacyCompletionProtocolReadOnly => "legacy_completion_protocol_read_only",
            Self::UnsupportedCompletionProtocol { .. }
            | Self::UnsupportedCompletionProtocolHeader(_) => "unsupported_completion_protocol",
            Self::CrossParent { .. } => "unauthorized",
            Self::NotFound(_) | Self::ParentNotFound(_) => "workflow_not_found",
            Self::StaleManifestRevision { .. } => "stale_manifest_revision",
            Self::StaleGraphRevision { .. } => "stale_graph_revision",
            Self::Busy(_) => "workflow_busy",
            Self::Persistence(_) => "workflow_persistence_failure",
            _ => "workflow_invalid",
        }
    }

    pub fn workflow_recovery_required() -> Self {
        Self::GateNotReady(WORKFLOW_RECOVERY_REQUIRED.into())
    }

    pub fn is_workflow_recovery_required(&self) -> bool {
        matches!(self, Self::GateNotReady(reason) if reason == WORKFLOW_RECOVERY_REQUIRED)
    }

    /// True when the client may retry the same operation after a short delay.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy(_) | Self::Persistence(_))
    }
}

pub fn require_v2_mutation(
    version: i64,
    mode: &CompletionProtocolMode,
) -> Result<(), WorkflowStoreError> {
    if version == 2 && mode == &CompletionProtocolMode::V2Enforce {
        return Ok(());
    }
    if version == 1 {
        return Err(WorkflowStoreError::LegacyCompletionProtocolReadOnly);
    }
    Err(WorkflowStoreError::UnsupportedCompletionProtocol {
        version,
        mode: mode.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::delegation_workflow::CompletionProtocolMode;

    #[test]
    fn require_v2_mutation_classifies_all_protocol_pairs() {
        use CompletionProtocolMode::{V2Enforce, V2Shadow, V1};

        assert_eq!(require_v2_mutation(2, &V2Enforce), Ok(()));
        for mode in [V1, V2Shadow, V2Enforce] {
            let error = require_v2_mutation(1, &mode).unwrap_err();
            assert_eq!(error.code(), "legacy_completion_protocol_read_only");
            assert!(!error.is_retryable());
        }
        for mode in [V1, V2Shadow] {
            let error = require_v2_mutation(2, &mode).unwrap_err();
            assert_eq!(error.code(), "unsupported_completion_protocol");
            assert!(!error.is_retryable());
        }
        for version in [0, 3] {
            for mode in [V1, V2Shadow, V2Enforce] {
                let error = require_v2_mutation(version, &mode).unwrap_err();
                assert_eq!(error.code(), "unsupported_completion_protocol");
                assert!(!error.is_retryable());
            }
        }
    }
}
