//! Publish / settle / get_workflow_state core store.
//!
//! Document gates only for settle. Execution-gate evaluation is Task 4.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseTransaction, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use crate::db::entities::delegation_attention_request::{self, AttentionKind};
use crate::db::entities::delegation_plan_round_authorization;
use crate::db::entities::delegation_task_run::{self, CompletionState, DelegationRunStatus};
use crate::db::entities::delegation_workflow::{self, WorkflowState};
use crate::db::entities::delegation_workflow_design_root_binding;
use crate::db::entities::delegation_workflow_gate_settlement::{
    self, GateSettlementOutcome, PlanReviewNextAction as DbPlanReviewNextAction,
    PlanReviewScope as DbPlanReviewScope, PlanRevisionKind as DbPlanRevisionKind,
};
use crate::db::entities::delegation_workflow_gate_state;
use crate::db::entities::delegation_workflow_manifest_revision;
use crate::db::entities::delegation_workflow_node_binding::{self, NodeOutcome};
use crate::db::entities::delegation_workflow_outbox_event;
use crate::db::entities::delegation_workflow_run_binding;
use crate::db::entities::recovery_authorization;
use crate::db::entities::{conversation, folder};
use crate::db::AppDatabase;
use crate::web::event_bridge::EventEmitter;

use crate::acp::recovery_authorization::{
    canonical_json, consume_txn, validate_for_consumption_txn, AuthorizationConsumeExpectation,
    RecoveryAllowedAction, RecoveryAuthorizationError, RecoveryConsumerKind, RecoverySubjectKind,
};

use super::super::card_summary::{
    parse_and_validate_summary_json, CardSummary, ReviewVerdict, WorkStatus,
};
use super::artifact_resolver::{
    read_bounded_workspace_file, resolve_document, resolve_final_delivery, ArtifactError,
    ResolvedArtifact,
};
use super::completion_evidence::{
    load_validated_completion_evidence, load_validated_frozen_git_completion_evidence,
    open_design_self_review_decision_txn, V2GateEvidenceIdentity,
};
use super::completion_intent::CompletionOutcome;
use super::completion_projection::{
    load_completion_projection, load_workflow_completion_projection_batch,
    validated_design_self_review_outcome, DesignSelfReviewDecisionError,
};
use super::error::{require_v2_mutation_for_connection, WorkflowStoreError};
use super::events::{
    emit_workflow_graph_changed, emit_workflow_recovery_event, WorkflowRecoveryEvent,
};
use super::evidence_scope::{
    build_design_root_review_scope, canonical_json_sha256, DesignRootScopeInput,
};
use super::final_findings::resolve_active_final_findings_packages_v1;
use super::gates::{
    evaluate_execution_gate, reduce_design_gate, ExecutionGateInput, ExecutionGateKind,
    RequiredReviewerEvidence,
};
use super::plan_material::{
    authorize_localized_plan_change, bind_plan_material, classify_plan_change, parse_plan_material,
    plan_publication_material_decision, select_corrective_reviewers, PlanMaterialChangeInputV1,
    PlanMaterialError, PlanMaterialMap, PlanPublicationMaterialDecisionV1, MAX_PLAN_MATERIAL_BYTES,
};
use super::plan_review::{
    derive_next_plan_review_round_v2, derive_plan_review_round, derive_plan_review_round_v2,
    plan_round_authorization_digest_v2, reviewer_outcome_rank, PlanArtifactSnapshotV2,
    PlanFindingUpdate, PlanReviewChangeV2, PlanReviewDecisionV2, PlanReviewError,
    PlanReviewNextAction, PlanReviewRoundInputV2, PlanReviewRoundState, PlanReviewRoundStateV2,
    PlanReviewRoundSubmission, PlanReviewScope, PlanReviewerOutcomeV2, PlanRevisionKind,
    PlanRoundAuthorizationV2, MAX_PLAN_ROUND_AUTHORIZATION_JSON_BYTES,
};
use super::project::evidence_from_run_binding_and_validated;
use super::recovery_policy::{
    decide_workflow_recovery, hash_displayed_reset_reason, WorkflowRecoveryActiveRun,
    WorkflowRecoveryBindingLifecycle, WorkflowRecoveryCauseCode, WorkflowRecoveryDisposition,
    WorkflowRecoveryDocumentIdentity, WorkflowRecoveryFrozenTaskCohort,
    WorkflowRecoveryPlanGateEvidence, WorkflowRecoveryPlanIdentity,
    WorkflowRecoveryPlanReviewerRankV2, WorkflowRecoverySnapshot,
};
use super::state_dto::{
    project_workflow_state_index, PlanRecoverySourceDto, WorkflowGateStateDto,
    WorkflowNodeStateDto, WorkflowStateDto, WorkflowStateIndexDto,
};
#[cfg(any(test, feature = "test-utils"))]
use super::types::ManifestNodeOutcome;
#[cfg(any(test, feature = "test-utils"))]
use super::types::CURRENT_COMPLETION_PROTOCOL_VERSION;
use super::types::{
    DocumentGateKind, DocumentRef, ManifestDocument, ManifestNode, ManifestNodeKind,
    ManifestNodeRole, ManifestRevisionKind, ManifestWorkflowState, NormalizedGate,
    NormalizedManifest, NormalizedNode, PlanChangeClassification, ResolutionMode,
    WorkflowBlockCause, MAX_ADJUDICATION_SUMMARY_BYTES, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use super::validate::validate_manifest_document;

/// Capability version stamped on new headers (B9 / A15).
pub const WORKFLOW_CAPABILITY_VERSION: &str = "workflow_manifest_v2";
const MAX_DESIGN_SELF_REVIEW_BYTES: usize = 2 * 1024 * 1024;

/// One immutable active-manifest row validated against the workflow header.
/// Completion admission consumes this snapshot instead of caller-provided
/// material or a projection assembled from mutable graph state.
#[derive(Debug, Clone)]
pub struct ActiveManifestSnapshot {
    pub document: ManifestDocument,
    pub normalized: NormalizedManifest,
}

pub async fn load_active_manifest_snapshot<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow: &delegation_workflow::Model,
) -> Result<ActiveManifestSnapshot, WorkflowStoreError> {
    let document = load_active_manifest_document_txn(
        conn,
        &workflow.workflow_id,
        workflow.active_manifest_revision,
    )
    .await?;
    let normalized = validate_manifest_document(&document)?;
    if normalized
        .workflow_id
        .as_deref()
        .is_some_and(|workflow_id| workflow_id != workflow.workflow_id)
    {
        return Err(WorkflowStoreError::Persistence(
            "active manifest workflow identity does not match its durable header".into(),
        ));
    }
    Ok(ActiveManifestSnapshot {
        document,
        normalized,
    })
}

/// Store-facing validated input for a future durable Plan publication transition.
pub fn estimated_plan_publication_material_decision(
    prior_manifest: &NormalizedManifest,
    prior: &PlanMaterialMap,
    current_manifest: &NormalizedManifest,
    current: &PlanMaterialMap,
) -> Result<PlanPublicationMaterialDecisionV1, PlanMaterialError> {
    plan_publication_material_decision(prior_manifest, prior, current_manifest, current)
}

#[cfg(test)]
mod plan_material_publication_tests {
    use super::super::plan_material::parse_plan_material;
    use super::super::types::{
        DocumentGateKind, ManifestNodeKind, ManifestNodeRole, ManifestTaskPolicy, ManifestTaskRisk,
        ManifestTaskRoute, ManifestWorkflowState, NormalizedGate, NormalizedManifest,
        NormalizedNode, ResolutionMode, TaskRiskLevel, MANIFEST_SCHEMA_VERSION,
        TASK_RISK_POLICY_VERSION, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use super::estimated_plan_publication_material_decision;

    #[test]
    fn estimated_plan_publication_resets_for_derived_selector_set_changes() {
        let prior_manifest = manifest(false);
        let current_manifest = manifest(true);
        let plan = parse_plan_material(b"## Task 1\nbody\n", &[1]).unwrap();

        let unchanged = estimated_plan_publication_material_decision(
            &prior_manifest,
            &plan,
            &prior_manifest,
            &plan,
        )
        .unwrap();
        assert!(!unchanged.requires_new_lineage());
        assert!(!unchanged.selector_sets_changed());

        let changed = estimated_plan_publication_material_decision(
            &prior_manifest,
            &plan,
            &current_manifest,
            &plan,
        )
        .unwrap();
        assert!(changed.requires_new_lineage());
        assert!(changed.selector_sets_changed());
        assert_eq!(changed.prior_material().body("task.1"), Some("body\n"));
        assert_eq!(changed.current_material().body("task.1"), Some("body\n"));
    }

    fn manifest(add_second_plan_reviewer: bool) -> NormalizedManifest {
        let mut nodes = vec![
            node(
                "plan-reviewer-codex",
                "plan",
                ManifestNodeRole::Reviewer,
                "codex",
                None,
            ),
            node(
                "task-1-implementer",
                "tasks",
                ManifestNodeRole::Implementer,
                "codex",
                Some(1),
            ),
            node(
                "task-1-codex",
                "tasks",
                ManifestNodeRole::Reviewer,
                "codex",
                Some(1),
            ),
        ];
        let mut cohort = vec!["plan-reviewer-codex".to_string()];
        if add_second_plan_reviewer {
            nodes.push(node(
                "plan-reviewer-grok",
                "plan",
                ManifestNodeRole::Reviewer,
                "grok",
                None,
            ));
            cohort.push("plan-reviewer-grok".into());
        }
        NormalizedManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
            plan_target_rel_path: "docs/plan.md".into(),
            risk_policy_version: TASK_RISK_POLICY_VERSION.into(),
            workflow_id: Some("workflow-1".into()),
            expected_manifest_revision: Some(1),
            publication_token: "publication-1".into(),
            workflow_state: ManifestWorkflowState::Estimated,
            design: None,
            plan: None,
            phases: Vec::new(),
            nodes,
            edges: Vec::new(),
            gates: vec![NormalizedGate {
                id: "plan-gate".into(),
                reviewer_cohort_node_ids: cohort.clone(),
                required_reviewer_node_ids: cohort,
                resolution_mode: ResolutionMode::ParentAdjudication,
                gate_kind: DocumentGateKind::Plan,
            }],
            task_policies: vec![ManifestTaskPolicy {
                task_index: 1,
                risk: ManifestTaskRisk {
                    level: TaskRiskLevel::High,
                    hard_triggers: Vec::new(),
                    soft_signals: Vec::new(),
                    score: 0,
                    reason: "fixture".into(),
                },
                route: ManifestTaskRoute {
                    implementer_node_id: "task-1-implementer".into(),
                    reviewer_node_ids: vec!["task-1-codex".into()],
                },
                allow_noop_verification: false,
            }],
            task_count: 1,
        }
    }

    fn node(
        id: &str,
        phase_id: &str,
        role: ManifestNodeRole,
        agent_type: &str,
        task_index: Option<u32>,
    ) -> NormalizedNode {
        NormalizedNode {
            id: id.into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(phase_id.into()),
            role: Some(role),
            agent_type: Some(agent_type.into()),
            profile_id: None,
            task_index,
            work_unit_key: Some(format!("test|{id}")),
            deps: Vec::new(),
            required: true,
            node_outcome: None,
            title: None,
        }
    }
}

const MAX_PERSISTED_PLAN_EVIDENCE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PersistedPlanReviewEvidence {
    submission: PlanReviewRoundSubmission,
    state: PlanReviewRoundState,
}

// Test-only failpoints are thread-local so parallel cargo tests do not interfere.
#[cfg(test)]
thread_local! {
    static INJECT_PUBLISH_PERSISTENCE_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static BINDING_DIFF_INVOCATION_COUNT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static INJECT_RECOVERY_MANIFEST_READ_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static RECOVERY_REVISION_QUERY_COUNT: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn set_inject_publish_persistence_failure(enabled: bool) {
    INJECT_PUBLISH_PERSISTENCE_FAILURE.with(|c| c.set(enabled));
}

#[cfg(test)]
fn inject_publish_persistence_failure() -> bool {
    INJECT_PUBLISH_PERSISTENCE_FAILURE.with(|c| c.get())
}

#[cfg(not(test))]
fn inject_publish_persistence_failure() -> bool {
    false
}

#[cfg(test)]
fn reset_binding_diff_invocation_count() {
    BINDING_DIFF_INVOCATION_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn binding_diff_invocation_count() -> usize {
    BINDING_DIFF_INVOCATION_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
fn set_inject_recovery_manifest_read_failure(enabled: bool) {
    INJECT_RECOVERY_MANIFEST_READ_FAILURE.with(|flag| flag.set(enabled));
}

#[cfg(test)]
fn inject_recovery_manifest_read_failure() -> bool {
    INJECT_RECOVERY_MANIFEST_READ_FAILURE.with(|flag| flag.get())
}

#[cfg(not(test))]
fn inject_recovery_manifest_read_failure() -> bool {
    false
}

#[cfg(test)]
fn reset_recovery_revision_query_count() {
    RECOVERY_REVISION_QUERY_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn recovery_revision_query_count() -> usize {
    RECOVERY_REVISION_QUERY_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
fn note_recovery_revision_query() {
    RECOVERY_REVISION_QUERY_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
fn note_recovery_revision_query() {}

#[cfg(test)]
fn note_binding_diff_invocation() {
    BINDING_DIFF_INVOCATION_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(all(not(test), feature = "test-utils"))]
fn note_binding_diff_invocation() {}

// ---------------------------------------------------------------------------
// Request / result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PublishWorkflowRequest {
    pub document: ManifestDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum SettleGateEvidence {
    Design {
        critical_count: i64,
        important_count: i64,
        minor_count: i64,
    },
    Plan(PlanReviewRoundSubmission),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PublishResult {
    pub workflow_id: String,
    pub manifest_revision: u64,
    pub graph_revision: u64,
    pub workflow_state: ManifestWorkflowState,
    pub idempotent_replay: bool,
    pub publication_committed: bool,
    pub disposition: WorkflowPublicationDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<WorkflowRecoveryRequiredProjection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPublicationDisposition {
    Published,
    WorkflowRecoveryRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkflowRecoveryRequiredProjection {
    pub workflow_id: String,
    pub workflow_state: ManifestWorkflowState,
    pub block_cause: Option<WorkflowBlockCause>,
    pub block_source_manifest_revision: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct StateOnlyRevisionRequest<'a> {
    pub target_state: ManifestWorkflowState,
    pub transition_reason_code: &'a str,
    pub recovery_authorization_id: Option<&'a str>,
    pub consumer_correlation_id: Option<&'a str>,
    pub recovery_source_state_fingerprint: Option<&'a str>,
    pub recovery_risk_class: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateOnlyRevisionResult {
    pub workflow_id: String,
    pub manifest_revision: u64,
    pub source_manifest_revision: u64,
    pub graph_revision: u64,
    pub workflow_state: ManifestWorkflowState,
    pub block_cause: Option<WorkflowBlockCause>,
}

#[derive(Debug, Clone)]
pub struct WorkflowBlockEntryRequest<'a> {
    pub cause: WorkflowBlockCause,
    pub consumer_correlation_id: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
#[cfg(any(test, feature = "test-utils"))]
fn publish_result(
    workflow_id: String,
    manifest_revision: u64,
    graph_revision: u64,
    workflow_state: ManifestWorkflowState,
    idempotent_replay: bool,
    publication_committed: bool,
    block_cause_code: Option<&str>,
    block_source_manifest_revision: Option<i64>,
) -> Result<PublishResult, WorkflowStoreError> {
    let blocked = workflow_state == ManifestWorkflowState::Blocked;
    let recovery = if blocked {
        let block_cause = WorkflowBlockCause::from_db(block_cause_code)
            .map_err(WorkflowStoreError::Persistence)?;
        Some(WorkflowRecoveryRequiredProjection {
            workflow_id: workflow_id.clone(),
            workflow_state,
            block_cause: Some(block_cause),
            block_source_manifest_revision: block_source_manifest_revision
                .map(|value| value as u64),
        })
    } else {
        None
    };
    Ok(PublishResult {
        workflow_id,
        manifest_revision,
        graph_revision,
        workflow_state,
        idempotent_replay,
        publication_committed,
        disposition: if blocked {
            WorkflowPublicationDisposition::WorkflowRecoveryRequired
        } else {
            WorkflowPublicationDisposition::Published
        },
        recovery,
    })
}

#[derive(Debug, Clone)]
pub(super) struct SettleWorkflowRequest {
    pub workflow_id: String,
    pub manifest_revision: u64,
    pub gate_id: String,
    pub expected_graph_revision: u64,
    pub gate_cycle: u64,
    pub outcome: GateSettlementOutcome,
    pub evidence: SettleGateEvidence,
    pub summary: String,
    pub recovery_authorization_id: Option<String>,
}

/// Protocol-v2 settlement request. All evidence identity, outcome reduction,
/// artifact coverage, lineage, round, and settlement-cycle fields come from
/// durable platform state.
#[derive(Debug, Clone)]
pub struct SettleWorkflowV2Request {
    pub workflow_id: String,
    pub gate_id: String,
    pub expected_graph_revision: u64,
    pub expected_review_round: Option<u64>,
    pub expected_outcome: Option<GateSettlementOutcome>,
    pub summary: String,
    pub recovery_authorization_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FinalDeliveryGuardRequest {
    pub workflow_id: String,
    pub gate_id: String,
    pub workspace_path: PathBuf,
    pub final_reviewer_task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalReviewReopened {
    pub workflow_id: String,
    pub gate_id: String,
    pub gate_lineage: String,
    pub review_round: i64,
    pub required_reviewer_node_ids: Vec<String>,
    pub graph_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalDeliveryGuardResult {
    Ready(ResolvedArtifact),
    Rejected(ArtifactError),
    Reopened {
        diagnostic: ArtifactError,
        state: FinalReviewReopened,
    },
}

impl FinalDeliveryGuardResult {
    pub fn diagnostic_code(&self) -> Option<&'static str> {
        match self {
            Self::Ready(_) => None,
            Self::Rejected(diagnostic) | Self::Reopened { diagnostic, .. } => {
                Some(diagnostic.code())
            }
        }
    }

    pub fn reopened(&self) -> Option<&FinalReviewReopened> {
        match self {
            Self::Reopened { state, .. } => Some(state),
            Self::Ready(_) | Self::Rejected(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RecoverWorkflowRequest {
    pub workflow_id: String,
    pub recovery_authorization_id: String,
    pub expected_manifest_revision: u64,
    pub correlation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RecoverWorkflowResult {
    pub workflow_id: String,
    pub old_state: ManifestWorkflowState,
    pub new_state: ManifestWorkflowState,
    pub source_manifest_revision: u64,
    pub manifest_revision: u64,
    pub graph_revision: u64,
    pub cause_code: String,
    pub recovery_authorization_id: String,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SettleResult {
    pub workflow_id: String,
    pub gate_id: String,
    pub gate_cycle: u64,
    pub graph_revision: u64,
    pub manifest_revision: u64,
    pub outcome: GateSettlementOutcome,
    pub idempotent_replay: bool,
    pub plan_next_action: Option<PlanReviewNextAction>,
    pub critical_count: i64,
    pub important_count: i64,
    pub minor_count: i64,
    pub stagnation_count: u32,
    pub rewrite_used: bool,
    #[serde(skip)]
    pub plan_metric_observation: Option<PlanSettlementMetricObservation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanSettlementMetricObservation {
    pub change: PlanReviewChangeV2,
    pub localized_intersection: bool,
    pub lineage_reset: bool,
    pub sibling_reruns: u64,
}

struct ApprovedPlanLineageReset {
    authorization: recovery_authorization::Model,
    source_state_fingerprint: String,
    action_payload: serde_json::Value,
    consumer_id: String,
    consumer_correlation_id: String,
    cause_code: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Production manifest publication is permanently retired. Historical
/// manifests remain readable and are seeded through the test-only fixture.
pub async fn publish_workflow_manifest_core(
    db: &AppDatabase,
    _emitter: &EventEmitter,
    parent_conversation_id: i32,
    _req: PublishWorkflowRequest,
) -> Result<PublishResult, WorkflowStoreError> {
    let error = match super::error::workflow_v2_publication_retired_for_conversation(
        &db.conn,
        parent_conversation_id,
    )
    .await
    {
        Ok(error) => error,
        Err(error @ WorkflowStoreError::WorkflowIdentityCorrupt { .. }) => error,
        Err(_) => WorkflowStoreError::workflow_v2_retired_with_navigation(parent_conversation_id),
    };
    Err(error)
}

/// Historical manifest fixture publication for read-model tests only.
#[cfg(any(test, feature = "test-utils"))]
pub async fn publish_workflow_manifest_fixture(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: PublishWorkflowRequest,
) -> Result<PublishResult, WorkflowStoreError> {
    super::error::with_historical_workflow_fixture_mutations(
        publish_workflow_manifest_fixture_inner(db, emitter, parent_conversation_id, req),
    )
    .await
}

#[cfg(any(test, feature = "test-utils"))]
async fn publish_workflow_manifest_fixture_inner(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: PublishWorkflowRequest,
) -> Result<PublishResult, WorkflowStoreError> {
    ensure_parent_exists(db, parent_conversation_id).await?;

    let normalized = validate_manifest_document(&req.document)?;
    let stored_doc = normalized_to_document(&normalized);
    let document_json = serde_json::to_string(&stored_doc)
        .map_err(|e| WorkflowStoreError::Persistence(format!("serialize manifest: {e}")))?;
    let document_digest = sha256_hex(document_json.as_bytes());

    let now = Utc::now();
    let protocol_version = CURRENT_COMPLETION_PROTOCOL_VERSION;
    let protocol_mode = super::types::current_completion_protocol_mode();
    let publication_token = normalized.publication_token.clone();
    let document_digest_for_race = document_digest.clone();

    // Token lookup + create/update run in one write transaction (A3/B8).
    // Concurrent same-token creates: unique/busy → SAVEPOINT rollback → reclassify;
    // if the winner is not visible under this snapshot, outer fresh reclassify.
    let result = db
        .conn
        .transaction::<_, PublishResult, WorkflowStoreError>(|txn| {
            Box::pin(async move {
                publish_in_txn(
                    txn,
                    parent_conversation_id,
                    &normalized,
                    &document_digest,
                    now,
                    protocol_version,
                    protocol_mode,
                )
                .await
            })
        })
        .await;

    let result = match result {
        Ok(r) => r,
        Err(sea_orm::TransactionError::Connection(e)) => {
            // SQLITE_BUSY / BUSY_SNAPSHOT on connection: reclassify with a fresh snapshot.
            if is_busy_or_snapshot_err_str(&e.to_string()) {
                classify_token_race_fresh(
                    db,
                    &publication_token,
                    &document_digest_for_race,
                    parent_conversation_id,
                )
                .await?
            } else {
                return Err(WorkflowStoreError::Persistence(e.to_string()));
            }
        }
        Err(sea_orm::TransactionError::Transaction(e)) => {
            if is_token_race_reclassify_marker(&e) {
                classify_token_race_fresh(
                    db,
                    &publication_token,
                    &document_digest_for_race,
                    parent_conversation_id,
                )
                .await?
            } else {
                return Err(e);
            }
        }
    };

    if result.publication_committed {
        emit_workflow_graph_changed(
            emitter,
            parent_conversation_id,
            &result.workflow_id,
            result.graph_revision,
        );
    }

    Ok(result)
}

/// Freeze delivery to the exact platform-bound passing Final artifact. A
/// commit-id mismatch rotates the Final lineage and full reviewer cohort in
/// the same transaction; callers emit delivery success only for `Ready`.
pub async fn guard_final_delivery_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    request: FinalDeliveryGuardRequest,
) -> Result<FinalDeliveryGuardResult, WorkflowStoreError> {
    require_stored_v2_header(&db.conn, &request.workflow_id).await?;
    let parent_conversation_id =
        delegation_workflow::Entity::find_by_id(request.workflow_id.clone())
            .one(&db.conn)
            .await
            .map_err(db_err)?
            .ok_or_else(|| WorkflowStoreError::NotFound(request.workflow_id.clone()))?
            .parent_conversation_id;
    let result = db
        .conn
        .transaction::<_, FinalDeliveryGuardResult, WorkflowStoreError>(|txn| {
            Box::pin(guard_final_delivery_txn(txn, request))
        })
        .await
        .map_err(|error| match error {
            sea_orm::TransactionError::Connection(error) => db_err(error),
            sea_orm::TransactionError::Transaction(error) => error,
        })?;
    if let Some(reopened) = result.reopened() {
        emit_workflow_graph_changed(
            emitter,
            parent_conversation_id,
            &reopened.workflow_id,
            reopened.graph_revision,
        );
    }
    Ok(result)
}

/// Run the Final delivery freeze once every required reviewer in the current
/// Final cohort has a passing run. Root workflow-state reads call this before
/// projection so a live branch drift cannot bypass the platform-owned gate.
pub async fn guard_current_final_delivery_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    workflow_id: Option<&str>,
) -> Result<Option<FinalDeliveryGuardResult>, WorkflowStoreError> {
    let workflow_id = match workflow_id {
        Some(workflow_id) => workflow_id.to_string(),
        None => load_workflow_id_by_parent_kind(&db.conn, parent_conversation_id)
            .await?
            .ok_or_else(|| {
                WorkflowStoreError::NotFound(format!(
                    "parent={parent_conversation_id} kind={WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY}"
                ))
            })?,
    };
    require_owned_stored_v2_header(&db.conn, &workflow_id, parent_conversation_id).await?;
    let header = delegation_workflow::Entity::find_by_id(&workflow_id)
        .one(&db.conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(workflow_id.clone()))?;
    if header.parent_conversation_id != parent_conversation_id {
        return Err(WorkflowStoreError::CrossParent {
            workflow_id: header.workflow_id,
            expected_parent: parent_conversation_id,
            actual_parent: header.parent_conversation_id,
        });
    }
    let Some(request) = current_final_delivery_request(db, &header, None).await? else {
        return Ok(None);
    };
    guard_final_delivery_core(db, emitter, request)
        .await
        .map(Some)
}

/// Run the Final delivery freeze while enriching one terminal task response.
/// Non-Final, stale, non-required, and incomplete-cohort tasks are not delivery
/// candidates and therefore leave ordinary completion projection untouched.
pub async fn guard_task_final_delivery_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    task_id: &str,
) -> Result<Option<FinalDeliveryGuardResult>, WorkflowStoreError> {
    let Some(binding) = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(&db.conn)
        .await
        .map_err(db_err)?
    else {
        return Ok(None);
    };
    require_stored_v2_header(&db.conn, &binding.workflow_id).await?;
    let header = delegation_workflow::Entity::find_by_id(binding.workflow_id)
        .one(&db.conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(task_id.to_string()))?;
    let Some(request) = current_final_delivery_request(db, &header, Some(task_id)).await? else {
        return Ok(None);
    };
    guard_final_delivery_core(db, emitter, request)
        .await
        .map(Some)
}

async fn current_final_delivery_request(
    db: &AppDatabase,
    header: &delegation_workflow::Model,
    required_task_id: Option<&str>,
) -> Result<Option<FinalDeliveryGuardRequest>, WorkflowStoreError> {
    let snapshot = load_active_manifest_snapshot(&db.conn, header).await?;
    let required_final_reviewers = snapshot
        .normalized
        .nodes
        .iter()
        .filter(|node| {
            node.phase_id.as_deref() == Some(super::types::PHASE_FINAL)
                && node.role == Some(ManifestNodeRole::Reviewer)
                && node.required
        })
        .collect::<Vec<_>>();
    if required_final_reviewers.is_empty() {
        return Ok(None);
    }
    let Some(current_final_gate) = delegation_workflow_gate_state::Entity::find_by_id((
        header.workflow_id.clone(),
        "final".to_string(),
    ))
    .one(&db.conn)
    .await
    .map_err(db_err)?
    else {
        return Ok(None);
    };
    // Align with selection-aware `guard_final_delivery_txn`: selected nodes
    // must cover the current review round, while retained unselected siblings
    // may keep earlier same-lineage rounds after roster-only add/remove.
    let selected_node_ids = match serde_json::from_str::<BTreeSet<String>>(
        &current_final_gate.selected_node_ids_json,
    ) {
        Ok(ids) => ids,
        Err(_) => return Ok(None),
    };
    let required_reviewer_node_ids = required_final_reviewers
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    if !selected_node_ids.is_subset(&required_reviewer_node_ids) {
        return Ok(None);
    }
    let binding_covers_current_selection = |candidate: &delegation_workflow_run_binding::Model| {
        candidate.gate_id.as_deref() == Some(current_final_gate.gate_id.as_str())
            && candidate.gate_lineage.as_deref() == Some(current_final_gate.gate_lineage.as_str())
            && if selected_node_ids.contains(&candidate.node_id) {
                candidate.review_round == Some(current_final_gate.current_review_round)
            } else {
                candidate.review_round.is_some_and(|review_round| {
                    review_round > 0 && review_round < current_final_gate.current_review_round
                })
            }
    };
    let mut delivery_anchor = None;
    let mut preferred_selected_anchor = None;
    let mut required_task_anchor = None;
    for reviewer in required_final_reviewers {
        let Some(binding) = delegation_workflow_run_binding::Entity::find()
            .filter(
                delegation_workflow_run_binding::Column::WorkflowId.eq(header.workflow_id.clone()),
            )
            .filter(delegation_workflow_run_binding::Column::NodeId.eq(reviewer.id.clone()))
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
            .one(&db.conn)
            .await
            .map_err(db_err)?
        else {
            return Ok(None);
        };
        if !binding_covers_current_selection(&binding) {
            return Ok(None);
        }
        let Some(run) = delegation_task_run::Entity::find_by_id(&binding.task_id)
            .one(&db.conn)
            .await
            .map_err(db_err)?
        else {
            return Ok(None);
        };
        let passing_outcome = run
            .completion_outcome
            .as_deref()
            .is_some_and(|outcome| matches!(outcome, "approve" | "approve_with_minors"));
        if run.status != DelegationRunStatus::Completed
            || run.completion_state != Some(CompletionState::Resolved)
            || !passing_outcome
        {
            return Ok(None);
        }
        if delivery_anchor.is_none() {
            delivery_anchor = Some((binding.clone(), run.clone()));
        }
        if selected_node_ids.contains(&binding.node_id) {
            preferred_selected_anchor = Some((binding.clone(), run.clone()));
        }
        if required_task_id == Some(binding.task_id.as_str()) {
            required_task_anchor = Some((binding, run));
        }
    }
    let (binding, run) = match required_task_id {
        Some(_) => match required_task_anchor {
            Some(anchor) => anchor,
            None => return Ok(None),
        },
        None => preferred_selected_anchor
            .or(delivery_anchor)
            .expect("required Final cohort is non-empty"),
    };
    let workspace_path = run
        .workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            WorkflowStoreError::GateNotReady(
                "current passing Final reviewer has no durable workspace".into(),
            )
        })?;
    Ok(Some(FinalDeliveryGuardRequest {
        workflow_id: header.workflow_id.clone(),
        gate_id: current_final_gate.gate_id,
        workspace_path,
        final_reviewer_task_id: binding.task_id,
    }))
}

async fn guard_final_delivery_txn(
    txn: &DatabaseTransaction,
    request: FinalDeliveryGuardRequest,
) -> Result<FinalDeliveryGuardResult, WorkflowStoreError> {
    require_stored_v2_header(txn, &request.workflow_id).await?;
    let header = delegation_workflow::Entity::find_by_id(request.workflow_id.clone())
        .one(txn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(request.workflow_id.clone()))?;
    require_v2_mutation_for_connection(
        txn,
        header.completion_protocol_version,
        &header.completion_protocol_mode,
    )
    .await?;
    let gate_state = delegation_workflow_gate_state::Entity::find_by_id((
        request.workflow_id.clone(),
        request.gate_id.clone(),
    ))
    .one(txn)
    .await
    .map_err(db_err)?
    .ok_or_else(|| {
        WorkflowStoreError::GateNotReady("current Final gate state is missing".into())
    })?;
    let binding =
        delegation_workflow_run_binding::Entity::find_by_id(request.final_reviewer_task_id.clone())
            .one(txn)
            .await
            .map_err(db_err)?
            .ok_or_else(|| WorkflowStoreError::NotFound(request.final_reviewer_task_id.clone()))?;
    if binding.workflow_id != request.workflow_id {
        return Err(WorkflowStoreError::GateNotReady(
            "Final delivery evidence belongs to another workflow".into(),
        ));
    }
    let node = delegation_workflow_node_binding::Entity::find_by_id((
        binding.workflow_id.clone(),
        binding.node_id.clone(),
    ))
    .one(txn)
    .await
    .map_err(db_err)?
    .ok_or_else(|| WorkflowStoreError::GateNotReady("Final reviewer node is missing".into()))?;
    if node.phase_id != super::types::PHASE_FINAL || node.role != "reviewer" {
        return Err(WorkflowStoreError::GateNotReady(
            "delivery evidence is not a Final reviewer artifact".into(),
        ));
    }
    if binding.gate_id.as_deref() != Some(request.gate_id.as_str())
        || binding.gate_lineage.as_deref() != Some(gate_state.gate_lineage.as_str())
    {
        return Err(WorkflowStoreError::GateNotReady(
            "Final reviewer evidence is not bound to the current gate lineage".into(),
        ));
    }
    let run = delegation_task_run::Entity::find_by_id(binding.task_id.clone())
        .one(txn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(binding.task_id.clone()))?;
    if run.status != DelegationRunStatus::Completed {
        return Err(WorkflowStoreError::GateNotReady(
            "Final reviewer evidence is not terminally completed".into(),
        ));
    }
    let snapshot = load_active_manifest_snapshot(txn, &header).await?;
    let required_reviewer_node_ids = snapshot
        .normalized
        .nodes
        .iter()
        .filter(|candidate| {
            candidate.phase_id.as_deref() == Some(super::types::PHASE_FINAL)
                && candidate.role == Some(ManifestNodeRole::Reviewer)
                && candidate.required
        })
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    if required_reviewer_node_ids.is_empty() {
        return Err(WorkflowStoreError::GateNotReady(
            "Final delivery requires a non-empty reviewer cohort".into(),
        ));
    }
    let selected_node_ids = serde_json::from_str::<BTreeSet<String>>(
        &gate_state.selected_node_ids_json,
    )
    .map_err(|error| {
        WorkflowStoreError::GateNotReady(format!(
            "current Final reviewer selection is invalid: {error}"
        ))
    })?;
    let required_reviewer_node_id_set = required_reviewer_node_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if !selected_node_ids.is_subset(&required_reviewer_node_id_set) {
        return Err(WorkflowStoreError::GateNotReady(
            "current Final reviewer selection is outside the required cohort".into(),
        ));
    }
    let binding_covers_current_selection = |candidate: &delegation_workflow_run_binding::Model| {
        candidate.gate_id.as_deref() == Some(request.gate_id.as_str())
            && candidate.gate_lineage.as_deref() == Some(gate_state.gate_lineage.as_str())
            && if selected_node_ids.contains(&candidate.node_id) {
                candidate.review_round == Some(gate_state.current_review_round)
            } else {
                candidate.review_round.is_some_and(|review_round| {
                    review_round > 0 && review_round < gate_state.current_review_round
                })
            }
    };
    if !required_reviewer_node_id_set.contains(&binding.node_id)
        || !binding_covers_current_selection(&binding)
    {
        return Err(WorkflowStoreError::GateNotReady(
            "Final reviewer evidence does not cover the current selective review round".into(),
        ));
    }

    let mut request_is_current = false;
    let mut required_reviewers = Vec::with_capacity(required_reviewer_node_ids.len());
    for reviewer_node_id in &required_reviewer_node_ids {
        let latest_binding = delegation_workflow_run_binding::Entity::find()
            .filter(
                delegation_workflow_run_binding::Column::WorkflowId.eq(request.workflow_id.clone()),
            )
            .filter(delegation_workflow_run_binding::Column::NodeId.eq(reviewer_node_id.clone()))
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
            .one(txn)
            .await
            .map_err(db_err)?;
        let evidence = match latest_binding {
            Some(latest_binding) => {
                if !binding_covers_current_selection(&latest_binding) {
                    return Err(WorkflowStoreError::GateNotReady(format!(
                        "Final reviewer {reviewer_node_id} evidence does not cover the current selective review round"
                    )));
                }
                request_is_current |= latest_binding.task_id == binding.task_id;
                let latest_run = delegation_task_run::Entity::find_by_id(&latest_binding.task_id)
                    .one(txn)
                    .await
                    .map_err(db_err)?;
                match latest_run {
                    Some(latest_run) => {
                        let validated = if latest_run.status == DelegationRunStatus::Completed {
                            Some(
                                load_validated_frozen_git_completion_evidence(
                                    txn,
                                    &latest_run.task_id,
                                )
                                .await
                                .map_err(|error| {
                                    WorkflowStoreError::GateNotReady(format!(
                                        "Final reviewer evidence failed v2 validation: {error}"
                                    ))
                                })?,
                            )
                        } else {
                            None
                        };
                        Some(evidence_from_run_binding_and_validated(
                            &latest_run,
                            &latest_binding,
                            2,
                            validated.as_ref(),
                        ))
                    }
                    None => None,
                }
            }
            None => None,
        };
        required_reviewers.push(RequiredReviewerEvidence {
            node_id: reviewer_node_id.clone(),
            evidence,
        });
    }
    if !request_is_current {
        return Err(WorkflowStoreError::GateNotReady(
            "Final delivery evidence is not the current required reviewer run".into(),
        ));
    }

    let final_fixer_node_id = snapshot
        .normalized
        .nodes
        .iter()
        .find(|candidate| {
            candidate.phase_id.as_deref() == Some(super::types::PHASE_FINAL)
                && candidate.role == Some(ManifestNodeRole::Fixer)
        })
        .map(|candidate| candidate.id.clone());
    let implementer_or_fixer = if let Some(final_fixer_node_id) = final_fixer_node_id {
        let latest_binding = delegation_workflow_run_binding::Entity::find()
            .filter(
                delegation_workflow_run_binding::Column::WorkflowId.eq(request.workflow_id.clone()),
            )
            .filter(delegation_workflow_run_binding::Column::NodeId.eq(final_fixer_node_id))
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
            .one(txn)
            .await
            .map_err(db_err)?;
        match latest_binding {
            Some(latest_binding) => {
                let latest_run = delegation_task_run::Entity::find_by_id(&latest_binding.task_id)
                    .one(txn)
                    .await
                    .map_err(db_err)?;
                match latest_run {
                    Some(latest_run) => {
                        let validated = if latest_run.status == DelegationRunStatus::Completed {
                            Some(
                                load_validated_frozen_git_completion_evidence(
                                    txn,
                                    &latest_run.task_id,
                                )
                                .await
                                .map_err(|error| {
                                    WorkflowStoreError::GateNotReady(format!(
                                        "Final fixer evidence failed v2 validation: {error}"
                                    ))
                                })?,
                            )
                        } else {
                            None
                        };
                        Some(evidence_from_run_binding_and_validated(
                            &latest_run,
                            &latest_binding,
                            2,
                            validated.as_ref(),
                        ))
                    }
                    None => None,
                }
            }
            None => None,
        }
    } else {
        None
    };
    let expected_final_head = required_reviewers
        .iter()
        .find_map(|required| required.evidence.as_ref())
        .and_then(|evidence| evidence.artifact_digest.clone())
        .ok_or_else(|| {
            WorkflowStoreError::GateNotReady("Final reviewer artifact is missing".into())
        })?;
    let final_gate = evaluate_execution_gate(&ExecutionGateInput {
        kind: ExecutionGateKind::Final,
        implementer_or_fixer,
        required_reviewers,
        branch_tip_digest: Some(expected_final_head.clone()),
    });
    if !final_gate.passed {
        return Err(WorkflowStoreError::GateNotReady(format!(
            "Final execution gate is not currently passing: {:?}",
            final_gate.reason
        )));
    }
    let resolved = resolve_final_delivery(&request.workspace_path, &expected_final_head).await;
    let diagnostic = match resolved {
        Ok(artifact) => {
            resolve_active_final_findings_packages_v1(
                txn,
                &request.workflow_id,
                &request.gate_id,
                header.graph_revision,
            )
            .await
            .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
            return Ok(FinalDeliveryGuardResult::Ready(artifact));
        }
        Err(diagnostic @ ArtifactError::FinalArtifactDrift { .. }) => diagnostic,
        Err(diagnostic) => return Ok(FinalDeliveryGuardResult::Rejected(diagnostic)),
    };

    let review_round = gate_state
        .current_review_round
        .checked_add(1)
        .ok_or_else(|| WorkflowStoreError::Persistence("Final review round overflow".into()))?;
    let graph_revision = header.graph_revision.checked_add(1).ok_or_else(|| {
        WorkflowStoreError::Persistence("workflow graph revision overflow".into())
    })?;
    let gate_lineage = mint_final_drift_lineage(
        &request.workflow_id,
        &request.gate_id,
        &gate_state.gate_lineage,
        review_round,
        &diagnostic,
    );
    let selected_node_ids_json = serde_json::to_string(&required_reviewer_node_ids)
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    let now = Utc::now();
    let diagnostic_payload = serde_json::json!({
        "diagnostic": diagnostic.code(),
        "workflow_id": request.workflow_id.clone(),
        "gate_id": request.gate_id.clone(),
        "prior_gate_lineage": gate_state.gate_lineage.clone(),
        "gate_lineage": gate_lineage.clone(),
        "review_round": review_round,
        "required_reviewer_node_ids": required_reviewer_node_ids.clone(),
        "final_reviewer_task_id": request.final_reviewer_task_id.clone(),
        "graph_revision": graph_revision,
    });
    let diagnostic_payload_json = serde_json::to_string(&diagnostic_payload)
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    let event_key = format!(
        "codeg.final-artifact-drift.v1\0{}\0{}\0{}",
        request.workflow_id, request.gate_id, graph_revision
    );
    let event_id = format!("sha256:{}", sha256_hex(event_key.as_bytes()));

    let mut state_active: delegation_workflow_gate_state::ActiveModel = gate_state.into();
    state_active.gate_lineage = Set(gate_lineage.clone());
    state_active.current_review_round = Set(review_round);
    state_active.selected_node_ids_json = Set(selected_node_ids_json);
    state_active.update(txn).await.map_err(db_err)?;
    let mut header_active: delegation_workflow::ActiveModel = header.into();
    header_active.graph_revision = Set(graph_revision);
    header_active.updated_at = Set(now);
    header_active.update(txn).await.map_err(db_err)?;
    delegation_workflow_outbox_event::ActiveModel {
        event_id: Set(event_id),
        workflow_id: Set(request.workflow_id.clone()),
        graph_revision: Set(graph_revision),
        event_kind: Set("final_artifact_drift".into()),
        subject_key: Set(request.gate_id.clone()),
        payload_json: Set(diagnostic_payload_json),
        dispatch_attempts: Set(0),
        created_at: Set(now),
        delivered_at: Set(None),
    }
    .insert(txn)
    .await
    .map_err(db_err)?;

    Ok(FinalDeliveryGuardResult::Reopened {
        diagnostic,
        state: FinalReviewReopened {
            workflow_id: request.workflow_id,
            gate_id: request.gate_id,
            gate_lineage,
            review_round,
            required_reviewer_node_ids,
            graph_revision: graph_revision as u64,
        },
    })
}

fn mint_final_drift_lineage(
    workflow_id: &str,
    gate_id: &str,
    prior_lineage: &str,
    review_round: i64,
    diagnostic: &ArtifactError,
) -> String {
    let (expected, actual) = match diagnostic {
        ArtifactError::FinalArtifactDrift { expected, actual } => {
            (expected.as_str(), actual.as_str())
        }
        _ => ("", ""),
    };
    let material = format!(
        "codeg.final-drift-lineage.v1\0{workflow_id}\0{gate_id}\0{prior_lineage}\0{review_round}\0{expected}\0{actual}"
    );
    format!("sha256:{}", sha256_hex(material.as_bytes()))
}

/// Recover a blocked workflow in place. The authorization is validated and
/// consumed in the same transaction as the state-only manifest revision.
/// Event delivery happens only after commit; delivery failure is logged and
/// the committed result is still returned to the caller.
pub async fn recover_workflow_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: RecoverWorkflowRequest,
) -> Result<RecoverWorkflowResult, WorkflowStoreError> {
    require_owned_stored_v2_header(&db.conn, &req.workflow_id, parent_conversation_id).await?;
    let rejection_req = req.clone();
    let result = db
        .conn
        .transaction::<_, RecoverWorkflowResult, WorkflowStoreError>(|txn| {
            Box::pin(async move {
                require_owned_stored_v2_header(txn, &req.workflow_id, parent_conversation_id)
                    .await?;
                let header = delegation_workflow::Entity::find_by_id(req.workflow_id.clone())
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or_else(|| WorkflowStoreError::NotFound(req.workflow_id.clone()))?;
                require_v2_mutation_for_connection(
                    txn,
                    header.completion_protocol_version,
                    &header.completion_protocol_mode,
                )
                .await?;

                if let Some(replay) =
                    load_committed_recovery_replay_txn(txn, &header, parent_conversation_id, &req)
                        .await?
                {
                    return Ok(replay);
                }

                if header.parent_conversation_id != parent_conversation_id {
                    return Err(WorkflowStoreError::CrossParent {
                        workflow_id: header.workflow_id.clone(),
                        expected_parent: parent_conversation_id,
                        actual_parent: header.parent_conversation_id,
                    });
                }
                if header.workflow_state != WorkflowState::Blocked {
                    return Err(WorkflowStoreError::WorkflowRecoveryNotAvailable);
                }
                if header.active_manifest_revision as u64 != req.expected_manifest_revision {
                    return Err(WorkflowStoreError::StaleManifestRevision {
                        expected: req.expected_manifest_revision,
                        current: header.active_manifest_revision as u64,
                    });
                }

                let snapshot = load_workflow_recovery_snapshot_txn(txn, &header, None).await?;
                let decision = decide_workflow_recovery(&snapshot);
                let WorkflowRecoveryDisposition::Recover { target_state } = decision.disposition
                else {
                    return Err(WorkflowStoreError::WorkflowRecoveryNotAvailable);
                };
                let action_payload = decision
                    .action_payload()
                    .expect("recover decision has action payload");
                let next_manifest_revision = header.active_manifest_revision + 1;
                let consumer_id = next_manifest_revision.to_string();
                let expectation = AuthorizationConsumeExpectation {
                    parent_conversation_id,
                    subject_kind: RecoverySubjectKind::Workflow,
                    subject_id: &header.workflow_id,
                    source_state_fingerprint: &decision.source_state_fingerprint,
                    allowed_action: RecoveryAllowedAction::RecoverWorkflow,
                    action_payload: &action_payload,
                    consumer_kind: RecoveryConsumerKind::WorkflowManifestRevision,
                    consumer_id: &consumer_id,
                    consumer_correlation_id: &req.correlation_id,
                };
                let approved = validate_for_consumption_txn(
                    txn,
                    &req.recovery_authorization_id,
                    &expectation,
                    Utc::now(),
                )
                .await
                .map_err(map_workflow_authorization_error)?;

                let now = Utc::now();
                let revision = append_state_only_revision_txn(
                    txn,
                    &header,
                    StateOnlyRevisionRequest {
                        target_state,
                        transition_reason_code: decision.cause_code.as_str(),
                        recovery_authorization_id: Some(&req.recovery_authorization_id),
                        consumer_correlation_id: Some(&req.correlation_id),
                        recovery_source_state_fingerprint: Some(&decision.source_state_fingerprint),
                        recovery_risk_class: Some(decision.risk_class.as_str()),
                    },
                    now,
                )
                .await?;
                consume_txn(txn, approved, &expectation, now)
                    .await
                    .map_err(map_workflow_authorization_error)?;

                Ok(RecoverWorkflowResult {
                    workflow_id: header.workflow_id,
                    old_state: ManifestWorkflowState::Blocked,
                    new_state: target_state,
                    source_manifest_revision: revision.source_manifest_revision,
                    manifest_revision: revision.manifest_revision,
                    graph_revision: revision.graph_revision,
                    cause_code: decision.cause_code.as_str().to_string(),
                    recovery_authorization_id: req.recovery_authorization_id.clone(),
                    idempotent_replay: false,
                })
            })
        })
        .await;

    let result = match result {
        Ok(result) => result,
        Err(sea_orm::TransactionError::Connection(error)) => {
            return Err(WorkflowStoreError::Persistence(error.to_string()));
        }
        Err(sea_orm::TransactionError::Transaction(error)) => {
            emit_recover_workflow_rejection_if_designated(
                db,
                emitter,
                parent_conversation_id,
                &rejection_req,
                &error,
            )
            .await;
            return Err(error);
        }
    };
    if !result.idempotent_replay {
        emit_committed_recovery_events(emitter, &result, "recover_workflow", false);
        emit_workflow_graph_changed(
            emitter,
            parent_conversation_id,
            &result.workflow_id,
            result.graph_revision,
        );
    }
    Ok(result)
}

async fn load_committed_recovery_replay_txn(
    txn: &DatabaseTransaction,
    header: &delegation_workflow::Model,
    parent_conversation_id: i32,
    req: &RecoverWorkflowRequest,
) -> Result<Option<RecoverWorkflowResult>, WorkflowStoreError> {
    let revision = delegation_workflow_manifest_revision::Entity::find()
        .filter(
            delegation_workflow_manifest_revision::Column::WorkflowId.eq(req.workflow_id.clone()),
        )
        .filter(
            delegation_workflow_manifest_revision::Column::RecoveryAuthorizationId
                .eq(req.recovery_authorization_id.clone()),
        )
        .one(txn)
        .await
        .map_err(db_err)?;
    let Some(revision) = revision else {
        return if header.workflow_state == WorkflowState::Blocked {
            Ok(None)
        } else {
            Err(WorkflowStoreError::WorkflowRecoveryConflict)
        };
    };
    let source_manifest_revision = revision
        .source_manifest_revision
        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;
    if header.parent_conversation_id != parent_conversation_id
        || source_manifest_revision as u64 != req.expected_manifest_revision
        || revision.consumer_correlation_id.as_deref() != Some(req.correlation_id.as_str())
        || revision.revision_kind.as_deref() != Some(ManifestRevisionKind::StateOnly.as_str())
    {
        return Err(WorkflowStoreError::WorkflowRecoveryConflict);
    }
    let receipt = recovery_authorization::Entity::find_by_id(req.recovery_authorization_id.clone())
        .one(txn)
        .await
        .map_err(db_err)?
        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;
    let source = delegation_workflow_manifest_revision::Entity::find_by_id((
        req.workflow_id.clone(),
        source_manifest_revision,
    ))
    .one(txn)
    .await
    .map_err(db_err)?
    .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;

    let old_state = parse_manifest_state(&source.manifest_state)
        .filter(|state| *state == ManifestWorkflowState::Blocked)
        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;
    let new_state = parse_manifest_state(&revision.manifest_state)
        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;
    let recovery_cause = revision
        .transition_reason_code
        .as_deref()
        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;
    replay_source_block_cause(recovery_cause)
        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;
    if !matches!(
        new_state,
        ManifestWorkflowState::Skeleton
            | ManifestWorkflowState::Estimated
            | ManifestWorkflowState::Approved
    ) {
        return Err(WorkflowStoreError::WorkflowRecoveryConflict);
    }
    let recovery_graph_revision = revision
        .graph_revision
        .filter(|value| *value > 0)
        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;
    let recovery_source_state_fingerprint = revision
        .recovery_source_state_fingerprint
        .as_deref()
        .unwrap_or(receipt.source_state_fingerprint.as_str());
    let recovery_risk_class = revision
        .recovery_risk_class
        .as_deref()
        .unwrap_or(receipt.risk_class.as_str());
    if recovery_source_state_fingerprint.is_empty() || recovery_risk_class.is_empty() {
        return Err(WorkflowStoreError::WorkflowRecoveryConflict);
    }
    let expected_payload = serde_json::json!({ "target_state": new_state });
    let expected_recovery_document = {
        let mut document = serde_json::from_str::<ManifestDocument>(&source.document_json)
            .map_err(|_| WorkflowStoreError::WorkflowRecoveryConflict)?;
        if document.workflow_state != ManifestWorkflowState::Blocked
            || source.document_digest != sha256_hex(source.document_json.as_bytes())
        {
            return Err(WorkflowStoreError::WorkflowRecoveryConflict);
        }
        document.workflow_state = new_state;
        serde_json::to_string(&document)
            .map_err(|_| WorkflowStoreError::WorkflowRecoveryConflict)?
    };
    let receipt_timestamps_valid = receipt
        .approved_at
        .zip(receipt.expires_at)
        .zip(receipt.consumed_at)
        .is_some_and(|((approved_at, expires_at), consumed_at)| {
            receipt.requested_at <= approved_at
                && approved_at <= consumed_at
                && consumed_at < expires_at
        });
    if receipt.status
        != crate::db::entities::recovery_authorization::RecoveryAuthorizationStatus::Consumed
        || receipt.parent_conversation_id != parent_conversation_id
        || receipt.subject_kind != RecoverySubjectKind::Workflow.as_str()
        || receipt.subject_id != req.workflow_id
        || receipt.source_task_id.is_some()
        || receipt.child_conversation_id.is_some()
        || receipt.lineage_root_task_id.is_some()
        || receipt.work_unit_key.is_some()
        || receipt.source_state_fingerprint != recovery_source_state_fingerprint
        || receipt.allowed_action != RecoveryAllowedAction::RecoverWorkflow.as_str()
        || receipt.action_payload_json
            != canonical_json(&expected_payload)
                .map_err(|_| WorkflowStoreError::WorkflowRecoveryConflict)?
        || receipt.cause_code != recovery_cause
        || receipt.risk_class != recovery_risk_class
        || receipt.display_reason.is_some()
        || !receipt_timestamps_valid
        || receipt.consumed_at != Some(revision.created_at)
        || receipt.consumed_by_kind.as_deref()
            != Some(RecoveryConsumerKind::WorkflowManifestRevision.as_str())
        || receipt.consumed_by_id.as_deref()
            != Some(revision.manifest_revision.to_string().as_str())
        || receipt.consumer_correlation_id.as_deref() != Some(req.correlation_id.as_str())
        || revision.document_json != expected_recovery_document
        || revision.document_digest != sha256_hex(revision.document_json.as_bytes())
        || revision.recovery_authorization_id.as_deref()
            != Some(req.recovery_authorization_id.as_str())
    {
        return Err(WorkflowStoreError::WorkflowRecoveryConflict);
    }
    Ok(Some(RecoverWorkflowResult {
        workflow_id: req.workflow_id.clone(),
        old_state,
        new_state,
        source_manifest_revision: source_manifest_revision as u64,
        manifest_revision: revision.manifest_revision as u64,
        graph_revision: recovery_graph_revision as u64,
        cause_code: recovery_cause.to_string(),
        recovery_authorization_id: req.recovery_authorization_id.clone(),
        idempotent_replay: true,
    }))
}

fn replay_source_block_cause(recovery_cause: &str) -> Option<WorkflowBlockCause> {
    match recovery_cause {
        "legacy_block_with_current_plan_approval"
        | "legacy_block_with_current_plan"
        | "legacy_block_without_plan" => Some(WorkflowBlockCause::LegacyUnknown),
        "plan_gate_blocked" => Some(WorkflowBlockCause::PlanGateBlocked),
        "explicit_manifest_block" => Some(WorkflowBlockCause::ExplicitManifestBlock),
        "unresolved_task_cohort" => Some(WorkflowBlockCause::UnresolvedTaskCohort),
        _ => None,
    }
}

fn map_workflow_authorization_error(error: RecoveryAuthorizationError) -> WorkflowStoreError {
    match error {
        RecoveryAuthorizationError::FingerprintMismatch
        | RecoveryAuthorizationError::PayloadMismatch => {
            WorkflowStoreError::RecoveryAuthorizationStale
        }
        RecoveryAuthorizationError::ConsumedConflict => {
            WorkflowStoreError::WorkflowRecoveryConflict
        }
        other => WorkflowStoreError::RecoveryAuthorizationRejected { code: other.code() },
    }
}

fn emit_committed_recovery_events(
    emitter: &EventEmitter,
    result: &RecoverWorkflowResult,
    action: &str,
    plan_lineage_reset: bool,
) {
    let events = [
        WorkflowRecoveryEvent::RecoveryDecision {
            workflow_id: result.workflow_id.clone(),
            source_manifest_revision: result.source_manifest_revision,
            graph_revision: result.graph_revision,
            action: action.to_string(),
            target_state: Some(result.new_state),
            cause_code: result.cause_code.clone(),
        },
        WorkflowRecoveryEvent::RecoveryConfirmationRequested {
            workflow_id: result.workflow_id.clone(),
            recovery_authorization_id: result.recovery_authorization_id.clone(),
            source_manifest_revision: result.source_manifest_revision,
            graph_revision: result.graph_revision,
            action: action.to_string(),
            target_state: Some(result.new_state),
            cause_code: result.cause_code.clone(),
        },
        WorkflowRecoveryEvent::RecoveryAuthorizationConsumed {
            workflow_id: result.workflow_id.clone(),
            recovery_authorization_id: result.recovery_authorization_id.clone(),
            manifest_revision: result.manifest_revision,
            graph_revision: result.graph_revision,
            action: action.to_string(),
        },
        WorkflowRecoveryEvent::StateOnlyRevisionCreated {
            workflow_id: result.workflow_id.clone(),
            source_manifest_revision: result.source_manifest_revision,
            manifest_revision: result.manifest_revision,
            graph_revision: result.graph_revision,
            target_state: result.new_state,
            cause_code: result.cause_code.clone(),
        },
        if plan_lineage_reset {
            WorkflowRecoveryEvent::PlanLineageReset {
                workflow_id: result.workflow_id.clone(),
                recovery_authorization_id: result.recovery_authorization_id.clone(),
                source_manifest_revision: result.source_manifest_revision,
                manifest_revision: result.manifest_revision,
                graph_revision: result.graph_revision,
                action: action.to_string(),
                target_state: result.new_state,
                cause_code: result.cause_code.clone(),
            }
        } else {
            WorkflowRecoveryEvent::BindingReactivated {
                workflow_id: result.workflow_id.clone(),
                manifest_revision: result.manifest_revision,
                graph_revision: result.graph_revision,
                target_state: result.new_state,
            }
        },
    ];
    for event in events {
        if let Err(error) = emit_workflow_recovery_event(emitter, event) {
            tracing::warn!(
                error,
                workflow_id = %result.workflow_id,
                manifest_revision = result.manifest_revision,
                "workflow recovery committed but event emission failed"
            );
        }
    }
    if plan_lineage_reset && result.new_state != ManifestWorkflowState::Blocked {
        let event = WorkflowRecoveryEvent::BindingReactivated {
            workflow_id: result.workflow_id.clone(),
            manifest_revision: result.manifest_revision,
            graph_revision: result.graph_revision,
            target_state: result.new_state,
        };
        if let Err(error) = emit_workflow_recovery_event(emitter, event) {
            tracing::warn!(
                error,
                workflow_id = %result.workflow_id,
                manifest_revision = result.manifest_revision,
                "workflow recovery committed but event emission failed"
            );
        }
    }
}

fn designated_recovery_rejection_code(error: &WorkflowStoreError) -> Option<&str> {
    match error {
        WorkflowStoreError::WorkflowRecoveryConflict => Some("workflow_recovery_conflict"),
        WorkflowStoreError::RecoveryAuthorizationStale => Some("recovery_authorization_stale"),
        WorkflowStoreError::RecoveryAuthorizationRejected { code } => Some(code),
        _ => None,
    }
}

fn parse_workflow_recovery_cause_code(value: &str) -> Option<WorkflowRecoveryCauseCode> {
    match value {
        "legacy_block_with_current_plan_approval" => {
            Some(WorkflowRecoveryCauseCode::LegacyBlockWithCurrentPlanApproval)
        }
        "legacy_block_with_current_plan" => {
            Some(WorkflowRecoveryCauseCode::LegacyBlockWithCurrentPlan)
        }
        "legacy_block_without_plan" => Some(WorkflowRecoveryCauseCode::LegacyBlockWithoutPlan),
        "plan_user_decision_required" => Some(WorkflowRecoveryCauseCode::PlanUserDecisionRequired),
        "plan_gate_blocked" => Some(WorkflowRecoveryCauseCode::PlanGateBlocked),
        "explicit_manifest_block" => Some(WorkflowRecoveryCauseCode::ExplicitManifestBlock),
        "unresolved_task_cohort" => Some(WorkflowRecoveryCauseCode::UnresolvedTaskCohort),
        "durable_state_inconsistent" => Some(WorkflowRecoveryCauseCode::DurableStateInconsistent),
        _ => None,
    }
}

async fn emit_recover_workflow_rejection_if_designated(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: &RecoverWorkflowRequest,
    error: &WorkflowStoreError,
) {
    let Some(rejection_code) = designated_recovery_rejection_code(error) else {
        return;
    };
    let workflow_id = req.workflow_id.clone();
    let recovery_authorization_id = req.recovery_authorization_id.clone();
    let expected_manifest_revision = req.expected_manifest_revision;
    let context = db
        .conn
        .transaction::<_, (u64, u64, String), WorkflowStoreError>(|txn| {
            let workflow_id = workflow_id.clone();
            let recovery_authorization_id = recovery_authorization_id.clone();
            Box::pin(async move {
                let header = delegation_workflow::Entity::find_by_id(workflow_id.clone())
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or_else(|| WorkflowStoreError::NotFound(workflow_id.clone()))?;
                if header.parent_conversation_id != parent_conversation_id {
                    return Err(WorkflowStoreError::WorkflowRecoveryConflict);
                }

                if header.workflow_state == WorkflowState::Blocked
                    && header.active_manifest_revision as u64 == expected_manifest_revision
                {
                    let decision = decide_workflow_recovery(
                        &load_workflow_recovery_snapshot_txn(txn, &header, None).await?,
                    );
                    if !matches!(
                        decision.disposition,
                        WorkflowRecoveryDisposition::Recover { .. }
                    ) {
                        return Err(WorkflowStoreError::WorkflowRecoveryNotAvailable);
                    }
                    return Ok((
                        header.active_manifest_revision as u64,
                        header.graph_revision as u64,
                        decision.cause_code.as_str().to_string(),
                    ));
                }

                let revision = delegation_workflow_manifest_revision::Entity::find()
                    .filter(
                        delegation_workflow_manifest_revision::Column::WorkflowId.eq(workflow_id),
                    )
                    .filter(
                        delegation_workflow_manifest_revision::Column::RecoveryAuthorizationId
                            .eq(recovery_authorization_id),
                    )
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?;
                Ok((
                    revision
                        .source_manifest_revision
                        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?
                        as u64,
                    header.graph_revision as u64,
                    revision
                        .transition_reason_code
                        .ok_or(WorkflowStoreError::WorkflowRecoveryConflict)?,
                ))
            })
        })
        .await;
    let Ok((source_manifest_revision, graph_revision, cause_code)) = context else {
        return;
    };
    let Some(cause_code) = parse_workflow_recovery_cause_code(&cause_code) else {
        return;
    };
    let event = WorkflowRecoveryEvent::RecoveryRejected {
        workflow_id: req.workflow_id.clone(),
        recovery_authorization_id: Some(req.recovery_authorization_id.clone()),
        source_manifest_revision,
        graph_revision,
        action: RecoveryAllowedAction::RecoverWorkflow.as_str().to_string(),
        cause_code: cause_code.as_str().to_string(),
        rejection_code: rejection_code.to_string(),
    };
    if let Err(emit_error) = emit_workflow_recovery_event(emitter, event) {
        tracing::warn!(
            error = emit_error,
            workflow_id = %req.workflow_id,
            rejection_code,
            "workflow recovery rejection event emission failed"
        );
    }
}

async fn emit_plan_lineage_reset_rejection_if_designated(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    workflow_id: &str,
    recovery_authorization_id: &str,
    displayed_reset_reason: &str,
    error: &WorkflowStoreError,
) {
    let Some(rejection_code) = designated_recovery_rejection_code(error) else {
        return;
    };
    let workflow_id_owned = workflow_id.to_string();
    let recovery_authorization_id_owned = recovery_authorization_id.to_string();
    let displayed_reset_reason = displayed_reset_reason.to_string();
    let context = db
        .conn
        .transaction::<_, (u64, u64, String), WorkflowStoreError>(|txn| {
            let workflow_id = workflow_id_owned.clone();
            let recovery_authorization_id = recovery_authorization_id_owned.clone();
            let displayed_reset_reason = displayed_reset_reason.clone();
            Box::pin(async move {
                let header = delegation_workflow::Entity::find_by_id(workflow_id.clone())
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or(WorkflowStoreError::NotFound(workflow_id.clone()))?;
                if header.parent_conversation_id != parent_conversation_id {
                    return Err(WorkflowStoreError::WorkflowRecoveryConflict);
                }
                let receipt = recovery_authorization::Entity::find_by_id(recovery_authorization_id)
                    .one(txn)
                    .await
                    .map_err(db_err)?;
                let cause_code = if let Some(receipt) = receipt.filter(|receipt| {
                    receipt.parent_conversation_id == parent_conversation_id
                        && receipt.subject_kind == RecoverySubjectKind::Workflow.as_str()
                        && receipt.subject_id == header.workflow_id
                }) {
                    receipt.cause_code
                } else {
                    decide_workflow_recovery(
                        &load_workflow_recovery_snapshot_txn(
                            txn,
                            &header,
                            Some(&displayed_reset_reason),
                        )
                        .await?,
                    )
                    .cause_code
                    .as_str()
                    .to_string()
                };
                Ok((
                    header.active_manifest_revision as u64,
                    header.graph_revision as u64,
                    cause_code,
                ))
            })
        })
        .await;
    let Ok((source_manifest_revision, graph_revision, cause_code)) = context else {
        return;
    };
    let Some(cause_code) = parse_workflow_recovery_cause_code(&cause_code) else {
        return;
    };
    let event = WorkflowRecoveryEvent::RecoveryRejected {
        workflow_id: workflow_id.to_string(),
        recovery_authorization_id: Some(recovery_authorization_id.to_string()),
        source_manifest_revision,
        graph_revision,
        action: RecoveryAllowedAction::ResetPlanLineage.as_str().to_string(),
        cause_code: cause_code.as_str().to_string(),
        rejection_code: rejection_code.to_string(),
    };
    if let Err(emit_error) = emit_workflow_recovery_event(emitter, event) {
        tracing::warn!(
            error = emit_error,
            workflow_id,
            rejection_code,
            "workflow Plan lineage reset rejection event emission failed"
        );
    }
}

#[cfg(test)]
pub(super) async fn settle_workflow_gate_v2_from_fixture(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: SettleWorkflowRequest,
) -> Result<SettleResult, WorkflowStoreError> {
    super::error::with_historical_workflow_fixture_mutations(
        settle_workflow_gate_v2_from_fixture_inner(db, emitter, parent_conversation_id, req),
    )
    .await
}

#[cfg(test)]
async fn settle_workflow_gate_v2_from_fixture_inner(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: SettleWorkflowRequest,
) -> Result<SettleResult, WorkflowStoreError> {
    let expected_review_round = if matches!(&req.evidence, SettleGateEvidence::Plan(_)) {
        delegation_workflow_gate_state::Entity::find_by_id((
            req.workflow_id.clone(),
            req.gate_id.clone(),
        ))
        .one(&db.conn)
        .await
        .map_err(db_err)?
        .map(|state| state.current_review_round as u64)
    } else {
        None
    };
    let current = delegation_workflow::Entity::find_by_id(req.workflow_id.clone())
        .one(&db.conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(req.workflow_id.clone()))?;
    settle_workflow_gate_v2_core(
        db,
        emitter,
        parent_conversation_id,
        SettleWorkflowV2Request {
            workflow_id: req.workflow_id,
            gate_id: req.gate_id,
            expected_graph_revision: current.graph_revision as u64,
            expected_review_round,
            expected_outcome: Some(req.outcome),
            summary: req.summary,
            recovery_authorization_id: req.recovery_authorization_id,
        },
    )
    .await
}

#[derive(Debug, Clone)]
struct V2SettlementExpectation {
    review_round: Option<u64>,
    outcome: Option<GateSettlementOutcome>,
}

#[cfg(test)]
struct SettleV2PreflightTestGate {
    workflow_id: String,
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}

#[cfg(test)]
static SETTLE_V2_PREFLIGHT_TEST_GATE: std::sync::Mutex<Option<SettleV2PreflightTestGate>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
static DESIGN_PREFLIGHT_HEADER_TEST_GATE: std::sync::Mutex<Option<SettleV2PreflightTestGate>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
fn install_settle_v2_preflight_test_gate(
    workflow_id: String,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let mut slot = SETTLE_V2_PREFLIGHT_TEST_GATE
        .lock()
        .expect("settle v2 preflight test gate lock");
    assert!(
        slot.is_none(),
        "settle v2 preflight test gate already installed"
    );
    *slot = Some(SettleV2PreflightTestGate {
        workflow_id,
        entered: entered_tx,
        release: release_rx,
    });
    (entered_rx, release_tx)
}

#[cfg(test)]
async fn honor_settle_v2_preflight_test_gate(workflow_id: &str) {
    let gate = {
        let mut slot = SETTLE_V2_PREFLIGHT_TEST_GATE
            .lock()
            .expect("settle v2 preflight test gate lock");
        if slot
            .as_ref()
            .is_some_and(|gate| gate.workflow_id == workflow_id)
        {
            slot.take()
        } else {
            None
        }
    };
    if let Some(gate) = gate {
        let _ = gate.entered.send(());
        let _ = gate.release.await;
    }
}

#[cfg(test)]
fn install_design_preflight_header_test_gate(
    workflow_id: String,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let mut slot = DESIGN_PREFLIGHT_HEADER_TEST_GATE
        .lock()
        .expect("Design preflight header test gate lock");
    assert!(
        slot.is_none(),
        "Design preflight header test gate already installed"
    );
    *slot = Some(SettleV2PreflightTestGate {
        workflow_id,
        entered: entered_tx,
        release: release_rx,
    });
    (entered_rx, release_tx)
}

#[cfg(test)]
async fn honor_design_preflight_header_test_gate(workflow_id: &str) {
    let gate = {
        let mut slot = DESIGN_PREFLIGHT_HEADER_TEST_GATE
            .lock()
            .expect("Design preflight header test gate lock");
        if slot
            .as_ref()
            .is_some_and(|gate| gate.workflow_id == workflow_id)
        {
            slot.take()
        } else {
            None
        }
    };
    if let Some(gate) = gate {
        let _ = gate.entered.send(());
        let _ = gate.release.await;
    }
}

pub async fn settle_workflow_gate_v2_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: SettleWorkflowV2Request,
) -> Result<SettleResult, WorkflowStoreError> {
    require_owned_stored_v2_header(&db.conn, &req.workflow_id, parent_conversation_id).await?;
    let guard_header = delegation_workflow::Entity::find_by_id(req.workflow_id.clone())
        .one(&db.conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(req.workflow_id.clone()))?;
    if guard_header.parent_conversation_id != parent_conversation_id {
        return Err(WorkflowStoreError::CrossParent {
            workflow_id: guard_header.workflow_id,
            expected_parent: parent_conversation_id,
            actual_parent: guard_header.parent_conversation_id,
        });
    }
    require_v2_mutation_for_connection(
        &db.conn,
        guard_header.completion_protocol_version,
        &guard_header.completion_protocol_mode,
    )
    .await?;
    #[cfg(test)]
    honor_settle_v2_preflight_test_gate(&req.workflow_id).await;
    require_owned_stored_v2_header(&db.conn, &req.workflow_id, parent_conversation_id).await?;
    if req.summary.len() > MAX_ADJUDICATION_SUMMARY_BYTES {
        return Err(WorkflowStoreError::SummaryTooLarge);
    }
    let document = load_active_manifest_document_txn(
        &db.conn,
        &guard_header.workflow_id,
        guard_header.active_manifest_revision,
    )
    .await?;
    let normalized = validate_manifest_document(&document)?;
    let gate = normalized
        .gates
        .iter()
        .find(|gate| gate.id == req.gate_id)
        .ok_or_else(|| {
            WorkflowStoreError::ExecutionGateSettleRejected(format!(
                "gate_id {} is not a document gate on the active manifest",
                req.gate_id
            ))
        })?;
    match gate.gate_kind {
        DocumentGateKind::Design if req.expected_outcome.is_none() => {
            return Err(WorkflowStoreError::GateNotReady(
                "Design settlement requires expected_outcome".into(),
            ))
        }
        DocumentGateKind::Plan if req.expected_review_round.is_none() => {
            return Err(WorkflowStoreError::GateNotReady(
                "Plan settlement requires expected_review_round".into(),
            ))
        }
        DocumentGateKind::Design | DocumentGateKind::Plan => {}
    }
    let preflight = SettleWorkflowRequest {
        workflow_id: req.workflow_id.clone(),
        manifest_revision: 0,
        gate_id: req.gate_id.clone(),
        expected_graph_revision: req.expected_graph_revision,
        gate_cycle: req.expected_review_round.unwrap_or(1),
        outcome: req
            .expected_outcome
            .clone()
            .unwrap_or(GateSettlementOutcome::Approved),
        evidence: SettleGateEvidence::Design {
            critical_count: 0,
            important_count: 0,
            minor_count: 0,
        },
        summary: req.summary.clone(),
        recovery_authorization_id: req.recovery_authorization_id.clone(),
    };
    match prepare_v2_design_self_review(db, parent_conversation_id, &preflight).await? {
        DesignSelfReviewReadiness::NotApplicable | DesignSelfReviewReadiness::Ready => {}
        DesignSelfReviewReadiness::DecisionRequired => {
            return Err(WorkflowStoreError::CompletionDecisionRequired)
        }
        DesignSelfReviewReadiness::Superseded => {
            return Err(WorkflowStoreError::CompletionDecisionSuperseded)
        }
    }

    require_owned_stored_v2_header(&db.conn, &req.workflow_id, parent_conversation_id).await?;
    let header = delegation_workflow::Entity::find_by_id(req.workflow_id.clone())
        .one(&db.conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(req.workflow_id.clone()))?;
    if header.parent_conversation_id != parent_conversation_id {
        return Err(WorkflowStoreError::CrossParent {
            workflow_id: header.workflow_id,
            expected_parent: parent_conversation_id,
            actual_parent: header.parent_conversation_id,
        });
    }
    require_v2_mutation_for_connection(
        &db.conn,
        header.completion_protocol_version,
        &header.completion_protocol_mode,
    )
    .await?;
    if req.expected_graph_revision != header.graph_revision as u64 {
        return Err(WorkflowStoreError::StaleGraphRevision {
            expected: req.expected_graph_revision,
            current: header.graph_revision as u64,
        });
    }
    let evidence_payload = match gate.gate_kind {
        DocumentGateKind::Design => SettleGateEvidence::Design {
            critical_count: 0,
            important_count: 0,
            minor_count: 0,
        },
        DocumentGateKind::Plan => SettleGateEvidence::Plan(PlanReviewRoundSubmission {
            scope: PlanReviewScope::Full,
            revision_kind: PlanRevisionKind::Initial,
            scope_reason: String::new(),
            covered_author_task_id: String::new(),
            covered_plan_digest: String::new(),
            required_reviewer_node_ids: Vec::new(),
            finding_updates: Vec::new(),
            lineage_reset_reason: None,
        }),
    };
    settle_workflow_gate_derived_core(
        db,
        emitter,
        parent_conversation_id,
        SettleWorkflowRequest {
            workflow_id: header.workflow_id,
            manifest_revision: header.active_manifest_revision as u64,
            gate_id: gate.id.clone(),
            expected_graph_revision: req.expected_graph_revision,
            gate_cycle: 1,
            outcome: req
                .expected_outcome
                .clone()
                .unwrap_or(GateSettlementOutcome::Approved),
            evidence: evidence_payload,
            summary: req.summary,
            recovery_authorization_id: req.recovery_authorization_id,
        },
        Some(V2SettlementExpectation {
            review_round: req.expected_review_round,
            outcome: req.expected_outcome,
        }),
    )
    .await
}

async fn settle_workflow_gate_derived_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: SettleWorkflowRequest,
    v2_expectation: Option<V2SettlementExpectation>,
) -> Result<SettleResult, WorkflowStoreError> {
    if req.summary.len() > MAX_ADJUDICATION_SUMMARY_BYTES {
        return Err(WorkflowStoreError::SummaryTooLarge);
    }
    let legacy_design_error = match &req.evidence {
        SettleGateEvidence::Design {
            critical_count,
            important_count,
            minor_count,
        } if *critical_count < 0 || *important_count < 0 || *minor_count < 0 => {
            Some(WorkflowStoreError::NegativeFindingCounts {
                critical: *critical_count,
                important: *important_count,
                minor: *minor_count,
            })
        }
        SettleGateEvidence::Design {
            critical_count,
            important_count,
            ..
        } if req.outcome == GateSettlementOutcome::Approved
            && (*critical_count > 0 || *important_count > 0) =>
        {
            Some(WorkflowStoreError::ApprovalWithOpenFindings {
                critical: *critical_count,
                important: *important_count,
            })
        }
        _ => None,
    };
    let has_legacy_lineage_reset_material = matches!(
        &req.evidence,
        SettleGateEvidence::Plan(submission) if submission.lineage_reset_reason.is_some()
    ) || req.recovery_authorization_id.is_some();
    let request_protocol_is_v2 =
        if legacy_design_error.is_some() || has_legacy_lineage_reset_material {
            delegation_workflow::Entity::find_by_id(req.workflow_id.clone())
                .one(&db.conn)
                .await
                .map_err(db_err)?
                .is_some_and(|workflow| workflow.completion_protocol_version == 2)
        } else {
            false
        };
    if let Some(error) = legacy_design_error {
        if !request_protocol_is_v2 {
            return Err(error);
        }
    }
    let lineage_reset_reason = if request_protocol_is_v2 {
        None
    } else {
        match &req.evidence {
            SettleGateEvidence::Plan(submission) => submission.lineage_reset_reason.clone(),
            SettleGateEvidence::Design { .. } => None,
        }
    };
    let rejection_workflow_id = req.workflow_id.clone();
    let rejection_authorization_id = req.recovery_authorization_id.clone();
    let rejection_reset_reason = lineage_reset_reason.clone();
    match (
        lineage_reset_reason.as_deref(),
        req.recovery_authorization_id.as_deref(),
    ) {
        (Some(_), None) => {
            return Err(WorkflowStoreError::RecoveryAuthorizationRequired {
                action: "reset_plan_lineage",
            });
        }
        (None, Some(_)) => {
            return Err(WorkflowStoreError::RecoveryAuthorizationRejected {
                code: "recovery_authorization_action_mismatch",
            });
        }
        _ => {}
    }
    if req.gate_cycle == 0 {
        return Err(WorkflowStoreError::GateCycleConflict(
            "gate_cycle must be 1-based".into(),
        ));
    }

    let result = db
        .conn
        .transaction::<_, (SettleResult, Option<RecoverWorkflowResult>), WorkflowStoreError>(
            |txn| {
                Box::pin(async move {
                    let mut req = req;
                    if v2_expectation.is_some() {
                        require_owned_stored_v2_header(
                            txn,
                            &req.workflow_id,
                            parent_conversation_id,
                        )
                        .await?;
                    }
                    let header = delegation_workflow::Entity::find_by_id(req.workflow_id.clone())
                        .one(txn)
                        .await
                        .map_err(db_err)?
                        .ok_or_else(|| WorkflowStoreError::NotFound(req.workflow_id.clone()))?;

                    if header.parent_conversation_id != parent_conversation_id {
                        return Err(WorkflowStoreError::CrossParent {
                            workflow_id: header.workflow_id.clone(),
                            expected_parent: parent_conversation_id,
                            actual_parent: header.parent_conversation_id,
                        });
                    }

                    if v2_expectation.is_some() {
                        require_v2_mutation_for_connection(
                            txn,
                            header.completion_protocol_version,
                            &header.completion_protocol_mode,
                        )
                        .await?;
                    }

                    // v1 preserves cycle-addressed replay before graph CAS. v2
                    // replays only after current evidence is revalidated below.
                    let gate_prior = delegation_workflow_gate_settlement::Entity::find()
                        .filter(
                            delegation_workflow_gate_settlement::Column::WorkflowId
                                .eq(header.workflow_id.clone()),
                        )
                        .filter(
                            delegation_workflow_gate_settlement::Column::GateId
                                .eq(req.gate_id.clone()),
                        )
                        .order_by_asc(delegation_workflow_gate_settlement::Column::GateCycle)
                        .all(txn)
                        .await
                        .map_err(db_err)?;

                    if v2_expectation.is_none() {
                        if let Some(existing) = gate_prior
                            .iter()
                            .find(|s| s.gate_cycle as u64 == req.gate_cycle)
                        {
                            if settlement_payload_matches(existing, &req)? {
                                return Ok((
                                    settle_result_from_row(
                                        existing,
                                        header.graph_revision as u64,
                                        header.active_manifest_revision as u64,
                                        true,
                                    )?,
                                    None,
                                ));
                            }
                            return Err(WorkflowStoreError::GateCycleConflict(format!(
                                "gate {} cycle {} already settled with a different payload",
                                req.gate_id, req.gate_cycle
                            )));
                        }
                    }

                    if req.manifest_revision != header.active_manifest_revision as u64 {
                        return Err(WorkflowStoreError::StaleManifestRevision {
                            expected: req.manifest_revision,
                            current: header.active_manifest_revision as u64,
                        });
                    }
                    if req.expected_graph_revision != header.graph_revision as u64 {
                        return Err(WorkflowStoreError::StaleGraphRevision {
                            expected: req.expected_graph_revision,
                            current: header.graph_revision as u64,
                        });
                    }

                    let doc = load_active_manifest_document_txn(
                        txn,
                        &header.workflow_id,
                        header.active_manifest_revision,
                    )
                    .await?;
                    let normalized = validate_manifest_document(&doc)?;
                    let gate = normalized
                        .gates
                        .iter()
                        .find(|g| g.id == req.gate_id)
                        .ok_or_else(|| {
                            WorkflowStoreError::ExecutionGateSettleRejected(format!(
                                "gate_id {} is not a document gate on the active manifest",
                                req.gate_id
                            ))
                        })?;

                    let v2_gate_evidence = if header.completion_protocol_version == 2 {
                        validate_current_design_self_review_bytes_txn(
                            txn,
                            &header,
                            &normalized,
                            gate,
                            parent_conversation_id,
                        )
                        .await?;
                        let evidence = load_validated_v2_gate_evidence(
                            txn,
                            &header.workflow_id,
                            gate,
                        )
                        .await?;
                        let expectation = v2_expectation
                            .as_ref()
                            .ok_or(WorkflowStoreError::V2CallerEvidenceRejected)?;
                        if expectation.review_round.is_some_and(|expected| {
                            expected != evidence.identity.review_round as u64
                        }) {
                            return Err(WorkflowStoreError::GateCycleConflict(format!(
                                "gate {} expected review round {:?}, current is {}",
                                gate.id,
                                expectation.review_round,
                                evidence.identity.review_round
                            )));
                        }
                        if let Some(existing) = gate_prior
                            .iter()
                            .rev()
                            .find(|settlement| evidence.identity.matches_settlement(settlement))
                        {
                            let reduced_outcome = if gate.gate_kind == DocumentGateKind::Plan {
                                let state = load_persisted_plan_state_v2(existing)?;
                                if !plan_state_matches_evidence(&state, &evidence) {
                                    return Err(WorkflowStoreError::Persistence(
                                        "persisted v2 Plan state does not match current evidence"
                                            .into(),
                                    ));
                                }
                                plan_v2_settlement_outcome(state.next_action)
                            } else {
                                evidence.outcome.clone()
                            };
                            if expectation
                                .outcome
                                .as_ref()
                                .is_some_and(|expected| *expected != reduced_outcome)
                            {
                                return Err(WorkflowStoreError::GateNotReady(format!(
                                    "expected outcome {:?} disagrees with complete v2 evidence reduction {:?}",
                                    expectation.outcome, reduced_outcome
                                )));
                            }
                            if existing.outcome == reduced_outcome
                                && existing.summary == req.summary
                                && existing.manifest_revision == header.active_manifest_revision
                                && existing.lineage_reset_authorization_id.as_deref()
                                    == req.recovery_authorization_id.as_deref()
                            {
                                return Ok((
                                    settle_result_from_row(
                                        existing,
                                        header.graph_revision as u64,
                                        header.active_manifest_revision as u64,
                                        true,
                                    )?,
                                    None,
                                ));
                            }
                            return Err(WorkflowStoreError::GateCycleConflict(format!(
                                "gate {} current v2 evidence is already settled with a different payload",
                                gate.id
                            )));
                        }
                        if gate.gate_kind == DocumentGateKind::Design {
                            if expectation
                                .outcome
                                .as_ref()
                                .is_some_and(|expected| *expected != evidence.outcome)
                            {
                                return Err(WorkflowStoreError::GateNotReady(format!(
                                    "expected outcome {:?} disagrees with complete v2 evidence reduction {:?}",
                                    expectation.outcome, evidence.outcome
                                )));
                            }
                            req.outcome = evidence.outcome.clone();
                        }
                        Some(evidence)
                    } else {
                        None
                    };

                    // Plan review is one workflow-wide lineage. Gate IDs are mutable
                    // manifest labels and must not reset cycles, findings, or stagnation.
                    let plan_prior = delegation_workflow_gate_settlement::Entity::find()
                        .filter(
                            delegation_workflow_gate_settlement::Column::WorkflowId
                                .eq(header.workflow_id.clone()),
                        )
                        .filter(
                            Condition::any()
                                .add(
                                    delegation_workflow_gate_settlement::Column::ReviewScope
                                        .is_not_null(),
                                )
                                .add(
                                    delegation_workflow_gate_settlement::Column::PlanRoundStateV2Json
                                        .is_not_null(),
                                ),
                        )
                        .order_by_asc(delegation_workflow_gate_settlement::Column::GateCycle)
                        .order_by_asc(delegation_workflow_gate_settlement::Column::CreatedAt)
                        .all(txn)
                        .await
                        .map_err(db_err)?;
                    let lineage_prior = if gate.gate_kind == DocumentGateKind::Plan {
                        &plan_prior
                    } else {
                        &gate_prior
                    };

                    let max_cycle = lineage_prior
                        .iter()
                        .map(|s| s.gate_cycle)
                        .max()
                        .unwrap_or(0);
                    let expected_next = (max_cycle + 1) as u64;
                    if v2_expectation.is_some() {
                        req.gate_cycle = expected_next;
                    } else if req.gate_cycle != expected_next {
                        return Err(WorkflowStoreError::GateCycleConflict(format!(
                            "gate {} expected cycle {expected_next}, got {}",
                            req.gate_id, req.gate_cycle
                        )));
                    }

                    let lineage_reset_authorization = if let Some(reason) =
                        lineage_reset_reason.as_deref()
                    {
                        let authorization_id = req
                            .recovery_authorization_id
                            .as_deref()
                            .expect("lineage reset preflight requires authorization id");
                        let authorization = recovery_authorization::Entity::find_by_id(
                            authorization_id.to_string(),
                        )
                        .one(txn)
                        .await
                        .map_err(db_err)?
                        .ok_or(
                            WorkflowStoreError::RecoveryAuthorizationRejected {
                                code: "recovery_authorization_not_found",
                            },
                        )?;
                        if authorization.allowed_action
                            != RecoveryAllowedAction::ResetPlanLineage.as_str()
                        {
                            return Err(WorkflowStoreError::RecoveryAuthorizationRejected {
                                code: "recovery_authorization_action_mismatch",
                            });
                        }
                        let decision = decide_workflow_recovery(
                            &load_workflow_recovery_snapshot_txn(txn, &header, Some(reason))
                                .await?,
                        );
                        if decision.disposition != WorkflowRecoveryDisposition::ResetPlanLineage {
                            return Err(WorkflowStoreError::RecoveryAuthorizationStale);
                        }
                        let action_payload = decision
                            .action_payload()
                            .expect("lineage reset decision has action payload");
                        let consumer_id = (header.active_manifest_revision + 1).to_string();
                        let consumer_correlation_id = format!(
                            "plan_lineage_reset:{}:{}:{}",
                            header.workflow_id, req.gate_id, req.gate_cycle
                        );
                        let expectation = AuthorizationConsumeExpectation {
                            parent_conversation_id,
                            subject_kind: RecoverySubjectKind::Workflow,
                            subject_id: &header.workflow_id,
                            source_state_fingerprint: &decision.source_state_fingerprint,
                            allowed_action: RecoveryAllowedAction::ResetPlanLineage,
                            action_payload: &action_payload,
                            consumer_kind: RecoveryConsumerKind::WorkflowManifestRevision,
                            consumer_id: &consumer_id,
                            consumer_correlation_id: &consumer_correlation_id,
                        };
                        let authorization = validate_for_consumption_txn(
                            txn,
                            authorization_id,
                            &expectation,
                            Utc::now(),
                        )
                        .await
                        .map_err(map_workflow_authorization_error)?;
                        Some(ApprovedPlanLineageReset {
                            authorization,
                            source_state_fingerprint: decision.source_state_fingerprint,
                            action_payload,
                            consumer_id,
                            consumer_correlation_id,
                            cause_code: decision.cause_code.as_str().to_string(),
                        })
                    } else {
                        None
                    };

                    // A2 freshness: required runs for this cycle against active
                    // document revision + design/plan digest + content fingerprint.
                    let current_doc_digest = document_digest_for_gate(gate, &normalized)?;
                    let content_fp = gate_content_fingerprint(gate.gate_kind, &header);
                    let mut v2_plan_decision: Option<PlanReviewDecisionV2> = None;
                    let mut v2_plan_author_task_id: Option<String> = None;
                    let mut v2_plan_digest: Option<String> = None;
                    let mut v2_localized_change_digest: Option<String> = None;
                    let mut plan_metric_observation: Option<PlanSettlementMetricObservation> = None;

                    let (
                        critical_count,
                        important_count,
                        minor_count,
                        plan_next_action,
                        stagnation_count,
                        rewrite_used,
                        persisted_plan,
                        report_files_json,
                    ) = match (&req.evidence, gate.gate_kind) {
                        (
                            SettleGateEvidence::Design {
                                critical_count,
                                important_count,
                                minor_count,
                            },
                            DocumentGateKind::Design,
                        ) => {
                            if header.completion_protocol_version != 2 {
                                verify_document_gate_ready(
                                    txn,
                                    &header.workflow_id,
                                    gate,
                                    req.gate_cycle as i64,
                                    header.active_manifest_revision,
                                    current_doc_digest.as_deref(),
                                    content_fp.as_str(),
                                    &req.outcome,
                                    lineage_prior.last(),
                                )
                                .await?;
                            }
                            (
                                if header.completion_protocol_version == 2 {
                                    0
                                } else {
                                    *critical_count
                                },
                                if header.completion_protocol_version == 2 {
                                    0
                                } else {
                                    *important_count
                                },
                                if header.completion_protocol_version == 2 {
                                    0
                                } else {
                                    *minor_count
                                },
                                None,
                                0,
                                false,
                                None,
                                None,
                            )
                        }
                        (SettleGateEvidence::Plan(submission), DocumentGateKind::Plan) => {
                            let active_required =
                                canonical_string_set(&gate.required_reviewer_node_ids);
                            let submitted_required =
                                canonical_string_set(&submission.required_reviewer_node_ids);
                            if header.completion_protocol_version != 2
                                && active_required != submitted_required
                            {
                                return Err(PlanReviewError::RequiredReviewerSetMismatch {
                                    expected: active_required,
                                    actual: submitted_required,
                                }
                                .into());
                            }
                            if header.completion_protocol_version != 2
                                && current_doc_digest.as_deref()
                                    != Some(submission.covered_plan_digest.as_str())
                            {
                                return Err(WorkflowStoreError::ArtifactDigestMismatch(
                                    "Plan submission digest does not match the active Plan artifact"
                                        .into(),
                                ));
                            }

                            let prior_state = if header.completion_protocol_version == 2 {
                                None
                            } else {
                                let prior_plan_row = lineage_prior
                                    .iter()
                                    .rev()
                                    .find(|row| row.review_scope.is_some());
                                let prior_state = prior_plan_row
                                    .map(load_persisted_plan_evidence)
                                    .transpose()?
                                    .map(|evidence| evidence.state);
                                let mut current_fingerprint_approved = false;
                                for row in lineage_prior
                                    .iter()
                                    .filter(|row| row.content_fingerprint == content_fp)
                                {
                                    let evidence = load_persisted_plan_evidence(row)?;
                                    if evidence.state.next_action
                                        == PlanReviewNextAction::Approved
                                    {
                                        current_fingerprint_approved = true;
                                        break;
                                    }
                                }
                                if current_fingerprint_approved {
                                    return Err(PlanReviewError::InvalidTransition(
                                        "an approved Plan review lineage cannot be re-entered"
                                            .into(),
                                    )
                                    .into());
                                }
                                prior_state
                            };

                            let active_author_node_id = normalized
                                .nodes
                                .iter()
                                .find(|node| {
                                    node.kind == ManifestNodeKind::WorkUnit
                                        && node.phase_id.as_deref()
                                            == Some(super::types::PHASE_PLAN)
                                        && node.role == Some(ManifestNodeRole::Author)
                                })
                                .map(|node| node.id.as_str())
                                .ok_or_else(|| {
                                    WorkflowStoreError::GateNotReady(
                                        "active manifest has no Plan Author node".into(),
                                    )
                                })?;

                            if header.completion_protocol_version == 2 {
                                let plan_digest = current_doc_digest.as_deref().ok_or_else(|| {
                                    WorkflowStoreError::GateNotReady(
                                        "active Plan artifact is missing".into(),
                                    )
                                })?;
                                let (author_task_id, covered_plan_digest, author_workspace) =
                                    load_validated_v2_plan_author(
                                    txn,
                                    &header.workflow_id,
                                    active_author_node_id,
                                    plan_digest,
                                )
                                .await?;
                                let evidence = v2_gate_evidence.as_ref().ok_or_else(|| {
                                    WorkflowStoreError::GateNotReady(
                                        "v2 Plan settlement requires complete platform evidence"
                                            .into(),
                                    )
                                })?;
                                let prior_v2_state = lineage_prior
                                    .iter()
                                    .rev()
                                    .find(|row| row.plan_round_state_v2_json.is_some())
                                    .map(load_persisted_plan_state_v2)
                                    .transpose()?;
                                let change = match prior_v2_state.as_ref() {
                                    None => PlanReviewChangeV2::InitialOrNewLineage,
                                    Some(previous)
                                        if previous.gate_lineage
                                            != evidence.identity.gate_lineage =>
                                    {
                                        if previous.next_action
                                            == PlanReviewNextAction::HolisticRewriteRequired
                                        {
                                            PlanReviewChangeV2::HolisticRewrite
                                        } else {
                                            PlanReviewChangeV2::InitialOrNewLineage
                                        }
                                    }
                                    Some(previous)
                                        if previous.required_node_ids
                                            != evidence.identity.required_node_ids =>
                                    {
                                        PlanReviewChangeV2::RosterOnly
                                    }
                                    Some(previous)
                                        if previous.next_action
                                            == PlanReviewNextAction::HolisticRewriteRequired =>
                                    {
                                        PlanReviewChangeV2::HolisticRewrite
                                    }
                                    Some(_) => PlanReviewChangeV2::Corrective,
                                };
                                let plan_document = normalized.plan.as_ref().ok_or_else(|| {
                                    WorkflowStoreError::GateNotReady(
                                        "active Plan document is missing".into(),
                                    )
                                })?;
                                let current_plan_snapshot = capture_plan_snapshot_v2(
                                    &author_workspace,
                                    plan_document,
                                )
                                .await?;
                                let mut authorized_localized_change = None;
                                if change == PlanReviewChangeV2::Corrective {
                                    let previous = prior_v2_state.as_ref().ok_or_else(|| {
                                        WorkflowStoreError::GateNotReady(
                                            "corrective Plan review has no prior round".into(),
                                        )
                                    })?;
                                    let authorization = load_plan_round_authorization_v2(
                                        txn,
                                        &header.workflow_id,
                                        &gate.id,
                                    )
                                    .await?
                                    .ok_or_else(|| {
                                        WorkflowStoreError::GateNotReady(
                                            "corrective Plan review has no pre-admission authorization"
                                                .into(),
                                        )
                                    })?;
                                    let prior_plan_digest = previous
                                        .plan_snapshot
                                        .as_ref()
                                        .map(|snapshot| snapshot.digest.as_str())
                                        .ok_or_else(|| {
                                            WorkflowStoreError::GateNotReady(
                                                "corrective Plan review has no prior snapshot"
                                                    .into(),
                                            )
                                        })?;
                                    if authorization.gate_lineage
                                        != evidence.identity.gate_lineage
                                        || i64::from(authorization.review_round)
                                            != evidence.identity.review_round
                                        || authorization.prior_review_round
                                            != previous.review_round
                                        || authorization.author_task_id != author_task_id
                                        || authorization.required_node_ids
                                            != evidence.identity.required_node_ids
                                        || authorization.selected_node_ids
                                            != evidence.selected_node_ids
                                        || authorization.prior_plan_digest != prior_plan_digest
                                        || authorization.current_plan_digest
                                            != current_plan_snapshot.digest
                                    {
                                        return Err(WorkflowStoreError::GateNotReady(
                                            "corrective Plan authorization does not match the active evidence"
                                                .into(),
                                        ));
                                    }
                                    let classification = classify_plan_settlement_change_v2(
                                        &normalized,
                                        previous,
                                        &current_plan_snapshot,
                                        &authorization.author_task_id,
                                    )?;
                                    match classification {
                                        PlanChangeClassification::Localized {
                                            change,
                                            corrective_reviewer_node_ids,
                                        } => {
                                            let selected = select_corrective_reviewers(
                                                &PlanChangeClassification::Localized {
                                                    change: change.clone(),
                                                    corrective_reviewer_node_ids:
                                                        corrective_reviewer_node_ids.clone(),
                                                },
                                            )
                                            .into_iter()
                                            .collect::<Vec<_>>();
                                            if selected != authorization.selected_node_ids
                                                || change != authorization.localized_change
                                            {
                                                return Err(WorkflowStoreError::GateNotReady(
                                                    "Plan classifier no longer matches the immutable round authorization"
                                                        .into(),
                                                ));
                                            }
                                            v2_localized_change_digest = Some(
                                                canonical_json_sha256(
                                                    "codeg.completion.plan_change.v2",
                                                    1,
                                                    &change,
                                                )
                                                .map_err(|error| {
                                                    WorkflowStoreError::Persistence(
                                                        error.to_string(),
                                                    )
                                                })?,
                                            );
                                            authorized_localized_change = Some(change);
                                        }
                                        PlanChangeClassification::NewLineage { .. } => {
                                            return Err(WorkflowStoreError::GateNotReady(
                                                "material Plan change requires a new full-cohort lineage"
                                                    .into(),
                                            ));
                                        }
                                    }
                                }
                                let mut decision = derive_plan_review_round_v2(
                                    prior_v2_state.as_ref(),
                                    PlanReviewRoundInputV2 {
                                        gate_lineage: evidence.identity.gate_lineage.clone(),
                                        review_round: u32::try_from(
                                            evidence.identity.review_round,
                                        )
                                        .map_err(|_| {
                                            WorkflowStoreError::Persistence(
                                                "v2 Plan review round exceeds u32".into(),
                                            )
                                        })?,
                                        required_node_ids: evidence
                                            .identity
                                            .required_node_ids
                                            .clone(),
                                        selected_node_ids: evidence.selected_node_ids.clone(),
                                        reviewers: evidence.reviewers.clone(),
                                    },
                                    change,
                                )?;
                                decision.state.plan_snapshot = Some(current_plan_snapshot);
                                let localized_intersection = authorized_localized_change.is_some();
                                decision.state.localized_change = authorized_localized_change;
                                let derived_outcome =
                                    plan_v2_settlement_outcome(decision.state.next_action);
                                if v2_expectation.as_ref().is_some_and(|expectation| {
                                    expectation
                                        .outcome
                                        .as_ref()
                                        .is_some_and(|expected| *expected != derived_outcome)
                                }) {
                                    return Err(WorkflowStoreError::GateNotReady(format!(
                                        "expected outcome disagrees with derived v2 Plan outcome {:?}",
                                        derived_outcome
                                    )));
                                }
                                req.outcome = derived_outcome;
                                let next_action = decision.state.next_action;
                                let stagnation_count = decision.state.stagnation_count;
                                let rewrite_used = decision.state.rewrite_used;
                                plan_metric_observation = Some(PlanSettlementMetricObservation {
                                    change,
                                    localized_intersection,
                                    lineage_reset: lineage_reset_authorization.is_some(),
                                    sibling_reruns: if prior_v2_state.is_some() {
                                        decision.state.selected_node_ids.len() as u64
                                    } else {
                                        0
                                    },
                                });
                                v2_plan_author_task_id = Some(author_task_id);
                                v2_plan_digest = Some(covered_plan_digest);
                                v2_plan_decision = Some(decision);
                                (
                                    0,
                                    0,
                                    0,
                                    Some(next_action),
                                    stagnation_count,
                                    rewrite_used,
                                    None,
                                    None,
                                )
                            } else {
                                verify_plan_gate_ready(
                                    txn,
                                    &header.workflow_id,
                                    active_author_node_id,
                                    gate,
                                    req.gate_cycle as i64,
                                    header.active_manifest_revision,
                                    content_fp.as_str(),
                                    submission,
                                    lineage_prior.last(),
                                )
                                .await?;

                                // Completed-round-only reducer: this call is deliberately
                                // after all required runs and Author bindings are validated.
                                let state = derive_plan_review_state_for_protocol(
                                    header.completion_protocol_version,
                                    prior_state.as_ref(),
                                    &gate.reviewer_cohort_node_ids,
                                    submission,
                                )?
                                .expect("protocol-v1 Plan review must derive legacy state");
                                validate_plan_outcome(&req.outcome, &state)?;
                                let persisted = PersistedPlanReviewEvidence {
                                    submission: submission.clone(),
                                    state: state.clone(),
                                };
                                let persisted_json =
                                    serialize_bounded_plan_evidence(&persisted)?;
                                let report_files_json =
                                    serialize_plan_report_files(&state.findings)?;
                                (
                                    i64::from(state.critical_count),
                                    i64::from(state.important_count),
                                    i64::from(state.minor_count),
                                    Some(state.next_action),
                                    state.stagnation_count,
                                    state.rewrite_used,
                                    Some((persisted, persisted_json)),
                                    Some(report_files_json),
                                )
                            }
                        }
                        (SettleGateEvidence::Design { .. }, DocumentGateKind::Plan)
                        | (SettleGateEvidence::Plan(_), DocumentGateKind::Design) => {
                            return Err(WorkflowStoreError::GateNotReady(
                                "settlement evidence kind does not match document gate kind".into(),
                            ));
                        }
                    };

                    let now = Utc::now();
                    let next_graph = header.graph_revision + 1;
                    let plan_state = persisted_plan.as_ref().map(|(evidence, _)| &evidence.state);
                    let row = delegation_workflow_gate_settlement::ActiveModel {
                        workflow_id: Set(header.workflow_id.clone()),
                        gate_id: Set(req.gate_id.clone()),
                        gate_cycle: Set(req.gate_cycle as i64),
                        manifest_revision: Set(header.active_manifest_revision),
                        structural_revision: Set(header.structural_revision),
                        content_fingerprint: Set(content_fp),
                        evidence_scope_digest: Set(v2_gate_evidence
                            .as_ref()
                            .map(|evidence| evidence.identity.aggregate_scope_digest.clone())),
                        gate_lineage: Set(v2_gate_evidence
                            .as_ref()
                            .map(|evidence| evidence.identity.gate_lineage.clone())),
                        review_round: Set(v2_gate_evidence
                            .as_ref()
                            .map(|evidence| evidence.identity.review_round)),
                        required_node_set_json: Set(v2_gate_evidence
                            .as_ref()
                            .map(|evidence| {
                                serde_json::to_string(&evidence.identity.required_node_ids)
                            })
                            .transpose()
                            .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?),
                        required_evidence_task_ids_json: Set(v2_gate_evidence
                            .as_ref()
                            .map(|evidence| serde_json::to_string(&evidence.identity.task_ids))
                            .transpose()
                            .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?),
                        evidence_scope_digests_json: Set(v2_gate_evidence
                            .as_ref()
                            .map(|evidence| serde_json::to_string(&evidence.identity.scope_digests))
                            .transpose()
                            .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?),
                        localized_change_digest: Set(v2_localized_change_digest),
                        plan_round_state_v2_json: Set(v2_plan_decision
                            .as_ref()
                            .map(|decision| serde_json::to_string(&decision.state))
                            .transpose()
                            .map_err(|error| {
                                WorkflowStoreError::Persistence(format!(
                                    "serialize v2 Plan round state: {error}"
                                ))
                            })?),
                        outcome: Set(req.outcome.clone()),
                        critical_count: Set((header.completion_protocol_version != 2)
                            .then_some(critical_count)),
                        important_count: Set((header.completion_protocol_version != 2)
                            .then_some(important_count)),
                        minor_count: Set((header.completion_protocol_version != 2)
                            .then_some(minor_count)),
                        summary: Set(req.summary.clone()),
                        graph_revision_at_settle: Set(header.graph_revision),
                        review_scope: Set(plan_state.map(|state| plan_scope_to_db(state.scope))),
                        revision_kind: Set(
                            plan_state.map(|state| plan_revision_kind_to_db(state.revision_kind))
                        ),
                        scope_reason: Set(plan_state.map(|state| state.scope_reason.clone())),
                        required_reviewer_node_ids_json: Set(plan_state
                            .map(|state| serde_json::to_string(&state.reviewed_reviewer_node_ids))
                            .transpose()
                            .map_err(|error| {
                                WorkflowStoreError::Persistence(format!(
                                    "serialize Plan reviewer set: {error}"
                                ))
                            })?),
                        covered_author_task_id: Set(v2_plan_author_task_id.or_else(|| {
                            plan_state.map(|state| state.covered_author_task_id.clone())
                        })),
                        covered_plan_digest: Set(v2_plan_digest.or_else(|| {
                            plan_state.map(|state| state.covered_plan_digest.clone())
                        })),
                        net_improvement: Set(v2_plan_decision
                            .as_ref()
                            .map(|decision| decision.strict_improvement)
                            .or_else(|| plan_state.map(|state| state.net_improvement))),
                        finding_ledger_json: Set(
                            persisted_plan.map(|(_, persisted_json)| persisted_json)
                        ),
                        stagnation_count: Set(i64::from(stagnation_count)),
                        rewrite_used: Set(rewrite_used),
                        next_action: Set(plan_next_action.map(plan_next_action_to_db)),
                        report_files_json: Set(report_files_json),
                        lineage_reset_authorization_id: Set(
                            (header.completion_protocol_version != 2)
                                .then(|| req.recovery_authorization_id.clone())
                                .flatten(),
                        ),
                        created_at: Set(now),
                    };
                    row.insert(txn).await.map_err(db_err)?;

                    if let Some(decision) = v2_plan_decision.as_ref() {
                        delegation_plan_round_authorization::Entity::delete_by_id((
                            header.workflow_id.clone(),
                            gate.id.clone(),
                        ))
                        .exec(txn)
                        .await
                        .map_err(db_err)?;
                        if let Some(mut opening) =
                            derive_next_plan_review_round_v2(&decision.state, &[])?
                        {
                            if decision.state.next_action == PlanReviewNextAction::ContinueReview {
                                opening.selected_node_ids.clear();
                            }
                            let gate_state = delegation_workflow_gate_state::Entity::find_by_id((
                                header.workflow_id.clone(),
                                gate.id.clone(),
                            ))
                            .one(txn)
                            .await
                            .map_err(db_err)?
                            .ok_or_else(|| {
                                WorkflowStoreError::GateNotReady(
                                    "current Plan gate state is missing".into(),
                                )
                            })?;
                            if gate_state.gate_lineage != decision.state.gate_lineage
                                || gate_state.current_review_round
                                    != i64::from(decision.state.review_round)
                            {
                                return Err(WorkflowStoreError::GateCycleConflict(
                                    "current Plan gate state changed during settlement".into(),
                                ));
                            }
                            let mut gate_state: delegation_workflow_gate_state::ActiveModel =
                                gate_state.into();
                            gate_state.gate_lineage = Set(opening.gate_lineage);
                            gate_state.current_review_round =
                                Set(i64::from(opening.review_round));
                            gate_state.selected_node_ids_json = Set(
                                serde_json::to_string(&opening.selected_node_ids).map_err(
                                    |error| WorkflowStoreError::Persistence(error.to_string()),
                                )?,
                            );
                            gate_state.update(txn).await.map_err(db_err)?;
                        }
                    }

                    let state_revision = if let Some(reset) = lineage_reset_authorization.as_ref() {
                        let target_state = match req.outcome {
                            GateSettlementOutcome::Approved => ManifestWorkflowState::Approved,
                            GateSettlementOutcome::ChangesRequested => {
                                ManifestWorkflowState::Estimated
                            }
                            GateSettlementOutcome::Blocked => ManifestWorkflowState::Blocked,
                        };
                        let transition_reason_code =
                            if target_state == ManifestWorkflowState::Blocked {
                                WorkflowBlockCause::PlanGateBlocked.as_str()
                            } else {
                                reset.cause_code.as_str()
                            };
                        Some(
                            append_state_only_revision_txn(
                                txn,
                                &header,
                                StateOnlyRevisionRequest {
                                    target_state,
                                    transition_reason_code,
                                    recovery_authorization_id: req
                                        .recovery_authorization_id
                                        .as_deref(),
                                    consumer_correlation_id: Some(&reset.consumer_correlation_id),
                                    recovery_source_state_fingerprint: None,
                                    recovery_risk_class: None,
                                },
                                now,
                            )
                            .await?,
                        )
                    } else if req.outcome == GateSettlementOutcome::Blocked
                        && gate.gate_kind == DocumentGateKind::Plan
                    {
                        let cause = if plan_next_action
                            == Some(PlanReviewNextAction::UserDecisionRequired)
                        {
                            WorkflowBlockCause::PlanUserDecisionRequired
                        } else {
                            WorkflowBlockCause::PlanGateBlocked
                        };
                        Some(
                            append_workflow_block_revision_txn(
                                txn,
                                &header,
                                WorkflowBlockEntryRequest {
                                    cause,
                                    consumer_correlation_id: None,
                                },
                                now,
                            )
                            .await?,
                        )
                    } else if gate.gate_kind == DocumentGateKind::Plan
                        && req.outcome == GateSettlementOutcome::Approved
                        && plan_next_action == Some(PlanReviewNextAction::Approved)
                        && header.workflow_state != WorkflowState::Blocked
                    {
                        Some(
                            append_state_only_revision_txn(
                                txn,
                                &header,
                                StateOnlyRevisionRequest {
                                    target_state: ManifestWorkflowState::Approved,
                                    transition_reason_code: "plan_gate_approved",
                                    recovery_authorization_id: None,
                                    consumer_correlation_id: None,
                                    recovery_source_state_fingerprint: None,
                                    recovery_risk_class: None,
                                },
                                now,
                            )
                            .await?,
                        )
                    } else {
                        None
                    };

                    let lineage_reset_event = if let Some(reset) = lineage_reset_authorization {
                        let authorization_id = req
                            .recovery_authorization_id
                            .as_deref()
                            .expect("authorized lineage reset has authorization id");
                        let expectation = AuthorizationConsumeExpectation {
                            parent_conversation_id,
                            subject_kind: RecoverySubjectKind::Workflow,
                            subject_id: &header.workflow_id,
                            source_state_fingerprint: &reset.source_state_fingerprint,
                            allowed_action: RecoveryAllowedAction::ResetPlanLineage,
                            action_payload: &reset.action_payload,
                            consumer_kind: RecoveryConsumerKind::WorkflowManifestRevision,
                            consumer_id: &reset.consumer_id,
                            consumer_correlation_id: &reset.consumer_correlation_id,
                        };
                        consume_txn(txn, reset.authorization, &expectation, now)
                            .await
                            .map_err(map_workflow_authorization_error)?;
                        let revision = state_revision
                            .as_ref()
                            .expect("authorized lineage reset always creates state revision");
                        Some(RecoverWorkflowResult {
                            workflow_id: header.workflow_id.clone(),
                            old_state: workflow_state_to_manifest(header.workflow_state.clone()),
                            new_state: revision.workflow_state,
                            source_manifest_revision: revision.source_manifest_revision,
                            manifest_revision: revision.manifest_revision,
                            graph_revision: next_graph as u64,
                            cause_code: reset.cause_code,
                            recovery_authorization_id: authorization_id.to_string(),
                            idempotent_replay: false,
                        })
                    } else {
                        None
                    };

                    let mut am: delegation_workflow::ActiveModel = header.clone().into();
                    am.graph_revision = Set(next_graph);
                    am.updated_at = Set(now);
                    am.update(txn).await.map_err(db_err)?;

                    Ok((
                        SettleResult {
                            workflow_id: header.workflow_id.clone(),
                            gate_id: req.gate_id.clone(),
                            gate_cycle: req.gate_cycle,
                            graph_revision: next_graph as u64,
                            manifest_revision: state_revision
                                .map_or(header.active_manifest_revision as u64, |revision| {
                                    revision.manifest_revision
                                }),
                            outcome: req.outcome.clone(),
                            idempotent_replay: false,
                            plan_next_action,
                            critical_count,
                            important_count,
                            minor_count,
                            stagnation_count,
                            rewrite_used,
                            plan_metric_observation,
                        },
                        lineage_reset_event,
                    ))
                })
            },
        )
        .await;

    let (result, lineage_reset_event) = match result {
        Ok(r) => r,
        Err(sea_orm::TransactionError::Connection(e)) => {
            return Err(WorkflowStoreError::Persistence(e.to_string()));
        }
        Err(sea_orm::TransactionError::Transaction(e)) => {
            if let (Some(authorization_id), Some(reset_reason)) = (
                rejection_authorization_id.as_deref(),
                rejection_reset_reason.as_deref(),
            ) {
                emit_plan_lineage_reset_rejection_if_designated(
                    db,
                    emitter,
                    parent_conversation_id,
                    &rejection_workflow_id,
                    authorization_id,
                    reset_reason,
                    &e,
                )
                .await;
            }
            return Err(e);
        }
    };

    if !result.idempotent_replay {
        if let Some(recovery) = lineage_reset_event.as_ref() {
            emit_committed_recovery_events(emitter, recovery, "reset_plan_lineage", true);
        }
        emit_workflow_graph_changed(
            emitter,
            parent_conversation_id,
            &result.workflow_id,
            result.graph_revision,
        );
    }

    Ok(result)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DesignSelfReviewReadiness {
    NotApplicable,
    Ready,
    DecisionRequired,
    Superseded,
}

fn map_design_preflight_completion_error(
    error: super::completion_evidence::CompletionMutationError,
) -> WorkflowStoreError {
    let (code, message) = match error {
        super::completion_evidence::CompletionMutationError::Protocol { code, message }
        | super::completion_evidence::CompletionMutationError::Evidence(
            super::error::CompletionEvidenceError::Protocol { code, message },
        ) => (code, message),
        super::completion_evidence::CompletionMutationError::Evidence(
            super::error::CompletionEvidenceError::Persistence(message),
        ) => return WorkflowStoreError::Persistence(message),
        other => return WorkflowStoreError::Persistence(other.to_string()),
    };
    match code {
        "legacy_completion_protocol_read_only" => {
            WorkflowStoreError::LegacyCompletionProtocolReadOnly
        }
        "unsupported_completion_protocol" => {
            WorkflowStoreError::UnsupportedCompletionProtocolHeader(message)
        }
        _ => WorkflowStoreError::Persistence(message),
    }
}

async fn prepare_v2_design_self_review(
    db: &AppDatabase,
    parent_conversation_id: i32,
    req: &SettleWorkflowRequest,
) -> Result<DesignSelfReviewReadiness, WorkflowStoreError> {
    let req = req.clone();
    #[cfg(test)]
    honor_design_preflight_header_test_gate(&req.workflow_id).await;
    let result = db
        .conn
        .transaction::<_, DesignSelfReviewReadiness, WorkflowStoreError>(|txn| {
            Box::pin(async move {
                require_owned_stored_v2_header(txn, &req.workflow_id, parent_conversation_id)
                    .await?;
                let header = delegation_workflow::Entity::find_by_id(req.workflow_id.clone())
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or_else(|| WorkflowStoreError::NotFound(req.workflow_id.clone()))?;
                if header.parent_conversation_id != parent_conversation_id {
                    return Err(WorkflowStoreError::CrossParent {
                        workflow_id: header.workflow_id,
                        expected_parent: parent_conversation_id,
                        actual_parent: header.parent_conversation_id,
                    });
                }
                require_v2_mutation_for_connection(
                    txn,
                    header.completion_protocol_version,
                    &header.completion_protocol_mode,
                )
                .await?;
                if req.expected_graph_revision != header.graph_revision as u64 {
                    return Err(WorkflowStoreError::StaleGraphRevision {
                        expected: req.expected_graph_revision,
                        current: header.graph_revision as u64,
                    });
                }
                let document = load_active_manifest_document_txn(
                    txn,
                    &header.workflow_id,
                    header.active_manifest_revision,
                )
                .await?;
                let normalized = validate_manifest_document(&document)?;
                let Some(gate) = normalized.gates.iter().find(|gate| gate.id == req.gate_id) else {
                    return Ok(DesignSelfReviewReadiness::NotApplicable);
                };
                if gate.gate_kind != DocumentGateKind::Design
                    || gate.resolution_mode != ResolutionMode::SelfReview
                    || !gate.required_reviewer_node_ids.is_empty()
                {
                    return Ok(DesignSelfReviewReadiness::NotApplicable);
                }
                let design = normalized.design.as_ref().ok_or_else(|| {
                    WorkflowStoreError::GateNotReady(
                        "Design self-review requires an active Design document".into(),
                    )
                })?;
                let parent = conversation::Entity::find_by_id(parent_conversation_id)
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or(WorkflowStoreError::ParentNotFound(parent_conversation_id))?;
                let workspace = folder::Entity::find_by_id(parent.folder_id)
                    .one(txn)
                    .await
                    .map_err(db_err)?
                    .ok_or_else(|| {
                        WorkflowStoreError::Persistence(
                            "Design self-review workspace folder is missing".into(),
                        )
                    })?;
                let resolved = resolve_document(
                    std::path::Path::new(&workspace.path),
                    &design.rel_path,
                    MAX_DESIGN_SELF_REVIEW_BYTES,
                )
                .await
                .map_err(|_| WorkflowStoreError::CompletionArtifactUnavailable)?;

                let desired_lineage = design_self_review_lineage(
                    &header.workflow_id,
                    &gate.id,
                    &header.workflow_kind,
                    resolved.rel_path(),
                    resolved.digest(),
                );
                let prior_state = delegation_workflow_gate_state::Entity::find_by_id((
                    header.workflow_id.clone(),
                    gate.id.clone(),
                ))
                .one(txn)
                .await
                .map_err(db_err)?;
                let rotated = prior_state
                    .as_ref()
                    .is_some_and(|state| state.gate_lineage != desired_lineage);
                match prior_state {
                    Some(state) if state.gate_lineage != desired_lineage => {
                        let mut active: delegation_workflow_gate_state::ActiveModel = state.into();
                        active.gate_lineage = Set(desired_lineage.clone());
                        active.current_review_round = Set(1);
                        active.selected_node_ids_json = Set("[]".into());
                        active.update(txn).await.map_err(db_err)?;
                    }
                    Some(_) => {}
                    None => {
                        delegation_workflow_gate_state::ActiveModel {
                            workflow_id: Set(header.workflow_id.clone()),
                            gate_id: Set(gate.id.clone()),
                            gate_lineage: Set(desired_lineage.clone()),
                            current_review_round: Set(1),
                            selected_node_ids_json: Set("[]".into()),
                        }
                        .insert(txn)
                        .await
                        .map_err(db_err)?;
                    }
                }

                let resolved_design = DocumentRef {
                    rel_path: resolved.rel_path().to_string(),
                    digest: resolved.digest().to_string(),
                };
                let review_scope = build_design_root_review_scope(&DesignRootScopeInput {
                    workflow_kind: &header.workflow_kind,
                    design: &resolved_design,
                    gate_id: &gate.id,
                    gate_lineage: &desired_lineage,
                    resolution_mode: gate.resolution_mode,
                })
                .map_err(|error| WorkflowStoreError::GateNotReady(error.to_string()))?;
                let scope_digest =
                    canonical_json_sha256("codeg.completion.review_scope.v2", 1, &review_scope)
                        .map_err(|error| WorkflowStoreError::GateNotReady(error.to_string()))?;
                let binding_id = sha256_hex(
                    format!(
                        "codeg.design-root-binding.v1\0{}\0{}\0{}",
                        header.workflow_id, gate.id, desired_lineage
                    )
                    .as_bytes(),
                );
                let node_id = format!(
                    "platform-design-root-node:{}",
                    &sha256_hex(
                        format!(
                            "codeg.design-root-node.v1\0{}\0{}",
                            header.workflow_id, gate.id
                        )
                        .as_bytes()
                    )[..24]
                );
                let binding = delegation_workflow_design_root_binding::Entity::find_by_id((
                    header.workflow_id.clone(),
                    gate.id.clone(),
                    desired_lineage.clone(),
                ))
                .one(txn)
                .await
                .map_err(db_err)?;
                let binding_created = binding.is_none();
                let mut binding = match binding {
                    Some(binding)
                        if binding.design_identity == resolved.digest()
                            && binding.evidence_scope_digest == scope_digest =>
                    {
                        binding
                    }
                    Some(_) => return Ok(DesignSelfReviewReadiness::Superseded),
                    None => delegation_workflow_design_root_binding::ActiveModel {
                        workflow_id: Set(header.workflow_id.clone()),
                        gate_id: Set(gate.id.clone()),
                        gate_lineage: Set(desired_lineage.clone()),
                        node_id: Set(node_id),
                        task_id: Set(format!("platform-design-root-task:{binding_id}")),
                        latest_run_id: Set(format!(
                            "platform-design-root-run:{}",
                            sha256_hex(
                                format!("{binding_id}\0{}\0{scope_digest}", resolved.digest())
                                    .as_bytes()
                            )
                        )),
                        design_identity: Set(resolved.digest().to_string()),
                        evidence_scope_digest: Set(scope_digest),
                        graph_revision: Set(header.graph_revision),
                    }
                    .insert(txn)
                    .await
                    .map_err(db_err)?,
                };

                let attention = delegation_attention_request::Entity::find()
                    .filter(delegation_attention_request::Column::TaskId.eq(&binding.task_id))
                    .filter(
                        delegation_attention_request::Column::Kind
                            .eq(AttentionKind::DesignSelfReviewDecision),
                    )
                    .order_by_desc(delegation_attention_request::Column::CreatedAt)
                    .one(txn)
                    .await
                    .map_err(db_err)?;
                let validated = validated_design_self_review_outcome(&binding, attention.as_ref());
                let opens_attention = matches!(&validated, Ok(None)) && attention.is_none();
                let graph_changed = rotated || binding_created || opens_attention;
                if graph_changed {
                    let next_graph = header.graph_revision.checked_add(1).ok_or_else(|| {
                        WorkflowStoreError::Persistence("graph revision overflow".into())
                    })?;
                    if binding.graph_revision != next_graph {
                        let mut active: delegation_workflow_design_root_binding::ActiveModel =
                            binding.into();
                        active.graph_revision = Set(next_graph);
                        binding = active.update(txn).await.map_err(db_err)?;
                    }
                    let mut active: delegation_workflow::ActiveModel = header.clone().into();
                    active.graph_revision = Set(next_graph);
                    active.updated_at = Set(Utc::now());
                    active.update(txn).await.map_err(db_err)?;
                    if rotated {
                        supersede_prior_design_self_review_attentions_txn(
                            txn,
                            &header.workflow_id,
                            &binding.task_id,
                            next_graph as u64,
                        )
                        .await?;
                    }
                }
                match validated {
                    Ok(Some(_)) if !rotated => Ok(DesignSelfReviewReadiness::Ready),
                    Ok(Some(_)) => Ok(DesignSelfReviewReadiness::Superseded),
                    Ok(None) => {
                        open_design_self_review_decision_txn(txn, &binding, parent_conversation_id)
                            .await
                            .map_err(map_design_preflight_completion_error)?;
                        Ok(if rotated {
                            DesignSelfReviewReadiness::Superseded
                        } else {
                            DesignSelfReviewReadiness::DecisionRequired
                        })
                    }
                    Err(DesignSelfReviewDecisionError::Superseded) => {
                        Ok(DesignSelfReviewReadiness::Superseded)
                    }
                    Err(DesignSelfReviewDecisionError::Corrupt) => {
                        Err(WorkflowStoreError::Persistence(
                            "Design self-review decision is corrupt".into(),
                        ))
                    }
                }
            })
        })
        .await;
    match result {
        Ok(readiness) => Ok(readiness),
        Err(sea_orm::TransactionError::Connection(error)) => Err(db_err(error)),
        Err(sea_orm::TransactionError::Transaction(error)) => Err(error),
    }
}

fn design_self_review_lineage(
    workflow_id: &str,
    gate_id: &str,
    workflow_kind: &str,
    design_path: &str,
    design_digest: &str,
) -> String {
    format!(
        "sha256:{}",
        sha256_hex(
            format!(
                "codeg.design-root-lineage.v1\0{workflow_id}\0{gate_id}\0{workflow_kind}\0{design_path}\0{design_digest}\0self_review"
            )
            .as_bytes()
        )
    )
}

async fn supersede_prior_design_self_review_attentions_txn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    current_task_id: &str,
    graph_revision: u64,
) -> Result<(), WorkflowStoreError> {
    let prior_task_ids = delegation_workflow_design_root_binding::Entity::find()
        .filter(
            delegation_workflow_design_root_binding::Column::WorkflowId.eq(workflow_id.to_string()),
        )
        .all(conn)
        .await
        .map_err(db_err)?
        .into_iter()
        .filter(|binding| binding.task_id != current_task_id)
        .map(|binding| binding.task_id)
        .collect::<Vec<_>>();
    if prior_task_ids.is_empty() {
        return Ok(());
    }
    let open = delegation_attention_request::Entity::find()
        .filter(delegation_attention_request::Column::TaskId.is_in(prior_task_ids))
        .filter(
            delegation_attention_request::Column::Kind.eq(AttentionKind::DesignSelfReviewDecision),
        )
        .filter(delegation_attention_request::Column::Status.eq("open"))
        .all(conn)
        .await
        .map_err(db_err)?;
    for row in open {
        let mut active: delegation_attention_request::ActiveModel = row.into();
        active.status = Set("resolved".into());
        active.resolution_code = Set(Some("superseded".into()));
        active.resolution_json = Set(Some(
            serde_json::json!({
                "version": 1,
                "code": "superseded",
                "graph_revision": graph_revision,
            })
            .to_string(),
        ));
        active.resolved_at = Set(Some(Utc::now()));
        active.update(conn).await.map_err(db_err)?;
    }
    Ok(())
}

/// Agent-facing compact recovery read (A5 + B4).
///
/// Entire snapshot is loaded in a single SQLite read transaction for consistency.
pub async fn get_workflow_state_core(
    db: &AppDatabase,
    parent_conversation_id: i32,
    workflow_id: Option<&str>,
) -> Result<WorkflowStateIndexDto, WorkflowStoreError> {
    let workflow_id_owned = workflow_id.map(|s| s.to_string());
    let result = db
        .conn
        .transaction::<_, WorkflowStateIndexDto, WorkflowStoreError>(|txn| {
            Box::pin(async move {
                let header = match workflow_id_owned.as_deref() {
                    Some(id) => {
                        let h = delegation_workflow::Entity::find_by_id(id.to_string())
                            .one(txn)
                            .await
                            .map_err(db_err)?
                            .ok_or_else(|| WorkflowStoreError::NotFound(id.to_string()))?;
                        if h.parent_conversation_id != parent_conversation_id {
                            return Err(WorkflowStoreError::CrossParent {
                                workflow_id: h.workflow_id.clone(),
                                expected_parent: parent_conversation_id,
                                actual_parent: h.parent_conversation_id,
                            });
                        }
                        h
                    }
                    None => delegation_workflow::Entity::find()
                        .filter(
                            delegation_workflow::Column::ParentConversationId
                                .eq(parent_conversation_id),
                        )
                        .filter(
                            delegation_workflow::Column::WorkflowKind
                                .eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY),
                        )
                        .one(txn)
                        .await
                        .map_err(db_err)?
                        .ok_or_else(|| {
                            WorkflowStoreError::NotFound(format!(
                                "parent={parent_conversation_id} kind={WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY}"
                            ))
                        })?,
                };
                let completion_protocol =
                    super::workflow_restart::completion_protocol_projection(txn, &header)
                        .await
                        .map_err(db_err)?;

                let recovery_snapshot = if header.workflow_state == WorkflowState::Blocked {
                    Some(
                        load_workflow_recovery_snapshot_detailed_conn(txn, &header, None, None)
                            .await?,
                    )
                } else {
                    None
                };
                let recovery = recovery_snapshot
                    .as_ref()
                    .map(|loaded| decide_workflow_recovery(&loaded.snapshot).projection());

                let normalized = match recovery_snapshot {
                    Some(loaded) => match loaded.normalized {
                        Some(normalized) => normalized,
                        None => {
                            return Ok(project_invalid_manifest_recovery_index(
                                &header,
                                &loaded.snapshot,
                                recovery.expect("blocked snapshot has projection"),
                                completion_protocol,
                            ));
                        }
                    },
                    None => {
                        let doc = load_active_manifest_document_txn(
                            txn,
                            &header.workflow_id,
                            header.active_manifest_revision,
                        )
                        .await?;
                        validate_manifest_document(&doc)?
                    }
                };

                let bindings = delegation_workflow_node_binding::Entity::find()
                    .filter(
                        delegation_workflow_node_binding::Column::WorkflowId
                            .eq(header.workflow_id.clone()),
                    )
                    .all(txn)
                    .await
                    .map_err(db_err)?;

                let run_bindings = delegation_workflow_run_binding::Entity::find()
                    .filter(
                        delegation_workflow_run_binding::Column::WorkflowId
                            .eq(header.workflow_id.clone()),
                    )
                    .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
                    .all(txn)
                    .await
                    .map_err(db_err)?;

                let settlements = delegation_workflow_gate_settlement::Entity::find()
                    .filter(
                        delegation_workflow_gate_settlement::Column::WorkflowId
                            .eq(header.workflow_id.clone()),
                    )
                    .order_by_asc(delegation_workflow_gate_settlement::Column::GateCycle)
                    .all(txn)
                    .await
                    .map_err(db_err)?;
                let current_final_gate = delegation_workflow_gate_state::Entity::find_by_id((
                    header.workflow_id.clone(),
                    "final".to_string(),
                ))
                .one(txn)
                .await
                .map_err(db_err)?;
                let required_final_reviewer_node_ids = normalized
                    .nodes
                    .iter()
                    .filter(|node| {
                        node.phase_id.as_deref() == Some(super::types::PHASE_FINAL)
                            && node.role == Some(ManifestNodeRole::Reviewer)
                            && node.required
                    })
                    .map(|node| node.id.as_str())
                    .collect::<HashSet<_>>();
                let completion_projection_bindings = match current_final_gate {
                    Some(current_final_gate) if !required_final_reviewer_node_ids.is_empty() => {
                        run_bindings
                            .iter()
                            .filter(|binding| {
                                !required_final_reviewer_node_ids
                                    .contains(binding.node_id.as_str())
                                    || (binding.gate_id.as_deref()
                                        == Some(current_final_gate.gate_id.as_str())
                                        && binding.gate_lineage.as_deref()
                                            == Some(current_final_gate.gate_lineage.as_str())
                                        && binding.review_round
                                            == Some(current_final_gate.current_review_round))
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    }
                    _ => run_bindings.clone(),
                };

                let mut latest_by_node: HashMap<
                    String,
                    &delegation_workflow_run_binding::Model,
                > = HashMap::new();
                for rb in &run_bindings {
                    latest_by_node.entry(rb.node_id.clone()).or_insert(rb);
                }

                let task_ids: Vec<String> = latest_by_node
                    .values()
                    .map(|rb| rb.task_id.clone())
                    .collect();
                let runs = if task_ids.is_empty() {
                    Vec::new()
                } else {
                    delegation_task_run::Entity::find()
                        .filter(delegation_task_run::Column::TaskId.is_in(task_ids))
                        .all(txn)
                        .await
                        .map_err(db_err)?
                };
                let run_by_id: HashMap<String, &delegation_task_run::Model> =
                    runs.iter().map(|r| (r.task_id.clone(), r)).collect();
                let completion_batch = load_workflow_completion_projection_batch(
                    txn,
                    &header,
                    &normalized,
                    &bindings,
                    &completion_projection_bindings,
                    &runs,
                )
                .await
                .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
                let validated_v2_by_task = completion_batch.validated_by_task;
                let completion_v2_by_task = completion_batch.completion_by_task;

                let required_node_ids: HashSet<String> = normalized
                    .gates
                    .iter()
                    .flat_map(|g| g.required_reviewer_node_ids.iter().cloned())
                    .collect();
                let active_manifest_node_ids: HashSet<String> = normalized
                    .nodes
                    .iter()
                    .filter(|node| node.kind == ManifestNodeKind::WorkUnit)
                    .map(|node| node.id.clone())
                    .collect();

                let nodes: Vec<WorkflowNodeStateDto> = bindings
                    .iter()
                    .map(|b| {
                        let latest = latest_by_node.get(&b.node_id);
                        let run = latest.and_then(|rb| run_by_id.get(&rb.task_id).copied());
                        let (verdict, report_file) = recovery_card_fields(run);
                        let evidence_time = run
                            .and_then(|r| r.finished_at)
                            .or_else(|| latest.map(|rb| rb.created_at))
                            .or(Some(b.updated_at));
                        WorkflowNodeStateDto {
                            node_id: b.node_id.clone(),
                            work_unit_key: b.work_unit_key.clone(),
                            role: b.role.clone(),
                            agent_type: b.agent_type.clone(),
                            profile_id: b.profile_id.clone(),
                            phase_id: b.phase_id.clone(),
                            task_index: b.task_index.map(|i| i as u32),
                            is_observed: b.is_observed,
                            retained_observed: b.retained_observed,
                            cohort_frozen: b.cohort_frozen,
                            node_outcome: b.node_outcome.as_ref().map(|o| match o {
                                NodeOutcome::Canceled => "canceled".to_string(),
                            }),
                            latest_task_id: latest.map(|rb| rb.task_id.clone()),
                            latest_status: run.map(|r| run_status_str(&r.status).to_string()),
                            latest_generation: run.map(|r| r.generation),
                            summary_validated: latest.map(|rb| rb.summary_validated),
                            artifact_digest: latest.and_then(|rb| rb.artifact_digest.clone()),
                            child_conversation_id: run.map(|run| run.child_conversation_id),
                            reviewed_task_id: latest.and_then(|rb| rb.reviewed_task_id.clone()),
                            verdict,
                            report_file,
                            completion: latest
                                .and_then(|rb| completion_v2_by_task.get(&rb.task_id))
                                .cloned(),
                            gate_id: latest.and_then(|rb| rb.gate_id.clone()),
                            gate_cycle: latest.and_then(|rb| rb.gate_cycle),
                            replaced_task_id: run.and_then(|r| r.replaced_task_id.clone()),
                            required_for_gate: required_node_ids.contains(&b.node_id),
                            evidence_time,
                        }
                    })
                    .collect();

                let mut gates = Vec::with_capacity(normalized.gates.len());
                for g in &normalized.gates {
                    let gate_settlements: Vec<_> =
                        settlements.iter().filter(|s| s.gate_id == g.id).collect();
                    let current_fp = gate_content_fingerprint(g.gate_kind, &header);
                    // Display settlement only when it covers current gate content.
                    let latest = gate_settlements
                        .iter()
                        .rfind(|s| s.content_fingerprint == current_fp)
                        .copied();
                    let cycle_settlements: Vec<_> = if g.gate_kind == DocumentGateKind::Plan {
                        settlements
                            .iter()
                            .filter(|settlement| settlement.review_scope.is_some())
                            .collect()
                    } else {
                        gate_settlements.clone()
                    };
                    let max_cycle = cycle_settlements
                        .iter()
                        .map(|s| s.gate_cycle)
                        .max()
                        .unwrap_or(0);
                    let next_cycle = match latest {
                        Some(s) if s.outcome == GateSettlementOutcome::Approved => {
                            s.gate_cycle + 1
                        }
                        Some(s) => s.gate_cycle + 1,
                        None => max_cycle + 1,
                    };
                    gates.push(WorkflowGateStateDto {
                        gate_id: g.id.clone(),
                        gate_kind: g.gate_kind.as_str().to_string(),
                        resolution_mode: resolution_mode_str(g.resolution_mode).to_string(),
                        reviewer_cohort_node_ids: g.reviewer_cohort_node_ids.clone(),
                        required_reviewer_node_ids: g.required_reviewer_node_ids.clone(),
                        latest_gate_cycle: latest.map(|s| s.gate_cycle),
                        latest_outcome: latest
                            .map(|s| settlement_outcome_str(&s.outcome).to_string()),
                        next_gate_cycle: next_cycle,
                    });
                }

                let current_plan_settlement = settlements
                    .iter()
                    .rev()
                    .find(|settlement| {
                        settlement.review_scope.is_some()
                            && settlement.content_fingerprint == header.plan_fingerprint
                    });
                let latest_plan_settlement = settlements
                    .iter()
                    .rev()
                    .find(|settlement| settlement.review_scope.is_some());
                let persisted_plan_review = current_plan_settlement
                    .or(latest_plan_settlement)
                    .map(load_persisted_plan_evidence)
                    .transpose();
                let latest_plan_review = match persisted_plan_review {
                    Ok(evidence) => evidence.map(|evidence| evidence.state),
                    Err(_error)
                        if recovery.as_ref().is_some_and(|projection| {
                            projection
                                .blockers
                                .iter()
                                .any(|blocker| blocker == "stale_plan_gate_evidence")
                        }) =>
                    {
                        None
                    }
                    Err(error) => return Err(error),
                };

                let mut task_gate_passed = BTreeMap::new();
                for policy in &normalized.task_policies {
                    let implementer_or_fixer = latest_by_node
                        .get(&policy.route.implementer_node_id)
                        .and_then(|binding| {
                            run_by_id.get(&binding.task_id).map(|run| {
                                evidence_from_run_binding_and_validated(
                                    run,
                                    binding,
                                    header.completion_protocol_version,
                                    validated_v2_by_task.get(&binding.task_id),
                                )
                            })
                        });
                    let required_reviewers = policy
                        .route
                        .reviewer_node_ids
                        .iter()
                        .map(|node_id| RequiredReviewerEvidence {
                            node_id: node_id.clone(),
                            evidence: latest_by_node.get(node_id).and_then(|binding| {
                                run_by_id.get(&binding.task_id).map(|run| {
                                    evidence_from_run_binding_and_validated(
                                        run,
                                        binding,
                                        header.completion_protocol_version,
                                        validated_v2_by_task.get(&binding.task_id),
                                    )
                                })
                            }),
                        })
                        .collect();
                    let evaluation = evaluate_execution_gate(&ExecutionGateInput {
                        kind: ExecutionGateKind::Task,
                        implementer_or_fixer,
                        required_reviewers,
                        branch_tip_digest: None,
                    });
                    task_gate_passed.insert(policy.task_index, evaluation.passed);
                }

                let design_root_completion =
                    delegation_workflow_design_root_binding::Entity::find()
                        .filter(
                            delegation_workflow_design_root_binding::Column::WorkflowId
                                .eq(header.workflow_id.clone()),
                        )
                        .one(txn)
                        .await
                        .map_err(db_err)?
                        .map(|binding| binding.task_id);
                let design_root_completion = match design_root_completion {
                    Some(task_id) => load_completion_projection(txn, &task_id)
                        .await
                        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?,
                    None => None,
                };

                let full_state = WorkflowStateDto {
                    workflow_id: header.workflow_id,
                    parent_conversation_id: header.parent_conversation_id,
                    workflow_kind: header.workflow_kind,
                    capability_version: header.capability_version,
                    workflow_state: workflow_state_to_manifest(header.workflow_state),
                    manifest_revision: header.active_manifest_revision as u64,
                    graph_revision: header.graph_revision as u64,
                    schema_version: header.schema_version as u64,
                    publication_token: header.publication_token,
                    plan_target_rel_path: normalized.plan_target_rel_path,
                    risk_policy_version: normalized.risk_policy_version,
                    completion_protocol,
                    completion: design_root_completion.or_else(|| {
                        nodes
                            .iter()
                            .filter_map(|node| node.completion.as_ref())
                            .find(|completion| {
                                completion.card.state != super::CompletionCardState::Resolved
                            })
                            .or_else(|| {
                                nodes
                                    .iter()
                                    .filter_map(|node| node.completion.as_ref())
                                    .next()
                            })
                            .cloned()
                    }),
                    task_policies: normalized.task_policies,
                    design: normalized.design,
                    plan: normalized.plan,
                    nodes,
                    gates,
                    latest_plan_review,
                    evidence_truncated: false,
                };
                let mut index = project_workflow_state_index(
                    full_state,
                    &active_manifest_node_ids,
                    &task_gate_passed,
                );
                index.recovery = recovery;
                constrain_plan_recovery_sources(&mut index);
                Ok(index)
            })
        })
        .await;

    match result {
        Ok(dto) => Ok(dto),
        Err(sea_orm::TransactionError::Connection(e)) => {
            Err(WorkflowStoreError::Persistence(e.to_string()))
        }
        Err(sea_orm::TransactionError::Transaction(e)) => Err(e),
    }
}

pub async fn load_workflow_recovery_snapshot_txn(
    txn: &DatabaseTransaction,
    header: &delegation_workflow::Model,
    displayed_reset_reason: Option<&str>,
) -> Result<WorkflowRecoverySnapshot, WorkflowStoreError> {
    load_workflow_recovery_snapshot_conn(txn, header, displayed_reset_reason).await
}

pub(crate) async fn load_workflow_recovery_snapshot_conn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    displayed_reset_reason: Option<&str>,
) -> Result<WorkflowRecoverySnapshot, WorkflowStoreError> {
    Ok(
        load_workflow_recovery_snapshot_detailed_conn(conn, header, displayed_reset_reason, None)
            .await?
            .snapshot,
    )
}

struct LoadedWorkflowRecoverySnapshot {
    snapshot: WorkflowRecoverySnapshot,
    normalized: Option<NormalizedManifest>,
}

async fn load_workflow_recovery_snapshot_detailed_conn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    displayed_reset_reason: Option<&str>,
    revision_ceiling: Option<i64>,
) -> Result<LoadedWorkflowRecoverySnapshot, WorkflowStoreError> {
    let mut contradictory_durable_state = false;
    let header_state = workflow_state_to_manifest(header.workflow_state.clone());
    if inject_recovery_manifest_read_failure() {
        return Err(WorkflowStoreError::Persistence(
            "injected recovery manifest read failure".into(),
        ));
    }
    note_recovery_revision_query();
    let mut revision_query = delegation_workflow_manifest_revision::Entity::find().filter(
        delegation_workflow_manifest_revision::Column::WorkflowId.eq(header.workflow_id.clone()),
    );
    if let Some(revision_ceiling) = revision_ceiling {
        revision_query = revision_query.filter(
            delegation_workflow_manifest_revision::Column::ManifestRevision.lte(revision_ceiling),
        );
    }
    let revisions = revision_query
        .order_by_asc(delegation_workflow_manifest_revision::Column::ManifestRevision)
        .all(conn)
        .await
        .map_err(db_err)?;
    let revision = revisions
        .last()
        .filter(|row| row.manifest_revision == header.active_manifest_revision);

    let active_manifest_revision_kind = revision
        .as_ref()
        .and_then(|row| ManifestRevisionKind::from_db(row.revision_kind.as_deref()).ok())
        .unwrap_or_else(|| {
            if revision.is_some() {
                contradictory_durable_state = true;
            }
            ManifestRevisionKind::Publication
        });
    let active_manifest_source_revision = revision
        .as_ref()
        .and_then(|row| row.source_manifest_revision)
        .and_then(|value| u64::try_from(value).ok());
    if active_manifest_revision_kind == ManifestRevisionKind::StateOnly
        && active_manifest_source_revision.is_none()
    {
        contradictory_durable_state = true;
    }
    if active_manifest_revision_kind == ManifestRevisionKind::StateOnly
        && active_manifest_source_revision.is_some_and(|source_revision| {
            source_revision >= header.active_manifest_revision as u64
        })
    {
        contradictory_durable_state = true;
    }

    let parsed_document = revision
        .as_ref()
        .and_then(|row| serde_json::from_str::<ManifestDocument>(&row.document_json).ok());
    let normalized = parsed_document
        .as_ref()
        .and_then(|document| validate_manifest_document(document).ok());
    let manifest_state = revision
        .as_ref()
        .and_then(|row| parse_manifest_state(&row.manifest_state));
    let normalized_manifest_state = normalized.as_ref().map(|document| document.workflow_state);
    let document_digest_valid = revision
        .as_ref()
        .is_some_and(|row| sha256_hex(row.document_json.as_bytes()) == row.document_digest);
    let active_manifest_valid = revision.is_some()
        && parsed_document.is_some()
        && normalized.is_some()
        && manifest_state.is_some()
        && document_digest_valid;
    let header_manifest_state_match =
        manifest_state == Some(header_state) && normalized_manifest_state == Some(header_state);
    let fingerprints_valid = normalized.as_ref().is_some_and(|document| {
        design_fingerprint_hash(document) == header.design_fingerprint
            && plan_fingerprint_hash(document) == header.plan_fingerprint
    });
    if !workflow_recovery_lineage_valid(header, &revisions, normalized.as_ref()) {
        contradictory_durable_state = true;
    }

    let bindings = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .all(conn)
        .await
        .map_err(db_err)?;
    let run_bindings = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .all(conn)
        .await
        .map_err(db_err)?;
    let task_ids = run_bindings
        .iter()
        .map(|binding| binding.task_id.clone())
        .collect::<Vec<_>>();
    let runs = if task_ids.is_empty() {
        Vec::new()
    } else {
        delegation_task_run::Entity::find()
            .filter(delegation_task_run::Column::TaskId.is_in(task_ids))
            .all(conn)
            .await
            .map_err(db_err)?
    };
    let run_by_id = runs
        .iter()
        .map(|run| (run.task_id.clone(), run))
        .collect::<HashMap<_, _>>();
    let active_binding_by_node = bindings
        .iter()
        .filter(|binding| binding.retired_revision.is_none())
        .map(|binding| (binding.node_id.clone(), binding))
        .collect::<HashMap<_, _>>();
    let mut latest_run_binding_by_node = HashMap::new();
    for run_binding in &run_bindings {
        latest_run_binding_by_node
            .entry(run_binding.node_id.clone())
            .or_insert(run_binding);
    }

    let binding_lifecycle = bindings
        .iter()
        .map(|binding| WorkflowRecoveryBindingLifecycle {
            node_id: binding.node_id.clone(),
            work_unit_key: binding.work_unit_key.clone(),
            role: binding.role.clone(),
            agent_type: binding.agent_type.clone(),
            profile_id: binding.profile_id.clone(),
            phase_id: binding.phase_id.clone(),
            task_index: binding
                .task_index
                .and_then(|value| u32::try_from(value).ok()),
            introduced_revision: u64::try_from(binding.introduced_revision).unwrap_or_default(),
            retired_revision: binding
                .retired_revision
                .and_then(|value| u64::try_from(value).ok()),
            observed: binding.is_observed,
            retained_observed: binding.retained_observed,
            frozen: binding.cohort_frozen,
            node_outcome: binding.node_outcome.as_ref().map(|outcome| match outcome {
                NodeOutcome::Canceled => "canceled".to_string(),
            }),
        })
        .collect::<Vec<_>>();

    let active_runs = run_bindings
        .iter()
        .filter_map(|binding| {
            let run = run_by_id.get(&binding.task_id)?;
            matches!(
                run.status,
                DelegationRunStatus::Running | DelegationRunStatus::Reserving
            )
            .then(|| WorkflowRecoveryActiveRun {
                task_id: run.task_id.clone(),
                node_id: binding.node_id.clone(),
                status: run.status.clone(),
                generation: run.generation,
                lineage_ordinal: binding.lineage_ordinal,
                replaced_task_id: run.replaced_task_id.clone(),
            })
        })
        .collect::<Vec<_>>();

    let mut latest_run_supersession_valid = true;
    let bound_task_ids = run_bindings
        .iter()
        .map(|binding| binding.task_id.as_str())
        .collect::<HashSet<_>>();
    let mut lineage_ordinals = HashSet::new();
    for binding in &run_bindings {
        let Some(run) = run_by_id.get(&binding.task_id) else {
            latest_run_supersession_valid = false;
            continue;
        };
        let replacement_valid = run.replaced_task_id.as_deref().is_none_or(|replaced| {
            bound_task_ids.contains(replaced)
                && run_bindings.iter().any(|candidate| {
                    candidate.task_id == replaced
                        && candidate.node_id == binding.node_id
                        && candidate.lineage_ordinal < binding.lineage_ordinal
                })
        });
        if !lineage_ordinals.insert((binding.node_id.as_str(), binding.lineage_ordinal))
            || !replacement_valid
        {
            latest_run_supersession_valid = false;
        }
    }

    let binding_evidence_consistent = normalized.as_ref().is_some_and(|document| {
        let active_manifest_nodes = document
            .nodes
            .iter()
            .filter(|node| node.kind == ManifestNodeKind::WorkUnit)
            .map(|node| (node.id.as_str(), node))
            .collect::<HashMap<_, _>>();
        let active_bindings_match = active_binding_by_node.iter().all(|(node_id, binding)| {
            active_manifest_nodes
                .get(node_id.as_str())
                .is_some_and(|node| {
                    node.work_unit_key.as_deref() == Some(binding.work_unit_key.as_str())
                        && node.role.map(role_str) == Some(binding.role.as_str())
                        && node.agent_type.as_deref() == Some(binding.agent_type.as_str())
                        && node.profile_id == binding.profile_id
                        && node.phase_id.as_deref() == Some(binding.phase_id.as_str())
                        && node.task_index.map(i64::from) == binding.task_index
                })
        });
        let manifest_nodes_bound = active_manifest_nodes
            .keys()
            .all(|node_id| active_binding_by_node.contains_key(*node_id));
        let run_bindings_match = run_bindings.iter().all(|run_binding| {
            bindings
                .iter()
                .any(|binding| binding.node_id == run_binding.node_id)
                && run_by_id.get(&run_binding.task_id).is_some_and(|run| {
                    run.parent_conversation_id == header.parent_conversation_id
                        && run.work_unit_key.as_deref()
                            == bindings
                                .iter()
                                .find(|binding| binding.node_id == run_binding.node_id)
                                .map(|binding| binding.work_unit_key.as_str())
                })
        });
        active_bindings_match && manifest_nodes_bound && run_bindings_match
    });

    let mut frozen_task_cohorts = Vec::new();
    if let Some(document) = normalized.as_ref() {
        let mut covered_task_indices = HashSet::new();
        for policy in &document.task_policies {
            covered_task_indices.insert(policy.task_index);
            let mut route_node_ids = vec![policy.route.implementer_node_id.clone()];
            route_node_ids.extend(policy.route.reviewer_node_ids.iter().cloned());
            let frozen = route_node_ids.iter().any(|node_id| {
                active_binding_by_node
                    .get(node_id)
                    .is_some_and(|binding| binding.cohort_frozen)
                    || run_bindings
                        .iter()
                        .any(|binding| binding.node_id == *node_id)
            });
            if !frozen {
                continue;
            }
            let route_complete = route_node_ids
                .iter()
                .all(|node_id| active_binding_by_node.contains_key(node_id));
            let complete_cohort_frozen = route_node_ids.iter().all(|node_id| {
                active_binding_by_node
                    .get(node_id)
                    .is_some_and(|binding| binding.cohort_frozen)
            });
            let canceled_evidence_consistent = route_node_ids.iter().all(|node_id| {
                let Some(binding) = active_binding_by_node.get(node_id) else {
                    return false;
                };
                if binding.node_outcome != Some(NodeOutcome::Canceled) {
                    return true;
                }
                latest_run_binding_by_node
                    .get(node_id)
                    .and_then(|run_binding| run_by_id.get(&run_binding.task_id))
                    .is_none_or(|run| run.status == DelegationRunStatus::Canceled)
            });
            let unresolved = !route_complete
                || !complete_cohort_frozen
                || !canceled_evidence_consistent
                || active_runs
                    .iter()
                    .any(|run| route_node_ids.contains(&run.node_id));
            frozen_task_cohorts.push(WorkflowRecoveryFrozenTaskCohort {
                task_index: policy.task_index,
                implementer_node_id: policy.route.implementer_node_id.clone(),
                reviewer_node_ids: policy.route.reviewer_node_ids.clone(),
                route_complete,
                unresolved,
                evidence_consistent: route_complete
                    && complete_cohort_frozen
                    && canceled_evidence_consistent,
            });
        }
        let orphaned_frozen_indices = bindings
            .iter()
            .filter(|binding| binding.cohort_frozen)
            .filter_map(|binding| binding.task_index)
            .filter_map(|index| u32::try_from(index).ok())
            .filter(|index| !covered_task_indices.contains(index))
            .collect::<BTreeSet<_>>();
        for task_index in orphaned_frozen_indices {
            frozen_task_cohorts.push(WorkflowRecoveryFrozenTaskCohort {
                task_index,
                implementer_node_id: String::new(),
                reviewer_node_ids: Vec::new(),
                route_complete: false,
                unresolved: true,
                evidence_consistent: false,
            });
        }
    }

    let mut validated_v2_by_task = HashMap::new();
    if header.completion_protocol_version == 2 {
        for binding in latest_run_binding_by_node.values() {
            if let Ok(validated) = load_validated_completion_evidence(conn, &binding.task_id).await
            {
                validated_v2_by_task.insert(binding.task_id.clone(), validated);
            }
        }
    }
    let open_completion_task_ids = delegation_attention_request::Entity::find()
        .filter(delegation_attention_request::Column::Status.eq("open"))
        .all(conn)
        .await
        .map_err(db_err)?
        .into_iter()
        .filter(|attention| {
            matches!(
                attention.kind,
                AttentionKind::CompletionDecision
                    | AttentionKind::CompletionArtifactRecovery
                    | AttentionKind::DesignSelfReviewDecision
            )
        })
        .map(|attention| attention.task_id)
        .collect::<HashSet<_>>();

    let plan_identity = |node_id: &str| -> Option<WorkflowRecoveryPlanIdentity> {
        let binding = active_binding_by_node.get(node_id)?;
        let latest = latest_run_binding_by_node.get(node_id).copied();
        let run = latest.and_then(|latest| run_by_id.get(&latest.task_id).copied());
        let validated = latest.and_then(|latest| validated_v2_by_task.get(&latest.task_id));
        let evidence_consistent = if header.completion_protocol_version == 2 {
            latest.is_none() || validated.is_some()
        } else {
            latest.is_none() || run.is_some()
        };
        Some(WorkflowRecoveryPlanIdentity {
            node_id: binding.node_id.clone(),
            work_unit_key: binding.work_unit_key.clone(),
            agent_type: binding.agent_type.clone(),
            profile_id: binding.profile_id.clone(),
            active: binding.retired_revision.is_none(),
            observed: binding.is_observed,
            latest_task_id: latest.map(|latest| latest.task_id.clone()),
            latest_status: run.map(|run| run.status.clone()),
            summary_validated: latest.is_some_and(|latest| latest.summary_validated),
            completion_state: run.and_then(|run| run.completion_state.clone()),
            completion_outcome: validated.map(|value| value.evidence.intent.outcome),
            completion_evidence_validated: validated.is_some_and(|value| value.evidence_validated),
            evidence_scope_digest: validated
                .map(|value| value.evidence.evidence_scope_digest.clone()),
            has_open_completion_attention: latest
                .is_some_and(|latest| open_completion_task_ids.contains(&latest.task_id)),
            artifact_digest: if header.completion_protocol_version == 2 {
                validated.map(|value| value.evidence.artifact.digest().to_string())
            } else {
                latest.and_then(|latest| latest.artifact_digest.clone())
            },
            gate_id: latest.and_then(|latest| latest.gate_id.clone()),
            gate_cycle: latest.and_then(|latest| latest.gate_cycle),
            reviewed_task_id: latest.and_then(|latest| latest.reviewed_task_id.clone()),
            evidence_consistent,
        })
    };

    let plan_gate = normalized.as_ref().and_then(|document| {
        document
            .gates
            .iter()
            .find(|gate| gate.gate_kind == DocumentGateKind::Plan)
    });
    let active_plan_author = normalized.as_ref().and_then(|document| {
        document
            .nodes
            .iter()
            .find(|node| {
                node.kind == ManifestNodeKind::WorkUnit
                    && node.phase_id.as_deref() == Some("plan")
                    && node.role == Some(ManifestNodeRole::Author)
            })
            .and_then(|node| plan_identity(&node.id))
    });
    let required_plan_reviewers = plan_gate
        .map(|gate| {
            gate.required_reviewer_node_ids
                .iter()
                .filter_map(|node_id| plan_identity(node_id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let settlements = delegation_workflow_gate_settlement::Entity::find()
        .filter(
            delegation_workflow_gate_settlement::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .order_by_desc(delegation_workflow_gate_settlement::Column::GateCycle)
        .all(conn)
        .await
        .map_err(db_err)?;
    let current_plan_state = if header.completion_protocol_version == 2 {
        if let Some(gate) = plan_gate {
            delegation_workflow_gate_state::Entity::find_by_id((
                header.workflow_id.clone(),
                gate.id.clone(),
            ))
            .one(conn)
            .await
            .map_err(db_err)?
        } else {
            None
        }
    } else {
        None
    };
    let current_v2_plan_identity = current_plan_state.as_ref().and_then(|state| {
        let gate = plan_gate?;
        if required_plan_reviewers.len() != gate.required_reviewer_node_ids.len() {
            return None;
        }
        let mut node_ids = Vec::with_capacity(required_plan_reviewers.len());
        let mut task_ids = Vec::with_capacity(required_plan_reviewers.len());
        let mut scope_digests = Vec::with_capacity(required_plan_reviewers.len());
        for reviewer in &required_plan_reviewers {
            if !reviewer.completion_evidence_validated {
                return None;
            }
            node_ids.push(reviewer.node_id.clone());
            task_ids.push(reviewer.latest_task_id.clone()?);
            scope_digests.push(reviewer.evidence_scope_digest.clone()?);
        }
        let identity = V2GateEvidenceIdentity::new(
            state.gate_lineage.clone(),
            state.current_review_round,
            node_ids,
            task_ids,
            scope_digests,
        )?;
        (identity.required_node_ids == canonical_string_set(&gate.required_reviewer_node_ids))
            .then_some(identity)
    });
    let latest_plan_settlement = settlements.iter().find(|settlement| {
        if header.completion_protocol_version == 2 {
            plan_gate.is_some_and(|gate| settlement.gate_id == gate.id)
                && settlement.gate_lineage.is_some()
        } else {
            settlement.review_scope.is_some()
        }
    });
    let current_plan_settlement = if header.completion_protocol_version == 2 {
        plan_gate.and_then(|gate| {
            current_v2_plan_identity.as_ref().and_then(|identity| {
                select_current_v2_plan_settlement(&settlements, &gate.id, identity)
            })
        })
    } else {
        settlements.iter().find(|settlement| {
            settlement.review_scope.is_some()
                && settlement.content_fingerprint == header.plan_fingerprint
        })
    };
    let project_plan_gate = |settlement: &delegation_workflow_gate_settlement::Model| {
        let persisted_evidence = load_persisted_plan_evidence(settlement);
        let parsed_reviewers = if header.completion_protocol_version == 2 {
            settlement.required_node_set_json.as_deref()
        } else {
            settlement.required_reviewer_node_ids_json.as_deref()
        }
        .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok());
        let ledger_valid = settlement
            .finding_ledger_json
            .as_deref()
            .is_some_and(|json| serde_json::from_str::<serde_json::Value>(json).is_ok());
        let required_reviewer_node_ids = parsed_reviewers.clone().unwrap_or_default();
        let reviewer_evidence_count = required_plan_reviewers
            .iter()
            .filter(|reviewer| {
                reviewer.latest_status == Some(DelegationRunStatus::Completed)
                    && if header.completion_protocol_version == 2 {
                        reviewer.completion_state == Some(CompletionState::Resolved)
                            && reviewer.completion_evidence_validated
                    } else {
                        reviewer.summary_validated
                    }
            })
            .count();
        let v2_evidence_consistent = current_v2_plan_identity
            .as_ref()
            .is_some_and(|identity| identity.matches_settlement(settlement));
        WorkflowRecoveryPlanGateEvidence {
            gate_id: settlement.gate_id.clone(),
            gate_cycle: settlement.gate_cycle,
            outcome: settlement.outcome.clone(),
            content_fingerprint: settlement.content_fingerprint.clone(),
            evidence_scope_digest: settlement.evidence_scope_digest.clone(),
            gate_lineage: settlement.gate_lineage.clone(),
            review_round: settlement.review_round,
            v2_evidence_consistent,
            plan_reviewer_ranks_v2: settlement
                .plan_round_state_v2_json
                .as_deref()
                .and_then(parse_plan_reviewer_ranks_v2),
            critical_count: settlement.critical_count.unwrap_or(-1),
            important_count: settlement.important_count.unwrap_or(-1),
            minor_count: settlement.minor_count.unwrap_or(-1),
            next_action: settlement
                .next_action
                .as_ref()
                .map(plan_next_action_from_db),
            covered_author_task_id: settlement.covered_author_task_id.clone(),
            covered_plan_digest: settlement.covered_plan_digest.clone(),
            required_reviewer_node_ids,
            reviewer_evidence_count,
            evidence_consistent: if header.completion_protocol_version == 2 {
                v2_evidence_consistent
            } else {
                persisted_evidence.as_ref().is_ok_and(|evidence| {
                    validate_plan_outcome(&settlement.outcome, &evidence.state).is_ok()
                }) && parsed_reviewers.is_some()
                    && ledger_valid
                    && settlement.critical_count.is_some_and(|count| count >= 0)
                    && settlement.important_count.is_some_and(|count| count >= 0)
                    && settlement.minor_count.is_some_and(|count| count >= 0)
                    && settlement.next_action.is_some()
                    && settlement.covered_author_task_id.is_some()
                    && settlement.covered_plan_digest.is_some()
            },
            lineage_reset_consumed: settlement.lineage_reset_authorization_id.is_some(),
        }
    };
    let latest_plan_gate = latest_plan_settlement.map(&project_plan_gate);
    let current_plan_gate = current_plan_settlement.map(project_plan_gate);

    if header.structural_revision <= 0
        || header.structural_revision > header.active_manifest_revision
        || header
            .supersedes_approved_revision
            .is_some_and(|revision| revision > header.active_manifest_revision)
        || header
            .block_source_manifest_revision
            .is_some_and(|revision| revision > header.active_manifest_revision)
    {
        contradictory_durable_state = true;
    }
    let block_cause = match WorkflowBlockCause::from_db(header.block_cause_code.as_deref()) {
        Ok(cause) => cause,
        Err(_) => {
            contradictory_durable_state = true;
            WorkflowBlockCause::DurableStateInconsistent
        }
    };
    let plan_lineage_reset_pending = latest_plan_gate.as_ref().is_some_and(|gate| {
        gate.next_action == Some(PlanReviewNextAction::UserDecisionRequired)
            && !gate.lineage_reset_consumed
    });

    let snapshot = WorkflowRecoverySnapshot {
        workflow_id: header.workflow_id.clone(),
        parent_conversation_id: header.parent_conversation_id,
        workflow_kind: header.workflow_kind.clone(),
        schema_version: u64::try_from(header.schema_version).unwrap_or_default(),
        capability_version: header.capability_version.clone(),
        completion_protocol_version: header.completion_protocol_version,
        header_state,
        active_manifest_revision: u64::try_from(header.active_manifest_revision)
            .unwrap_or_default(),
        structural_revision: u64::try_from(header.structural_revision).unwrap_or_default(),
        active_manifest_revision_kind,
        active_manifest_source_revision,
        supersedes_approved_revision: header
            .supersedes_approved_revision
            .and_then(|value| u64::try_from(value).ok()),
        active_manifest_digest: revision.as_ref().map(|row| row.document_digest.clone()),
        manifest_state,
        normalized_manifest_state,
        header_manifest_state_match,
        active_manifest_valid,
        fingerprints_valid,
        design_fingerprint: header.design_fingerprint.clone(),
        plan_fingerprint: header.plan_fingerprint.clone(),
        plan_target_rel_path: normalized
            .as_ref()
            .map(|document| document.plan_target_rel_path.clone())
            .unwrap_or_default(),
        design: normalized.as_ref().and_then(|document| {
            document
                .design
                .as_ref()
                .map(|design| WorkflowRecoveryDocumentIdentity {
                    rel_path: design.rel_path.clone(),
                    digest: design.digest.clone(),
                })
        }),
        plan: normalized.as_ref().and_then(|document| {
            document
                .plan
                .as_ref()
                .map(|plan| WorkflowRecoveryDocumentIdentity {
                    rel_path: plan.rel_path.clone(),
                    digest: plan.digest.clone(),
                })
        }),
        current_plan_gate_id: plan_gate.map(|gate| gate.id.clone()),
        active_plan_author,
        required_plan_reviewers,
        latest_plan_gate,
        current_plan_gate,
        binding_lifecycle,
        active_runs,
        frozen_task_cohorts,
        binding_evidence_consistent,
        latest_run_supersession_valid,
        contradictory_durable_state,
        block_cause,
        block_source_manifest_revision: header
            .block_source_manifest_revision
            .and_then(|value| u64::try_from(value).ok()),
        plan_lineage_reset_pending,
        displayed_reset_reason_hash: displayed_reset_reason.map(hash_displayed_reset_reason),
    };
    Ok(LoadedWorkflowRecoverySnapshot {
        snapshot,
        normalized: if active_manifest_valid {
            normalized
        } else {
            None
        },
    })
}

fn workflow_recovery_lineage_valid(
    header: &delegation_workflow::Model,
    revisions: &[delegation_workflow_manifest_revision::Model],
    active_normalized: Option<&NormalizedManifest>,
) -> bool {
    let Some(active_normalized) = active_normalized else {
        return false;
    };
    let Ok(active_manifest_revision) = usize::try_from(header.active_manifest_revision) else {
        return false;
    };
    if active_manifest_revision == 0 || revisions.len() != active_manifest_revision {
        return false;
    }

    let mut validated = Vec::with_capacity(revisions.len());
    for (index, revision) in revisions.iter().enumerate() {
        let expected_revision = i64::try_from(index + 1).unwrap_or(i64::MAX);
        if revision.workflow_id != header.workflow_id
            || revision.manifest_revision != expected_revision
        {
            return false;
        }
        let Ok(kind) = ManifestRevisionKind::from_db(revision.revision_kind.as_deref()) else {
            return false;
        };
        let Some(normalized) = parse_valid_recovery_revision(revision) else {
            return false;
        };
        validated.push((kind, normalized));
    }

    let mut previous_publication_plan_fingerprint: Option<String> = None;
    let mut authoritative_structural_revision = None;
    let mut current_publication = None;
    for (index, ((kind, normalized), revision)) in
        validated.iter().zip(revisions.iter()).enumerate()
    {
        match kind {
            ManifestRevisionKind::Publication => {
                let plan_fingerprint = plan_fingerprint_hash(normalized);
                if previous_publication_plan_fingerprint.as_ref() != Some(&plan_fingerprint) {
                    authoritative_structural_revision = Some(revision.manifest_revision);
                }
                previous_publication_plan_fingerprint = Some(plan_fingerprint);
                current_publication = Some((revision, normalized));
            }
            ManifestRevisionKind::StateOnly => {
                if index == 0
                    || revision.source_manifest_revision
                        != Some(revisions[index - 1].manifest_revision)
                    || !manifests_equal_except_state_authority(&validated[index - 1].1, normalized)
                {
                    return false;
                }
            }
        }
    }

    let Some((current_publication_revision, current_publication_normalized)) = current_publication
    else {
        return false;
    };
    let Some(authoritative_structural_revision) = authoritative_structural_revision else {
        return false;
    };
    if header.structural_revision != authoritative_structural_revision
        || header.structural_revision <= 0
        || header.structural_revision > current_publication_revision.manifest_revision
        || !manifests_equal_except_state_authority(
            current_publication_normalized,
            active_normalized,
        )
        || design_fingerprint_hash(current_publication_normalized) != header.design_fingerprint
        || plan_fingerprint_hash(current_publication_normalized) != header.plan_fingerprint
        || plan_fingerprint_hash(active_normalized) != header.plan_fingerprint
    {
        return false;
    }

    let structural_index = usize::try_from(authoritative_structural_revision - 1).ok();
    structural_index.is_some_and(|index| {
        validated.get(index).is_some_and(|(kind, normalized)| {
            *kind == ManifestRevisionKind::Publication
                && plan_fingerprint_hash(normalized) == header.plan_fingerprint
        })
    })
}

fn parse_valid_recovery_revision(
    revision: &delegation_workflow_manifest_revision::Model,
) -> Option<NormalizedManifest> {
    if sha256_hex(revision.document_json.as_bytes()) != revision.document_digest {
        return None;
    }
    let manifest_state = parse_manifest_state(&revision.manifest_state)?;
    let document = serde_json::from_str::<ManifestDocument>(&revision.document_json).ok()?;
    let normalized = validate_manifest_document(&document).ok()?;
    (normalized.workflow_state == manifest_state).then_some(normalized)
}

fn project_invalid_manifest_recovery_index(
    header: &delegation_workflow::Model,
    snapshot: &WorkflowRecoverySnapshot,
    recovery: super::recovery_policy::WorkflowRecoveryProjection,
    completion_protocol: super::types::CompletionProtocolWorkflowProjection,
) -> WorkflowStateIndexDto {
    WorkflowStateIndexDto {
        workflow_id: header.workflow_id.clone(),
        parent_conversation_id: header.parent_conversation_id,
        workflow_kind: header.workflow_kind.clone(),
        capability_version: header.capability_version.clone(),
        publication_token: header.publication_token.clone(),
        workflow_state: workflow_state_to_manifest(header.workflow_state.clone()),
        manifest_revision: u64::try_from(header.active_manifest_revision).unwrap_or_default(),
        graph_revision: u64::try_from(header.graph_revision).unwrap_or_default(),
        schema_version: u64::try_from(header.schema_version).unwrap_or_default(),
        plan_target_rel_path: snapshot.plan_target_rel_path.clone(),
        risk_policy_version: String::new(),
        completion_protocol,
        completion: None,
        recovery: Some(recovery),
        detail: super::state_dto::WorkflowStateDetail::Index,
        inline_findings: false,
        payload_truncated: true,
        omitted: vec!["invalid_active_manifest".into()],
        evidence_truncated: true,
        design: snapshot
            .design
            .as_ref()
            .map(|document| super::types::DocumentRef {
                rel_path: document.rel_path.clone(),
                digest: document.digest.clone(),
            }),
        plan: snapshot
            .plan
            .as_ref()
            .map(|document| super::types::DocumentRef {
                rel_path: document.rel_path.clone(),
                digest: document.digest.clone(),
            }),
        gates: Vec::new(),
        latest_plan_review: None,
        nodes: Vec::new(),
        task_policies: Vec::new(),
        actionable_task_routes: Vec::new(),
        omission_state: super::state_dto::WorkflowIndexOmissionState { nodes: Vec::new() },
    }
}

fn parse_manifest_state(value: &str) -> Option<ManifestWorkflowState> {
    Some(match value {
        "skeleton" => ManifestWorkflowState::Skeleton,
        "estimated" => ManifestWorkflowState::Estimated,
        "approved" => ManifestWorkflowState::Approved,
        "blocked" => ManifestWorkflowState::Blocked,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn db_err(e: sea_orm::DbErr) -> WorkflowStoreError {
    WorkflowStoreError::Persistence(e.to_string())
}

pub(crate) fn map_completion_protocol_header_db_error(error: sea_orm::DbErr) -> WorkflowStoreError {
    match error {
        sea_orm::DbErr::Type(message) => {
            WorkflowStoreError::UnsupportedCompletionProtocolHeader(message)
        }
        error @ sea_orm::DbErr::TryIntoErr { .. } => {
            WorkflowStoreError::UnsupportedCompletionProtocolHeader(error.to_string())
        }
        other => WorkflowStoreError::Persistence(other.to_string()),
    }
}

pub async fn load_completion_protocol_header<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
) -> Result<Option<(i64, delegation_workflow::CompletionProtocolMode)>, WorkflowStoreError> {
    delegation_workflow::Entity::find_by_id(workflow_id.to_string())
        .select_only()
        .column(delegation_workflow::Column::CompletionProtocolVersion)
        .column(delegation_workflow::Column::CompletionProtocolMode)
        .into_tuple::<(i64, delegation_workflow::CompletionProtocolMode)>()
        .one(conn)
        .await
        .map_err(map_completion_protocol_header_db_error)
}

async fn load_workflow_id_by_parent_kind<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
) -> Result<Option<String>, WorkflowStoreError> {
    delegation_workflow::Entity::find()
        .select_only()
        .column(delegation_workflow::Column::WorkflowId)
        .filter(delegation_workflow::Column::ParentConversationId.eq(parent_conversation_id))
        .filter(delegation_workflow::Column::WorkflowKind.eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY))
        .into_tuple::<String>()
        .one(conn)
        .await
        .map_err(db_err)
}

pub async fn load_completion_protocol_for_conversation(
    db: &AppDatabase,
    conversation_id: i32,
) -> Result<Option<(i64, delegation_workflow::CompletionProtocolMode)>, WorkflowStoreError> {
    let mut workflow_ids = BTreeSet::new();
    if let Some(workflow_id) = load_workflow_id_by_parent_kind(&db.conn, conversation_id).await? {
        workflow_ids.insert(workflow_id);
    }

    let task_ids = delegation_task_run::Entity::find()
        .select_only()
        .column(delegation_task_run::Column::TaskId)
        .filter(delegation_task_run::Column::ChildConversationId.eq(conversation_id))
        .into_tuple::<String>()
        .all(&db.conn)
        .await
        .map_err(db_err)?;
    if !task_ids.is_empty() {
        workflow_ids.extend(
            delegation_workflow_run_binding::Entity::find()
                .select_only()
                .column(delegation_workflow_run_binding::Column::WorkflowId)
                .filter(delegation_workflow_run_binding::Column::TaskId.is_in(task_ids))
                .into_tuple::<String>()
                .all(&db.conn)
                .await
                .map_err(db_err)?,
        );
    }

    let mut allowed = None;
    let mut legacy = None;
    let mut unsupported = None;
    for workflow_id in workflow_ids {
        let header = load_completion_protocol_header(&db.conn, &workflow_id)
            .await?
            .ok_or_else(|| WorkflowStoreError::NotFound(workflow_id.clone()))?;
        match require_v2_mutation_for_connection(&db.conn, header.0, &header.1).await {
            Ok(()) => allowed.get_or_insert(header),
            Err(WorkflowStoreError::LegacyCompletionProtocolReadOnly) => {
                legacy.get_or_insert(header)
            }
            Err(WorkflowStoreError::UnsupportedCompletionProtocol { .. }) => {
                unsupported.get_or_insert(header)
            }
            Err(error) => return Err(error),
        };
    }

    Ok(unsupported.or(legacy).or(allowed))
}

async fn require_stored_v2_header<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
) -> Result<(), WorkflowStoreError> {
    let (version, mode) = load_completion_protocol_header(conn, workflow_id)
        .await?
        .ok_or_else(|| WorkflowStoreError::NotFound(workflow_id.to_string()))?;
    require_v2_mutation_for_connection(conn, version, &mode).await
}

async fn require_owned_stored_v2_header<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    expected_parent: i32,
) -> Result<(), WorkflowStoreError> {
    let actual_parent = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
        .select_only()
        .column(delegation_workflow::Column::ParentConversationId)
        .into_tuple::<i32>()
        .one(conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(workflow_id.to_string()))?;
    if actual_parent != expected_parent {
        return Err(WorkflowStoreError::CrossParent {
            workflow_id: workflow_id.to_string(),
            expected_parent,
            actual_parent,
        });
    }
    require_stored_v2_header(conn, workflow_id).await
}

#[cfg(any(test, feature = "test-utils"))]
async fn ensure_parent_exists(
    db: &AppDatabase,
    parent_conversation_id: i32,
) -> Result<(), WorkflowStoreError> {
    let found = conversation::Entity::find_by_id(parent_conversation_id)
        .one(&db.conn)
        .await
        .map_err(db_err)?;
    if found.is_none() {
        return Err(WorkflowStoreError::ParentNotFound(parent_conversation_id));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-utils"))]
async fn load_by_publication_token_txn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    token: &str,
    parent_conversation_id: i32,
) -> Result<Option<delegation_workflow::Model>, WorkflowStoreError> {
    let workflow_id = delegation_workflow::Entity::find()
        .select_only()
        .column(delegation_workflow::Column::WorkflowId)
        .filter(delegation_workflow::Column::PublicationToken.eq(token.to_string()))
        .into_tuple::<String>()
        .one(conn)
        .await
        .map_err(db_err)?;
    let Some(workflow_id) = workflow_id else {
        return Ok(None);
    };
    require_owned_stored_v2_header(conn, &workflow_id, parent_conversation_id).await?;
    delegation_workflow::Entity::find_by_id(&workflow_id)
        .one(conn)
        .await
        .map_err(db_err)
}

#[cfg(any(test, feature = "test-utils"))]
async fn load_active_manifest_digest_txn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    revision: i64,
) -> Result<Option<String>, WorkflowStoreError> {
    let row = delegation_workflow_manifest_revision::Entity::find_by_id((
        workflow_id.to_string(),
        revision,
    ))
    .one(conn)
    .await
    .map_err(db_err)?;
    Ok(row.map(|r| r.document_digest))
}

async fn load_active_manifest_document_txn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    revision: i64,
) -> Result<ManifestDocument, WorkflowStoreError> {
    let row = delegation_workflow_manifest_revision::Entity::find_by_id((
        workflow_id.to_string(),
        revision,
    ))
    .one(conn)
    .await
    .map_err(db_err)?
    .ok_or_else(|| {
        WorkflowStoreError::NotFound(format!(
            "manifest revision {revision} for workflow {workflow_id}"
        ))
    })?;
    serde_json::from_str(&row.document_json)
        .map_err(|e| WorkflowStoreError::Persistence(format!("parse manifest json: {e}")))
}

/// Append an immutable revision that changes workflow state and provenance only.
pub async fn append_state_only_revision_txn(
    txn: &DatabaseTransaction,
    header: &delegation_workflow::Model,
    request: StateOnlyRevisionRequest<'_>,
    now: DateTime<Utc>,
) -> Result<StateOnlyRevisionResult, WorkflowStoreError> {
    require_stored_v2_header(txn, &header.workflow_id).await?;
    let current = delegation_workflow::Entity::find_by_id(header.workflow_id.clone())
        .one(txn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(header.workflow_id.clone()))?;
    require_v2_mutation_for_connection(
        txn,
        current.completion_protocol_version,
        &current.completion_protocol_mode,
    )
    .await?;
    if current.active_manifest_revision != header.active_manifest_revision {
        return Err(WorkflowStoreError::StaleManifestRevision {
            expected: header.active_manifest_revision as u64,
            current: current.active_manifest_revision as u64,
        });
    }
    if current.graph_revision != header.graph_revision {
        return Err(WorkflowStoreError::StaleGraphRevision {
            expected: header.graph_revision as u64,
            current: current.graph_revision as u64,
        });
    }

    let mut document = load_active_manifest_document_txn(
        txn,
        &current.workflow_id,
        current.active_manifest_revision,
    )
    .await?;
    validate_manifest_document(&document)?;
    if document.workflow_state != workflow_state_to_manifest(current.workflow_state.clone()) {
        return Err(WorkflowStoreError::Persistence(
            "durable workflow header and active manifest state disagree".into(),
        ));
    }

    let leaving_blocked = current.workflow_state == WorkflowState::Blocked
        && request.target_state != ManifestWorkflowState::Blocked;
    if leaving_blocked && request.recovery_authorization_id.is_none() {
        return Err(WorkflowStoreError::workflow_recovery_required());
    }

    let block_cause = if request.target_state == ManifestWorkflowState::Blocked {
        let cause = WorkflowBlockCause::from_transition_reason(request.transition_reason_code)
            .map_err(WorkflowStoreError::Persistence)?;
        if cause == WorkflowBlockCause::LegacyUnknown {
            return Err(WorkflowStoreError::Persistence(
                "new blocked revisions cannot use legacy_unknown".into(),
            ));
        }
        Some(cause)
    } else {
        None
    };

    document.workflow_state = request.target_state;
    let document_json = serde_json::to_string(&document)
        .map_err(|error| WorkflowStoreError::Persistence(format!("serialize manifest: {error}")))?;
    let document_digest = sha256_hex(document_json.as_bytes());
    let source_manifest_revision = current.active_manifest_revision;
    let next_manifest_revision = source_manifest_revision + 1;

    delegation_workflow_manifest_revision::ActiveModel {
        workflow_id: Set(current.workflow_id.clone()),
        manifest_revision: Set(next_manifest_revision),
        manifest_state: Set(manifest_state_str(request.target_state).into()),
        document_json: Set(document_json),
        document_digest: Set(document_digest),
        revision_kind: Set(Some(ManifestRevisionKind::StateOnly.as_str().into())),
        source_manifest_revision: Set(Some(source_manifest_revision)),
        recovery_authorization_id: Set(request.recovery_authorization_id.map(str::to_string)),
        transition_reason_code: Set(Some(request.transition_reason_code.to_string())),
        consumer_correlation_id: Set(request.consumer_correlation_id.map(str::to_string)),
        graph_revision: Set(Some(current.graph_revision)),
        recovery_source_state_fingerprint: Set(request
            .recovery_source_state_fingerprint
            .map(str::to_string)),
        recovery_risk_class: Set(request.recovery_risk_class.map(str::to_string)),
        created_at: Set(now),
    }
    .insert(txn)
    .await
    .map_err(db_err)?;

    let mut update = delegation_workflow::ActiveModel {
        active_manifest_revision: Set(next_manifest_revision),
        workflow_state: Set(manifest_state_to_db(request.target_state)),
        updated_at: Set(now),
        ..Default::default()
    };
    if let Some(cause) = block_cause {
        update.block_cause_code = Set(Some(cause.as_str().into()));
        update.block_source_manifest_revision = Set(Some(source_manifest_revision));
    } else if leaving_blocked {
        update.block_cause_code = Set(None);
        update.block_source_manifest_revision = Set(None);
    }
    let changed = delegation_workflow::Entity::update_many()
        .set(update)
        .filter(delegation_workflow::Column::WorkflowId.eq(current.workflow_id.clone()))
        .filter(delegation_workflow::Column::ActiveManifestRevision.eq(source_manifest_revision))
        .filter(delegation_workflow::Column::GraphRevision.eq(current.graph_revision))
        .exec(txn)
        .await
        .map_err(db_err)?;
    if changed.rows_affected != 1 {
        return Err(WorkflowStoreError::StaleManifestRevision {
            expected: source_manifest_revision as u64,
            current: current.active_manifest_revision as u64,
        });
    }

    if inject_publish_persistence_failure() {
        return Err(WorkflowStoreError::Persistence(
            "injected state-only persistence failure".into(),
        ));
    }

    Ok(StateOnlyRevisionResult {
        workflow_id: current.workflow_id,
        manifest_revision: next_manifest_revision as u64,
        source_manifest_revision: source_manifest_revision as u64,
        graph_revision: current.graph_revision as u64,
        workflow_state: request.target_state,
        block_cause,
    })
}

/// Authoritative typed entry boundary for placing a workflow in blocked state.
pub async fn append_workflow_block_revision_txn(
    txn: &DatabaseTransaction,
    header: &delegation_workflow::Model,
    request: WorkflowBlockEntryRequest<'_>,
    now: DateTime<Utc>,
) -> Result<StateOnlyRevisionResult, WorkflowStoreError> {
    if request.cause == WorkflowBlockCause::LegacyUnknown {
        return Err(WorkflowStoreError::Persistence(
            "new blocked revisions cannot use legacy_unknown".into(),
        ));
    }
    append_state_only_revision_txn(
        txn,
        header,
        StateOnlyRevisionRequest {
            target_state: ManifestWorkflowState::Blocked,
            transition_reason_code: request.cause.as_str(),
            recovery_authorization_id: None,
            consumer_correlation_id: request.consumer_correlation_id,
            recovery_source_state_fingerprint: None,
            recovery_risk_class: None,
        },
        now,
    )
    .await
}

/// Internal marker: winner not visible in this txn snapshot; outer must re-read.
#[cfg(any(test, feature = "test-utils"))]
const TOKEN_RACE_RECLASSIFY_MARKER: &str = "__workflow_publication_token_race_reclassify__";

#[cfg(any(test, feature = "test-utils"))]
fn is_token_race_reclassify_marker(err: &WorkflowStoreError) -> bool {
    matches!(
        err,
        WorkflowStoreError::Persistence(s) if s == TOKEN_RACE_RECLASSIFY_MARKER
    )
}

#[cfg(any(test, feature = "test-utils"))]
fn is_unique_constraint(err: &sea_orm::DbErr) -> bool {
    let s = err.to_string();
    s.contains("UNIQUE")
        || s.contains("unique")
        || s.contains("idx_dw_publication_token")
        || s.contains("idx_dw_parent_kind")
        || s.contains("2067") // SQLITE_CONSTRAINT_UNIQUE
}

#[cfg(any(test, feature = "test-utils"))]
fn is_busy_or_snapshot_err_str(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("busy")
        || lower.contains("snapshot")
        || lower.contains("locked")
        || lower.contains("database is locked")
        || s.contains("5") && lower.contains("sqlite") // SQLITE_BUSY
}

#[cfg(any(test, feature = "test-utils"))]
fn is_token_race_db_err(err: &sea_orm::DbErr) -> bool {
    is_unique_constraint(err) || is_busy_or_snapshot_err_str(&err.to_string())
}

/// Core publish body (runs inside a single write transaction).
#[cfg(any(test, feature = "test-utils"))]
async fn publish_in_txn(
    txn: &sea_orm::DatabaseTransaction,
    parent_conversation_id: i32,
    normalized: &NormalizedManifest,
    document_digest: &str,
    now: chrono::DateTime<Utc>,
    protocol_version: i64,
    protocol_mode: delegation_workflow::CompletionProtocolMode,
) -> Result<PublishResult, WorkflowStoreError> {
    // --- re-read by publication_token (inside write txn) -------------------
    if let Some(by_token) =
        load_by_publication_token_txn(txn, &normalized.publication_token, parent_conversation_id)
            .await?
    {
        if by_token.parent_conversation_id != parent_conversation_id {
            return Err(WorkflowStoreError::CrossParent {
                workflow_id: by_token.workflow_id.clone(),
                expected_parent: parent_conversation_id,
                actual_parent: by_token.parent_conversation_id,
            });
        }
        require_v2_mutation_for_connection(
            txn,
            by_token.completion_protocol_version,
            &by_token.completion_protocol_mode,
        )
        .await?;
        let active_digest = load_active_manifest_digest_txn(
            txn,
            &by_token.workflow_id,
            by_token.active_manifest_revision,
        )
        .await?;
        let ordinary_unblock_attempt = by_token.workflow_state == WorkflowState::Blocked
            && normalized.workflow_state != ManifestWorkflowState::Blocked;
        let replay_digest = if ordinary_unblock_attempt {
            manifest_document_digest_with_state(normalized, ManifestWorkflowState::Blocked)?
        } else {
            document_digest.to_string()
        };
        if active_digest.as_deref() == Some(replay_digest.as_str()) {
            let by_token = if ordinary_unblock_attempt {
                by_token
            } else {
                stamp_workflow_capability_version(txn, by_token).await?
            };
            let state = workflow_state_to_manifest(by_token.workflow_state.clone());
            return publish_result(
                by_token.workflow_id,
                by_token.active_manifest_revision as u64,
                by_token.graph_revision as u64,
                state,
                true,
                false,
                by_token.block_cause_code.as_deref(),
                by_token.block_source_manifest_revision,
            );
        }
        let is_explicit_update = normalized
            .workflow_id
            .as_deref()
            .is_some_and(|id| id == by_token.workflow_id);
        if !is_explicit_update {
            if normalized.workflow_id.is_some() {
                if let Some(binding) =
                    retired_reactivation_candidate(txn, &by_token.workflow_id, normalized).await?
                {
                    return Err(binding_identity_conflict(&binding));
                }
            }
            // Create / bare replay with different digest → B8 mismatch.
            return Err(WorkflowStoreError::PublicationTokenMismatch {
                publication_token: normalized.publication_token.clone(),
                workflow_id: by_token.workflow_id,
            });
        }
        // Explicit CAS update with same token continues into parent/CAS path.
    }

    let by_parent = load_by_parent_kind_txn(txn, parent_conversation_id).await?;
    if let Some(existing) = by_parent.as_ref() {
        require_v2_mutation_for_connection(
            txn,
            existing.completion_protocol_version,
            &existing.completion_protocol_mode,
        )
        .await?;
    }

    let (workflow_id, next_manifest_rev, next_graph_rev, prior_header) =
        match (&normalized.workflow_id, by_parent) {
            (None, Some(existing)) => {
                // Parent already has a workflow. Same token → B8 digest classify
                // (not Conflict). Different token → Conflict.
                if existing.publication_token == normalized.publication_token {
                    return classify_existing_header(
                        txn,
                        existing,
                        parent_conversation_id,
                        &normalized.publication_token,
                        document_digest,
                    )
                    .await;
                }
                return Err(WorkflowStoreError::PublicationTokenConflict {
                    existing_workflow_id: existing.workflow_id,
                });
            }
            (None, None) => {
                let id = uuid::Uuid::new_v4().to_string();
                (id, 1_i64, 1_i64, None)
            }
            (Some(id), None) => {
                return Err(WorkflowStoreError::NotFound(id.clone()));
            }
            (Some(id), Some(existing)) => {
                if existing.workflow_id != *id {
                    return Err(WorkflowStoreError::PublicationTokenConflict {
                        existing_workflow_id: existing.workflow_id,
                    });
                }
                if existing.parent_conversation_id != parent_conversation_id {
                    return Err(WorkflowStoreError::CrossParent {
                        workflow_id: existing.workflow_id.clone(),
                        expected_parent: parent_conversation_id,
                        actual_parent: existing.parent_conversation_id,
                    });
                }
                let expected = normalized.expected_manifest_revision.ok_or(
                    WorkflowStoreError::StaleManifestRevision {
                        expected: 0,
                        current: existing.active_manifest_revision as u64,
                    },
                )?;
                if expected != existing.active_manifest_revision as u64 {
                    return Err(WorkflowStoreError::StaleManifestRevision {
                        expected,
                        current: existing.active_manifest_revision as u64,
                    });
                }
                let active_digest = load_active_manifest_digest_txn(
                    txn,
                    &existing.workflow_id,
                    existing.active_manifest_revision,
                )
                .await?;
                if active_digest.as_deref() == Some(document_digest) {
                    let existing = stamp_workflow_capability_version(txn, existing).await?;
                    let state = workflow_state_to_manifest(existing.workflow_state.clone());
                    return publish_result(
                        existing.workflow_id.clone(),
                        existing.active_manifest_revision as u64,
                        existing.graph_revision as u64,
                        state,
                        true,
                        false,
                        existing.block_cause_code.as_deref(),
                        existing.block_source_manifest_revision,
                    );
                }
                let next_m = existing.active_manifest_revision + 1;
                let next_g = existing.graph_revision + 1;
                (existing.workflow_id.clone(), next_m, next_g, Some(existing))
            }
        };

    let prior_normalized = if let Some(prior) = prior_header.as_ref() {
        let document = load_active_manifest_document_txn(
            txn,
            &prior.workflow_id,
            prior.active_manifest_revision,
        )
        .await?;
        Some(validate_manifest_document(&document)?)
    } else {
        None
    };

    let sticky_blocked = prior_header
        .as_ref()
        .is_some_and(|prior| prior.workflow_state == WorkflowState::Blocked)
        && normalized.workflow_state != ManifestWorkflowState::Blocked;
    if sticky_blocked
        && prior_normalized
            .as_ref()
            .is_some_and(|prior| manifests_equal_except_state_authority(prior, normalized))
    {
        let prior = prior_header
            .as_ref()
            .expect("sticky state has prior header");
        return publish_result(
            prior.workflow_id.clone(),
            prior.active_manifest_revision as u64,
            prior.graph_revision as u64,
            ManifestWorkflowState::Blocked,
            false,
            false,
            prior.block_cause_code.as_deref(),
            prior.block_source_manifest_revision,
        );
    }

    // A8: material Plan structure change forces demotion when previously
    // approved or already demoted (supersedes set). Design fingerprint is
    // independent so Design settlements survive plan-only rewrites.
    let mut effective_state = if sticky_blocked {
        ManifestWorkflowState::Blocked
    } else {
        normalized.workflow_state
    };
    let mut supersedes =
        compute_supersedes(prior_header.as_ref(), effective_state, next_manifest_rev);
    let next_design_fp = design_fingerprint_hash(normalized);
    let next_plan_fp = plan_fingerprint_hash(normalized);
    let mut next_structural_rev = next_manifest_rev;
    if let Some(prior) = prior_header.as_ref() {
        let plan_changed = if prior.plan_fingerprint.is_empty() {
            prior_normalized
                .as_ref()
                .is_none_or(|prior| plan_structure_changed(prior, normalized))
        } else {
            prior.plan_fingerprint != next_plan_fp
        };
        if plan_changed {
            next_structural_rev = next_manifest_rev;
            let demote = prior.workflow_state == WorkflowState::Approved
                || prior.supersedes_approved_revision.is_some();
            if demote {
                if (matches!(
                    effective_state,
                    ManifestWorkflowState::Approved | ManifestWorkflowState::Estimated
                ) || prior.workflow_state == WorkflowState::Approved)
                    && effective_state != ManifestWorkflowState::Blocked
                {
                    effective_state = ManifestWorkflowState::Estimated;
                }
                supersedes = Some(prior.active_manifest_revision);
            }
        } else {
            next_structural_rev = prior.structural_revision;
        }
    }
    let workflow_state = manifest_state_to_db(effective_state);
    let mut effective_normalized = normalized.clone();
    effective_normalized.workflow_state = effective_state;
    let effective_document = normalized_to_document(&effective_normalized);
    let effective_document_json = serde_json::to_string(&effective_document)
        .map_err(|error| WorkflowStoreError::Persistence(format!("serialize manifest: {error}")))?;
    let effective_document_digest = sha256_hex(effective_document_json.as_bytes());

    let (active_block_cause, active_block_source) = match prior_header.as_ref() {
        Some(prior) if prior.workflow_state == WorkflowState::Blocked => (
            prior.block_cause_code.clone(),
            prior.block_source_manifest_revision,
        ),
        Some(prior) if effective_state == ManifestWorkflowState::Blocked => (
            Some(WorkflowBlockCause::ExplicitManifestBlock.as_str().into()),
            Some(prior.active_manifest_revision),
        ),
        None if effective_state == ManifestWorkflowState::Blocked => (
            Some(WorkflowBlockCause::ExplicitManifestBlock.as_str().into()),
            Some(next_manifest_rev),
        ),
        _ => (None, None),
    };

    let existing_bindings = if prior_header.is_some() {
        delegation_workflow_node_binding::Entity::find()
            .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(workflow_id.clone()))
            .all(txn)
            .await
            .map_err(db_err)?
    } else {
        Vec::new()
    };

    let has_run_bindings = if prior_header.is_some() {
        load_observed_node_ids(txn, &workflow_id).await?
    } else {
        HashSet::new()
    };

    let binding_diff = plan_binding_diff(
        &workflow_id,
        &effective_normalized,
        prior_normalized.as_ref(),
        &existing_bindings,
        &has_run_bindings,
    )?;

    // Header first so child FK rows can land in the same transaction.
    if let Some(prior) = prior_header.clone() {
        let mut am: delegation_workflow::ActiveModel = prior.into();
        am.active_manifest_revision = Set(next_manifest_rev);
        am.graph_revision = Set(next_graph_rev);
        am.workflow_state = Set(workflow_state);
        am.capability_version = Set(WORKFLOW_CAPABILITY_VERSION.into());
        am.supersedes_approved_revision = Set(supersedes);
        am.structural_revision = Set(next_structural_rev);
        am.design_fingerprint = Set(next_design_fp);
        am.plan_fingerprint = Set(next_plan_fp);
        if effective_state == ManifestWorkflowState::Blocked {
            am.block_cause_code = Set(active_block_cause.clone());
            am.block_source_manifest_revision = Set(active_block_source);
        }
        am.updated_at = Set(now);
        am.update(txn).await.map_err(db_err)?;
    } else {
        // CREATE: insert under SAVEPOINT so unique/busy can reclassify cleanly.
        if let Some(classified) = insert_header_create_or_reclassify(
            txn,
            &workflow_id,
            parent_conversation_id,
            &effective_normalized,
            next_manifest_rev,
            next_graph_rev,
            workflow_state,
            &effective_document_digest,
            now,
            protocol_version,
            protocol_mode.clone(),
        )
        .await?
        {
            return Ok(classified);
        }
    }

    apply_binding_diff(txn, next_manifest_rev, binding_diff, now).await?;

    let v2_enforce = prior_header.as_ref().map_or_else(
        || {
            protocol_version == 2
                && protocol_mode
                    == crate::db::entities::delegation_workflow::CompletionProtocolMode::V2Enforce
        },
        |header| {
            header.completion_protocol_version == 2
                && header.completion_protocol_mode
                    == crate::db::entities::delegation_workflow::CompletionProtocolMode::V2Enforce
        },
    );
    if v2_enforce {
        initialize_v2_gate_states_txn(
            txn,
            &workflow_id,
            &effective_normalized,
            prior_normalized.as_ref(),
        )
        .await?;
    }

    let rev_row = delegation_workflow_manifest_revision::ActiveModel {
        workflow_id: Set(workflow_id.clone()),
        manifest_revision: Set(next_manifest_rev),
        manifest_state: Set(manifest_state_str(effective_state).into()),
        document_json: Set(effective_document_json),
        document_digest: Set(effective_document_digest),
        revision_kind: Set(Some(ManifestRevisionKind::Publication.as_str().into())),
        source_manifest_revision: Set(if effective_state == ManifestWorkflowState::Blocked {
            active_block_source
        } else {
            None
        }),
        recovery_authorization_id: Set(None),
        transition_reason_code: Set(if effective_state == ManifestWorkflowState::Blocked {
            active_block_cause.clone()
        } else {
            None
        }),
        consumer_correlation_id: Set(None),
        graph_revision: Set(Some(next_graph_rev)),
        recovery_source_state_fingerprint: Set(None),
        recovery_risk_class: Set(None),
        created_at: Set(now),
    };
    rev_row.insert(txn).await.map_err(db_err)?;

    if inject_publish_persistence_failure() {
        return Err(WorkflowStoreError::Persistence(
            "injected publish persistence failure".into(),
        ));
    }

    publish_result(
        workflow_id.clone(),
        next_manifest_rev as u64,
        next_graph_rev as u64,
        effective_state,
        false,
        true,
        active_block_cause.as_deref(),
        active_block_source,
    )
}

#[cfg(any(test, feature = "test-utils"))]
async fn initialize_v2_gate_states_txn(
    txn: &sea_orm::DatabaseTransaction,
    workflow_id: &str,
    normalized: &NormalizedManifest,
    prior_normalized: Option<&NormalizedManifest>,
) -> Result<(), WorkflowStoreError> {
    let mut desired = Vec::new();
    for gate in &normalized.gates {
        if gate.required_reviewer_node_ids.is_empty() {
            continue;
        }
        let selected = canonical_string_set(&gate.required_reviewer_node_ids);
        let material = match gate.gate_kind {
            DocumentGateKind::Design => serde_json::json!({
                "workflow_id": workflow_id,
                "gate_id": gate.id,
                "gate_kind": gate.gate_kind.as_str(),
                "resolution_mode": gate.resolution_mode,
                "design": normalized.design,
            }),
            DocumentGateKind::Plan => serde_json::json!({
                "workflow_id": workflow_id,
                "gate_id": gate.id,
                "gate_kind": gate.gate_kind.as_str(),
                "resolution_mode": gate.resolution_mode,
                "design": normalized.design,
                "plan": normalized.plan,
                "risk_policy_version": normalized.risk_policy_version,
                "task_policies": normalized.task_policies,
            }),
        };
        let lineage = workflow_gate_lineage(&material)?;
        let prior_selected = prior_normalized.and_then(|prior| {
            prior
                .gates
                .iter()
                .find(|prior_gate| prior_gate.id == gate.id)
                .map(|prior_gate| canonical_string_set(&prior_gate.required_reviewer_node_ids))
        });
        desired.push((gate.id.clone(), lineage, selected, prior_selected));
    }

    let final_reviewers = canonical_string_set(
        &normalized
            .nodes
            .iter()
            .filter(|node| {
                node.phase_id.as_deref() == Some(super::types::PHASE_FINAL)
                    && node.role == Some(ManifestNodeRole::Reviewer)
                    && node.required
            })
            .map(|node| node.id.clone())
            .collect::<Vec<_>>(),
    );
    if !final_reviewers.is_empty() {
        let material = serde_json::json!({
            "workflow_id": workflow_id,
            "gate_id": "final",
            "gate_kind": "final",
            "design": normalized.design,
            "plan": normalized.plan,
            "risk_policy_version": normalized.risk_policy_version,
            "task_policies": normalized.task_policies,
        });
        let lineage = workflow_gate_lineage(&material)?;
        let prior_final_reviewers = prior_normalized.map(required_final_reviewer_node_ids);
        desired.push((
            "final".to_string(),
            lineage,
            final_reviewers,
            prior_final_reviewers,
        ));
    }

    for (gate_id, gate_lineage, required_node_ids, prior_required_node_ids) in desired {
        let selected_node_ids_json = serde_json::to_string(&required_node_ids)
            .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
        let prior = delegation_workflow_gate_state::Entity::find_by_id((
            workflow_id.to_string(),
            gate_id.clone(),
        ))
        .one(txn)
        .await
        .map_err(db_err)?;
        match prior {
            Some(state) if state.gate_lineage == gate_lineage => {
                let Some(prior_required_node_ids) = prior_required_node_ids else {
                    continue;
                };
                if prior_required_node_ids == required_node_ids {
                    continue;
                }

                let added_node_ids = required_node_ids
                    .iter()
                    .filter(|node_id| prior_required_node_ids.binary_search(node_id).is_err())
                    .cloned()
                    .collect::<Vec<_>>();
                let (review_round, selected_node_ids) = if added_node_ids.is_empty() {
                    let current_required = required_node_ids.iter().collect::<BTreeSet<_>>();
                    let prior_selected =
                        serde_json::from_str::<Vec<String>>(&state.selected_node_ids_json)
                            .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
                    (
                        state.current_review_round,
                        canonical_string_set(&prior_selected)
                            .into_iter()
                            .filter(|node_id| current_required.contains(node_id))
                            .collect(),
                    )
                } else {
                    (
                        state.current_review_round.checked_add(1).ok_or_else(|| {
                            WorkflowStoreError::Persistence(
                                "workflow gate review round overflow".into(),
                            )
                        })?,
                        added_node_ids,
                    )
                };
                let mut active: delegation_workflow_gate_state::ActiveModel = state.into();
                active.current_review_round = Set(review_round);
                active.selected_node_ids_json = Set(serde_json::to_string(&selected_node_ids)
                    .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?);
                active.update(txn).await.map_err(db_err)?;
                if gate_id == super::types::PHASE_PLAN {
                    delete_plan_round_authorization_txn(txn, workflow_id, &gate_id).await?;
                }
            }
            Some(state) => {
                if gate_id == super::types::PHASE_PLAN
                    && is_pending_plan_corrective_round_txn(
                        txn,
                        workflow_id,
                        &gate_id,
                        &state,
                        normalized.plan.as_ref().map(|plan| plan.digest.as_str()),
                        &required_node_ids,
                    )
                    .await?
                {
                    continue;
                }
                let mut active: delegation_workflow_gate_state::ActiveModel = state.into();
                active.gate_lineage = Set(gate_lineage);
                active.current_review_round = Set(1);
                active.selected_node_ids_json = Set(selected_node_ids_json);
                active.update(txn).await.map_err(db_err)?;
                if gate_id == super::types::PHASE_PLAN {
                    delete_plan_round_authorization_txn(txn, workflow_id, &gate_id).await?;
                }
            }
            None => {
                delegation_workflow_gate_state::ActiveModel {
                    workflow_id: Set(workflow_id.to_string()),
                    gate_id: Set(gate_id),
                    gate_lineage: Set(gate_lineage),
                    current_review_round: Set(1),
                    selected_node_ids_json: Set(selected_node_ids_json),
                }
                .insert(txn)
                .await
                .map_err(db_err)?;
            }
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-utils"))]
fn required_final_reviewer_node_ids(normalized: &NormalizedManifest) -> Vec<String> {
    canonical_string_set(
        &normalized
            .nodes
            .iter()
            .filter(|node| {
                node.phase_id.as_deref() == Some(super::types::PHASE_FINAL)
                    && node.role == Some(ManifestNodeRole::Reviewer)
                    && node.required
            })
            .map(|node| node.id.clone())
            .collect::<Vec<_>>(),
    )
}

#[cfg(any(test, feature = "test-utils"))]
async fn is_pending_plan_corrective_round_txn(
    txn: &sea_orm::DatabaseTransaction,
    workflow_id: &str,
    gate_id: &str,
    state: &delegation_workflow_gate_state::Model,
    active_plan_digest: Option<&str>,
    required_node_ids: &[String],
) -> Result<bool, WorkflowStoreError> {
    let prior_settlement = delegation_workflow_gate_settlement::Entity::find()
        .filter(delegation_workflow_gate_settlement::Column::WorkflowId.eq(workflow_id))
        .filter(delegation_workflow_gate_settlement::Column::GateId.eq(gate_id))
        .filter(delegation_workflow_gate_settlement::Column::PlanRoundStateV2Json.is_not_null())
        .order_by_desc(delegation_workflow_gate_settlement::Column::GateCycle)
        .one(txn)
        .await
        .map_err(db_err)?;
    let Some(prior_settlement) = prior_settlement else {
        return Ok(false);
    };
    let prior_state = load_persisted_plan_state_v2(&prior_settlement)?;
    if prior_state.next_action != PlanReviewNextAction::ContinueReview
        || prior_state.gate_lineage != state.gate_lineage
        || i64::from(prior_state.review_round).checked_add(1) != Some(state.current_review_round)
    {
        return Ok(false);
    }

    let selected_node_ids = canonical_string_set(
        &serde_json::from_str::<Vec<String>>(&state.selected_node_ids_json)
            .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?,
    );
    if selected_node_ids.is_empty() {
        return Ok(true);
    }
    let Some(active_plan_digest) = active_plan_digest else {
        return Ok(false);
    };
    let authorization = load_plan_round_authorization_v2(txn, workflow_id, gate_id).await?;
    Ok(authorization.is_some_and(|authorization| {
        authorization.gate_lineage == state.gate_lineage
            && i64::from(authorization.review_round) == state.current_review_round
            && authorization.current_plan_digest == active_plan_digest
            && authorization.required_node_ids == required_node_ids
            && authorization.selected_node_ids == selected_node_ids
    }))
}

#[cfg(any(test, feature = "test-utils"))]
async fn delete_plan_round_authorization_txn(
    txn: &sea_orm::DatabaseTransaction,
    workflow_id: &str,
    gate_id: &str,
) -> Result<(), WorkflowStoreError> {
    delegation_plan_round_authorization::Entity::delete_by_id((
        workflow_id.to_string(),
        gate_id.to_string(),
    ))
    .exec(txn)
    .await
    .map_err(db_err)?;
    Ok(())
}

#[cfg(any(test, feature = "test-utils"))]
fn workflow_gate_lineage(material: &serde_json::Value) -> Result<String, WorkflowStoreError> {
    let canonical = canonical_json(material)
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    let mut hasher = Sha256::new();
    hasher.update(b"codeg.workflow-gate-lineage.v2\0");
    hasher.update(canonical.as_bytes());
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// Insert create header under SAVEPOINT.
///
/// - `Ok(None)` — header inserted; caller continues bindings/revision.
/// - `Ok(Some(result))` — race reclassified to same-digest idempotent replay.
/// - `Err(PublicationTokenMismatch|Conflict|CrossParent)` — typed race outcome.
/// - `Err(Persistence(TOKEN_RACE_RECLASSIFY_MARKER))` — winner not visible; outer
///   must re-read with a fresh snapshot (never returned as raw busy/unique).
#[allow(clippy::too_many_arguments)]
#[cfg(any(test, feature = "test-utils"))]
async fn insert_header_create_or_reclassify(
    txn: &sea_orm::DatabaseTransaction,
    workflow_id: &str,
    parent_conversation_id: i32,
    normalized: &NormalizedManifest,
    next_manifest_rev: i64,
    next_graph_rev: i64,
    workflow_state: WorkflowState,
    document_digest: &str,
    now: chrono::DateTime<Utc>,
    protocol_version: i64,
    protocol_mode: delegation_workflow::CompletionProtocolMode,
) -> Result<Option<PublishResult>, WorkflowStoreError> {
    use sea_orm::ConnectionTrait;

    const SP: &str = "sp_wf_pub_header";
    txn.execute_unprepared(&format!("SAVEPOINT {SP}"))
        .await
        .map_err(db_err)?;

    // Double-check token immediately before insert (another writer may have landed).
    if let Some(by_token) =
        load_by_publication_token_txn(txn, &normalized.publication_token, parent_conversation_id)
            .await?
    {
        let _ = txn.execute_unprepared(&format!("RELEASE {SP}")).await;
        return Ok(Some(
            classify_existing_header(
                txn,
                by_token,
                parent_conversation_id,
                &normalized.publication_token,
                document_digest,
            )
            .await?,
        ));
    }

    let header = delegation_workflow::ActiveModel {
        workflow_id: Set(workflow_id.to_string()),
        parent_conversation_id: Set(parent_conversation_id),
        workflow_kind: Set(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into()),
        schema_version: Set(normalized.schema_version as i64),
        active_manifest_revision: Set(next_manifest_rev),
        graph_revision: Set(next_graph_rev),
        workflow_state: Set(workflow_state.clone()),
        capability_version: Set(WORKFLOW_CAPABILITY_VERSION.into()),
        publication_token: Set(normalized.publication_token.clone()),
        supersedes_approved_revision: Set(None),
        structural_revision: Set(next_manifest_rev),
        design_fingerprint: Set(design_fingerprint_hash(normalized)),
        plan_fingerprint: Set(plan_fingerprint_hash(normalized)),
        block_cause_code: Set((workflow_state == WorkflowState::Blocked)
            .then(|| WorkflowBlockCause::ExplicitManifestBlock.as_str().into())),
        block_source_manifest_revision: Set(
            (workflow_state == WorkflowState::Blocked).then_some(next_manifest_rev)
        ),
        completion_protocol_version: Set(protocol_version),
        completion_protocol_mode: Set(protocol_mode),
        legacy_source_workflow_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };

    match header.insert(txn).await {
        Ok(_) => {
            let _ = txn.execute_unprepared(&format!("RELEASE {SP}")).await;
            Ok(None)
        }
        Err(e) if is_token_race_db_err(&e) => {
            let _ = txn.execute_unprepared(&format!("ROLLBACK TO {SP}")).await;
            let _ = txn.execute_unprepared(&format!("RELEASE {SP}")).await;
            match classify_token_race_visible(
                txn,
                &normalized.publication_token,
                document_digest,
                parent_conversation_id,
            )
            .await?
            {
                Some(r) => Ok(Some(r)),
                None => Err(WorkflowStoreError::Persistence(
                    TOKEN_RACE_RECLASSIFY_MARKER.into(),
                )),
            }
        }
        Err(e) => {
            let _ = txn.execute_unprepared(&format!("ROLLBACK TO {SP}")).await;
            // Never surface raw unique/busy as Persistence for token races.
            if is_token_race_db_err(&e) {
                Err(WorkflowStoreError::Persistence(
                    TOKEN_RACE_RECLASSIFY_MARKER.into(),
                ))
            } else {
                Err(db_err(e))
            }
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
async fn load_by_parent_kind_txn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
) -> Result<Option<delegation_workflow::Model>, WorkflowStoreError> {
    let workflow_id = load_workflow_id_by_parent_kind(conn, parent_conversation_id).await?;
    let Some(workflow_id) = workflow_id else {
        return Ok(None);
    };
    require_stored_v2_header(conn, &workflow_id).await?;
    delegation_workflow::Entity::find_by_id(&workflow_id)
        .one(conn)
        .await
        .map_err(db_err)
}

/// Re-read winner after a race. `None` = not visible under this snapshot.
#[cfg(any(test, feature = "test-utils"))]
async fn classify_token_race_visible<C: sea_orm::ConnectionTrait>(
    conn: &C,
    token: &str,
    document_digest: &str,
    parent_conversation_id: i32,
) -> Result<Option<PublishResult>, WorkflowStoreError> {
    if let Some(by_token) =
        load_by_publication_token_txn(conn, token, parent_conversation_id).await?
    {
        return Ok(Some(
            classify_existing_header(
                conn,
                by_token,
                parent_conversation_id,
                token,
                document_digest,
            )
            .await?,
        ));
    }
    // Parent row may be visible before token unique index under rare timings.
    if let Some(by_parent) = load_by_parent_kind_txn(conn, parent_conversation_id).await? {
        require_v2_mutation_for_connection(
            conn,
            by_parent.completion_protocol_version,
            &by_parent.completion_protocol_mode,
        )
        .await?;
        if by_parent.publication_token == token {
            return Ok(Some(
                classify_existing_header(
                    conn,
                    by_parent,
                    parent_conversation_id,
                    token,
                    document_digest,
                )
                .await?,
            ));
        }
        return Err(WorkflowStoreError::PublicationTokenConflict {
            existing_workflow_id: by_parent.workflow_id,
        });
    }
    Ok(None)
}

#[cfg(any(test, feature = "test-utils"))]
async fn classify_existing_header<C: sea_orm::ConnectionTrait>(
    conn: &C,
    mut header: delegation_workflow::Model,
    parent_conversation_id: i32,
    token: &str,
    document_digest: &str,
) -> Result<PublishResult, WorkflowStoreError> {
    if header.parent_conversation_id != parent_conversation_id {
        return Err(WorkflowStoreError::CrossParent {
            workflow_id: header.workflow_id,
            expected_parent: parent_conversation_id,
            actual_parent: header.parent_conversation_id,
        });
    }
    require_v2_mutation_for_connection(
        conn,
        header.completion_protocol_version,
        &header.completion_protocol_mode,
    )
    .await?;
    let active_digest =
        load_active_manifest_digest_txn(conn, &header.workflow_id, header.active_manifest_revision)
            .await?;
    if header.parent_conversation_id == parent_conversation_id
        && active_digest.as_deref() == Some(document_digest)
    {
        header = stamp_workflow_capability_version(conn, header).await?;
    }
    let block_cause_code = header.block_cause_code.clone();
    let block_source_manifest_revision = header.block_source_manifest_revision;
    let result = classify_header_against_digest(
        token,
        parent_conversation_id,
        header.parent_conversation_id,
        header.workflow_id,
        active_digest.as_deref(),
        document_digest,
        header.active_manifest_revision as u64,
        header.graph_revision as u64,
        workflow_state_to_manifest(header.workflow_state),
    )?;
    if result.workflow_state == ManifestWorkflowState::Blocked {
        return publish_result(
            result.workflow_id,
            result.manifest_revision,
            result.graph_revision,
            result.workflow_state,
            result.idempotent_replay,
            result.publication_committed,
            block_cause_code.as_deref(),
            block_source_manifest_revision,
        );
    }
    Ok(result)
}

#[cfg(any(test, feature = "test-utils"))]
async fn stamp_workflow_capability_version<C: sea_orm::ConnectionTrait>(
    conn: &C,
    header: delegation_workflow::Model,
) -> Result<delegation_workflow::Model, WorkflowStoreError> {
    if header.capability_version == WORKFLOW_CAPABILITY_VERSION {
        return Ok(header);
    }
    let mut active: delegation_workflow::ActiveModel = header.into();
    active.capability_version = Set(WORKFLOW_CAPABILITY_VERSION.into());
    active.update(conn).await.map_err(db_err)
}

/// Pure B8 reclassify once a **durable** header row is known.
///
/// Never invents `PublicationTokenMismatch` without a real `workflow_id`.
/// Same digest → idempotent replay; different digest → mismatch with that id.
#[allow(clippy::too_many_arguments)]
#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn classify_header_against_digest(
    token: &str,
    expected_parent: i32,
    header_parent: i32,
    workflow_id: String,
    active_digest: Option<&str>,
    document_digest: &str,
    manifest_revision: u64,
    graph_revision: u64,
    workflow_state: ManifestWorkflowState,
) -> Result<PublishResult, WorkflowStoreError> {
    if header_parent != expected_parent {
        return Err(WorkflowStoreError::CrossParent {
            workflow_id,
            expected_parent,
            actual_parent: header_parent,
        });
    }
    if active_digest == Some(document_digest) {
        return publish_result(
            workflow_id,
            manifest_revision,
            graph_revision,
            workflow_state,
            true,
            false,
            None,
            None,
        );
    }
    // Different digest for the same token → B8 Mismatch (requires durable row).
    Err(WorkflowStoreError::PublicationTokenMismatch {
        publication_token: token.to_string(),
        workflow_id,
    })
}

/// Backoff schedule for post-race re-reads (~500ms total).
#[cfg(any(test, feature = "test-utils"))]
const TOKEN_RACE_BACKOFF_MS: &[u64] = &[5, 10, 20, 40, 80, 100, 120, 125];

/// Fresh-snapshot reclassify after concurrent unique/busy (outer, after txn ends).
///
/// - Durable same-token row + same digest → IdempotentReplay
/// - Durable same-token row + different digest → PublicationTokenMismatch (real id)
/// - Parent has other-token workflow → PublicationTokenConflict
/// - Still absent after exponential backoff → Busy (retryable), **never** fabricated Mismatch
#[cfg(any(test, feature = "test-utils"))]
async fn classify_token_race_fresh(
    db: &AppDatabase,
    token: &str,
    document_digest: &str,
    parent_conversation_id: i32,
) -> Result<PublishResult, WorkflowStoreError> {
    for (i, &delay_ms) in TOKEN_RACE_BACKOFF_MS.iter().enumerate() {
        match classify_token_race_visible(&db.conn, token, document_digest, parent_conversation_id)
            .await?
        {
            Some(r) => return Ok(r),
            None => {
                if i + 1 < TOKEN_RACE_BACKOFF_MS.len() {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }
    // One last attempt after the final delay.
    tokio::time::sleep(std::time::Duration::from_millis(
        *TOKEN_RACE_BACKOFF_MS.last().unwrap_or(&50),
    ))
    .await;
    match classify_token_race_visible(&db.conn, token, document_digest, parent_conversation_id)
        .await?
    {
        Some(r) => Ok(r),
        None => {
            // Parent with a *different* token is a durable conflict, not busy.
            if let Some(by_parent) =
                load_by_parent_kind_txn(&db.conn, parent_conversation_id).await?
            {
                require_v2_mutation_for_connection(
                    &db.conn,
                    by_parent.completion_protocol_version,
                    &by_parent.completion_protocol_mode,
                )
                .await?;
                if by_parent.publication_token == token {
                    // Token-equal parent without digest load earlier: classify now.
                    return classify_existing_header(
                        &db.conn,
                        by_parent,
                        parent_conversation_id,
                        token,
                        document_digest,
                    )
                    .await;
                }
                return Err(WorkflowStoreError::PublicationTokenConflict {
                    existing_workflow_id: by_parent.workflow_id,
                });
            }
            // No durable token row → retryable busy. Never invent Mismatch.
            Err(WorkflowStoreError::Busy(format!(
                "publication_token race: durable row for token not visible after retries; \
                 retry publish (token={token})"
            )))
        }
    }
}

fn document_digest_for_gate(
    gate: &NormalizedGate,
    normalized: &NormalizedManifest,
) -> Result<Option<String>, WorkflowStoreError> {
    match gate.gate_kind {
        DocumentGateKind::Design => Ok(normalized.design.as_ref().map(|d| d.digest.clone())),
        DocumentGateKind::Plan => Ok(normalized.plan.as_ref().map(|d| d.digest.clone())),
    }
}

#[cfg(any(test, feature = "test-utils"))]
async fn load_observed_node_ids<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
) -> Result<HashSet<String>, WorkflowStoreError> {
    let rows = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .all(conn)
        .await
        .map_err(db_err)?;
    Ok(rows.into_iter().map(|r| r.node_id).collect())
}

#[cfg(any(test, feature = "test-utils"))]
fn frozen_cohort_error(task_index: i64) -> WorkflowStoreError {
    WorkflowStoreError::CohortFrozen {
        node_id: format!("Task {task_index}"),
    }
}

#[cfg(any(test, feature = "test-utils"))]
async fn retired_reactivation_candidate<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    normalized: &NormalizedManifest,
) -> Result<Option<delegation_workflow_node_binding::Model>, WorkflowStoreError> {
    let bindings = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .all(conn)
        .await
        .map_err(db_err)?;
    Ok(bindings.into_iter().find(|binding| {
        binding.retired_revision.is_some()
            && normalized.nodes.iter().any(|node| {
                node.kind == ManifestNodeKind::WorkUnit
                    && (node.id == binding.node_id
                        || node.work_unit_key.as_ref() == Some(&binding.work_unit_key))
            })
    }))
}

#[cfg(any(test, feature = "test-utils"))]
enum BindingDiffAction {
    Reactivate {
        binding: delegation_workflow_node_binding::Model,
    },
    Retire {
        binding: delegation_workflow_node_binding::Model,
        retained_observed: bool,
    },
    Retain {
        binding: delegation_workflow_node_binding::Model,
        node: NormalizedNode,
        is_admitted: bool,
        freeze_cohort: bool,
    },
    Insert {
        node: NormalizedNode,
        freeze_cohort: bool,
    },
}

#[cfg(any(test, feature = "test-utils"))]
struct BindingDiffPlan {
    workflow_id: String,
    actions: Vec<BindingDiffAction>,
}

/// Validate every binding lifecycle decision without performing a write.
#[cfg(any(test, feature = "test-utils"))]
fn plan_binding_diff(
    workflow_id: &str,
    normalized: &NormalizedManifest,
    prior_normalized: Option<&NormalizedManifest>,
    existing: &[delegation_workflow_node_binding::Model],
    nodes_with_runs: &HashSet<String>,
) -> Result<BindingDiffPlan, WorkflowStoreError> {
    note_binding_diff_invocation();
    let existing_by_id: HashMap<&str, &delegation_workflow_node_binding::Model> =
        existing.iter().map(|b| (b.node_id.as_str(), b)).collect();
    let existing_by_key: HashMap<&str, &delegation_workflow_node_binding::Model> = existing
        .iter()
        .map(|binding| (binding.work_unit_key.as_str(), binding))
        .collect();

    let new_work_units: Vec<&NormalizedNode> = normalized
        .nodes
        .iter()
        .filter(|n| n.kind == ManifestNodeKind::WorkUnit && n.work_unit_key.is_some())
        .collect();
    let new_by_id: HashMap<&str, &NormalizedNode> = new_work_units
        .iter()
        .map(|node| (node.id.as_str(), *node))
        .collect();

    let admitted_task_indices: HashSet<i64> = existing
        .iter()
        .filter(|b| {
            b.retired_revision.is_none()
                && b.task_index.is_some()
                && (b.is_observed
                    || b.retained_observed
                    || b.cohort_frozen
                    || nodes_with_runs.contains(&b.node_id))
        })
        .filter_map(|b| b.task_index)
        .collect();

    let mut frozen_routes: HashMap<i64, HashSet<String>> = HashMap::new();
    for task_index in admitted_task_indices {
        let prior_policy = prior_normalized
            .and_then(|manifest| {
                manifest
                    .task_policies
                    .iter()
                    .find(|policy| policy.task_index as i64 == task_index)
            })
            .ok_or_else(|| frozen_cohort_error(task_index))?;
        let next_policy = normalized
            .task_policies
            .iter()
            .find(|policy| policy.task_index as i64 == task_index)
            .ok_or_else(|| frozen_cohort_error(task_index))?;
        if prior_policy != next_policy {
            return Err(frozen_cohort_error(task_index));
        }
        let mut route = HashSet::with_capacity(prior_policy.route.reviewer_node_ids.len() + 1);
        route.insert(prior_policy.route.implementer_node_id.clone());
        route.extend(prior_policy.route.reviewer_node_ids.iter().cloned());
        for node_id in &route {
            let binding = existing_by_id
                .get(node_id.as_str())
                .filter(|binding| binding.retired_revision.is_none())
                .ok_or_else(|| frozen_cohort_error(task_index))?;
            let node = new_by_id
                .get(node_id.as_str())
                .ok_or_else(|| frozen_cohort_error(task_index))?;
            if !binding_identity_matches(workflow_id, binding, node) {
                return Err(frozen_cohort_error(task_index));
            }
        }
        frozen_routes.insert(task_index, route);
    }

    let mut actions = Vec::with_capacity(existing.len() + new_work_units.len());
    for binding in existing {
        if binding.workflow_id != workflow_id {
            return Err(binding_identity_conflict(binding));
        }
        let next_node = new_by_id.get(binding.node_id.as_str()).copied();
        match (binding.retired_revision.is_some(), next_node) {
            (true, None) => continue,
            (true, Some(node)) => {
                require_exact_binding_identity(workflow_id, binding, node)?;
                actions.push(BindingDiffAction::Reactivate {
                    binding: binding.clone(),
                });
            }
            (false, None) => {
                let is_admitted = binding.is_observed || nodes_with_runs.contains(&binding.node_id);
                let binding_protected = binding.cohort_frozen
                    || binding.is_observed
                    || binding.retained_observed
                    || nodes_with_runs.contains(&binding.node_id);
                if binding_protected && !is_canceled_drop(normalized, binding) {
                    return Err(WorkflowStoreError::CohortFrozen {
                        node_id: binding.node_id.clone(),
                    });
                }
                actions.push(BindingDiffAction::Retire {
                    binding: binding.clone(),
                    retained_observed: binding.retained_observed
                        || is_admitted
                        || binding.cohort_frozen,
                });
            }
            (false, Some(node)) => {
                let is_admitted = binding.is_observed || nodes_with_runs.contains(&node.id);
                let identity_changed = !binding_identity_matches(workflow_id, binding, node);
                let freeze_cohort = node.task_index.is_some_and(|task_index| {
                    frozen_routes
                        .get(&i64::from(task_index))
                        .is_some_and(|route| route.contains(&node.id))
                });
                if freeze_cohort && identity_changed {
                    return Err(frozen_cohort_error(i64::from(
                        node.task_index.expect("frozen cohort Task index"),
                    )));
                }
                if is_admitted && identity_changed {
                    return Err(binding_identity_conflict(binding));
                }
                actions.push(BindingDiffAction::Retain {
                    binding: binding.clone(),
                    node: node.clone(),
                    is_admitted,
                    freeze_cohort,
                });
            }
        }
    }

    for node in new_work_units {
        if existing_by_id.contains_key(node.id.as_str()) {
            continue;
        }
        let key = node.work_unit_key.as_deref().expect("work unit key");
        if let Some(binding) = existing_by_key.get(key) {
            return Err(binding_identity_conflict(binding));
        }
        let freeze_cohort = node.task_index.is_some_and(|task_index| {
            frozen_routes
                .get(&(task_index as i64))
                .is_some_and(|route| route.contains(&node.id))
        });
        actions.push(BindingDiffAction::Insert {
            node: node.clone(),
            freeze_cohort,
        });
    }

    Ok(BindingDiffPlan {
        workflow_id: workflow_id.to_string(),
        actions,
    })
}

#[cfg(any(test, feature = "test-utils"))]
fn binding_identity_matches(
    workflow_id: &str,
    binding: &delegation_workflow_node_binding::Model,
    node: &NormalizedNode,
) -> bool {
    binding.workflow_id == workflow_id
        && binding.node_id == node.id
        && node.work_unit_key.as_ref() == Some(&binding.work_unit_key)
        && node.role.map(role_str) == Some(binding.role.as_str())
        && node.agent_type.as_ref() == Some(&binding.agent_type)
        && node.profile_id == binding.profile_id
        && node.phase_id.as_ref() == Some(&binding.phase_id)
        && node.task_index.map(i64::from) == binding.task_index
}

#[cfg(any(test, feature = "test-utils"))]
fn binding_identity_conflict(
    binding: &delegation_workflow_node_binding::Model,
) -> WorkflowStoreError {
    WorkflowStoreError::AdmittedNodeIdentityMutation {
        node_id: binding.node_id.clone(),
    }
}

#[cfg(any(test, feature = "test-utils"))]
fn require_exact_binding_identity(
    workflow_id: &str,
    binding: &delegation_workflow_node_binding::Model,
    node: &NormalizedNode,
) -> Result<(), WorkflowStoreError> {
    binding_identity_matches(workflow_id, binding, node)
        .then_some(())
        .ok_or_else(|| binding_identity_conflict(binding))
}

/// Apply a preflighted binding plan. No lifecycle validation belongs here.
#[cfg(any(test, feature = "test-utils"))]
async fn apply_binding_diff<C: sea_orm::ConnectionTrait>(
    conn: &C,
    next_revision: i64,
    plan: BindingDiffPlan,
    now: chrono::DateTime<Utc>,
) -> Result<(), WorkflowStoreError> {
    for action in plan.actions {
        match action {
            BindingDiffAction::Reactivate { binding } => {
                let mut active: delegation_workflow_node_binding::ActiveModel = binding.into();
                active.retired_revision = Set(None);
                active.updated_at = Set(now);
                active.update(conn).await.map_err(db_err)?;
            }
            BindingDiffAction::Retire {
                binding,
                retained_observed,
            } => {
                let mut active: delegation_workflow_node_binding::ActiveModel = binding.into();
                active.retired_revision = Set(Some(next_revision));
                active.retained_observed = Set(retained_observed);
                active.updated_at = Set(now);
                active.update(conn).await.map_err(db_err)?;
            }
            BindingDiffAction::Retain {
                binding,
                node,
                is_admitted,
                freeze_cohort,
            } => {
                let mut active: delegation_workflow_node_binding::ActiveModel = binding.into();
                if !is_admitted {
                    active.work_unit_key = Set(node.work_unit_key.expect("work unit key"));
                    active.role = Set(role_str(node.role.expect("work unit role")).into());
                    active.agent_type = Set(node.agent_type.expect("agent"));
                    active.profile_id = Set(node.profile_id);
                    active.phase_id = Set(node.phase_id.expect("phase"));
                    active.task_index = Set(node.task_index.map(i64::from));
                }
                if freeze_cohort {
                    active.cohort_frozen = Set(true);
                }
                if let Some(outcome) = node.node_outcome {
                    active.node_outcome = Set(Some(match outcome {
                        ManifestNodeOutcome::Canceled => NodeOutcome::Canceled,
                    }));
                }
                active.updated_at = Set(now);
                active.update(conn).await.map_err(db_err)?;
            }
            BindingDiffAction::Insert {
                node,
                freeze_cohort,
            } => {
                let outcome = node.node_outcome.map(|outcome| match outcome {
                    ManifestNodeOutcome::Canceled => NodeOutcome::Canceled,
                });
                delegation_workflow_node_binding::ActiveModel {
                    workflow_id: Set(plan.workflow_id.clone()),
                    node_id: Set(node.id),
                    work_unit_key: Set(node.work_unit_key.expect("work unit key")),
                    role: Set(role_str(node.role.expect("work unit role")).into()),
                    agent_type: Set(node.agent_type.expect("agent")),
                    profile_id: Set(node.profile_id),
                    phase_id: Set(node.phase_id.expect("phase")),
                    task_index: Set(node.task_index.map(i64::from)),
                    introduced_revision: Set(next_revision),
                    retired_revision: Set(None),
                    is_observed: Set(false),
                    retained_observed: Set(false),
                    cohort_frozen: Set(freeze_cohort),
                    node_outcome: Set(outcome),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(conn)
                .await
                .map_err(db_err)?;
            }
        }
    }
    Ok(())
}

/// Drop is legal only when cancellation is explicit for the binding or its pair.
#[cfg(any(test, feature = "test-utils"))]
fn is_canceled_drop(
    normalized: &NormalizedManifest,
    binding: &delegation_workflow_node_binding::Model,
) -> bool {
    // Partner still listed with canceled outcome, or this node listed canceled.
    normalized
        .nodes
        .iter()
        .any(|n| n.id == binding.node_id && n.node_outcome == Some(ManifestNodeOutcome::Canceled))
        || normalized.nodes.iter().any(|n| {
            n.task_index == binding.task_index.map(|i| i as u32)
                && n.node_outcome == Some(ManifestNodeOutcome::Canceled)
        })
}

#[derive(Debug)]
struct ValidatedV2GateEvidenceSet {
    identity: V2GateEvidenceIdentity,
    selected_node_ids: Vec<String>,
    reviewers: Vec<PlanReviewerOutcomeV2>,
    outcome: GateSettlementOutcome,
}

async fn validate_current_design_self_review_bytes_txn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    normalized: &NormalizedManifest,
    gate: &NormalizedGate,
    parent_conversation_id: i32,
) -> Result<(), WorkflowStoreError> {
    if gate.gate_kind != DocumentGateKind::Design
        || gate.resolution_mode != ResolutionMode::SelfReview
        || !gate.required_reviewer_node_ids.is_empty()
    {
        return Ok(());
    }
    let design = normalized.design.as_ref().ok_or_else(|| {
        WorkflowStoreError::GateNotReady(
            "Design self-review requires an active Design document".into(),
        )
    })?;
    let parent = conversation::Entity::find_by_id(parent_conversation_id)
        .one(conn)
        .await
        .map_err(db_err)?
        .ok_or(WorkflowStoreError::ParentNotFound(parent_conversation_id))?;
    let workspace = folder::Entity::find_by_id(parent.folder_id)
        .one(conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| {
            WorkflowStoreError::Persistence("Design self-review workspace folder is missing".into())
        })?;
    let resolved = resolve_document(
        std::path::Path::new(&workspace.path),
        &design.rel_path,
        MAX_DESIGN_SELF_REVIEW_BYTES,
    )
    .await
    .map_err(|_| WorkflowStoreError::CompletionArtifactUnavailable)?;
    let current_lineage = design_self_review_lineage(
        &header.workflow_id,
        &gate.id,
        &header.workflow_kind,
        resolved.rel_path(),
        resolved.digest(),
    );
    let state = delegation_workflow_gate_state::Entity::find_by_id((
        header.workflow_id.clone(),
        gate.id.clone(),
    ))
    .one(conn)
    .await
    .map_err(db_err)?
    .ok_or(WorkflowStoreError::CompletionDecisionRequired)?;
    let binding = delegation_workflow_design_root_binding::Entity::find_by_id((
        header.workflow_id.clone(),
        gate.id.clone(),
        current_lineage.clone(),
    ))
    .one(conn)
    .await
    .map_err(db_err)?;
    if state.gate_lineage != current_lineage
        || binding
            .as_ref()
            .is_none_or(|binding| binding.design_identity != resolved.digest())
    {
        return Err(WorkflowStoreError::CompletionDecisionSuperseded);
    }
    Ok(())
}

async fn load_validated_v2_plan_author<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    node_id: &str,
    expected_plan_digest: &str,
) -> Result<(String, String, String), WorkflowStoreError> {
    let binding = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id))
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(node_id))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .one(conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::GateNotReady("current Plan Author is missing".into()))?;
    let validated = load_validated_completion_evidence(conn, &binding.task_id)
        .await
        .map_err(|error| WorkflowStoreError::GateNotReady(error.to_string()))?;
    if !matches!(
        validated.evidence.intent.outcome,
        CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns
    ) || validated.evidence.artifact.digest() != expected_plan_digest
    {
        return Err(WorkflowStoreError::GateNotReady(
            "current Plan Author evidence does not cover the active Plan".into(),
        ));
    }
    let workspace = delegation_task_run::Entity::find_by_id(&binding.task_id)
        .one(conn)
        .await
        .map_err(db_err)?
        .and_then(|run| run.workspace_path)
        .filter(|path| !path.trim().is_empty())
        .ok_or_else(|| {
            WorkflowStoreError::GateNotReady("current Plan Author workspace is unavailable".into())
        })?;
    Ok((
        binding.task_id,
        validated.evidence.artifact.digest().to_string(),
        workspace,
    ))
}

async fn capture_plan_snapshot_v2(
    workspace: &str,
    document: &DocumentRef,
) -> Result<PlanArtifactSnapshotV2, WorkflowStoreError> {
    let workspace = PathBuf::from(workspace);
    let rel_path = document.rel_path.clone();
    let read_path = rel_path.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        read_bounded_workspace_file(&workspace, &read_path, MAX_PLAN_MATERIAL_BYTES)
    })
    .await
    .map_err(|_| WorkflowStoreError::CompletionArtifactUnavailable)?
    .map_err(|_| WorkflowStoreError::CompletionArtifactUnavailable)?;
    let digest = format!("sha256:{}", sha256_hex(&bytes));
    if digest != document.digest {
        return Err(WorkflowStoreError::ArtifactDigestMismatch(
            "active Plan bytes do not match the manifest digest".into(),
        ));
    }
    Ok(PlanArtifactSnapshotV2 {
        rel_path,
        digest,
        content_base64: BASE64_STANDARD.encode(bytes),
    })
}

pub(super) async fn authorize_plan_round_after_author_completion<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow: &delegation_workflow::Model,
    author_node_id: &str,
    author_task_id: &str,
    author_workspace: &str,
    artifact_digest: &str,
) -> Result<(), WorkflowStoreError> {
    if workflow.completion_protocol_version != 2 {
        return Ok(());
    }
    let snapshot = load_active_manifest_snapshot(conn, workflow).await?;
    let Some(plan_document) = snapshot.normalized.plan.as_ref() else {
        return Ok(());
    };
    let is_active_author = snapshot.normalized.nodes.iter().any(|node| {
        node.id == author_node_id
            && node.phase_id.as_deref() == Some(super::types::PHASE_PLAN)
            && node.role == Some(ManifestNodeRole::Author)
    });
    if !is_active_author {
        return Err(WorkflowStoreError::GateNotReady(
            "completed Plan Author is not the active manifest Author".into(),
        ));
    }
    let Some(gate) = snapshot
        .normalized
        .gates
        .iter()
        .find(|gate| gate.gate_kind == DocumentGateKind::Plan)
    else {
        return Ok(());
    };
    let Some(gate_state) = delegation_workflow_gate_state::Entity::find_by_id((
        workflow.workflow_id.clone(),
        gate.id.clone(),
    ))
    .one(conn)
    .await
    .map_err(db_err)?
    else {
        return Ok(());
    };
    let selected: Vec<String> = serde_json::from_str(&gate_state.selected_node_ids_json)
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    if !selected.is_empty() {
        return Ok(());
    }

    let prior_settlement = delegation_workflow_gate_settlement::Entity::find()
        .filter(delegation_workflow_gate_settlement::Column::WorkflowId.eq(&workflow.workflow_id))
        .filter(delegation_workflow_gate_settlement::Column::GateId.eq(&gate.id))
        .filter(delegation_workflow_gate_settlement::Column::PlanRoundStateV2Json.is_not_null())
        .order_by_desc(delegation_workflow_gate_settlement::Column::GateCycle)
        .one(conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| {
            WorkflowStoreError::GateNotReady(
                "pending Plan reviewer round has no prior settlement".into(),
            )
        })?;
    let prior_state = load_persisted_plan_state_v2(&prior_settlement)?;
    let current_round = u32::try_from(gate_state.current_review_round).map_err(|_| {
        WorkflowStoreError::Persistence("pending Plan review round exceeds u32".into())
    })?;
    if prior_state.next_action != PlanReviewNextAction::ContinueReview
        || prior_state.gate_lineage != gate_state.gate_lineage
        || current_round != prior_state.review_round.saturating_add(1)
    {
        return Err(WorkflowStoreError::GateCycleConflict(
            "pending Plan reviewer round does not follow its corrective settlement".into(),
        ));
    }
    let current_snapshot = capture_plan_snapshot_v2(author_workspace, plan_document).await?;
    if current_snapshot.digest != artifact_digest {
        return Err(WorkflowStoreError::ArtifactDigestMismatch(
            "completed Plan Author artifact does not match the active Plan snapshot".into(),
        ));
    }
    let classification = classify_plan_settlement_change_v2(
        &snapshot.normalized,
        &prior_state,
        &current_snapshot,
        author_task_id,
    )?;
    match classification {
        PlanChangeClassification::Localized {
            change,
            corrective_reviewer_node_ids,
        } => {
            let required_node_ids = canonical_string_set(&gate.required_reviewer_node_ids);
            let selected_node_ids = corrective_reviewer_node_ids.into_iter().collect::<Vec<_>>();
            let authorization = PlanRoundAuthorizationV2::new(
                gate_state.gate_lineage.clone(),
                current_round,
                prior_state.review_round,
                author_task_id.to_string(),
                required_node_ids,
                selected_node_ids.clone(),
                change.prior_plan_digest.clone(),
                change.current_plan_digest.clone(),
                change,
            )?;
            let authorization_digest = plan_round_authorization_digest_v2(&authorization)?;
            let authorization_json = serde_json::to_string(&authorization)
                .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;

            let mut state: delegation_workflow_gate_state::ActiveModel = gate_state.into();
            state.selected_node_ids_json = Set(serde_json::to_string(&selected_node_ids)
                .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?);
            state.update(conn).await.map_err(db_err)?;
            delegation_plan_round_authorization::ActiveModel {
                workflow_id: Set(workflow.workflow_id.clone()),
                gate_id: Set(gate.id.clone()),
                gate_lineage: Set(authorization.gate_lineage.clone()),
                review_round: Set(i64::from(authorization.review_round)),
                author_task_id: Set(authorization.author_task_id.clone()),
                authorization_json: Set(authorization_json),
                authorization_digest: Set(authorization_digest),
                created_at: Set(Utc::now()),
            }
            .insert(conn)
            .await
            .map_err(db_err)?;
        }
        PlanChangeClassification::NewLineage { .. } => {
            let required_node_ids = canonical_string_set(&gate.required_reviewer_node_ids);
            let mut hasher = Sha256::new();
            hasher.update(b"codeg.plan-authorized-lineage.v2\0");
            hasher.update(prior_state.gate_lineage.as_bytes());
            hasher.update([0]);
            hasher.update(prior_state.review_round.to_be_bytes());
            hasher.update([0]);
            hasher.update(author_task_id.as_bytes());
            hasher.update([0]);
            hasher.update(current_snapshot.digest.as_bytes());
            let mut state: delegation_workflow_gate_state::ActiveModel = gate_state.into();
            state.gate_lineage = Set(format!("sha256:{:x}", hasher.finalize()));
            state.current_review_round = Set(1);
            state.selected_node_ids_json = Set(serde_json::to_string(&required_node_ids)
                .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?);
            state.update(conn).await.map_err(db_err)?;
            delegation_plan_round_authorization::Entity::delete_by_id((
                workflow.workflow_id.clone(),
                gate.id.clone(),
            ))
            .exec(conn)
            .await
            .map_err(db_err)?;
        }
    }
    Ok(())
}

pub(super) async fn load_plan_round_authorization_v2<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    gate_id: &str,
) -> Result<Option<PlanRoundAuthorizationV2>, WorkflowStoreError> {
    let Some(row) = delegation_plan_round_authorization::Entity::find_by_id((
        workflow_id.to_string(),
        gate_id.to_string(),
    ))
    .one(conn)
    .await
    .map_err(db_err)?
    else {
        return Ok(None);
    };
    if row.authorization_json.len() > MAX_PLAN_ROUND_AUTHORIZATION_JSON_BYTES {
        return Err(WorkflowStoreError::Persistence(
            "Plan round authorization JSON exceeds its bound".into(),
        ));
    }
    let authorization: PlanRoundAuthorizationV2 = serde_json::from_str(&row.authorization_json)
        .map_err(|_| {
            WorkflowStoreError::Persistence("Plan round authorization JSON is corrupt".into())
        })?;
    let digest = plan_round_authorization_digest_v2(&authorization)?;
    if row.gate_lineage != authorization.gate_lineage
        || row.review_round != i64::from(authorization.review_round)
        || row.author_task_id != authorization.author_task_id
        || row.authorization_digest != digest
    {
        return Err(WorkflowStoreError::Persistence(
            "Plan round authorization columns disagree with its canonical value".into(),
        ));
    }
    Ok(Some(authorization))
}

fn classify_plan_settlement_change_v2(
    manifest: &NormalizedManifest,
    previous: &PlanReviewRoundStateV2,
    current_snapshot: &PlanArtifactSnapshotV2,
    authorization_id: &str,
) -> Result<PlanChangeClassification, WorkflowStoreError> {
    let prior_snapshot = previous.plan_snapshot.as_ref().ok_or_else(|| {
        WorkflowStoreError::GateNotReady(
            "prior Plan round has no immutable material snapshot; new lineage required".into(),
        )
    })?;
    let prior_bytes = decode_plan_snapshot_v2(prior_snapshot)?;
    let current_bytes = decode_plan_snapshot_v2(current_snapshot)?;
    let mut task_indices = manifest
        .task_policies
        .iter()
        .map(|policy| policy.task_index)
        .collect::<Vec<_>>();
    task_indices.sort_unstable();
    task_indices.dedup();
    let prior = parse_plan_material(&prior_bytes, &task_indices)
        .and_then(|material| bind_plan_material(manifest, &material))
        .map_err(|error| WorkflowStoreError::GateNotReady(error.to_string()))?;
    let current = parse_plan_material(&current_bytes, &task_indices)
        .and_then(|material| bind_plan_material(manifest, &material))
        .map_err(|error| WorkflowStoreError::GateNotReady(error.to_string()))?;
    let reviewer_states = previous
        .reviewers
        .iter()
        .map(|reviewer| (reviewer.node_id.clone(), reviewer.rank == 0))
        .collect::<BTreeMap<_, _>>();
    let authorization =
        authorize_localized_plan_change(manifest, &current, authorization_id, &reviewer_states)
            .map_err(|error| WorkflowStoreError::GateNotReady(error.to_string()))?;
    Ok(classify_plan_change(
        &PlanMaterialChangeInputV1::parsed(prior),
        &PlanMaterialChangeInputV1::parsed(current),
        &authorization,
    ))
}

fn decode_plan_snapshot_v2(
    snapshot: &PlanArtifactSnapshotV2,
) -> Result<Vec<u8>, WorkflowStoreError> {
    let bytes = BASE64_STANDARD
        .decode(&snapshot.content_base64)
        .map_err(|_| WorkflowStoreError::Persistence("Plan snapshot base64 is corrupt".into()))?;
    if bytes.len() > MAX_PLAN_MATERIAL_BYTES
        || format!("sha256:{}", sha256_hex(&bytes)) != snapshot.digest
    {
        return Err(WorkflowStoreError::Persistence(
            "Plan snapshot digest is corrupt".into(),
        ));
    }
    Ok(bytes)
}

async fn load_validated_v2_gate_evidence<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    gate: &NormalizedGate,
) -> Result<ValidatedV2GateEvidenceSet, WorkflowStoreError> {
    let state = delegation_workflow_gate_state::Entity::find_by_id((
        workflow_id.to_string(),
        gate.id.clone(),
    ))
    .one(conn)
    .await
    .map_err(db_err)?
    .ok_or_else(|| WorkflowStoreError::GateNotReady("current gate state is missing".into()))?;
    if gate.required_reviewer_node_ids.is_empty() {
        if gate.gate_kind != DocumentGateKind::Design
            || gate.resolution_mode != ResolutionMode::SelfReview
        {
            return Err(WorkflowStoreError::CompletionDecisionRequired);
        }
        let binding = delegation_workflow_design_root_binding::Entity::find_by_id((
            workflow_id.to_string(),
            gate.id.clone(),
            state.gate_lineage.clone(),
        ))
        .one(conn)
        .await
        .map_err(db_err)?
        .ok_or(WorkflowStoreError::CompletionDecisionRequired)?;
        let attention = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(&binding.task_id))
            .filter(
                delegation_attention_request::Column::Kind
                    .eq(AttentionKind::DesignSelfReviewDecision),
            )
            .order_by_desc(delegation_attention_request::Column::CreatedAt)
            .one(conn)
            .await
            .map_err(db_err)?;
        let outcome = match validated_design_self_review_outcome(&binding, attention.as_ref()) {
            Ok(Some(outcome)) => outcome,
            Ok(None) => return Err(WorkflowStoreError::CompletionDecisionRequired),
            Err(DesignSelfReviewDecisionError::Superseded) => {
                return Err(WorkflowStoreError::CompletionDecisionSuperseded)
            }
            Err(DesignSelfReviewDecisionError::Corrupt) => {
                return Err(WorkflowStoreError::Persistence(
                    "Design self-review decision is corrupt".into(),
                ))
            }
        };
        let identity = V2GateEvidenceIdentity::new(
            state.gate_lineage,
            state.current_review_round,
            vec![binding.node_id],
            vec![binding.task_id],
            vec![binding.evidence_scope_digest],
        )
        .ok_or_else(|| {
            WorkflowStoreError::Persistence(
                "Design self-review evidence identity is invalid".into(),
            )
        })?;
        let selected_node_ids = identity.required_node_ids.clone();
        return Ok(ValidatedV2GateEvidenceSet {
            identity,
            selected_node_ids,
            reviewers: Vec::new(),
            outcome: design_outcome_to_settlement(outcome)?,
        });
    }
    let selected = serde_json::from_str::<BTreeSet<String>>(&state.selected_node_ids_json)
        .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?;
    let required_node_ids = canonical_string_set(&gate.required_reviewer_node_ids);
    let mut task_ids = Vec::with_capacity(required_node_ids.len());
    let mut scope_digests = Vec::with_capacity(required_node_ids.len());
    let mut evidence = Vec::with_capacity(required_node_ids.len());

    for node_id in &required_node_ids {
        let binding = delegation_workflow_run_binding::Entity::find()
            .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id))
            .filter(delegation_workflow_run_binding::Column::NodeId.eq(node_id))
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
            .one(conn)
            .await
            .map_err(db_err)?
            .ok_or_else(|| {
                WorkflowStoreError::GateNotReady(format!(
                    "required reviewer {node_id} has no durable run evidence"
                ))
            })?;
        let run = delegation_task_run::Entity::find_by_id(binding.task_id.clone())
            .one(conn)
            .await
            .map_err(db_err)?
            .ok_or_else(|| {
                WorkflowStoreError::GateNotReady(format!(
                    "required reviewer {node_id} run is missing"
                ))
            })?;
        match run.completion_state {
            Some(CompletionState::NeedsDecision) => {
                return Err(WorkflowStoreError::CompletionDecisionRequired)
            }
            Some(CompletionState::ArtifactRecovery) => {
                return Err(WorkflowStoreError::CompletionArtifactUnavailable)
            }
            _ => {}
        }
        if selected.contains(node_id) && binding.review_round != Some(state.current_review_round) {
            return Err(WorkflowStoreError::GateNotReady(format!(
                "selected reviewer {node_id} lacks current round {} evidence",
                state.current_review_round
            )));
        }
        if binding.gate_lineage.as_deref() != Some(state.gate_lineage.as_str()) {
            return Err(WorkflowStoreError::GateNotReady(format!(
                "reviewer {node_id} evidence belongs to a stale gate lineage"
            )));
        }
        let validated = load_validated_completion_evidence(conn, &binding.task_id)
            .await
            .map_err(|error| WorkflowStoreError::GateNotReady(error.to_string()))?;
        task_ids.push(binding.task_id);
        scope_digests.push(validated.evidence.evidence_scope_digest.clone());
        evidence.push(validated);
    }

    let identity = V2GateEvidenceIdentity::new(
        state.gate_lineage,
        state.current_review_round,
        required_node_ids,
        task_ids,
        scope_digests,
    )
    .ok_or_else(|| {
        WorkflowStoreError::GateNotReady(
            "current v2 gate evidence identity is incomplete or duplicated".into(),
        )
    })?;
    if !evidence.iter().all(|validated| {
        matches!(
            validated.evidence.intent.outcome,
            CompletionOutcome::Approve
                | CompletionOutcome::ApproveWithMinors
                | CompletionOutcome::RequestChanges
                | CompletionOutcome::Block
        )
    }) {
        return Err(WorkflowStoreError::GateNotReady(
            "required reviewer evidence has a role-invalid outcome".into(),
        ));
    }
    let reviewers = evidence
        .iter()
        .map(|validated| PlanReviewerOutcomeV2 {
            node_id: validated.evidence.binding.node_id.clone(),
            outcome: validated.evidence.intent.outcome,
            rank: reviewer_outcome_rank(validated.evidence.intent.outcome),
            evidence_task_id: validated.evidence.binding.task_id.clone(),
            evidence_scope_digest: validated.evidence.evidence_scope_digest.clone(),
        })
        .collect();
    let outcome = reduce_design_gate(&evidence);

    Ok(ValidatedV2GateEvidenceSet {
        identity,
        selected_node_ids: selected.into_iter().collect(),
        reviewers,
        outcome,
    })
}

fn design_outcome_to_settlement(
    outcome: CompletionOutcome,
) -> Result<GateSettlementOutcome, WorkflowStoreError> {
    match outcome {
        CompletionOutcome::Approve | CompletionOutcome::ApproveWithMinors => {
            Ok(GateSettlementOutcome::Approved)
        }
        CompletionOutcome::RequestChanges => Ok(GateSettlementOutcome::ChangesRequested),
        CompletionOutcome::Block => Ok(GateSettlementOutcome::Blocked),
        _ => Err(WorkflowStoreError::Persistence(
            "Design self-review outcome is role-invalid".into(),
        )),
    }
}

fn select_current_v2_plan_settlement<'a>(
    settlements: &'a [delegation_workflow_gate_settlement::Model],
    gate_id: &str,
    identity: &V2GateEvidenceIdentity,
) -> Option<&'a delegation_workflow_gate_settlement::Model> {
    settlements
        .iter()
        .find(|settlement| settlement.gate_id == gate_id && identity.matches_settlement(settlement))
}

#[allow(clippy::too_many_arguments)]
async fn verify_document_gate_ready<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    gate: &NormalizedGate,
    gate_cycle: i64,
    active_manifest_revision: i64,
    current_doc_digest: Option<&str>,
    current_content_fingerprint: &str,
    outcome: &GateSettlementOutcome,
    prior_settlement: Option<&delegation_workflow_gate_settlement::Model>,
) -> Result<(), WorkflowStoreError> {
    // A12: zero-reviewer Design self_review — no run set required.
    if gate.required_reviewer_node_ids.is_empty() {
        if gate.resolution_mode != ResolutionMode::SelfReview
            || gate.gate_kind != DocumentGateKind::Design
        {
            return Err(WorkflowStoreError::GateNotReady(
                "empty reviewer set only legal for Design self_review".into(),
            ));
        }
        return Ok(());
    }

    let prior_ts = prior_settlement.map(|s| s.created_at);
    let approving = *outcome == GateSettlementOutcome::Approved;

    for reviewer_id in &gate.required_reviewer_node_ids {
        let bindings = delegation_workflow_run_binding::Entity::find()
            .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.to_string()))
            .filter(delegation_workflow_run_binding::Column::NodeId.eq(reviewer_id.clone()))
            .filter(delegation_workflow_run_binding::Column::GateId.eq(gate.id.clone()))
            .filter(delegation_workflow_run_binding::Column::GateCycle.eq(gate_cycle))
            .all(conn)
            .await
            .map_err(db_err)?;

        let mut found_pass = false;
        let mut found_failed_terminal = false;

        for rb in &bindings {
            if !rb.summary_validated {
                continue;
            }
            // A2: bind to active document revision (cycle-1 intro + current).
            if rb.manifest_revision != active_manifest_revision {
                continue;
            }
            // Stale structural-generation runs (old plan/design fingerprint) do not count.
            if current_content_fingerprint.is_empty()
                || rb.content_fingerprint.as_deref() != Some(current_content_fingerprint)
            {
                continue;
            }
            // A2: current reviewed artifact digest for document gate.
            if let Some(expected) = current_doc_digest {
                match rb.artifact_digest.as_deref() {
                    Some(d) if d == expected => {}
                    _ => continue,
                }
            }
            // Freshness vs prior cycle settlement timestamp (N>1).
            if let Some(ts) = prior_ts {
                if rb.created_at <= ts {
                    continue;
                }
            }

            let run = delegation_task_run::Entity::find_by_id(rb.task_id.clone())
                .one(conn)
                .await
                .map_err(db_err)?;
            let Some(run) = run else {
                continue;
            };
            match run.status {
                DelegationRunStatus::Completed => {
                    found_pass = true;
                    break;
                }
                DelegationRunStatus::Failed | DelegationRunStatus::Canceled => {
                    found_failed_terminal = true;
                }
                DelegationRunStatus::Reserving | DelegationRunStatus::Running => {}
            }
        }

        if approving {
            if found_pass {
                continue;
            }
            if found_failed_terminal {
                return Err(WorkflowStoreError::ApprovalRejectedFailedReviewer {
                    node_id: reviewer_id.clone(),
                });
            }
            return Err(WorkflowStoreError::GateNotReady(format!(
                "reviewer node {reviewer_id} lacks a fresh completed run with validated summary bound to active revision/digest for gate {} cycle {gate_cycle}",
                gate.id
            )));
        }

        // Non-approve: completed OR failed/canceled terminal counts for adjudication.
        if !found_pass && !found_failed_terminal {
            return Err(WorkflowStoreError::GateNotReady(format!(
                "reviewer node {reviewer_id} lacks a fresh terminal run with validated summary bound to active revision/digest for gate {} cycle {gate_cycle}",
                gate.id
            )));
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn verify_plan_gate_ready<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    active_author_node_id: &str,
    gate: &NormalizedGate,
    gate_cycle: i64,
    active_manifest_revision: i64,
    current_content_fingerprint: &str,
    submission: &PlanReviewRoundSubmission,
    prior_settlement: Option<&delegation_workflow_gate_settlement::Model>,
) -> Result<(), WorkflowStoreError> {
    let author_binding = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .filter(
            delegation_workflow_run_binding::Column::NodeId.eq(active_author_node_id.to_string()),
        )
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .order_by_desc(delegation_workflow_run_binding::Column::CreatedAt)
        .order_by_desc(delegation_workflow_run_binding::Column::TaskId)
        .one(conn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| {
            WorkflowStoreError::GateNotReady(format!(
                "active Plan Author node {active_author_node_id} has no run binding"
            ))
        })?;
    let author_node = delegation_workflow_node_binding::Entity::find_by_id((
        workflow_id.to_string(),
        author_binding.node_id.clone(),
    ))
    .one(conn)
    .await
    .map_err(db_err)?
    .filter(|node| node.role == "author" && node.phase_id == super::types::PHASE_PLAN)
    .ok_or_else(|| {
        WorkflowStoreError::GateNotReady(
            "covered Author task is not bound to the active Plan Author node".into(),
        )
    })?;
    if author_node.node_id != active_author_node_id || author_node.retired_revision.is_some() {
        return Err(WorkflowStoreError::GateNotReady(
            "covered Plan Author node is retired".into(),
        ));
    }
    if author_binding.task_id != submission.covered_author_task_id {
        return Err(WorkflowStoreError::ReviewedTaskStale(format!(
            "covered Author task {} is not the latest active Plan Author task {}",
            submission.covered_author_task_id, author_binding.task_id
        )));
    }
    if !author_binding.summary_validated {
        return Err(WorkflowStoreError::GateNotReady(
            "covered Author evidence is not validated".into(),
        ));
    }
    if author_binding.artifact_digest.as_deref() != Some(submission.covered_plan_digest.as_str()) {
        return Err(WorkflowStoreError::ArtifactDigestMismatch(
            "Author evidence does not match the covered Plan digest".into(),
        ));
    }
    let author_run =
        delegation_task_run::Entity::find_by_id(submission.covered_author_task_id.clone())
            .one(conn)
            .await
            .map_err(db_err)?
            .filter(|run| run.status == DelegationRunStatus::Completed)
            .ok_or_else(|| {
                WorkflowStoreError::GateNotReady(
                    "covered Plan Author task is not infrastructure-successful".into(),
                )
            })?;
    match author_run
        .card_summary_json
        .as_deref()
        .and_then(parse_and_validate_summary_json)
    {
        Some(CardSummary::Author {
            status: WorkStatus::Done | WorkStatus::DoneWithConcerns,
            plan_digest,
            ..
        }) if plan_digest == submission.covered_plan_digest => {}
        Some(CardSummary::Author {
            status: WorkStatus::Done | WorkStatus::DoneWithConcerns,
            ..
        }) => {
            return Err(WorkflowStoreError::ArtifactDigestMismatch(
                "completed Plan Author card does not match the covered Plan digest".into(),
            ));
        }
        _ => {
            return Err(WorkflowStoreError::GateNotReady(
                "covered Plan Author task lacks matching completed Author evidence".into(),
            ));
        }
    }

    let prior_ts = prior_settlement.map(|settlement| settlement.created_at);
    for reviewer_id in &submission.required_reviewer_node_ids {
        let binding = delegation_workflow_run_binding::Entity::find()
            .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.to_string()))
            .filter(delegation_workflow_run_binding::Column::NodeId.eq(reviewer_id.clone()))
            .filter(delegation_workflow_run_binding::Column::GateId.eq(gate.id.clone()))
            .filter(delegation_workflow_run_binding::Column::GateCycle.eq(gate_cycle))
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
            .order_by_desc(delegation_workflow_run_binding::Column::CreatedAt)
            .order_by_desc(delegation_workflow_run_binding::Column::TaskId)
            .one(conn)
            .await
            .map_err(db_err)?
            .ok_or_else(|| {
                WorkflowStoreError::GateNotReady(format!(
                    "Plan reviewer {reviewer_id} has no binding for gate {} cycle {gate_cycle}",
                    gate.id
                ))
            })?;
        if binding.reviewed_task_id.as_deref() != Some(submission.covered_author_task_id.as_str()) {
            return Err(WorkflowStoreError::ReviewedTaskStale(format!(
                "Plan reviewer {reviewer_id} does not cover Author task {}",
                submission.covered_author_task_id
            )));
        }
        if binding.artifact_digest.as_deref() != Some(submission.covered_plan_digest.as_str()) {
            return Err(WorkflowStoreError::ArtifactDigestMismatch(format!(
                "Plan reviewer {reviewer_id} does not cover digest {}",
                submission.covered_plan_digest
            )));
        }
        let binding_matches = binding.summary_validated
            && binding.manifest_revision == active_manifest_revision
            && binding.content_fingerprint.as_deref() == Some(current_content_fingerprint)
            && prior_ts.is_none_or(|timestamp| binding.created_at > timestamp);
        let run = delegation_task_run::Entity::find_by_id(binding.task_id)
            .one(conn)
            .await
            .map_err(db_err)?;
        let infrastructure_complete = run.is_some_and(|run| {
            run.status == DelegationRunStatus::Completed
                && matches!(
                    run.card_summary_json
                        .as_deref()
                        .and_then(parse_and_validate_summary_json),
                    Some(CardSummary::Review { .. })
                )
        });
        if !binding_matches || !infrastructure_complete {
            return Err(WorkflowStoreError::GateNotReady(format!(
                "Plan reviewer {reviewer_id} lacks fresh infrastructure-successful evidence bound to Author task {} and digest {} for cycle {gate_cycle}",
                submission.covered_author_task_id, submission.covered_plan_digest
            )));
        }
    }
    Ok(())
}

fn settlement_payload_matches(
    existing: &delegation_workflow_gate_settlement::Model,
    req: &SettleWorkflowRequest,
) -> Result<bool, WorkflowStoreError> {
    if existing.outcome != req.outcome
        || existing.summary != req.summary
        || existing.manifest_revision as u64 != req.manifest_revision
        || existing.lineage_reset_authorization_id.as_deref()
            != req.recovery_authorization_id.as_deref()
    {
        return Ok(false);
    }
    if existing.gate_lineage.is_some() {
        return Ok(true);
    }
    match &req.evidence {
        SettleGateEvidence::Design {
            critical_count,
            important_count,
            minor_count,
        } => Ok(existing.review_scope.is_none()
            && existing.critical_count == Some(*critical_count)
            && existing.important_count == Some(*important_count)
            && existing.minor_count == Some(*minor_count)),
        SettleGateEvidence::Plan(submission) => Ok(existing.review_scope.is_some()
            && load_persisted_plan_evidence(existing)?.submission == *submission),
    }
}

fn settle_result_from_row(
    row: &delegation_workflow_gate_settlement::Model,
    graph_revision: u64,
    manifest_revision: u64,
    idempotent_replay: bool,
) -> Result<SettleResult, WorkflowStoreError> {
    let is_v2 = row.gate_lineage.is_some();
    let plan_state = if is_v2 {
        None
    } else {
        row.review_scope
            .as_ref()
            .map(|_| load_persisted_plan_evidence(row))
            .transpose()?
            .map(|evidence| evidence.state)
    };
    let (critical_count, important_count, minor_count) = if is_v2 {
        (0, 0, 0)
    } else {
        (
            row.critical_count.ok_or_else(|| {
                WorkflowStoreError::Persistence("settlement is missing critical count".into())
            })?,
            row.important_count.ok_or_else(|| {
                WorkflowStoreError::Persistence("settlement is missing important count".into())
            })?,
            row.minor_count.ok_or_else(|| {
                WorkflowStoreError::Persistence("settlement is missing minor count".into())
            })?,
        )
    };
    Ok(SettleResult {
        workflow_id: row.workflow_id.clone(),
        gate_id: row.gate_id.clone(),
        gate_cycle: row.gate_cycle as u64,
        graph_revision,
        manifest_revision,
        outcome: row.outcome.clone(),
        idempotent_replay,
        plan_next_action: if is_v2 {
            row.next_action.as_ref().map(plan_next_action_from_db)
        } else {
            plan_state.as_ref().map(|state| state.next_action)
        },
        critical_count,
        important_count,
        minor_count,
        stagnation_count: u32::try_from(row.stagnation_count).map_err(|_| {
            WorkflowStoreError::Persistence("invalid persisted Plan stagnation count".into())
        })?,
        rewrite_used: row.rewrite_used,
        plan_metric_observation: None,
    })
}

#[cfg(test)]
mod completion_v2_shared_validator_replay_tests {
    use super::super::plan_review::{FindingSeverity, FindingStatus};
    use super::*;

    fn v2_settlement(
        identity: &V2GateEvidenceIdentity,
    ) -> delegation_workflow_gate_settlement::Model {
        delegation_workflow_gate_settlement::Model {
            workflow_id: "workflow-v2".into(),
            gate_id: "plan".into(),
            gate_cycle: 1,
            manifest_revision: 2,
            structural_revision: 2,
            content_fingerprint: "legacy-inert".into(),
            evidence_scope_digest: Some(identity.aggregate_scope_digest.clone()),
            gate_lineage: Some(identity.gate_lineage.clone()),
            review_round: Some(identity.review_round),
            required_node_set_json: Some(
                serde_json::to_string(&identity.required_node_ids).unwrap(),
            ),
            required_evidence_task_ids_json: Some(
                serde_json::to_string(&identity.task_ids).unwrap(),
            ),
            evidence_scope_digests_json: Some(
                serde_json::to_string(&identity.scope_digests).unwrap(),
            ),
            localized_change_digest: None,
            plan_round_state_v2_json: None,
            outcome: GateSettlementOutcome::Approved,
            critical_count: None,
            important_count: None,
            minor_count: None,
            summary: "approved".into(),
            graph_revision_at_settle: 4,
            review_scope: None,
            revision_kind: None,
            scope_reason: None,
            required_reviewer_node_ids_json: None,
            covered_author_task_id: None,
            covered_plan_digest: None,
            finding_ledger_json: None,
            net_improvement: None,
            stagnation_count: 0,
            rewrite_used: false,
            next_action: None,
            report_files_json: None,
            lineage_reset_authorization_id: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn completion_v2_review_fixes_recovery_requires_current_round_identity() {
        let lineage = format!("sha256:{}", "a".repeat(64));
        let round_one = V2GateEvidenceIdentity::new(
            lineage.clone(),
            1,
            vec!["reviewer".into()],
            vec!["review-task-r1".into()],
            vec![format!("sha256:{}", "1".repeat(64))],
        )
        .unwrap();
        let settlement = v2_settlement(&round_one);
        let round_two = V2GateEvidenceIdentity::new(
            lineage,
            2,
            vec!["reviewer".into()],
            vec!["review-task-r2".into()],
            vec![format!("sha256:{}", "2".repeat(64))],
        )
        .unwrap();

        assert!(select_current_v2_plan_settlement(
            std::slice::from_ref(&settlement),
            "plan",
            &round_two,
        )
        .is_none());
        assert!(select_current_v2_plan_settlement(&[settlement], "plan", &round_one).is_some());
    }

    #[test]
    fn completion_v2_review_fixes_plan_payload_does_not_mint_legacy_round_state() {
        let caller_payload = PlanReviewRoundSubmission {
            scope: PlanReviewScope::Scoped,
            revision_kind: PlanRevisionKind::Localized,
            scope_reason: String::new(),
            covered_author_task_id: String::new(),
            covered_plan_digest: String::new(),
            required_reviewer_node_ids: vec!["reviewer".into()],
            finding_updates: vec![PlanFindingUpdate {
                finding_id: "parent-authored".into(),
                severity: FindingSeverity::Critical,
                status: FindingStatus::Open,
                owner_reviewer_node_ids: vec!["reviewer".into()],
                summary: "must remain legacy-only".into(),
                evidence_ref: "parent".into(),
                report_file: "reports/parent.md".into(),
            }],
            lineage_reset_reason: Some("parent requested reset".into()),
        };

        assert_eq!(
            derive_plan_review_state_for_protocol(2, None, &["reviewer".into()], &caller_payload,)
                .unwrap(),
            None
        );
        assert!(derive_plan_review_state_for_protocol(
            1,
            None,
            &["reviewer".into()],
            &caller_payload,
        )
        .is_err());
    }

    #[test]
    fn task14_v2_plan_state_replay_rejects_evidence_column_drift() {
        let identity = V2GateEvidenceIdentity::new(
            format!("sha256:{}", "a".repeat(64)),
            1,
            vec!["reviewer".into()],
            vec!["review-task".into()],
            vec![format!("sha256:{}", "1".repeat(64))],
        )
        .unwrap();
        let state = PlanReviewRoundStateV2 {
            gate_lineage: identity.gate_lineage.clone(),
            review_round: 1,
            required_node_ids: identity.required_node_ids.clone(),
            selected_node_ids: identity.required_node_ids.clone(),
            reviewers: vec![PlanReviewerOutcomeV2 {
                node_id: "reviewer".into(),
                outcome: CompletionOutcome::RequestChanges,
                rank: 1,
                evidence_task_id: "review-task".into(),
                evidence_scope_digest: format!("sha256:{}", "1".repeat(64)),
            }],
            stagnation_count: 0,
            rewrite_used: false,
            next_action: PlanReviewNextAction::ContinueReview,
            plan_snapshot: None,
            localized_change: None,
        };
        let mut row = v2_settlement(&identity);
        row.outcome = GateSettlementOutcome::ChangesRequested;
        row.plan_round_state_v2_json = Some(serde_json::to_string(&state).unwrap());
        row.next_action = Some(DbPlanReviewNextAction::ContinueReview);
        row.covered_author_task_id = Some("author-task".into());
        row.covered_plan_digest = Some(format!("sha256:{}", "2".repeat(64)));
        row.net_improvement = Some(false);

        assert_eq!(load_persisted_plan_state_v2(&row).unwrap(), state);
        row.required_evidence_task_ids_json = Some("[\"different-task\"]".into());
        assert!(load_persisted_plan_state_v2(&row).is_err());
    }

    #[test]
    fn completion_v2_shared_validator_replays_without_legacy_finding_counts() {
        let row = delegation_workflow_gate_settlement::Model {
            workflow_id: "workflow-v2".into(),
            gate_id: "design".into(),
            gate_cycle: 1,
            manifest_revision: 2,
            structural_revision: 2,
            content_fingerprint: "legacy-inert".into(),
            evidence_scope_digest: Some(format!("sha256:{}", "a".repeat(64))),
            gate_lineage: Some(format!("sha256:{}", "b".repeat(64))),
            review_round: Some(1),
            required_node_set_json: Some("[\"reviewer\"]".into()),
            required_evidence_task_ids_json: Some("[\"task\"]".into()),
            evidence_scope_digests_json: Some("[\"sha256:scope\"]".into()),
            localized_change_digest: None,
            plan_round_state_v2_json: None,
            outcome: GateSettlementOutcome::Approved,
            critical_count: None,
            important_count: None,
            minor_count: None,
            summary: "approved".into(),
            graph_revision_at_settle: 4,
            review_scope: None,
            revision_kind: None,
            scope_reason: None,
            required_reviewer_node_ids_json: None,
            covered_author_task_id: None,
            covered_plan_digest: None,
            finding_ledger_json: None,
            net_improvement: None,
            stagnation_count: 0,
            rewrite_used: false,
            next_action: None,
            report_files_json: None,
            lineage_reset_authorization_id: None,
            created_at: Utc::now(),
        };

        let replay = settle_result_from_row(&row, 5, 2, true).unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.critical_count, 0);
        assert_eq!(replay.important_count, 0);
        assert_eq!(replay.minor_count, 0);
    }
}

fn load_persisted_plan_evidence(
    row: &delegation_workflow_gate_settlement::Model,
) -> Result<PersistedPlanReviewEvidence, WorkflowStoreError> {
    let json = row.finding_ledger_json.as_deref().ok_or_else(|| {
        WorkflowStoreError::Persistence("Plan settlement is missing immutable evidence".into())
    })?;
    if json.len() > MAX_PERSISTED_PLAN_EVIDENCE_BYTES {
        return Err(WorkflowStoreError::Persistence(
            "persisted Plan evidence exceeds the bounded size".into(),
        ));
    }
    let evidence: PersistedPlanReviewEvidence = serde_json::from_str(json).map_err(|error| {
        WorkflowStoreError::Persistence(format!("parse persisted Plan evidence: {error}"))
    })?;
    let state = &evidence.state;
    let reviewer_ids_json =
        serde_json::to_string(&state.reviewed_reviewer_node_ids).map_err(|error| {
            WorkflowStoreError::Persistence(format!("serialize Plan reviewer set: {error}"))
        })?;
    let report_files_json = serialize_plan_report_files(&state.findings)?;
    if row.critical_count != Some(i64::from(state.critical_count))
        || row.important_count != Some(i64::from(state.important_count))
        || row.minor_count != Some(i64::from(state.minor_count))
        || row.stagnation_count != i64::from(state.stagnation_count)
        || row.rewrite_used != state.rewrite_used
        || row.next_action.as_ref().map(plan_next_action_from_db) != Some(state.next_action)
        || row.review_scope.as_ref().map(plan_scope_from_db) != Some(state.scope)
        || row.revision_kind.as_ref().map(plan_revision_kind_from_db) != Some(state.revision_kind)
        || row.scope_reason.as_deref() != Some(state.scope_reason.as_str())
        || row.required_reviewer_node_ids_json.as_deref() != Some(reviewer_ids_json.as_str())
        || row.covered_author_task_id.as_deref() != Some(state.covered_author_task_id.as_str())
        || row.covered_plan_digest.as_deref() != Some(state.covered_plan_digest.as_str())
        || row.net_improvement != Some(state.net_improvement)
        || row.report_files_json.as_deref() != Some(report_files_json.as_str())
    {
        return Err(WorkflowStoreError::Persistence(
            "persisted Plan settlement columns disagree with immutable evidence".into(),
        ));
    }
    Ok(evidence)
}

fn serialize_bounded_plan_evidence(
    evidence: &PersistedPlanReviewEvidence,
) -> Result<String, WorkflowStoreError> {
    let json = serde_json::to_string(evidence).map_err(|error| {
        WorkflowStoreError::Persistence(format!("serialize Plan evidence: {error}"))
    })?;
    if json.len() > MAX_PERSISTED_PLAN_EVIDENCE_BYTES {
        return Err(WorkflowStoreError::Persistence(
            "serialized Plan evidence exceeds the bounded size".into(),
        ));
    }
    Ok(json)
}

fn serialize_plan_report_files(
    findings: &[PlanFindingUpdate],
) -> Result<String, WorkflowStoreError> {
    let files: BTreeSet<&str> = findings
        .iter()
        .map(|finding| finding.report_file.as_str())
        .collect();
    serde_json::to_string(&files).map_err(|error| {
        WorkflowStoreError::Persistence(format!("serialize Plan report files: {error}"))
    })
}

fn derive_plan_review_state_for_protocol(
    completion_protocol_version: i64,
    prior: Option<&PlanReviewRoundState>,
    reviewer_cohort_node_ids: &[String],
    submission: &PlanReviewRoundSubmission,
) -> Result<Option<PlanReviewRoundState>, PlanReviewError> {
    if completion_protocol_version == 2 {
        return Ok(None);
    }
    derive_plan_review_round(prior, reviewer_cohort_node_ids, submission).map(Some)
}

fn load_persisted_plan_state_v2(
    row: &delegation_workflow_gate_settlement::Model,
) -> Result<PlanReviewRoundStateV2, WorkflowStoreError> {
    let json = row.plan_round_state_v2_json.as_deref().ok_or_else(|| {
        WorkflowStoreError::Persistence("v2 Plan settlement is missing round state".into())
    })?;
    if json.len() > MAX_PERSISTED_PLAN_EVIDENCE_BYTES {
        return Err(WorkflowStoreError::Persistence(
            "v2 Plan round state exceeds the bounded size".into(),
        ));
    }
    let state: PlanReviewRoundStateV2 = serde_json::from_str(json).map_err(|error| {
        WorkflowStoreError::Persistence(format!("parse v2 Plan round state: {error}"))
    })?;
    if let Some(snapshot) = state.plan_snapshot.as_ref() {
        decode_plan_snapshot_v2(snapshot)?;
        if row.covered_plan_digest.as_deref() != Some(snapshot.digest.as_str()) {
            return Err(WorkflowStoreError::Persistence(
                "v2 Plan snapshot disagrees with covered Plan digest".into(),
            ));
        }
    }
    let localized_change_digest = state
        .localized_change
        .as_ref()
        .map(|change| {
            canonical_json_sha256("codeg.completion.plan_change.v2", 1, change)
                .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))
        })
        .transpose()?;
    if row.localized_change_digest != localized_change_digest {
        return Err(WorkflowStoreError::Persistence(
            "v2 Plan localized-change proof disagrees with round state".into(),
        ));
    }
    let identity = V2GateEvidenceIdentity::new(
        state.gate_lineage.clone(),
        i64::from(state.review_round),
        state.required_node_ids.clone(),
        state
            .reviewers
            .iter()
            .map(|reviewer| reviewer.evidence_task_id.clone())
            .collect(),
        state
            .reviewers
            .iter()
            .map(|reviewer| reviewer.evidence_scope_digest.clone())
            .collect(),
    )
    .ok_or_else(|| {
        WorkflowStoreError::Persistence("v2 Plan round evidence identity is invalid".into())
    })?;
    if !identity.matches_settlement(row)
        || row.review_round != Some(i64::from(state.review_round))
        || row.required_node_set_json.as_deref()
            != Some(
                serde_json::to_string(&state.required_node_ids)
                    .map_err(|error| WorkflowStoreError::Persistence(error.to_string()))?
                    .as_str(),
            )
        || row.stagnation_count != i64::from(state.stagnation_count)
        || row.rewrite_used != state.rewrite_used
        || row.next_action.as_ref().map(plan_next_action_from_db) != Some(state.next_action)
        || row
            .covered_author_task_id
            .as_deref()
            .is_none_or(str::is_empty)
        || row.covered_plan_digest.as_deref().is_none_or(str::is_empty)
        || row.net_improvement.is_none()
    {
        return Err(WorkflowStoreError::Persistence(
            "v2 Plan settlement columns disagree with round state".into(),
        ));
    }
    Ok(state)
}

fn plan_state_matches_evidence(
    state: &PlanReviewRoundStateV2,
    evidence: &ValidatedV2GateEvidenceSet,
) -> bool {
    state.gate_lineage == evidence.identity.gate_lineage
        && i64::from(state.review_round) == evidence.identity.review_round
        && state.required_node_ids == evidence.identity.required_node_ids
        && state.selected_node_ids == evidence.selected_node_ids
        && state.reviewers == evidence.reviewers
}

fn plan_v2_settlement_outcome(next_action: PlanReviewNextAction) -> GateSettlementOutcome {
    match next_action {
        PlanReviewNextAction::Approved => GateSettlementOutcome::Approved,
        PlanReviewNextAction::ContinueReview | PlanReviewNextAction::HolisticRewriteRequired => {
            GateSettlementOutcome::ChangesRequested
        }
        PlanReviewNextAction::UserDecisionRequired => GateSettlementOutcome::Blocked,
    }
}

fn validate_plan_outcome(
    outcome: &GateSettlementOutcome,
    state: &PlanReviewRoundState,
) -> Result<(), WorkflowStoreError> {
    if *outcome == GateSettlementOutcome::Approved
        && (state.critical_count > 0 || state.important_count > 0)
    {
        return Err(WorkflowStoreError::ApprovalWithOpenFindings {
            critical: i64::from(state.critical_count),
            important: i64::from(state.important_count),
        });
    }
    if state.next_action == PlanReviewNextAction::Approved
        && *outcome != GateSettlementOutcome::Approved
    {
        return Err(WorkflowStoreError::GateCycleConflict(
            "derived approved Plan round requires outcome=approved".into(),
        ));
    }
    if state.next_action != PlanReviewNextAction::Approved
        && *outcome == GateSettlementOutcome::Approved
    {
        return Err(WorkflowStoreError::ApprovalWithOpenFindings {
            critical: i64::from(state.critical_count),
            important: i64::from(state.important_count),
        });
    }
    if state.next_action == PlanReviewNextAction::UserDecisionRequired
        && *outcome != GateSettlementOutcome::Blocked
    {
        return Err(WorkflowStoreError::GateCycleConflict(
            "user_decision_required Plan round must block the workflow".into(),
        ));
    }
    Ok(())
}

fn canonical_string_set(values: &[String]) -> Vec<String> {
    values
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn parse_plan_reviewer_ranks_v2(json: &str) -> Option<Vec<WorkflowRecoveryPlanReviewerRankV2>> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let reviewers = value.get("reviewers")?.as_array()?;
    let mut seen = HashSet::with_capacity(reviewers.len());
    let mut ranks = Vec::with_capacity(reviewers.len());
    for reviewer in reviewers {
        let node_id = reviewer.get("node_id")?.as_str()?.trim();
        let rank = u8::try_from(reviewer.get("rank")?.as_u64()?).ok()?;
        if node_id.is_empty() || rank > 2 || !seen.insert(node_id.to_string()) {
            return None;
        }
        ranks.push(WorkflowRecoveryPlanReviewerRankV2 {
            node_id: node_id.to_string(),
            rank,
        });
    }
    ranks.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    Some(ranks)
}

fn plan_scope_to_db(scope: PlanReviewScope) -> DbPlanReviewScope {
    match scope {
        PlanReviewScope::Full => DbPlanReviewScope::Full,
        PlanReviewScope::Scoped => DbPlanReviewScope::Scoped,
    }
}

fn plan_scope_from_db(scope: &DbPlanReviewScope) -> PlanReviewScope {
    match scope {
        DbPlanReviewScope::Full => PlanReviewScope::Full,
        DbPlanReviewScope::Scoped => PlanReviewScope::Scoped,
    }
}

fn plan_revision_kind_to_db(kind: PlanRevisionKind) -> DbPlanRevisionKind {
    match kind {
        PlanRevisionKind::Initial => DbPlanRevisionKind::Initial,
        PlanRevisionKind::Localized => DbPlanRevisionKind::Localized,
        PlanRevisionKind::Material => DbPlanRevisionKind::Material,
        PlanRevisionKind::HolisticRewrite => DbPlanRevisionKind::HolisticRewrite,
    }
}

fn plan_revision_kind_from_db(kind: &DbPlanRevisionKind) -> PlanRevisionKind {
    match kind {
        DbPlanRevisionKind::Initial => PlanRevisionKind::Initial,
        DbPlanRevisionKind::Localized => PlanRevisionKind::Localized,
        DbPlanRevisionKind::Material => PlanRevisionKind::Material,
        DbPlanRevisionKind::HolisticRewrite => PlanRevisionKind::HolisticRewrite,
    }
}

fn plan_next_action_to_db(action: PlanReviewNextAction) -> DbPlanReviewNextAction {
    match action {
        PlanReviewNextAction::ContinueReview => DbPlanReviewNextAction::ContinueReview,
        PlanReviewNextAction::HolisticRewriteRequired => {
            DbPlanReviewNextAction::HolisticRewriteRequired
        }
        PlanReviewNextAction::UserDecisionRequired => DbPlanReviewNextAction::UserDecisionRequired,
        PlanReviewNextAction::Approved => DbPlanReviewNextAction::Approved,
    }
}

fn plan_next_action_from_db(action: &DbPlanReviewNextAction) -> PlanReviewNextAction {
    match action {
        DbPlanReviewNextAction::ContinueReview => PlanReviewNextAction::ContinueReview,
        DbPlanReviewNextAction::HolisticRewriteRequired => {
            PlanReviewNextAction::HolisticRewriteRequired
        }
        DbPlanReviewNextAction::UserDecisionRequired => PlanReviewNextAction::UserDecisionRequired,
        DbPlanReviewNextAction::Approved => PlanReviewNextAction::Approved,
    }
}

#[cfg(any(test, feature = "test-utils"))]
fn compute_supersedes(
    prior: Option<&delegation_workflow::Model>,
    new_state: ManifestWorkflowState,
    _next_rev: i64,
) -> Option<i64> {
    let prior = prior?;
    if prior.workflow_state == WorkflowState::Approved
        && matches!(
            new_state,
            ManifestWorkflowState::Estimated | ManifestWorkflowState::Skeleton
        )
    {
        return Some(prior.active_manifest_revision);
    }
    prior.supersedes_approved_revision
}

/// True when Plan material structure changed between revisions (A8).
#[cfg(any(test, feature = "test-utils"))]
fn plan_structure_changed(prior: &NormalizedManifest, next: &NormalizedManifest) -> bool {
    plan_structure_fingerprint(prior) != plan_structure_fingerprint(next)
}

pub(crate) fn design_fingerprint_hash(m: &NormalizedManifest) -> String {
    sha256_hex(design_structure_fingerprint(m).as_bytes())
}

pub(crate) fn plan_fingerprint_hash(m: &NormalizedManifest) -> String {
    sha256_hex(plan_structure_fingerprint(m).as_bytes())
}

fn gate_content_fingerprint(kind: DocumentGateKind, header: &delegation_workflow::Model) -> String {
    match kind {
        DocumentGateKind::Design => header.design_fingerprint.clone(),
        DocumentGateKind::Plan => header.plan_fingerprint.clone(),
    }
}

/// Design-side fingerprint: design path + digest (A2 freshness), design gates,
/// and design work units. Digest is required so Design-only content edits
/// invalidate Design settlements. Plan material is excluded so Plan-only
/// rewrites do not invalidate Design.
fn design_structure_fingerprint(m: &NormalizedManifest) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    match &m.design {
        Some(d) => {
            let _ = writeln!(out, "design|{}|{}", d.rel_path, d.digest);
        }
        None => out.push_str("design|none\n"),
    }
    let mut design_gates: Vec<&NormalizedGate> = m
        .gates
        .iter()
        .filter(|g| g.gate_kind == DocumentGateKind::Design)
        .collect();
    design_gates.sort_by(|a, b| a.id.cmp(&b.id));
    for g in design_gates {
        let mut reviewers = g.required_reviewer_node_ids.clone();
        reviewers.sort();
        let _ = writeln!(
            out,
            "gate|{}|{:?}|{}",
            g.id,
            g.resolution_mode,
            reviewers.join(",")
        );
    }
    let mut nodes: Vec<&NormalizedNode> = m
        .nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, ManifestNodeKind::WorkUnit)
                && n.phase_id.as_deref() == Some(super::types::PHASE_DESIGN)
        })
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    for n in nodes {
        let mut deps = n.deps.clone();
        deps.sort();
        let _ = writeln!(
            out,
            "node|{}|{:?}|{:?}|{:?}|{:?}|req={}|title={:?}|deps={}",
            n.id,
            n.role,
            n.agent_type,
            n.profile_id,
            n.work_unit_key,
            n.required,
            n.title,
            deps.join(","),
        );
    }
    out
}

/// Material Plan fingerprint: plan digests, **design document identity**
/// (rel_path + digest), Plan gates, Plan/Task/Final work units (deps, titles,
/// required), and edges that touch those nodes.
///
/// Design path+digest is included so Design content changes demote Plan and
/// invalidate Plan settlements. Design *gate* fingerprint still excludes plan
/// material so Plan-only rewrites do not invalidate Design.
fn plan_structure_fingerprint(m: &NormalizedManifest) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "target|{}", m.plan_target_rel_path);
    let _ = writeln!(out, "risk_policy|{}", m.risk_policy_version);
    match &m.plan {
        Some(p) => {
            let _ = writeln!(out, "plan|{}|{}", p.rel_path, p.digest);
        }
        None => out.push_str("plan|none\n"),
    }
    // Design document identity is a material input to Plan.
    match &m.design {
        Some(d) => {
            let _ = writeln!(out, "design_doc|{}|{}", d.rel_path, d.digest);
        }
        None => out.push_str("design_doc|none\n"),
    }

    let mut plan_gates: Vec<&NormalizedGate> = m
        .gates
        .iter()
        .filter(|g| g.gate_kind == DocumentGateKind::Plan)
        .collect();
    plan_gates.sort_by(|a, b| a.id.cmp(&b.id));
    for g in plan_gates {
        let mut cohort = g.reviewer_cohort_node_ids.clone();
        cohort.sort();
        let mut reviewers = g.required_reviewer_node_ids.clone();
        reviewers.sort();
        let _ = writeln!(
            out,
            "gate|{}|{:?}|cohort={}|required={}",
            g.id,
            g.resolution_mode,
            cohort.join(","),
            reviewers.join(",")
        );
    }

    let mut task_policies = m.task_policies.clone();
    task_policies.sort_by_key(|policy| policy.task_index);
    for policy in task_policies {
        let policy_json = serde_json::to_string(&policy)
            .expect("validated Task policy must serialize for Plan fingerprint");
        let _ = writeln!(out, "task_policy|{policy_json}");
    }

    let material_phases = [
        super::types::PHASE_PLAN,
        super::types::PHASE_TASKS,
        super::types::PHASE_FINAL,
    ];
    let mut nodes: Vec<&NormalizedNode> = m
        .nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, ManifestNodeKind::WorkUnit)
                && n.phase_id
                    .as_deref()
                    .is_some_and(|p| material_phases.contains(&p))
        })
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    let material_ids: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    for n in &nodes {
        let mut deps = n.deps.clone();
        deps.sort();
        let _ = writeln!(
            out,
            "node|{}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|req={}|title={:?}|deps={}|outcome={:?}",
            n.id,
            n.phase_id,
            n.role,
            n.agent_type,
            n.profile_id,
            n.task_index,
            n.work_unit_key,
            n.required,
            n.title,
            deps.join(","),
            n.node_outcome
        );
    }

    let mut edges: Vec<&super::types::ManifestEdge> = m
        .edges
        .iter()
        .filter(|e| material_ids.contains(e.from.as_str()) || material_ids.contains(e.to.as_str()))
        .collect();
    edges.sort_by(|a, b| {
        a.from
            .cmp(&b.from)
            .then_with(|| a.to.cmp(&b.to))
            .then_with(|| a.id.cmp(&b.id))
    });
    for e in edges {
        let _ = writeln!(
            out,
            "edge|{:?}|{}|{}",
            e.id.as_deref().unwrap_or(""),
            e.from,
            e.to
        );
    }
    out
}

pub(crate) fn normalized_to_document(m: &NormalizedManifest) -> ManifestDocument {
    ManifestDocument {
        schema_version: m.schema_version,
        workflow_kind: m.workflow_kind.clone(),
        plan_target_rel_path: m.plan_target_rel_path.clone(),
        risk_policy_version: m.risk_policy_version.clone(),
        workflow_id: m.workflow_id.clone(),
        expected_manifest_revision: m.expected_manifest_revision,
        publication_token: m.publication_token.clone(),
        workflow_state: m.workflow_state,
        design: m.design.clone(),
        plan: m.plan.clone(),
        phases: m.phases.clone(),
        nodes: m
            .nodes
            .iter()
            .map(|n| ManifestNode {
                id: n.id.clone(),
                kind: n.kind,
                phase_id: n.phase_id.clone(),
                role: n.role,
                agent_type: n.agent_type.clone(),
                profile_id: n.profile_id.clone(),
                task_index: n.task_index,
                work_unit_key: n.work_unit_key.clone(),
                deps: n.deps.clone(),
                required: Some(n.required),
                node_outcome: n.node_outcome,
                title: n.title.clone(),
            })
            .collect(),
        edges: m.edges.clone(),
        gates: m
            .gates
            .iter()
            .map(|g| super::types::ManifestGate {
                id: g.id.clone(),
                reviewer_cohort_node_ids: g.reviewer_cohort_node_ids.clone(),
                required_reviewer_node_ids: g.required_reviewer_node_ids.clone(),
                resolution_mode: g.resolution_mode,
                gate_kind: Some(g.gate_kind),
            })
            .collect(),
        task_policies: m.task_policies.clone(),
    }
}

#[cfg(any(test, feature = "test-utils"))]
fn manifest_document_digest_with_state(
    normalized: &NormalizedManifest,
    workflow_state: ManifestWorkflowState,
) -> Result<String, WorkflowStoreError> {
    let mut document = normalized_to_document(normalized);
    document.workflow_state = workflow_state;
    let document_json = serde_json::to_string(&document)
        .map_err(|error| WorkflowStoreError::Persistence(format!("serialize manifest: {error}")))?;
    Ok(sha256_hex(document_json.as_bytes()))
}

fn manifests_equal_except_state_authority(
    prior: &NormalizedManifest,
    requested: &NormalizedManifest,
) -> bool {
    let mut prior = normalized_to_document(prior);
    let mut requested = normalized_to_document(requested);
    prior.workflow_id = None;
    requested.workflow_id = None;
    prior.expected_manifest_revision = None;
    requested.expected_manifest_revision = None;
    prior.workflow_state = ManifestWorkflowState::Blocked;
    requested.workflow_state = ManifestWorkflowState::Blocked;
    prior == requested
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn manifest_state_to_db(s: ManifestWorkflowState) -> WorkflowState {
    match s {
        ManifestWorkflowState::Skeleton => WorkflowState::Skeleton,
        ManifestWorkflowState::Estimated => WorkflowState::Estimated,
        ManifestWorkflowState::Approved => WorkflowState::Approved,
        ManifestWorkflowState::Blocked => WorkflowState::Blocked,
    }
}

fn workflow_state_to_manifest(s: WorkflowState) -> ManifestWorkflowState {
    match s {
        WorkflowState::Skeleton => ManifestWorkflowState::Skeleton,
        WorkflowState::Estimated => ManifestWorkflowState::Estimated,
        WorkflowState::Approved => ManifestWorkflowState::Approved,
        WorkflowState::Blocked => ManifestWorkflowState::Blocked,
    }
}

fn manifest_state_str(s: ManifestWorkflowState) -> &'static str {
    match s {
        ManifestWorkflowState::Skeleton => "skeleton",
        ManifestWorkflowState::Estimated => "estimated",
        ManifestWorkflowState::Approved => "approved",
        ManifestWorkflowState::Blocked => "blocked",
    }
}

fn role_str(r: ManifestNodeRole) -> &'static str {
    match r {
        ManifestNodeRole::Author => "author",
        ManifestNodeRole::Reviewer => "reviewer",
        ManifestNodeRole::Implementer => "implementer",
        ManifestNodeRole::Fixer => "fixer",
    }
}

fn resolution_mode_str(m: ResolutionMode) -> &'static str {
    match m {
        ResolutionMode::ParentAdjudication => "parent_adjudication",
        ResolutionMode::SelfReview => "self_review",
    }
}

fn settlement_outcome_str(o: &GateSettlementOutcome) -> &'static str {
    match o {
        GateSettlementOutcome::Approved => "approved",
        GateSettlementOutcome::ChangesRequested => "changes_requested",
        GateSettlementOutcome::Blocked => "blocked",
    }
}

fn run_status_str(s: &DelegationRunStatus) -> &'static str {
    match s {
        DelegationRunStatus::Reserving => "reserving",
        DelegationRunStatus::Running => "running",
        DelegationRunStatus::Completed => "completed",
        DelegationRunStatus::Failed => "failed",
        DelegationRunStatus::Canceled => "canceled",
    }
}

fn recovery_card_fields(
    run: Option<&delegation_task_run::Model>,
) -> (Option<String>, Option<String>) {
    match run
        .and_then(|run| run.card_summary_json.as_deref())
        .and_then(parse_and_validate_summary_json)
    {
        Some(CardSummary::Review {
            verdict,
            report_file,
            ..
        }) => (Some(review_verdict_str(verdict).into()), report_file),
        Some(CardSummary::Author { report_file, .. }) => (None, Some(report_file)),
        Some(CardSummary::Implementation { report_file, .. }) => (None, report_file),
        None => (None, None),
    }
}

fn constrain_plan_recovery_sources(index: &mut WorkflowStateIndexDto) {
    let required_reviewer_node_ids = index
        .gates
        .iter()
        .find(|gate| gate.gate_kind == "plan")
        .map(|gate| gate.required_reviewer_node_ids.clone())
        .unwrap_or_default();
    let Some(review) = index.latest_plan_review.as_mut() else {
        return;
    };

    let fallback = review
        .recovery_sources
        .iter()
        .find(|source| {
            !required_reviewer_node_ids.contains(&source.node_id)
                && (source.report_file.is_some() || source.latest_task_id.is_some())
        })
        .cloned();
    let mut authoritative_sources = required_reviewer_node_ids
        .iter()
        .map(|node_id| {
            review
                .recovery_sources
                .iter()
                .find(|source| source.node_id == *node_id)
                .cloned()
                .unwrap_or_else(|| PlanRecoverySourceDto {
                    node_id: node_id.clone(),
                    report_file: None,
                    latest_task_id: None,
                    child_conversation_id: None,
                })
        })
        .collect::<Vec<_>>();

    let has_current_pointer = authoritative_sources
        .iter()
        .any(|source| source.report_file.is_some() || source.latest_task_id.is_some());
    if !has_current_pointer {
        if let (Some(fallback), Some(first)) = (fallback, authoritative_sources.first_mut()) {
            first.report_file = fallback.report_file;
            first.latest_task_id = fallback.latest_task_id;
            first.child_conversation_id = fallback.child_conversation_id;
        }
    }
    review.recovery_sources = authoritative_sources;
}

fn review_verdict_str(verdict: ReviewVerdict) -> &'static str {
    match verdict {
        ReviewVerdict::Approve => "approve",
        ReviewVerdict::ApproveWithMinors => "approve_with_minors",
        ReviewVerdict::RequestChanges => "request_changes",
        ReviewVerdict::Block => "block",
    }
}

// ---------------------------------------------------------------------------
// Tests (B10 owned by Task 3)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::settle_workflow_gate_v2_from_fixture as settle_workflow_gate_core;
    use super::*;
    use crate::acp::delegation::workflow::events::WORKFLOW_GRAPH_CHANGED_EVENT as CHANGED;
    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::types::{
        DocumentRef, ManifestEdge, ManifestGate, ManifestNode, ManifestPhase,
        ManifestTaskHardTrigger, ManifestTaskPolicy, ManifestTaskRisk, ManifestTaskRoute,
        ManifestTaskSoftSignal, TaskHardTriggerKind, TaskRiskLevel, TaskSoftSignalKind,
        WorkUnitKeyParts, WorkflowError, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL,
        PHASE_PLAN, PHASE_TASKS,
    };
    use crate::acp::delegation::workflow::{
        FindingSeverity, FindingStatus, PlanFindingUpdate, PlanReviewNextAction,
        PlanReviewRoundSubmission, PlanReviewScope, PlanRevisionKind, WorkflowStateDetail,
    };
    use crate::db::entities::delegation_task_run::AdmissionClass;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;
    use crate::web::event_bridge::WebEventBroadcaster;
    use std::sync::Arc;

    async fn settle_workflow_gate_v2_core(
        db: &AppDatabase,
        emitter: &EventEmitter,
        parent_conversation_id: i32,
        request: SettleWorkflowV2Request,
    ) -> Result<SettleResult, WorkflowStoreError> {
        crate::acp::delegation::workflow::with_historical_workflow_fixture_mutations(
            super::settle_workflow_gate_v2_core(db, emitter, parent_conversation_id, request),
        )
        .await
    }

    async fn recover_workflow_core(
        db: &AppDatabase,
        emitter: &EventEmitter,
        parent_conversation_id: i32,
        request: RecoverWorkflowRequest,
    ) -> Result<RecoverWorkflowResult, WorkflowStoreError> {
        crate::acp::delegation::workflow::with_historical_workflow_fixture_mutations(
            super::recover_workflow_core(db, emitter, parent_conversation_id, request),
        )
        .await
    }

    async fn guard_final_delivery_core(
        db: &AppDatabase,
        emitter: &EventEmitter,
        request: FinalDeliveryGuardRequest,
    ) -> Result<FinalDeliveryGuardResult, WorkflowStoreError> {
        crate::acp::delegation::workflow::with_historical_workflow_fixture_mutations(
            super::guard_final_delivery_core(db, emitter, request),
        )
        .await
    }

    async fn append_state_only_revision_txn(
        txn: &DatabaseTransaction,
        header: &delegation_workflow::Model,
        request: StateOnlyRevisionRequest<'_>,
        now: DateTime<Utc>,
    ) -> Result<StateOnlyRevisionResult, WorkflowStoreError> {
        crate::acp::delegation::workflow::with_historical_workflow_fixture_mutations(
            super::append_state_only_revision_txn(txn, header, request, now),
        )
        .await
    }

    async fn append_workflow_block_revision_txn(
        txn: &DatabaseTransaction,
        header: &delegation_workflow::Model,
        request: WorkflowBlockEntryRequest<'_>,
        now: DateTime<Utc>,
    ) -> Result<StateOnlyRevisionResult, WorkflowStoreError> {
        crate::acp::delegation::workflow::with_historical_workflow_fixture_mutations(
            super::append_workflow_block_revision_txn(txn, header, request, now),
        )
        .await
    }

    #[test]
    fn header_db_error_classification() {
        let permanent = [
            sea_orm::DbErr::Type("invalid completion_protocol_mode".into()),
            sea_orm::DbErr::TryIntoErr {
                from: "String",
                into: "CompletionProtocolMode",
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unknown enum value",
                )),
            },
        ];
        for error in permanent {
            let mapped = map_completion_protocol_header_db_error(error);
            assert!(matches!(
                mapped,
                WorkflowStoreError::UnsupportedCompletionProtocolHeader(_)
            ));
            assert_eq!(mapped.code(), "unsupported_completion_protocol");
            assert!(!mapped.is_retryable());
        }

        let infrastructure = [
            sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout),
            sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::ConnectionClosed),
            sea_orm::DbErr::Conn(sea_orm::RuntimeErr::Internal("closed connection".into())),
            sea_orm::DbErr::Query(sea_orm::RuntimeErr::Internal("query failure".into())),
            sea_orm::DbErr::Exec(sea_orm::RuntimeErr::Internal("database is locked".into())),
        ];
        for error in infrastructure {
            let mapped = map_completion_protocol_header_db_error(error);
            assert!(matches!(mapped, WorkflowStoreError::Persistence(_)));
            assert_eq!(mapped.code(), "workflow_persistence_failure");
            assert!(mapped.is_retryable());
        }
    }

    #[test]
    fn design_preflight_completion_protocol_errors_keep_stable_classification() {
        let read_only = map_design_preflight_completion_error(
            super::super::completion_evidence::CompletionMutationError::Protocol {
                code: "legacy_completion_protocol_read_only",
                message: "legacy workflow is read-only".into(),
            },
        );
        assert_eq!(read_only.code(), "legacy_completion_protocol_read_only");
        assert!(!read_only.is_retryable());

        let unsupported = map_design_preflight_completion_error(
            super::super::completion_evidence::CompletionMutationError::Evidence(
                super::super::error::CompletionEvidenceError::Protocol {
                    code: "unsupported_completion_protocol",
                    message: "completion protocol header is corrupt".into(),
                },
            ),
        );
        assert_eq!(unsupported.code(), "unsupported_completion_protocol");
        assert!(!unsupported.is_retryable());
    }

    fn emitter_with_rx() -> (
        EventEmitter,
        tokio::sync::broadcast::Receiver<crate::web::event_bridge::WebEvent>,
    ) {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let rx = broadcaster.subscribe();
        let emitter = EventEmitter::test_web_only(broadcaster);
        (emitter, rx)
    }

    async fn set_initialized_gate_state(
        db: &AppDatabase,
        workflow_id: &str,
        gate_id: &str,
        gate_lineage: String,
        current_review_round: i64,
        selected_node_ids_json: String,
    ) {
        let state = delegation_workflow_gate_state::Entity::find_by_id((
            workflow_id.to_string(),
            gate_id.to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .expect("fixed-v2 publication initializes gate state");
        let mut state: delegation_workflow_gate_state::ActiveModel = state.into();
        state.gate_lineage = Set(gate_lineage);
        state.current_review_round = Set(current_review_round);
        state.selected_node_ids_json = Set(selected_node_ids_json);
        state.update(&db.conn).await.unwrap();
    }

    fn design_plan_doc(token: &str) -> ManifestDocument {
        let design_path = "docs/superpowers/specs/x.md";
        let plan_path = "docs/superpowers/plans/p.md";
        let design_key = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: design_path,
            agent_type: "code_buddy",
            profile_id: Some("a1c14cde-f9c0-4fce-9d7f-66c3f8e85039"),
        })
        .unwrap();
        let plan_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: plan_path,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: plan_path,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let task_impl = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let task_rev = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let final_rev = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let final_fix = build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();

        ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.to_string(),
            plan_target_rel_path: plan_path.into(),
            risk_policy_version: "b2d_task_risk_v1".into(),
            workflow_id: None,
            expected_manifest_revision: None,
            publication_token: token.into(),
            workflow_state: ManifestWorkflowState::Estimated,
            design: Some(DocumentRef {
                rel_path: design_path.into(),
                digest: "sha256:2c1f01e37a150fd02e10dd63ce8a268c168a68813b40f16f18c3430319073ce6"
                    .into(),
            }),
            plan: Some(DocumentRef {
                rel_path: plan_path.into(),
                digest: "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7"
                    .into(),
            }),
            phases: vec![
                phase(PHASE_DESIGN),
                phase(PHASE_PLAN),
                phase(PHASE_TASKS),
                phase(PHASE_FINAL),
            ],
            nodes: vec![
                wu(
                    "design-reviewer-1",
                    PHASE_DESIGN,
                    ManifestNodeRole::Reviewer,
                    "code_buddy",
                    Some("a1c14cde-f9c0-4fce-9d7f-66c3f8e85039"),
                    None,
                    design_key,
                    vec![],
                ),
                wu(
                    "plan-reviewer-1",
                    PHASE_PLAN,
                    ManifestNodeRole::Reviewer,
                    "codex",
                    None,
                    None,
                    plan_key,
                    vec!["design-reviewer-1".into()],
                ),
                wu(
                    "task-1-impl",
                    PHASE_TASKS,
                    ManifestNodeRole::Implementer,
                    "grok",
                    None,
                    Some(1),
                    task_impl,
                    vec!["plan-reviewer-1".into()],
                ),
                wu(
                    "task-1-rev",
                    PHASE_TASKS,
                    ManifestNodeRole::Reviewer,
                    "codex",
                    None,
                    Some(1),
                    task_rev,
                    vec!["task-1-impl".into()],
                ),
                wu(
                    "final-reviewer",
                    PHASE_FINAL,
                    ManifestNodeRole::Reviewer,
                    "codex",
                    None,
                    None,
                    final_rev,
                    vec!["task-1-rev".into()],
                ),
                wu(
                    "final-fixer",
                    PHASE_FINAL,
                    ManifestNodeRole::Fixer,
                    "grok",
                    None,
                    None,
                    final_fix,
                    vec!["final-reviewer".into()],
                ),
                wu(
                    "plan-author",
                    PHASE_PLAN,
                    ManifestNodeRole::Author,
                    "codex",
                    None,
                    None,
                    author_key,
                    vec![],
                ),
            ],
            edges: vec![ManifestEdge {
                id: Some("e1".into()),
                from: "task-1-impl".into(),
                to: "task-1-rev".into(),
            }],
            gates: vec![
                ManifestGate {
                    id: "design".into(),
                    reviewer_cohort_node_ids: vec!["design-reviewer-1".into()],
                    required_reviewer_node_ids: vec!["design-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some(DocumentGateKind::Design),
                },
                ManifestGate {
                    id: "plan".into(),
                    reviewer_cohort_node_ids: vec!["plan-reviewer-1".into()],
                    required_reviewer_node_ids: vec!["plan-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some(DocumentGateKind::Plan),
                },
            ],
            task_policies: vec![ManifestTaskPolicy {
                task_index: 1,
                risk: ManifestTaskRisk {
                    level: TaskRiskLevel::Normal,
                    hard_triggers: vec![],
                    soft_signals: vec![],
                    score: 0,
                    reason: "normal fixture".into(),
                },
                route: ManifestTaskRoute {
                    implementer_node_id: "task-1-impl".into(),
                    reviewer_node_ids: vec!["task-1-rev".into()],
                },
                allow_noop_verification: false,
            }],
        }
    }

    fn zero_reviewer_design_doc(token: &str) -> ManifestDocument {
        let mut doc = design_plan_doc(token);
        // Remove design reviewer node; Design gate becomes self_review.
        doc.nodes.retain(|n| n.id != "design-reviewer-1");
        // Fix plan deps — no design-reviewer dep.
        for n in &mut doc.nodes {
            n.deps.retain(|d| d != "design-reviewer-1");
        }
        doc.gates = vec![
            ManifestGate {
                id: "design".into(),
                reviewer_cohort_node_ids: vec![],
                required_reviewer_node_ids: vec![],
                resolution_mode: ResolutionMode::SelfReview,
                gate_kind: Some(DocumentGateKind::Design),
            },
            ManifestGate {
                id: "plan".into(),
                reviewer_cohort_node_ids: vec!["plan-reviewer-1".into()],
                required_reviewer_node_ids: vec!["plan-reviewer-1".into()],
                resolution_mode: ResolutionMode::ParentAdjudication,
                gate_kind: Some(DocumentGateKind::Plan),
            },
        ];
        doc
    }

    fn skeleton_doc(token: &str) -> ManifestDocument {
        let mut doc = design_plan_doc(token);
        doc.workflow_state = ManifestWorkflowState::Skeleton;
        doc.plan = None;
        doc.nodes.retain(|node| {
            node.id == "design-reviewer-1" || node.role == Some(ManifestNodeRole::Author)
        });
        doc.edges.clear();
        doc.gates
            .retain(|gate| gate.gate_kind == Some(DocumentGateKind::Design));
        doc.task_policies.clear();
        doc
    }

    fn two_reviewer_plan_doc(token: &str) -> ManifestDocument {
        let mut doc = design_plan_doc(token);
        let plan_path = doc.plan_target_rel_path.clone();
        let key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: &plan_path,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        doc.nodes.push(wu(
            "plan-reviewer-2",
            PHASE_PLAN,
            ManifestNodeRole::Reviewer,
            "grok",
            None,
            None,
            key,
            vec!["plan-author".into()],
        ));
        let gate = doc
            .gates
            .iter_mut()
            .find(|gate| gate.gate_kind == Some(DocumentGateKind::Plan))
            .unwrap();
        gate.reviewer_cohort_node_ids = vec!["plan-reviewer-1".into(), "plan-reviewer-2".into()];
        gate.required_reviewer_node_ids = gate.reviewer_cohort_node_ids.clone();
        doc
    }

    #[test]
    fn task14_fix_plan_settlement_uses_classifier_selected_reviewers() {
        let manifest =
            validate_manifest_document(&two_reviewer_plan_doc("task14-localized-classifier"))
                .unwrap();
        let prior_bytes = b"## Task 1\nprior body\n";
        let current_bytes = b"## Task 1\nchanged body\n";
        let snapshot = |bytes: &[u8]| PlanArtifactSnapshotV2 {
            rel_path: "docs/superpowers/plans/p.md".into(),
            digest: format!("sha256:{}", sha256_hex(bytes)),
            content_base64: BASE64_STANDARD.encode(bytes),
        };
        let prior_state = PlanReviewRoundStateV2 {
            gate_lineage: format!("sha256:{}", "a".repeat(64)),
            review_round: 1,
            required_node_ids: vec!["plan-reviewer-1".into(), "plan-reviewer-2".into()],
            selected_node_ids: vec!["plan-reviewer-1".into(), "plan-reviewer-2".into()],
            reviewers: vec![
                PlanReviewerOutcomeV2 {
                    node_id: "plan-reviewer-1".into(),
                    outcome: CompletionOutcome::Done,
                    rank: 0,
                    evidence_task_id: "review-task-1".into(),
                    evidence_scope_digest: format!("sha256:{}", "1".repeat(64)),
                },
                PlanReviewerOutcomeV2 {
                    node_id: "plan-reviewer-2".into(),
                    outcome: CompletionOutcome::RequestChanges,
                    rank: 1,
                    evidence_task_id: "review-task-2".into(),
                    evidence_scope_digest: format!("sha256:{}", "2".repeat(64)),
                },
            ],
            stagnation_count: 0,
            rewrite_used: false,
            next_action: PlanReviewNextAction::ContinueReview,
            plan_snapshot: Some(snapshot(prior_bytes)),
            localized_change: None,
        };

        let classification = classify_plan_settlement_change_v2(
            &manifest,
            &prior_state,
            &snapshot(current_bytes),
            "task14-localized-authorization",
        )
        .unwrap();

        let PlanChangeClassification::Localized { change, .. } = &classification else {
            panic!("expected a localized Plan change");
        };
        assert_eq!(change.changed_keys, BTreeSet::from(["task.1".to_string()]));
        assert_eq!(
            select_corrective_reviewers(&classification),
            BTreeSet::from(["plan-reviewer-1".to_string(), "plan-reviewer-2".to_string(),])
        );
    }

    fn finding(
        finding_id: &str,
        severity: FindingSeverity,
        status: FindingStatus,
        owners: &[&str],
    ) -> PlanFindingUpdate {
        PlanFindingUpdate {
            finding_id: finding_id.into(),
            severity,
            status,
            owner_reviewer_node_ids: owners.iter().map(|owner| (*owner).into()).collect(),
            summary: format!("summary for {finding_id}"),
            evidence_ref: format!("evidence/{finding_id}"),
            report_file: format!("reports/{finding_id}.md"),
        }
    }

    fn plan_submission(
        scope: PlanReviewScope,
        revision_kind: PlanRevisionKind,
        required: &[&str],
        findings: Vec<PlanFindingUpdate>,
        author_task_id: &str,
        plan_digest: &str,
    ) -> PlanReviewRoundSubmission {
        PlanReviewRoundSubmission {
            scope,
            revision_kind,
            scope_reason: "review the current Author artifact".into(),
            covered_author_task_id: author_task_id.into(),
            covered_plan_digest: plan_digest.into(),
            required_reviewer_node_ids: required
                .iter()
                .map(|reviewer| (*reviewer).into())
                .collect(),
            finding_updates: findings,
            lineage_reset_reason: None,
        }
    }

    type TestGateEvidence = SettleGateEvidence;

    #[derive(Debug)]
    struct TestSettleResult {
        idempotent_replay: bool,
        graph_revision: u64,
        outcome: GateSettlementOutcome,
        plan_next_action: Option<PlanReviewNextAction>,
        critical_count: i64,
        important_count: i64,
        minor_count: i64,
        stagnation_count: u32,
        rewrite_used: bool,
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_for_test(
        db: &AppDatabase,
        emitter: &EventEmitter,
        parent: i32,
        workflow_id: &str,
        gate_id: &str,
        manifest_revision: u64,
        expected_graph_revision: u64,
        gate_cycle: u64,
        outcome: GateSettlementOutcome,
        evidence: TestGateEvidence,
        summary: &str,
    ) -> Result<TestSettleResult, WorkflowStoreError> {
        let result = settle_workflow_gate_core(
            db,
            emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: workflow_id.into(),
                manifest_revision,
                gate_id: gate_id.into(),
                expected_graph_revision,
                gate_cycle,
                outcome,
                evidence,
                summary: summary.into(),
                recovery_authorization_id: None,
            },
        )
        .await?;
        Ok(TestSettleResult {
            idempotent_replay: result.idempotent_replay,
            graph_revision: result.graph_revision,
            outcome: result.outcome,
            plan_next_action: result.plan_next_action,
            critical_count: result.critical_count,
            important_count: result.important_count,
            minor_count: result.minor_count,
            stagnation_count: result.stagnation_count,
            rewrite_used: result.rewrite_used,
        })
    }

    fn design_evidence(
        critical_count: i64,
        important_count: i64,
        minor_count: i64,
    ) -> SettleGateEvidence {
        SettleGateEvidence::Design {
            critical_count,
            important_count,
            minor_count,
        }
    }

    fn phase(id: &str) -> ManifestPhase {
        ManifestPhase {
            id: id.into(),
            kind: Some(id.into()),
            title: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn wu(
        id: &str,
        phase: &str,
        role: ManifestNodeRole,
        agent: &str,
        profile: Option<&str>,
        task_index: Option<u32>,
        key: String,
        deps: Vec<String>,
    ) -> ManifestNode {
        ManifestNode {
            id: id.into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(phase.into()),
            role: Some(role),
            agent_type: Some(agent.into()),
            profile_id: profile.map(|s| s.into()),
            task_index,
            work_unit_key: Some(key),
            deps,
            required: Some(true),
            node_outcome: None,
            title: None,
        }
    }

    async fn seed_parent() -> (AppDatabase, i32) {
        let db = fresh_in_memory_db().await;
        let workspace = tempfile::tempdir().unwrap().keep();
        let design_path = workspace.join("docs/superpowers/specs/x.md");
        let plan_path = workspace.join("docs/superpowers/plans/p.md");
        std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(design_path, DESIGN_DOC_BYTES).unwrap();
        std::fs::write(plan_path, PLAN_DOC_BYTES).unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Codeg Test",
                "-c",
                "user.email=codeg@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(output.status.success(), "fixture git command failed");
        }
        let folder = seed_folder(&db, workspace.to_str().unwrap()).await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        (db, parent)
    }

    #[tokio::test]
    async fn workflow_manifest_v2_persisted_header_stamps_capability_version() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: skeleton_doc("workflow-manifest-v2-header"),
            },
        )
        .await
        .expect("publish v2 skeleton");
        let header = delegation_workflow::Entity::find_by_id(published.workflow_id)
            .one(&db.conn)
            .await
            .expect("load header")
            .expect("persisted header");
        assert_eq!(header.capability_version, "workflow_manifest_v2");
        assert_eq!(WORKFLOW_CAPABILITY_VERSION, "workflow_manifest_v2");
    }

    #[tokio::test]
    async fn failed_historical_publication_does_not_leak_fixture_permission() {
        use sea_orm::PaginatorTrait;

        let (db, parent) = seed_parent().await;
        let mut document = skeleton_doc("failed-historical-publication-scope");
        document.schema_version = MANIFEST_SCHEMA_VERSION + 1;
        let before = delegation_workflow::Entity::find()
            .count(&db.conn)
            .await
            .unwrap();

        publish_workflow_manifest_fixture(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .expect_err("invalid historical publication must fail");

        assert_eq!(
            delegation_workflow::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            before
        );
        let error = crate::acp::delegation::workflow::require_v2_mutation_for_connection(
            &db.conn,
            2,
            &delegation_workflow::CompletionProtocolMode::V2Enforce,
        )
        .await
        .expect_err("fixture permission must unwind after publication error");
        assert_eq!(error.code(), "workflow_v2_retired");
    }

    #[tokio::test]
    async fn manifest_publication_is_retired_without_creating_a_header() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();

        let error = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: skeleton_doc("workflow-v2-retired-publication"),
            },
        )
        .await
        .expect_err("production publication must be retired");
        assert_eq!(error.code(), "workflow_v2_retired");
        assert_eq!(error.source_conversation_id(), Some(parent));
        assert_eq!(error.successor_conversation_id(), None);
        assert_eq!(error.can_create_simple_successor(), Some(false));
        assert!(delegation_workflow::Entity::find()
            .filter(delegation_workflow::Column::ParentConversationId.eq(parent))
            .one(&db.conn)
            .await
            .expect("query headers")
            .is_none());
    }

    #[tokio::test]
    async fn workflow_v2_retired_store_mutations_stop_before_semantic_side_effects() {
        use crate::acp::delegation::workflow::WORKFLOW_V2_RETIRED_MESSAGE;
        use crate::db::entities::{
            delegation_attention_request, delegation_workflow_gate_settlement,
            delegation_workflow_outbox_event, recovery_authorization,
        };
        use sea_orm::PaginatorTrait;

        async fn side_effect_counts(db: &AppDatabase) -> [u64; 5] {
            [
                delegation_workflow_manifest_revision::Entity::find()
                    .count(&db.conn)
                    .await
                    .unwrap(),
                delegation_workflow_gate_settlement::Entity::find()
                    .count(&db.conn)
                    .await
                    .unwrap(),
                recovery_authorization::Entity::find()
                    .count(&db.conn)
                    .await
                    .unwrap(),
                delegation_attention_request::Entity::find()
                    .count(&db.conn)
                    .await
                    .unwrap(),
                delegation_workflow_outbox_event::Entity::find()
                    .count(&db.conn)
                    .await
                    .unwrap(),
            ]
        }

        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: skeleton_doc("workflow-v2-retired-store-matrix"),
            },
        )
        .await
        .unwrap();

        let before = side_effect_counts(&db).await;

        let settlement = super::settle_workflow_gate_v2_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowV2Request {
                workflow_id: published.workflow_id.clone(),
                gate_id: "missing-gate".into(),
                expected_graph_revision: published.graph_revision,
                expected_review_round: None,
                expected_outcome: None,
                summary: "must not be parsed".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        let recovery = super::recover_workflow_core(
            &db,
            &emitter,
            parent,
            RecoverWorkflowRequest {
                workflow_id: published.workflow_id.clone(),
                recovery_authorization_id: "must-not-be-consumed".into(),
                expected_manifest_revision: published.manifest_revision,
                correlation_id: "must-not-be-recorded".into(),
            },
        )
        .await
        .unwrap_err();
        let delivery = super::guard_final_delivery_core(
            &db,
            &emitter,
            FinalDeliveryGuardRequest {
                workflow_id: published.workflow_id.clone(),
                gate_id: "missing-final-gate".into(),
                workspace_path: PathBuf::from("/must/not/read"),
                final_reviewer_task_id: "must-not-load".into(),
            },
        )
        .await
        .unwrap_err();

        for error in [settlement, recovery, delivery] {
            assert_eq!(error.code(), "workflow_v2_retired");
            assert_eq!(error.to_string(), WORKFLOW_V2_RETIRED_MESSAGE);
        }
        assert_eq!(side_effect_counts(&db).await, before);
        let header = delegation_workflow::Entity::find_by_id(&published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            header.active_manifest_revision as u64,
            published.manifest_revision
        );
        assert_eq!(header.graph_revision as u64, published.graph_revision);
    }

    #[tokio::test]
    async fn completion_artifact_contract_final_delivery_drift_reopens_full_final_review() {
        use crate::acp::delegation::run_store::{ReservingRunInsert, RunStore};
        use crate::acp::delegation::workflow::completion_evidence::{
            materialize_terminal_completion_txn, TerminalCompletionInput,
        };
        use crate::db::entities::delegation_workflow::CompletionProtocolMode;
        use crate::db::entities::delegation_workflow_gate_state;
        use crate::db::entities::delegation_workflow_outbox_event;
        use std::path::Path;
        use std::process::Command;

        fn git(repo: &Path, args: &[&str]) -> String {
            let output = Command::new("git")
                .args(args)
                .current_dir(repo)
                .output()
                .expect("run final delivery git fixture command");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout)
                .expect("git output is UTF-8")
                .trim()
                .to_string()
        }

        async fn complete_v2_run(
            db: &AppDatabase,
            parent: i32,
            workflow_id: &str,
            workspace: &Path,
            node_id: &str,
            task_id: &str,
            final_text: &str,
        ) {
            let node = delegation_workflow_node_binding::Entity::find_by_id((
                workflow_id.to_string(),
                node_id.to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let child = seed_conversation(
                db,
                seed_folder(db, workspace.to_str().unwrap()).await,
                AgentType::Codex,
            )
            .await;
            let runs = RunStore::new(Arc::new(AppDatabase {
                conn: db.conn.clone(),
            }));
            super::super::with_historical_workflow_fixture_mutations(runs.admit_gen1_reserving(
                ReservingRunInsert {
                    dispatch_intent_id: None,
                    orchestration_binding: None,
                    task_id: task_id.into(),
                    root_task_id: task_id.into(),
                    previous_task_id: None,
                    generation: 1,
                    parent_conversation_id: parent,
                    parent_tool_use_id: Some(format!("tool-{task_id}")),
                    child_conversation_id: child,
                    agent_type: "codex".into(),
                    profile_id: None,
                    workspace_path: Some(workspace.to_string_lossy().into_owned()),
                    route_fingerprint: Some(format!("route-{task_id}")),
                    launch_snapshot_version: Some("v1".into()),
                    mode_id: None,
                    config_values_json: Some("{}".into()),
                    task_preview: Some(format!("Complete {node_id}")),
                    request_fingerprint: Some(format!("fingerprint-{task_id}")),
                    admission_class: AdmissionClass::NormalRevision,
                    lineage_root_task_id: task_id.into(),
                    work_unit_key: Some(node.work_unit_key),
                    history_only: false,
                    replaced_task_id: None,
                    replacement_reason: None,
                    started_at: Some(Utc::now()),
                },
            ))
            .await
            .unwrap();
            let run = delegation_task_run::Entity::find_by_id(task_id)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut run: delegation_task_run::ActiveModel = run.into();
            run.status = Set(DelegationRunStatus::Completed);
            run.reached_running_at = Set(Some(Utc::now()));
            run.finished_at = Set(Some(Utc::now()));
            run.update(&db.conn).await.unwrap();
            let completion = super::super::with_historical_workflow_fixture_mutations(
                materialize_terminal_completion_txn(
                    &db.conn,
                    TerminalCompletionInput {
                        task_id: task_id.into(),
                        terminal_status: DelegationRunStatus::Completed,
                        final_assistant_text: final_text.into(),
                        pre_read_reports: Vec::new(),
                        pre_read_artifact: None,
                    },
                ),
            )
            .await
            .unwrap();
            assert_eq!(completion.state, CompletionState::Resolved);
        }

        let repo = tempfile::tempdir().expect("temp final delivery repo");
        git(repo.path(), &["init", "--quiet"]);
        std::fs::write(repo.path().join("owned.txt"), b"reviewed\n")
            .expect("write reviewed commit");
        let design_bytes = b"# Design\n\nFinal delivery drift fixture.\n";
        let design_path = repo.path().join("docs/superpowers/specs/x.md");
        std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
        std::fs::write(&design_path, design_bytes).unwrap();
        let plan_bytes = b"## Global Constraints\n\n- Final delivery drift fixture.\n";
        let plan_path = repo.path().join("docs/superpowers/plans/p.md");
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&plan_path, plan_bytes).unwrap();
        git(
            repo.path(),
            &[
                "add",
                "owned.txt",
                "docs/superpowers/specs/x.md",
                "docs/superpowers/plans/p.md",
            ],
        );
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Codeg Test",
                "-c",
                "user.email=codeg@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "reviewed",
            ],
        );
        let reviewed_head = git(repo.path(), &["rev-parse", "HEAD"]);

        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let mut document = design_plan_doc("tok-task7-final-delivery");
        document.design.as_mut().unwrap().digest = format!("sha256:{}", sha256_hex(design_bytes));
        document.plan.as_mut().unwrap().digest = format!("sha256:{}", sha256_hex(plan_bytes));
        document.nodes.retain(|node| {
            matches!(
                node.id.as_str(),
                "plan-author" | "plan-reviewer-1" | "final-reviewer"
            )
        });
        document.edges.clear();
        document
            .gates
            .retain(|gate| gate.gate_kind == Some(DocumentGateKind::Plan));
        document.task_policies.clear();
        document
            .nodes
            .iter_mut()
            .find(|node| node.id == "plan-reviewer-1")
            .unwrap()
            .deps = vec!["plan-author".into()];
        document
            .nodes
            .iter_mut()
            .find(|node| node.id == "final-reviewer")
            .unwrap()
            .deps
            .clear();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .expect("publish final delivery fixture");
        while rx.try_recv().is_ok() {}
        let header = delegation_workflow::Entity::find_by_id(published.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut header_am: delegation_workflow::ActiveModel = header.into();
        header_am.completion_protocol_version = Set(2);
        header_am.completion_protocol_mode = Set(CompletionProtocolMode::V2Enforce);
        header_am.update(&db.conn).await.unwrap();
        set_initialized_gate_state(
            &db,
            &published.workflow_id,
            "plan",
            format!("sha256:{}", "a".repeat(64)),
            1,
            "[\"plan-reviewer-1\"]".into(),
        )
        .await;
        complete_v2_run(
            &db,
            parent,
            &published.workflow_id,
            repo.path(),
            "plan-author",
            "task7-final-plan-author",
            "Plan authored.\n\nConclusion: done",
        )
        .await;
        complete_v2_run(
            &db,
            parent,
            &published.workflow_id,
            repo.path(),
            "plan-reviewer-1",
            "task7-final-plan-reviewer",
            "Plan review complete.\n\nConclusion: approve",
        )
        .await;
        let current = delegation_workflow::Entity::find_by_id(&published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let settled = settle_workflow_gate_v2_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowV2Request {
                workflow_id: published.workflow_id.clone(),
                gate_id: "plan".into(),
                expected_graph_revision: current.graph_revision as u64,
                expected_review_round: Some(1),
                expected_outcome: Some(GateSettlementOutcome::Approved),
                summary: "Final delivery fixture Plan approval".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(settled.outcome, GateSettlementOutcome::Approved);
        let final_lineage = format!("sha256:{}", "f".repeat(64));
        set_initialized_gate_state(
            &db,
            &published.workflow_id,
            "final",
            final_lineage.clone(),
            3,
            "[\"final-reviewer\"]".into(),
        )
        .await;
        let final_task_id = "task7-passing-final-review";
        complete_v2_run(
            &db,
            parent,
            &published.workflow_id,
            repo.path(),
            "final-reviewer",
            final_task_id,
            "Final review complete.\n\nConclusion: approve",
        )
        .await;
        let final_binding = delegation_workflow_run_binding::Entity::find_by_id(final_task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            final_binding.artifact_digest.as_deref(),
            Some(reviewed_head.as_str())
        );
        while rx.try_recv().is_ok() {}

        let ready = guard_final_delivery_core(
            &db,
            &emitter,
            FinalDeliveryGuardRequest {
                workflow_id: published.workflow_id.clone(),
                gate_id: "final".into(),
                workspace_path: repo.path().to_path_buf(),
                final_reviewer_task_id: final_task_id.into(),
            },
        )
        .await
        .expect("clean reviewed commit is deliverable");
        assert!(matches!(ready, FinalDeliveryGuardResult::Ready(_)));
        assert!(
            rx.try_recv().is_err(),
            "ready delivery does not reopen Final"
        );

        std::fs::write(repo.path().join("owned.txt"), b"post-final drift\n")
            .expect("write post-Final drift");
        git(repo.path(), &["add", "owned.txt"]);
        git(
            repo.path(),
            &[
                "-c",
                "user.name=Codeg Test",
                "-c",
                "user.email=codeg@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "post-final drift",
            ],
        );

        let guarded = guard_final_delivery_core(
            &db,
            &emitter,
            FinalDeliveryGuardRequest {
                workflow_id: published.workflow_id.clone(),
                gate_id: "final".into(),
                workspace_path: repo.path().to_path_buf(),
                final_reviewer_task_id: final_task_id.into(),
            },
        )
        .await
        .expect("delivery guard commits reopen state");
        assert_eq!(guarded.diagnostic_code(), Some("final_artifact_drift"));
        let reopened = guarded.reopened().expect("Final review reopened");
        assert_eq!(reopened.required_reviewer_node_ids, vec!["final-reviewer"]);
        assert_eq!(reopened.review_round, 4);
        assert_ne!(
            reopened.gate_lineage, final_lineage,
            "drift must mint a new Final lineage"
        );

        let state = delegation_workflow_gate_state::Entity::find_by_id((
            published.workflow_id.clone(),
            "final".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        assert_eq!(state.gate_lineage, reopened.gate_lineage);
        assert_eq!(state.current_review_round, 4);
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&state.selected_node_ids_json).unwrap(),
            vec!["final-reviewer"]
        );
        let outbox = delegation_workflow_outbox_event::Entity::find()
            .filter(
                delegation_workflow_outbox_event::Column::WorkflowId
                    .eq(published.workflow_id.clone()),
            )
            .filter(delegation_workflow_outbox_event::Column::EventKind.eq("final_artifact_drift"))
            .one(&db.conn)
            .await
            .unwrap()
            .expect("durable Final drift diagnostic");
        let payload: serde_json::Value = serde_json::from_str(&outbox.payload_json).unwrap();
        assert_eq!(payload["diagnostic"], "final_artifact_drift");
        assert_eq!(payload["gate_lineage"], reopened.gate_lineage);
        assert_eq!(outbox.graph_revision as u64, reopened.graph_revision);
        let event = rx.try_recv().expect("reopen graph event after commit");
        assert_eq!(event.channel, CHANGED);
        assert_eq!(
            event.payload["graph_revision"].as_u64(),
            Some(reopened.graph_revision)
        );
    }

    #[tokio::test]
    async fn workflow_manifest_v2_replay_and_update_upgrade_existing_header() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut document = design_plan_doc("workflow-manifest-v2-upgrade");
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: document.clone(),
            },
        )
        .await
        .expect("publish initial manifest");

        let downgrade_header = async || {
            let header = delegation_workflow::Entity::find_by_id(published.workflow_id.clone())
                .one(&db.conn)
                .await
                .expect("load header")
                .expect("persisted header");
            let mut active: delegation_workflow::ActiveModel = header.into();
            active.capability_version = Set("workflow_manifest_v1".into());
            active.update(&db.conn).await.expect("seed v1 header");
        };

        downgrade_header().await;
        let replay = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: document.clone(),
            },
        )
        .await
        .expect("replay through v2 endpoint");
        assert!(replay.idempotent_replay);
        let recovery = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
            .await
            .expect("recover replayed workflow");
        assert_eq!(recovery.capability_version, "workflow_manifest_v2");

        downgrade_header().await;
        document.workflow_id = Some(published.workflow_id.clone());
        document.expected_manifest_revision = Some(1);
        document.plan.as_mut().expect("Plan document").digest = "sha256:plan-v2".into();
        let update = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .expect("update through v2 endpoint");
        assert!(!update.idempotent_replay);
        let recovery = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
            .await
            .expect("recover updated workflow");
        assert_eq!(recovery.capability_version, "workflow_manifest_v2");
    }

    #[tokio::test]
    async fn workflow_v2_caller_artifact_digest_is_not_authority() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("workflow-manifest-v2-digest-error"),
            },
        )
        .await
        .expect("publish Plan workflow");

        let error = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-task-1",
                "sha256:contradictory-plan",
            )),
            "contradictory Plan digest",
        )
        .await
        .expect_err("caller evidence cannot fabricate durable Plan evidence");
        assert!(matches!(&error, WorkflowStoreError::GateNotReady(_)));
        let code = crate::acp::delegation::listener::workflow_store_error_code_for_test(error);
        assert_eq!(code, "gate_not_ready");
    }

    #[tokio::test]
    async fn workflow_manifest_v2_author_card_digest_mismatch_has_typed_marker() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("workflow-manifest-v2-author-card-digest"),
            },
        )
        .await
        .expect("publish Plan workflow");
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-card-digest-task",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-card-digest.md",
            0,
        )
        .await;
        let run = delegation_task_run::Entity::find_by_id("author-card-digest-task")
            .one(&db.conn)
            .await
            .expect("load Author run")
            .expect("persisted Author run");
        let mut active: delegation_task_run::ActiveModel = run.into();
        active.card_summary_json = Set(Some(
            serde_json::json!({
                "kind": "author",
                "status": "done",
                "summary": "Plan artifact completed",
                "plan_digest": "sha256:contradictory-author-card",
                "report_file": "reports/author-card-digest.md"
            })
            .to_string(),
        ));
        active.update(&db.conn).await.expect("mutate Author card");
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "author-card-digest-review",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-card-digest-task",
            "request_changes",
            "reports/author-card-digest-review.md",
            1,
        )
        .await;

        let settled = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-card-digest-task",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "contradictory Author card digest",
        )
        .await
        .expect("legacy Card digest is not fixed-v2 settlement authority");
        assert_eq!(settled.outcome, GateSettlementOutcome::ChangesRequested);
    }

    const DESIGN_DOC_BYTES: &[u8] = b"# Design\n";
    const PLAN_DOC_BYTES: &[u8] = b"## Global Constraints\n\n- exact\n\n## Task 1: Build\n\nbody\n";
    /// Design document digest used by `design_plan_doc`.
    const DESIGN_DOC_DIGEST: &str =
        "sha256:2c1f01e37a150fd02e10dd63ce8a268c168a68813b40f16f18c3430319073ce6";

    #[allow(clippy::too_many_arguments)]
    async fn insert_terminal_reviewer_run(
        db: &AppDatabase,
        parent: i32,
        workflow_id: &str,
        node_id: &str,
        gate_id: &str,
        gate_cycle: i64,
        task_id: &str,
        _summary_validated: bool,
        created_offset_secs: i64,
        _artifact_digest: &str,
        status: DelegationRunStatus,
        _manifest_revision: i64,
    ) {
        let now = Utc::now() + chrono::Duration::seconds(created_offset_secs);
        let workspace = tempfile::tempdir()
            .expect("create completion fixture workspace")
            .keep();
        let design_path = workspace.join("docs/superpowers/specs/x.md");
        let plan_path = workspace.join("docs/superpowers/plans/p.md");
        std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(design_path, DESIGN_DOC_BYTES).unwrap();
        std::fs::write(plan_path, PLAN_DOC_BYTES).unwrap();
        let node = delegation_workflow_node_binding::Entity::find_by_id((
            workflow_id.to_string(),
            node_id.to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        if node.role == "reviewer" {
            let state = delegation_workflow_gate_state::Entity::find_by_id((
                workflow_id.to_string(),
                gate_id.to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .expect("fixed-v2 publication initializes gate state");
            let mut selected: Vec<String> =
                serde_json::from_str(&state.selected_node_ids_json).unwrap();
            if (gate_cycle == 1 || !selected.is_empty())
                && !selected.iter().any(|selected| selected == node_id)
            {
                selected.push(node_id.to_string());
                selected.sort();
                selected.dedup();
            }
            let mut state: delegation_workflow_gate_state::ActiveModel = state.into();
            state.selected_node_ids_json = Set(serde_json::to_string(&selected).unwrap());
            state.update(&db.conn).await.unwrap();
        }
        let child_agent = match node.agent_type.as_str() {
            "code_buddy" => AgentType::CodeBuddy,
            "grok" => AgentType::Grok,
            _ => AgentType::Codex,
        };
        let child = seed_conversation(
            db,
            seed_folder(db, workspace.to_str().unwrap()).await,
            child_agent,
        )
        .await;

        let run = delegation_task_run::ActiveModel {
            task_id: Set(task_id.to_string()),
            root_task_id: Set(task_id.to_string()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(parent),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set(node.agent_type.clone()),
            profile_id: Set(node.profile_id.clone()),
            workspace_path: Set(Some(workspace.to_string_lossy().into_owned())),
            route_fingerprint: Set(None),
            launch_snapshot_version: Set(None),
            mode_id: Set(None),
            config_values_json: Set(None),
            task_preview: Set(None),
            request_fingerprint: Set(None),
            admission_class: Set(AdmissionClass::NormalRevision),
            reached_running_at: Set(None),
            lineage_root_task_id: Set(task_id.to_string()),
            work_unit_key: Set(Some(node.work_unit_key.clone())),
            legacy_parent_tool_use_id: Set(None),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Reserving),
            error_code: Set(None),
            termination_audit_json: Set(None),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            tool_call_count: Set(None),
            edit_tool_call_count: Set(None),
            touched_files_json: Set(None),
            touched_files_truncated: Set(None),
            additions: Set(None),
            deletions: Set(None),
            line_counts_complete: Set(None),
            card_summary_json: Set(Some("{}".into())),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(None),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        run.insert(&db.conn).await.expect("insert run");

        super::super::with_historical_workflow_fixture_mutations(
            super::super::admission::admit_workflow_run_txn(
                &db.conn,
                &super::super::admission::WorkflowAdmitInput {
                    parent_conversation_id: parent,
                    child_conversation_id: child,
                    task_id,
                    work_unit_key: Some(&node.work_unit_key),
                    agent_type: &node.agent_type,
                    profile_id: node.profile_id.as_deref(),
                    lineage_root_task_id: task_id,
                    generation: 1,
                    kind: super::super::admission::AdmissionDispatchKind::FirstDispatch,
                    admission_class: AdmissionClass::NormalRevision,
                    workspace_path: workspace.to_str(),
                },
            ),
        )
        .await
        .expect("admit completion fixture run");

        let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut run: delegation_task_run::ActiveModel = run.into();
        run.status = Set(status);
        run.reached_running_at = Set(Some(now));
        run.finished_at = Set(Some(now));
        run.card_summary_json = Set(Some("{}".into()));
        run.updated_at = Set(now);
        run.update(&db.conn).await.unwrap();

        let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut binding: delegation_workflow_run_binding::ActiveModel = binding.into();
        binding.created_at = Set(now);
        binding.updated_at = Set(now);
        binding.update(&db.conn).await.expect("update run binding");
    }

    async fn materialize_fixture_completion(db: &AppDatabase, task_id: &str, conclusion: &str) {
        let conclusion = match conclusion {
            "request_changes" => "request changes",
            "approve_with_minors" => "approve with minors",
            other => other,
        };
        let result = super::super::with_historical_workflow_fixture_mutations(
            super::super::completion_evidence::materialize_terminal_completion_txn(
                &db.conn,
                super::super::completion_evidence::TerminalCompletionInput {
                    task_id: task_id.to_string(),
                    terminal_status: DelegationRunStatus::Completed,
                    final_assistant_text: format!("Conclusion: {conclusion}"),
                    pre_read_reports: Vec::new(),
                    pre_read_artifact: None,
                },
            ),
        )
        .await
        .unwrap_or_else(|error| panic!("materialize {task_id}: {error:?}"));
        assert_eq!(result.state, CompletionState::Resolved);
    }

    /// Convenience: completed design-gate reviewer on active revision 1.
    async fn insert_design_reviewer_evidence(
        db: &AppDatabase,
        parent: i32,
        workflow_id: &str,
        task_id: &str,
        gate_cycle: i64,
        offset_secs: i64,
        outcome: &str,
    ) {
        insert_terminal_reviewer_run(
            db,
            parent,
            workflow_id,
            "design-reviewer-1",
            "design",
            gate_cycle,
            task_id,
            true,
            offset_secs,
            DESIGN_DOC_DIGEST,
            DelegationRunStatus::Completed,
            1,
        )
        .await;
        materialize_fixture_completion(db, task_id, outcome).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_plan_author_evidence(
        db: &AppDatabase,
        parent: i32,
        workflow_id: &str,
        task_id: &str,
        manifest_revision: i64,
        plan_digest: &str,
        report_file: &str,
        created_offset_secs: i64,
    ) -> i32 {
        insert_terminal_reviewer_run(
            db,
            parent,
            workflow_id,
            "plan-author",
            "plan",
            1,
            task_id,
            true,
            created_offset_secs,
            plan_digest,
            DelegationRunStatus::Completed,
            manifest_revision,
        )
        .await;
        let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let child_conversation_id = run.child_conversation_id;
        let mut am: delegation_task_run::ActiveModel = run.into();
        am.card_summary_json = Set(Some(
            serde_json::json!({
                "kind": "author",
                "status": "done",
                "summary": "Plan artifact completed",
                "plan_digest": plan_digest,
                "report_file": report_file,
            })
            .to_string(),
        ));
        am.update(&db.conn).await.unwrap();
        let node = delegation_workflow_node_binding::Entity::find_by_id((
            workflow_id.to_string(),
            "plan-author".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut node_am: delegation_workflow_node_binding::ActiveModel = node.into();
        node_am.is_observed = Set(true);
        node_am.update(&db.conn).await.unwrap();
        materialize_fixture_completion(db, task_id, "done").await;
        child_conversation_id
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_plan_reviewer_evidence(
        db: &AppDatabase,
        parent: i32,
        workflow_id: &str,
        node_id: &str,
        task_id: &str,
        gate_cycle: i64,
        manifest_revision: i64,
        plan_digest: &str,
        author_task_id: &str,
        verdict: &str,
        report_file: &str,
        created_offset_secs: i64,
    ) -> i32 {
        let state = delegation_workflow_gate_state::Entity::find_by_id((
            workflow_id.to_string(),
            "plan".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .expect("fixed-v2 publication initializes Plan gate state");
        let needs_corrective_author = gate_cycle > 1
            && serde_json::from_str::<Vec<String>>(&state.selected_node_ids_json)
                .unwrap()
                .is_empty();
        let effective_author_task_id = if needs_corrective_author {
            format!("{author_task_id}-round-{gate_cycle}")
        } else {
            author_task_id.to_string()
        };
        if delegation_task_run::Entity::find_by_id(effective_author_task_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .is_none()
        {
            insert_plan_author_evidence(
                db,
                parent,
                workflow_id,
                &effective_author_task_id,
                manifest_revision,
                plan_digest,
                &format!("reports/{effective_author_task_id}.md"),
                created_offset_secs.saturating_sub(1),
            )
            .await;
        }
        insert_terminal_reviewer_run(
            db,
            parent,
            workflow_id,
            node_id,
            "plan",
            gate_cycle,
            task_id,
            true,
            created_offset_secs,
            plan_digest,
            DelegationRunStatus::Completed,
            manifest_revision,
        )
        .await;
        let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let child_conversation_id = run.child_conversation_id;
        let mut run_am: delegation_task_run::ActiveModel = run.into();
        run_am.card_summary_json = Set(Some(
            serde_json::json!({
                "kind": "review",
                "verdict": verdict,
                "critical": 0,
                "important": 0,
                "minor": 0,
                "summary": "Plan review completed",
                "report_file": report_file,
            })
            .to_string(),
        ));
        run_am.update(&db.conn).await.unwrap();

        materialize_fixture_completion(db, task_id, verdict).await;
        child_conversation_id
    }

    fn high_risk(task_index: u32) -> ManifestTaskRisk {
        ManifestTaskRisk {
            level: TaskRiskLevel::High,
            hard_triggers: vec![ManifestTaskHardTrigger {
                kind: TaskHardTriggerKind::ConcurrencyLifecycle,
                evidence: vec![format!("Task {task_index} has durable serial gate state")],
            }],
            soft_signals: vec![],
            score: 0,
            reason: format!("Task {task_index} requires dual independent review"),
        }
    }

    fn rename_node_id(doc: &mut ManifestDocument, old: &str, new: &str) {
        doc.nodes.iter_mut().find(|node| node.id == old).unwrap().id = new.into();
        for node in &mut doc.nodes {
            for dep in &mut node.deps {
                if dep == old {
                    *dep = new.into();
                }
            }
        }
        for edge in &mut doc.edges {
            if edge.from == old {
                edge.from = new.into();
            }
            if edge.to == old {
                edge.to = new.into();
            }
        }
    }

    fn current_plan_cohort_doc(token: &str) -> ManifestDocument {
        let mut doc = two_reviewer_plan_doc(token);
        rename_node_id(&mut doc, "plan-reviewer-1", "plan-reviewer-codex");
        rename_node_id(&mut doc, "plan-reviewer-2", "plan-reviewer-grok");
        let gate = doc
            .gates
            .iter_mut()
            .find(|gate| gate.gate_kind == Some(DocumentGateKind::Plan))
            .unwrap();
        gate.reviewer_cohort_node_ids =
            vec!["plan-reviewer-codex".into(), "plan-reviewer-grok".into()];
        gate.required_reviewer_node_ids = gate.reviewer_cohort_node_ids.clone();
        doc
    }

    fn two_task_index_doc(token: &str) -> ManifestDocument {
        let mut doc = design_plan_doc(token);
        rename_node_id(&mut doc, "task-1-rev", "task-1-review-codex");
        let task_1_impl = doc
            .nodes
            .iter_mut()
            .find(|node| node.id == "task-1-impl")
            .unwrap();
        task_1_impl.agent_type = Some("codex".into());
        task_1_impl.work_unit_key = Some(
            build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                task_index: 1,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap(),
        );
        let task_1_grok_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        doc.nodes.push(wu(
            "task-1-review-grok",
            PHASE_TASKS,
            ManifestNodeRole::Reviewer,
            "grok",
            None,
            Some(1),
            task_1_grok_key,
            vec!["task-1-impl".into()],
        ));
        doc.task_policies[0] = ManifestTaskPolicy {
            task_index: 1,
            risk: high_risk(1),
            route: ManifestTaskRoute {
                implementer_node_id: "task-1-impl".into(),
                reviewer_node_ids: vec!["task-1-review-codex".into(), "task-1-review-grok".into()],
            },
            allow_noop_verification: false,
        };

        let task_2_impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 2,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let task_2_codex_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 2,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let task_2_grok_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 2,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        doc.nodes.extend([
            wu(
                "task-2-impl",
                PHASE_TASKS,
                ManifestNodeRole::Implementer,
                "codex",
                None,
                Some(2),
                task_2_impl_key,
                vec!["task-1-review-codex".into(), "task-1-review-grok".into()],
            ),
            wu(
                "task-2-review-codex",
                PHASE_TASKS,
                ManifestNodeRole::Reviewer,
                "codex",
                None,
                Some(2),
                task_2_codex_key,
                vec!["task-2-impl".into()],
            ),
            wu(
                "task-2-review-grok",
                PHASE_TASKS,
                ManifestNodeRole::Reviewer,
                "grok",
                None,
                Some(2),
                task_2_grok_key,
                vec!["task-2-impl".into()],
            ),
        ]);
        doc.task_policies.push(ManifestTaskPolicy {
            task_index: 2,
            risk: high_risk(2),
            route: ManifestTaskRoute {
                implementer_node_id: "task-2-impl".into(),
                reviewer_node_ids: vec!["task-2-review-codex".into(), "task-2-review-grok".into()],
            },
            allow_noop_verification: false,
        });
        doc
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_task_route_evidence(
        db: &AppDatabase,
        parent: i32,
        workflow_id: &str,
        node_id: &str,
        task_id: &str,
        agent_type: &str,
        artifact_digest: &str,
        reviewed_task_id: Option<&str>,
        lineage_ordinal: i64,
    ) {
        let now = Utc::now() + chrono::Duration::seconds(lineage_ordinal);
        let child = seed_conversation(
            db,
            seed_folder(db, &format!("/tmp/{task_id}")).await,
            AgentType::Codex,
        )
        .await;
        let card_summary_json = if reviewed_task_id.is_some() {
            serde_json::json!({
                "kind": "review",
                "verdict": "approve",
                "critical": 0,
                "important": 0,
                "minor": 0,
                "summary": "Task review completed",
                "report_file": format!("reports/{task_id}.md"),
            })
        } else {
            serde_json::json!({
                "kind": "implementation",
                "phase": "implementation",
                "status": "done",
                "summary": "Task implementation completed",
                "commits": [],
                "tests": null,
                "concerns": [],
                "report_file": format!("reports/{task_id}.md"),
            })
        };
        delegation_task_run::ActiveModel {
            task_id: Set(task_id.into()),
            root_task_id: Set(task_id.into()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(parent),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set(agent_type.into()),
            profile_id: Set(None),
            workspace_path: Set(None),
            route_fingerprint: Set(None),
            launch_snapshot_version: Set(None),
            mode_id: Set(None),
            config_values_json: Set(None),
            task_preview: Set(None),
            request_fingerprint: Set(None),
            admission_class: Set(AdmissionClass::NormalRevision),
            reached_running_at: Set(Some(now)),
            lineage_root_task_id: Set(task_id.into()),
            work_unit_key: Set(None),
            legacy_parent_tool_use_id: Set(None),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            error_code: Set(None),
            termination_audit_json: Set(None),
            started_at: Set(Some(now)),
            finished_at: Set(Some(now)),
            tool_call_count: Set(None),
            edit_tool_call_count: Set(None),
            touched_files_json: Set(None),
            touched_files_truncated: Set(None),
            additions: Set(None),
            deletions: Set(None),
            line_counts_complete: Set(None),
            card_summary_json: Set(Some(card_summary_json.to_string())),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(None),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
        delegation_workflow_run_binding::ActiveModel {
            task_id: Set(task_id.into()),
            workflow_id: Set(workflow_id.into()),
            node_id: Set(node_id.into()),
            gate_id: Set(None),
            gate_cycle: Set(None),
            manifest_revision: Set(1),
            content_fingerprint: Set(None),
            artifact_digest: Set(Some(artifact_digest.into())),
            reviewed_task_id: Set(reviewed_task_id.map(str::to_string)),
            reviewed_implementer_generation: Set(reviewed_task_id.map(|_| 1)),
            lineage_ordinal: Set(lineage_ordinal),
            summary_validated: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .unwrap();
    }

    struct IndexWorkflowFixture {
        db: AppDatabase,
        parent: i32,
        workflow_id: String,
        publication_token: String,
    }

    impl IndexWorkflowFixture {
        async fn complete_task_1_implementer_only(&self) {
            insert_task_route_evidence(
                &self.db,
                self.parent,
                &self.workflow_id,
                "task-1-impl",
                "task-1-implementation",
                "codex",
                "sha256:task-1",
                None,
                1,
            )
            .await;
        }

        async fn complete_both_task_1_reviews_against_latest_artifact(&self) {
            for (node_id, task_id, agent_type, ordinal) in [
                ("task-1-review-codex", "task-1-codex-review", "codex", 2),
                ("task-1-review-grok", "task-1-grok-review", "grok", 3),
            ] {
                insert_task_route_evidence(
                    &self.db,
                    self.parent,
                    &self.workflow_id,
                    node_id,
                    task_id,
                    agent_type,
                    "sha256:task-1",
                    Some("task-1-implementation"),
                    ordinal,
                )
                .await;
            }
        }

        async fn materially_republish_plan_with_reviewers(&self, reviewer_ids: [&str; 2]) {
            let (emitter, _) = emitter_with_rx();
            let mut doc = current_plan_cohort_doc(&self.publication_token);
            assert_eq!(reviewer_ids, ["plan-reviewer-codex", "plan-reviewer-grok"]);
            let historical_profile = "11111111-1111-4111-8111-111111111111";
            let historical_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
                rel_plan_path: &doc.plan_target_rel_path,
                agent_type: "code_buddy",
                profile_id: Some(historical_profile),
            })
            .unwrap();
            doc.nodes.push(wu(
                "plan-reviewer-old",
                PHASE_PLAN,
                ManifestNodeRole::Reviewer,
                "code_buddy",
                Some(historical_profile),
                None,
                historical_key,
                vec!["plan-author".into()],
            ));
            let plan_gate = doc
                .gates
                .iter_mut()
                .find(|gate| gate.gate_kind == Some(DocumentGateKind::Plan))
                .unwrap();
            plan_gate
                .reviewer_cohort_node_ids
                .insert(0, "plan-reviewer-old".into());
            doc.workflow_id = Some(self.workflow_id.clone());
            doc.expected_manifest_revision = Some(1);
            doc.plan.as_mut().unwrap().digest = "sha256:plan-v2".into();
            publish_workflow_manifest_fixture(
                &self.db,
                &emitter,
                self.parent,
                PublishWorkflowRequest { document: doc },
            )
            .await
            .unwrap();
        }
    }

    async fn seed_two_task_index_workflow() -> IndexWorkflowFixture {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let publication_token = "tok-two-task-index".to_string();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: two_task_index_doc(&publication_token),
            },
        )
        .await
        .unwrap();
        IndexWorkflowFixture {
            db,
            parent,
            workflow_id: published.workflow_id,
            publication_token,
        }
    }

    async fn seed_open_plan_findings_with_reviewer_runs() -> IndexWorkflowFixture {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let publication_token = "tok-open-plan-sources".to_string();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: current_plan_cohort_doc(&publication_token),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "current-plan-author",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/current-plan-author.md",
            0,
        )
        .await;
        let state = delegation_workflow_gate_state::Entity::find_by_id((
            published.workflow_id.clone(),
            "plan".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        set_initialized_gate_state(
            &db,
            &published.workflow_id,
            "plan",
            state.gate_lineage,
            1,
            r#"["plan-reviewer-codex","plan-reviewer-grok"]"#.into(),
        )
        .await;
        for (node_id, task_id, report, ordinal) in [
            (
                "plan-reviewer-codex",
                "current-plan-codex",
                "reports/current-plan-codex.md",
                1,
            ),
            (
                "plan-reviewer-grok",
                "current-plan-grok",
                "reports/current-plan-grok.md",
                2,
            ),
        ] {
            insert_plan_reviewer_evidence(
                &db,
                parent,
                &published.workflow_id,
                node_id,
                task_id,
                1,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                "current-plan-author",
                "request_changes",
                report,
                ordinal,
            )
            .await;
        }
        settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-codex", "plan-reviewer-grok"],
                vec![
                    finding(
                        "F-codex",
                        FindingSeverity::Important,
                        FindingStatus::Open,
                        &["plan-reviewer-codex"],
                    ),
                    finding(
                        "F-grok",
                        FindingSeverity::Important,
                        FindingStatus::Open,
                        &["plan-reviewer-grok"],
                    ),
                ],
                "current-plan-author",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "current Plan findings",
        )
        .await
        .unwrap();
        IndexWorkflowFixture {
            db,
            parent,
            workflow_id: published.workflow_id,
            publication_token,
        }
    }

    async fn seed_historical_plan_round_with_required_reviewers(
        reviewer_ids: [&str; 1],
    ) -> IndexWorkflowFixture {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let publication_token = "tok-historical-plan-cohort".to_string();
        let mut doc = design_plan_doc(&publication_token);
        rename_node_id(&mut doc, "plan-reviewer-1", reviewer_ids[0]);
        let historical_profile = "11111111-1111-4111-8111-111111111111";
        let historical = doc
            .nodes
            .iter_mut()
            .find(|node| node.id == reviewer_ids[0])
            .unwrap();
        historical.agent_type = Some("code_buddy".into());
        historical.profile_id = Some(historical_profile.into());
        historical.work_unit_key = Some(
            build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
                rel_plan_path: &doc.plan_target_rel_path,
                agent_type: "code_buddy",
                profile_id: Some(historical_profile),
            })
            .unwrap(),
        );
        let gate = doc
            .gates
            .iter_mut()
            .find(|gate| gate.gate_kind == Some(DocumentGateKind::Plan))
            .unwrap();
        gate.reviewer_cohort_node_ids = vec![reviewer_ids[0].into()];
        gate.required_reviewer_node_ids = gate.reviewer_cohort_node_ids.clone();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "historical-plan-author",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/historical-plan-author.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            reviewer_ids[0],
            "historical-plan-review",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "historical-plan-author",
            "request_changes",
            "reports/historical-plan-review.md",
            1,
        )
        .await;
        settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &[reviewer_ids[0]],
                vec![finding(
                    "F-historical",
                    FindingSeverity::Important,
                    FindingStatus::Open,
                    &[reviewer_ids[0]],
                )],
                "historical-plan-author",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "historical Plan findings",
        )
        .await
        .unwrap();
        IndexWorkflowFixture {
            db,
            parent,
            workflow_id: published.workflow_id,
            publication_token,
        }
    }

    #[tokio::test]
    async fn index_routes_ignore_unvalidated_legacy_run_projections() {
        let fixture = seed_two_task_index_workflow().await;
        fixture.complete_task_1_implementer_only().await;
        let active =
            get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id))
                .await
                .unwrap();
        assert_eq!(
            active
                .actionable_task_routes
                .iter()
                .map(|route| route.task_index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            active.actionable_task_routes[0].reviewer_node_ids,
            vec!["task-1-review-codex", "task-1-review-grok"]
        );

        fixture
            .complete_both_task_1_reviews_against_latest_artifact()
            .await;
        let next = get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id))
            .await
            .unwrap();
        assert_eq!(
            next.actionable_task_routes
                .iter()
                .map(|route| route.task_index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[tokio::test]
    async fn v2_plan_settlement_covers_each_required_reviewer() {
        let fixture = seed_open_plan_findings_with_reviewer_runs().await;
        let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
            fixture.workflow_id,
            "plan".to_string(),
            1,
        ))
        .one(&fixture.db.conn)
        .await
        .unwrap()
        .unwrap();
        let review = load_persisted_plan_state_v2(&settlement).unwrap();
        assert_eq!(
            review
                .reviewers
                .iter()
                .map(|source| source.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["plan-reviewer-codex", "plan-reviewer-grok"]
        );
        assert_eq!(
            serde_json::from_str::<Vec<String>>(
                settlement
                    .required_evidence_task_ids_json
                    .as_deref()
                    .unwrap()
            )
            .unwrap(),
            vec!["current-plan-codex", "current-plan-grok"]
        );
    }

    #[tokio::test]
    async fn material_republish_rejects_unproven_unselected_historical_reviewer() {
        let fixture =
            seed_historical_plan_round_with_required_reviewers(["plan-reviewer-old"]).await;
        fixture
            .materially_republish_plan_with_reviewers(["plan-reviewer-codex", "plan-reviewer-grok"])
            .await;

        let state =
            get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id))
                .await
                .expect("invalid historical completion is omitted from the bounded index");
        let plan_gate = state
            .gates
            .iter()
            .find(|gate| gate.gate_kind == "plan")
            .unwrap();
        assert_eq!(plan_gate.latest_outcome, None);
        assert!(state.latest_plan_review.is_none());
    }

    #[tokio::test]
    async fn publish_create_and_idempotent_token_replay() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let doc = design_plan_doc("tok-1");
        let r1 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .expect("create");
        assert!(!r1.idempotent_replay);
        assert_eq!(r1.manifest_revision, 1);
        assert_eq!(r1.graph_revision, 1);

        let evt = rx.try_recv().expect("changed event");
        assert_eq!(evt.channel, CHANGED);
        assert_eq!(evt.payload["graph_revision"].as_u64(), Some(1));

        // Same token + same digest → idempotent, no second event.
        let r2 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("replay");
        assert!(r2.idempotent_replay);
        assert_eq!(r2.workflow_id, r1.workflow_id);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn publication_token_digest_mismatch_no_mutation() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-mm");
        publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();

        // Change plan digest → different normalized document.
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-OTHER".into();
        }
        let err = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            WorkflowStoreError::PublicationTokenMismatch { .. }
        ));
        // Still one revision.
        let state = get_workflow_state_core(&db, parent, None).await.unwrap();
        assert_eq!(state.manifest_revision, 1);
    }

    #[tokio::test]
    async fn publish_cas_stale_manifest_revision() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-cas");
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();

        doc.workflow_id = Some(r.workflow_id.clone());
        doc.expected_manifest_revision = Some(0); // stale
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-v2".into();
        }
        let err = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap_err();
        match err {
            WorkflowStoreError::StaleManifestRevision { expected, current } => {
                assert_eq!(expected, 0);
                assert_eq!(current, 1);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn publish_update_bumps_revision_and_emits_once() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-upd");
        let r1 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        let _ = rx.try_recv();

        doc.workflow_id = Some(r1.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-v2".into();
        }
        let r2 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        assert!(!r2.idempotent_replay);
        assert_eq!(r2.manifest_revision, 2);
        assert_eq!(r2.graph_revision, 2);
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt.payload["graph_revision"].as_u64(), Some(2));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn cross_parent_reject_on_get_and_settle() {
        let (db, parent_a) = seed_parent().await;
        let folder_b = seed_folder(&db, "/tmp/wf-b").await;
        let parent_b = seed_conversation(&db, folder_b, AgentType::Codex).await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent_a,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-xp"),
            },
        )
        .await
        .unwrap();

        let err = get_workflow_state_core(&db, parent_b, Some(&r.workflow_id))
            .await
            .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::CrossParent { .. }));

        let err = settle_workflow_gate_core(
            &db,
            &emitter,
            parent_b,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(0, 0, 0),
                summary: "ok".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::CrossParent { .. }));
    }

    #[tokio::test]
    async fn settle_before_all_reviewers_rejected() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-settle-early"),
            },
        )
        .await
        .unwrap();

        let err = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(0, 0, 0),
                summary: "premature".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::GateNotReady(_)));
    }

    #[tokio::test]
    async fn settle_happy_path_idempotent_and_conflict() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-settle"),
            },
        )
        .await
        .unwrap();
        let _ = rx.try_recv();

        insert_design_reviewer_evidence(
            &db,
            parent,
            &r.workflow_id,
            "task-design-1",
            1,
            0,
            "request_changes",
        )
        .await;

        let req = SettleWorkflowRequest {
            workflow_id: r.workflow_id.clone(),
            manifest_revision: 1,
            gate_id: "design".into(),
            expected_graph_revision: 1,
            gate_cycle: 1,
            outcome: GateSettlementOutcome::ChangesRequested,
            evidence: design_evidence(1, 0, 0),
            summary: "needs work".into(),
            recovery_authorization_id: None,
        };
        let graph_before_settlement = delegation_workflow::Entity::find_by_id(r.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .graph_revision as u64;
        let s1 = settle_workflow_gate_core(&db, &emitter, parent, req.clone())
            .await
            .unwrap();
        assert!(!s1.idempotent_replay);
        assert_eq!(s1.graph_revision, graph_before_settlement + 1);
        let _ = rx.try_recv().unwrap();

        // Same payload → idempotent, no event.
        let s2 = settle_workflow_gate_core(&db, &emitter, parent, req.clone())
            .await
            .unwrap();
        assert!(s2.idempotent_replay);
        assert!(rx.try_recv().is_err());

        // Conflicting payload for same cycle.
        let mut bad = req;
        bad.summary = "different".into();
        bad.expected_graph_revision = 2;
        let err = settle_workflow_gate_core(&db, &emitter, parent, bad)
            .await
            .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::GateCycleConflict(_)));
    }

    #[tokio::test]
    async fn cycle_n_plus_1_rejects_cycle_n_runs() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-cycle"),
            },
        )
        .await
        .unwrap();

        insert_design_reviewer_evidence(
            &db,
            parent,
            &r.workflow_id,
            "task-c1",
            1,
            0,
            "request_changes",
        )
        .await;

        let s1 = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id.clone(),
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::ChangesRequested,
                evidence: design_evidence(0, 1, 0),
                summary: "again".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap();

        // Reuse cycle-1 run for cycle 2 (same gate_cycle on binding = 1 only).
        // Even if we incorrectly tag gate_cycle=2 on the same old run created_at
        // before settlement, freshness fails. Insert binding for cycle 2 with old ts.
        let old = Utc::now() - chrono::Duration::hours(1);
        // The cycle-1 run is too old relative to settlement; add a cycle-2 binding
        // pointing at same task but with created_at before settlement.
        // Stale: created before prior settlement (and would also fail digest if wrong).
        let header_fp = delegation_workflow::Entity::find_by_id(r.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .design_fingerprint;
        let rb = delegation_workflow_run_binding::ActiveModel {
            task_id: Set("task-c1-stale".into()),
            workflow_id: Set(r.workflow_id.clone()),
            node_id: Set("design-reviewer-1".into()),
            gate_id: Set(Some("design".into())),
            gate_cycle: Set(Some(2)),
            manifest_revision: Set(1),
            content_fingerprint: Set(Some(header_fp)),
            artifact_digest: Set(Some(DESIGN_DOC_DIGEST.into())),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(2),
            summary_validated: Set(true),
            created_at: Set(old),
            updated_at: Set(old),
            ..Default::default()
        };
        // Need a run row for terminal check.
        let child =
            seed_conversation(&db, seed_folder(&db, "/tmp/stale").await, AgentType::Codex).await;
        let run = delegation_task_run::ActiveModel {
            task_id: Set("task-c1-stale".into()),
            root_task_id: Set("task-c1-stale".into()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(parent),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set("codex".into()),
            profile_id: Set(None),
            workspace_path: Set(None),
            route_fingerprint: Set(None),
            launch_snapshot_version: Set(None),
            mode_id: Set(None),
            config_values_json: Set(None),
            task_preview: Set(None),
            request_fingerprint: Set(None),
            admission_class: Set(AdmissionClass::NormalRevision),
            reached_running_at: Set(Some(old)),
            lineage_root_task_id: Set("task-c1-stale".into()),
            work_unit_key: Set(None),
            legacy_parent_tool_use_id: Set(None),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            error_code: Set(None),
            termination_audit_json: Set(None),
            started_at: Set(Some(old)),
            finished_at: Set(Some(old)),
            tool_call_count: Set(None),
            edit_tool_call_count: Set(None),
            touched_files_json: Set(None),
            touched_files_truncated: Set(None),
            additions: Set(None),
            deletions: Set(None),
            line_counts_complete: Set(None),
            card_summary_json: Set(Some("{}".into())),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(None),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(old),
            updated_at: Set(old),
            ..Default::default()
        };
        run.insert(&db.conn).await.unwrap();
        rb.insert(&db.conn).await.unwrap();

        let err = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id.clone(),
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: s1.graph_revision,
                gate_cycle: 2,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(0, 0, 0),
                summary: "stale".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::GateNotReady(_)));

        // Fresh cycle-2 run works.
        insert_design_reviewer_evidence(
            &db,
            parent,
            &r.workflow_id,
            "task-c2-fresh",
            2,
            10,
            "approve",
        )
        .await;
        settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: s1.graph_revision,
                gate_cycle: 2,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(0, 0, 0),
                summary: "ok".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .expect("fresh cycle 2");
    }

    #[tokio::test]
    async fn zero_reviewer_design_requires_self_review_decision() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: zero_reviewer_design_doc("tok-self"),
            },
        )
        .await
        .unwrap();

        let error = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(0, 0, 0),
                summary: "self ack".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .expect_err("self-review needs a platform decision");
        assert_eq!(error, WorkflowStoreError::CompletionDecisionRequired);
    }

    #[tokio::test]
    async fn design_self_review_preflight_rechecks_exact_protocol_pair_before_writes() {
        use crate::db::entities::delegation_workflow::CompletionProtocolMode;

        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs/superpowers/specs")).unwrap();
        let design_bytes = b"# Current Design\n";
        std::fs::write(
            workspace.path().join("docs/superpowers/specs/x.md"),
            design_bytes,
        )
        .unwrap();
        let db = crate::db::test_helpers::historical_completion_protocol_db_before_v2_only().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let (emitter, _) = emitter_with_rx();
        let mut document = zero_reviewer_design_doc("task3-preflight-pair-flip");
        document.design.as_mut().unwrap().digest = format!("sha256:{}", sha256_hex(design_bytes));
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .unwrap();
        let header_before = delegation_workflow::Entity::find_by_id(&published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(header_before.completion_protocol_version, 2);
        assert_eq!(
            header_before.completion_protocol_mode,
            CompletionProtocolMode::V2Enforce
        );
        let gate_states_before = delegation_workflow_gate_state::Entity::find()
            .filter(delegation_workflow_gate_state::Column::WorkflowId.eq(&published.workflow_id))
            .order_by_asc(delegation_workflow_gate_state::Column::GateId)
            .all(&db.conn)
            .await
            .unwrap();
        let design_bindings_before = delegation_workflow_design_root_binding::Entity::find()
            .filter(
                delegation_workflow_design_root_binding::Column::WorkflowId
                    .eq(&published.workflow_id),
            )
            .order_by_asc(delegation_workflow_design_root_binding::Column::GateId)
            .all(&db.conn)
            .await
            .unwrap();
        let attentions_before = delegation_attention_request::Entity::find()
            .order_by_asc(delegation_attention_request::Column::RequestId)
            .all(&db.conn)
            .await
            .unwrap();

        let (preflight_entered, release_preflight) =
            install_settle_v2_preflight_test_gate(published.workflow_id.clone());
        let settle_db = AppDatabase {
            conn: db.conn.clone(),
        };
        let settle_emitter = emitter.clone();
        let workflow_id = published.workflow_id.clone();
        let expected_graph_revision = published.graph_revision;
        let settle_task = tokio::spawn(async move {
            settle_workflow_gate_v2_core(
                &settle_db,
                &settle_emitter,
                parent,
                SettleWorkflowV2Request {
                    workflow_id,
                    gate_id: "design".into(),
                    expected_graph_revision,
                    expected_review_round: Some(1),
                    expected_outcome: Some(GateSettlementOutcome::Approved),
                    summary: "pair changed before Design preflight".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
        });
        preflight_entered
            .await
            .expect("outer exact-pair guard must complete before the test pair flip");

        let mut flipped: delegation_workflow::ActiveModel =
            delegation_workflow::Entity::find_by_id(&published.workflow_id)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap()
                .into();
        flipped.completion_protocol_mode = Set(CompletionProtocolMode::V2Shadow);
        flipped.update(&db.conn).await.unwrap();
        crate::db::test_helpers::complete_historical_completion_protocol_migrations(&db).await;
        release_preflight.send(()).unwrap();

        let error = settle_task.await.unwrap().unwrap_err();
        assert_eq!(
            error,
            WorkflowStoreError::UnsupportedCompletionProtocol {
                version: 2,
                mode: CompletionProtocolMode::V2Shadow,
            }
        );
        let header_after = delegation_workflow::Entity::find_by_id(&published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            header_after.graph_revision, header_before.graph_revision,
            "rejected preflight must not rotate graph revision"
        );
        assert_eq!(
            delegation_workflow_gate_state::Entity::find()
                .filter(
                    delegation_workflow_gate_state::Column::WorkflowId.eq(&published.workflow_id),
                )
                .order_by_asc(delegation_workflow_gate_state::Column::GateId)
                .all(&db.conn)
                .await
                .unwrap(),
            gate_states_before,
            "rejected preflight must not mutate gate states"
        );
        assert_eq!(
            delegation_workflow_design_root_binding::Entity::find()
                .filter(
                    delegation_workflow_design_root_binding::Column::WorkflowId
                        .eq(&published.workflow_id),
                )
                .order_by_asc(delegation_workflow_design_root_binding::Column::GateId)
                .all(&db.conn)
                .await
                .unwrap(),
            design_bindings_before,
            "rejected preflight must not mutate Design-root bindings"
        );
        assert_eq!(
            delegation_attention_request::Entity::find()
                .order_by_asc(delegation_attention_request::Column::RequestId)
                .all(&db.conn)
                .await
                .unwrap(),
            attentions_before,
            "rejected preflight must not mutate attention requests"
        );
    }

    async fn design_preflight_semantic_snapshot(db: &AppDatabase, workflow_id: &str) -> String {
        let graph_revision = db
            .conn
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Sqlite,
                "SELECT graph_revision FROM delegation_workflows WHERE workflow_id = ?",
                vec![workflow_id.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "graph_revision")
            .unwrap();
        let gate_states = delegation_workflow_gate_state::Entity::find()
            .filter(delegation_workflow_gate_state::Column::WorkflowId.eq(workflow_id))
            .order_by_asc(delegation_workflow_gate_state::Column::GateId)
            .all(&db.conn)
            .await
            .unwrap();
        let design_bindings = delegation_workflow_design_root_binding::Entity::find()
            .filter(delegation_workflow_design_root_binding::Column::WorkflowId.eq(workflow_id))
            .order_by_asc(delegation_workflow_design_root_binding::Column::GateId)
            .all(&db.conn)
            .await
            .unwrap();
        let attentions = delegation_attention_request::Entity::find()
            .order_by_asc(delegation_attention_request::Column::RequestId)
            .all(&db.conn)
            .await
            .unwrap();
        format!(
            "graph={graph_revision}|gates={gate_states:?}|bindings={design_bindings:?}|attentions={attentions:?}"
        )
    }

    #[tokio::test]
    async fn design_self_review_preflight_maps_concurrent_corrupt_header_without_writes() {
        use sea_orm::ConnectionTrait;

        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs/superpowers/specs")).unwrap();
        let design_bytes = b"# Current Design\n";
        std::fs::write(
            workspace.path().join("docs/superpowers/specs/x.md"),
            design_bytes,
        )
        .unwrap();
        let db = crate::db::test_helpers::historical_completion_protocol_db_before_v2_only().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let (emitter, _) = emitter_with_rx();
        let mut document = zero_reviewer_design_doc("task4-preflight-corrupt-mode");
        document.design.as_mut().unwrap().digest = format!("sha256:{}", sha256_hex(design_bytes));
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .unwrap();
        let before = design_preflight_semantic_snapshot(&db, &published.workflow_id).await;

        let (preflight_entered, release_preflight) =
            install_design_preflight_header_test_gate(published.workflow_id.clone());
        let settle_db = AppDatabase {
            conn: db.conn.clone(),
        };
        let settle_emitter = emitter.clone();
        let workflow_id = published.workflow_id.clone();
        let corrupt_workflow_id = workflow_id.clone();
        let settle_task = tokio::spawn(async move {
            settle_workflow_gate_v2_core(
                &settle_db,
                &settle_emitter,
                parent,
                SettleWorkflowV2Request {
                    workflow_id,
                    gate_id: "design".into(),
                    expected_graph_revision: published.graph_revision,
                    expected_review_round: Some(1),
                    expected_outcome: Some(GateSettlementOutcome::Approved),
                    summary: "corrupt header before Design preflight read".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
        });
        preflight_entered
            .await
            .expect("all outer guards must complete before corrupting the preflight header");

        db.conn
            .execute_unprepared("PRAGMA ignore_check_constraints = ON")
            .await
            .unwrap();
        let update = db
            .conn
            .execute(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Sqlite,
                "UPDATE delegation_workflows SET completion_protocol_mode = ? WHERE workflow_id = ?",
                vec!["corrupt_mode".into(), corrupt_workflow_id.into()],
            ))
            .await;
        db.conn
            .execute_unprepared("PRAGMA ignore_check_constraints = OFF")
            .await
            .unwrap();
        update.unwrap();
        crate::db::test_helpers::complete_historical_completion_protocol_migrations(&db).await;
        release_preflight.send(()).unwrap();

        let error = settle_task.await.unwrap().unwrap_err();
        assert_eq!(error.code(), "unsupported_completion_protocol");
        assert!(!error.is_retryable());
        assert!(matches!(
            error,
            WorkflowStoreError::UnsupportedCompletionProtocolHeader(_)
        ));
        assert_eq!(
            design_preflight_semantic_snapshot(&db, &published.workflow_id).await,
            before,
            "corrupt Design preflight rejection must not mutate semantic state"
        );
    }

    #[tokio::test]
    async fn design_self_review_decision_is_required_and_persists_null_counts() {
        use crate::acp::delegation::workflow::completion_evidence::{
            completion_attention_public_node_id, resolve_design_self_review_txn,
            CompletionAttentionCas,
        };
        use crate::db::entities::delegation_workflow::{CompletionProtocolMode, WorkflowState};

        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs/superpowers/specs")).unwrap();
        let design_bytes = b"# Current Design\n";
        std::fs::write(
            workspace.path().join("docs/superpowers/specs/x.md"),
            design_bytes,
        )
        .unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let (emitter, _) = emitter_with_rx();
        let mut document = zero_reviewer_design_doc("task13-design-self-review");
        document.design.as_mut().unwrap().digest = format!("sha256:{}", sha256_hex(design_bytes));
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .unwrap();
        let header = delegation_workflow::Entity::find_by_id(&published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(header.workflow_state, WorkflowState::Estimated);
        let mut header: delegation_workflow::ActiveModel = header.into();
        header.completion_protocol_version = Set(2);
        header.completion_protocol_mode = Set(CompletionProtocolMode::V2Enforce);
        header.update(&db.conn).await.unwrap();

        let request = SettleWorkflowV2Request {
            workflow_id: published.workflow_id.clone(),
            gate_id: "design".into(),
            expected_graph_revision: published.graph_revision,
            expected_review_round: Some(1),
            expected_outcome: Some(GateSettlementOutcome::Approved),
            summary: "caller fields are not authority".into(),
            recovery_authorization_id: None,
        };
        assert_eq!(
            settle_workflow_gate_v2_core(&db, &emitter, parent, request.clone())
                .await
                .unwrap_err(),
            WorkflowStoreError::CompletionDecisionRequired
        );

        let binding = delegation_workflow_design_root_binding::Entity::find()
            .filter(
                delegation_workflow_design_root_binding::Column::WorkflowId
                    .eq(&published.workflow_id),
            )
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert!(delegation_task_run::Entity::find_by_id(&binding.task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .is_none());
        let platform_node = delegation_workflow_node_binding::Entity::find_by_id((
            published.workflow_id.clone(),
            binding.node_id.clone(),
        ))
        .one(&db.conn)
        .await
        .unwrap();
        assert!(
            platform_node.is_none(),
            "platform Design root leaked into manifest node bindings"
        );
        let readiness_header = delegation_workflow::Entity::find_by_id(&published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            readiness_header.graph_revision as u64,
            published.graph_revision + 1,
            "opening Design self-review authority must rotate graph CAS"
        );
        assert_eq!(binding.graph_revision, readiness_header.graph_revision);
        let attention = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(&binding.task_id))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let cas = CompletionAttentionCas {
            attention_id: attention.request_id,
            task_id: attention.task_id,
            kind: attention.kind,
            captured_scope_digest: attention.captured_scope_digest.unwrap(),
            latest_run_id: attention.latest_run_id.unwrap(),
            node_id: completion_attention_public_node_id(&attention.node_id.unwrap()),
        };
        let resolved = super::super::with_historical_workflow_fixture_mutations(
            resolve_design_self_review_txn(
                &db,
                parent,
                cas,
                CompletionOutcome::Approve,
                "authenticated-user",
            ),
        )
        .await
        .unwrap();
        let mut request = request;
        request.expected_graph_revision = resolved.graph_revision;
        let settled = settle_workflow_gate_v2_core(&db, &emitter, parent, request)
            .await
            .unwrap();
        assert_eq!(settled.outcome, GateSettlementOutcome::Approved);
        let row = delegation_workflow_gate_settlement::Entity::find_by_id((
            published.workflow_id,
            "design".to_string(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            (row.critical_count, row.important_count, row.minor_count),
            (None, None, None)
        );

        let round_mismatch = settle_workflow_gate_v2_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowV2Request {
                workflow_id: row.workflow_id.clone(),
                gate_id: "design".into(),
                expected_graph_revision: settled.graph_revision,
                expected_review_round: Some(2),
                expected_outcome: Some(GateSettlementOutcome::Approved),
                summary: "caller fields are not authority".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            round_mismatch,
            WorkflowStoreError::GateCycleConflict(_)
        ));

        let outcome_mismatch = settle_workflow_gate_v2_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowV2Request {
                workflow_id: row.workflow_id.clone(),
                gate_id: "design".into(),
                expected_graph_revision: settled.graph_revision,
                expected_review_round: Some(1),
                expected_outcome: Some(GateSettlementOutcome::Blocked),
                summary: "caller fields are not authority".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            outcome_mismatch,
            WorkflowStoreError::GateNotReady(_)
        ));

        let replay = settle_workflow_gate_v2_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowV2Request {
                workflow_id: row.workflow_id.clone(),
                gate_id: "design".into(),
                expected_graph_revision: settled.graph_revision,
                expected_review_round: Some(1),
                expected_outcome: Some(GateSettlementOutcome::Approved),
                summary: "caller fields are not authority".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.gate_cycle, 1);
        assert_eq!(
            delegation_workflow_gate_settlement::Entity::find()
                .filter(
                    delegation_workflow_gate_settlement::Column::WorkflowId.eq(&row.workflow_id),
                )
                .filter(delegation_workflow_gate_settlement::Column::GateId.eq("design"))
                .all(&db.conn)
                .await
                .unwrap()
                .len(),
            1
        );

        std::fs::write(
            workspace.path().join("docs/superpowers/specs/x.md"),
            b"# Changed Design\n",
        )
        .unwrap();
        let header = delegation_workflow::Entity::find_by_id(&row.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let superseded = settle_workflow_gate_v2_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowV2Request {
                workflow_id: row.workflow_id,
                gate_id: "design".into(),
                expected_graph_revision: header.graph_revision as u64,
                expected_review_round: Some(1),
                expected_outcome: Some(GateSettlementOutcome::Approved),
                summary: "stale decision cannot settle changed bytes".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(superseded, WorkflowStoreError::CompletionDecisionSuperseded);
        let rotated_header = delegation_workflow::Entity::find_by_id(&header.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rotated_header.graph_revision, header.graph_revision + 1);
        assert_eq!(
            delegation_workflow_design_root_binding::Entity::find()
                .filter(
                    delegation_workflow_design_root_binding::Column::WorkflowId
                        .eq(&header.workflow_id),
                )
                .all(&db.conn)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn external_design_gate_reduces_current_validated_reviewer_evidence() {
        use crate::acp::delegation::run_store::{ReservingRunInsert, RunStore};
        use crate::acp::delegation::workflow::completion_evidence::{
            materialize_terminal_completion_txn, TerminalCompletionInput,
        };
        use crate::db::entities::delegation_completion_tool_intent;
        use crate::db::entities::delegation_workflow::{CompletionProtocolMode, WorkflowState};
        for (review_outcome, expected) in [
            (CompletionOutcome::Approve, GateSettlementOutcome::Approved),
            (
                CompletionOutcome::RequestChanges,
                GateSettlementOutcome::ChangesRequested,
            ),
            (CompletionOutcome::Block, GateSettlementOutcome::Blocked),
        ] {
            let workspace = tempfile::tempdir().unwrap();
            let design_bytes = format!("# External Design\n\n{}\n", review_outcome.as_str());
            let design_path = workspace.path().join("docs/superpowers/specs/x.md");
            std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
            std::fs::write(&design_path, design_bytes.as_bytes()).unwrap();

            let db = fresh_in_memory_db().await;
            let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
            let parent = seed_conversation(&db, folder, AgentType::Codex).await;
            let child = seed_conversation(&db, folder, AgentType::Codex).await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc(&format!(
                "task13-external-design-{}",
                review_outcome.as_str()
            ));
            document.design.as_mut().unwrap().digest =
                format!("sha256:{}", sha256_hex(design_bytes.as_bytes()));
            let reviewer = document
                .nodes
                .iter_mut()
                .find(|node| node.id == "design-reviewer-1")
                .unwrap();
            reviewer.agent_type = Some("codex".into());
            reviewer.profile_id = None;
            reviewer.work_unit_key = Some(
                build_work_unit_key(&WorkUnitKeyParts::Design {
                    rel_doc_path: "docs/superpowers/specs/x.md",
                    agent_type: "codex",
                    profile_id: None,
                })
                .unwrap(),
            );
            let published = publish_workflow_manifest_fixture(
                &db,
                &emitter,
                parent,
                PublishWorkflowRequest { document },
            )
            .await
            .unwrap();
            let header = delegation_workflow::Entity::find_by_id(&published.workflow_id)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(header.workflow_state, WorkflowState::Estimated);
            let mut header: delegation_workflow::ActiveModel = header.into();
            header.completion_protocol_version = Set(2);
            header.completion_protocol_mode = Set(CompletionProtocolMode::V2Enforce);
            header.update(&db.conn).await.unwrap();
            set_initialized_gate_state(
                &db,
                &published.workflow_id,
                "design",
                format!("sha256:{}", "d".repeat(64)),
                1,
                "[\"design-reviewer-1\"]".into(),
            )
            .await;

            let task_id = format!("task13-design-{}", review_outcome.as_str());
            let node = delegation_workflow_node_binding::Entity::find_by_id((
                published.workflow_id.clone(),
                "design-reviewer-1".to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let runs = RunStore::new(Arc::new(AppDatabase {
                conn: db.conn.clone(),
            }));
            super::super::with_historical_workflow_fixture_mutations(runs.admit_gen1_reserving(
                ReservingRunInsert {
                    dispatch_intent_id: None,
                    orchestration_binding: None,
                    task_id: task_id.clone(),
                    root_task_id: task_id.clone(),
                    previous_task_id: None,
                    generation: 1,
                    parent_conversation_id: parent,
                    parent_tool_use_id: Some(format!("tool-{task_id}")),
                    child_conversation_id: child,
                    agent_type: "codex".into(),
                    profile_id: None,
                    workspace_path: Some(workspace.path().to_string_lossy().into_owned()),
                    route_fingerprint: Some(format!("route-{task_id}")),
                    launch_snapshot_version: Some("v1".into()),
                    mode_id: None,
                    config_values_json: Some("{}".into()),
                    task_preview: Some("Review external Design".into()),
                    request_fingerprint: Some(format!("fingerprint-{task_id}")),
                    admission_class: AdmissionClass::NormalRevision,
                    lineage_root_task_id: task_id.clone(),
                    work_unit_key: Some(node.work_unit_key),
                    history_only: false,
                    replaced_task_id: None,
                    replacement_reason: None,
                    started_at: Some(Utc::now()),
                },
            ))
            .await
            .unwrap();
            let run = delegation_task_run::Entity::find_by_id(&task_id)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut run: delegation_task_run::ActiveModel = run.into();
            run.status = Set(DelegationRunStatus::Completed);
            run.finished_at = Set(Some(Utc::now()));
            run.card_summary_json = Set(Some("{malformed legacy card".into()));
            run.update(&db.conn).await.unwrap();
            delegation_completion_tool_intent::ActiveModel {
                intent_id: Set(format!("intent-{task_id}")),
                task_id: Set(task_id.clone()),
                child_tool_call_id: Set(format!("call-{task_id}")),
                accepted_ordinal: Set(1),
                outcome: Set(review_outcome.as_str().into()),
                summary: Set(Some("typed reviewer outcome".into())),
                report_hint: Set(None),
                request_digest: Set(format!("digest-{task_id}")),
                created_at: Set(Utc::now()),
            }
            .insert(&db.conn)
            .await
            .unwrap();
            let completion = super::super::with_historical_workflow_fixture_mutations(
                materialize_terminal_completion_txn(
                    &db.conn,
                    TerminalCompletionInput {
                        task_id: task_id.clone(),
                        terminal_status: DelegationRunStatus::Completed,
                        final_assistant_text: "typed completion intent wins".into(),
                        pre_read_reports: Vec::new(),
                        pre_read_artifact: None,
                    },
                ),
            )
            .await
            .unwrap();
            assert_eq!(completion.state, CompletionState::Resolved);

            let current = delegation_workflow::Entity::find_by_id(&published.workflow_id)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let settled = settle_workflow_gate_v2_core(
                &db,
                &emitter,
                parent,
                SettleWorkflowV2Request {
                    workflow_id: published.workflow_id.clone(),
                    gate_id: "design".into(),
                    expected_graph_revision: current.graph_revision as u64,
                    expected_review_round: Some(1),
                    expected_outcome: Some(expected.clone()),
                    summary: "platform-reduced external Design".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .unwrap();
            assert_eq!(settled.outcome, expected);
            let row = delegation_workflow_gate_settlement::Entity::find_by_id((
                published.workflow_id,
                "design".to_string(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                (row.critical_count, row.important_count, row.minor_count),
                (None, None, None)
            );
        }
    }

    #[tokio::test]
    async fn caller_finding_counts_do_not_bypass_design_self_review() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: zero_reviewer_design_doc("tok-crit"),
            },
        )
        .await
        .unwrap();

        let err = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(1, 0, 0),
                summary: "bad approve".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err, WorkflowStoreError::CompletionDecisionRequired);
    }

    #[tokio::test]
    async fn summary_oversize_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: zero_reviewer_design_doc("tok-sum"),
            },
        )
        .await
        .unwrap();

        let err = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::ChangesRequested,
                evidence: design_evidence(0, 0, 0),
                summary: "x".repeat(MAX_ADJUDICATION_SUMMARY_BYTES + 1),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::SummaryTooLarge));
    }

    #[tokio::test]
    async fn caller_negative_finding_counts_are_not_v2_authority() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: zero_reviewer_design_doc("tok-neg-counts"),
            },
        )
        .await
        .unwrap();

        for (critical, important, minor) in [(-1, 0, 0), (0, -1, 0), (0, 0, -1), (-2, -3, -4)] {
            let err = settle_workflow_gate_core(
                &db,
                &emitter,
                parent,
                SettleWorkflowRequest {
                    workflow_id: r.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "design".into(),
                    expected_graph_revision: 1,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::ChangesRequested,
                    evidence: design_evidence(critical, important, minor),
                    summary: "negative counts".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .unwrap_err();
            assert_eq!(err, WorkflowStoreError::CompletionDecisionRequired);
        }
    }

    #[tokio::test]
    async fn b14_partner_freeze_on_plan_revision() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-b14");
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();

        // Only implementer observed — cohort_frozen not pre-set (admission would set it;
        // publish must still protect partner via is_observed on mate).
        let impl_binding = delegation_workflow_node_binding::Entity::find_by_id((
            r.workflow_id.clone(),
            "task-1-impl".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut am: delegation_workflow_node_binding::ActiveModel = impl_binding.into();
        am.is_observed = Set(true);
        am.update(&db.conn).await.unwrap();

        // Drop reviewer partner silently.
        doc.workflow_id = Some(r.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.nodes.retain(|n| n.id != "task-1-rev");
        for n in &mut doc.nodes {
            n.deps.retain(|d| d != "task-1-rev");
        }
        // final deps used task-1-rev
        for n in &mut doc.nodes {
            if n.id == "final-reviewer" {
                n.deps = vec!["task-1-impl".into()];
            }
        }
        doc.edges
            .retain(|e| e.from != "task-1-rev" && e.to != "task-1-rev");
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-rev".into();
        }

        let err = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap_err();
        // V2 route validation rejects an omitted cohort node before the existing
        // frozen-pair defense-in-depth path needs to inspect the binding diff.
        assert!(
            matches!(err, WorkflowStoreError::CohortFrozen { .. })
                || matches!(
                    err,
                    WorkflowStoreError::Validation(
                        super::super::types::WorkflowError::TaskRouteMismatch(_)
                    )
                ),
            "expected freeze or route-validation reject, got {err:?}"
        );
    }

    #[tokio::test]
    async fn workflow_v2_typed_error_real_producers_cohort_frozen() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut document = design_plan_doc("typed-cohort-frozen");
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: document.clone(),
            },
        )
        .await
        .expect("publish workflow");

        let binding = delegation_workflow_node_binding::Entity::find_by_id((
            published.workflow_id.clone(),
            "task-1-impl".to_string(),
        ))
        .one(&db.conn)
        .await
        .expect("load implementer binding")
        .expect("implementer binding");
        let mut active: delegation_workflow_node_binding::ActiveModel = binding.into();
        active.is_observed = Set(true);
        active.update(&db.conn).await.expect("observe implementer");

        document.workflow_id = Some(published.workflow_id);
        document.expected_manifest_revision = Some(1);
        document.task_policies[0].risk.reason = "changed after cohort admission".into();
        document.plan.as_mut().expect("Plan document").digest = "sha256:plan-revision".into();
        let error = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document },
        )
        .await
        .expect_err("an admitted Task policy is immutable");

        assert!(matches!(&error, WorkflowStoreError::CohortFrozen { .. }));
        let code = crate::acp::delegation::listener::workflow_store_error_code_for_test(error);
        assert_eq!(code, "cohort_frozen");
    }

    #[tokio::test]
    async fn b14_3_cancel_block_publish_retains_bindings() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-b143");
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();

        // Freeze pair as if implementer started.
        for node_id in ["task-1-impl", "task-1-rev"] {
            let b = delegation_workflow_node_binding::Entity::find_by_id((
                r.workflow_id.clone(),
                node_id.to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut am: delegation_workflow_node_binding::ActiveModel = b.into();
            am.cohort_frozen = Set(true);
            if node_id == "task-1-impl" {
                am.is_observed = Set(true);
            }
            am.update(&db.conn).await.unwrap();
        }

        doc.workflow_id = Some(r.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.workflow_state = ManifestWorkflowState::Blocked;
        for n in &mut doc.nodes {
            if n.id == "task-1-impl" || n.id == "task-1-rev" {
                n.node_outcome = Some(ManifestNodeOutcome::Canceled);
            }
        }
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-cancel".into();
        }

        let r2 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("cancel publish");
        assert_eq!(r2.workflow_state, ManifestWorkflowState::Blocked);

        let state = get_workflow_state_core(&db, parent, Some(&r.workflow_id))
            .await
            .unwrap();
        let impl_n = state
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-impl")
            .unwrap();
        let rev_n = state
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-rev")
            .unwrap();
        assert_eq!(state.workflow_state, ManifestWorkflowState::Blocked);
        assert!(state.actionable_task_routes.is_empty());
        assert_eq!(impl_n.task_index, Some(1));
        assert_eq!(rev_n.task_index, Some(1));
        let state_json = serde_json::to_value(state).unwrap();
        assert!(state_json.pointer("/nodes/0/node_outcome").is_none());
        assert!(state_json.pointer("/nodes/0/cohort_frozen").is_none());
    }

    #[tokio::test]
    async fn admitted_node_identity_mutation_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-idmut");
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();

        let b = delegation_workflow_node_binding::Entity::find_by_id((
            r.workflow_id.clone(),
            "design-reviewer-1".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut am: delegation_workflow_node_binding::ActiveModel = b.into();
        am.is_observed = Set(true);
        am.update(&db.conn).await.unwrap();

        // Change agent_type on admitted design reviewer (also changes key).
        doc.workflow_id = Some(r.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        for n in &mut doc.nodes {
            if n.id == "design-reviewer-1" {
                n.agent_type = Some("codex".into());
                n.profile_id = None;
                n.work_unit_key = Some(
                    build_work_unit_key(&WorkUnitKeyParts::Design {
                        rel_doc_path: "docs/superpowers/specs/x.md",
                        agent_type: "codex",
                        profile_id: None,
                    })
                    .unwrap(),
                );
            }
        }
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-id".into();
        }

        let err = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            WorkflowStoreError::AdmittedNodeIdentityMutation { .. }
        ));
    }

    #[tokio::test]
    async fn injected_persistence_rollback_no_partial_header() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        set_inject_publish_persistence_failure(true);
        let err = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-fail"),
            },
        )
        .await
        .unwrap_err();
        set_inject_publish_persistence_failure(false);
        assert!(matches!(err, WorkflowStoreError::Persistence(_)));

        let count = delegation_workflow::Entity::find()
            .all(&db.conn)
            .await
            .unwrap()
            .len();
        assert_eq!(count, 0, "header must not remain after rollback");
        let revs = delegation_workflow_manifest_revision::Entity::find()
            .all(&db.conn)
            .await
            .unwrap()
            .len();
        assert_eq!(revs, 0);
    }

    #[tokio::test]
    async fn get_workflow_state_index_preserves_recovery_authority() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-state"),
            },
        )
        .await
        .unwrap();

        insert_design_reviewer_evidence(
            &db,
            parent,
            &r.workflow_id,
            "task-state-1",
            1,
            0,
            "approve",
        )
        .await;

        let state = get_workflow_state_core(&db, parent, Some(&r.workflow_id))
            .await
            .unwrap();
        assert_eq!(state.workflow_id, r.workflow_id);
        assert_eq!(state.manifest_revision, 1);
        assert_eq!(state.detail, WorkflowStateDetail::Index);
        assert!(!state.inline_findings);
        let design = state
            .nodes
            .iter()
            .find(|n| n.node_id == "design-reviewer-1")
            .unwrap();
        assert_eq!(design.latest_task_id.as_deref(), Some("task-state-1"));
        assert_eq!(design.latest_status.as_deref(), Some("completed"));
        assert!(serde_json::to_value(state)
            .unwrap()
            .pointer("/nodes/0/latest_generation")
            .is_none());
    }

    #[tokio::test]
    async fn second_create_token_conflict() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-a"),
            },
        )
        .await
        .unwrap();
        let err = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-b"),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            WorkflowStoreError::PublicationTokenConflict { .. }
        ));
    }

    #[tokio::test]
    async fn a2_stale_manifest_revision_on_run_binding_rejected() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-a2-rev"),
            },
        )
        .await
        .unwrap();

        // Review bound to wrong (stale) manifest_revision=0.
        insert_terminal_reviewer_run(
            &db,
            parent,
            &r.workflow_id,
            "design-reviewer-1",
            "design",
            1,
            "task-stale-rev",
            true,
            0,
            DESIGN_DOC_DIGEST,
            DelegationRunStatus::Completed,
            0,
        )
        .await;

        let err = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(0, 0, 0),
                summary: "stale rev".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::GateNotReady(_)));
    }

    #[tokio::test]
    async fn a2_stale_artifact_digest_rejected() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-a2-digest"),
            },
        )
        .await
        .unwrap();

        insert_terminal_reviewer_run(
            &db,
            parent,
            &r.workflow_id,
            "design-reviewer-1",
            "design",
            1,
            "task-stale-digest",
            true,
            0,
            "sha256:OLD-design",
            DelegationRunStatus::Completed,
            1,
        )
        .await;

        let err = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(0, 0, 0),
                summary: "stale digest".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::GateNotReady(_)));
    }

    #[tokio::test]
    async fn failed_reviewer_cannot_approve() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-fail-approve"),
            },
        )
        .await
        .unwrap();

        insert_terminal_reviewer_run(
            &db,
            parent,
            &r.workflow_id,
            "design-reviewer-1",
            "design",
            1,
            "task-failed-rev",
            true,
            0,
            DESIGN_DOC_DIGEST,
            DelegationRunStatus::Failed,
            1,
        )
        .await;

        let err = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id.clone(),
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::Approved,
                evidence: design_evidence(0, 0, 0),
                summary: "bad approve".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::GateNotReady(_)));

        let error = settle_workflow_gate_core(
            &db,
            &emitter,
            parent,
            SettleWorkflowRequest {
                workflow_id: r.workflow_id,
                manifest_revision: 1,
                gate_id: "design".into(),
                expected_graph_revision: 1,
                gate_cycle: 1,
                outcome: GateSettlementOutcome::ChangesRequested,
                evidence: design_evidence(1, 0, 0),
                summary: "review failed".into(),
                recovery_authorization_id: None,
            },
        )
        .await
        .expect_err("failed reviewer cannot provide v2 gate authority");
        assert!(matches!(error, WorkflowStoreError::GateNotReady(_)));
    }

    #[test]
    fn pure_reclassify_same_digest_is_idempotent_replay() {
        let r = classify_header_against_digest(
            "tok",
            1,
            1,
            "wf-1".into(),
            Some("digest-a"),
            "digest-a",
            1,
            1,
            ManifestWorkflowState::Estimated,
        )
        .expect("same digest");
        assert!(r.idempotent_replay);
        assert_eq!(r.workflow_id, "wf-1");
    }

    #[test]
    fn pure_reclassify_different_digest_mismatch_has_real_workflow_id() {
        let err = classify_header_against_digest(
            "tok",
            1,
            1,
            "wf-real".into(),
            Some("digest-a"),
            "digest-b",
            1,
            1,
            ManifestWorkflowState::Estimated,
        )
        .unwrap_err();
        match err {
            WorkflowStoreError::PublicationTokenMismatch {
                publication_token,
                workflow_id,
            } => {
                assert_eq!(publication_token, "tok");
                assert_eq!(workflow_id, "wf-real");
                assert!(!workflow_id.is_empty());
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }
    }

    #[test]
    fn pure_reclassify_cross_parent() {
        let err = classify_header_against_digest(
            "tok",
            1,
            99,
            "wf-1".into(),
            Some("d"),
            "d",
            1,
            1,
            ManifestWorkflowState::Estimated,
        )
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::CrossParent { .. }));
    }

    #[tokio::test]
    async fn reclassify_absent_token_is_busy_not_fabricated_mismatch() {
        let (db, parent) = seed_parent().await;
        // No header for this token — after backoff must be Busy, never empty Mismatch.
        let err = classify_token_race_fresh(&db, "tok-absent-race", "sha256:x", parent)
            .await
            .unwrap_err();
        assert!(
            matches!(err, WorkflowStoreError::Busy(_)),
            "expected Busy, got {err:?}"
        );
        assert!(err.is_retryable());
        assert!(
            !matches!(err, WorkflowStoreError::PublicationTokenMismatch { .. }),
            "must not invent Mismatch without a durable token row"
        );
    }

    #[tokio::test]
    async fn approved_structural_plan_revision_force_demotes_to_estimated() {
        // A8: approved → approved with plan digest/structure change must demote
        // so old Plan settlements cannot stay valid for the new revision.
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-a8-demote");
        doc.workflow_state = ManifestWorkflowState::Approved;
        let r1 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(r1.workflow_state, ManifestWorkflowState::Approved);

        // Seed an approved plan settlement on rev 1 (stale after structural change).
        let now = Utc::now();
        let h1 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let row = delegation_workflow_gate_settlement::ActiveModel {
            workflow_id: Set(r1.workflow_id.clone()),
            gate_id: Set("plan".into()),
            gate_cycle: Set(1),
            manifest_revision: Set(1),
            structural_revision: Set(h1.structural_revision),
            content_fingerprint: Set(h1.plan_fingerprint.clone()),
            outcome: Set(GateSettlementOutcome::Approved),
            critical_count: Set(Some(0)),
            important_count: Set(Some(0)),
            minor_count: Set(Some(0)),
            summary: Set("plan ok".into()),
            graph_revision_at_settle: Set(r1.graph_revision as i64),
            created_at: Set(now),
            ..Default::default()
        };
        row.insert(&db.conn).await.unwrap();

        doc.workflow_id = Some(r1.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.workflow_state = ManifestWorkflowState::Approved; // client mistakenly keeps approved
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-structural-v2".into();
        }
        let r2 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("structural plan republish");
        assert_eq!(
            r2.workflow_state,
            ManifestWorkflowState::Estimated,
            "must force-demote approved→approved structural plan revision"
        );
        assert_eq!(r2.manifest_revision, 2);

        let header = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(header.workflow_state, WorkflowState::Estimated);
        assert_eq!(header.supersedes_approved_revision, Some(1));

        // Agent recovery state must not surface stale plan approved settlement.
        let state = get_workflow_state_core(&db, parent, Some(&r1.workflow_id))
            .await
            .unwrap();
        let plan_gate = state.gates.iter().find(|g| g.gate_id == "plan").unwrap();
        assert!(
            plan_gate.latest_outcome.is_none(),
            "stale rev-1 plan settlement must not display on rev 2"
        );
        assert_eq!(header.structural_revision, 2);
    }

    #[tokio::test]
    async fn approved_edge_only_change_force_demotes() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-edge-demote");
        doc.workflow_state = ManifestWorkflowState::Approved;
        let r1 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(r1.workflow_state, ManifestWorkflowState::Approved);

        // Edge-only material change (no digest/node id churn).
        doc.workflow_id = Some(r1.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.workflow_state = ManifestWorkflowState::Approved;
        doc.edges.push(ManifestEdge {
            id: Some("e-extra".into()),
            from: "task-1-impl".into(),
            to: "final-reviewer".into(),
        });
        let r2 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("edge-only republish");
        assert_eq!(
            r2.workflow_state,
            ManifestWorkflowState::Estimated,
            "edge-only structural change must demote approved"
        );
        let header = delegation_workflow::Entity::find_by_id(r1.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(header.structural_revision, 2);
        assert_eq!(header.supersedes_approved_revision, Some(1));
    }

    #[tokio::test]
    async fn design_digest_only_change_invalidates_both_settlements_and_demotes() {
        // Design+Plan approved; Design digest-only edit must demote and
        // invalidate *both* Design and Plan settlements (A2 + plan material input).
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-design-digest-demote");
        doc.workflow_state = ManifestWorkflowState::Approved;
        let r1 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(r1.workflow_state, ManifestWorkflowState::Approved);

        let now = Utc::now();
        let h1 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let design_fp = h1.design_fingerprint.clone();
        let plan_fp = h1.plan_fingerprint.clone();
        assert!(!design_fp.is_empty());
        assert!(!plan_fp.is_empty());

        for (gate_id, fp) in [("design", design_fp.as_str()), ("plan", plan_fp.as_str())] {
            let row = delegation_workflow_gate_settlement::ActiveModel {
                workflow_id: Set(r1.workflow_id.clone()),
                gate_id: Set(gate_id.into()),
                gate_cycle: Set(1),
                manifest_revision: Set(1),
                structural_revision: Set(h1.structural_revision),
                content_fingerprint: Set(fp.into()),
                outcome: Set(GateSettlementOutcome::Approved),
                critical_count: Set(Some(0)),
                important_count: Set(Some(0)),
                minor_count: Set(Some(0)),
                summary: Set(format!("{gate_id} ok")),
                graph_revision_at_settle: Set(1),
                created_at: Set(now),
                ..Default::default()
            };
            row.insert(&db.conn).await.unwrap();
        }

        // Design digest only (path, plan doc, graph unchanged).
        doc.workflow_id = Some(r1.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.workflow_state = ManifestWorkflowState::Approved;
        if let Some(ref mut design) = doc.design {
            design.digest = "sha256:design-edited".into();
        }
        let r2 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("design-digest republish");
        assert_eq!(
            r2.workflow_state,
            ManifestWorkflowState::Estimated,
            "design digest change must demote approved via plan fingerprint"
        );

        let h2 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(
            h2.design_fingerprint, design_fp,
            "design_fp includes path+digest (A2)"
        );
        assert_ne!(
            h2.plan_fingerprint, plan_fp,
            "plan_fp includes design identity"
        );
        assert_eq!(h2.supersedes_approved_revision, Some(1));
        assert_eq!(h2.structural_revision, 2);

        let state = get_workflow_state_core(&db, parent, Some(&r1.workflow_id))
            .await
            .unwrap();
        let plan_gate = state.gates.iter().find(|g| g.gate_id == "plan").unwrap();
        assert!(
            plan_gate.latest_outcome.is_none(),
            "plan settlement must be invalid after design digest change"
        );
        let design_gate = state.gates.iter().find(|g| g.gate_id == "design").unwrap();
        assert!(
            design_gate.latest_outcome.is_none(),
            "design settlement must be invalid after design digest change (A2)"
        );
    }

    #[tokio::test]
    async fn plan_only_rewrite_keeps_design_settlement() {
        // Plan digest change demotes and invalidates Plan settlement, but Design
        // settlement stays valid because design_fp excludes plan material.
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-plan-only-keeps-design");
        doc.workflow_state = ManifestWorkflowState::Approved;
        let r1 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        assert_eq!(r1.workflow_state, ManifestWorkflowState::Approved);

        let now = Utc::now();
        let h1 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let design_fp = h1.design_fingerprint.clone();
        let plan_fp = h1.plan_fingerprint.clone();

        for (gate_id, fp) in [("design", design_fp.as_str()), ("plan", plan_fp.as_str())] {
            let row = delegation_workflow_gate_settlement::ActiveModel {
                workflow_id: Set(r1.workflow_id.clone()),
                gate_id: Set(gate_id.into()),
                gate_cycle: Set(1),
                manifest_revision: Set(1),
                structural_revision: Set(h1.structural_revision),
                content_fingerprint: Set(fp.into()),
                outcome: Set(GateSettlementOutcome::Approved),
                critical_count: Set(Some(0)),
                important_count: Set(Some(0)),
                minor_count: Set(Some(0)),
                summary: Set(format!("{gate_id} ok")),
                graph_revision_at_settle: Set(1),
                created_at: Set(now),
                ..Default::default()
            };
            row.insert(&db.conn).await.unwrap();
        }

        doc.workflow_id = Some(r1.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.workflow_state = ManifestWorkflowState::Approved;
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-only-v2".into();
        }
        let r2 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("plan-only republish");
        assert_eq!(
            r2.workflow_state,
            ManifestWorkflowState::Estimated,
            "plan-only change must demote approved"
        );

        let h2 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            h2.design_fingerprint, design_fp,
            "design_fp must not include plan material"
        );
        assert_ne!(h2.plan_fingerprint, plan_fp);
        assert_eq!(h2.supersedes_approved_revision, Some(1));

        let state = get_workflow_state_core(&db, parent, Some(&r1.workflow_id))
            .await
            .unwrap();
        let plan_gate = state.gates.iter().find(|g| g.gate_id == "plan").unwrap();
        assert!(
            plan_gate.latest_outcome.is_none(),
            "plan settlement invalid after plan-only rewrite"
        );
        let design_gate = state.gates.iter().find(|g| g.gate_id == "design").unwrap();
        assert_eq!(
            design_gate.latest_outcome.as_deref(),
            Some("approved"),
            "design settlement must remain valid on plan-only rewrite"
        );
    }

    #[tokio::test]
    async fn state_only_approve_keeps_plan_settlement_visible() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-state-only-approve");
        doc.workflow_state = ManifestWorkflowState::Estimated;
        let r1 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();

        // Plan settled while estimated; content_fingerprint = header plan_fingerprint.
        let now = Utc::now();
        let h1 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let row = delegation_workflow_gate_settlement::ActiveModel {
            workflow_id: Set(r1.workflow_id.clone()),
            gate_id: Set("plan".into()),
            gate_cycle: Set(1),
            manifest_revision: Set(1),
            structural_revision: Set(h1.structural_revision),
            content_fingerprint: Set(h1.plan_fingerprint.clone()),
            outcome: Set(GateSettlementOutcome::Approved),
            critical_count: Set(Some(0)),
            important_count: Set(Some(0)),
            minor_count: Set(Some(0)),
            summary: Set("plan ok".into()),
            graph_revision_at_settle: Set(1),
            created_at: Set(now),
            ..Default::default()
        };
        row.insert(&db.conn).await.unwrap();

        // State-only estimated → approved (same plan content).
        doc.workflow_id = Some(r1.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.workflow_state = ManifestWorkflowState::Approved;
        let r2 = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("state-only approve");
        assert_eq!(r2.workflow_state, ManifestWorkflowState::Approved);
        assert_eq!(r2.manifest_revision, 2);

        let header = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            header.structural_revision, 1,
            "state-only approve must not bump structural_revision"
        );

        let state = get_workflow_state_core(&db, parent, Some(&r1.workflow_id))
            .await
            .unwrap();
        let plan_gate = state.gates.iter().find(|g| g.gate_id == "plan").unwrap();
        assert_eq!(
            plan_gate.latest_outcome.as_deref(),
            Some("approved"),
            "plan settlement must remain visible after state-only approve"
        );
        assert_eq!(plan_gate.latest_gate_cycle, Some(1));
    }

    #[tokio::test]
    async fn concurrent_same_token_same_digest_idempotent() {
        let (db, parent) = seed_parent().await;
        let (emitter_a, _) = emitter_with_rx();
        let (emitter_b, _) = emitter_with_rx();
        let doc = design_plan_doc("tok-race-same");

        let (r1, r2) = tokio::join!(
            publish_workflow_manifest_fixture(
                &db,
                &emitter_a,
                parent,
                PublishWorkflowRequest {
                    document: doc.clone(),
                },
            ),
            publish_workflow_manifest_fixture(
                &db,
                &emitter_b,
                parent,
                PublishWorkflowRequest { document: doc },
            ),
        );

        let a = r1.expect("first publish");
        let b = r2.expect("second publish");
        assert_eq!(a.workflow_id, b.workflow_id);
        assert_eq!(a.manifest_revision, 1);
        assert_eq!(b.manifest_revision, 1);
        // Both concurrent publishes must succeed; one may be an idempotent replay.
        assert_eq!(
            a.workflow_id, b.workflow_id,
            "both results usable: a={a:?} b={b:?}"
        );
        let headers = delegation_workflow::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(headers.len(), 1);
        let revs = delegation_workflow_manifest_revision::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(revs.len(), 1);
    }

    #[tokio::test]
    async fn concurrent_same_token_different_digest_typed() {
        let (db, parent) = seed_parent().await;
        let (emitter_a, _) = emitter_with_rx();
        let (emitter_b, _) = emitter_with_rx();
        let doc_a = design_plan_doc("tok-race-diff");
        let mut doc_b = design_plan_doc("tok-race-diff");
        if let Some(ref mut plan) = doc_b.plan {
            plan.digest = "sha256:plan-OTHER-race".into();
        }

        let (r1, r2) = tokio::join!(
            publish_workflow_manifest_fixture(
                &db,
                &emitter_a,
                parent,
                PublishWorkflowRequest { document: doc_a },
            ),
            publish_workflow_manifest_fixture(
                &db,
                &emitter_b,
                parent,
                PublishWorkflowRequest { document: doc_b },
            ),
        );

        let mut ok_count = 0;
        let mut mismatch_count = 0;
        for r in [r1, r2] {
            match r {
                Ok(_) => ok_count += 1,
                Err(WorkflowStoreError::PublicationTokenMismatch { .. }) => {
                    mismatch_count += 1;
                }
                Err(other) => panic!(
                    "different-digest race must be Ok or PublicationTokenMismatch, got {other:?}"
                ),
            }
        }
        assert_eq!(ok_count, 1, "exactly one digest wins");
        assert_eq!(
            mismatch_count, 1,
            "loser must be PublicationTokenMismatch (not Conflict/Persistence)"
        );
        let headers = delegation_workflow::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(headers.len(), 1);
    }

    #[tokio::test]
    async fn task4_v2_skeleton_estimated_after_author_and_cas_replay() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let skeleton = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: skeleton_doc("tok-task4-publish"),
            },
        )
        .await
        .expect("v2 skeleton publish");
        assert_eq!(skeleton.workflow_state, ManifestWorkflowState::Skeleton);

        insert_plan_author_evidence(
            &db,
            parent,
            &skeleton.workflow_id,
            "author-task-publish",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-publish.md",
            0,
        )
        .await;

        let mut estimated = design_plan_doc("tok-task4-publish");
        estimated.workflow_id = Some(skeleton.workflow_id.clone());
        estimated.expected_manifest_revision = Some(1);
        let update_request = PublishWorkflowRequest {
            document: estimated,
        };
        let updated =
            publish_workflow_manifest_fixture(&db, &emitter, parent, update_request.clone())
                .await
                .expect("estimated publish after Author evidence");
        assert_eq!(updated.manifest_revision, 2);
        assert_eq!(updated.workflow_state, ManifestWorkflowState::Estimated);

        let replay = publish_workflow_manifest_fixture(&db, &emitter, parent, update_request)
            .await
            .expect("same CAS payload replay");
        assert!(replay.idempotent_replay);
        assert_eq!(replay.manifest_revision, 2);

        let author_binding = delegation_workflow_node_binding::Entity::find_by_id((
            skeleton.workflow_id,
            "plan-author".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .expect("observed Author binding survives estimated publish");
        assert!(author_binding.is_observed);
    }

    #[tokio::test]
    async fn task4_publish_rejects_v1_manifest() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-task4-v1");
        doc.schema_version = 1;
        let error = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            WorkflowStoreError::Validation(WorkflowError::InvalidSchemaVersion(1))
        ));
    }

    #[test]
    fn task4_plan_fingerprint_covers_target_author_cohort_policy_and_route() {
        let base = validate_manifest_document(&design_plan_doc("tok-task4-fp")).unwrap();
        let base_fp = plan_fingerprint_hash(&base);

        let mut target = base.clone();
        target.plan_target_rel_path = "docs/superpowers/plans/other.md".into();
        assert_ne!(base_fp, plan_fingerprint_hash(&target));

        let mut author = base.clone();
        author
            .nodes
            .iter_mut()
            .find(|node| node.role == Some(ManifestNodeRole::Author))
            .unwrap()
            .title = Some("different Author material".into());
        assert_ne!(base_fp, plan_fingerprint_hash(&author));

        let mut cohort = base.clone();
        cohort
            .gates
            .iter_mut()
            .find(|gate| gate.gate_kind == DocumentGateKind::Plan)
            .unwrap()
            .reviewer_cohort_node_ids
            .push("plan-reviewer-2".into());
        assert_ne!(base_fp, plan_fingerprint_hash(&cohort));

        let mut required = base.clone();
        required
            .gates
            .iter_mut()
            .find(|gate| gate.gate_kind == DocumentGateKind::Plan)
            .unwrap()
            .required_reviewer_node_ids = vec!["different-required-reviewer".into()];
        assert_ne!(base_fp, plan_fingerprint_hash(&required));

        let mut policy = base.clone();
        policy.task_policies[0].risk.reason = "changed risk reason".into();
        assert_ne!(base_fp, plan_fingerprint_hash(&policy));

        let mut route = base;
        route.task_policies[0].route.implementer_node_id = "different-implementer".into();
        assert_ne!(base_fp, plan_fingerprint_hash(&route));
    }

    #[tokio::test]
    async fn task4_required_subset_publish_uses_current_validated_gate_runs() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let doc = two_reviewer_plan_doc("tok-task4-subset-fp");
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-stale-subset",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-stale-subset",
            "request_changes",
            "reports/review-stale-subset.md",
            0,
        )
        .await;
        let before = delegation_workflow::Entity::find_by_id(published.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();

        let mut subset = doc;
        subset.workflow_id = Some(published.workflow_id.clone());
        subset.expected_manifest_revision = Some(1);
        subset
            .gates
            .iter_mut()
            .find(|gate| gate.gate_kind == Some(DocumentGateKind::Plan))
            .unwrap()
            .required_reviewer_node_ids = vec!["plan-reviewer-1".into()];
        let updated = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: subset },
        )
        .await
        .unwrap();
        let after = delegation_workflow::Entity::find_by_id(published.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(before.plan_fingerprint, after.plan_fingerprint);

        let settled = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            2,
            updated.graph_revision,
            1,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![finding(
                    "F-subset",
                    FindingSeverity::Important,
                    FindingStatus::Open,
                    &["plan-reviewer-1"],
                )],
                "author-stale-subset",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "stale evidence",
        )
        .await
        .unwrap();
        assert_eq!(settled.outcome, GateSettlementOutcome::ChangesRequested);
    }

    #[tokio::test]
    async fn task4_score3_high_route_persists_and_recovers() {
        const PUBLICATION_TOKEN: &str = "tok-task10-score3-high-store";
        const RISK_REASON: &str = "three canonical soft-signal points require high-risk routing";
        const IMPLEMENTER_NODE_ID: &str = "task-1-impl";
        const CODEX_REVIEWER_NODE_ID: &str = "task-1-rev";
        const GROK_REVIEWER_NODE_ID: &str = "task-1-rev-grok";

        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc(PUBLICATION_TOKEN);
        doc.task_policies[0].risk = ManifestTaskRisk {
            level: TaskRiskLevel::High,
            hard_triggers: vec![],
            soft_signals: vec![
                ManifestTaskSoftSignal {
                    kind: TaskSoftSignalKind::CrossRuntimeOrProcess,
                    score: 2,
                    evidence: vec!["Tauri and server runtimes share the workflow store".into()],
                },
                ManifestTaskSoftSignal {
                    kind: TaskSoftSignalKind::SharedInterface,
                    score: 1,
                    evidence: vec!["publish and recovery share the manifest policy contract".into()],
                },
            ],
            score: 3,
            reason: RISK_REASON.into(),
        };

        let task_impl = doc
            .nodes
            .iter_mut()
            .find(|node| node.id == IMPLEMENTER_NODE_ID)
            .unwrap();
        task_impl.agent_type = Some("codex".into());
        task_impl.work_unit_key = Some(
            build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                task_index: 1,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap(),
        );
        let grok_reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        doc.nodes.push(wu(
            GROK_REVIEWER_NODE_ID,
            PHASE_TASKS,
            ManifestNodeRole::Reviewer,
            "grok",
            None,
            Some(1),
            grok_reviewer_key,
            vec![IMPLEMENTER_NODE_ID.into()],
        ));
        doc.task_policies[0].route = ManifestTaskRoute {
            implementer_node_id: IMPLEMENTER_NODE_ID.into(),
            reviewer_node_ids: vec![CODEX_REVIEWER_NODE_ID.into(), GROK_REVIEWER_NODE_ID.into()],
        };

        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish score-3 high-risk workflow through the real store");
        assert_eq!(published.manifest_revision, 1);

        let persisted = delegation_workflow_manifest_revision::Entity::find_by_id((
            published.workflow_id.clone(),
            1,
        ))
        .one(&db.conn)
        .await
        .expect("load persisted score-3 manifest revision")
        .expect("score-3 manifest revision exists");
        let persisted_doc: ManifestDocument = serde_json::from_str(&persisted.document_json)
            .expect("deserialize persisted score-3 manifest");
        assert_eq!(persisted_doc.publication_token, PUBLICATION_TOKEN);
        assert_eq!(persisted_doc.risk_policy_version, "b2d_task_risk_v1");
        assert_eq!(persisted_doc.task_policies.len(), 1);
        let persisted_policy = &persisted_doc.task_policies[0];
        assert_eq!(persisted_policy.risk.level, TaskRiskLevel::High);
        assert!(persisted_policy.risk.hard_triggers.is_empty());
        assert_eq!(persisted_policy.risk.score, 3);
        assert_eq!(persisted_policy.risk.reason, RISK_REASON);
        assert_eq!(
            persisted_policy
                .risk
                .soft_signals
                .iter()
                .map(|signal| (signal.kind, signal.score))
                .collect::<Vec<_>>(),
            vec![
                (TaskSoftSignalKind::CrossRuntimeOrProcess, 2),
                (TaskSoftSignalKind::SharedInterface, 1),
            ]
        );
        assert_eq!(
            persisted_policy.route.implementer_node_id,
            IMPLEMENTER_NODE_ID
        );
        assert_eq!(
            persisted_policy.route.reviewer_node_ids,
            vec![CODEX_REVIEWER_NODE_ID, GROK_REVIEWER_NODE_ID]
        );

        let recovery = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
            .await
            .expect("recover score-3 high-risk workflow from the real store");
        assert_eq!(recovery.publication_token, PUBLICATION_TOKEN);
        assert_eq!(recovery.workflow_state, ManifestWorkflowState::Estimated);
        assert_eq!(recovery.risk_policy_version, "b2d_task_risk_v1");
        assert_eq!(recovery.task_policies.len(), 1);
        let policy = &recovery.task_policies[0];
        assert_eq!(policy.task_index, 1);
        assert_eq!(policy.level, TaskRiskLevel::High);
        assert_eq!(recovery.actionable_task_routes.len(), 1);
        let route = &recovery.actionable_task_routes[0];
        assert_eq!(route.task_index, 1);
        assert_eq!(route.level, TaskRiskLevel::High);
        assert_eq!(route.implementer_node_id, IMPLEMENTER_NODE_ID);
        assert_eq!(
            route.reviewer_node_ids,
            vec![CODEX_REVIEWER_NODE_ID, GROK_REVIEWER_NODE_ID]
        );
    }

    #[tokio::test]
    async fn task4_plan_initial_round_persists_derived_state_and_index_recovery() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = two_reviewer_plan_doc("tok-task4-recovery");
        doc.task_policies[0].risk = ManifestTaskRisk {
            level: TaskRiskLevel::High,
            hard_triggers: vec![
                ManifestTaskHardTrigger {
                    kind: TaskHardTriggerKind::ConcurrencyLifecycle,
                    evidence: vec!["CAS and gate ordering".into()],
                },
                ManifestTaskHardTrigger {
                    kind: TaskHardTriggerKind::MigrationDestructivePersistence,
                    evidence: vec!["durable immutable settlement".into()],
                },
            ],
            soft_signals: vec![ManifestTaskSoftSignal {
                kind: TaskSoftSignalKind::SharedInterface,
                score: 1,
                evidence: vec!["store and recovery DTO contract".into()],
            }],
            score: 1,
            reason: "hard triggers freeze this Task at high risk".into(),
        };
        let task_impl = doc
            .nodes
            .iter_mut()
            .find(|node| node.id == "task-1-impl")
            .unwrap();
        task_impl.agent_type = Some("codex".into());
        task_impl.work_unit_key = Some(
            build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                task_index: 1,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap(),
        );
        let grok_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        doc.nodes.push(wu(
            "task-1-rev-grok",
            PHASE_TASKS,
            ManifestNodeRole::Reviewer,
            "grok",
            None,
            Some(1),
            grok_key,
            vec!["task-1-impl".into()],
        ));
        doc.task_policies[0].route = ManifestTaskRoute {
            implementer_node_id: "task-1-impl".into(),
            reviewer_node_ids: vec!["task-1-rev".into(), "task-1-rev-grok".into()],
        };

        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-recovery",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-recovery.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-task-recovery-1",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-recovery",
            "request_changes",
            "reports/reviewer-1.md",
            1,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-2",
            "review-task-recovery-2",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-recovery",
            "request_changes",
            "reports/reviewer-2.md",
            2,
        )
        .await;

        let submission = plan_submission(
            PlanReviewScope::Full,
            PlanRevisionKind::Initial,
            &["plan-reviewer-1", "plan-reviewer-2"],
            vec![
                finding(
                    "F-critical",
                    FindingSeverity::Critical,
                    FindingStatus::Open,
                    &["plan-reviewer-1"],
                ),
                finding(
                    "F-important",
                    FindingSeverity::Important,
                    FindingStatus::New,
                    &["plan-reviewer-2"],
                ),
                finding(
                    "F-minor",
                    FindingSeverity::Minor,
                    FindingStatus::Open,
                    &["plan-reviewer-2"],
                ),
            ],
            "author-task-recovery",
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
        );
        let settled = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(submission.clone()),
            "initial Plan review",
        )
        .await
        .unwrap();
        assert_eq!(
            settled.plan_next_action,
            Some(PlanReviewNextAction::ContinueReview)
        );
        assert_eq!(
            (
                settled.critical_count,
                settled.important_count,
                settled.minor_count
            ),
            (0, 0, 0)
        );
        assert_eq!(settled.stagnation_count, 0);
        assert!(!settled.rewrite_used);

        let row = delegation_workflow_gate_settlement::Entity::find_by_id((
            published.workflow_id.clone(),
            "plan".to_string(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        assert!(row.review_scope.is_none());
        assert!(row.revision_kind.is_none());
        assert_eq!(
            row.covered_author_task_id.as_deref(),
            Some("author-task-recovery")
        );
        assert_eq!(
            row.covered_plan_digest.as_deref(),
            Some("sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7")
        );
        assert!(row.finding_ledger_json.is_none());
        assert!(row.report_files_json.is_none());
        let state = load_persisted_plan_state_v2(&row).unwrap();
        assert_eq!(
            state.required_node_ids,
            vec!["plan-reviewer-1", "plan-reviewer-2"]
        );
        assert_eq!(state.selected_node_ids, state.required_node_ids);
        assert_eq!(state.next_action, PlanReviewNextAction::ContinueReview);
    }

    #[tokio::test]
    async fn task4_caller_author_identity_does_not_override_durable_review_bindings() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: two_reviewer_plan_doc("tok-task4-same-author"),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-shared",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-shared.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-shared-1",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-shared",
            "approve",
            "reports/review-shared-1.md",
            1,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-2",
            "review-shared-2",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "different-author-task",
            "approve",
            "reports/review-shared-2.md",
            2,
        )
        .await;

        let settled = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1", "plan-reviewer-2"],
                vec![],
                "author-task-shared",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "same artifact required",
        )
        .await
        .unwrap();
        assert_eq!(settled.outcome, GateSettlementOutcome::Approved);
    }

    #[tokio::test]
    async fn task4_plan_reducer_requires_infrastructure_successful_reviewer_evidence() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-task4-reviewer-failed"),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-reviewer-failed",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-reviewer-failed.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-task-infrastructure-failed",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-reviewer-failed",
            "approve",
            "reports/reviewer-infrastructure-failed.md",
            1,
        )
        .await;

        let reviewer_run = delegation_task_run::Entity::find_by_id(
            "review-task-infrastructure-failed".to_string(),
        )
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut reviewer_am: delegation_task_run::ActiveModel = reviewer_run.into();
        reviewer_am.status = Set(DelegationRunStatus::Failed);
        reviewer_am.update(&db.conn).await.unwrap();

        let error = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-task-reviewer-failed",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "failed reviewer cannot reduce",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::GateNotReady(_)));

        let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
            published.workflow_id,
            "plan".to_string(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap();
        assert!(
            settlement.is_none(),
            "failed evidence must not persist a round"
        );
    }

    #[tokio::test]
    async fn task4_parent_supplied_lineage_reset_reason_is_not_v2_authority() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-task4-lineage-reset"),
            },
        )
        .await
        .unwrap();
        let mut submission = plan_submission(
            PlanReviewScope::Full,
            PlanRevisionKind::Initial,
            &["plan-reviewer-1"],
            vec![],
            "author-task-lineage-reset",
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
        );
        submission.lineage_reset_reason = Some("parent claims user approval".into());

        let error = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(submission),
            "untrusted lineage reset",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::GateNotReady(_)));
    }

    #[tokio::test]
    async fn task4_plan_gate_rename_invalidates_prior_v2_completion_scope() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-task4-gate-rename");
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-gate-rename",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-gate-rename.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-gate-rename-c1",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-gate-rename",
            "request_changes",
            "reports/review-gate-rename-c1.md",
            1,
        )
        .await;
        settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![finding(
                    "F-gate-rename",
                    FindingSeverity::Important,
                    FindingStatus::Open,
                    &["plan-reviewer-1"],
                )],
                "author-task-gate-rename",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "initial gate lineage",
        )
        .await
        .unwrap();

        doc.workflow_id = Some(published.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.gates
            .iter_mut()
            .find(|gate| gate.gate_kind == Some(DocumentGateKind::Plan))
            .unwrap()
            .id = "renamed-plan".into();
        let updated = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();

        let state = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
            .await
            .expect("renamed gate omits completion from the prior immutable scope");
        let renamed_gate = state
            .gates
            .iter()
            .find(|gate| gate.gate_id == "renamed-plan")
            .unwrap();
        assert_eq!(renamed_gate.latest_outcome, None);
        assert!(state.latest_plan_review.is_none());
        assert!(updated.manifest_revision > published.manifest_revision);
    }

    #[tokio::test]
    async fn workflow_v2_completed_review_scope_survives_later_author_completion() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-task4-latest-author"),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-old",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-old.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-old-author",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-old",
            "approve",
            "reports/review-old-author.md",
            1,
        )
        .await;
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-current",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-current.md",
            2,
        )
        .await;
        let current =
            delegation_workflow_run_binding::Entity::find_by_id("author-task-current".to_string())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
        let mut current_am: delegation_workflow_run_binding::ActiveModel = current.into();
        current_am.lineage_ordinal = Set(2);
        current_am.update(&db.conn).await.unwrap();

        let settled = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-task-old",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "older Author task must not settle",
        )
        .await
        .unwrap();
        assert_eq!(settled.outcome, GateSettlementOutcome::Approved);
    }

    #[tokio::test]
    async fn task4_latest_plan_reviewer_binding_is_required() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-task4-latest-reviewer"),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-latest-reviewer",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-latest-reviewer.md",
            0,
        )
        .await;
        for (task_id, offset) in [("review-old-completed", 1), ("review-current-running", 2)] {
            insert_plan_reviewer_evidence(
                &db,
                parent,
                &published.workflow_id,
                "plan-reviewer-1",
                task_id,
                1,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                "author-task-latest-reviewer",
                "approve",
                &format!("reports/{task_id}.md"),
                offset,
            )
            .await;
        }
        let current_binding = delegation_workflow_run_binding::Entity::find_by_id(
            "review-current-running".to_string(),
        )
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut binding_am: delegation_workflow_run_binding::ActiveModel = current_binding.into();
        binding_am.lineage_ordinal = Set(100);
        binding_am.update(&db.conn).await.unwrap();
        let current_run =
            delegation_task_run::Entity::find_by_id("review-current-running".to_string())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
        let mut run_am: delegation_task_run::ActiveModel = current_run.into();
        run_am.status = Set(DelegationRunStatus::Running);
        run_am.finished_at = Set(None);
        run_am.completion_state = Set(None);
        run_am.completion_outcome = Set(None);
        run_am.completion_evidence_json = Set(None);
        run_am.update(&db.conn).await.unwrap();

        let error = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-task-latest-reviewer",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "older reviewer completion must not settle",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::GateNotReady(_)));
    }

    #[tokio::test]
    async fn task4_historical_unselected_reviewer_evidence_fails_closed() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc_a = design_plan_doc("tok-task4-historical-a");
        doc_a.workflow_state = ManifestWorkflowState::Approved;
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc_a.clone(),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-historical",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-historical.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-historical-a1",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-historical",
            "approve",
            "reports/review-historical-a1.md",
            1,
        )
        .await;
        settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-task-historical",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "approve fingerprint A",
        )
        .await
        .unwrap();

        let mut doc_b = doc_a.clone();
        doc_b.workflow_id = Some(published.workflow_id.clone());
        doc_b.expected_manifest_revision = Some(2);
        doc_b.publication_token = "tok-task4-historical-b".into();
        doc_b.task_policies[0].risk.reason = "fingerprint B risk".into();
        let published_b = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc_b },
        )
        .await
        .unwrap();
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-historical-b",
            2,
            3,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-historical",
            "request_changes",
            "reports/review-historical-b.md",
            2,
        )
        .await;
        settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            3,
            published_b.graph_revision,
            2,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Material,
                &["plan-reviewer-1"],
                vec![finding(
                    "F-historical",
                    FindingSeverity::Important,
                    FindingStatus::Open,
                    &["plan-reviewer-1"],
                )],
                "author-task-historical",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "review fingerprint B",
        )
        .await
        .unwrap();

        doc_a.workflow_id = Some(published.workflow_id.clone());
        doc_a.expected_manifest_revision = Some(3);
        doc_a.publication_token = "tok-task4-historical-a-again".into();
        let published_a_again = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc_a },
        )
        .await
        .unwrap();
        let state = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
            .await
            .expect("unproven historical evidence is omitted from the bounded index");
        assert!(state.latest_plan_review.is_none());
        assert_eq!(state.workflow_state, ManifestWorkflowState::Estimated);
        assert!(published_a_again.manifest_revision > published_b.manifest_revision);
    }

    #[tokio::test]
    async fn task4_stale_approved_fingerprint_allows_material_reapproval() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-task4-material-reapprove");
        doc.workflow_state = ManifestWorkflowState::Approved;
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-material-reapprove",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-material-reapprove.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-material-reapprove-c1",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-material-reapprove",
            "approve",
            "reports/review-material-reapprove-c1.md",
            1,
        )
        .await;
        let approved = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-task-material-reapprove",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "initial approval",
        )
        .await
        .unwrap();

        doc.workflow_id = Some(published.workflow_id.clone());
        doc.expected_manifest_revision = Some(2);
        doc.task_policies[0].risk.reason = "material risk-policy correction".into();
        let updated = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        assert_eq!(updated.workflow_state, ManifestWorkflowState::Estimated);
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-material-reapprove-c2",
            2,
            3,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-material-reapprove",
            "approve",
            "reports/review-material-reapprove-c2.md",
            100,
        )
        .await;
        let reapproved = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            3,
            updated.graph_revision,
            2,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Material,
                &["plan-reviewer-1"],
                vec![],
                "author-task-material-reapprove",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "material reapproval",
        )
        .await
        .unwrap();
        assert_eq!(
            approved.plan_next_action,
            Some(PlanReviewNextAction::Approved)
        );
        assert_eq!(
            reapproved.plan_next_action,
            Some(PlanReviewNextAction::Approved)
        );
    }

    #[tokio::test]
    async fn task4_retired_plan_author_evidence_fails_closed() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-task4-retired-author"),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-retired",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-retired.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-retired-author",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-retired",
            "approve",
            "reports/review-retired-author.md",
            1,
        )
        .await;
        let author = delegation_workflow_node_binding::Entity::find_by_id((
            published.workflow_id.clone(),
            "plan-author".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut author_am: delegation_workflow_node_binding::ActiveModel = author.into();
        author_am.retired_revision = Set(Some(2));
        author_am.retained_observed = Set(true);
        author_am.update(&db.conn).await.unwrap();

        let error = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-task-retired",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "retired Author cannot settle",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::GateNotReady(_)));
    }

    #[tokio::test]
    async fn task4_caller_findings_do_not_narrow_v2_corrective_subset() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let doc = two_reviewer_plan_doc("tok-task4-scoped");
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-scoped",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-scoped.md",
            0,
        )
        .await;
        for (node, task, offset) in [
            ("plan-reviewer-1", "review-scoped-c1-1", 1),
            ("plan-reviewer-2", "review-scoped-c1-2", 2),
        ] {
            insert_plan_reviewer_evidence(
                &db,
                parent,
                &published.workflow_id,
                node,
                task,
                1,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                "author-task-scoped",
                "request_changes",
                &format!("reports/{task}.md"),
                offset,
            )
            .await;
        }
        let first = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1", "plan-reviewer-2"],
                vec![finding(
                    "F-owner",
                    FindingSeverity::Important,
                    FindingStatus::Open,
                    &["plan-reviewer-1"],
                )],
                "author-task-scoped",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "initial owner round",
        )
        .await
        .unwrap();

        assert_eq!(first.important_count, 0);
        let state = delegation_workflow_gate_state::Entity::find_by_id((
            published.workflow_id,
            "plan".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        assert_eq!(state.current_review_round, 2);
        assert_eq!(
            serde_json::from_str::<Vec<String>>(&state.selected_node_ids_json).unwrap(),
            Vec::<String>::new()
        );
    }

    #[tokio::test]
    async fn task4_approved_plan_replay_fails_closed_after_state_revision() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-task4-replay"),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-replay",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-replay.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-task-replay",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-replay",
            "approve",
            "reports/review-replay.md",
            1,
        )
        .await;
        let submission = plan_submission(
            PlanReviewScope::Full,
            PlanRevisionKind::Initial,
            &["plan-reviewer-1"],
            vec![],
            "author-task-replay",
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
        );
        let first = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(submission.clone()),
            "approved replay",
        )
        .await
        .unwrap();
        let replay_error = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(submission.clone()),
            "approved replay",
        )
        .await
        .unwrap_err();
        assert!(!first.idempotent_replay);
        assert!(matches!(
            replay_error,
            WorkflowStoreError::GateCycleConflict(_)
        ));
    }

    #[tokio::test]
    async fn task4_plan_stagnation_rewrite_then_user_decision_blocks() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-task4-stagnation"),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-stagnation",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-stagnation.md",
            0,
        )
        .await;

        let rounds = [
            (PlanReviewScope::Full, PlanRevisionKind::Initial),
            (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
            (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
            (PlanReviewScope::Full, PlanRevisionKind::HolisticRewrite),
            (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
            (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
        ];
        let mut graph_revision = published.graph_revision;
        let mut results = Vec::new();
        for (index, (scope, revision_kind)) in rounds.into_iter().enumerate() {
            let cycle = index as u64 + 1;
            insert_plan_reviewer_evidence(
                &db,
                parent,
                &published.workflow_id,
                "plan-reviewer-1",
                &format!("review-stagnation-{cycle}"),
                cycle as i64,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                "author-task-stagnation",
                "request_changes",
                &format!("reports/review-stagnation-{cycle}.md"),
                cycle as i64 * 100,
            )
            .await;
            let findings = if cycle == 1 {
                vec![finding(
                    "F-stagnant",
                    FindingSeverity::Important,
                    FindingStatus::Open,
                    &["plan-reviewer-1"],
                )]
            } else {
                vec![]
            };
            let outcome = if cycle == 6 {
                GateSettlementOutcome::Blocked
            } else {
                GateSettlementOutcome::ChangesRequested
            };
            let result = settle_for_test(
                &db,
                &emitter,
                parent,
                &published.workflow_id,
                "plan",
                1,
                graph_revision,
                cycle,
                outcome,
                TestGateEvidence::Plan(plan_submission(
                    scope,
                    revision_kind,
                    &["plan-reviewer-1"],
                    findings,
                    "author-task-stagnation",
                    "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                )),
                &format!("stagnation round {cycle}"),
            )
            .await
            .unwrap();
            graph_revision = result.graph_revision;
            results.push(result);
        }
        assert_eq!(
            results[2].plan_next_action,
            Some(PlanReviewNextAction::HolisticRewriteRequired)
        );
        assert_eq!(results[2].stagnation_count, 2);
        assert!(!results[2].rewrite_used);
        assert_eq!(
            results[5].plan_next_action,
            Some(PlanReviewNextAction::UserDecisionRequired)
        );
        assert_eq!(results[5].stagnation_count, 2);
        assert!(results[5].rewrite_used);
        assert_eq!(results[5].outcome, GateSettlementOutcome::Blocked);
        let header = delegation_workflow::Entity::find_by_id(published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(header.workflow_state, WorkflowState::Blocked);
    }

    #[tokio::test]
    async fn task4_caller_findings_cannot_override_durable_plan_approval() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_fixture(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-task4-approve"),
            },
        )
        .await
        .unwrap();
        insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-approve",
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "reports/author-approve.md",
            0,
        )
        .await;
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-approve-open",
            1,
            1,
            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            "author-task-approve",
            "approve",
            "reports/review-approve-open.md",
            1,
        )
        .await;
        let approved = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            published.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![finding(
                    "F-open",
                    FindingSeverity::Critical,
                    FindingStatus::Open,
                    &["plan-reviewer-1"],
                )],
                "author-task-approve",
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            )),
            "cannot approve open finding",
        )
        .await
        .unwrap();
        assert_eq!(approved.outcome, GateSettlementOutcome::Approved);
        assert_eq!(
            approved.plan_next_action,
            Some(PlanReviewNextAction::Approved)
        );
    }

    #[cfg(test)]
    mod binding_lifecycle {
        use super::*;

        async fn binding(
            db: &AppDatabase,
            workflow_id: &str,
            node_id: &str,
        ) -> delegation_workflow_node_binding::Model {
            delegation_workflow_node_binding::Entity::find_by_id((
                workflow_id.to_string(),
                node_id.to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
        }

        async fn header(db: &AppDatabase, workflow_id: &str) -> delegation_workflow::Model {
            delegation_workflow::Entity::find_by_id(workflow_id.to_string())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap()
        }

        async fn task_cohort(
            db: &AppDatabase,
            workflow_id: &str,
        ) -> Vec<delegation_workflow_node_binding::Model> {
            delegation_workflow_node_binding::Entity::find()
                .filter(
                    delegation_workflow_node_binding::Column::WorkflowId
                        .eq(workflow_id.to_string()),
                )
                .filter(delegation_workflow_node_binding::Column::TaskIndex.eq(1))
                .order_by_asc(delegation_workflow_node_binding::Column::NodeId)
                .all(&db.conn)
                .await
                .unwrap()
        }

        fn update_doc(doc: &mut ManifestDocument, workflow_id: &str, revision: u64, digest: &str) {
            doc.workflow_id = Some(workflow_id.into());
            doc.expected_manifest_revision = Some(revision);
            doc.plan.as_mut().unwrap().digest = digest.into();
        }

        fn omit_node(doc: &mut ManifestDocument, node_id: &str) {
            doc.nodes.retain(|node| node.id != node_id);
            for node in &mut doc.nodes {
                node.deps.retain(|dependency| dependency != node_id);
            }
            doc.edges
                .retain(|edge| edge.from != node_id && edge.to != node_id);
        }

        async fn publish(
            db: &AppDatabase,
            emitter: &EventEmitter,
            parent: i32,
            document: ManifestDocument,
        ) -> Result<PublishResult, WorkflowStoreError> {
            publish_workflow_manifest_fixture(
                db,
                emitter,
                parent,
                PublishWorkflowRequest { document },
            )
            .await
        }

        async fn observe(db: &AppDatabase, workflow_id: &str, node_id: &str) {
            let row = binding(db, workflow_id, node_id).await;
            let mut active: delegation_workflow_node_binding::ActiveModel = row.into();
            active.is_observed = Set(true);
            active.update(&db.conn).await.unwrap();
        }

        async fn retire_final_fixer(
            db: &AppDatabase,
            emitter: &EventEmitter,
            parent: i32,
            workflow_id: &str,
            source: &ManifestDocument,
        ) -> ManifestDocument {
            observe(db, workflow_id, "final-fixer").await;
            let mut omitted = source.clone();
            update_doc(&mut omitted, workflow_id, 1, "sha256:retire-final-fixer");
            omitted
                .nodes
                .iter_mut()
                .find(|node| node.id == "final-reviewer")
                .unwrap()
                .node_outcome = Some(ManifestNodeOutcome::Canceled);
            omit_node(&mut omitted, "final-fixer");
            assert_eq!(
                publish(db, emitter, parent, omitted.clone())
                    .await
                    .unwrap()
                    .manifest_revision,
                2
            );
            omitted
        }

        async fn freeze_task_cohort(db: &AppDatabase, workflow_id: &str) {
            for node_id in ["task-1-impl", "task-1-rev"] {
                let row = binding(db, workflow_id, node_id).await;
                let mut active: delegation_workflow_node_binding::ActiveModel = row.into();
                active.cohort_frozen = Set(true);
                if node_id == "task-1-impl" {
                    active.is_observed = Set(true);
                }
                active.update(&db.conn).await.unwrap();
            }
        }

        async fn frozen_rejects_without_writes(
            db: &AppDatabase,
            emitter: &EventEmitter,
            parent: i32,
            workflow_id: &str,
            doc: ManifestDocument,
            before_header: &delegation_workflow::Model,
            before_cohort: &[delegation_workflow_node_binding::Model],
        ) {
            let error = publish(db, emitter, parent, doc).await.unwrap_err();
            assert!(
                matches!(error, WorkflowStoreError::CohortFrozen { .. })
                    || matches!(
                        error,
                        WorkflowStoreError::Validation(WorkflowError::TaskRouteMismatch(_))
                    ),
                "unexpected frozen-route error: {error:?}"
            );
            assert_eq!(header(db, workflow_id).await, *before_header);
            assert_eq!(task_cohort(db, workflow_id).await, before_cohort);
        }

        #[tokio::test]
        async fn frozen_cohort_rejection_preflights_before_header_update() {
            use sea_orm::ConnectionTrait;

            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let source = design_plan_doc("frozen-preflight-order");
            let initial = publish(&db, &emitter, parent, source.clone())
                .await
                .unwrap();
            freeze_task_cohort(&db, &initial.workflow_id).await;

            db.conn
                .execute_unprepared(
                    "CREATE TRIGGER reject_workflow_header_update \
                     BEFORE UPDATE ON delegation_workflows \
                     BEGIN SELECT RAISE(ABORT, 'header update reached'); END",
                )
                .await
                .unwrap();

            let mut invalid = source;
            update_doc(
                &mut invalid,
                &initial.workflow_id,
                1,
                "sha256:frozen-preflight-order-invalid",
            );
            let node = invalid
                .nodes
                .iter_mut()
                .find(|node| node.id == "task-1-impl")
                .unwrap();
            let profile = "preflight-order-profile";
            node.profile_id = Some(profile.into());
            node.work_unit_key = Some(
                build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                    task_index: 1,
                    agent_type: node.agent_type.as_deref().unwrap(),
                    profile_id: Some(profile),
                })
                .unwrap(),
            );

            assert_eq!(
                publish(&db, &emitter, parent, invalid).await.unwrap_err(),
                WorkflowStoreError::CohortFrozen {
                    node_id: "Task 1".into()
                },
                "frozen validation must reject before the header UPDATE trigger fires"
            );
        }

        #[tokio::test]
        async fn retired_omitted_binding_is_a_byte_stable_noop_across_republish() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let source = design_plan_doc("retired-omitted-noop");
            let initial = publish(&db, &emitter, parent, source.clone())
                .await
                .unwrap();
            let mut omitted =
                retire_final_fixer(&db, &emitter, parent, &initial.workflow_id, &source).await;
            let snapshot = binding(&db, &initial.workflow_id, "final-fixer").await;

            update_doc(
                &mut omitted,
                &initial.workflow_id,
                2,
                "sha256:retire-final-fixer",
            );
            publish(&db, &emitter, parent, omitted.clone())
                .await
                .unwrap();
            assert_eq!(
                binding(&db, &initial.workflow_id, "final-fixer").await,
                snapshot
            );

            update_doc(
                &mut omitted,
                &initial.workflow_id,
                3,
                "sha256:structurally-changed-plan",
            );
            omitted.task_policies[0].risk.reason = "structurally changed Task plan".into();
            publish(&db, &emitter, parent, omitted).await.unwrap();
            assert_eq!(
                binding(&db, &initial.workflow_id, "final-fixer").await,
                snapshot
            );
        }

        #[tokio::test]
        async fn retired_present_binding_reactivates_only_exact_identity() {
            async fn retired_fixture(
                token: &str,
            ) -> (
                AppDatabase,
                i32,
                EventEmitter,
                PublishResult,
                ManifestDocument,
                ManifestDocument,
            ) {
                let (db, parent) = seed_parent().await;
                let (emitter, _) = emitter_with_rx();
                let source = design_plan_doc(token);
                let initial = publish(&db, &emitter, parent, source.clone())
                    .await
                    .unwrap();
                let retired_doc =
                    retire_final_fixer(&db, &emitter, parent, &initial.workflow_id, &source).await;
                (db, parent, emitter, initial, source, retired_doc)
            }

            let (db, parent, emitter, initial, source, mut exact_doc) =
                retired_fixture("retired-exact-reactivation").await;
            let retired = binding(&db, &initial.workflow_id, "final-fixer").await;
            exact_doc.nodes.push(
                source
                    .nodes
                    .iter()
                    .find(|node| node.id == "final-fixer")
                    .unwrap()
                    .clone(),
            );
            update_doc(
                &mut exact_doc,
                &initial.workflow_id,
                2,
                "sha256:exact-reactivation",
            );
            publish(&db, &emitter, parent, exact_doc).await.unwrap();
            let reactivated = binding(&db, &initial.workflow_id, "final-fixer").await;
            let mut expected = retired;
            expected.retired_revision = None;
            expected.updated_at = reactivated.updated_at;
            assert_eq!(reactivated, expected, "only retirement state may clear");

            for field in [
                "workflow_id",
                "node_id",
                "work_unit_key",
                "role",
                "agent_type",
                "profile_id",
                "phase_id",
                "task_index",
            ] {
                let token = format!("retired-identity-conflict-{field}");
                let (db, parent, emitter, initial, source, mut candidate) =
                    retired_fixture(&token).await;
                candidate.nodes.push(
                    source
                        .nodes
                        .iter()
                        .find(|node| node.id == "final-fixer")
                        .unwrap()
                        .clone(),
                );
                update_doc(
                    &mut candidate,
                    &initial.workflow_id,
                    2,
                    &format!("sha256:identity-conflict-{field}"),
                );

                if field == "workflow_id" {
                    candidate.workflow_id = Some("different-workflow".into());
                } else if field == "node_id" {
                    candidate
                        .nodes
                        .iter_mut()
                        .find(|node| node.id == "final-fixer")
                        .unwrap()
                        .id = "different-final-fixer".into();
                } else {
                    let row = binding(&db, &initial.workflow_id, "final-fixer").await;
                    let mut active: delegation_workflow_node_binding::ActiveModel = row.into();
                    match field {
                        "work_unit_key" => active.work_unit_key = Set("different-key".into()),
                        "role" => active.role = Set("reviewer".into()),
                        "agent_type" => active.agent_type = Set("codex".into()),
                        "profile_id" => active.profile_id = Set(Some("different-profile".into())),
                        "phase_id" => active.phase_id = Set(PHASE_PLAN.into()),
                        "task_index" => active.task_index = Set(Some(1)),
                        _ => unreachable!(),
                    }
                    active.update(&db.conn).await.unwrap();
                }

                let snapshot = binding(&db, &initial.workflow_id, "final-fixer").await;
                assert_eq!(
                    publish(&db, &emitter, parent, candidate).await.unwrap_err(),
                    WorkflowStoreError::AdmittedNodeIdentityMutation {
                        node_id: snapshot.node_id.clone()
                    },
                    "real publication must return the exact identity conflict for {field}"
                );
                assert_eq!(
                    binding(&db, &initial.workflow_id, "final-fixer").await,
                    snapshot,
                    "failed {field} publication must not change the retired row"
                );
            }
        }

        #[tokio::test]
        async fn active_observed_binding_retires_once_and_preserves_first_revision() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let source = design_plan_doc("active-observed-retirement");
            let initial = publish(&db, &emitter, parent, source.clone())
                .await
                .unwrap();
            let mut omitted =
                retire_final_fixer(&db, &emitter, parent, &initial.workflow_id, &source).await;
            let first = binding(&db, &initial.workflow_id, "final-fixer").await;
            assert_eq!(first.retired_revision, Some(2));
            assert!(first.retained_observed);
            for (revision, digest) in [(2, "sha256:repeat-1"), (3, "sha256:repeat-2")] {
                update_doc(&mut omitted, &initial.workflow_id, revision, digest);
                publish(&db, &emitter, parent, omitted.clone())
                    .await
                    .unwrap();
                let after = binding(&db, &initial.workflow_id, "final-fixer").await;
                assert_eq!(after.retired_revision, Some(2));
                assert!(after.retained_observed);
                assert_eq!(after.created_at, first.created_at);
                assert_eq!(after.updated_at, first.updated_at);
            }
        }

        #[tokio::test]
        async fn blocked_manifest_cannot_remove_or_redefine_frozen_task_cohort() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let source = design_plan_doc("blocked-frozen-cohort");
            let initial = publish(&db, &emitter, parent, source.clone())
                .await
                .unwrap();
            freeze_task_cohort(&db, &initial.workflow_id).await;
            let before_header = header(&db, &initial.workflow_id).await;
            let before_cohort = task_cohort(&db, &initial.workflow_id).await;

            for side in ["task-1-impl", "task-1-rev"] {
                let mut variants = Vec::new();
                let mut omission = source.clone();
                omit_node(&mut omission, side);
                variants.push(omission);

                let mut replacement = source.clone();
                let replacement_id = format!("{side}-replacement");
                rename_node_id(&mut replacement, side, &replacement_id);
                let route = &mut replacement.task_policies[0].route;
                if side == "task-1-impl" {
                    route.implementer_node_id = replacement_id;
                } else {
                    route.reviewer_node_ids = vec![replacement_id];
                }
                variants.push(replacement);

                let mut identity = source.clone();
                let node = identity
                    .nodes
                    .iter_mut()
                    .find(|node| node.id == side)
                    .unwrap();
                let profile = "different-profile";
                node.profile_id = Some(profile.into());
                node.work_unit_key = Some(
                    match node.role.unwrap() {
                        ManifestNodeRole::Implementer => {
                            build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                                task_index: 1,
                                agent_type: node.agent_type.as_deref().unwrap(),
                                profile_id: Some(profile),
                            })
                        }
                        ManifestNodeRole::Reviewer => {
                            build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
                                task_index: 1,
                                agent_type: node.agent_type.as_deref().unwrap(),
                                profile_id: Some(profile),
                            })
                        }
                        _ => unreachable!(),
                    }
                    .unwrap(),
                );
                variants.push(identity);

                let mut route_reassignment = source.clone();
                let route = &mut route_reassignment.task_policies[0].route;
                if side == "task-1-impl" {
                    route.implementer_node_id = "task-1-rev".into();
                } else {
                    route.reviewer_node_ids = vec!["task-1-impl".into()];
                }
                variants.push(route_reassignment);

                for (variant_index, mut doc) in variants.into_iter().enumerate() {
                    update_doc(
                        &mut doc,
                        &initial.workflow_id,
                        1,
                        &format!("sha256:blocked-{side}-{variant_index}"),
                    );
                    doc.workflow_state = ManifestWorkflowState::Blocked;
                    frozen_rejects_without_writes(
                        &db,
                        &emitter,
                        parent,
                        &initial.workflow_id,
                        doc,
                        &before_header,
                        &before_cohort,
                    )
                    .await;
                }
            }
        }

        #[tokio::test]
        async fn canceled_outcome_can_update_without_erasing_frozen_binding() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let source = design_plan_doc("canceled-frozen-binding");
            let initial = publish(&db, &emitter, parent, source.clone())
                .await
                .unwrap();
            freeze_task_cohort(&db, &initial.workflow_id).await;
            let before = task_cohort(&db, &initial.workflow_id).await;
            let mut canceled = source.clone();
            update_doc(&mut canceled, &initial.workflow_id, 1, "sha256:canceled");
            canceled.workflow_state = ManifestWorkflowState::Blocked;
            canceled
                .nodes
                .iter_mut()
                .find(|node| node.id == "task-1-impl")
                .unwrap()
                .node_outcome = Some(ManifestNodeOutcome::Canceled);
            publish(&db, &emitter, parent, canceled.clone())
                .await
                .unwrap();
            let after = task_cohort(&db, &initial.workflow_id).await;
            assert_eq!(after.len(), 2);
            for prior in before {
                let current = after
                    .iter()
                    .find(|row| row.node_id == prior.node_id)
                    .unwrap();
                assert_eq!(current.work_unit_key, prior.work_unit_key);
                assert_eq!(current.role, prior.role);
                assert_eq!(current.agent_type, prior.agent_type);
                assert_eq!(current.profile_id, prior.profile_id);
                assert_eq!(current.phase_id, prior.phase_id);
                assert_eq!(current.task_index, prior.task_index);
                assert_eq!(current.retired_revision, None);
            }
            assert_eq!(
                after
                    .iter()
                    .find(|row| row.node_id == "task-1-impl")
                    .unwrap()
                    .node_outcome,
                Some(NodeOutcome::Canceled)
            );

            let before_header = header(&db, &initial.workflow_id).await;
            let before_cohort = task_cohort(&db, &initial.workflow_id).await;
            update_doc(
                &mut canceled,
                &initial.workflow_id,
                2,
                "sha256:blocked-not-drop-permission",
            );
            omit_node(&mut canceled, "task-1-rev");
            frozen_rejects_without_writes(
                &db,
                &emitter,
                parent,
                &initial.workflow_id,
                canceled,
                &before_header,
                &before_cohort,
            )
            .await;
        }
    }

    #[cfg(test)]
    mod workflow_state_authority {
        use super::*;
        use crate::acp::delegation::workflow::recovery_policy::{
            WorkflowRecoveryDecision, WorkflowRecoveryDisposition,
        };
        use crate::db::test_helpers::fresh_disk_db;
        use sea_orm::ConnectionTrait;

        async fn load_header(db: &AppDatabase, workflow_id: &str) -> delegation_workflow::Model {
            delegation_workflow::Entity::find_by_id(workflow_id.to_string())
                .one(&db.conn)
                .await
                .expect("load workflow header")
                .expect("persisted workflow header")
        }

        async fn load_revisions(
            db: &AppDatabase,
            workflow_id: &str,
        ) -> Vec<delegation_workflow_manifest_revision::Model> {
            delegation_workflow_manifest_revision::Entity::find()
                .filter(
                    delegation_workflow_manifest_revision::Column::WorkflowId
                        .eq(workflow_id.to_string()),
                )
                .order_by_asc(delegation_workflow_manifest_revision::Column::ManifestRevision)
                .all(&db.conn)
                .await
                .expect("load workflow revisions")
        }

        async fn append_blocked_state_only(
            db: &AppDatabase,
            workflow_id: &str,
        ) -> delegation_workflow::Model {
            let header = load_header(db, workflow_id).await;
            let txn = db.conn.begin().await.expect("begin state-only append");
            append_state_only_revision_txn(
                &txn,
                &header,
                StateOnlyRevisionRequest {
                    target_state: ManifestWorkflowState::Blocked,
                    transition_reason_code: WorkflowBlockCause::ExplicitManifestBlock.as_str(),
                    recovery_authorization_id: None,
                    consumer_correlation_id: None,
                    recovery_source_state_fingerprint: None,
                    recovery_risk_class: None,
                },
                Utc::now(),
            )
            .await
            .expect("append blocked state-only revision");
            txn.commit().await.expect("commit state-only append");
            load_header(db, workflow_id).await
        }

        async fn publish_blocked_design_only_bridge(
            db: &AppDatabase,
            emitter: &EventEmitter,
            parent: i32,
            token: &str,
        ) -> (PublishResult, delegation_workflow::Model) {
            let mut document = design_plan_doc(token);
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(db, emitter, parent, document.clone())
                .await
                .expect("publish blocked workflow");
            document.workflow_id = Some(published.workflow_id.clone());
            document.expected_manifest_revision = Some(published.manifest_revision);
            document.nodes[0].title = Some("updated Design-only title".into());
            let republished = publish_document(db, emitter, parent, document)
                .await
                .expect("publish Design-only structural update");
            assert_eq!(republished.manifest_revision, 2);
            let publication_header = load_header(db, &published.workflow_id).await;
            assert_eq!(publication_header.structural_revision, 1);
            let header = append_blocked_state_only(db, &published.workflow_id).await;
            (published, header)
        }

        fn assert_inconsistent_recovery(snapshot: &WorkflowRecoverySnapshot) {
            let decision = decide_workflow_recovery(snapshot);
            assert_eq!(
                decision.disposition,
                WorkflowRecoveryDisposition::InconsistentDurableState
            );
            assert_eq!(decision.proposed_action(), None);
            assert_eq!(decision.target_state(), None);
            assert!(!decision.requires_authorization());
        }

        async fn load_recovery_fingerprint(db: &AppDatabase, workflow_id: &str) -> String {
            load_recovery_decision(db, workflow_id)
                .await
                .source_state_fingerprint
        }

        async fn load_recovery_decision(
            db: &AppDatabase,
            workflow_id: &str,
        ) -> WorkflowRecoveryDecision {
            let header = load_header(db, workflow_id).await;
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .expect("load workflow recovery snapshot");
            decide_workflow_recovery(&snapshot)
        }

        struct LinkedPlanChildConversations {
            author: conversation::Model,
            reviewer: conversation::Model,
        }

        async fn load_linked_plan_child_conversations(
            db: &AppDatabase,
            author_task_id: &str,
            reviewer_task_id: &str,
        ) -> LinkedPlanChildConversations {
            let author_run = delegation_task_run::Entity::find_by_id(author_task_id.to_string())
                .one(&db.conn)
                .await
                .expect("load Author run")
                .expect("persisted Author run");
            let reviewer_run =
                delegation_task_run::Entity::find_by_id(reviewer_task_id.to_string())
                    .one(&db.conn)
                    .await
                    .expect("load reviewer run")
                    .expect("persisted reviewer run");
            assert_ne!(
                author_run.child_conversation_id,
                reviewer_run.child_conversation_id
            );
            let author = conversation::Entity::find_by_id(author_run.child_conversation_id)
                .one(&db.conn)
                .await
                .expect("load Author child conversation")
                .expect("linked Author child conversation");
            let reviewer = conversation::Entity::find_by_id(reviewer_run.child_conversation_id)
                .one(&db.conn)
                .await
                .expect("load reviewer child conversation")
                .expect("linked reviewer child conversation");
            LinkedPlanChildConversations { author, reviewer }
        }

        async fn load_bindings(
            db: &AppDatabase,
            workflow_id: &str,
        ) -> Vec<delegation_workflow_node_binding::Model> {
            delegation_workflow_node_binding::Entity::find()
                .filter(
                    delegation_workflow_node_binding::Column::WorkflowId
                        .eq(workflow_id.to_string()),
                )
                .order_by_asc(delegation_workflow_node_binding::Column::NodeId)
                .all(&db.conn)
                .await
                .expect("load workflow bindings")
        }

        async fn publish_document(
            db: &AppDatabase,
            emitter: &EventEmitter,
            parent: i32,
            document: ManifestDocument,
        ) -> Result<PublishResult, WorkflowStoreError> {
            publish_workflow_manifest_fixture(
                db,
                emitter,
                parent,
                PublishWorkflowRequest { document },
            )
            .await
        }

        async fn seed_ready_plan_round(
            db: &AppDatabase,
            parent: i32,
            workflow_id: &str,
            suffix: &str,
        ) {
            let author_task_id = format!("author-state-authority-{suffix}");
            insert_plan_author_evidence(
                db,
                parent,
                workflow_id,
                &author_task_id,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                &format!("reports/author-state-authority-{suffix}.md"),
                0,
            )
            .await;
            insert_plan_reviewer_evidence(
                db,
                parent,
                workflow_id,
                "plan-reviewer-1",
                &format!("review-state-authority-{suffix}"),
                1,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                &author_task_id,
                "approve",
                &format!("reports/review-state-authority-{suffix}.md"),
                1,
            )
            .await;
        }

        fn approved_plan_submission(suffix: &str) -> SettleGateEvidence {
            SettleGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                &format!("author-state-authority-{suffix}"),
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            ))
        }

        #[tokio::test]
        async fn ordinary_publication_cannot_leave_blocked_or_call_binding_diff_for_state_only_change(
        ) {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut blocked = design_plan_doc("sticky-state-only-publication");
            blocked.workflow_state = ManifestWorkflowState::Blocked;
            let initial = publish_document(&db, &emitter, parent, blocked.clone())
                .await
                .expect("publish blocked workflow");
            let before_header = load_header(&db, &initial.workflow_id).await;
            let before_revisions = load_revisions(&db, &initial.workflow_id).await;
            let before_bindings = load_bindings(&db, &initial.workflow_id).await;

            blocked.workflow_id = Some(initial.workflow_id.clone());
            blocked.expected_manifest_revision = Some(initial.manifest_revision);
            blocked.workflow_state = ManifestWorkflowState::Estimated;
            reset_binding_diff_invocation_count();
            let rejected = publish_document(&db, &emitter, parent, blocked)
                .await
                .expect("sticky state rejection is a typed publish response");

            assert_eq!(
                rejected.disposition,
                WorkflowPublicationDisposition::WorkflowRecoveryRequired
            );
            assert!(!rejected.publication_committed);
            assert_eq!(rejected.manifest_revision, initial.manifest_revision);
            assert_eq!(binding_diff_invocation_count(), 0);
            assert_eq!(load_header(&db, &initial.workflow_id).await, before_header);
            assert_eq!(
                load_revisions(&db, &initial.workflow_id).await,
                before_revisions
            );
            assert_eq!(
                load_bindings(&db, &initial.workflow_id).await,
                before_bindings
            );
        }

        #[tokio::test]
        async fn unauthorized_blocked_replay_is_read_only_for_legacy_header() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut blocked = design_plan_doc("legacy-sticky-state-only-publication");
            blocked.workflow_state = ManifestWorkflowState::Blocked;
            let initial = publish_document(&db, &emitter, parent, blocked.clone())
                .await
                .expect("publish blocked workflow");

            blocked.workflow_id = Some(initial.workflow_id.clone());
            blocked.expected_manifest_revision = Some(initial.manifest_revision);
            blocked.workflow_state = ManifestWorkflowState::Estimated;
            blocked.plan.as_mut().expect("Plan document").digest =
                "sha256:legacy-sticky-structural-update".into();
            let published = publish_document(&db, &emitter, parent, blocked.clone())
                .await
                .expect("commit structural publication with effective blocked state");
            assert!(published.publication_committed);
            assert_eq!(published.workflow_state, ManifestWorkflowState::Blocked);

            let header = load_header(&db, &initial.workflow_id).await;
            let mut legacy: delegation_workflow::ActiveModel = header.into();
            legacy.capability_version = Set("workflow-manifest-v1".into());
            legacy
                .update(&db.conn)
                .await
                .expect("downgrade capability fixture");
            let before_header = load_header(&db, &initial.workflow_id).await;
            let before_revisions = load_revisions(&db, &initial.workflow_id).await;
            let before_bindings = load_bindings(&db, &initial.workflow_id).await;

            db.conn
                .execute_unprepared(
                    "CREATE TRIGGER reject_legacy_unblock_header_update \
                     BEFORE UPDATE ON delegation_workflows \
                     BEGIN SELECT RAISE(ABORT, 'legacy unblock header update reached'); END",
                )
                .await
                .expect("install no-header-write trigger");

            reset_binding_diff_invocation_count();
            let rejected = publish_document(&db, &emitter, parent, blocked)
                .await
                .expect("ordinary unblock replay returns read-only recovery projection");

            assert_eq!(
                rejected.disposition,
                WorkflowPublicationDisposition::WorkflowRecoveryRequired
            );
            assert!(!rejected.publication_committed);
            assert_eq!(rejected.manifest_revision, published.manifest_revision);
            assert_eq!(binding_diff_invocation_count(), 0);
            assert_eq!(load_header(&db, &initial.workflow_id).await, before_header);
            assert_eq!(
                load_revisions(&db, &initial.workflow_id).await,
                before_revisions
            );
            assert_eq!(
                load_bindings(&db, &initial.workflow_id).await,
                before_bindings
            );
        }

        #[tokio::test]
        async fn invalid_persisted_block_cause_fails_closed_without_fabricating_provenance() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut blocked = design_plan_doc("invalid-persisted-block-cause");
            blocked.workflow_state = ManifestWorkflowState::Blocked;
            let initial = publish_document(&db, &emitter, parent, blocked.clone())
                .await
                .expect("publish blocked workflow");

            let header = load_header(&db, &initial.workflow_id).await;
            let mut corrupt: delegation_workflow::ActiveModel = header.into();
            corrupt.block_cause_code = Set(Some("unknown_future_cause".into()));
            corrupt
                .update(&db.conn)
                .await
                .expect("persist invalid non-NULL block cause fixture");

            let error = publish_document(&db, &emitter, parent, blocked)
                .await
                .expect_err("invalid persisted cause must fail closed");
            assert!(
                matches!(
                    error,
                    WorkflowStoreError::Persistence(ref reason)
                        if reason.contains("unknown workflow block cause: unknown_future_cause")
                ),
                "unexpected corrupt durable-state error: {error:?}"
            );
            let persisted = load_header(&db, &initial.workflow_id).await;
            assert_eq!(
                persisted.block_cause_code.as_deref(),
                Some("unknown_future_cause")
            );
            assert_ne!(
                WorkflowBlockCause::from_db(persisted.block_cause_code.as_deref()),
                Ok(WorkflowBlockCause::DurableStateInconsistent)
            );
        }

        #[tokio::test]
        async fn blocked_workflow_can_publish_real_plan_structure_but_effective_state_stays_blocked(
        ) {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut blocked = design_plan_doc("sticky-structural-publication");
            blocked.workflow_state = ManifestWorkflowState::Blocked;
            let initial = publish_document(&db, &emitter, parent, blocked.clone())
                .await
                .expect("publish blocked workflow");
            let source_header = load_header(&db, &initial.workflow_id).await;

            blocked.workflow_id = Some(initial.workflow_id.clone());
            blocked.expected_manifest_revision = Some(initial.manifest_revision);
            blocked.workflow_state = ManifestWorkflowState::Approved;
            blocked.plan.as_mut().expect("Plan document").digest =
                "sha256:material-plan-rewrite".into();
            let published = publish_document(&db, &emitter, parent, blocked.clone())
                .await
                .expect("material structure remains publishable while blocked");

            assert!(published.publication_committed);
            assert_eq!(published.manifest_revision, initial.manifest_revision + 1);
            assert_eq!(published.workflow_state, ManifestWorkflowState::Blocked);
            assert_eq!(
                published.disposition,
                WorkflowPublicationDisposition::WorkflowRecoveryRequired
            );
            let recovery = published.recovery.expect("read-only recovery projection");
            assert_eq!(recovery.workflow_id, initial.workflow_id);
            assert_eq!(recovery.workflow_state, ManifestWorkflowState::Blocked);
            assert_eq!(
                recovery.block_cause,
                Some(WorkflowBlockCause::ExplicitManifestBlock)
            );
            assert_eq!(
                recovery.block_source_manifest_revision,
                source_header
                    .block_source_manifest_revision
                    .map(|value| value as u64)
            );

            let header = load_header(&db, &recovery.workflow_id).await;
            assert_eq!(header.workflow_state, WorkflowState::Blocked);
            assert_eq!(header.block_cause_code, source_header.block_cause_code);
            assert_eq!(
                header.block_source_manifest_revision,
                source_header.block_source_manifest_revision
            );
            let active = load_active_manifest_document_txn(
                &db.conn,
                &recovery.workflow_id,
                header.active_manifest_revision,
            )
            .await
            .expect("load active blocked publication");
            assert_eq!(active.workflow_state, ManifestWorkflowState::Blocked);
            assert_eq!(
                active.plan.expect("Plan document").digest,
                "sha256:material-plan-rewrite"
            );

            reset_binding_diff_invocation_count();
            let replay = publish_document(&db, &emitter, parent, blocked.clone())
                .await
                .expect("exact retry after lost structural response");
            assert!(replay.idempotent_replay);
            assert!(!replay.publication_committed);
            assert_eq!(replay.manifest_revision, published.manifest_revision);
            assert_eq!(replay.graph_revision, published.graph_revision);
            assert_eq!(replay.workflow_state, ManifestWorkflowState::Blocked);
            assert_eq!(
                replay.recovery.as_ref().and_then(|value| value.block_cause),
                Some(WorkflowBlockCause::ExplicitManifestBlock)
            );
            assert_eq!(binding_diff_invocation_count(), 0);

            let active_digest = load_active_manifest_digest_txn(
                &db.conn,
                &recovery.workflow_id,
                header.active_manifest_revision,
            )
            .await
            .expect("load active digest")
            .expect("active digest");
            let reclassified =
                crate::acp::delegation::workflow::with_historical_workflow_fixture_mutations(
                    classify_existing_header(
                        &db.conn,
                        header,
                        parent,
                        &blocked.publication_token,
                        &active_digest,
                    ),
                )
                .await
                .expect("race reclassifies blocked publication");
            let reclassified_recovery = reclassified
                .recovery
                .expect("race result retains recovery provenance");
            assert_eq!(
                reclassified_recovery.block_cause,
                Some(WorkflowBlockCause::ExplicitManifestBlock)
            );
            assert_eq!(
                reclassified_recovery.block_source_manifest_revision,
                source_header
                    .block_source_manifest_revision
                    .map(|value| value as u64)
            );
        }

        #[tokio::test]
        async fn nonblocked_plan_approval_atomically_appends_approved_state_only_revision() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let published = publish_document(
                &db,
                &emitter,
                parent,
                design_plan_doc("plan-approval-state-only"),
            )
            .await
            .expect("publish estimated workflow");
            seed_ready_plan_round(&db, parent, &published.workflow_id, "commit").await;

            let settled = settle_workflow_gate_core(
                &db,
                &emitter,
                parent,
                SettleWorkflowRequest {
                    workflow_id: published.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "plan".into(),
                    expected_graph_revision: published.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::Approved,
                    evidence: approved_plan_submission("commit"),
                    summary: "exact current Plan approved".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect("settle Plan approval");
            assert_eq!(settled.manifest_revision, 2);

            let header = load_header(&db, &published.workflow_id).await;
            assert_eq!(header.active_manifest_revision, 2);
            assert_eq!(header.workflow_state, WorkflowState::Approved);
            let revisions = load_revisions(&db, &published.workflow_id).await;
            assert_eq!(revisions.len(), 2);
            assert_eq!(
                ManifestRevisionKind::from_db(revisions[1].revision_kind.as_deref())
                    .expect("typed revision kind"),
                ManifestRevisionKind::StateOnly
            );
            assert_eq!(revisions[1].manifest_state, "approved");
            assert_eq!(revisions[1].source_manifest_revision, Some(1));
            let approved_doc: ManifestDocument =
                serde_json::from_str(&revisions[1].document_json).expect("approved document");
            assert_eq!(approved_doc.workflow_state, ManifestWorkflowState::Approved);
            assert!(delegation_workflow_gate_settlement::Entity::find_by_id((
                published.workflow_id.clone(),
                "plan".to_string(),
                1,
            ))
            .one(&db.conn)
            .await
            .expect("load Plan settlement")
            .is_some());

            let (rollback_db, rollback_parent) = seed_parent().await;
            let rollback_published = publish_document(
                &rollback_db,
                &emitter,
                rollback_parent,
                design_plan_doc("plan-approval-state-only-rollback"),
            )
            .await
            .expect("publish rollback fixture");
            seed_ready_plan_round(
                &rollback_db,
                rollback_parent,
                &rollback_published.workflow_id,
                "rollback",
            )
            .await;
            set_inject_publish_persistence_failure(true);
            let error = settle_workflow_gate_core(
                &rollback_db,
                &emitter,
                rollback_parent,
                SettleWorkflowRequest {
                    workflow_id: rollback_published.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "plan".into(),
                    expected_graph_revision: rollback_published.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::Approved,
                    evidence: approved_plan_submission("rollback"),
                    summary: "injected rollback".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect_err("injected failure must roll back settlement and state revision");
            set_inject_publish_persistence_failure(false);
            assert!(matches!(error, WorkflowStoreError::Persistence(_)));
            let rollback_header = load_header(&rollback_db, &rollback_published.workflow_id).await;
            assert_eq!(rollback_header.active_manifest_revision, 1);
            assert_eq!(rollback_header.workflow_state, WorkflowState::Estimated);
            assert_eq!(
                load_revisions(&rollback_db, &rollback_published.workflow_id)
                    .await
                    .len(),
                1
            );
            assert!(delegation_workflow_gate_settlement::Entity::find()
                .filter(
                    delegation_workflow_gate_settlement::Column::WorkflowId
                        .eq(rollback_published.workflow_id),
                )
                .all(&rollback_db.conn)
                .await
                .expect("load rolled-back settlements")
                .is_empty());
        }

        #[tokio::test]
        async fn approval_while_blocked_persists_gate_evidence_without_unblocking() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("blocked-plan-approval");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            seed_ready_plan_round(&db, parent, &published.workflow_id, "blocked").await;

            let settled = settle_workflow_gate_core(
                &db,
                &emitter,
                parent,
                SettleWorkflowRequest {
                    workflow_id: published.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "plan".into(),
                    expected_graph_revision: published.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::Approved,
                    evidence: approved_plan_submission("blocked"),
                    summary: "approval evidence while blocked".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect("persist blocked Plan approval evidence");
            assert_eq!(settled.manifest_revision, 1);
            assert_eq!(
                settled.plan_next_action,
                Some(PlanReviewNextAction::Approved)
            );
            let header = load_header(&db, &published.workflow_id).await;
            assert_eq!(header.active_manifest_revision, 1);
            assert_eq!(header.workflow_state, WorkflowState::Blocked);
            let revisions = load_revisions(&db, &published.workflow_id).await;
            assert_eq!(revisions.len(), 1);
            let active: ManifestDocument =
                serde_json::from_str(&revisions[0].document_json).expect("blocked document");
            assert_eq!(active.workflow_state, ManifestWorkflowState::Blocked);
            let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
                published.workflow_id,
                "plan".to_string(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .expect("approved Plan settlement remains durable");
            let plan = load_persisted_plan_state_v2(&settlement).unwrap();
            assert_eq!(plan.next_action, PlanReviewNextAction::Approved);
            assert_eq!(
                settlement.covered_plan_digest.as_deref(),
                Some("sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7")
            );
        }

        #[tokio::test]
        async fn state_only_revision_preserves_structural_revision_and_fingerprints_across_restart()
        {
            let temp = tempfile::tempdir().expect("temporary workflow database");
            let db = fresh_disk_db(temp.path()).await;
            let folder = seed_folder(&db, "/tmp/wf-state-only-restart").await;
            let parent = seed_conversation(&db, folder, AgentType::Codex).await;
            let (emitter, _) = emitter_with_rx();
            let published =
                publish_document(&db, &emitter, parent, design_plan_doc("state-only-restart"))
                    .await
                    .expect("publish restart fixture");
            let source_header = load_header(&db, &published.workflow_id).await;
            let source_doc = load_active_manifest_document_txn(
                &db.conn,
                &published.workflow_id,
                source_header.active_manifest_revision,
            )
            .await
            .expect("load source document");
            let source_bindings = load_bindings(&db, &published.workflow_id).await;

            let txn = db.conn.begin().await.expect("begin state-only transaction");
            let result = append_state_only_revision_txn(
                &txn,
                &source_header,
                StateOnlyRevisionRequest {
                    target_state: ManifestWorkflowState::Approved,
                    transition_reason_code: "plan_gate_approved",
                    recovery_authorization_id: None,
                    consumer_correlation_id: None,
                    recovery_source_state_fingerprint: None,
                    recovery_risk_class: None,
                },
                Utc::now(),
            )
            .await
            .expect("append approved state-only revision");
            txn.commit().await.expect("commit state-only revision");
            assert_eq!(result.manifest_revision, 2);
            assert_eq!(result.source_manifest_revision, 1);

            db.conn.close().await.expect("close source database");
            let reopened = fresh_disk_db(temp.path()).await;
            let header = load_header(&reopened, &published.workflow_id).await;
            let revisions = load_revisions(&reopened, &published.workflow_id).await;
            let active: ManifestDocument = serde_json::from_str(&revisions[1].document_json)
                .expect("reopened active document");
            assert_eq!(
                header.structural_revision,
                source_header.structural_revision
            );
            assert_eq!(header.graph_revision, source_header.graph_revision);
            assert_eq!(header.design_fingerprint, source_header.design_fingerprint);
            assert_eq!(header.plan_fingerprint, source_header.plan_fingerprint);
            assert_eq!(active.plan_target_rel_path, source_doc.plan_target_rel_path);
            assert_eq!(active.plan, source_doc.plan);
            assert_eq!(active.nodes, source_doc.nodes);
            assert_eq!(active.task_policies, source_doc.task_policies);
            assert_eq!(
                load_bindings(&reopened, &published.workflow_id).await,
                source_bindings
            );
            assert_eq!(header.active_manifest_revision, 2);
            assert_eq!(active.workflow_state, ManifestWorkflowState::Approved);
            assert_eq!(revisions[1].source_manifest_revision, Some(1));
            assert_eq!(
                revisions[1].transition_reason_code.as_deref(),
                Some("plan_gate_approved")
            );
            assert_ne!(revisions[1].document_digest, revisions[0].document_digest);
        }

        #[tokio::test]
        async fn blocked_settlement_records_typed_cause_in_a_state_only_revision() {
            let (explicit_db, explicit_parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut explicit_document = design_plan_doc("typed-explicit-manifest-block");
            explicit_document.workflow_state = ManifestWorkflowState::Blocked;
            let explicit = publish_document(
                &explicit_db,
                &emitter,
                explicit_parent,
                explicit_document.clone(),
            )
            .await
            .expect("publish explicit blocked manifest");
            let explicit_header = load_header(&explicit_db, &explicit.workflow_id).await;
            let explicit_revisions = load_revisions(&explicit_db, &explicit.workflow_id).await;
            assert_eq!(explicit_header.block_source_manifest_revision, Some(1));
            assert_eq!(
                WorkflowBlockCause::from_db(explicit_header.block_cause_code.as_deref())
                    .expect("explicit block cause"),
                WorkflowBlockCause::ExplicitManifestBlock
            );
            assert_eq!(explicit_revisions[0].source_manifest_revision, Some(1));
            assert_eq!(
                explicit_revisions[0].transition_reason_code.as_deref(),
                Some(WorkflowBlockCause::ExplicitManifestBlock.as_str())
            );
            assert_eq!(
                ManifestRevisionKind::from_db(explicit_revisions[0].revision_kind.as_deref())
                    .expect("explicit publication kind"),
                ManifestRevisionKind::Publication
            );

            let (plan_db, plan_parent) = seed_parent().await;
            let plan = publish_document(
                &plan_db,
                &emitter,
                plan_parent,
                design_plan_doc("typed-plan-gate-block"),
            )
            .await
            .expect("publish Plan gate fixture");
            let plan_author_task = "author-state-authority-plan-block";
            insert_plan_author_evidence(
                &plan_db,
                plan_parent,
                &plan.workflow_id,
                plan_author_task,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                "reports/author-state-authority-plan-block.md",
                0,
            )
            .await;
            insert_plan_reviewer_evidence(
                &plan_db,
                plan_parent,
                &plan.workflow_id,
                "plan-reviewer-1",
                "review-state-authority-plan-block",
                1,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                plan_author_task,
                "block",
                "reports/review-state-authority-plan-block.md",
                1,
            )
            .await;
            let plan_changes_requested = settle_workflow_gate_core(
                &plan_db,
                &emitter,
                plan_parent,
                SettleWorkflowRequest {
                    workflow_id: plan.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "plan".into(),
                    expected_graph_revision: plan.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::ChangesRequested,
                    evidence: SettleGateEvidence::Plan(plan_submission(
                        PlanReviewScope::Full,
                        PlanRevisionKind::Initial,
                        &["plan-reviewer-1"],
                        vec![finding(
                            "F-plan-gate-blocked",
                            FindingSeverity::Important,
                            FindingStatus::Open,
                            &["plan-reviewer-1"],
                        )],
                        "author-state-authority-plan-block",
                        "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                    )),
                    summary: "Plan gate blocks on current findings".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect("settle non-passing Plan gate");
            assert_eq!(plan_changes_requested.manifest_revision, 1);
            let plan_header = load_header(&plan_db, &plan.workflow_id).await;
            let plan_revisions = load_revisions(&plan_db, &plan.workflow_id).await;
            assert_eq!(plan_header.workflow_state, WorkflowState::Estimated);
            assert_eq!(plan_header.block_source_manifest_revision, None);
            assert_eq!(plan_header.block_cause_code, None);
            assert_eq!(plan_revisions.len(), 1);

            let (design_db, design_parent) = seed_parent().await;
            let design = publish_document(
                &design_db,
                &emitter,
                design_parent,
                zero_reviewer_design_doc("typed-design-gate-block"),
            )
            .await
            .expect("publish Design gate fixture");
            let design_error = settle_workflow_gate_core(
                &design_db,
                &emitter,
                design_parent,
                SettleWorkflowRequest {
                    workflow_id: design.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "design".into(),
                    expected_graph_revision: design.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::Blocked,
                    evidence: design_evidence(0, 1, 0),
                    summary: "Design gate evidence is blocked".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect_err("fixed-v2 Design self-review requires a platform decision");
            assert_eq!(design_error, WorkflowStoreError::CompletionDecisionRequired);
            let design_header = load_header(&design_db, &design.workflow_id).await;
            assert_eq!(design_header.workflow_state, WorkflowState::Estimated);
            assert_eq!(design_header.block_cause_code, None);
            assert_eq!(design_header.block_source_manifest_revision, None);
            assert_eq!(
                load_revisions(&design_db, &design.workflow_id).await.len(),
                1
            );

            let (decision_db, decision_parent) = seed_parent().await;
            let decision = publish_document(
                &decision_db,
                &emitter,
                decision_parent,
                design_plan_doc("typed-plan-user-decision"),
            )
            .await
            .expect("publish user-decision fixture");
            insert_plan_author_evidence(
                &decision_db,
                decision_parent,
                &decision.workflow_id,
                "author-typed-user-decision",
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                "reports/author-typed-user-decision.md",
                0,
            )
            .await;
            let rounds = [
                (PlanReviewScope::Full, PlanRevisionKind::Initial),
                (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
                (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
                (PlanReviewScope::Full, PlanRevisionKind::HolisticRewrite),
                (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
                (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
            ];
            let mut graph_revision = decision.graph_revision;
            for (index, (scope, revision_kind)) in rounds.into_iter().enumerate() {
                let cycle = index as u64 + 1;
                insert_plan_reviewer_evidence(
                    &decision_db,
                    decision_parent,
                    &decision.workflow_id,
                    "plan-reviewer-1",
                    &format!("review-typed-user-decision-{cycle}"),
                    cycle as i64,
                    1,
                    "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                    "author-typed-user-decision",
                    "request_changes",
                    &format!("reports/review-typed-user-decision-{cycle}.md"),
                    cycle as i64,
                )
                .await;
                let findings = if cycle == 1 {
                    vec![finding(
                        "F-typed-user-decision",
                        FindingSeverity::Important,
                        FindingStatus::Open,
                        &["plan-reviewer-1"],
                    )]
                } else {
                    vec![]
                };
                let outcome = if cycle == 6 {
                    GateSettlementOutcome::Blocked
                } else {
                    GateSettlementOutcome::ChangesRequested
                };
                let settled = settle_workflow_gate_core(
                    &decision_db,
                    &emitter,
                    decision_parent,
                    SettleWorkflowRequest {
                        workflow_id: decision.workflow_id.clone(),
                        manifest_revision: 1,
                        gate_id: "plan".into(),
                        expected_graph_revision: graph_revision,
                        gate_cycle: cycle,
                        outcome,
                        evidence: SettleGateEvidence::Plan(plan_submission(
                            scope,
                            revision_kind,
                            &["plan-reviewer-1"],
                            findings,
                            "author-typed-user-decision",
                            "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                        )),
                        summary: format!("typed user-decision round {cycle}"),
                        recovery_authorization_id: None,
                    },
                )
                .await
                .expect("settle user-decision Plan round");
                graph_revision = settled.graph_revision;
            }
            let decision_header = load_header(&decision_db, &decision.workflow_id).await;
            let decision_revisions = load_revisions(&decision_db, &decision.workflow_id).await;
            assert_eq!(decision_header.block_source_manifest_revision, Some(1));
            assert_eq!(
                WorkflowBlockCause::from_db(decision_header.block_cause_code.as_deref())
                    .expect("user-decision block cause"),
                WorkflowBlockCause::PlanUserDecisionRequired
            );
            assert_eq!(decision_revisions[1].source_manifest_revision, Some(1));
            assert_eq!(
                decision_revisions[1].transition_reason_code.as_deref(),
                Some(WorkflowBlockCause::PlanUserDecisionRequired.as_str())
            );

            let legacy_header = load_header(&explicit_db, &explicit.workflow_id).await;
            let mut legacy: delegation_workflow::ActiveModel = legacy_header.into();
            legacy.block_cause_code = Set(None);
            legacy
                .update(&explicit_db.conn)
                .await
                .expect("persist historical NULL cause");
            let legacy_projection =
                publish_document(&explicit_db, &emitter, explicit_parent, explicit_document)
                    .await
                    .expect("load historical NULL through normal projection");
            assert_eq!(
                legacy_projection
                    .recovery
                    .and_then(|projection| projection.block_cause),
                Some(WorkflowBlockCause::LegacyUnknown)
            );

            for (cause, suffix) in [
                (
                    WorkflowBlockCause::UnresolvedTaskCohort,
                    "unresolved-task-cohort",
                ),
                (
                    WorkflowBlockCause::DurableStateInconsistent,
                    "durable-state-inconsistent",
                ),
            ] {
                let (boundary_db, boundary_parent) = seed_parent().await;
                let boundary = publish_document(
                    &boundary_db,
                    &emitter,
                    boundary_parent,
                    design_plan_doc(&format!("typed-boundary-{suffix}")),
                )
                .await
                .expect("publish typed block-entry boundary fixture");
                let source = load_header(&boundary_db, &boundary.workflow_id).await;
                let txn = boundary_db
                    .conn
                    .begin()
                    .await
                    .expect("begin typed block-entry transaction");
                let result = append_workflow_block_revision_txn(
                    &txn,
                    &source,
                    WorkflowBlockEntryRequest {
                        cause,
                        consumer_correlation_id: Some("task-6-policy-boundary"),
                    },
                    Utc::now(),
                )
                .await
                .expect("append through typed block-entry boundary");
                txn.commit()
                    .await
                    .expect("commit typed block-entry transaction");

                assert_eq!(result.block_cause, Some(cause));
                assert_eq!(result.source_manifest_revision, 1);
                let header = load_header(&boundary_db, &boundary.workflow_id).await;
                assert_eq!(
                    WorkflowBlockCause::from_db(header.block_cause_code.as_deref())
                        .expect("typed boundary cause"),
                    cause
                );
                assert_eq!(header.block_source_manifest_revision, Some(1));
                let revisions = load_revisions(&boundary_db, &boundary.workflow_id).await;
                assert_eq!(revisions[1].source_manifest_revision, Some(1));
                assert_eq!(
                    revisions[1].transition_reason_code.as_deref(),
                    Some(cause.as_str())
                );
                assert_eq!(
                    revisions[1].consumer_correlation_id.as_deref(),
                    Some("task-6-policy-boundary")
                );
            }

            assert_eq!(
                ManifestRevisionKind::from_db(None).expect("historical revision kind"),
                ManifestRevisionKind::Publication
            );
            assert!(WorkflowBlockCause::from_db(Some("legacy_unknown")).is_err());
        }

        #[tokio::test]
        async fn task7_recovery_lineage_arbitrary_older_source_is_inconsistent() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-lineage-arbitrary-source");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            append_blocked_state_only(&db, &published.workflow_id).await;
            let header = append_blocked_state_only(&db, &published.workflow_id).await;
            let active = delegation_workflow_manifest_revision::Entity::find_by_id((
                published.workflow_id.clone(),
                header.active_manifest_revision,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut corrupt: delegation_workflow_manifest_revision::ActiveModel = active.into();
            corrupt.source_manifest_revision = Set(Some(1));
            corrupt.update(&db.conn).await.unwrap();

            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert_inconsistent_recovery(&snapshot);
        }

        #[tokio::test]
        async fn task7_recovery_lineage_missing_immediate_source_is_inconsistent() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-lineage-missing-source");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            append_blocked_state_only(&db, &published.workflow_id).await;
            let header = append_blocked_state_only(&db, &published.workflow_id).await;
            delegation_workflow_manifest_revision::Entity::delete_by_id((
                published.workflow_id.clone(),
                header.active_manifest_revision - 1,
            ))
            .exec(&db.conn)
            .await
            .unwrap();

            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert_inconsistent_recovery(&snapshot);
        }

        #[tokio::test]
        async fn task7_recovery_lineage_missing_structural_root_is_inconsistent() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let (published, header) = publish_blocked_design_only_bridge(
                &db,
                &emitter,
                parent,
                "task7-lineage-missing-root",
            )
            .await;
            delegation_workflow_manifest_revision::Entity::delete_by_id((
                published.workflow_id.clone(),
                header.structural_revision,
            ))
            .exec(&db.conn)
            .await
            .unwrap();

            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert_inconsistent_recovery(&snapshot);
        }

        #[tokio::test]
        async fn task7_recovery_lineage_wrong_kind_structural_root_is_inconsistent() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let (published, header) = publish_blocked_design_only_bridge(
                &db,
                &emitter,
                parent,
                "task7-lineage-wrong-root-kind",
            )
            .await;
            let root = delegation_workflow_manifest_revision::Entity::find_by_id((
                published.workflow_id.clone(),
                header.structural_revision,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut corrupt: delegation_workflow_manifest_revision::ActiveModel = root.into();
            corrupt.revision_kind = Set(Some(ManifestRevisionKind::StateOnly.as_str().into()));
            corrupt.source_manifest_revision = Set(None);
            corrupt.update(&db.conn).await.unwrap();

            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert_inconsistent_recovery(&snapshot);
        }

        #[tokio::test]
        async fn task7_recovery_lineage_wrong_structure_root_is_inconsistent() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let (published, header) = publish_blocked_design_only_bridge(
                &db,
                &emitter,
                parent,
                "task7-lineage-wrong-root-structure",
            )
            .await;
            let root = delegation_workflow_manifest_revision::Entity::find_by_id((
                published.workflow_id.clone(),
                header.structural_revision,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut root_document: ManifestDocument =
                serde_json::from_str(&root.document_json).unwrap();
            root_document.plan.as_mut().unwrap().digest = "sha256:different-plan".into();
            validate_manifest_document(&root_document).expect("different root remains valid");
            let document_json = serde_json::to_string(&root_document).unwrap();
            let mut corrupt: delegation_workflow_manifest_revision::ActiveModel = root.into();
            corrupt.document_digest = Set(sha256_hex(document_json.as_bytes()));
            corrupt.document_json = Set(document_json);
            corrupt.update(&db.conn).await.unwrap();

            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert_inconsistent_recovery(&snapshot);
        }

        #[tokio::test]
        async fn task7_recovery_lineage_valid_publication_bridge_is_recoverable() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let (_published, header) = publish_blocked_design_only_bridge(
                &db,
                &emitter,
                parent,
                "task7-lineage-publication-bridge",
            )
            .await;
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert!(!snapshot.contradictory_durable_state, "{snapshot:#?}");
            let decision = decide_workflow_recovery(&snapshot);
            assert!(matches!(
                decision.disposition,
                WorkflowRecoveryDisposition::Recover { .. }
            ));
            assert!(decision.requires_authorization());
        }

        #[tokio::test]
        async fn task7_recovery_lineage_valid_multi_state_only_chain_is_recoverable() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-lineage-valid-chain");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            append_blocked_state_only(&db, &published.workflow_id).await;
            let header = append_blocked_state_only(&db, &published.workflow_id).await;

            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert!(!snapshot.contradictory_durable_state, "{snapshot:#?}");
            let decision = decide_workflow_recovery(&snapshot);
            assert!(matches!(
                decision.disposition,
                WorkflowRecoveryDisposition::Recover { .. }
            ));
            assert!(decision.requires_authorization());
        }

        #[tokio::test]
        async fn task7_recovery_structural_clock_rejects_stale_a_root_after_a_b_a() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-structural-clock-a-b-a");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let first_a = publish_document(&db, &emitter, parent, document.clone())
                .await
                .expect("publish first Plan A");

            document.workflow_id = Some(first_a.workflow_id.clone());
            document.expected_manifest_revision = Some(first_a.manifest_revision);
            document.plan.as_mut().unwrap().digest = "sha256:plan-b".into();
            let plan_b = publish_document(&db, &emitter, parent, document.clone())
                .await
                .expect("publish Plan B");

            document.expected_manifest_revision = Some(plan_b.manifest_revision);
            document.plan.as_mut().unwrap().digest =
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7".into();
            let final_a = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish final Plan A");
            assert_eq!(final_a.manifest_revision, 3);
            let header = load_header(&db, &first_a.workflow_id).await;
            assert_eq!(header.structural_revision, 3);

            let mut corrupt: delegation_workflow::ActiveModel = header.into();
            corrupt.structural_revision = Set(1);
            let corrupt_header = corrupt.update(&db.conn).await.unwrap();
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &corrupt_header, None)
                .await
                .unwrap();
            assert_inconsistent_recovery(&snapshot);
        }

        #[tokio::test]
        async fn task7_recovery_structural_clock_mixed_publications_preserve_latest_plan_change() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-structural-clock-mixed");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let first_a = publish_document(&db, &emitter, parent, document.clone())
                .await
                .expect("publish first Plan A");

            document.workflow_id = Some(first_a.workflow_id.clone());
            document.expected_manifest_revision = Some(1);
            document.nodes[0].title = Some("Design-only A bridge".into());
            let design_a = publish_document(&db, &emitter, parent, document.clone())
                .await
                .expect("publish Design-only A bridge");
            assert_eq!(
                load_header(&db, &first_a.workflow_id)
                    .await
                    .structural_revision,
                1
            );

            document.expected_manifest_revision = Some(design_a.manifest_revision);
            document.plan.as_mut().unwrap().digest = "sha256:plan-b".into();
            let plan_b = publish_document(&db, &emitter, parent, document.clone())
                .await
                .expect("publish Plan B");
            assert_eq!(
                load_header(&db, &first_a.workflow_id)
                    .await
                    .structural_revision,
                3
            );

            document.expected_manifest_revision = Some(plan_b.manifest_revision);
            document.nodes[0].title = Some("Design-only B bridge".into());
            let design_b = publish_document(&db, &emitter, parent, document.clone())
                .await
                .expect("publish Design-only B bridge");
            assert_eq!(
                load_header(&db, &first_a.workflow_id)
                    .await
                    .structural_revision,
                3
            );

            document.expected_manifest_revision = Some(design_b.manifest_revision);
            document.plan.as_mut().unwrap().digest =
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7".into();
            let final_a = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish final Plan A");
            assert_eq!(final_a.manifest_revision, 5);
            assert_eq!(
                load_header(&db, &first_a.workflow_id)
                    .await
                    .structural_revision,
                5
            );

            append_blocked_state_only(&db, &first_a.workflow_id).await;
            let header = append_blocked_state_only(&db, &first_a.workflow_id).await;
            assert_eq!(header.structural_revision, 5);
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert!(!snapshot.contradictory_durable_state, "{snapshot:#?}");
            assert!(matches!(
                decide_workflow_recovery(&snapshot).disposition,
                WorkflowRecoveryDisposition::Recover { .. }
            ));
        }

        #[tokio::test]
        async fn task7_recovery_structural_clock_rejects_malformed_intervening_publication() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-structural-clock-malformed-middle");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let first_a = publish_document(&db, &emitter, parent, document.clone())
                .await
                .expect("publish first Plan A");
            document.workflow_id = Some(first_a.workflow_id.clone());
            document.expected_manifest_revision = Some(1);
            document.plan.as_mut().unwrap().digest = "sha256:plan-b".into();
            let plan_b = publish_document(&db, &emitter, parent, document.clone())
                .await
                .expect("publish Plan B");
            document.expected_manifest_revision = Some(plan_b.manifest_revision);
            document.plan.as_mut().unwrap().digest =
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7".into();
            publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish final Plan A");

            let middle = delegation_workflow_manifest_revision::Entity::find_by_id((
                first_a.workflow_id.clone(),
                2,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut corrupt: delegation_workflow_manifest_revision::ActiveModel = middle.into();
            corrupt.document_digest = Set("invalid-intervening-digest".into());
            corrupt.update(&db.conn).await.unwrap();

            let header = load_header(&db, &first_a.workflow_id).await;
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert_inconsistent_recovery(&snapshot);
        }

        #[tokio::test]
        async fn task7_recovery_lineage_long_chain_uses_one_revision_query() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-long-state-only-lineage");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            let mut header = load_header(&db, &published.workflow_id).await;
            for _ in 0..64 {
                header = append_blocked_state_only(&db, &published.workflow_id).await;
            }

            reset_recovery_revision_query_count();
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert!(!snapshot.contradictory_durable_state, "{snapshot:#?}");
            assert!(matches!(
                decide_workflow_recovery(&snapshot).disposition,
                WorkflowRecoveryDisposition::Recover { .. }
            ));
            assert_eq!(
                recovery_revision_query_count(),
                1,
                "recovery must preload this workflow's revision history once"
            );
        }

        #[tokio::test]
        async fn task7_recovery_fingerprint_excludes_real_nondurable_loader_inputs() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let author_task_id = "author-state-authority-loader-exclusions";
            let reviewer_task_id = "review-state-authority-loader-exclusions";
            let mut document = design_plan_doc("task7-real-loader-exclusions");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            seed_ready_plan_round(&db, parent, &published.workflow_id, "loader-exclusions").await;
            let linked_children =
                load_linked_plan_child_conversations(&db, author_task_id, reviewer_task_id).await;
            assert_ne!(linked_children.author.id, linked_children.reviewer.id);
            for (node_id, task_id) in [
                ("plan-author", author_task_id),
                ("plan-reviewer-1", reviewer_task_id),
            ] {
                let binding = delegation_workflow_node_binding::Entity::find_by_id((
                    published.workflow_id.clone(),
                    node_id.to_string(),
                ))
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
                let mut observed: delegation_workflow_node_binding::ActiveModel =
                    binding.clone().into();
                observed.is_observed = Set(true);
                observed.update(&db.conn).await.unwrap();
                let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap();
                let mut bound_run: delegation_task_run::ActiveModel = run.into();
                bound_run.work_unit_key = Set(Some(binding.work_unit_key));
                bound_run.update(&db.conn).await.unwrap();
            }
            settle_workflow_gate_core(
                &db,
                &emitter,
                parent,
                SettleWorkflowRequest {
                    workflow_id: published.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "plan".into(),
                    expected_graph_revision: published.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::Approved,
                    evidence: approved_plan_submission("loader-exclusions"),
                    summary: "baseline gate/report prose".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect("settle Plan approval");

            let header = load_header(&db, &published.workflow_id).await;
            let baseline_snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .expect("load baseline recovery snapshot");
            assert!(!baseline_snapshot.contradictory_durable_state);
            let baseline_decision = decide_workflow_recovery(&baseline_snapshot);
            assert_eq!(
                baseline_decision.disposition,
                WorkflowRecoveryDisposition::Recover {
                    target_state: ManifestWorkflowState::Approved,
                }
            );
            let baseline = baseline_decision.source_state_fingerprint.clone();
            assert!(baseline.starts_with("workflow_recovery_v1:"));
            assert_eq!(baseline.len(), 85);

            let mut changed: conversation::ActiveModel = linked_children.author.clone().into();
            changed.external_id = Set(Some("raw-author-acp-session-id".into()));
            changed.update(&db.conn).await.unwrap();
            assert_eq!(
                baseline_decision,
                load_recovery_decision(&db, &published.workflow_id).await
            );
            let mut restore: conversation::ActiveModel =
                conversation::Entity::find_by_id(linked_children.author.id)
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
            restore.external_id = Set(linked_children.author.external_id.clone());
            restore.update(&db.conn).await.unwrap();

            let mut changed: conversation::ActiveModel = linked_children.reviewer.clone().into();
            changed.external_id = Set(Some("raw-reviewer-acp-session-id".into()));
            changed.update(&db.conn).await.unwrap();
            assert_eq!(
                baseline_decision,
                load_recovery_decision(&db, &published.workflow_id).await
            );
            let mut restore: conversation::ActiveModel =
                conversation::Entity::find_by_id(linked_children.reviewer.id)
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
            restore.external_id = Set(linked_children.reviewer.external_id.clone());
            restore.update(&db.conn).await.unwrap();

            let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
                published.workflow_id.clone(),
                "plan".to_string(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let original_summary = settlement.summary.clone();
            let mut changed: delegation_workflow_gate_settlement::ActiveModel = settlement.into();
            changed.summary = Set("different Plan/gate report prose".into());
            changed.update(&db.conn).await.unwrap();
            assert_eq!(
                baseline,
                load_recovery_fingerprint(&db, &published.workflow_id).await
            );
            let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
                published.workflow_id.clone(),
                "plan".to_string(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut restore: delegation_workflow_gate_settlement::ActiveModel = settlement.into();
            restore.summary = Set(original_summary);
            restore.update(&db.conn).await.unwrap();

            let author_run = delegation_task_run::Entity::find_by_id(author_task_id.to_string())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut changed: delegation_task_run::ActiveModel = author_run.clone().into();
            changed.task_preview = Set(Some("different delegation prompt/task description".into()));
            changed.update(&db.conn).await.unwrap();
            assert_eq!(
                baseline,
                load_recovery_fingerprint(&db, &published.workflow_id).await
            );
            let mut restore: delegation_task_run::ActiveModel =
                delegation_task_run::Entity::find_by_id(author_task_id.to_string())
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
            restore.task_preview = Set(author_run.task_preview.clone());
            restore.update(&db.conn).await.unwrap();

            let author_run = delegation_task_run::Entity::find_by_id(author_task_id.to_string())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let original_connection_id = author_run.child_connection_id.clone();
            let mut changed: delegation_task_run::ActiveModel = author_run.into();
            changed.child_connection_id = Set(Some("internal-codeg-connection-uuid".into()));
            changed.update(&db.conn).await.unwrap();
            assert_eq!(
                baseline_decision,
                load_recovery_decision(&db, &published.workflow_id).await
            );
            let mut restore: delegation_task_run::ActiveModel =
                delegation_task_run::Entity::find_by_id(author_task_id.to_string())
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap()
                    .into();
            restore.child_connection_id = Set(original_connection_id);
            restore.update(&db.conn).await.unwrap();

            let parent_row = conversation::Entity::find_by_id(parent)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let original_title = parent_row.title.clone();
            let mut changed: conversation::ActiveModel = parent_row.into();
            changed.title = Set(Some("unrelated expanded UI projection".into()));
            changed.update(&db.conn).await.unwrap();
            assert_eq!(
                baseline,
                load_recovery_fingerprint(&db, &published.workflow_id).await
            );
            let mut restore: conversation::ActiveModel = conversation::Entity::find_by_id(parent)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap()
                .into();
            restore.title = Set(original_title);
            restore.update(&db.conn).await.unwrap();

            let header = load_header(&db, &published.workflow_id).await;
            let original_capability = header.capability_version.clone();
            let mut changed: delegation_workflow::ActiveModel = header.into();
            changed.capability_version = Set("workflow_manifest_test_included".into());
            changed.update(&db.conn).await.unwrap();
            assert_ne!(
                baseline,
                load_recovery_fingerprint(&db, &published.workflow_id).await
            );
            let header = load_header(&db, &published.workflow_id).await;
            let mut restore: delegation_workflow::ActiveModel = header.into();
            restore.capability_version = Set(original_capability);
            restore.update(&db.conn).await.unwrap();

            let binding = delegation_workflow_node_binding::Entity::find_by_id((
                published.workflow_id.clone(),
                "plan-author".to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let original_observed = binding.is_observed;
            let mut changed: delegation_workflow_node_binding::ActiveModel = binding.into();
            changed.is_observed = Set(false);
            changed.update(&db.conn).await.unwrap();
            assert_ne!(
                baseline,
                load_recovery_fingerprint(&db, &published.workflow_id).await
            );
            let mut restore: delegation_workflow_node_binding::ActiveModel =
                delegation_workflow_node_binding::Entity::find_by_id((
                    published.workflow_id.clone(),
                    "plan-author".to_string(),
                ))
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap()
                .into();
            restore.is_observed = Set(original_observed);
            restore.update(&db.conn).await.unwrap();

            let reviewer_binding = delegation_workflow_node_binding::Entity::find_by_id((
                published.workflow_id.clone(),
                "plan-reviewer-1".to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut changed: delegation_workflow_node_binding::ActiveModel =
                reviewer_binding.into();
            changed.profile_id = Set(Some("included-reviewer-profile-id".into()));
            changed.update(&db.conn).await.unwrap();
            assert_ne!(
                baseline,
                load_recovery_fingerprint(&db, &published.workflow_id).await
            );
        }

        #[tokio::test]
        async fn task7_recovery_manifest_read_failure_propagates_persistence_error() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-manifest-read-failure");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");

            set_inject_recovery_manifest_read_failure(true);
            let result = get_workflow_state_core(&db, parent, Some(&published.workflow_id)).await;
            set_inject_recovery_manifest_read_failure(false);
            assert!(matches!(result, Err(WorkflowStoreError::Persistence(_))));
        }

        #[tokio::test]
        async fn task7_corrupt_plan_evidence_blocks_approved_recovery() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-corrupt-plan-evidence");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            seed_ready_plan_round(&db, parent, &published.workflow_id, "task7-corrupt").await;
            settle_workflow_gate_core(
                &db,
                &emitter,
                parent,
                SettleWorkflowRequest {
                    workflow_id: published.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "plan".into(),
                    expected_graph_revision: published.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::Approved,
                    evidence: approved_plan_submission("task7-corrupt"),
                    summary: "approved before evidence corruption".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect("settle approval");

            let row = delegation_workflow_gate_settlement::Entity::find_by_id((
                published.workflow_id.clone(),
                "plan".to_string(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut corrupt: delegation_workflow_gate_settlement::ActiveModel = row.into();
            corrupt.plan_round_state_v2_json = Set(Some("{}".into()));
            corrupt.update(&db.conn).await.unwrap();

            let corrupt = delegation_workflow_gate_settlement::Entity::find_by_id((
                published.workflow_id,
                "plan".to_string(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            assert!(load_persisted_plan_state_v2(&corrupt).is_err());
        }

        #[tokio::test]
        async fn task7_exact_current_historical_approval_survives_later_other_plan_round() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-historical-current-approval");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            seed_ready_plan_round(&db, parent, &published.workflow_id, "task7-history").await;
            for (node_id, task_id) in [
                ("plan-author", "author-state-authority-task7-history"),
                ("plan-reviewer-1", "review-state-authority-task7-history"),
            ] {
                let binding = delegation_workflow_node_binding::Entity::find_by_id((
                    published.workflow_id.clone(),
                    node_id.to_string(),
                ))
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
                let mut observed: delegation_workflow_node_binding::ActiveModel =
                    binding.clone().into();
                observed.is_observed = Set(true);
                observed.update(&db.conn).await.unwrap();
                let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap();
                let mut bound_run: delegation_task_run::ActiveModel = run.into();
                bound_run.work_unit_key = Set(Some(binding.work_unit_key));
                bound_run.update(&db.conn).await.unwrap();
            }
            settle_workflow_gate_core(
                &db,
                &emitter,
                parent,
                SettleWorkflowRequest {
                    workflow_id: published.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "plan".into(),
                    expected_graph_revision: published.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::Approved,
                    evidence: approved_plan_submission("task7-history"),
                    summary: "exact current Plan approval".into(),
                    recovery_authorization_id: None,
                },
            )
            .await
            .expect("settle current approval");

            let approved = delegation_workflow_gate_settlement::Entity::find_by_id((
                published.workflow_id.clone(),
                "plan".to_string(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut evidence = load_persisted_plan_state_v2(&approved).unwrap();
            evidence.next_action = PlanReviewNextAction::ContinueReview;
            let evidence_json = serde_json::to_string(&evidence).unwrap();
            let mut later: delegation_workflow_gate_settlement::ActiveModel = approved.into();
            later.gate_cycle = Set(2);
            later.content_fingerprint = Set("different-plan-fingerprint".into());
            later.outcome = Set(GateSettlementOutcome::ChangesRequested);
            later.next_action = Set(Some(DbPlanReviewNextAction::ContinueReview));
            later.plan_round_state_v2_json = Set(Some(evidence_json));
            later.insert(&db.conn).await.unwrap();

            let header = load_header(&db, &published.workflow_id).await;
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert!(
                snapshot.binding_evidence_consistent,
                "historical approval fixture: {snapshot:#?}"
            );
            assert_eq!(snapshot.latest_plan_gate.as_ref().unwrap().gate_cycle, 2);
            assert_eq!(snapshot.current_plan_gate.as_ref().unwrap().gate_cycle, 2);
        }

        #[tokio::test]
        async fn task7_partially_frozen_task_cohort_blocks_recovery() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-partial-frozen-cohort");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            let binding = delegation_workflow_node_binding::Entity::find_by_id((
                published.workflow_id.clone(),
                "task-1-impl".to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut partially_frozen: delegation_workflow_node_binding::ActiveModel =
                binding.into();
            partially_frozen.cohort_frozen = Set(true);
            partially_frozen.update(&db.conn).await.unwrap();

            let header = load_header(&db, &published.workflow_id).await;
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            assert!(snapshot
                .frozen_task_cohorts
                .iter()
                .any(|cohort| cohort.task_index == 1
                    && cohort.unresolved
                    && !cohort.evidence_consistent));
        }

        #[tokio::test]
        async fn task7_corrupt_manifest_returns_fail_closed_recovery_projection() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-corrupt-manifest-projection");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
                published.workflow_id.clone(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut corrupt: delegation_workflow_manifest_revision::ActiveModel = revision.into();
            corrupt.document_json = Set("{".into());
            corrupt.update(&db.conn).await.unwrap();

            let state = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
                .await
                .expect("corrupt blocked manifest still returns recovery projection");
            let recovery = state.recovery.expect("typed recovery projection");
            assert_eq!(recovery.disposition, "blocked");
            assert!(!recovery.authorization_required);
            assert!(recovery
                .blockers
                .contains(&"invalid_active_manifest".to_string()));
        }

        #[tokio::test]
        async fn task7_corrupt_manifest_digest_returns_bounded_recovery_projection() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-corrupt-manifest-digest");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
                published.workflow_id.clone(),
                1,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut corrupt: delegation_workflow_manifest_revision::ActiveModel = revision.into();
            corrupt.document_digest = Set("invalid-digest".into());
            corrupt.update(&db.conn).await.unwrap();

            let state = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
                .await
                .expect("digest-corrupt blocked manifest returns bounded projection");
            assert!(state.payload_truncated);
            assert!(state.evidence_truncated);
            assert_eq!(state.omitted, vec!["invalid_active_manifest"]);
            assert!(state.nodes.is_empty());
            assert!(state.task_policies.is_empty());
            let recovery = state.recovery.expect("typed recovery projection");
            assert_eq!(recovery.disposition, "blocked");
            assert!(!recovery.authorization_required);
            assert!(recovery
                .blockers
                .contains(&"invalid_active_manifest".to_string()));
        }

        #[tokio::test]
        async fn task7_missing_manifest_row_returns_bounded_recovery_projection() {
            let (db, parent) = seed_parent().await;
            let (emitter, _) = emitter_with_rx();
            let mut document = design_plan_doc("task7-missing-manifest-row");
            document.workflow_state = ManifestWorkflowState::Blocked;
            let published = publish_document(&db, &emitter, parent, document)
                .await
                .expect("publish blocked workflow");
            delegation_workflow_manifest_revision::Entity::delete_by_id((
                published.workflow_id.clone(),
                1,
            ))
            .exec(&db.conn)
            .await
            .unwrap();

            let state = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
                .await
                .expect("missing blocked manifest returns bounded projection");
            assert!(state.payload_truncated);
            assert!(state.evidence_truncated);
            assert_eq!(state.omitted, vec!["invalid_active_manifest"]);
            assert!(state.nodes.is_empty());
            assert!(state.task_policies.is_empty());
            let recovery = state.recovery.expect("typed recovery projection");
            assert_eq!(recovery.disposition, "blocked");
            assert!(!recovery.authorization_required);
            assert!(recovery
                .blockers
                .contains(&"invalid_active_manifest".to_string()));
        }
    }

    #[cfg(test)]
    mod authorized_workflow_recovery {
        use super::*;
        use crate::acp::delegation::workflow::events::set_inject_workflow_recovery_event_failure;
        use crate::acp::delegation::workflow::recovery_policy::{
            WorkflowRecoveryDecision, WorkflowRecoveryDisposition,
        };
        use crate::acp::question::{QuestionAnsweredItem, QuestionOutcome};
        use crate::acp::recovery_authorization::{
            PreparedAuthorization, RecoveryAllowedAction, RecoveryAuthorizationService,
            RecoveryChallenge, RecoverySubjectKind, RECOVERY_APPROVE_LABEL,
        };
        use crate::db::entities::recovery_authorization::{self, RecoveryAuthorizationStatus};
        use sea_orm::TransactionTrait;
        use serde_json::json;

        #[derive(Debug, Clone, PartialEq)]
        struct DurableWorkflowState {
            header: delegation_workflow::Model,
            revisions: Vec<delegation_workflow_manifest_revision::Model>,
            bindings: Vec<delegation_workflow_node_binding::Model>,
            settlements: Vec<delegation_workflow_gate_settlement::Model>,
        }

        async fn durable_state(db: &AppDatabase, workflow_id: &str) -> DurableWorkflowState {
            DurableWorkflowState {
                header: load_header(db, workflow_id).await,
                revisions: load_revisions(db, workflow_id).await,
                bindings: delegation_workflow_node_binding::Entity::find()
                    .filter(
                        delegation_workflow_node_binding::Column::WorkflowId
                            .eq(workflow_id.to_string()),
                    )
                    .order_by_asc(delegation_workflow_node_binding::Column::NodeId)
                    .all(&db.conn)
                    .await
                    .expect("load workflow bindings"),
                settlements: delegation_workflow_gate_settlement::Entity::find()
                    .filter(
                        delegation_workflow_gate_settlement::Column::WorkflowId
                            .eq(workflow_id.to_string()),
                    )
                    .order_by_asc(delegation_workflow_gate_settlement::Column::GateCycle)
                    .all(&db.conn)
                    .await
                    .expect("load workflow settlements"),
            }
        }

        async fn load_header(db: &AppDatabase, workflow_id: &str) -> delegation_workflow::Model {
            delegation_workflow::Entity::find_by_id(workflow_id.to_string())
                .one(&db.conn)
                .await
                .expect("load workflow header")
                .expect("persisted workflow header")
        }

        async fn load_revisions(
            db: &AppDatabase,
            workflow_id: &str,
        ) -> Vec<delegation_workflow_manifest_revision::Model> {
            delegation_workflow_manifest_revision::Entity::find()
                .filter(
                    delegation_workflow_manifest_revision::Column::WorkflowId
                        .eq(workflow_id.to_string()),
                )
                .order_by_asc(delegation_workflow_manifest_revision::Column::ManifestRevision)
                .all(&db.conn)
                .await
                .expect("load workflow revisions")
        }

        async fn load_authorization(
            db: &AppDatabase,
            authorization_id: &str,
        ) -> recovery_authorization::Model {
            recovery_authorization::Entity::find_by_id(authorization_id.to_string())
                .one(&db.conn)
                .await
                .expect("load recovery authorization")
                .expect("persisted recovery authorization")
        }

        async fn append_blocked_revision(
            db: &AppDatabase,
            workflow_id: &str,
        ) -> delegation_workflow::Model {
            let header = load_header(db, workflow_id).await;
            let txn = db.conn.begin().await.expect("begin block transaction");
            append_state_only_revision_txn(
                &txn,
                &header,
                StateOnlyRevisionRequest {
                    target_state: ManifestWorkflowState::Blocked,
                    transition_reason_code: WorkflowBlockCause::ExplicitManifestBlock.as_str(),
                    recovery_authorization_id: None,
                    consumer_correlation_id: None,
                    recovery_source_state_fingerprint: None,
                    recovery_risk_class: None,
                },
                Utc::now(),
            )
            .await
            .expect("append blocked state-only revision");
            txn.commit().await.expect("commit blocked revision");
            load_header(db, workflow_id).await
        }

        async fn recovery_decision(
            db: &AppDatabase,
            workflow_id: &str,
            displayed_reason: Option<&str>,
        ) -> WorkflowRecoveryDecision {
            let header = load_header(db, workflow_id).await;
            let txn = db.conn.begin().await.expect("begin recovery read");
            let snapshot = load_workflow_recovery_snapshot_txn(&txn, &header, displayed_reason)
                .await
                .expect("load recovery snapshot");
            txn.rollback().await.expect("rollback recovery read");
            decide_workflow_recovery(&snapshot)
        }

        async fn authorize_decision(
            db: &AppDatabase,
            parent: i32,
            workflow_id: &str,
            displayed_reason: Option<&str>,
        ) -> (String, WorkflowRecoveryDecision) {
            let decision = recovery_decision(db, workflow_id, displayed_reason).await;
            let allowed_action = match decision.proposed_action() {
                Some("recover_workflow") => RecoveryAllowedAction::RecoverWorkflow,
                Some("reset_plan_lineage") => RecoveryAllowedAction::ResetPlanLineage,
                other => {
                    panic!("expected authorized workflow decision, got {other:?}: {decision:#?}")
                }
            };
            let service = RecoveryAuthorizationService::new(db.conn.clone());
            let prepared = service
                .prepare(RecoveryChallenge {
                    parent_conversation_id: parent,
                    subject_kind: RecoverySubjectKind::Workflow,
                    subject_id: workflow_id.to_string(),
                    delegation_identity: None,
                    source_state_fingerprint: decision.source_state_fingerprint.clone(),
                    allowed_action,
                    action_payload: decision.action_payload().expect("decision action payload"),
                    cause_code: decision.cause_code.as_str().to_string(),
                    risk_class: decision.risk_class.as_str().to_string(),
                    display_reason: displayed_reason.map(str::to_string),
                })
                .await
                .expect("prepare workflow authorization");
            let authorization_id = match prepared {
                PreparedAuthorization::Pending { row, .. } => row.authorization_id,
                other => panic!("expected pending authorization, got {other:?}"),
            };
            service
                .resolve_question(
                    &authorization_id,
                    QuestionOutcome {
                        answers: vec![QuestionAnsweredItem {
                            question: "recovery_authorization".into(),
                            header: "Recovery".into(),
                            multi_select: false,
                            selected: vec![RECOVERY_APPROVE_LABEL.into()],
                        }],
                        declined: false,
                    },
                )
                .await
                .expect("approve workflow authorization");
            (authorization_id, decision)
        }

        async fn authorize_generic_recovery_for_reset_fixture(
            db: &AppDatabase,
            parent: i32,
            workflow_id: &str,
        ) -> String {
            let decision = recovery_decision(db, workflow_id, None).await;
            let service = RecoveryAuthorizationService::new(db.conn.clone());
            let prepared = service
                .prepare(RecoveryChallenge {
                    parent_conversation_id: parent,
                    subject_kind: RecoverySubjectKind::Workflow,
                    subject_id: workflow_id.to_string(),
                    delegation_identity: None,
                    source_state_fingerprint: decision.source_state_fingerprint,
                    allowed_action: RecoveryAllowedAction::RecoverWorkflow,
                    action_payload: json!({ "target_state": "estimated" }),
                    cause_code: "plan_user_decision_required".into(),
                    risk_class: "normal".into(),
                    display_reason: None,
                })
                .await
                .expect("prepare generic workflow authorization");
            let authorization_id = match prepared {
                PreparedAuthorization::Pending { row, .. } => row.authorization_id,
                other => panic!("expected pending authorization, got {other:?}"),
            };
            service
                .resolve_question(
                    &authorization_id,
                    QuestionOutcome {
                        answers: vec![QuestionAnsweredItem {
                            question: "recovery_authorization".into(),
                            header: "Recovery".into(),
                            multi_select: false,
                            selected: vec![RECOVERY_APPROVE_LABEL.into()],
                        }],
                        declined: false,
                    },
                )
                .await
                .expect("approve generic workflow authorization");
            authorization_id
        }

        async fn blocked_recovery_fixture(
            target: ManifestWorkflowState,
            token: &str,
        ) -> (
            AppDatabase,
            i32,
            String,
            EventEmitter,
            tokio::sync::broadcast::Receiver<crate::web::event_bridge::WebEvent>,
        ) {
            let (db, parent) = seed_parent().await;
            let (emitter, mut rx) = emitter_with_rx();
            let mut document = if target == ManifestWorkflowState::Skeleton {
                skeleton_doc(token)
            } else {
                design_plan_doc(token)
            };
            document.workflow_state = if target == ManifestWorkflowState::Approved {
                ManifestWorkflowState::Estimated
            } else {
                target
            };
            let published = publish_workflow_manifest_fixture(
                &db,
                &emitter,
                parent,
                PublishWorkflowRequest { document },
            )
            .await
            .expect("publish workflow fixture");

            if target == ManifestWorkflowState::Approved {
                insert_plan_author_evidence(
                    &db,
                    parent,
                    &published.workflow_id,
                    &format!("author-{token}"),
                    1,
                    "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                    &format!("reports/author-{token}.md"),
                    0,
                )
                .await;
                insert_plan_reviewer_evidence(
                    &db,
                    parent,
                    &published.workflow_id,
                    "plan-reviewer-1",
                    &format!("review-{token}"),
                    1,
                    1,
                    "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                    &format!("author-{token}"),
                    "approve",
                    &format!("reports/review-{token}.md"),
                    1,
                )
                .await;
                settle_for_test(
                    &db,
                    &emitter,
                    parent,
                    &published.workflow_id,
                    "plan",
                    1,
                    published.graph_revision,
                    1,
                    GateSettlementOutcome::Approved,
                    SettleGateEvidence::Plan(plan_submission(
                        PlanReviewScope::Full,
                        PlanRevisionKind::Initial,
                        &["plan-reviewer-1"],
                        vec![],
                        &format!("author-{token}"),
                        "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                    )),
                    "approve fixture Plan",
                )
                .await
                .expect("approve fixture Plan");
                align_run_work_unit_keys(&db, &published.workflow_id).await;
            }
            append_blocked_revision(&db, &published.workflow_id).await;
            while rx.try_recv().is_ok() {}
            (db, parent, published.workflow_id, emitter, rx)
        }

        async fn plan_lineage_reset_fixture(
            token: &str,
        ) -> (
            AppDatabase,
            i32,
            String,
            EventEmitter,
            tokio::sync::broadcast::Receiver<crate::web::event_bridge::WebEvent>,
        ) {
            let (db, parent) = seed_parent().await;
            let (emitter, mut rx) = emitter_with_rx();
            let published = publish_workflow_manifest_fixture(
                &db,
                &emitter,
                parent,
                PublishWorkflowRequest {
                    document: design_plan_doc(token),
                },
            )
            .await
            .expect("publish lineage fixture");
            let author_task_id = format!("author-{token}");
            insert_plan_author_evidence(
                &db,
                parent,
                &published.workflow_id,
                &author_task_id,
                1,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                &format!("reports/author-{token}.md"),
                0,
            )
            .await;
            let rounds = [
                (PlanReviewScope::Full, PlanRevisionKind::Initial),
                (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
                (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
                (PlanReviewScope::Full, PlanRevisionKind::HolisticRewrite),
                (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
                (PlanReviewScope::Scoped, PlanRevisionKind::Localized),
            ];
            let mut graph_revision = published.graph_revision;
            for (index, (scope, revision_kind)) in rounds.into_iter().enumerate() {
                let cycle = index as u64 + 1;
                insert_plan_reviewer_evidence(
                    &db,
                    parent,
                    &published.workflow_id,
                    "plan-reviewer-1",
                    &format!("review-{token}-{cycle}"),
                    cycle as i64,
                    1,
                    "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                    &author_task_id,
                    "request_changes",
                    &format!("reports/review-{token}-{cycle}.md"),
                    cycle as i64 * 100,
                )
                .await;
                let findings = if cycle == 1 {
                    vec![finding(
                        "F-stagnant",
                        FindingSeverity::Important,
                        FindingStatus::Open,
                        &["plan-reviewer-1"],
                    )]
                } else {
                    vec![]
                };
                let settled = settle_for_test(
                    &db,
                    &emitter,
                    parent,
                    &published.workflow_id,
                    "plan",
                    1,
                    graph_revision,
                    cycle,
                    if cycle == 6 {
                        GateSettlementOutcome::Blocked
                    } else {
                        GateSettlementOutcome::ChangesRequested
                    },
                    SettleGateEvidence::Plan(plan_submission(
                        scope,
                        revision_kind,
                        &["plan-reviewer-1"],
                        findings,
                        &author_task_id,
                        "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                    )),
                    &format!("lineage fixture round {cycle}"),
                )
                .await
                .expect("settle lineage fixture round");
                graph_revision = settled.graph_revision;
            }
            let header = load_header(&db, &published.workflow_id).await;
            assert_eq!(header.workflow_state, WorkflowState::Blocked);
            assert_eq!(header.active_manifest_revision, 2);
            align_run_work_unit_keys(&db, &published.workflow_id).await;
            while rx.try_recv().is_ok() {}
            (db, parent, published.workflow_id, emitter, rx)
        }

        async fn insert_reset_reviewer_evidence(
            db: &AppDatabase,
            _parent: i32,
            workflow_id: &str,
            _token: &str,
        ) {
            align_run_work_unit_keys(db, workflow_id).await;
        }

        async fn align_run_work_unit_keys(db: &AppDatabase, workflow_id: &str) {
            let run_bindings = delegation_workflow_run_binding::Entity::find()
                .filter(
                    delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.to_string()),
                )
                .all(&db.conn)
                .await
                .expect("load run bindings for identity alignment");
            for run_binding in run_bindings {
                let node = delegation_workflow_node_binding::Entity::find_by_id((
                    workflow_id.to_string(),
                    run_binding.node_id.clone(),
                ))
                .one(&db.conn)
                .await
                .expect("load node binding for identity alignment")
                .expect("bound workflow node");
                let run = delegation_task_run::Entity::find_by_id(run_binding.task_id.clone())
                    .one(&db.conn)
                    .await
                    .expect("load run for identity alignment")
                    .expect("bound delegation run");
                let mut active: delegation_task_run::ActiveModel = run.into();
                active.work_unit_key = Set(Some(node.work_unit_key.clone()));
                active.update(&db.conn).await.expect("align run identity");

                let mut active_node: delegation_workflow_node_binding::ActiveModel = node.into();
                active_node.is_observed = Set(true);
                active_node
                    .update(&db.conn)
                    .await
                    .expect("mark bound node observed");
            }
        }

        fn reset_submission(
            token: &str,
            reason: &str,
            findings: Vec<PlanFindingUpdate>,
        ) -> PlanReviewRoundSubmission {
            let mut submission = plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                findings,
                &format!("author-{token}"),
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
            );
            submission.lineage_reset_reason = Some(reason.to_string());
            submission
        }

        #[allow(clippy::too_many_arguments)] // Test fixture maps the complete recovery request explicitly.
        async fn settle_reset(
            db: &AppDatabase,
            emitter: &EventEmitter,
            parent: i32,
            workflow_id: &str,
            token: &str,
            reason: &str,
            authorization_id: Option<String>,
            outcome: GateSettlementOutcome,
            findings: Vec<PlanFindingUpdate>,
        ) -> Result<SettleResult, WorkflowStoreError> {
            let header = load_header(db, workflow_id).await;
            settle_workflow_gate_core(
                db,
                emitter,
                parent,
                SettleWorkflowRequest {
                    workflow_id: workflow_id.to_string(),
                    manifest_revision: header.active_manifest_revision as u64,
                    gate_id: "plan".into(),
                    expected_graph_revision: header.graph_revision as u64,
                    gate_cycle: 6,
                    outcome,
                    evidence: SettleGateEvidence::Plan(reset_submission(token, reason, findings)),
                    summary: "authorized requirements lineage reset".into(),
                    recovery_authorization_id: authorization_id,
                },
            )
            .await
        }

        #[tokio::test]
        async fn recover_workflow_derives_target_and_consumes_receipt_with_state_only_revision() {
            for (target, token) in [
                (ManifestWorkflowState::Approved, "recover-approved"),
                (ManifestWorkflowState::Estimated, "recover-estimated"),
                (ManifestWorkflowState::Skeleton, "recover-skeleton"),
            ] {
                let (db, parent, workflow_id, emitter, mut rx) =
                    blocked_recovery_fixture(target, token).await;
                let before = durable_state(&db, &workflow_id).await;
                let (authorization_id, decision) =
                    authorize_decision(&db, parent, &workflow_id, None).await;
                assert_eq!(
                    decision.disposition,
                    WorkflowRecoveryDisposition::Recover {
                        target_state: target
                    }
                );
                let request = RecoverWorkflowRequest {
                    workflow_id: workflow_id.clone(),
                    recovery_authorization_id: authorization_id.clone(),
                    expected_manifest_revision: before.header.active_manifest_revision as u64,
                    correlation_id: format!("recover-{token}"),
                };

                let recovered = recover_workflow_core(&db, &emitter, parent, request)
                    .await
                    .expect("recover blocked workflow");
                let after = durable_state(&db, &workflow_id).await;
                let receipt = load_authorization(&db, &authorization_id).await;

                assert_eq!(recovered.old_state, ManifestWorkflowState::Blocked);
                assert_eq!(recovered.new_state, target);
                assert_eq!(
                    recovered.source_manifest_revision,
                    before.revisions.len() as u64
                );
                assert_eq!(
                    recovered.manifest_revision,
                    before.revisions.len() as u64 + 1
                );
                assert!(!recovered.idempotent_replay);
                assert_eq!(after.revisions.len(), before.revisions.len() + 1);
                assert_eq!(
                    after.header.active_manifest_revision as u64,
                    recovered.manifest_revision
                );
                assert_eq!(after.header.workflow_state, manifest_state_to_db(target));
                assert_eq!(
                    after.header.structural_revision,
                    before.header.structural_revision
                );
                assert_eq!(
                    after.header.design_fingerprint,
                    before.header.design_fingerprint
                );
                assert_eq!(
                    after.header.plan_fingerprint,
                    before.header.plan_fingerprint
                );
                assert_eq!(after.header.block_cause_code, None);
                assert_eq!(after.header.block_source_manifest_revision, None);
                assert_eq!(after.bindings, before.bindings);
                assert_eq!(after.settlements, before.settlements);
                let revision = after.revisions.last().expect("recovery revision");
                assert_eq!(revision.revision_kind.as_deref(), Some("state_only"));
                assert_eq!(
                    revision.source_manifest_revision,
                    Some(before.header.active_manifest_revision)
                );
                assert_eq!(
                    revision.recovery_authorization_id.as_deref(),
                    Some(authorization_id.as_str())
                );
                assert_eq!(
                    revision.consumer_correlation_id.as_deref(),
                    Some(format!("recover-{token}").as_str())
                );
                assert_eq!(receipt.status, RecoveryAuthorizationStatus::Consumed);
                assert_eq!(
                    receipt.consumed_by_kind.as_deref(),
                    Some("workflow_manifest_revision")
                );
                assert_eq!(
                    receipt.consumed_by_id.as_deref(),
                    Some(recovered.manifest_revision.to_string().as_str())
                );
                assert_eq!(
                    receipt.consumer_correlation_id.as_deref(),
                    Some(format!("recover-{token}").as_str())
                );
                let channels = std::iter::from_fn(|| rx.try_recv().ok())
                    .map(|event| event.channel)
                    .collect::<Vec<_>>();
                assert_eq!(
                    channels,
                    vec![
                        "workflow.recovery_decision",
                        "workflow.recovery_confirmation_requested",
                        "workflow.recovery_authorization_consumed",
                        "workflow.state_only_revision_created",
                        "workflow.binding_reactivated",
                        super::super::super::events::WORKFLOW_GRAPH_CHANGED_EVENT,
                    ]
                );
            }
        }

        #[tokio::test]
        async fn recovery_rejects_active_run_changed_revision_stale_gate_and_frozen_contradiction_without_consuming(
        ) {
            let (db, parent, workflow_id, emitter, mut rx) =
                blocked_recovery_fixture(ManifestWorkflowState::Estimated, "race-active").await;
            let before_authorization = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            insert_terminal_reviewer_run(
                &db,
                parent,
                &workflow_id,
                "plan-author",
                "plan",
                1,
                "race-active-run",
                true,
                2000,
                "sha256:d4ca4b7928291f3ad8cc2dcb8845ee50f6ad12a3ea138769740fe28d247bcbd7",
                DelegationRunStatus::Running,
                before_authorization.active_manifest_revision,
            )
            .await;
            let before = durable_state(&db, &workflow_id).await;
            let error = recover_workflow_core(
                &db,
                &emitter,
                parent,
                RecoverWorkflowRequest {
                    workflow_id: workflow_id.clone(),
                    recovery_authorization_id: authorization_id.clone(),
                    expected_manifest_revision: before_authorization.active_manifest_revision
                        as u64,
                    correlation_id: "race-active".into(),
                },
            )
            .await
            .expect_err("active run must stop recovery");
            assert_eq!(error, WorkflowStoreError::WorkflowRecoveryNotAvailable);
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            assert!(rx.try_recv().is_err());

            let (db, parent, workflow_id, emitter, mut rx) =
                blocked_recovery_fixture(ManifestWorkflowState::Estimated, "race-revision").await;
            let approved_header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            append_blocked_revision(&db, &workflow_id).await;
            let before = durable_state(&db, &workflow_id).await;
            let error = recover_workflow_core(
                &db,
                &emitter,
                parent,
                RecoverWorkflowRequest {
                    workflow_id: workflow_id.clone(),
                    recovery_authorization_id: authorization_id.clone(),
                    expected_manifest_revision: approved_header.active_manifest_revision as u64,
                    correlation_id: "race-revision".into(),
                },
            )
            .await
            .expect_err("changed revision must fail stale gate");
            assert_eq!(
                error,
                WorkflowStoreError::StaleManifestRevision {
                    expected: approved_header.active_manifest_revision as u64,
                    current: before.header.active_manifest_revision as u64,
                }
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            assert!(rx.try_recv().is_err());

            let (db, parent, workflow_id, emitter, mut rx) =
                blocked_recovery_fixture(ManifestWorkflowState::Estimated, "race-orphan").await;
            let header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let active = delegation_workflow_manifest_revision::Entity::find_by_id((
                workflow_id.clone(),
                header.active_manifest_revision,
            ))
            .one(&db.conn)
            .await
            .expect("load active revision before orphan")
            .expect("active revision before orphan");
            delegation_workflow_manifest_revision::ActiveModel {
                workflow_id: Set(workflow_id.clone()),
                manifest_revision: Set(header.active_manifest_revision + 1),
                manifest_state: Set(active.manifest_state),
                document_json: Set(active.document_json),
                document_digest: Set(active.document_digest),
                revision_kind: Set(Some(ManifestRevisionKind::StateOnly.as_str().into())),
                source_manifest_revision: Set(Some(header.active_manifest_revision)),
                recovery_authorization_id: Set(None),
                transition_reason_code: Set(Some("plan_gate_blocked".into())),
                consumer_correlation_id: Set(None),
                graph_revision: Set(Some(header.graph_revision)),
                recovery_source_state_fingerprint: Set(None),
                recovery_risk_class: Set(None),
                created_at: Set(Utc::now()),
            }
            .insert(&db.conn)
            .await
            .expect("insert orphan revision without advancing header");
            let before = durable_state(&db, &workflow_id).await;
            let error = recover_workflow_core(
                &db,
                &emitter,
                parent,
                RecoverWorkflowRequest {
                    workflow_id: workflow_id.clone(),
                    recovery_authorization_id: authorization_id.clone(),
                    expected_manifest_revision: header.active_manifest_revision as u64,
                    correlation_id: "race-orphan".into(),
                },
            )
            .await
            .expect_err("future orphan revision must fail closed before consumption");
            assert_eq!(error, WorkflowStoreError::WorkflowRecoveryNotAvailable);
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            assert!(rx.try_recv().is_err());

            let (db, parent, workflow_id, emitter, mut rx) =
                blocked_recovery_fixture(ManifestWorkflowState::Approved, "race-stale-gate").await;
            let header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let settlement = delegation_workflow_gate_settlement::Entity::find()
                .filter(
                    delegation_workflow_gate_settlement::Column::WorkflowId.eq(workflow_id.clone()),
                )
                .one(&db.conn)
                .await
                .expect("load Plan settlement")
                .expect("approved Plan settlement");
            let mut stale: delegation_workflow_gate_settlement::ActiveModel = settlement.into();
            stale.plan_round_state_v2_json = Set(Some("not-json".into()));
            stale
                .update(&db.conn)
                .await
                .expect("make Plan gate evidence stale");
            let before = durable_state(&db, &workflow_id).await;
            let error = recover_workflow_core(
                &db,
                &emitter,
                parent,
                RecoverWorkflowRequest {
                    workflow_id: workflow_id.clone(),
                    recovery_authorization_id: authorization_id.clone(),
                    expected_manifest_revision: header.active_manifest_revision as u64,
                    correlation_id: "race-stale-gate".into(),
                },
            )
            .await
            .expect_err("stale Plan gate evidence must stop recovery");
            assert_eq!(error, WorkflowStoreError::RecoveryAuthorizationStale);
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            let rejection = rx.try_recv().expect("stale authorization rejection event");
            assert_eq!(rejection.channel, "workflow.recovery_rejected");
            assert_eq!(rejection.payload["workflow_id"], workflow_id);
            assert_eq!(
                rejection.payload["recovery_authorization_id"],
                authorization_id
            );
            assert_eq!(
                rejection.payload["rejection_code"],
                "recovery_authorization_stale"
            );
            assert!(rx.try_recv().is_err());

            let (db, parent, workflow_id, emitter, mut rx) =
                blocked_recovery_fixture(ManifestWorkflowState::Estimated, "race-frozen").await;
            let header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let binding = delegation_workflow_node_binding::Entity::find_by_id((
                workflow_id.clone(),
                "task-1-impl".to_string(),
            ))
            .one(&db.conn)
            .await
            .expect("load frozen binding")
            .expect("task binding");
            let mut active: delegation_workflow_node_binding::ActiveModel = binding.into();
            active.cohort_frozen = Set(true);
            active
                .update(&db.conn)
                .await
                .expect("freeze incomplete cohort");
            let before = durable_state(&db, &workflow_id).await;
            let error = recover_workflow_core(
                &db,
                &emitter,
                parent,
                RecoverWorkflowRequest {
                    workflow_id: workflow_id.clone(),
                    recovery_authorization_id: authorization_id.clone(),
                    expected_manifest_revision: header.active_manifest_revision as u64,
                    correlation_id: "race-frozen".into(),
                },
            )
            .await
            .expect_err("frozen contradiction must stop recovery");
            assert_eq!(error, WorkflowStoreError::WorkflowRecoveryNotAvailable);
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            assert!(rx.try_recv().is_err());
        }

        #[tokio::test]
        async fn exact_replay_returns_original_revision_and_different_correlation_conflicts() {
            let (db, parent, workflow_id, _, _) =
                blocked_recovery_fixture(ManifestWorkflowState::Estimated, "exact-replay").await;
            let header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let broadcaster = Arc::new(WebEventBroadcaster::new());
            let mut rx = broadcaster.subscribe();
            let emitter = EventEmitter::test_web_only(broadcaster);
            let request = RecoverWorkflowRequest {
                workflow_id: workflow_id.clone(),
                recovery_authorization_id: authorization_id.clone(),
                expected_manifest_revision: header.active_manifest_revision as u64,
                correlation_id: "exact-replay-correlation".into(),
            };
            let first = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("first recovery");
            while rx.try_recv().is_ok() {}
            let before_replay = durable_state(&db, &workflow_id).await;
            let replay = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("exact recovery replay");
            assert_eq!(replay.manifest_revision, first.manifest_revision);
            assert_eq!(
                replay.source_manifest_revision,
                first.source_manifest_revision
            );
            assert_eq!(replay.workflow_id, first.workflow_id);
            assert_eq!(replay.old_state, first.old_state);
            assert_eq!(replay.new_state, first.new_state);
            assert_eq!(replay.graph_revision, first.graph_revision);
            assert_eq!(replay.cause_code, first.cause_code);
            assert_eq!(
                replay.recovery_authorization_id,
                first.recovery_authorization_id
            );
            assert!(replay.idempotent_replay);
            assert_eq!(durable_state(&db, &workflow_id).await, before_replay);
            assert!(rx.try_recv().is_err(), "exact replay emits no event");

            let mut changed = request.clone();
            changed.correlation_id = "different-correlation".into();
            assert_eq!(
                recover_workflow_core(&db, &emitter, parent, changed)
                    .await
                    .unwrap_err(),
                WorkflowStoreError::WorkflowRecoveryConflict
            );
            let mut changed = request.clone();
            changed.expected_manifest_revision += 1;
            assert_eq!(
                recover_workflow_core(&db, &emitter, parent, changed)
                    .await
                    .unwrap_err(),
                WorkflowStoreError::WorkflowRecoveryConflict
            );
            assert!(matches!(
                recover_workflow_core(&db, &emitter, parent + 1, request.clone())
                    .await
                    .unwrap_err(),
                WorkflowStoreError::CrossParent { .. }
            ));
            let mut changed = request.clone();
            changed.recovery_authorization_id = "different-authorization".into();
            assert_eq!(
                recover_workflow_core(&db, &emitter, parent, changed)
                    .await
                    .unwrap_err(),
                WorkflowStoreError::WorkflowRecoveryConflict
            );
            let replay_rejections = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
            assert_eq!(replay_rejections.len(), 2);
            for rejection in replay_rejections {
                assert_eq!(rejection.channel, "workflow.recovery_rejected");
                assert_eq!(rejection.payload["workflow_id"], workflow_id);
                assert_eq!(
                    rejection.payload["recovery_authorization_id"],
                    authorization_id
                );
                assert_eq!(
                    rejection.payload["source_manifest_revision"],
                    first.source_manifest_revision
                );
                assert_eq!(
                    rejection.payload["rejection_code"],
                    "workflow_recovery_conflict"
                );
            }
            let receipt = load_authorization(&db, &authorization_id).await;
            let original_payload = receipt.action_payload_json.clone();
            let mut tampered: recovery_authorization::ActiveModel = receipt.into();
            tampered.action_payload_json = Set(json!({ "target_state": "approved" }).to_string());
            tampered
                .update(&db.conn)
                .await
                .expect("tamper replay payload");
            assert_eq!(
                recover_workflow_core(
                    &db,
                    &emitter,
                    parent,
                    RecoverWorkflowRequest {
                        workflow_id: workflow_id.clone(),
                        recovery_authorization_id: authorization_id.clone(),
                        expected_manifest_revision: first.source_manifest_revision,
                        correlation_id: "exact-replay-correlation".into(),
                    },
                )
                .await
                .unwrap_err(),
                WorkflowStoreError::WorkflowRecoveryConflict
            );
            let receipt = load_authorization(&db, &authorization_id).await;
            let mut restored: recovery_authorization::ActiveModel = receipt.into();
            restored.action_payload_json = Set(original_payload);
            restored
                .update(&db.conn)
                .await
                .expect("restore replay payload");
            assert_eq!(durable_state(&db, &workflow_id).await, before_replay);
            let payload_rejection = rx.try_recv().expect("payload conflict rejection");
            assert_eq!(payload_rejection.channel, "workflow.recovery_rejected");
            assert_eq!(
                payload_rejection.payload["rejection_code"],
                "workflow_recovery_conflict"
            );
            assert!(rx.try_recv().is_err());

            let later_header = load_header(&db, &workflow_id).await;
            let txn = db.conn.begin().await.expect("begin later revision");
            append_state_only_revision_txn(
                &txn,
                &later_header,
                StateOnlyRevisionRequest {
                    target_state: ManifestWorkflowState::Estimated,
                    transition_reason_code: "plan_gate_approved",
                    recovery_authorization_id: None,
                    consumer_correlation_id: None,
                    recovery_source_state_fingerprint: None,
                    recovery_risk_class: None,
                },
                Utc::now(),
            )
            .await
            .expect("append later state-only revision");
            txn.commit().await.expect("commit later revision");
            let after_later_publication = durable_state(&db, &workflow_id).await;
            let replay_after_later_publication =
                recover_workflow_core(&db, &emitter, parent, request)
                    .await
                    .expect("exact replay after later workflow activity");
            let mut expected_replay = first;
            expected_replay.idempotent_replay = true;
            assert_eq!(replay_after_later_publication, expected_replay);
            assert!(replay_after_later_publication.idempotent_replay);
            assert_eq!(
                durable_state(&db, &workflow_id).await,
                after_later_publication
            );
            assert!(rx.try_recv().is_err());

            for mutation in [
                "receipt_fingerprint",
                "receipt_cause",
                "receipt_risk",
                "noncanonical_payload",
                "receipt_action",
                "receipt_workflow_identity",
                "receipt_consumed_at",
                "receipt_consumed_at_value",
                "revision_digest",
                "revision_fingerprint",
                "revision_risk",
            ] {
                let token = format!("exact-replay-{mutation}");
                let (db, parent, workflow_id, emitter, mut rx) =
                    blocked_recovery_fixture(ManifestWorkflowState::Estimated, &token).await;
                let header = load_header(&db, &workflow_id).await;
                let (authorization_id, _) =
                    authorize_decision(&db, parent, &workflow_id, None).await;
                let request = RecoverWorkflowRequest {
                    workflow_id: workflow_id.clone(),
                    recovery_authorization_id: authorization_id.clone(),
                    expected_manifest_revision: header.active_manifest_revision as u64,
                    correlation_id: format!("exact-replay-{mutation}"),
                };
                recover_workflow_core(&db, &emitter, parent, request.clone())
                    .await
                    .expect("commit recovery before replay mutation");
                while rx.try_recv().is_ok() {}

                match mutation {
                    "receipt_fingerprint"
                    | "receipt_cause"
                    | "receipt_risk"
                    | "noncanonical_payload"
                    | "receipt_action"
                    | "receipt_workflow_identity"
                    | "receipt_consumed_at"
                    | "receipt_consumed_at_value" => {
                        let receipt = load_authorization(&db, &authorization_id).await;
                        let mut tampered: recovery_authorization::ActiveModel = receipt.into();
                        match mutation {
                            "receipt_fingerprint" => {
                                tampered.source_state_fingerprint =
                                    Set("workflow_recovery_v1:tampered".into());
                            }
                            "receipt_cause" => {
                                tampered.cause_code = Set("tampered_cause".into());
                            }
                            "receipt_risk" => {
                                tampered.risk_class = Set("tampered_risk".into());
                            }
                            "noncanonical_payload" => {
                                tampered.action_payload_json =
                                    Set("{ \"target_state\": \"estimated\" }".into());
                            }
                            "receipt_action" => {
                                tampered.allowed_action =
                                    Set(RecoveryAllowedAction::ResetPlanLineage.as_str().into());
                            }
                            "receipt_workflow_identity" => {
                                tampered.source_task_id = Set(Some("unexpected-task".into()));
                            }
                            "receipt_consumed_at" => tampered.consumed_at = Set(None),
                            "receipt_consumed_at_value" => {
                                tampered.consumed_at = Set(tampered.approved_at.clone().unwrap());
                            }
                            _ => unreachable!(),
                        }
                        tampered
                            .update(&db.conn)
                            .await
                            .expect("tamper consumed recovery receipt");
                    }
                    "revision_digest" | "revision_fingerprint" | "revision_risk" => {
                        let header = load_header(&db, &workflow_id).await;
                        let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
                            workflow_id.clone(),
                            header.active_manifest_revision,
                        ))
                        .one(&db.conn)
                        .await
                        .expect("load recovered revision")
                        .expect("recovered revision");
                        let mut tampered: delegation_workflow_manifest_revision::ActiveModel =
                            revision.into();
                        match mutation {
                            "revision_digest" => {
                                tampered.document_digest = Set("tampered-digest".into())
                            }
                            "revision_fingerprint" => {
                                tampered.recovery_source_state_fingerprint =
                                    Set(Some("workflow_recovery_v1:tampered".into()))
                            }
                            "revision_risk" => {
                                tampered.recovery_risk_class = Set(Some("tampered_risk".into()))
                            }
                            _ => unreachable!(),
                        }
                        tampered
                            .update(&db.conn)
                            .await
                            .expect("tamper recovered revision digest");
                    }
                    _ => unreachable!(),
                }

                let before_rejected_replay = durable_state(&db, &workflow_id).await;
                let before_receipt = load_authorization(&db, &authorization_id).await;
                let replay = recover_workflow_core(&db, &emitter, parent, request).await;
                assert!(
                    matches!(replay, Err(WorkflowStoreError::WorkflowRecoveryConflict)),
                    "mutation {mutation} must fail closed, got {replay:?}"
                );
                assert_eq!(
                    durable_state(&db, &workflow_id).await,
                    before_rejected_replay,
                    "mutation {mutation} must remain read-only"
                );
                assert_eq!(
                    load_authorization(&db, &authorization_id).await,
                    before_receipt,
                    "mutation {mutation} must not rewrite the consumed receipt"
                );
                let rejection = rx
                    .try_recv()
                    .unwrap_or_else(|_| panic!("mutation {mutation} must emit rejection"));
                assert_eq!(rejection.channel, "workflow.recovery_rejected");
                assert_eq!(
                    rejection.payload["rejection_code"],
                    "workflow_recovery_conflict"
                );
                assert!(rx.try_recv().is_err());
            }
        }

        #[tokio::test]
        async fn exact_replay_accepts_pre_upgrade_nullable_revision_evidence() {
            let (db, parent, workflow_id, emitter, mut rx) = blocked_recovery_fixture(
                ManifestWorkflowState::Estimated,
                "nullable-replay-evidence",
            )
            .await;
            let source_header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let request = RecoverWorkflowRequest {
                workflow_id: workflow_id.clone(),
                recovery_authorization_id: authorization_id,
                expected_manifest_revision: source_header.active_manifest_revision as u64,
                correlation_id: "nullable-replay-evidence".into(),
            };
            let first = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("initial recovery");
            while rx.try_recv().is_ok() {}

            let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
                workflow_id.clone(),
                first.manifest_revision as i64,
            ))
            .one(&db.conn)
            .await
            .expect("load recovery revision")
            .expect("recovery revision");
            let mut pre_upgrade: delegation_workflow_manifest_revision::ActiveModel =
                revision.into();
            pre_upgrade.recovery_source_state_fingerprint = Set(None);
            pre_upgrade.recovery_risk_class = Set(None);
            pre_upgrade
                .update(&db.conn)
                .await
                .expect("simulate pre-upgrade nullable recovery evidence");
            let before_replay = durable_state(&db, &workflow_id).await;

            let replay = recover_workflow_core(&db, &emitter, parent, request)
                .await
                .expect("replay using consumed receipt compatibility evidence");
            assert_eq!(
                replay,
                RecoverWorkflowResult {
                    idempotent_replay: true,
                    ..first
                }
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before_replay);
            assert!(
                rx.try_recv().is_err(),
                "compatibility replay emits no event"
            );
        }

        #[tokio::test]
        async fn exact_replay_survives_ordinary_later_workflow_activity() {
            let (db, parent, workflow_id, emitter, mut rx) =
                blocked_recovery_fixture(ManifestWorkflowState::Estimated, "later-activity").await;
            let source_header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let request = RecoverWorkflowRequest {
                workflow_id: workflow_id.clone(),
                recovery_authorization_id: authorization_id,
                expected_manifest_revision: source_header.active_manifest_revision as u64,
                correlation_id: "later-activity-replay".into(),
            };
            let first = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("initial recovery");
            while rx.try_recv().is_ok() {}

            let recovery_revision = delegation_workflow_manifest_revision::Entity::find_by_id((
                workflow_id.clone(),
                first.manifest_revision as i64,
            ))
            .one(&db.conn)
            .await
            .expect("load committed recovery revision")
            .expect("committed recovery revision");
            let mut document =
                serde_json::from_str::<ManifestDocument>(&recovery_revision.document_json)
                    .expect("decode recovery document");
            document.workflow_id = Some(workflow_id.clone());
            document.expected_manifest_revision = Some(first.manifest_revision);
            document
                .plan
                .as_mut()
                .expect("estimated fixture Plan")
                .digest = "sha256:plan-after-recovery".into();
            let later = publish_workflow_manifest_fixture(
                &db,
                &emitter,
                parent,
                PublishWorkflowRequest { document },
            )
            .await
            .expect("ordinary later workflow publication");
            assert!(later.manifest_revision > first.manifest_revision);
            assert!(later.graph_revision > first.graph_revision);
            let later_header = load_header(&db, &workflow_id).await;
            assert_eq!(
                later_header.active_manifest_revision,
                later.manifest_revision as i64
            );
            assert_ne!(later_header.updated_at, recovery_revision.created_at);
            while rx.try_recv().is_ok() {}

            let before_replay = durable_state(&db, &workflow_id).await;
            let replay = recover_workflow_core(&db, &emitter, parent, request)
                .await
                .expect("exact replay after later workflow activity");

            assert_eq!(
                replay,
                RecoverWorkflowResult {
                    idempotent_replay: true,
                    ..first
                }
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before_replay);
            assert!(rx.try_recv().is_err(), "exact replay emits no event");
        }

        #[tokio::test]
        async fn exact_replay_survives_later_task_admission_and_active_run() {
            use crate::acp::delegation::workflow::{
                admit_workflow_run_txn, AdmissionDispatchKind, WorkflowAdmitInput,
            };

            let (db, parent, workflow_id, emitter, mut rx) =
                blocked_recovery_fixture(ManifestWorkflowState::Approved, "later-admission").await;
            let source_header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let request = RecoverWorkflowRequest {
                workflow_id: workflow_id.clone(),
                recovery_authorization_id: authorization_id.clone(),
                expected_manifest_revision: source_header.active_manifest_revision as u64,
                correlation_id: "later-admission-replay".into(),
            };
            let first = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("initial workflow recovery");
            while rx.try_recv().is_ok() {}

            let task_binding = delegation_workflow_node_binding::Entity::find_by_id((
                workflow_id.clone(),
                "task-1-impl".to_string(),
            ))
            .one(&db.conn)
            .await
            .expect("load Task binding")
            .expect("Task binding");
            let task_id = "later-admission-task";
            let parent_row = conversation::Entity::find_by_id(parent)
                .one(&db.conn)
                .await
                .expect("load parent conversation")
                .expect("parent conversation");
            let workspace = folder::Entity::find_by_id(parent_row.folder_id)
                .one(&db.conn)
                .await
                .expect("load parent workspace")
                .expect("parent workspace")
                .path;
            let task_child = seed_conversation(&db, parent_row.folder_id, AgentType::Grok).await;
            let now = Utc::now();
            delegation_task_run::ActiveModel {
                task_id: Set(task_id.into()),
                root_task_id: Set(task_id.into()),
                previous_task_id: Set(None),
                generation: Set(1),
                parent_conversation_id: Set(parent),
                parent_tool_use_id: Set(Some("pt-later-admission".into())),
                child_conversation_id: Set(task_child),
                agent_type: Set("grok".into()),
                profile_id: Set(None),
                workspace_path: Set(Some(workspace.clone())),
                route_fingerprint: Set(Some("route-later-admission".into())),
                launch_snapshot_version: Set(Some("v1".into())),
                mode_id: Set(Some("default".into())),
                config_values_json: Set(Some("{}".into())),
                task_preview: Set(Some("execute Task 1".into())),
                request_fingerprint: Set(Some("request-later-admission".into())),
                admission_class: Set(AdmissionClass::NormalRevision),
                reached_running_at: Set(None),
                lineage_root_task_id: Set(task_id.into()),
                work_unit_key: Set(Some(task_binding.work_unit_key.clone())),
                legacy_parent_tool_use_id: Set(None),
                history_only: Set(false),
                status: Set(DelegationRunStatus::Reserving),
                error_code: Set(None),
                termination_audit_json: Set(None),
                started_at: Set(Some(now)),
                finished_at: Set(None),
                tool_call_count: Set(None),
                edit_tool_call_count: Set(None),
                touched_files_json: Set(None),
                touched_files_truncated: Set(None),
                additions: Set(None),
                deletions: Set(None),
                line_counts_complete: Set(None),
                card_summary_json: Set(None),
                child_turn_anchor: Set(None),
                child_connection_id: Set(None),
                replaced_task_id: Set(None),
                replacement_reason: Set(None),
                recovery_authorization_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            }
            .insert(&db.conn)
            .await
            .expect("insert reserving Task run");
            let txn = db.conn.begin().await.expect("begin Task admission");
            crate::acp::delegation::workflow::with_historical_workflow_fixture_mutations(
                admit_workflow_run_txn(
                    &txn,
                    &WorkflowAdmitInput {
                        parent_conversation_id: parent,
                        child_conversation_id: task_child,
                        task_id,
                        work_unit_key: Some(&task_binding.work_unit_key),
                        agent_type: "grok",
                        profile_id: None,
                        lineage_root_task_id: task_id,
                        generation: 1,
                        kind: AdmissionDispatchKind::FirstDispatch,
                        admission_class: AdmissionClass::NormalRevision,
                        workspace_path: Some(workspace.as_str()),
                    },
                ),
            )
            .await
            .expect("admit Task after recovery");
            txn.commit().await.expect("commit Task admission");
            let evolved_header = load_header(&db, &workflow_id).await;
            assert!(evolved_header.graph_revision > first.graph_revision as i64);
            let active = delegation_task_run::Entity::find_by_id(task_id)
                .one(&db.conn)
                .await
                .expect("load active Task run")
                .expect("active Task run");
            assert_eq!(active.status, DelegationRunStatus::Reserving);
            while rx.try_recv().is_ok() {}

            let before_replay = durable_state(&db, &workflow_id).await;
            let receipt_before = load_authorization(&db, &authorization_id).await;
            let replay = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("exact replay after Task admission");
            assert_eq!(
                replay,
                RecoverWorkflowResult {
                    idempotent_replay: true,
                    ..first
                }
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before_replay);
            assert_eq!(
                load_authorization(&db, &authorization_id).await,
                receipt_before
            );
            assert!(rx.try_recv().is_err(), "exact replay emits no event");

            let mut changed = request;
            changed.correlation_id = "later-admission-changed".into();
            assert_eq!(
                recover_workflow_core(&db, &emitter, parent, changed)
                    .await
                    .unwrap_err(),
                WorkflowStoreError::WorkflowRecoveryConflict
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before_replay);
            assert_eq!(
                load_authorization(&db, &authorization_id).await,
                receipt_before
            );
        }

        #[tokio::test]
        async fn generic_recover_receipt_cannot_satisfy_reset_plan_lineage() {
            let token = "generic-cannot-reset";
            let reason = "requirements changed after review";
            let (db, parent, workflow_id, emitter, mut rx) =
                plan_lineage_reset_fixture(token).await;
            insert_reset_reviewer_evidence(&db, parent, &workflow_id, token).await;
            let authorization_id =
                authorize_generic_recovery_for_reset_fixture(&db, parent, &workflow_id).await;
            let before = durable_state(&db, &workflow_id).await;
            let error = settle_reset(
                &db,
                &emitter,
                parent,
                &workflow_id,
                token,
                reason,
                Some(authorization_id.clone()),
                GateSettlementOutcome::Approved,
                vec![],
            )
            .await
            .expect_err("generic receipt cannot reset Plan lineage");
            assert_eq!(
                error,
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            assert!(rx.try_recv().is_err());
        }

        #[tokio::test]
        async fn lineage_reset_requires_exact_reason_receipt_and_persists_provenance() {
            let token = "exact-reset-reason";
            let reason = "requirements changed after review";
            let (db, parent, workflow_id, emitter, mut rx) =
                plan_lineage_reset_fixture(token).await;
            insert_reset_reviewer_evidence(&db, parent, &workflow_id, token).await;
            let before = durable_state(&db, &workflow_id).await;
            let missing = settle_reset(
                &db,
                &emitter,
                parent,
                &workflow_id,
                token,
                reason,
                None,
                GateSettlementOutcome::Approved,
                vec![],
            )
            .await
            .expect_err("lineage reset requires authorization");
            assert!(matches!(missing, WorkflowStoreError::GateNotReady(_)));
            assert_eq!(durable_state(&db, &workflow_id).await, before);

            let (authorization_id, _) =
                authorize_decision(&db, parent, &workflow_id, Some(reason)).await;
            let stale = settle_reset(
                &db,
                &emitter,
                parent,
                &workflow_id,
                token,
                "displayed reason changed",
                Some(authorization_id.clone()),
                GateSettlementOutcome::Approved,
                vec![],
            )
            .await
            .expect_err("changed displayed reason is stale");
            assert_eq!(
                stale,
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );

            let header = load_header(&db, &workflow_id).await;
            let original_plan_fingerprint = header.plan_fingerprint.clone();
            let mut changed_plan: delegation_workflow::ActiveModel = header.into();
            changed_plan.plan_fingerprint = Set("changed-plan-fingerprint".into());
            changed_plan
                .update(&db.conn)
                .await
                .expect("change Plan identity");
            assert_eq!(
                settle_reset(
                    &db,
                    &emitter,
                    parent,
                    &workflow_id,
                    token,
                    reason,
                    Some(authorization_id.clone()),
                    GateSettlementOutcome::Approved,
                    vec![],
                )
                .await
                .unwrap_err(),
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            let header = load_header(&db, &workflow_id).await;
            let mut restored_plan: delegation_workflow::ActiveModel = header.into();
            restored_plan.plan_fingerprint = Set(original_plan_fingerprint);
            restored_plan
                .update(&db.conn)
                .await
                .expect("restore Plan identity");

            let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
                workflow_id.clone(),
                "plan".to_string(),
                5,
            ))
            .one(&db.conn)
            .await
            .expect("load decision settlement")
            .expect("decision settlement");
            let original_content_fingerprint = settlement.content_fingerprint.clone();
            let original_reviewers = settlement.required_reviewer_node_ids_json.clone();
            let mut changed_gate: delegation_workflow_gate_settlement::ActiveModel =
                settlement.into();
            changed_gate.content_fingerprint = Set("changed-gate-fingerprint".into());
            changed_gate
                .update(&db.conn)
                .await
                .expect("change gate evidence");
            assert_eq!(
                settle_reset(
                    &db,
                    &emitter,
                    parent,
                    &workflow_id,
                    token,
                    reason,
                    Some(authorization_id.clone()),
                    GateSettlementOutcome::Approved,
                    vec![],
                )
                .await
                .unwrap_err(),
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
                workflow_id.clone(),
                "plan".to_string(),
                5,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut restored_gate: delegation_workflow_gate_settlement::ActiveModel =
                settlement.into();
            restored_gate.content_fingerprint = Set(original_content_fingerprint);
            restored_gate
                .update(&db.conn)
                .await
                .expect("restore gate evidence");

            let author_binding = delegation_workflow_run_binding::Entity::find()
                .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.clone()))
                .filter(delegation_workflow_run_binding::Column::NodeId.eq("plan-author"))
                .one(&db.conn)
                .await
                .expect("load author evidence")
                .expect("author evidence");
            let original_author_digest = author_binding.artifact_digest.clone();
            let mut changed_author: delegation_workflow_run_binding::ActiveModel =
                author_binding.into();
            changed_author.artifact_digest = Set(Some("changed-author-digest".into()));
            changed_author
                .update(&db.conn)
                .await
                .expect("change author evidence");
            assert_eq!(
                settle_reset(
                    &db,
                    &emitter,
                    parent,
                    &workflow_id,
                    token,
                    reason,
                    Some(authorization_id.clone()),
                    GateSettlementOutcome::Approved,
                    vec![],
                )
                .await
                .unwrap_err(),
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            let author_binding = delegation_workflow_run_binding::Entity::find()
                .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.clone()))
                .filter(delegation_workflow_run_binding::Column::NodeId.eq("plan-author"))
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut restored_author: delegation_workflow_run_binding::ActiveModel =
                author_binding.into();
            restored_author.artifact_digest = Set(original_author_digest);
            restored_author
                .update(&db.conn)
                .await
                .expect("restore author evidence");

            let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
                workflow_id.clone(),
                "plan".to_string(),
                5,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut changed_reviewers: delegation_workflow_gate_settlement::ActiveModel =
                settlement.into();
            changed_reviewers.required_reviewer_node_ids_json = Set(Some("[]".into()));
            changed_reviewers
                .update(&db.conn)
                .await
                .expect("change reviewer set");
            assert_eq!(
                settle_reset(
                    &db,
                    &emitter,
                    parent,
                    &workflow_id,
                    token,
                    reason,
                    Some(authorization_id.clone()),
                    GateSettlementOutcome::Approved,
                    vec![],
                )
                .await
                .unwrap_err(),
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
                workflow_id.clone(),
                "plan".to_string(),
                5,
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
            let mut restored_reviewers: delegation_workflow_gate_settlement::ActiveModel =
                settlement.into();
            restored_reviewers.required_reviewer_node_ids_json = Set(original_reviewers);
            restored_reviewers
                .update(&db.conn)
                .await
                .expect("restore reviewer set");
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            assert!(
                rx.try_recv().is_err(),
                "legacy lineage-reset inputs must not emit recovery events"
            );

            let error = settle_reset(
                &db,
                &emitter,
                parent,
                &workflow_id,
                token,
                reason,
                Some(authorization_id.clone()),
                GateSettlementOutcome::Approved,
                vec![],
            )
            .await
            .expect_err("fixed-v2 settlement rejects legacy lineage-reset authority");
            assert_eq!(
                error,
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            assert!(rx.try_recv().is_err());
        }

        #[tokio::test]
        async fn legacy_lineage_reset_receipt_is_read_only_under_fixed_v2() {
            let token = "fixed-v2-read-only-reset";
            let reason = "requirements changed after review";
            let (db, parent, workflow_id, emitter, mut rx) =
                plan_lineage_reset_fixture(token).await;
            insert_reset_reviewer_evidence(&db, parent, &workflow_id, token).await;
            let (authorization_id, _) =
                authorize_decision(&db, parent, &workflow_id, Some(reason)).await;
            let before = durable_state(&db, &workflow_id).await;
            let error = settle_reset(
                &db,
                &emitter,
                parent,
                &workflow_id,
                token,
                reason,
                Some(authorization_id.clone()),
                GateSettlementOutcome::Approved,
                vec![],
            )
            .await
            .expect_err("fixed-v2 settlement rejects legacy lineage-reset authority");
            assert_eq!(
                error,
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Approved
            );
            assert!(rx.try_recv().is_err());
        }

        #[tokio::test]
        async fn rejection_events_suppress_corrupt_persisted_causes() {
            let (db, parent, workflow_id, emitter, mut rx) = blocked_recovery_fixture(
                ManifestWorkflowState::Estimated,
                "corrupt-replay-rejection-cause",
            )
            .await;
            let source_header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let request = RecoverWorkflowRequest {
                workflow_id: workflow_id.clone(),
                recovery_authorization_id: authorization_id.clone(),
                expected_manifest_revision: source_header.active_manifest_revision as u64,
                correlation_id: "corrupt-replay-rejection-cause".into(),
            };
            let recovered = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("recover before corrupting replay provenance");
            while rx.try_recv().is_ok() {}

            let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
                workflow_id.clone(),
                recovered.manifest_revision as i64,
            ))
            .one(&db.conn)
            .await
            .expect("load recovery revision for corruption")
            .expect("committed recovery revision");
            let mut corrupt_revision: delegation_workflow_manifest_revision::ActiveModel =
                revision.into();
            corrupt_revision.transition_reason_code =
                Set(Some("corrupt arbitrary replay cause".into()));
            corrupt_revision
                .update(&db.conn)
                .await
                .expect("corrupt persisted replay cause");
            let before_replay = durable_state(&db, &workflow_id).await;
            let mut conflicting_replay = request;
            conflicting_replay.correlation_id = "corrupt-replay-rejection-conflict".into();
            assert_eq!(
                recover_workflow_core(&db, &emitter, parent, conflicting_replay)
                    .await
                    .unwrap_err(),
                WorkflowStoreError::WorkflowRecoveryConflict
            );
            assert_eq!(durable_state(&db, &workflow_id).await, before_replay);
            let replay_rejection_channels = std::iter::from_fn(|| rx.try_recv().ok())
                .map(|event| event.channel)
                .collect::<Vec<_>>();

            let reset_reason = "authorized reset reason";
            let reset_token = "corrupt-reset-rejection-cause";
            let (reset_db, reset_parent, reset_workflow_id, reset_emitter, mut reset_rx) =
                plan_lineage_reset_fixture(reset_token).await;
            insert_reset_reviewer_evidence(
                &reset_db,
                reset_parent,
                &reset_workflow_id,
                reset_token,
            )
            .await;
            let (reset_authorization_id, _) = authorize_decision(
                &reset_db,
                reset_parent,
                &reset_workflow_id,
                Some(reset_reason),
            )
            .await;
            let receipt = load_authorization(&reset_db, &reset_authorization_id).await;
            let mut corrupt_receipt: recovery_authorization::ActiveModel = receipt.into();
            corrupt_receipt.cause_code = Set("corrupt arbitrary reset cause".into());
            corrupt_receipt
                .update(&reset_db.conn)
                .await
                .expect("corrupt persisted reset cause");
            let before_reset = durable_state(&reset_db, &reset_workflow_id).await;
            let receipt_before_reset = load_authorization(&reset_db, &reset_authorization_id).await;
            assert_eq!(
                settle_reset(
                    &reset_db,
                    &reset_emitter,
                    reset_parent,
                    &reset_workflow_id,
                    reset_token,
                    "changed reset reason",
                    Some(reset_authorization_id.clone()),
                    GateSettlementOutcome::Approved,
                    vec![],
                )
                .await
                .unwrap_err(),
                WorkflowStoreError::RecoveryAuthorizationRejected {
                    code: "recovery_authorization_action_mismatch"
                }
            );
            assert_eq!(
                durable_state(&reset_db, &reset_workflow_id).await,
                before_reset
            );
            assert_eq!(
                load_authorization(&reset_db, &reset_authorization_id).await,
                receipt_before_reset
            );
            let reset_rejection_channels = std::iter::from_fn(|| reset_rx.try_recv().ok())
                .map(|event| event.channel)
                .collect::<Vec<_>>();

            assert_eq!(
                (replay_rejection_channels, reset_rejection_channels),
                (Vec::<String>::new(), Vec::<String>::new()),
                "corrupt persisted causes must suppress rejection events"
            );
        }

        #[tokio::test]
        async fn event_failure_after_commit_keeps_durable_recovered_state() {
            let (db, parent, workflow_id, emitter, mut rx) =
                blocked_recovery_fixture(ManifestWorkflowState::Estimated, "event-failure").await;
            let header = load_header(&db, &workflow_id).await;
            let (authorization_id, _) = authorize_decision(&db, parent, &workflow_id, None).await;
            let request = RecoverWorkflowRequest {
                workflow_id: workflow_id.clone(),
                recovery_authorization_id: authorization_id.clone(),
                expected_manifest_revision: header.active_manifest_revision as u64,
                correlation_id: "event-failure-correlation".into(),
            };
            set_inject_workflow_recovery_event_failure(true);
            let committed = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("event delivery failure still returns committed result");
            set_inject_workflow_recovery_event_failure(false);
            assert!(!committed.idempotent_replay);
            let graph_event = rx
                .try_recv()
                .expect("durable graph notification remains independently deliverable");
            assert_eq!(
                graph_event.channel,
                super::super::super::events::WORKFLOW_GRAPH_CHANGED_EVENT
            );
            assert_eq!(
                *graph_event.payload,
                json!({
                    "parent_conversation_id": parent,
                    "workflow_id": workflow_id,
                    "graph_revision": committed.graph_revision,
                })
            );
            assert!(rx.try_recv().is_err());
            let durable = durable_state(&db, &workflow_id).await;
            assert_eq!(durable.header.workflow_state, WorkflowState::Estimated);
            assert_eq!(
                durable.header.active_manifest_revision as u64,
                committed.manifest_revision
            );
            assert_eq!(durable.revisions.len() as u64, committed.manifest_revision);
            assert_eq!(
                load_authorization(&db, &authorization_id).await.status,
                RecoveryAuthorizationStatus::Consumed
            );
            let status = get_workflow_state_core(&db, parent, Some(&workflow_id))
                .await
                .expect("later status converges from durable state");
            assert_eq!(status.workflow_state, ManifestWorkflowState::Estimated);
            assert!(status.recovery.is_none());
            let replay = recover_workflow_core(&db, &emitter, parent, request)
                .await
                .expect("retry after event failure is exact replay");
            assert!(replay.idempotent_replay);
            assert_eq!(replay.manifest_revision, committed.manifest_revision);
            assert_eq!(
                load_revisions(&db, &workflow_id).await.len(),
                durable.revisions.len()
            );
            assert!(
                rx.try_recv().is_err(),
                "exact replay must not duplicate recovery or graph events"
            );
        }

        #[tokio::test]
        async fn workflow_recovery_events_exclude_plan_contents_prompts_and_display_reason() {
            let (db, parent, workflow_id, emitter, mut rx) = blocked_recovery_fixture(
                ManifestWorkflowState::Estimated,
                "production-event-privacy",
            )
            .await;
            let source_header = load_header(&db, &workflow_id).await;
            let (authorization_id, decision) =
                authorize_decision(&db, parent, &workflow_id, None).await;
            let request = RecoverWorkflowRequest {
                workflow_id: workflow_id.clone(),
                recovery_authorization_id: authorization_id.clone(),
                expected_manifest_revision: source_header.active_manifest_revision as u64,
                correlation_id: "production-event-privacy".into(),
            };
            let recovered = recover_workflow_core(&db, &emitter, parent, request.clone())
                .await
                .expect("capture committed recovery events");
            let mut events = std::iter::from_fn(|| rx.try_recv().ok())
                .filter(|event| event.channel.starts_with("workflow."))
                .collect::<Vec<_>>();

            let mut conflicting_replay = request;
            conflicting_replay.correlation_id = "production-event-replay-conflict".into();
            assert_eq!(
                recover_workflow_core(&db, &emitter, parent, conflicting_replay)
                    .await
                    .unwrap_err(),
                WorkflowStoreError::WorkflowRecoveryConflict
            );
            events.extend(
                std::iter::from_fn(|| rx.try_recv().ok())
                    .filter(|event| event.channel.starts_with("workflow.")),
            );

            let channels = events
                .iter()
                .map(|event| event.channel.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                channels,
                vec![
                    "workflow.recovery_decision",
                    "workflow.recovery_confirmation_requested",
                    "workflow.recovery_authorization_consumed",
                    "workflow.state_only_revision_created",
                    "workflow.binding_reactivated",
                    "workflow.recovery_rejected",
                ]
            );

            for event in events {
                let value = event.payload;
                assert_eq!(value["event"], event.channel);
                let mut keys = value
                    .as_object()
                    .expect("event object")
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                keys.sort_unstable();
                let expected_keys = match event.channel.as_str() {
                    "workflow.recovery_decision" => vec![
                        "action",
                        "cause_code",
                        "event",
                        "graph_revision",
                        "source_manifest_revision",
                        "target_state",
                        "workflow_id",
                    ],
                    "workflow.recovery_confirmation_requested" => vec![
                        "action",
                        "cause_code",
                        "event",
                        "graph_revision",
                        "recovery_authorization_id",
                        "source_manifest_revision",
                        "target_state",
                        "workflow_id",
                    ],
                    "workflow.recovery_authorization_consumed" => vec![
                        "action",
                        "event",
                        "graph_revision",
                        "manifest_revision",
                        "recovery_authorization_id",
                        "workflow_id",
                    ],
                    "workflow.recovery_rejected" => vec![
                        "action",
                        "cause_code",
                        "event",
                        "graph_revision",
                        "recovery_authorization_id",
                        "rejection_code",
                        "source_manifest_revision",
                        "workflow_id",
                    ],
                    "workflow.state_only_revision_created" => vec![
                        "cause_code",
                        "event",
                        "graph_revision",
                        "manifest_revision",
                        "source_manifest_revision",
                        "target_state",
                        "workflow_id",
                    ],
                    "workflow.plan_lineage_reset" => vec![
                        "action",
                        "cause_code",
                        "event",
                        "graph_revision",
                        "manifest_revision",
                        "recovery_authorization_id",
                        "source_manifest_revision",
                        "target_state",
                        "workflow_id",
                    ],
                    "workflow.binding_reactivated" => vec![
                        "event",
                        "graph_revision",
                        "manifest_revision",
                        "target_state",
                        "workflow_id",
                    ],
                    other => panic!("unexpected production recovery event {other}"),
                };
                assert_eq!(keys, expected_keys);
                let encoded = value.to_string();
                for forbidden in [
                    "plan_contents",
                    "prompt",
                    "display_reason",
                    "external_session_id",
                    "action_payload",
                ] {
                    assert!(
                        !encoded.contains(forbidden),
                        "workflow event leaked {forbidden}"
                    );
                }

                if event.channel == "workflow.recovery_rejected"
                    && value["workflow_id"] == workflow_id
                {
                    assert_eq!(value["workflow_id"], workflow_id);
                    assert_eq!(value["recovery_authorization_id"], authorization_id);
                    assert_eq!(
                        value["source_manifest_revision"],
                        recovered.source_manifest_revision
                    );
                    assert_eq!(value["graph_revision"], recovered.graph_revision);
                    assert_eq!(value["action"], "recover_workflow");
                    assert_eq!(value["cause_code"], decision.cause_code.as_str());
                    assert_eq!(value["rejection_code"], "workflow_recovery_conflict");
                } else {
                    assert_eq!(value["workflow_id"], workflow_id);
                    assert_eq!(value["graph_revision"], recovered.graph_revision);
                    if value.get("action").is_some() {
                        assert_eq!(value["action"], "recover_workflow");
                    }
                    if value.get("cause_code").is_some() {
                        assert_eq!(value["cause_code"], decision.cause_code.as_str());
                    }
                    if value.get("recovery_authorization_id").is_some() {
                        assert_eq!(value["recovery_authorization_id"], authorization_id);
                    }
                }
            }
        }
    }
}
