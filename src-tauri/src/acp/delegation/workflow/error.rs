//! Store and validation errors for the workflow graph.

use std::collections::BTreeSet;

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect};
use thiserror::Error;

use crate::db::entities::delegation_workflow::CompletionProtocolMode;
use crate::db::entities::{
    delegation_task_run, delegation_workflow, delegation_workflow_run_binding, simple_workflow,
};

pub use super::artifact_resolver::{ArtifactError, ArtifactFailure};
pub use super::evidence_scope::EvidenceScopeError;
pub use super::plan_material::{PlanMaterialError, PlanMaterialErrorKind};
use super::plan_review::PlanReviewError;
use super::recovery_policy::WorkflowRecoveryProjection;
use super::store::map_completion_protocol_header_db_error;
pub use super::types::WorkflowError;

pub const WORKFLOW_RECOVERY_REQUIRED: &str = "workflow_recovery_required";
pub const WORKFLOW_RECOVERY_NOT_AVAILABLE: &str = "workflow_recovery_not_available";
pub const WORKFLOW_RECOVERY_CONFLICT: &str = "workflow_recovery_conflict";
pub const WORKFLOW_V2_RETIRED_MESSAGE: &str =
    "This workflow is archived and read-only. Continue in a Simple successor.";

#[cfg(any(test, feature = "test-utils"))]
tokio::task_local! {
    static HISTORICAL_WORKFLOW_FIXTURE_MUTATIONS: ();
}

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
    #[error("This workflow is archived and read-only. Continue in a Simple successor.")]
    WorkflowV2Retired {
        source_conversation_id: Option<i32>,
        successor_conversation_id: Option<i32>,
        can_create_simple_successor: bool,
    },

    #[error("conversation {source_conversation_id} has conflicting workflow identities")]
    WorkflowIdentityCorrupt { source_conversation_id: i32 },

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
            Self::WorkflowV2Retired { .. } => "workflow_v2_retired",
            Self::WorkflowIdentityCorrupt { .. } => "workflow_identity_corrupt",
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

    pub const fn workflow_v2_retired() -> Self {
        Self::WorkflowV2Retired {
            source_conversation_id: None,
            successor_conversation_id: None,
            can_create_simple_successor: false,
        }
    }

    pub const fn workflow_v2_retired_with_navigation(
        source_conversation_id: i32,
        successor_conversation_id: Option<i32>,
        can_create_simple_successor: bool,
    ) -> Self {
        Self::WorkflowV2Retired {
            source_conversation_id: Some(source_conversation_id),
            successor_conversation_id,
            can_create_simple_successor,
        }
    }

    pub const fn source_conversation_id(&self) -> Option<i32> {
        match self {
            Self::WorkflowV2Retired {
                source_conversation_id,
                ..
            } => *source_conversation_id,
            Self::WorkflowIdentityCorrupt {
                source_conversation_id,
            } => Some(*source_conversation_id),
            _ => None,
        }
    }

    pub const fn successor_conversation_id(&self) -> Option<i32> {
        match self {
            Self::WorkflowV2Retired {
                successor_conversation_id,
                ..
            } => *successor_conversation_id,
            _ => None,
        }
    }

    pub const fn can_create_simple_successor(&self) -> Option<bool> {
        match self {
            Self::WorkflowV2Retired {
                can_create_simple_successor,
                ..
            } => Some(*can_create_simple_successor),
            _ => None,
        }
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
        return Err(WorkflowStoreError::workflow_v2_retired());
    }
    if version == 1 {
        return Err(WorkflowStoreError::LegacyCompletionProtocolReadOnly);
    }
    Err(WorkflowStoreError::UnsupportedCompletionProtocol {
        version,
        mode: mode.clone(),
    })
}

/// Lexically permits historical workflow mutation only while an explicit
/// test fixture future is being polled. The permission cannot survive a
/// successful or failed fixture call and is never persisted in application
/// state.
#[cfg(any(test, feature = "test-utils"))]
pub async fn with_historical_workflow_fixture_mutations<F>(future: F) -> F::Output
where
    F: std::future::IntoFuture,
{
    HISTORICAL_WORKFLOW_FIXTURE_MUTATIONS
        .scope((), std::future::IntoFuture::into_future(future))
        .await
}

#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn historical_workflow_fixture_mutations_enabled() -> bool {
    HISTORICAL_WORKFLOW_FIXTURE_MUTATIONS
        .try_with(|()| true)
        .unwrap_or(false)
}

pub async fn require_v2_mutation_for_connection<C: ConnectionTrait>(
    _conn: &C,
    version: i64,
    mode: &CompletionProtocolMode,
) -> Result<(), WorkflowStoreError> {
    let result = require_v2_mutation(version, mode);
    #[cfg(any(test, feature = "test-utils"))]
    if matches!(&result, Err(WorkflowStoreError::WorkflowV2Retired { .. }))
        && historical_workflow_fixture_mutations_enabled()
    {
        return Ok(());
    }
    result
}

#[derive(Debug)]
struct ArchivedWorkflowNavigation {
    source_conversation_id: i32,
    successor_conversation_id: Option<i32>,
    completion_protocol_version: i64,
    completion_protocol_mode: CompletionProtocolMode,
}

async fn archived_workflow_navigation<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<Option<ArchivedWorkflowNavigation>, WorkflowStoreError> {
    let mut workflow_ids = BTreeSet::new();
    workflow_ids.extend(
        delegation_workflow::Entity::find()
            .select_only()
            .column(delegation_workflow::Column::WorkflowId)
            .filter(delegation_workflow::Column::ParentConversationId.eq(conversation_id))
            .filter(
                delegation_workflow::Column::WorkflowKind.eq("brainstorm_to_delivery"),
            )
            .into_tuple::<String>()
            .all(conn)
            .await
            .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?,
    );

    let task_ids = delegation_task_run::Entity::find()
        .select_only()
        .column(delegation_task_run::Column::TaskId)
        .filter(delegation_task_run::Column::ChildConversationId.eq(conversation_id))
        .into_tuple::<String>()
        .all(conn)
        .await
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    if !task_ids.is_empty() {
        workflow_ids.extend(
            delegation_workflow_run_binding::Entity::find()
                .select_only()
                .column(delegation_workflow_run_binding::Column::WorkflowId)
                .filter(delegation_workflow_run_binding::Column::TaskId.is_in(task_ids))
                .into_tuple::<String>()
                .all(conn)
                .await
                .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?,
        );
    }

    if workflow_ids.is_empty() {
        return Ok(None);
    }

    let workflows = delegation_workflow::Entity::find()
        .select_only()
        .column(delegation_workflow::Column::WorkflowId)
        .column(delegation_workflow::Column::ParentConversationId)
        .column(delegation_workflow::Column::CompletionProtocolVersion)
        .column(delegation_workflow::Column::CompletionProtocolMode)
        .filter(delegation_workflow::Column::WorkflowId.is_in(workflow_ids.iter().cloned()))
        .into_tuple::<(String, i32, i64, CompletionProtocolMode)>()
        .all(conn)
        .await
        .map_err(map_completion_protocol_header_db_error)?;
    if workflows.len() != 1 || workflows.len() != workflow_ids.len() {
        return Err(WorkflowStoreError::WorkflowIdentityCorrupt {
            source_conversation_id: conversation_id,
        });
    }
    let (
        workflow_id,
        parent_conversation_id,
        completion_protocol_version,
        completion_protocol_mode,
    ) = workflows.into_iter().next().expect("one workflow");

    if simple_workflow::Entity::find()
        .filter(simple_workflow::Column::ParentConversationId.eq(parent_conversation_id))
        .one(conn)
        .await
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?
        .is_some()
    {
        return Err(WorkflowStoreError::WorkflowIdentityCorrupt {
            source_conversation_id: parent_conversation_id,
        });
    }

    let successors = simple_workflow::Entity::find()
        .filter(simple_workflow::Column::SourceWorkflowId.eq(workflow_id))
        .all(conn)
        .await
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    if successors.len() > 1 {
        return Err(WorkflowStoreError::WorkflowIdentityCorrupt {
            source_conversation_id: parent_conversation_id,
        });
    }

    Ok(Some(ArchivedWorkflowNavigation {
        source_conversation_id: parent_conversation_id,
        successor_conversation_id: successors
            .into_iter()
            .next()
            .map(|successor| successor.parent_conversation_id),
        completion_protocol_version,
        completion_protocol_mode,
    }))
}

pub async fn workflow_v2_retired_for_conversation<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<WorkflowStoreError, WorkflowStoreError> {
    if let Some(navigation) = archived_workflow_navigation(conn, conversation_id).await? {
        let error = require_v2_mutation(
            navigation.completion_protocol_version,
            &navigation.completion_protocol_mode,
        )
        .expect_err("persisted workflow protocol pairs are read-only");
        return Ok(match error {
            WorkflowStoreError::WorkflowV2Retired { .. } => {
                WorkflowStoreError::workflow_v2_retired_with_navigation(
                    navigation.source_conversation_id,
                    navigation.successor_conversation_id,
                    navigation.successor_conversation_id.is_none(),
                )
            }
            other => other,
        });
    }

    let simple = simple_workflow::Entity::find()
        .filter(simple_workflow::Column::ParentConversationId.eq(conversation_id))
        .one(conn)
        .await
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    Ok(WorkflowStoreError::workflow_v2_retired_with_navigation(
        conversation_id,
        simple.as_ref().map(|_| conversation_id),
        simple.is_none(),
    ))
}

/// Publication is retired independently of a persisted protocol header. This
/// resolves navigation from durable workflow/Simple identity without allowing
/// legacy or malformed headers to change the publication error code.
pub async fn workflow_v2_publication_retired_for_conversation<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<WorkflowStoreError, WorkflowStoreError> {
    if let Some(navigation) = archived_workflow_navigation(conn, conversation_id).await? {
        return Ok(WorkflowStoreError::workflow_v2_retired_with_navigation(
            navigation.source_conversation_id,
            navigation.successor_conversation_id,
            navigation.successor_conversation_id.is_none(),
        ));
    }

    let simple = simple_workflow::Entity::find()
        .filter(simple_workflow::Column::ParentConversationId.eq(conversation_id))
        .one(conn)
        .await
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    Ok(WorkflowStoreError::workflow_v2_retired_with_navigation(
        conversation_id,
        simple.as_ref().map(|_| conversation_id),
        simple.is_none(),
    ))
}

/// Durable fence for all new mutations originating from a conversation. The
/// resolver follows both root ownership and run bindings, so an archived child
/// cannot bypass the retired root by presenting only its child id.
pub async fn require_writable_conversation_workflow<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<(), WorkflowStoreError> {
    #[cfg(any(test, feature = "test-utils"))]
    if historical_workflow_fixture_mutations_enabled() {
        return Ok(());
    }
    if let Some(navigation) = archived_workflow_navigation(conn, conversation_id).await? {
        let result = require_v2_mutation(
            navigation.completion_protocol_version,
            &navigation.completion_protocol_mode,
        );
        return match result {
            Err(WorkflowStoreError::WorkflowV2Retired { .. }) => {
                Err(WorkflowStoreError::workflow_v2_retired_with_navigation(
                    navigation.source_conversation_id,
                    navigation.successor_conversation_id,
                    navigation.successor_conversation_id.is_none(),
                ))
            }
            result => result,
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::entities::delegation_workflow::CompletionProtocolMode;

    #[test]
    fn require_v2_mutation_classifies_all_protocol_pairs() {
        use CompletionProtocolMode::{V2Enforce, V2Shadow, V1};

        let retired = require_v2_mutation(2, &V2Enforce).unwrap_err();
        assert_eq!(retired.code(), "workflow_v2_retired");
        assert_eq!(retired.to_string(), WORKFLOW_V2_RETIRED_MESSAGE);
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

    #[tokio::test]
    async fn historical_fixture_permission_is_lexical_and_unwinds_after_failure() {
        use crate::db::test_helpers::fresh_in_memory_db;

        let fixture_db = fresh_in_memory_db().await;
        let ordinary_db = fresh_in_memory_db().await;

        let ordinary_error = require_v2_mutation_for_connection(
            &ordinary_db.conn,
            2,
            &CompletionProtocolMode::V2Enforce,
        )
        .await
        .unwrap_err();
        assert_eq!(ordinary_error.code(), "workflow_v2_retired");

        let fixture_error = with_historical_workflow_fixture_mutations(async {
            require_v2_mutation_for_connection(
                &fixture_db.conn,
                2,
                &CompletionProtocolMode::V2Enforce,
            )
            .await
            .unwrap();
            require_v2_mutation_for_connection(
                &ordinary_db.conn,
                2,
                &CompletionProtocolMode::V2Enforce,
            )
            .await
            .unwrap();
            assert_eq!(
                require_v2_mutation(2, &CompletionProtocolMode::V2Enforce)
                    .unwrap_err()
                    .code(),
                "workflow_v2_retired"
            );
            Err::<(), _>("fixture failed")
        })
        .await
        .unwrap_err();
        assert_eq!(fixture_error, "fixture failed");

        for db in [&fixture_db, &ordinary_db] {
            let error = require_v2_mutation_for_connection(
                &db.conn,
                2,
                &CompletionProtocolMode::V2Enforce,
            )
            .await
            .unwrap_err();
            assert_eq!(error.code(), "workflow_v2_retired");
        }
        assert_eq!(
            require_v2_mutation(2, &CompletionProtocolMode::V2Enforce)
                .unwrap_err()
                .code(),
            "workflow_v2_retired"
        );
    }

    #[tokio::test]
    async fn workflow_v2_retired_resolves_root_child_and_simple_successor_navigation() {
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, Set};

        use crate::db::entities::delegation_task_run::{
            self, AdmissionClass, DelegationRunStatus,
        };
        use crate::db::entities::{
            delegation_workflow, delegation_workflow_run_binding, simple_workflow,
        };
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
        use crate::models::AgentType;

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/workflow-retired-navigation").await;
        let root = seed_conversation(&db, folder, AgentType::Codex).await;
        let child = seed_conversation(&db, folder, AgentType::Codex).await;
        let successor = seed_conversation(&db, folder, AgentType::Codex).await;
        let now = Utc::now();
        delegation_workflow::ActiveModel {
            workflow_id: Set("workflow-retired-navigation".into()),
            parent_conversation_id: Set(root),
            workflow_kind: Set("brainstorm_to_delivery".into()),
            schema_version: Set(2),
            active_manifest_revision: Set(1),
            graph_revision: Set(1),
            workflow_state: Set(delegation_workflow::WorkflowState::Approved),
            capability_version: Set("workflow_manifest_v2".into()),
            publication_token: Set("workflow-retired-navigation-token".into()),
            supersedes_approved_revision: Set(None),
            structural_revision: Set(1),
            design_fingerprint: Set("design".into()),
            plan_fingerprint: Set("plan".into()),
            block_cause_code: Set(None),
            block_source_manifest_revision: Set(None),
            completion_protocol_version: Set(2),
            completion_protocol_mode: Set(CompletionProtocolMode::V2Enforce),
            legacy_source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        delegation_task_run::ActiveModel {
            task_id: Set("retired-bound-task".into()),
            root_task_id: Set("retired-bound-task".into()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(root),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set("codex".into()),
            admission_class: Set(AdmissionClass::NormalRevision),
            lineage_root_task_id: Set("retired-bound-task".into()),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
        delegation_workflow_run_binding::ActiveModel {
            task_id: Set("retired-bound-task".into()),
            workflow_id: Set("workflow-retired-navigation".into()),
            node_id: Set("task-1".into()),
            manifest_revision: Set(1),
            lineage_ordinal: Set(1),
            summary_validated: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
        simple_workflow::ActiveModel {
            parent_conversation_id: Set(successor),
            plan_rel_path: Set("docs/plan.md".into()),
            progress_rel_path: Set(".superpowers/sdd/successor/progress.md".into()),
            source_workflow_id: Set(Some("workflow-retired-navigation".into())),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .unwrap();

        for conversation_id in [root, child] {
            let error = require_writable_conversation_workflow(&db.conn, conversation_id)
                .await
                .unwrap_err();
            assert_eq!(error.code(), "workflow_v2_retired");
            assert_eq!(error.to_string(), WORKFLOW_V2_RETIRED_MESSAGE);
            assert_eq!(error.source_conversation_id(), Some(root));
            assert_eq!(error.successor_conversation_id(), Some(successor));
            assert_eq!(error.can_create_simple_successor(), Some(false));
        }

        with_historical_workflow_fixture_mutations(async {
            for conversation_id in [root, child] {
                require_writable_conversation_workflow(&db.conn, conversation_id)
                    .await
                    .expect("explicit historical fixture scope may build archived read models");
            }
        })
        .await;
        for conversation_id in [root, child] {
            assert_eq!(
                require_writable_conversation_workflow(&db.conn, conversation_id)
                    .await
                    .unwrap_err()
                    .code(),
                "workflow_v2_retired"
            );
        }
    }

    #[tokio::test]
    async fn writable_guard_allows_ordinary_simple_and_no_manifest_a1_but_rejects_corrupt_identity(
    ) {
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, EntityTrait, Set};

        use crate::db::entities::delegation_task_run::{
            self, AdmissionClass, DelegationRunStatus,
        };
        use crate::db::entities::delegation_workflow::{self, WorkflowState};
        use crate::db::entities::{conversation, simple_workflow};
        use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
        use crate::models::AgentType;

        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/workflow-writable-modes").await;
        let ordinary = seed_conversation(&db, folder, AgentType::Codex).await;
        let simple = seed_conversation(&db, folder, AgentType::Codex).await;
        let archived = seed_conversation(&db, folder, AgentType::Codex).await;
        let prebound_child = seed_conversation(&db, folder, AgentType::Codex).await;
        let observed_a1_child = seed_conversation(&db, folder, AgentType::Codex).await;
        let now = Utc::now();

        simple_workflow::ActiveModel {
            parent_conversation_id: Set(simple),
            plan_rel_path: Set("docs/simple-plan.md".into()),
            progress_rel_path: Set(".superpowers/sdd/simple/progress.md".into()),
            source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        delegation_task_run::ActiveModel {
            task_id: Set("ordinary-a1-observed-task".into()),
            root_task_id: Set("ordinary-a1-observed-task".into()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(ordinary),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(observed_a1_child),
            agent_type: Set("codex".into()),
            admission_class: Set(AdmissionClass::NormalRevision),
            lineage_root_task_id: Set("ordinary-a1-observed-task".into()),
            work_unit_key: Set(Some("task|1|implementer|codex|none".into())),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
        delegation_workflow::ActiveModel {
            workflow_id: Set("workflow-corrupt-identity".into()),
            parent_conversation_id: Set(archived),
            workflow_kind: Set("brainstorm_to_delivery".into()),
            schema_version: Set(2),
            active_manifest_revision: Set(1),
            graph_revision: Set(1),
            workflow_state: Set(WorkflowState::Approved),
            capability_version: Set("workflow_manifest_v2".into()),
            publication_token: Set("workflow-corrupt-identity-token".into()),
            supersedes_approved_revision: Set(None),
            structural_revision: Set(1),
            design_fingerprint: Set("design".into()),
            plan_fingerprint: Set("plan".into()),
            block_cause_code: Set(None),
            block_source_manifest_revision: Set(None),
            completion_protocol_version: Set(2),
            completion_protocol_mode: Set(CompletionProtocolMode::V2Enforce),
            legacy_source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        let child = conversation::Entity::find_by_id(prebound_child)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut child: conversation::ActiveModel = child.into();
        child.parent_id = Set(Some(archived));
        child.update(&db.conn).await.unwrap();

        for conversation_id in [ordinary, simple, prebound_child, observed_a1_child] {
            require_writable_conversation_workflow(&db.conn, conversation_id)
                .await
                .unwrap();
        }

        simple_workflow::ActiveModel {
            parent_conversation_id: Set(archived),
            plan_rel_path: Set("docs/conflicting-plan.md".into()),
            progress_rel_path: Set(".superpowers/sdd/conflict/progress.md".into()),
            source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        let error = require_writable_conversation_workflow(&db.conn, archived)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "workflow_identity_corrupt");
        assert!(!matches!(
            error,
            WorkflowStoreError::WorkflowV2Retired { .. }
        ));
    }

    #[tokio::test]
    async fn archived_workflow_navigation_preserves_corrupt_header_classification() {
        use chrono::Utc;
        use sea_orm::{ActiveModelTrait, ConnectionTrait, DbBackend, Set, Statement};

        use crate::db::entities::delegation_workflow::{self, WorkflowState};
        use crate::db::test_helpers::{
            complete_historical_completion_protocol_migrations,
            historical_completion_protocol_db_before_v2_only, seed_conversation, seed_folder,
        };
        use crate::models::AgentType;

        let db = historical_completion_protocol_db_before_v2_only().await;
        let folder = seed_folder(&db, "/tmp/workflow-corrupt-header-navigation").await;
        let conversation_id = seed_conversation(&db, folder, AgentType::Codex).await;
        let now = Utc::now();
        delegation_workflow::ActiveModel {
            workflow_id: Set("workflow-corrupt-header-navigation".into()),
            parent_conversation_id: Set(conversation_id),
            workflow_kind: Set("brainstorm_to_delivery".into()),
            schema_version: Set(2),
            active_manifest_revision: Set(1),
            graph_revision: Set(1),
            workflow_state: Set(WorkflowState::Approved),
            capability_version: Set("workflow_manifest_v2".into()),
            publication_token: Set("workflow-corrupt-header-navigation-token".into()),
            supersedes_approved_revision: Set(None),
            structural_revision: Set(1),
            design_fingerprint: Set("design".into()),
            plan_fingerprint: Set("plan".into()),
            block_cause_code: Set(None),
            block_source_manifest_revision: Set(None),
            completion_protocol_version: Set(2),
            completion_protocol_mode: Set(CompletionProtocolMode::V2Enforce),
            legacy_source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        db.conn
            .execute_unprepared("PRAGMA ignore_check_constraints = ON")
            .await
            .unwrap();
        db.conn
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                "UPDATE delegation_workflows SET completion_protocol_mode = 'corrupt_mode' \
                 WHERE workflow_id = 'workflow-corrupt-header-navigation'",
            ))
            .await
            .unwrap();
        db.conn
            .execute_unprepared("PRAGMA ignore_check_constraints = OFF")
            .await
            .unwrap();
        complete_historical_completion_protocol_migrations(&db).await;

        let error = archived_workflow_navigation(&db.conn, conversation_id)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WorkflowStoreError::UnsupportedCompletionProtocolHeader(_)
        ));
        assert_eq!(error.code(), "unsupported_completion_protocol");
        assert!(!error.is_retryable());
    }
}
