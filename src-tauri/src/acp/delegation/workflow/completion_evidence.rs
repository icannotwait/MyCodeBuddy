//! Atomic protocol-v2 terminal evidence and artifact-recovery materialization.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{SecondsFormat, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::admission::bump_graph_revision;
use super::artifact_resolver::{
    resolve_document, resolve_producer_completion, resolve_reviewer_completion, ArtifactError,
    ArtifactFailure, ArtifactKind, GitHeadV1Artifact, ResolvedArtifact,
};
use super::completion_intent::{
    resolve_completion_intent, CompletionCandidate, CompletionDiagnostic, CompletionIntent,
    CompletionIntentReason, CompletionIntentSource, CompletionOutcome, CompletionReportCandidate,
    CompletionResolution, CompletionResolverInput, CompletionRole, CompletionToolIntent,
};
use super::dto::sha256_hex_str;
use super::error::{
    require_v2_mutation, CompletionEvidenceError, CompletionRecoveryFenceError, WorkflowStoreError,
};
use super::events::{CompletionDecisionResolvedPayloadV1, COMPLETION_DECISION_RESOLVED_EVENT};
use super::evidence_scope::{
    build_admission_completion_context,
    build_persisted_completion_context as build_persisted_context,
    build_preloaded_persisted_completion_context, preload_persisted_completion_context,
    validate_completion_evidence, AdmissionCandidate, PersistedCompletionContextPreload,
    WorkflowStore,
};
use super::final_findings::{
    bounded_terminal_context_v1, build_final_findings_package_v1,
    count_active_final_findings_packages_for_workflow_v1, persist_final_findings_package_v1,
    remediation_context_inputs_from_snapshots_v1,
    resolve_active_final_findings_packages_for_workflow_v1,
    resolve_active_final_findings_packages_v1, snapshot_remediation_contexts_v1,
    verify_remediation_context_snapshots_v1, FinalFindingInputV1, FinalFindingsError,
    FinalFindingsPackageInputV1, FinalFindingsPackageV1, FinalReviewerEvaluationV1,
    RemediationContextAvailability, RemediationContextInputV1, RemediationContextSnapshotV1,
};
use super::store::load_completion_protocol_header;
use super::types::{
    AdmissionCompletionContextV2, ArtifactSubjectIdentityV2, CompletionArtifactV2,
    CompletionEvidenceBindingV2, CompletionEvidenceV2, CompletionScopeRole,
    EvidenceValidationContext, NormalizedManifest, ValidatedCompletionEvidence,
    COMPLETION_PROTOCOL_VERSION_V2,
};
use crate::acp::delegation::attention::{
    open_design_self_review_attention_txn, open_terminal_completion_attention_txn,
    CompletionAttentionResolutionCode, DesignSelfReviewAttentionInput,
    TerminalCompletionAttentionInput, ATTENTION_PAYLOAD_MAX_BYTES,
};
use crate::acp::delegation::metrics::{
    CompletionFinalMetricState, CompletionMetricPhase, CompletionScopeInvalidationDimension,
    DelegationMetrics,
};
use crate::acp::delegation::types::CompletionMutationResult;
use crate::db::entities::conversation;
use crate::db::entities::delegation_attention_request::{self, AttentionKind};
use crate::db::entities::delegation_task_run::{self, CompletionState, DelegationRunStatus};
use crate::db::entities::delegation_workflow::CompletionProtocolMode;
use crate::db::entities::{
    delegation_completion_tool_intent, delegation_workflow,
    delegation_workflow_design_root_binding, delegation_workflow_gate_settlement,
    delegation_workflow_gate_state, delegation_workflow_node_binding,
    delegation_workflow_outbox_event, delegation_workflow_run_binding,
};
use crate::db::AppDatabase;

const MAX_DOCUMENT_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
const COMPLETION_DECISION_MESSAGE: &str = "Completion outcome requires a direct decision.";
const ARTIFACT_RECOVERY_MESSAGE: &str = "Completion artifact is not yet available.";
const DESIGN_SELF_REVIEW_MESSAGE: &str = "Design self-review requires a direct decision.";

#[cfg(test)]
thread_local! {
    static TERMINAL_ROW_LOAD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PRELOADED_COMPLETION_VALIDATION_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_terminal_row_load_count() {
    TERMINAL_ROW_LOAD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn terminal_row_load_count() -> usize {
    TERMINAL_ROW_LOAD_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_preloaded_completion_validation_count() {
    PRELOADED_COMPLETION_VALIDATION_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn preloaded_completion_validation_count() -> usize {
    PRELOADED_COMPLETION_VALIDATION_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
fn note_terminal_row_load() {
    TERMINAL_ROW_LOAD_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
fn note_terminal_row_load() {}

#[cfg(test)]
fn note_preloaded_completion_validation() {
    PRELOADED_COMPLETION_VALIDATION_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
fn note_preloaded_completion_validation() {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct V2GateEvidenceIdentity {
    pub gate_lineage: String,
    pub review_round: i64,
    pub required_node_ids: Vec<String>,
    pub task_ids: Vec<String>,
    pub scope_digests: Vec<String>,
    pub aggregate_scope_digest: String,
}

impl V2GateEvidenceIdentity {
    pub(crate) fn new(
        gate_lineage: String,
        review_round: i64,
        required_node_ids: Vec<String>,
        task_ids: Vec<String>,
        scope_digests: Vec<String>,
    ) -> Option<Self> {
        if gate_lineage.is_empty() || review_round <= 0 {
            return None;
        }
        let required_node_ids = canonical_identity_set(required_node_ids)?;
        let task_ids = canonical_identity_set(task_ids)?;
        let scope_digests = canonical_identity_set(scope_digests)?;
        if required_node_ids.is_empty()
            || task_ids.len() != required_node_ids.len()
            || scope_digests.len() != required_node_ids.len()
        {
            return None;
        }
        let scope_json = serde_json::to_string(&scope_digests).ok()?;
        let aggregate_scope_digest = format!("sha256:{}", sha256_hex_str(&scope_json));
        Some(Self {
            gate_lineage,
            review_round,
            required_node_ids,
            task_ids,
            scope_digests,
            aggregate_scope_digest,
        })
    }

    pub(crate) fn matches_settlement(
        &self,
        settlement: &delegation_workflow_gate_settlement::Model,
    ) -> bool {
        settlement.gate_lineage.as_deref() == Some(self.gate_lineage.as_str())
            && settlement.review_round == Some(self.review_round)
            && settlement.evidence_scope_digest.as_deref()
                == Some(self.aggregate_scope_digest.as_str())
            && persisted_identity_set(settlement.required_node_set_json.as_deref()).as_ref()
                == Some(&self.required_node_ids)
            && persisted_identity_set(settlement.required_evidence_task_ids_json.as_deref())
                .as_ref()
                == Some(&self.task_ids)
            && persisted_identity_set(settlement.evidence_scope_digests_json.as_deref()).as_ref()
                == Some(&self.scope_digests)
    }
}

fn canonical_identity_set(mut values: Vec<String>) -> Option<Vec<String>> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return None;
    }
    values.sort();
    if values.windows(2).any(|pair| pair[0] == pair[1]) {
        return None;
    }
    Some(values)
}

fn persisted_identity_set(json: Option<&str>) -> Option<Vec<String>> {
    canonical_identity_set(serde_json::from_str(json?).ok()?)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompletionMutationError {
    #[error("attention kind does not match this operation")]
    KindMismatch,
    #[error("completion attention is owned by another root conversation")]
    Unauthorized,
    #[error("completion decision was superseded")]
    Superseded,
    #[error("completion decision conflicts with a committed outcome")]
    Conflict,
    #[error("completion outcome is not legal for the durable role")]
    RoleMismatch,
    #[error("completion attention is invalid: {0}")]
    InvalidAttention(String),
    #[error("{message}")]
    Protocol { code: &'static str, message: String },
    #[error(transparent)]
    Evidence(#[from] CompletionEvidenceError),
}

impl CompletionMutationError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::KindMismatch => "attention_kind_mismatch",
            Self::Unauthorized => "unauthorized",
            Self::Superseded => "completion_decision_superseded",
            Self::Conflict => "completion_decision_conflict",
            Self::RoleMismatch => "completion_outcome_role_mismatch",
            Self::InvalidAttention(_) => "completion_attention_invalid",
            Self::Protocol { code, .. } => code,
            Self::Evidence(error) => error.code(),
        }
    }
}

fn completion_store_mutation_error(error: WorkflowStoreError) -> CompletionMutationError {
    match error {
        WorkflowStoreError::Persistence(message) => {
            CompletionEvidenceError::Persistence(message).into()
        }
        other => CompletionMutationError::Protocol {
            code: other.code(),
            message: other.to_string(),
        },
    }
}

fn completion_store_evidence_error(error: WorkflowStoreError) -> CompletionEvidenceError {
    match error {
        WorkflowStoreError::Persistence(message) => CompletionEvidenceError::Persistence(message),
        other => CompletionEvidenceError::Protocol {
            code: other.code(),
            message: other.to_string(),
        },
    }
}

async fn require_workflow_v2_completion_mutation<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
) -> Result<(), CompletionMutationError> {
    let (version, mode) = load_completion_protocol_header(conn, workflow_id)
        .await
        .map_err(completion_store_mutation_error)?
        .ok_or_else(|| {
            CompletionMutationError::InvalidAttention("workflow protocol header is missing".into())
        })?;
    require_v2_mutation(version, &mode).map_err(completion_store_mutation_error)
}

async fn require_task_v2_completion_mutation<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<(), CompletionMutationError> {
    let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            CompletionMutationError::InvalidAttention("workflow run binding is missing".into())
        })?;
    require_workflow_v2_completion_mutation(conn, &binding.workflow_id).await
}

async fn require_task_v2_completion_evidence<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<(), CompletionEvidenceError> {
    let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            CompletionEvidenceError::InvalidTerminalState("workflow run binding is missing".into())
        })?;
    let (version, mode) = load_completion_protocol_header(conn, &binding.workflow_id)
        .await
        .map_err(completion_store_evidence_error)?
        .ok_or_else(|| {
            CompletionEvidenceError::InvalidTerminalState(
                "workflow protocol header is missing".into(),
            )
        })?;
    require_v2_mutation(version, &mode).map_err(completion_store_evidence_error)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignSelfReviewPayloadV1 {
    pub version: u32,
    pub design_identity: String,
    pub gate_lineage: String,
    pub legal_outcomes: Vec<CompletionOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserOutcomeResolutionV1 {
    version: u32,
    code: String,
    outcome: CompletionOutcome,
    actor_identity: String,
    committed_scope_digest: String,
    graph_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedReportCandidate {
    pub path: String,
    pub contents: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCompletionInput {
    pub task_id: String,
    pub terminal_status: DelegationRunStatus,
    pub final_assistant_text: String,
    pub pre_read_reports: Vec<ValidatedReportCandidate>,
    pub pre_read_artifact: Option<ResolvedArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionAttentionCas {
    pub attention_id: String,
    pub task_id: String,
    pub kind: AttentionKind,
    pub captured_scope_digest: String,
    pub latest_run_id: String,
    pub node_id: String,
}

pub const COMPLETION_ATTENTION_NODE_ID_MAX_CHARS: usize = 128;

pub fn completion_attention_public_node_id(node_id: &str) -> String {
    super::dto::safe_public_id(node_id)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalCompletionResult {
    pub state: CompletionState,
    pub evidence: Option<CompletionEvidenceV2>,
    pub attention: Option<CompletionAttentionCas>,
    pub graph_revision: u64,
    #[serde(skip)]
    pub final_metric_states: Vec<CompletionFinalMetricState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionSourceAuditRef {
    ToolIntent { intent_id: String },
    AssistantConclusion,
    Report { report_file: Option<String> },
    UserAdjudication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecoveryPayloadV1 {
    pub version: u32,
    pub normalized_intent: CompletionIntent,
    pub source_audit_ref: CompletionSourceAuditRef,
    pub resolver_failure: ArtifactFailure,
    pub producer_scope_digest: String,
    pub producer_baseline_head: String,
    pub expected_resolver_kind: ArtifactKind,
    pub producer_task_id: String,
    pub producer_generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionDecisionPayloadV1 {
    version: u32,
    reason_code: CompletionIntentReason,
    role: CompletionRole,
    legal_outcomes: Vec<CompletionOutcome>,
    bounded_candidates: Vec<CompletionCandidate>,
    diagnostics: Vec<CompletionDiagnostic>,
}

#[derive(Debug)]
struct LoadedTerminal {
    run: delegation_task_run::Model,
    binding: delegation_workflow_run_binding::Model,
    workflow: delegation_workflow::Model,
    node: delegation_workflow_node_binding::Model,
}

enum FinalFindingsTerminalAction {
    NotFinal,
    Incomplete,
    Resolve { gate_id: String },
    Persist(FinalFindingsPackageV1),
    NeedsDecision { gate_id: String },
}

struct FinalFindingsTerminalEvaluation {
    action: FinalFindingsTerminalAction,
    current_contexts: Option<Vec<RemediationContextSnapshotV1>>,
}

#[derive(Debug)]
struct LoadedToolIntent {
    intent_id: String,
    intent: CompletionToolIntent,
}

#[derive(Debug)]
enum RetryTxnOutcome {
    Resolved {
        result: Box<TerminalCompletionResult>,
        idempotent_replay: bool,
    },
    Superseded {
        phase: CompletionMetricPhase,
        dimension: CompletionScopeInvalidationDimension,
        record_metric: bool,
    },
}

enum DecisionTxnOutcome {
    Resolved(Box<CompletionMutationResult>),
    Superseded,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompletionAttentionReconcileReport {
    pub retained: usize,
    pub superseded: usize,
    pub reopened: usize,
    pub workflow_deleted: usize,
}

enum CompletionAttentionReconcileOutcome {
    Retained,
    Superseded,
    Reopened,
}

/// Reconcile completion-family attention before replaying the durable outbox.
/// Child questions remain owned by the Broker's live-edge reconciliation.
pub async fn reconcile_completion_attentions_txn(
    db: &AppDatabase,
) -> Result<CompletionAttentionReconcileReport, CompletionEvidenceError> {
    db.conn
        .transaction::<_, CompletionAttentionReconcileReport, CompletionEvidenceError>(|txn| {
            Box::pin(async move {
                let rows = delegation_attention_request::Entity::find()
                    .filter(delegation_attention_request::Column::Status.eq("open"))
                    .filter(
                        delegation_attention_request::Column::Kind.ne(AttentionKind::ChildQuestion),
                    )
                    .order_by_asc(delegation_attention_request::Column::CreatedAt)
                    .order_by_asc(delegation_attention_request::Column::RequestId)
                    .all(txn)
                    .await
                    .map_err(db_error)?;
                let mut report = CompletionAttentionReconcileReport::default();
                for row in rows {
                    if reconcile_deleted_root_attention(txn, &row).await? {
                        report.workflow_deleted += 1;
                        continue;
                    }
                    let outcome = match row.kind {
                        AttentionKind::CompletionDecision
                        | AttentionKind::CompletionArtifactRecovery => {
                            reconcile_terminal_attention(txn, &row).await?
                        }
                        AttentionKind::DesignSelfReviewDecision => {
                            reconcile_design_attention(txn, &row).await?
                        }
                        AttentionKind::ChildQuestion => continue,
                    };
                    match outcome {
                        CompletionAttentionReconcileOutcome::Retained => report.retained += 1,
                        CompletionAttentionReconcileOutcome::Superseded => report.superseded += 1,
                        CompletionAttentionReconcileOutcome::Reopened => {
                            report.superseded += 1;
                            report.reopened += 1;
                        }
                    }
                }
                Ok(report)
            })
        })
        .await
        .map_err(|error| match error {
            sea_orm::TransactionError::Connection(error) => {
                CompletionEvidenceError::Persistence(error.to_string())
            }
            sea_orm::TransactionError::Transaction(error) => error,
        })
}

/// Close every open completion-family row owned by a workflow before an
/// explicit workflow termination/deletion. Recoverable `Blocked` state never
/// calls this API.
pub async fn resolve_workflow_completion_attentions_txn(
    db: &AppDatabase,
    workflow_id: &str,
    code: CompletionAttentionResolutionCode,
) -> Result<usize, CompletionEvidenceError> {
    if !matches!(
        code,
        CompletionAttentionResolutionCode::WorkflowTerminated
            | CompletionAttentionResolutionCode::WorkflowDeleted
    ) {
        return Err(CompletionEvidenceError::InvalidAttention(
            "workflow cleanup requires a terminal lifecycle code".into(),
        ));
    }
    let workflow_id = workflow_id.to_string();
    db.conn
        .transaction::<_, usize, CompletionEvidenceError>(|txn| {
            let workflow_id = workflow_id.clone();
            Box::pin(async move {
                resolve_workflow_completion_attentions(txn, &workflow_id, code).await
            })
        })
        .await
        .map_err(|error| match error {
            sea_orm::TransactionError::Connection(error) => {
                CompletionEvidenceError::Persistence(error.to_string())
            }
            sea_orm::TransactionError::Transaction(error) => error,
        })
}

async fn resolve_workflow_completion_attentions<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    code: CompletionAttentionResolutionCode,
) -> Result<usize, CompletionEvidenceError> {
    let rows = delegation_attention_request::Entity::find()
        .filter(delegation_attention_request::Column::Status.eq("open"))
        .filter(delegation_attention_request::Column::Kind.ne(AttentionKind::ChildQuestion))
        .all(conn)
        .await
        .map_err(db_error)?;
    let mut targets = Vec::new();
    for row in rows {
        let belongs = match row.kind {
            AttentionKind::CompletionDecision | AttentionKind::CompletionArtifactRecovery => {
                delegation_workflow_run_binding::Entity::find_by_id(&row.task_id)
                    .one(conn)
                    .await
                    .map_err(db_error)?
                    .is_some_and(|binding| binding.workflow_id == workflow_id)
            }
            AttentionKind::DesignSelfReviewDecision => {
                delegation_workflow_design_root_binding::Entity::find()
                    .filter(
                        delegation_workflow_design_root_binding::Column::TaskId.eq(&row.task_id),
                    )
                    .one(conn)
                    .await
                    .map_err(db_error)?
                    .is_some_and(|binding| binding.workflow_id == workflow_id)
            }
            AttentionKind::ChildQuestion => false,
        };
        if belongs {
            targets.push(row);
        }
    }
    if targets.is_empty()
        && count_active_final_findings_packages_for_workflow_v1(conn, workflow_id)
            .await
            .map_err(map_final_findings_error)?
            == 0
    {
        return Ok(0);
    }
    let graph_revision = if delegation_workflow::Entity::find_by_id(workflow_id)
        .one(conn)
        .await
        .map_err(db_error)?
        .is_some()
    {
        bump_graph_revision(conn, workflow_id, Utc::now())
            .await
            .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?
    } else {
        0
    };
    for row in &targets {
        resolve_completion_attention_row(conn, row, code, graph_revision).await?;
    }
    if graph_revision > 0 {
        resolve_active_final_findings_packages_for_workflow_v1(
            conn,
            workflow_id,
            i64::try_from(graph_revision).map_err(|_| {
                CompletionEvidenceError::Persistence("graph revision exceeds i64".into())
            })?,
        )
        .await
        .map_err(map_final_findings_error)?;
    }
    Ok(targets.len())
}

pub async fn resolve_deleted_conversation_completion_attentions_txn<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
) -> Result<usize, CompletionEvidenceError> {
    let workflows = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::ParentConversationId.eq(parent_conversation_id))
        .all(conn)
        .await
        .map_err(db_error)?;
    let mut resolved = 0;
    for workflow in workflows {
        resolved += resolve_workflow_completion_attentions(
            conn,
            &workflow.workflow_id,
            CompletionAttentionResolutionCode::WorkflowDeleted,
        )
        .await?;
    }
    Ok(resolved)
}

async fn reconcile_deleted_root_attention<C: ConnectionTrait>(
    conn: &C,
    row: &delegation_attention_request::Model,
) -> Result<bool, CompletionEvidenceError> {
    let workflow_id = match row.kind {
        AttentionKind::CompletionDecision | AttentionKind::CompletionArtifactRecovery => {
            delegation_workflow_run_binding::Entity::find_by_id(&row.task_id)
                .one(conn)
                .await
                .map_err(db_error)?
                .map(|binding| binding.workflow_id)
        }
        AttentionKind::DesignSelfReviewDecision => {
            delegation_workflow_design_root_binding::Entity::find()
                .filter(delegation_workflow_design_root_binding::Column::TaskId.eq(&row.task_id))
                .one(conn)
                .await
                .map_err(db_error)?
                .map(|binding| binding.workflow_id)
        }
        AttentionKind::ChildQuestion => None,
    };
    let Some(workflow_id) = workflow_id else {
        return Ok(false);
    };
    let Some(workflow) = delegation_workflow::Entity::find_by_id(&workflow_id)
        .one(conn)
        .await
        .map_err(db_error)?
    else {
        return Ok(false);
    };
    let owner_is_deleted = conversation::Entity::find_by_id(workflow.parent_conversation_id)
        .one(conn)
        .await
        .map_err(db_error)?
        .is_some_and(|owner| owner.deleted_at.is_some());
    if !owner_is_deleted {
        return Ok(false);
    }
    let graph_revision = bump_graph_revision(conn, &workflow_id, Utc::now())
        .await
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    resolve_completion_attention_row(
        conn,
        row,
        CompletionAttentionResolutionCode::WorkflowDeleted,
        graph_revision,
    )
    .await?;
    Ok(true)
}

async fn reconcile_terminal_attention<C: ConnectionTrait>(
    conn: &C,
    row: &delegation_attention_request::Model,
) -> Result<CompletionAttentionReconcileOutcome, CompletionEvidenceError> {
    let seed = match load_terminal(conn, &row.task_id).await {
        Ok(loaded) => loaded,
        Err(CompletionEvidenceError::InvalidTerminalState(_)) => {
            supersede_orphaned_attention(conn, row).await?;
            return Ok(CompletionAttentionReconcileOutcome::Superseded);
        }
        Err(error) => return Err(error),
    };
    let Some(loaded) = load_latest_unresolved_terminal_subject(conn, &seed).await? else {
        let graph_revision =
            bump_completion_graph(conn, &seed, "completion_decision_superseded").await?;
        resolve_lifecycle_attention(
            conn,
            &attention_cas(row)?,
            CompletionAttentionResolutionCode::Superseded,
            graph_revision,
        )
        .await
        .map_err(completion_mutation_error)?;
        return Ok(CompletionAttentionReconcileOutcome::Superseded);
    };
    let expected_state = match row.kind {
        AttentionKind::CompletionDecision => CompletionState::NeedsDecision,
        AttentionKind::CompletionArtifactRecovery => CompletionState::ArtifactRecovery,
        _ => {
            return Err(CompletionEvidenceError::InvalidAttention(
                "invalid terminal attention kind".into(),
            ))
        }
    };
    if loaded.run.completion_state != Some(expected_state.clone()) {
        let graph_revision =
            bump_completion_graph(conn, &loaded, "completion_decision_superseded").await?;
        resolve_lifecycle_attention(
            conn,
            &attention_cas(row)?,
            CompletionAttentionResolutionCode::Superseded,
            graph_revision,
        )
        .await
        .map_err(completion_mutation_error)?;
        return Ok(CompletionAttentionReconcileOutcome::Superseded);
    }
    let context = rebuild_completion_context(conn, &loaded).await?;
    let decision_payload = if row.kind == AttentionKind::CompletionDecision {
        parse_attention_payload::<CompletionDecisionPayloadV1>(row).ok()
    } else {
        None
    };
    let artifact_payload = if row.kind == AttentionKind::CompletionArtifactRecovery {
        parse_attention_payload::<ArtifactRecoveryPayloadV1>(row).ok()
    } else {
        None
    };
    let payload_is_current = match row.kind {
        AttentionKind::CompletionDecision => decision_payload.as_ref().is_some_and(|payload| {
            payload.version == 1
                && payload.role == context.scope_role.completion_role()
                && payload.legal_outcomes == legal_outcomes(context.scope_role.completion_role())
        }),
        AttentionKind::CompletionArtifactRecovery => {
            artifact_payload.as_ref().is_some_and(|payload| {
                payload.version == 1
                    && payload.producer_task_id == loaded.run.task_id
                    && payload.producer_generation == loaded.run.generation
                    && payload.producer_scope_digest == context.evidence_scope_digest
                    && payload.producer_baseline_head
                        == loaded
                            .binding
                            .producer_baseline_head
                            .clone()
                            .unwrap_or_default()
            })
        }
        _ => false,
    };
    let current = loaded.run.status == DelegationRunStatus::Completed
        && loaded.run.completion_state == Some(expected_state.clone())
        && loaded.run.parent_conversation_id == row.parent_conversation_id
        && row.latest_run_id.as_deref() == Some(loaded.run.task_id.as_str())
        && row.node_id.as_deref() == Some(loaded.node.node_id.as_str())
        && row.captured_scope_digest.as_deref() == Some(context.evidence_scope_digest.as_str())
        && ensure_context_matches_binding(&context, &loaded.binding).is_ok()
        && payload_is_current;
    if current {
        return Ok(CompletionAttentionReconcileOutcome::Retained);
    }

    let request = attention_cas(row)?;
    let graph_revision =
        bump_completion_graph(conn, &loaded, "completion_decision_superseded").await?;
    resolve_lifecycle_attention(
        conn,
        &request,
        CompletionAttentionResolutionCode::Superseded,
        graph_revision,
    )
    .await
    .map_err(completion_mutation_error)?;

    if loaded.run.task_id != row.task_id
        && delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(&loaded.run.task_id))
            .filter(delegation_attention_request::Column::Kind.eq(row.kind.clone()))
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .one(conn)
            .await
            .map_err(db_error)?
            .is_some()
    {
        return Ok(CompletionAttentionReconcileOutcome::Superseded);
    }
    if loaded.run.status != DelegationRunStatus::Completed
        || loaded.run.completion_state != Some(expected_state)
    {
        return Ok(CompletionAttentionReconcileOutcome::Superseded);
    }
    match row.kind {
        AttentionKind::CompletionDecision => {
            let Some(payload) = decision_payload else {
                return Ok(CompletionAttentionReconcileOutcome::Superseded);
            };
            open_completion_decision(
                conn,
                &loaded,
                &context,
                payload.reason_code,
                payload.bounded_candidates,
                payload.diagnostics,
            )
            .await?;
        }
        AttentionKind::CompletionArtifactRecovery => {
            let Some(payload) = artifact_payload else {
                return Ok(CompletionAttentionReconcileOutcome::Superseded);
            };
            open_artifact_recovery(
                conn,
                &loaded,
                &context,
                payload.normalized_intent,
                payload.source_audit_ref,
                payload.resolver_failure,
                None,
            )
            .await?;
        }
        _ => unreachable!("terminal kind checked above"),
    }
    Ok(CompletionAttentionReconcileOutcome::Reopened)
}

async fn load_latest_unresolved_terminal_subject<C: ConnectionTrait>(
    conn: &C,
    seed: &LoadedTerminal,
) -> Result<Option<LoadedTerminal>, CompletionEvidenceError> {
    let bindings = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(&seed.binding.workflow_id))
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(&seed.binding.node_id))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .order_by_desc(delegation_workflow_run_binding::Column::CreatedAt)
        .all(conn)
        .await
        .map_err(db_error)?;
    for binding in bindings {
        let run = delegation_task_run::Entity::find_by_id(&binding.task_id)
            .one(conn)
            .await
            .map_err(db_error)?;
        if run.is_some_and(|run| {
            run.status == DelegationRunStatus::Completed
                && matches!(
                    run.completion_state,
                    Some(CompletionState::NeedsDecision | CompletionState::ArtifactRecovery)
                )
        }) {
            return load_terminal(conn, &binding.task_id).await.map(Some);
        }
    }
    Ok(None)
}

async fn reconcile_design_attention<C: ConnectionTrait>(
    conn: &C,
    row: &delegation_attention_request::Model,
) -> Result<CompletionAttentionReconcileOutcome, CompletionEvidenceError> {
    let binding = delegation_workflow_design_root_binding::Entity::find()
        .filter(delegation_workflow_design_root_binding::Column::TaskId.eq(&row.task_id))
        .one(conn)
        .await
        .map_err(db_error)?;
    let Some(binding) = binding else {
        supersede_orphaned_attention(conn, row).await?;
        return Ok(CompletionAttentionReconcileOutcome::Superseded);
    };
    let workflow = delegation_workflow::Entity::find_by_id(&binding.workflow_id)
        .one(conn)
        .await
        .map_err(db_error)?;
    let Some(workflow) = workflow else {
        supersede_orphaned_attention(conn, row).await?;
        return Ok(CompletionAttentionReconcileOutcome::Superseded);
    };
    let payload = parse_attention_payload::<DesignSelfReviewPayloadV1>(row).ok();
    let current = payload.as_ref().is_some_and(|payload| {
        payload.version == 1
            && payload.design_identity == binding.design_identity
            && payload.gate_lineage == binding.gate_lineage
            && payload.legal_outcomes == legal_outcomes(CompletionRole::Reviewer)
            && workflow.parent_conversation_id == row.parent_conversation_id
            && row.latest_run_id.as_deref() == Some(binding.latest_run_id.as_str())
            && row.node_id.as_deref() == Some(binding.node_id.as_str())
            && row.captured_scope_digest.as_deref() == Some(binding.evidence_scope_digest.as_str())
    });
    if current {
        return Ok(CompletionAttentionReconcileOutcome::Retained);
    }

    let graph_revision = bump_graph_revision(conn, &workflow.workflow_id, Utc::now())
        .await
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    resolve_lifecycle_attention(
        conn,
        &attention_cas(row)?,
        CompletionAttentionResolutionCode::Superseded,
        graph_revision,
    )
    .await
    .map_err(completion_mutation_error)?;
    open_design_self_review_decision_txn(conn, &binding, row.parent_conversation_id)
        .await
        .map_err(completion_mutation_error)?;
    Ok(CompletionAttentionReconcileOutcome::Reopened)
}

async fn supersede_orphaned_attention<C: ConnectionTrait>(
    conn: &C,
    row: &delegation_attention_request::Model,
) -> Result<(), CompletionEvidenceError> {
    resolve_completion_attention_row(conn, row, CompletionAttentionResolutionCode::Superseded, 0)
        .await
}

async fn resolve_completion_attention_row<C: ConnectionTrait>(
    conn: &C,
    row: &delegation_attention_request::Model,
    code: CompletionAttentionResolutionCode,
    graph_revision: u64,
) -> Result<(), CompletionEvidenceError> {
    let resolution_json = serde_json::to_string(&json!({
        "version": 1,
        "code": code.as_str(),
        "graph_revision": graph_revision,
    }))
    .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    let result = delegation_attention_request::Entity::update_many()
        .col_expr(
            delegation_attention_request::Column::Status,
            sea_orm::sea_query::Expr::value("resolved"),
        )
        .col_expr(
            delegation_attention_request::Column::ResolutionCode,
            sea_orm::sea_query::Expr::value(Some(code.as_str().to_string())),
        )
        .col_expr(
            delegation_attention_request::Column::ResolutionJson,
            sea_orm::sea_query::Expr::value(Some(resolution_json)),
        )
        .col_expr(
            delegation_attention_request::Column::ResolvedAt,
            sea_orm::sea_query::Expr::value(Some(Utc::now())),
        )
        .filter(delegation_attention_request::Column::RequestId.eq(&row.request_id))
        .filter(delegation_attention_request::Column::Status.eq("open"))
        .exec(conn)
        .await
        .map_err(db_error)?;
    if result.rows_affected == 1 {
        Ok(())
    } else {
        Err(CompletionEvidenceError::DecisionSuperseded)
    }
}

fn completion_mutation_error(error: CompletionMutationError) -> CompletionEvidenceError {
    match error {
        CompletionMutationError::Evidence(error) => error,
        CompletionMutationError::Protocol { code, message } => {
            CompletionEvidenceError::Protocol { code, message }
        }
        error => CompletionEvidenceError::InvalidAttention(error.to_string()),
    }
}

pub async fn resolve_completion_decision_txn(
    db: &AppDatabase,
    parent_conversation_id: i32,
    request: CompletionAttentionCas,
    outcome: CompletionOutcome,
    actor_identity: &str,
) -> Result<CompletionMutationResult, CompletionMutationError> {
    let actor_identity = actor_identity.to_string();
    let result = db
        .conn
        .transaction::<_, DecisionTxnOutcome, CompletionMutationError>(|txn| {
            let request = request.clone();
            let actor_identity = actor_identity.clone();
            Box::pin(async move {
                resolve_completion_decision_once(
                    txn,
                    parent_conversation_id,
                    &request,
                    outcome,
                    &actor_identity,
                )
                .await
            })
        })
        .await;
    let mut result = match result {
        Ok(DecisionTxnOutcome::Resolved(result)) => *result,
        Ok(DecisionTxnOutcome::Superseded) => return Err(CompletionMutationError::Superseded),
        Err(sea_orm::TransactionError::Connection(error)) => {
            return Err(CompletionMutationError::Evidence(
                CompletionEvidenceError::Persistence(error.to_string()),
            ))
        }
        Err(sea_orm::TransactionError::Transaction(error)) => return Err(error),
    };
    attach_durable_completion_projection(db, &mut result).await?;
    Ok(result)
}

async fn resolve_completion_decision_once<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
    request: &CompletionAttentionCas,
    outcome: CompletionOutcome,
    actor_identity: &str,
) -> Result<DecisionTxnOutcome, CompletionMutationError> {
    let attention = load_mutation_attention(conn, &request.attention_id).await?;
    require_attention_kind(&attention, request, AttentionKind::CompletionDecision)?;
    require_attention_owner(&attention, parent_conversation_id)?;
    require_task_v2_completion_mutation(conn, &attention.task_id).await?;
    if attention.status == "resolved" {
        return replay_user_outcome(conn, &attention, request, outcome).await;
    }
    if !attention_cas_fields_match(&attention, request) {
        return Err(CompletionMutationError::Superseded);
    }
    validate_attention_cas(&attention, request)?;
    let payload: CompletionDecisionPayloadV1 = parse_attention_payload(&attention)?;
    if payload.version != 1
        || !payload.legal_outcomes.contains(&outcome)
        || !payload.role.accepts(outcome)
    {
        return Err(CompletionMutationError::RoleMismatch);
    }

    let loaded = load_terminal(conn, &request.task_id).await?;
    let context = rebuild_completion_context(conn, &loaded).await?;
    if ensure_context_matches_binding(&context, &loaded.binding).is_err()
        || context.evidence_scope_digest != request.captured_scope_digest
        || loaded.run.task_id != request.latest_run_id
        || completion_attention_public_node_id(&loaded.node.node_id) != request.node_id
        || context.scope_role.completion_role() != payload.role
    {
        supersede_completion_attention(conn, &loaded, request).await?;
        return Ok(DecisionTxnOutcome::Superseded);
    }
    let intent = CompletionIntent {
        outcome,
        summary: None,
        report_file: None,
        source: CompletionIntentSource::UserAdjudication,
    };
    match resolve_terminal_artifact(conn, &loaded, &context, outcome).await {
        Ok(artifact) => {
            let evidence =
                persist_evidence_state(conn, &loaded, &context, intent, artifact, None, Utc::now())
                    .await?;
            let graph_revision = enqueue_completion_decision_resolved(
                conn,
                &loaded.workflow,
                &loaded.run.task_id,
                &loaded.node.node_id,
                AttentionKind::CompletionDecision,
                outcome,
                &context.evidence_scope_digest,
            )
            .await?;
            resolve_user_outcome_attention(conn, request, outcome, actor_identity, graph_revision)
                .await?;
            Ok(DecisionTxnOutcome::Resolved(Box::new(
                CompletionMutationResult {
                    workflow_id: loaded.workflow.workflow_id,
                    task_id: loaded.run.task_id,
                    node_id: completion_attention_public_node_id(&loaded.node.node_id),
                    kind: AttentionKind::CompletionDecision,
                    outcome,
                    evidence_scope_digest: context.evidence_scope_digest,
                    graph_revision,
                    idempotent_replay: false,
                    completion: Some(super::project_terminal_completion(
                        &TerminalCompletionResult {
                            state: CompletionState::Resolved,
                            evidence: Some(evidence),
                            attention: None,
                            graph_revision,
                            final_metric_states: Vec::new(),
                        },
                    )),
                },
            )))
        }
        Err(ArtifactError::Unavailable(failure)) => {
            let mut completion = open_artifact_recovery(
                conn,
                &loaded,
                &context,
                intent,
                CompletionSourceAuditRef::UserAdjudication,
                failure,
                None,
            )
            .await?;
            let graph_revision = enqueue_completion_decision_resolved(
                conn,
                &loaded.workflow,
                &loaded.run.task_id,
                &loaded.node.node_id,
                AttentionKind::CompletionDecision,
                outcome,
                &context.evidence_scope_digest,
            )
            .await?;
            completion.graph_revision = graph_revision;
            resolve_user_outcome_attention(conn, request, outcome, actor_identity, graph_revision)
                .await?;
            Ok(DecisionTxnOutcome::Resolved(Box::new(
                CompletionMutationResult {
                    workflow_id: loaded.workflow.workflow_id,
                    task_id: loaded.run.task_id,
                    node_id: completion_attention_public_node_id(&loaded.node.node_id),
                    kind: AttentionKind::CompletionDecision,
                    outcome,
                    evidence_scope_digest: context.evidence_scope_digest,
                    graph_revision,
                    idempotent_replay: false,
                    completion: Some(super::project_terminal_completion(&completion)),
                },
            )))
        }
        Err(error) => Err(CompletionEvidenceError::Artifact(error).into()),
    }
}

pub async fn open_design_self_review_decision_txn<C: ConnectionTrait>(
    conn: &C,
    binding: &delegation_workflow_design_root_binding::Model,
    parent_conversation_id: i32,
) -> Result<CompletionAttentionCas, CompletionMutationError> {
    require_workflow_v2_completion_mutation(conn, &binding.workflow_id).await?;
    let payload = DesignSelfReviewPayloadV1 {
        version: 1,
        design_identity: binding.design_identity.clone(),
        gate_lineage: binding.gate_lineage.clone(),
        legal_outcomes: legal_outcomes(CompletionRole::Reviewer),
    };
    let payload_json = serde_json::to_string(&payload).map_err(|error| {
        CompletionMutationError::Evidence(CompletionEvidenceError::Persistence(error.to_string()))
    })?;
    let row = open_design_self_review_attention_txn(
        conn,
        &DesignSelfReviewAttentionInput {
            binding,
            parent_conversation_id,
            message: DESIGN_SELF_REVIEW_MESSAGE,
            payload_json: &payload_json,
            created_at: Utc::now(),
        },
    )
    .await
    .map_err(|error| CompletionMutationError::InvalidAttention(error.to_string()))?;
    attention_cas(&row).map_err(Into::into)
}

pub async fn resolve_design_self_review_txn(
    db: &AppDatabase,
    parent_conversation_id: i32,
    request: CompletionAttentionCas,
    outcome: CompletionOutcome,
    actor_identity: &str,
) -> Result<CompletionMutationResult, CompletionMutationError> {
    let actor_identity = actor_identity.to_string();
    let result = db
        .conn
        .transaction::<_, DecisionTxnOutcome, CompletionMutationError>(|txn| {
            let request = request.clone();
            let actor_identity = actor_identity.clone();
            Box::pin(async move {
                let attention = load_mutation_attention(txn, &request.attention_id).await?;
                require_attention_kind(
                    &attention,
                    &request,
                    AttentionKind::DesignSelfReviewDecision,
                )?;
                require_attention_owner(&attention, parent_conversation_id)?;
                let binding = delegation_workflow_design_root_binding::Entity::find()
                    .filter(
                        delegation_workflow_design_root_binding::Column::TaskId
                            .eq(&attention.task_id),
                    )
                    .one(txn)
                    .await
                    .map_err(db_error)?
                    .ok_or(CompletionMutationError::Superseded)?;
                require_workflow_v2_completion_mutation(txn, &binding.workflow_id).await?;
                if attention.status == "resolved" {
                    return replay_user_outcome(txn, &attention, &request, outcome).await;
                }
                if !attention_cas_fields_match(&attention, &request) {
                    return Err(CompletionMutationError::Superseded);
                }
                validate_attention_cas(&attention, &request)?;
                let payload: DesignSelfReviewPayloadV1 = parse_attention_payload(&attention)?;
                if payload.version != 1
                    || !payload.legal_outcomes.contains(&outcome)
                    || !CompletionRole::Reviewer.accepts(outcome)
                {
                    return Err(CompletionMutationError::RoleMismatch);
                }
                let workflow = delegation_workflow::Entity::find_by_id(&binding.workflow_id)
                    .one(txn)
                    .await
                    .map_err(db_error)?
                    .ok_or(CompletionMutationError::Superseded)?;
                if workflow.parent_conversation_id != parent_conversation_id {
                    return Err(CompletionMutationError::Unauthorized);
                }
                if binding.task_id != request.task_id
                    || binding.latest_run_id != request.latest_run_id
                    || completion_attention_public_node_id(&binding.node_id) != request.node_id
                    || binding.evidence_scope_digest != request.captured_scope_digest
                    || binding.design_identity != payload.design_identity
                    || binding.gate_lineage != payload.gate_lineage
                {
                    let graph_revision =
                        bump_graph_revision(txn, &workflow.workflow_id, Utc::now())
                            .await
                            .map_err(|error| {
                                CompletionMutationError::Evidence(
                                    CompletionEvidenceError::Persistence(error.to_string()),
                                )
                            })?;
                    resolve_lifecycle_attention(
                        txn,
                        &request,
                        CompletionAttentionResolutionCode::Superseded,
                        graph_revision,
                    )
                    .await?;
                    return Ok(DecisionTxnOutcome::Superseded);
                }
                let graph_revision = enqueue_completion_decision_resolved(
                    txn,
                    &workflow,
                    &binding.task_id,
                    &binding.node_id,
                    AttentionKind::DesignSelfReviewDecision,
                    outcome,
                    &binding.evidence_scope_digest,
                )
                .await?;
                resolve_user_outcome_attention(
                    txn,
                    &request,
                    outcome,
                    &actor_identity,
                    graph_revision,
                )
                .await?;
                Ok(DecisionTxnOutcome::Resolved(Box::new(
                    CompletionMutationResult {
                        workflow_id: workflow.workflow_id,
                        task_id: binding.task_id,
                        node_id: completion_attention_public_node_id(&binding.node_id),
                        kind: AttentionKind::DesignSelfReviewDecision,
                        outcome,
                        evidence_scope_digest: binding.evidence_scope_digest,
                        graph_revision,
                        idempotent_replay: false,
                        completion: None,
                    },
                )))
            })
        })
        .await;
    let mut result = match result {
        Ok(DecisionTxnOutcome::Resolved(result)) => *result,
        Ok(DecisionTxnOutcome::Superseded) => return Err(CompletionMutationError::Superseded),
        Err(sea_orm::TransactionError::Connection(error)) => {
            return Err(CompletionMutationError::Evidence(
                CompletionEvidenceError::Persistence(error.to_string()),
            ))
        }
        Err(sea_orm::TransactionError::Transaction(error)) => return Err(error),
    };
    attach_durable_completion_projection(db, &mut result).await?;
    Ok(result)
}

async fn attach_durable_completion_projection(
    db: &AppDatabase,
    result: &mut CompletionMutationResult,
) -> Result<(), CompletionMutationError> {
    result.completion = super::load_completion_projection(&db.conn, &result.task_id)
        .await
        .map_err(CompletionMutationError::Evidence)?;
    if result.completion.is_none() {
        return Err(CompletionMutationError::InvalidAttention(
            "committed completion projection is missing".into(),
        ));
    }
    Ok(())
}

async fn load_mutation_attention<C: ConnectionTrait>(
    conn: &C,
    attention_id: &str,
) -> Result<delegation_attention_request::Model, CompletionMutationError> {
    delegation_attention_request::Entity::find_by_id(attention_id)
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or(CompletionMutationError::Superseded)
}

fn require_attention_kind(
    row: &delegation_attention_request::Model,
    request: &CompletionAttentionCas,
    expected: AttentionKind,
) -> Result<(), CompletionMutationError> {
    if request.kind == expected && row.kind == expected {
        Ok(())
    } else {
        Err(CompletionMutationError::KindMismatch)
    }
}

fn require_attention_owner(
    row: &delegation_attention_request::Model,
    parent_conversation_id: i32,
) -> Result<(), CompletionMutationError> {
    if row.parent_conversation_id == parent_conversation_id {
        Ok(())
    } else {
        Err(CompletionMutationError::Unauthorized)
    }
}

fn parse_attention_payload<T: serde::de::DeserializeOwned>(
    row: &delegation_attention_request::Model,
) -> Result<T, CompletionMutationError> {
    let json = row.payload_json.as_deref().ok_or_else(|| {
        CompletionMutationError::InvalidAttention("typed attention payload is missing".into())
    })?;
    serde_json::from_str(json)
        .map_err(|error| CompletionMutationError::InvalidAttention(error.to_string()))
}

async fn replay_user_outcome<C: ConnectionTrait>(
    conn: &C,
    row: &delegation_attention_request::Model,
    request: &CompletionAttentionCas,
    outcome: CompletionOutcome,
) -> Result<DecisionTxnOutcome, CompletionMutationError> {
    if !attention_cas_fields_match(row, request) {
        return Err(CompletionMutationError::Superseded);
    }
    match row.resolution_code.as_deref() {
        Some("superseded" | "workflow_terminated" | "workflow_deleted") => {
            return Err(CompletionMutationError::Superseded)
        }
        Some("user_outcome_committed") => {}
        _ => return Err(CompletionMutationError::Conflict),
    }
    let resolution: UserOutcomeResolutionV1 = row
        .resolution_json
        .as_deref()
        .ok_or_else(|| {
            CompletionMutationError::InvalidAttention(
                "committed attention resolution is missing".into(),
            )
        })
        .and_then(|json| {
            serde_json::from_str(json)
                .map_err(|error| CompletionMutationError::InvalidAttention(error.to_string()))
        })?;
    if resolution.version != 1
        || resolution.code != CompletionAttentionResolutionCode::UserOutcomeCommitted.as_str()
        || resolution.outcome != outcome
        || resolution.committed_scope_digest != request.captured_scope_digest
    {
        return Err(CompletionMutationError::Conflict);
    }

    let (workflow_id, node_id, completion) = match row.kind {
        AttentionKind::CompletionDecision => {
            let loaded = load_terminal(conn, &request.task_id).await?;
            let mut completion = existing_result(conn, &loaded).await?;
            if let Some(value) = completion.as_mut() {
                value.graph_revision = resolution.graph_revision;
            }
            (
                loaded.workflow.workflow_id,
                completion_attention_public_node_id(&loaded.node.node_id),
                completion,
            )
        }
        AttentionKind::DesignSelfReviewDecision => {
            let binding = delegation_workflow_design_root_binding::Entity::find()
                .filter(
                    delegation_workflow_design_root_binding::Column::TaskId.eq(&request.task_id),
                )
                .one(conn)
                .await
                .map_err(db_error)?
                .ok_or(CompletionMutationError::Superseded)?;
            (
                binding.workflow_id,
                completion_attention_public_node_id(&binding.node_id),
                None,
            )
        }
        _ => return Err(CompletionMutationError::KindMismatch),
    };
    Ok(DecisionTxnOutcome::Resolved(Box::new(
        CompletionMutationResult {
            workflow_id,
            task_id: request.task_id.clone(),
            node_id,
            kind: row.kind.clone(),
            outcome,
            evidence_scope_digest: request.captured_scope_digest.clone(),
            graph_revision: resolution.graph_revision,
            idempotent_replay: true,
            completion: completion.as_ref().map(super::project_terminal_completion),
        },
    )))
}

async fn resolve_user_outcome_attention<C: ConnectionTrait>(
    conn: &C,
    request: &CompletionAttentionCas,
    outcome: CompletionOutcome,
    actor_identity: &str,
    graph_revision: u64,
) -> Result<(), CompletionMutationError> {
    let resolution_json = serde_json::to_string(&UserOutcomeResolutionV1 {
        version: 1,
        code: CompletionAttentionResolutionCode::UserOutcomeCommitted
            .as_str()
            .into(),
        outcome,
        actor_identity: actor_identity.to_string(),
        committed_scope_digest: request.captured_scope_digest.clone(),
        graph_revision,
    })
    .map_err(|error| {
        CompletionMutationError::Evidence(CompletionEvidenceError::Persistence(error.to_string()))
    })?;
    resolve_attention_txn(
        conn,
        request,
        CompletionAttentionResolutionCode::UserOutcomeCommitted.as_str(),
        Some(resolution_json),
    )
    .await
    .map_err(Into::into)
}

async fn resolve_lifecycle_attention<C: ConnectionTrait>(
    conn: &C,
    request: &CompletionAttentionCas,
    code: CompletionAttentionResolutionCode,
    graph_revision: u64,
) -> Result<(), CompletionMutationError> {
    let resolution_json = serde_json::to_string(&json!({
        "version": 1,
        "code": code.as_str(),
        "graph_revision": graph_revision,
    }))
    .map_err(|error| {
        CompletionMutationError::Evidence(CompletionEvidenceError::Persistence(error.to_string()))
    })?;
    resolve_attention_txn(conn, request, code.as_str(), Some(resolution_json))
        .await
        .map_err(Into::into)
}

async fn supersede_completion_attention<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    request: &CompletionAttentionCas,
) -> Result<(), CompletionMutationError> {
    let graph_revision =
        bump_completion_graph(conn, loaded, "completion_decision_superseded").await?;
    resolve_lifecycle_attention(
        conn,
        request,
        CompletionAttentionResolutionCode::Superseded,
        graph_revision,
    )
    .await
}

async fn enqueue_completion_decision_resolved<C: ConnectionTrait>(
    conn: &C,
    workflow: &delegation_workflow::Model,
    task_id: &str,
    node_id: &str,
    kind: AttentionKind,
    outcome: CompletionOutcome,
    evidence_scope_digest: &str,
) -> Result<u64, CompletionEvidenceError> {
    let graph_revision = bump_graph_revision(conn, &workflow.workflow_id, Utc::now())
        .await
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    let event_id = uuid::Uuid::new_v4().to_string();
    let payload_json = serde_json::to_string(&CompletionDecisionResolvedPayloadV1 {
        version: 1,
        event_id: event_id.clone(),
        workflow_id: workflow.workflow_id.clone(),
        task_id: task_id.to_string(),
        node_id: completion_attention_public_node_id(node_id),
        kind: kind.clone(),
        outcome,
        evidence_scope_digest: evidence_scope_digest.to_string(),
        graph_revision,
    })
    .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    delegation_workflow_outbox_event::ActiveModel {
        event_id: Set(event_id),
        workflow_id: Set(workflow.workflow_id.clone()),
        graph_revision: Set(i64::try_from(graph_revision).map_err(|_| {
            CompletionEvidenceError::Persistence("graph revision exceeds i64".into())
        })?),
        event_kind: Set(COMPLETION_DECISION_RESOLVED_EVENT.into()),
        subject_key: Set(format!("{}:{task_id}", attention_kind_label(&kind))),
        payload_json: Set(payload_json),
        dispatch_attempts: Set(0),
        created_at: Set(Utc::now()),
        delivered_at: Set(None),
    }
    .insert(conn)
    .await
    .map_err(db_error)?;
    Ok(graph_revision)
}

fn attention_kind_label(kind: &AttentionKind) -> &'static str {
    match kind {
        AttentionKind::ChildQuestion => "child_question",
        AttentionKind::CompletionDecision => "completion_decision",
        AttentionKind::CompletionArtifactRecovery => "completion_artifact_recovery",
        AttentionKind::DesignSelfReviewDecision => "design_self_review_decision",
    }
}

pub async fn materialize_terminal_completion_txn<C: ConnectionTrait>(
    conn: &C,
    input: TerminalCompletionInput,
) -> Result<TerminalCompletionResult, CompletionEvidenceError> {
    if input.terminal_status != DelegationRunStatus::Completed {
        return Err(CompletionEvidenceError::InvalidTerminalState(
            "only completed runs can materialize semantic completion".into(),
        ));
    }
    let loaded = load_terminal(conn, &input.task_id).await?;
    validate_v2_terminal(&loaded, &input)?;
    if let Some(existing) = existing_result(conn, &loaded).await? {
        return Ok(existing);
    }

    let context = rebuild_completion_context(conn, &loaded).await?;
    ensure_context_matches_binding(&context, &loaded.binding)?;
    let tool_intents = load_tool_intents(conn, &input.task_id).await?;
    let final_assistant_text = input.final_assistant_text;
    let pre_read_reports = input.pre_read_reports;
    let resolution = resolve_completion_intent(&CompletionResolverInput {
        role: context.scope_role.completion_role(),
        tool_intents: tool_intents
            .iter()
            .map(|loaded| loaded.intent.clone())
            .collect(),
        final_assistant_text: final_assistant_text.clone(),
        report_candidates: pre_read_reports
            .iter()
            .map(|report| CompletionReportCandidate {
                path: report.path.clone(),
                contents: report.contents.clone(),
                summary: report.summary.clone(),
            })
            .collect(),
        touched_report_candidates: Vec::new(),
        report_read_failures: Vec::new(),
    });

    match resolution {
        CompletionResolution::NeedsDecision {
            reason_code,
            bounded_candidates,
            diagnostics,
        } => {
            open_completion_decision(
                conn,
                &loaded,
                &context,
                reason_code,
                bounded_candidates,
                diagnostics,
            )
            .await
        }
        CompletionResolution::Resolved(intent) => {
            let expected_graph_revision = u64::try_from(loaded.workflow.graph_revision)
                .ok()
                .and_then(|revision| revision.checked_add(1))
                .ok_or_else(|| {
                    CompletionEvidenceError::Persistence(
                        "workflow graph revision cannot advance".into(),
                    )
                })?;
            let final_evaluation = derive_final_findings_terminal_action(
                conn,
                &loaded,
                &context,
                &intent,
                &final_assistant_text,
                &pre_read_reports,
                expected_graph_revision,
            )
            .await?;
            let final_metric_states = final_metric_states(&final_evaluation);
            if let FinalFindingsTerminalAction::NeedsDecision { gate_id } = &final_evaluation.action
            {
                resolve_active_final_findings_packages_v1(
                    conn,
                    &loaded.workflow.workflow_id,
                    gate_id,
                    i64::try_from(expected_graph_revision).map_err(|_| {
                        CompletionEvidenceError::Persistence("graph revision exceeds i64".into())
                    })?,
                )
                .await
                .map_err(map_final_findings_error)?;
                let mut result = open_completion_decision(
                    conn,
                    &loaded,
                    &context,
                    CompletionIntentReason::RemediationContextRequired,
                    Vec::new(),
                    Vec::new(),
                )
                .await?;
                result.final_metric_states = final_metric_states;
                return Ok(result);
            }

            match resolve_terminal_artifact(conn, &loaded, &context, intent.outcome).await {
                Ok(artifact) => {
                    let evidence = persist_evidence_state(
                        conn,
                        &loaded,
                        &context,
                        intent,
                        artifact,
                        final_evaluation.current_contexts.as_deref(),
                        Utc::now(),
                    )
                    .await?;
                    let graph_revision =
                        bump_completion_graph(conn, &loaded, "completion_resolved").await?;
                    if graph_revision != expected_graph_revision {
                        return Err(CompletionEvidenceError::Persistence(
                            "Final findings graph revision changed during completion".into(),
                        ));
                    }
                    apply_final_findings_terminal_action(
                        conn,
                        &loaded,
                        final_evaluation.action,
                        graph_revision,
                    )
                    .await?;
                    Ok(TerminalCompletionResult {
                        state: CompletionState::Resolved,
                        evidence: Some(evidence),
                        attention: None,
                        graph_revision,
                        final_metric_states,
                    })
                }
                Err(ArtifactError::Unavailable(failure)) => {
                    let source_audit_ref = source_audit_ref(&tool_intents, &intent);
                    let result = open_artifact_recovery(
                        conn,
                        &loaded,
                        &context,
                        intent,
                        source_audit_ref,
                        failure,
                        final_evaluation.current_contexts.as_deref(),
                    )
                    .await?;
                    if result.graph_revision != expected_graph_revision {
                        return Err(CompletionEvidenceError::Persistence(
                            "Final findings graph revision changed during artifact recovery".into(),
                        ));
                    }
                    apply_final_findings_terminal_action(
                        conn,
                        &loaded,
                        final_evaluation.action,
                        result.graph_revision,
                    )
                    .await?;
                    Ok(TerminalCompletionResult {
                        final_metric_states,
                        ..result
                    })
                }
                Err(error) => Err(error.into()),
            }
        }
    }
}

fn final_metric_states(
    evaluation: &FinalFindingsTerminalEvaluation,
) -> Vec<CompletionFinalMetricState> {
    let mut states = Vec::new();
    if let Some(contexts) = evaluation.current_contexts.as_deref() {
        states.push(
            if contexts.iter().any(|context| {
                context.availability == RemediationContextAvailability::Available
                    && context.byte_len > 0
            }) {
                CompletionFinalMetricState::ContextAvailable
            } else {
                CompletionFinalMetricState::ContextMissing
            },
        );
    }
    let package_state = match &evaluation.action {
        FinalFindingsTerminalAction::NotFinal => None,
        FinalFindingsTerminalAction::Incomplete => {
            Some(CompletionFinalMetricState::PackageIncomplete)
        }
        FinalFindingsTerminalAction::Resolve { .. } => {
            Some(CompletionFinalMetricState::PackageResolved)
        }
        FinalFindingsTerminalAction::Persist(_) => {
            Some(CompletionFinalMetricState::PackagePersisted)
        }
        FinalFindingsTerminalAction::NeedsDecision { .. } => {
            Some(CompletionFinalMetricState::DecisionRequired)
        }
    };
    states.extend(package_state);
    states
}

async fn derive_final_findings_terminal_action<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    intent: &CompletionIntent,
    final_assistant_text: &str,
    pre_read_reports: &[ValidatedReportCandidate],
    graph_revision: u64,
) -> Result<FinalFindingsTerminalEvaluation, CompletionEvidenceError> {
    if context.scope_role != CompletionScopeRole::FinalReviewer {
        return Ok(FinalFindingsTerminalEvaluation {
            action: FinalFindingsTerminalAction::NotFinal,
            current_contexts: None,
        });
    }
    let gate_id = context.evidence_scope.gate_id.clone().ok_or_else(|| {
        CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer completion has no durable gate id".into(),
        )
    })?;
    let gate_lineage = context.evidence_scope.gate_lineage.clone().ok_or_else(|| {
        CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer completion has no durable gate lineage".into(),
        )
    })?;
    let review_round = context.evidence_scope.review_round.ok_or_else(|| {
        CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer completion has no durable review round".into(),
        )
    })?;
    let gate = delegation_workflow_gate_state::Entity::find_by_id((
        loaded.workflow.workflow_id.clone(),
        gate_id.clone(),
    ))
    .one(conn)
    .await
    .map_err(db_error)?
    .ok_or_else(|| {
        CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer completion has no current gate state".into(),
        )
    })?;
    if gate.gate_lineage != gate_lineage
        || u32::try_from(gate.current_review_round).ok() != Some(review_round)
    {
        return Err(CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer completion is stale for the current gate state".into(),
        ));
    }
    let selected: Vec<String> =
        serde_json::from_str(&gate.selected_node_ids_json).map_err(|_| {
            CompletionEvidenceError::EvidenceCorrupt("Final Reviewer selection is corrupt".into())
        })?;
    let unique = selected.iter().cloned().collect::<BTreeSet<_>>();
    if selected.is_empty()
        || unique.len() != selected.len()
        || selected.iter().any(|node_id| node_id.trim().is_empty())
    {
        return Err(CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer selection is invalid".into(),
        ));
    }

    let snapshot = super::store::load_active_manifest_snapshot(conn, &loaded.workflow)
        .await
        .map_err(|error| CompletionEvidenceError::EvidenceCorrupt(error.to_string()))?;
    let required_reviewer_node_ids = snapshot
        .normalized
        .nodes
        .iter()
        .filter(|node| {
            node.phase_id.as_deref() == Some(super::types::PHASE_FINAL)
                && node.role == Some(super::types::ManifestNodeRole::Reviewer)
                && node.required
        })
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if required_reviewer_node_ids.is_empty()
        || unique
            .iter()
            .any(|node_id| required_reviewer_node_ids.binary_search(node_id).is_err())
    {
        return Err(CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer selection is outside the active required cohort".into(),
        ));
    }
    let requirements_identity = context.requirements_identity.clone().ok_or_else(|| {
        CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer evaluation has no active requirements identity".into(),
        )
    })?;
    let mut target_work_unit_keys = Vec::new();
    let mut remediation_route_ids = Vec::new();
    for policy in &snapshot.normalized.task_policies {
        if let Some(work_unit_key) = snapshot
            .normalized
            .nodes
            .iter()
            .find(|node| node.id == policy.route.implementer_node_id)
            .and_then(|node| node.work_unit_key.clone())
        {
            target_work_unit_keys.push(work_unit_key);
        }
        remediation_route_ids.push(policy.route.implementer_node_id.clone());
        remediation_route_ids.extend(policy.route.reviewer_node_ids.clone());
    }

    let current_contexts = snapshot_current_final_contexts(
        &loaded.run.task_id,
        intent,
        final_assistant_text,
        pre_read_reports,
    )?;
    let mut findings = Vec::new();
    let mut reviewer_evaluations = Vec::new();
    let mut remediation_contexts = Vec::new();
    for node_id in required_reviewer_node_ids {
        let is_selected = unique.contains(&node_id);
        let mut binding_query = delegation_workflow_run_binding::Entity::find()
            .filter(
                delegation_workflow_run_binding::Column::WorkflowId
                    .eq(&loaded.workflow.workflow_id),
            )
            .filter(delegation_workflow_run_binding::Column::NodeId.eq(&node_id))
            .filter(delegation_workflow_run_binding::Column::GateId.eq(&gate_id))
            .filter(delegation_workflow_run_binding::Column::GateLineage.eq(&gate_lineage))
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal);
        if is_selected {
            binding_query = binding_query.filter(
                delegation_workflow_run_binding::Column::ReviewRound.eq(i64::from(review_round)),
            );
        }
        let binding = binding_query.one(conn).await.map_err(db_error)?;
        let Some(binding) = binding else {
            return Ok(FinalFindingsTerminalEvaluation {
                action: FinalFindingsTerminalAction::Incomplete,
                current_contexts: Some(current_contexts),
            });
        };
        if !is_selected
            && binding.review_round.is_none_or(|binding_round| {
                binding_round <= 0 || binding_round >= i64::from(review_round)
            })
        {
            return Err(CompletionEvidenceError::EvidenceCorrupt(
                "retained Final Reviewer evidence is not from an earlier round".into(),
            ));
        }
        let is_current = binding.task_id == loaded.run.task_id;
        let (reviewer_intent, evidence_scope_digest, reviewer_contexts) = if is_current {
            (
                intent.clone(),
                context.evidence_scope_digest.clone(),
                current_contexts.clone(),
            )
        } else {
            let run = delegation_task_run::Entity::find_by_id(binding.task_id.clone())
                .one(conn)
                .await
                .map_err(db_error)?;
            if run.as_ref().is_none_or(|run| {
                run.status != DelegationRunStatus::Completed
                    || run.completion_state != Some(CompletionState::Resolved)
            }) {
                return Ok(FinalFindingsTerminalEvaluation {
                    action: FinalFindingsTerminalAction::Incomplete,
                    current_contexts: Some(current_contexts),
                });
            }
            let validated = load_validated_completion_evidence(conn, &binding.task_id).await?;
            let reviewer_intent = validated.evidence.intent;
            let reviewer_contexts = load_final_context_snapshots(&run.unwrap(), &reviewer_intent)?;
            (
                reviewer_intent,
                validated.evidence.evidence_scope_digest,
                reviewer_contexts,
            )
        };
        if binding.requirements_identity.as_deref() != Some(requirements_identity.as_str()) {
            return Err(CompletionEvidenceError::EvidenceCorrupt(
                "Final Reviewer requirements identity changed within the evaluation".into(),
            ));
        }
        reviewer_evaluations.push(FinalReviewerEvaluationV1 {
            reviewer_node_id: node_id.clone(),
            evidence_task_id: binding.task_id.clone(),
            evidence_scope_digest: evidence_scope_digest.clone(),
            outcome: reviewer_intent.outcome,
        });
        if !matches!(
            reviewer_intent.outcome,
            CompletionOutcome::RequestChanges | CompletionOutcome::Block
        ) {
            continue;
        }
        findings.push(FinalFindingInputV1 {
            reviewer_node_id: node_id,
            evidence_task_id: binding.task_id.clone(),
            evidence_scope_digest,
            outcome: reviewer_intent.outcome,
            target_work_unit_keys: target_work_unit_keys.clone(),
            remediation_route_ids: remediation_route_ids.clone(),
        });
        remediation_contexts.extend(
            remediation_context_inputs_from_snapshots_v1(&reviewer_contexts)
                .map_err(map_final_findings_error)?,
        );
    }

    if findings.is_empty() {
        return Ok(FinalFindingsTerminalEvaluation {
            action: FinalFindingsTerminalAction::Resolve { gate_id },
            current_contexts: Some(current_contexts),
        });
    }
    let action = match build_final_findings_package_v1(FinalFindingsPackageInputV1 {
        workflow_id: loaded.workflow.workflow_id.clone(),
        gate_id,
        gate_lineage,
        requirements_identity,
        graph_revision,
        reviewer_evaluations,
        findings,
        remediation_contexts,
    }) {
        Ok(package) => FinalFindingsTerminalAction::Persist(package),
        Err(FinalFindingsError::RemediationContextRequired) => {
            FinalFindingsTerminalAction::NeedsDecision {
                gate_id: gate.gate_id,
            }
        }
        Err(error) => return Err(map_final_findings_error(error)),
    };
    Ok(FinalFindingsTerminalEvaluation {
        action,
        current_contexts: Some(current_contexts),
    })
}

fn snapshot_current_final_contexts(
    task_id: &str,
    intent: &CompletionIntent,
    final_assistant_text: &str,
    pre_read_reports: &[ValidatedReportCandidate],
) -> Result<Vec<RemediationContextSnapshotV1>, CompletionEvidenceError> {
    if !matches!(
        intent.outcome,
        CompletionOutcome::RequestChanges | CompletionOutcome::Block
    ) {
        return Ok(Vec::new());
    }
    let mut inputs = pre_read_reports
        .iter()
        .map(|report| {
            RemediationContextInputV1::available_report(
                task_id,
                &report.path,
                report.contents.as_bytes().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    if pre_read_reports.is_empty() {
        if let Some(report_file) = intent.report_file.as_deref() {
            inputs.push(RemediationContextInputV1::missing_report(
                task_id,
                report_file,
            ));
        }
    }
    let has_available = inputs.iter().any(|context| {
        context
            .bytes
            .as_ref()
            .is_some_and(|bytes| !bytes.is_empty())
    });
    if !has_available {
        inputs.push(if final_assistant_text.is_empty() {
            RemediationContextInputV1::missing_terminal(task_id)
        } else {
            bounded_terminal_context_v1(task_id, final_assistant_text.as_bytes())
        });
    }
    snapshot_remediation_contexts_v1(inputs).map_err(map_final_findings_error)
}

fn load_final_context_snapshots(
    run: &delegation_task_run::Model,
    intent: &CompletionIntent,
) -> Result<Vec<RemediationContextSnapshotV1>, CompletionEvidenceError> {
    let Some(json) = run.final_remediation_contexts_json.as_deref() else {
        let mut legacy = Vec::new();
        if let Some(report_file) = intent.report_file.as_deref() {
            legacy.push(RemediationContextInputV1::missing_report(
                &run.task_id,
                report_file,
            ));
        }
        legacy.push(RemediationContextInputV1::missing_terminal(&run.task_id));
        return snapshot_remediation_contexts_v1(legacy).map_err(map_final_findings_error);
    };
    let contexts: Vec<RemediationContextSnapshotV1> = serde_json::from_str(json).map_err(|_| {
        CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer remediation snapshot is corrupt".into(),
        )
    })?;
    verify_remediation_context_snapshots_v1(&contexts).map_err(map_final_findings_error)?;
    if contexts
        .iter()
        .any(|context| context.source_evidence_task_id != run.task_id)
    {
        return Err(CompletionEvidenceError::EvidenceCorrupt(
            "Final Reviewer remediation snapshot belongs to another task".into(),
        ));
    }
    Ok(contexts)
}

async fn apply_final_findings_terminal_action<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    action: FinalFindingsTerminalAction,
    graph_revision: u64,
) -> Result<(), CompletionEvidenceError> {
    let graph_revision = i64::try_from(graph_revision)
        .map_err(|_| CompletionEvidenceError::Persistence("graph revision exceeds i64".into()))?;
    match action {
        FinalFindingsTerminalAction::Persist(package) => {
            persist_final_findings_package_v1(conn, &package, graph_revision)
                .await
                .map_err(map_final_findings_error)?;
        }
        FinalFindingsTerminalAction::Incomplete => {}
        FinalFindingsTerminalAction::Resolve { gate_id } => {
            resolve_active_final_findings_packages_v1(
                conn,
                &loaded.workflow.workflow_id,
                &gate_id,
                graph_revision,
            )
            .await
            .map_err(map_final_findings_error)?;
        }
        FinalFindingsTerminalAction::NotFinal => {}
        FinalFindingsTerminalAction::NeedsDecision { .. } => unreachable!(),
    }
    Ok(())
}

fn map_final_findings_error(error: FinalFindingsError) -> CompletionEvidenceError {
    match error {
        FinalFindingsError::Persistence(message) => CompletionEvidenceError::Persistence(message),
        FinalFindingsError::RemediationContextRequired => {
            CompletionEvidenceError::EvidenceCorrupt(error.to_string())
        }
        FinalFindingsError::InvalidField(_)
        | FinalFindingsError::BoundsExceeded(_)
        | FinalFindingsError::EvidenceCorrupt => {
            CompletionEvidenceError::EvidenceCorrupt(error.to_string())
        }
    }
}

/// Execute a typed artifact retry as its own atomic transaction. Returning a
/// superseded error happens only after the stale attention resolution commits.
pub async fn retry_completion_artifact_txn(
    db: &AppDatabase,
    request: CompletionAttentionCas,
) -> Result<TerminalCompletionResult, CompletionEvidenceError> {
    let outcome = db
        .conn
        .transaction::<_, RetryTxnOutcome, CompletionEvidenceError>(|txn| {
            let request = request.clone();
            Box::pin(async move { retry_completion_artifact_once(txn, &request).await })
        })
        .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(sea_orm::TransactionError::Connection(error)) => {
            return Err(CompletionEvidenceError::Persistence(error.to_string()))
        }
        Err(sea_orm::TransactionError::Transaction(error)) => return Err(error),
    };
    match outcome {
        RetryTxnOutcome::Resolved { result, .. } => Ok(*result),
        RetryTxnOutcome::Superseded { .. } => Err(CompletionEvidenceError::DecisionSuperseded),
    }
}

pub async fn retry_completion_artifact_for_user_txn(
    db: &AppDatabase,
    parent_conversation_id: i32,
    request: CompletionAttentionCas,
    metrics: &DelegationMetrics,
) -> Result<CompletionMutationResult, CompletionMutationError> {
    let result = db
        .conn
        .transaction::<_, RetryTxnOutcome, CompletionMutationError>(|txn| {
            let request = request.clone();
            Box::pin(async move {
                let attention = load_mutation_attention(txn, &request.attention_id).await?;
                require_attention_kind(
                    &attention,
                    &request,
                    AttentionKind::CompletionArtifactRecovery,
                )?;
                require_attention_owner(&attention, parent_conversation_id)?;
                require_task_v2_completion_mutation(txn, &attention.task_id).await?;
                if !attention_cas_fields_match(&attention, &request) {
                    return Err(CompletionMutationError::Superseded);
                }
                retry_completion_artifact_once(txn, &request)
                    .await
                    .map_err(Into::into)
            })
        })
        .await;
    match result {
        Ok(RetryTxnOutcome::Resolved {
            result,
            idempotent_replay,
        }) => {
            let completion = *result;
            let evidence = completion.evidence.as_ref().ok_or_else(|| {
                CompletionMutationError::InvalidAttention(
                    "resolved artifact retry has no completion evidence".into(),
                )
            })?;
            let mut mutation = CompletionMutationResult {
                workflow_id: evidence.binding.workflow_id.clone(),
                task_id: evidence.binding.task_id.clone(),
                node_id: completion_attention_public_node_id(&evidence.binding.node_id),
                kind: AttentionKind::CompletionArtifactRecovery,
                outcome: evidence.intent.outcome,
                evidence_scope_digest: evidence.evidence_scope_digest.clone(),
                graph_revision: completion.graph_revision,
                idempotent_replay,
                completion: None,
            };
            attach_durable_completion_projection(db, &mut mutation).await?;
            Ok(mutation)
        }
        Ok(RetryTxnOutcome::Superseded {
            phase,
            dimension,
            record_metric,
        }) => {
            if record_metric {
                metrics.record_completion_scope_invalidation(phase, dimension);
            }
            Err(CompletionMutationError::Superseded)
        }
        Err(sea_orm::TransactionError::Connection(error)) => {
            Err(CompletionMutationError::Evidence(
                CompletionEvidenceError::Persistence(error.to_string()),
            ))
        }
        Err(sea_orm::TransactionError::Transaction(error)) => Err(error),
    }
}

async fn retry_completion_artifact_once<C: ConnectionTrait>(
    conn: &C,
    request: &CompletionAttentionCas,
) -> Result<RetryTxnOutcome, CompletionEvidenceError> {
    if request.kind != AttentionKind::CompletionArtifactRecovery {
        return Err(CompletionEvidenceError::InvalidAttention(
            "retry requires completion_artifact_recovery".into(),
        ));
    }
    let attention = delegation_attention_request::Entity::find_by_id(request.attention_id.clone())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| CompletionEvidenceError::InvalidAttention("attention not found".into()))?;
    require_task_v2_completion_evidence(conn, &attention.task_id).await?;
    if !attention_cas_fields_match(&attention, request) {
        return Err(CompletionEvidenceError::InvalidAttention(
            "attention CAS mismatch".into(),
        ));
    }
    if attention.status == "resolved" {
        return match attention.resolution_code.as_deref() {
            Some("artifact_resolved") => {
                let loaded = load_terminal(conn, &request.task_id).await?;
                let graph_revision = attention
                    .resolution_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                    .and_then(|value| value.get("graph_revision").and_then(|value| value.as_u64()))
                    .ok_or_else(|| {
                        CompletionEvidenceError::InvalidAttention(
                            "resolved artifact attention has no committed graph revision".into(),
                        )
                    })?;
                let mut result = existing_result(conn, &loaded)
                    .await?
                    .filter(|result| result.state == CompletionState::Resolved)
                    .ok_or_else(|| {
                        CompletionEvidenceError::InvalidAttention(
                            "resolved artifact attention has no resolved evidence".into(),
                        )
                    })?;
                result.graph_revision = graph_revision;
                Ok(RetryTxnOutcome::Resolved {
                    result: Box::new(result),
                    idempotent_replay: true,
                })
            }
            Some("superseded") => Ok(RetryTxnOutcome::Superseded {
                phase: CompletionMetricPhase::Unknown,
                dimension: CompletionScopeInvalidationDimension::Policy,
                record_metric: false,
            }),
            _ => Err(CompletionEvidenceError::InvalidAttention(
                "artifact attention has an incompatible resolution".into(),
            )),
        };
    }
    validate_attention_cas(&attention, request)?;
    let payload_json = attention.payload_json.as_deref().ok_or_else(|| {
        CompletionEvidenceError::InvalidAttention("artifact payload is missing".into())
    })?;
    let payload: ArtifactRecoveryPayloadV1 = serde_json::from_str(payload_json)
        .map_err(|error| CompletionEvidenceError::InvalidAttention(error.to_string()))?;
    if payload.version != 1
        || payload.producer_task_id != request.task_id
        || payload.producer_scope_digest != request.captured_scope_digest
    {
        return Err(CompletionEvidenceError::InvalidAttention(
            "artifact payload does not match its CAS".into(),
        ));
    }

    let loaded = load_terminal(conn, &request.task_id).await?;
    let context = rebuild_completion_context(conn, &loaded).await?;
    let current_matches = ensure_context_matches_binding(&context, &loaded.binding).is_ok() && {
        context.evidence_scope_digest == payload.producer_scope_digest
            && loaded.run.generation == payload.producer_generation
            && loaded
                .binding
                .producer_baseline_head
                .as_deref()
                .unwrap_or_default()
                == payload.producer_baseline_head
    };
    if !current_matches {
        let graph_revision =
            bump_completion_graph(conn, &loaded, "completion_decision_superseded").await?;
        let resolution_json = serde_json::to_string(&json!({
            "version": 1,
            "code": CompletionAttentionResolutionCode::Superseded.as_str(),
            "graph_revision": graph_revision,
        }))
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
        resolve_attention_txn(
            conn,
            request,
            CompletionAttentionResolutionCode::Superseded.as_str(),
            Some(resolution_json),
        )
        .await?;
        let (phase, dimension) = retry_scope_invalidation(&loaded, Some(&context), &payload);
        return Ok(RetryTxnOutcome::Superseded {
            phase,
            dimension,
            record_metric: true,
        });
    }
    let artifact =
        resolve_terminal_artifact(conn, &loaded, &context, payload.normalized_intent.outcome)
            .await?;
    if artifact.kind() != payload.expected_resolver_kind {
        return Err(CompletionEvidenceError::InvalidAttention(
            "resolved artifact kind changed".into(),
        ));
    }
    let resolved_outcome = payload.normalized_intent.outcome;
    let evidence = persist_evidence_state(
        conn,
        &loaded,
        &context,
        payload.normalized_intent,
        artifact.clone(),
        None,
        Utc::now(),
    )
    .await?;
    let graph_revision = enqueue_completion_decision_resolved(
        conn,
        &loaded.workflow,
        &loaded.run.task_id,
        &loaded.node.node_id,
        AttentionKind::CompletionArtifactRecovery,
        resolved_outcome,
        &context.evidence_scope_digest,
    )
    .await?;
    let resolution_json = serde_json::to_string(&json!({
        "version": 1,
        "code": "artifact_resolved",
        "resolver_kind": artifact.kind(),
        "artifact": completion_artifact(&artifact),
        "graph_revision": graph_revision,
    }))
    .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    resolve_attention_txn(conn, request, "artifact_resolved", Some(resolution_json)).await?;
    Ok(RetryTxnOutcome::Resolved {
        result: Box::new(TerminalCompletionResult {
            state: CompletionState::Resolved,
            evidence: Some(evidence),
            attention: None,
            graph_revision,
            final_metric_states: Vec::new(),
        }),
        idempotent_replay: false,
    })
}

async fn load_terminal<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<LoadedTerminal, CompletionEvidenceError> {
    note_terminal_row_load();
    let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| CompletionEvidenceError::InvalidTerminalState("run not found".into()))?;
    let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            CompletionEvidenceError::InvalidTerminalState("run is not workflow-bound".into())
        })?;
    let workflow = delegation_workflow::Entity::find_by_id(binding.workflow_id.clone())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            CompletionEvidenceError::InvalidTerminalState("workflow not found".into())
        })?;
    let node = delegation_workflow_node_binding::Entity::find_by_id((
        binding.workflow_id.clone(),
        binding.node_id.clone(),
    ))
    .one(conn)
    .await
    .map_err(db_error)?
    .ok_or_else(|| CompletionEvidenceError::InvalidTerminalState("node not found".into()))?;
    Ok(LoadedTerminal {
        run,
        binding,
        workflow,
        node,
    })
}

/// Load and revalidate one persisted protocol-v2 completion against current
/// durable workflow scope and the current platform-resolved artifact.
pub async fn load_validated_completion_evidence<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<ValidatedCompletionEvidence, CompletionEvidenceError> {
    let loaded = load_terminal(conn, task_id).await?;
    validate_loaded_completion_evidence(conn, &loaded, None, ArtifactValidationMode::Current).await
}

pub(crate) async fn load_validated_frozen_git_completion_evidence<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<ValidatedCompletionEvidence, CompletionEvidenceError> {
    let loaded = load_terminal(conn, task_id).await?;
    validate_loaded_completion_evidence(
        conn,
        &loaded,
        None,
        ArtifactValidationMode::FrozenFinalDelivery,
    )
    .await
}

pub(crate) async fn validate_preloaded_completion_evidence<C: ConnectionTrait>(
    conn: &C,
    run: &delegation_task_run::Model,
    binding: &delegation_workflow_run_binding::Model,
    workflow: &delegation_workflow::Model,
    node: &delegation_workflow_node_binding::Model,
) -> Result<ValidatedCompletionEvidence, CompletionEvidenceError> {
    validate_preloaded_completion_evidence_inner(conn, run, binding, workflow, node, None).await
}

pub(crate) async fn validate_preloaded_completion_evidence_with_context<C: ConnectionTrait>(
    conn: &C,
    run: &delegation_task_run::Model,
    binding: &delegation_workflow_run_binding::Model,
    workflow: &delegation_workflow::Model,
    node: &delegation_workflow_node_binding::Model,
    preload: &PersistedCompletionContextPreload,
) -> Result<ValidatedCompletionEvidence, CompletionEvidenceError> {
    validate_preloaded_completion_evidence_inner(conn, run, binding, workflow, node, Some(preload))
        .await
}

async fn validate_preloaded_completion_evidence_inner<C: ConnectionTrait>(
    conn: &C,
    run: &delegation_task_run::Model,
    binding: &delegation_workflow_run_binding::Model,
    workflow: &delegation_workflow::Model,
    node: &delegation_workflow_node_binding::Model,
    preload: Option<&PersistedCompletionContextPreload>,
) -> Result<ValidatedCompletionEvidence, CompletionEvidenceError> {
    note_preloaded_completion_validation();
    validate_loaded_completion_evidence(
        conn,
        &LoadedTerminal {
            run: run.clone(),
            binding: binding.clone(),
            workflow: workflow.clone(),
            node: node.clone(),
        },
        preload,
        ArtifactValidationMode::Current,
    )
    .await
}

#[derive(Clone, Copy)]
enum ArtifactValidationMode {
    Current,
    FrozenFinalDelivery,
}

pub(crate) async fn preload_completion_validation_context<C: ConnectionTrait>(
    conn: &C,
    workflow: &delegation_workflow::Model,
    normalized: &NormalizedManifest,
    workspace: &str,
) -> Result<PersistedCompletionContextPreload, CompletionEvidenceError> {
    preload_persisted_completion_context(
        &WorkflowStore::new(conn, Path::new(workspace)),
        workflow,
        normalized,
    )
    .await
    .map_err(Into::into)
}

pub(crate) fn completion_validation_workspace(
    run: &delegation_task_run::Model,
) -> Result<&str, CompletionEvidenceError> {
    run.workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| ArtifactError::Unavailable(ArtifactFailure::WorkspaceUnavailable).into())
}

async fn validate_loaded_completion_evidence<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    preload: Option<&PersistedCompletionContextPreload>,
    artifact_validation: ArtifactValidationMode,
) -> Result<ValidatedCompletionEvidence, CompletionEvidenceError> {
    if loaded.workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2)
        || loaded.node.retired_revision.is_some()
        || loaded.node.node_outcome.is_some()
        || loaded.run.status != DelegationRunStatus::Completed
        || loaded.run.completion_state != Some(CompletionState::Resolved)
    {
        return Err(CompletionEvidenceError::InvalidTerminalState(
            "completion evidence is not attached to a current resolved v2 run".into(),
        ));
    }

    let outcome = loaded
        .run
        .completion_outcome
        .as_deref()
        .and_then(parse_outcome)
        .ok_or_else(|| {
            CompletionEvidenceError::InvalidTerminalState(
                "resolved run has no legal durable completion outcome".into(),
            )
        })?;
    let evidence_json = loaded
        .run
        .completion_evidence_json
        .as_deref()
        .ok_or_else(|| {
            CompletionEvidenceError::InvalidTerminalState(
                "resolved run has no durable completion evidence".into(),
            )
        })?;
    let context = rebuild_persisted_completion_context(conn, loaded, preload).await?;
    ensure_context_matches_binding(&context, &loaded.binding)?;
    let artifact = match artifact_validation {
        ArtifactValidationMode::Current => {
            completion_artifact(&resolve_terminal_artifact(conn, loaded, &context, outcome).await?)
        }
        ArtifactValidationMode::FrozenFinalDelivery => {
            if !matches!(
                context.scope_role,
                CompletionScopeRole::FinalFixer | CompletionScopeRole::FinalReviewer
            ) {
                return Err(CompletionEvidenceError::InvalidTerminalState(
                    "frozen delivery validation requires Final evidence".into(),
                ));
            }
            let expected = loaded
                .binding
                .artifact_digest
                .as_deref()
                .map(str::trim)
                .filter(|digest| !digest.is_empty())
                .ok_or_else(|| {
                    CompletionEvidenceError::InvalidTerminalState(
                        "Final evidence has no frozen artifact digest".into(),
                    )
                })?;
            match &context.evidence_scope.artifact_subject {
                ArtifactSubjectIdentityV2::GitHeadV1 { digest } if digest == expected => {
                    CompletionArtifactV2::GitHeadV1 {
                        head: expected.to_string(),
                    }
                }
                _ => {
                    return Err(CompletionEvidenceError::InvalidTerminalState(
                        "Final evidence is not bound to its frozen git artifact".into(),
                    ));
                }
            }
        }
    };
    if loaded.binding.artifact_digest.as_deref() != Some(artifact.digest()) {
        return Err(ArtifactError::ScopeChanged {
            expected: loaded
                .binding
                .artifact_digest
                .as_deref()
                .unwrap_or_default()
                .to_string(),
            actual: artifact.digest().to_string(),
        }
        .into());
    }
    let validated = validate_completion_evidence(
        evidence_json,
        &EvidenceValidationContext {
            role: context.scope_role.completion_role(),
            binding: completion_binding(loaded, &context)?,
            artifact,
            scope: context.evidence_scope,
        },
    )?;
    if !validated.evidence_validated || validated.evidence.intent.outcome != outcome {
        return Err(CompletionEvidenceError::InvalidTerminalState(
            "durable completion outcome does not match validated evidence".into(),
        ));
    }
    Ok(validated)
}

/// Fence unresolved completion recovery for every current v2 workflow node
/// carrying this node id. Callers that already have a task id should use the
/// task-scoped wrapper below so workflow-local node ids cannot collide.
pub async fn ensure_completion_recovery_not_fenced_txn<C: ConnectionTrait>(
    conn: &C,
    node_id: &str,
) -> Result<(), CompletionRecoveryFenceError> {
    let nodes = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::NodeId.eq(node_id))
        .filter(delegation_workflow_node_binding::Column::RetiredRevision.is_null())
        .all(conn)
        .await
        .map_err(|_| CompletionRecoveryFenceError::ArtifactUnavailable)?;
    for node in nodes {
        ensure_workflow_node_completion_recovery_not_fenced_txn(
            conn,
            &node.workflow_id,
            &node.node_id,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn ensure_task_completion_recovery_not_fenced_txn<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<(), CompletionRecoveryFenceError> {
    let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(|_| CompletionRecoveryFenceError::ArtifactUnavailable)?;
    let Some(binding) = binding else {
        return Ok(());
    };
    ensure_workflow_node_completion_recovery_not_fenced_txn(
        conn,
        &binding.workflow_id,
        &binding.node_id,
    )
    .await
}

async fn ensure_workflow_node_completion_recovery_not_fenced_txn<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    node_id: &str,
) -> Result<(), CompletionRecoveryFenceError> {
    let workflow = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
        .one(conn)
        .await
        .map_err(|_| CompletionRecoveryFenceError::ArtifactUnavailable)?;
    let Some(workflow) = workflow else {
        return Ok(());
    };
    if workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2) {
        return Ok(());
    }
    let node = delegation_workflow_node_binding::Entity::find_by_id((
        workflow_id.to_string(),
        node_id.to_string(),
    ))
    .one(conn)
    .await
    .map_err(|_| CompletionRecoveryFenceError::ArtifactUnavailable)?;
    let Some(node) = node.filter(|node| node.retired_revision.is_none()) else {
        return Ok(());
    };
    let binding = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id))
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(node_id))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .one(conn)
        .await
        .map_err(|_| CompletionRecoveryFenceError::ArtifactUnavailable)?;
    let Some(binding) = binding else {
        return Ok(());
    };
    let run = delegation_task_run::Entity::find_by_id(binding.task_id.clone())
        .one(conn)
        .await
        .map_err(|_| CompletionRecoveryFenceError::ArtifactUnavailable)?;
    let Some(run) = run else {
        return Ok(());
    };
    let loaded = LoadedTerminal {
        run,
        binding,
        workflow,
        node,
    };

    // A material/policy/producer scope change supersedes the old attention.
    let context = rebuild_completion_context(conn, &loaded)
        .await
        .map_err(|_| CompletionRecoveryFenceError::ArtifactUnavailable)?;
    if ensure_context_matches_binding(&context, &loaded.binding).is_err() {
        return Ok(());
    }

    let current_attention = delegation_attention_request::Entity::find()
        .filter(delegation_attention_request::Column::NodeId.eq(node_id))
        .filter(delegation_attention_request::Column::LatestRunId.eq(&loaded.run.task_id))
        .filter(delegation_attention_request::Column::Status.eq("open"))
        .order_by_desc(delegation_attention_request::Column::CreatedAt)
        .one(conn)
        .await
        .map_err(|_| CompletionRecoveryFenceError::ArtifactUnavailable)?
        .filter(|attention| {
            attention.captured_scope_digest.as_deref()
                == Some(context.evidence_scope_digest.as_str())
        });

    match loaded.run.completion_state {
        Some(CompletionState::NeedsDecision) => Err(CompletionRecoveryFenceError::DecisionRequired),
        Some(CompletionState::ArtifactRecovery) => {
            Err(CompletionRecoveryFenceError::ArtifactUnavailable)
        }
        _ => match current_attention.map(|attention| attention.kind) {
            Some(AttentionKind::CompletionDecision) => {
                Err(CompletionRecoveryFenceError::DecisionRequired)
            }
            Some(AttentionKind::CompletionArtifactRecovery) => {
                Err(CompletionRecoveryFenceError::ArtifactUnavailable)
            }
            _ => Ok(()),
        },
    }
}

fn validate_v2_terminal(
    loaded: &LoadedTerminal,
    input: &TerminalCompletionInput,
) -> Result<(), CompletionEvidenceError> {
    if loaded.run.task_id != input.task_id
        || loaded.run.status != DelegationRunStatus::Completed
        || loaded.workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2)
        || loaded.workflow.completion_protocol_mode != CompletionProtocolMode::V2Enforce
    {
        return Err(CompletionEvidenceError::InvalidTerminalState(
            "run is not a completed v2-enforce workflow run".into(),
        ));
    }
    Ok(())
}

async fn rebuild_completion_context<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
) -> Result<AdmissionCompletionContextV2, CompletionEvidenceError> {
    let workspace = completion_validation_workspace(&loaded.run)?;
    build_admission_completion_context(
        &WorkflowStore::new(conn, Path::new(workspace)),
        &AdmissionCandidate {
            workflow: &loaded.workflow,
            node: &loaded.node,
            task_id: &loaded.run.task_id,
            artifact_digest: loaded.binding.artifact_digest.as_deref(),
            reviewed_task_id: loaded.binding.reviewed_task_id.as_deref(),
            reviewed_generation: loaded.binding.reviewed_implementer_generation,
            producer_baseline_head: loaded.binding.producer_baseline_head.as_deref(),
        },
    )
    .await
    .map_err(Into::into)
}

async fn rebuild_persisted_completion_context<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    preload: Option<&PersistedCompletionContextPreload>,
) -> Result<AdmissionCompletionContextV2, CompletionEvidenceError> {
    let workspace = loaded
        .run
        .workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or(ArtifactError::Unavailable(
            ArtifactFailure::WorkspaceUnavailable,
        ))?;
    let store = WorkflowStore::new(conn, Path::new(workspace));
    let candidate = AdmissionCandidate {
        workflow: &loaded.workflow,
        node: &loaded.node,
        task_id: &loaded.run.task_id,
        artifact_digest: loaded.binding.artifact_digest.as_deref(),
        reviewed_task_id: loaded.binding.reviewed_task_id.as_deref(),
        reviewed_generation: loaded.binding.reviewed_implementer_generation,
        producer_baseline_head: loaded.binding.producer_baseline_head.as_deref(),
    };
    match preload {
        Some(preload) => build_preloaded_persisted_completion_context(
            &store,
            &candidate,
            &loaded.binding,
            preload,
        )
        .await
        .map_err(Into::into),
        None => build_persisted_context(&store, &candidate, &loaded.binding)
            .await
            .map_err(Into::into),
    }
}

fn ensure_context_matches_binding(
    context: &AdmissionCompletionContextV2,
    binding: &delegation_workflow_run_binding::Model,
) -> Result<(), CompletionEvidenceError> {
    let matches = binding.evidence_scope_digest.as_deref()
        == Some(context.evidence_scope_digest.as_str())
        && binding.gate_lineage == context.evidence_scope.gate_lineage
        && binding.review_round == context.evidence_scope.review_round.map(i64::from)
        && binding.instruction_block_digest.as_deref() == Some(context.instruction.digest.as_str())
        && binding.material_selector_digest == context.material_selector_digest
        && binding.subject_material_digest == context.subject_material_digest
        && binding.requirements_identity == context.requirements_identity
        && binding.task_specification_identity == context.task_specification_identity
        && binding.final_findings_identity == context.final_findings_identity;
    if matches {
        Ok(())
    } else {
        Err(super::evidence_scope::EvidenceScopeError::ScopeChanged.into())
    }
}

fn retry_scope_invalidation(
    loaded: &LoadedTerminal,
    current: Option<&AdmissionCompletionContextV2>,
    payload: &ArtifactRecoveryPayloadV1,
) -> (CompletionMetricPhase, CompletionScopeInvalidationDimension) {
    let phase = match loaded.node.phase_id.as_str() {
        "design" => CompletionMetricPhase::Design,
        "plan" => CompletionMetricPhase::Plan,
        "tasks" => CompletionMetricPhase::Tasks,
        "final" => CompletionMetricPhase::Final,
        _ => CompletionMetricPhase::Unknown,
    };
    let dimension = if loaded.run.generation != payload.producer_generation
        || loaded
            .binding
            .producer_baseline_head
            .as_deref()
            .unwrap_or_default()
            != payload.producer_baseline_head
    {
        CompletionScopeInvalidationDimension::Producer
    } else if let Some(current) = current {
        if loaded.binding.instruction_block_digest.as_deref()
            != Some(current.instruction.digest.as_str())
        {
            CompletionScopeInvalidationDimension::Instruction
        } else if loaded.binding.requirements_identity != current.requirements_identity {
            CompletionScopeInvalidationDimension::Requirements
        } else if loaded.binding.final_findings_identity != current.final_findings_identity {
            CompletionScopeInvalidationDimension::FinalFindings
        } else if loaded.binding.gate_lineage != current.evidence_scope.gate_lineage
            || loaded.binding.review_round != current.evidence_scope.review_round.map(i64::from)
        {
            CompletionScopeInvalidationDimension::Lineage
        } else if loaded.binding.task_specification_identity != current.task_specification_identity
            || loaded.binding.material_selector_digest != current.material_selector_digest
            || loaded.binding.subject_material_digest != current.subject_material_digest
        {
            CompletionScopeInvalidationDimension::TaskScope
        } else if loaded.binding.evidence_scope_digest.as_deref()
            != Some(current.evidence_scope_digest.as_str())
        {
            CompletionScopeInvalidationDimension::Policy
        } else {
            CompletionScopeInvalidationDimension::Artifact
        }
    } else {
        CompletionScopeInvalidationDimension::Artifact
    };
    (phase, dimension)
}

async fn load_tool_intents<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<Vec<LoadedToolIntent>, CompletionEvidenceError> {
    let rows = delegation_completion_tool_intent::Entity::find()
        .filter(delegation_completion_tool_intent::Column::TaskId.eq(task_id))
        .order_by_asc(delegation_completion_tool_intent::Column::AcceptedOrdinal)
        .all(conn)
        .await
        .map_err(db_error)?;
    rows.into_iter()
        .map(|row| {
            let outcome = parse_outcome(&row.outcome).ok_or_else(|| {
                CompletionEvidenceError::InvalidTerminalState(
                    "stored tool intent has an invalid outcome".into(),
                )
            })?;
            Ok(LoadedToolIntent {
                intent_id: row.intent_id,
                intent: CompletionToolIntent {
                    accepted_ordinal: row.accepted_ordinal,
                    outcome,
                    summary: row.summary,
                    report_file: row.report_hint,
                },
            })
        })
        .collect()
}

async fn resolve_terminal_artifact<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    outcome: CompletionOutcome,
) -> Result<ResolvedArtifact, ArtifactError> {
    let workspace = loaded
        .run
        .workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or(ArtifactError::Unavailable(
            ArtifactFailure::WorkspaceUnavailable,
        ))?;
    match &context.evidence_scope.artifact_subject {
        ArtifactSubjectIdentityV2::DocumentSha256 { rel_path, .. }
        | ArtifactSubjectIdentityV2::PendingDocument { rel_path }
        | ArtifactSubjectIdentityV2::PlanMaterial {
            plan_rel_path: rel_path,
            ..
        } => resolve_document(Path::new(workspace), rel_path, MAX_DOCUMENT_ARTIFACT_BYTES)
            .await
            .map(Into::into),
        ArtifactSubjectIdentityV2::GitHeadV1 { digest } => {
            if matches!(
                context.scope_role,
                super::types::CompletionScopeRole::TaskImplementer
                    | super::types::CompletionScopeRole::FinalFixer
            ) {
                if !matches!(
                    outcome,
                    CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns
                ) {
                    return Ok(GitHeadV1Artifact {
                        head: digest.clone(),
                    }
                    .into());
                }
                let allow_noop = context.scope_role
                    == super::types::CompletionScopeRole::TaskImplementer
                    && durable_task_allows_noop(conn, &loaded.workflow, loaded.node.task_index)
                        .await
                        .unwrap_or(false);
                resolve_producer_completion(Path::new(workspace), outcome, digest, allow_noop)
                    .await?
                    .ok_or(ArtifactError::Unavailable(ArtifactFailure::CommitRequired))
            } else {
                resolve_reviewer_completion(Path::new(workspace), digest).await
            }
        }
    }
}

async fn durable_task_allows_noop<C: ConnectionTrait>(
    conn: &C,
    workflow: &delegation_workflow::Model,
    task_index: Option<i64>,
) -> Result<bool, CompletionEvidenceError> {
    let Some(task_index) = task_index.and_then(|value| u32::try_from(value).ok()) else {
        return Ok(false);
    };
    let snapshot = super::store::load_active_manifest_snapshot(conn, workflow)
        .await
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    Ok(snapshot
        .normalized
        .task_policies
        .iter()
        .find(|policy| policy.task_index == task_index)
        .is_some_and(|policy| policy.allow_noop_verification))
}

async fn persist_evidence_state<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    intent: CompletionIntent,
    artifact: ResolvedArtifact,
    final_remediation_contexts: Option<&[RemediationContextSnapshotV1]>,
    captured_at: chrono::DateTime<Utc>,
) -> Result<CompletionEvidenceV2, CompletionEvidenceError> {
    let artifact = completion_artifact(&artifact);
    let binding_projection = completion_binding(loaded, context)?;
    let evidence = CompletionEvidenceV2 {
        version: COMPLETION_PROTOCOL_VERSION_V2,
        intent,
        binding: binding_projection.clone(),
        artifact: artifact.clone(),
        review_scope_digest: context.review_scope_digest.clone(),
        evidence_scope_digest: context.evidence_scope_digest.clone(),
        captured_at: captured_at.to_rfc3339_opts(SecondsFormat::Millis, true),
    };
    let evidence_json = serde_json::to_string(&evidence)
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    let validated = validate_completion_evidence(
        &evidence_json,
        &EvidenceValidationContext {
            role: context.scope_role.completion_role(),
            binding: binding_projection,
            artifact,
            scope: context.evidence_scope.clone(),
        },
    )?;
    if !validated.evidence_validated || validated.evidence != evidence {
        return Err(CompletionEvidenceError::InvalidTerminalState(
            "evidence round trip changed the platform value".into(),
        ));
    }

    let mut run: delegation_task_run::ActiveModel = loaded.run.clone().into();
    run.completion_state = Set(Some(CompletionState::Resolved));
    run.completion_outcome = Set(Some(evidence.intent.outcome.as_str().into()));
    run.completion_evidence_json = Set(Some(evidence_json));
    if let Some(contexts) = final_remediation_contexts {
        verify_remediation_context_snapshots_v1(contexts).map_err(map_final_findings_error)?;
        run.final_remediation_contexts_json =
            Set(Some(serde_json::to_string(contexts).map_err(|error| {
                CompletionEvidenceError::Persistence(error.to_string())
            })?));
    }
    run.card_summary_json = Set(None);
    run.updated_at = Set(captured_at);
    run.update(conn).await.map_err(db_error)?;
    update_binding_projection(conn, loaded, context, Some(evidence.artifact.digest())).await?;
    if context.scope_role == CompletionScopeRole::PlanAuthor {
        let workspace = loaded.run.workspace_path.as_deref().ok_or_else(|| {
            CompletionEvidenceError::Persistence(
                "completed Plan Author has no durable workspace".into(),
            )
        })?;
        super::store::authorize_plan_round_after_author_completion(
            conn,
            &loaded.workflow,
            &loaded.node.node_id,
            &loaded.run.task_id,
            workspace,
            evidence.artifact.digest(),
        )
        .await
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    }
    Ok(evidence)
}

async fn open_completion_decision<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    reason_code: CompletionIntentReason,
    bounded_candidates: Vec<CompletionCandidate>,
    diagnostics: Vec<CompletionDiagnostic>,
) -> Result<TerminalCompletionResult, CompletionEvidenceError> {
    let payload = CompletionDecisionPayloadV1 {
        version: 1,
        reason_code,
        role: context.scope_role.completion_role(),
        legal_outcomes: legal_outcomes(context.scope_role.completion_role()),
        bounded_candidates,
        diagnostics,
    };
    let payload_json = serialize_completion_decision_payload(payload)?;
    let attention = open_attention(
        conn,
        loaded,
        AttentionKind::CompletionDecision,
        COMPLETION_DECISION_MESSAGE,
        &payload_json,
        &context.evidence_scope_digest,
    )
    .await?;
    persist_unresolved_state(conn, loaded, context, CompletionState::NeedsDecision, None).await?;
    let graph_revision = bump_completion_graph(conn, loaded, "completion_decision_opened").await?;
    Ok(TerminalCompletionResult {
        state: CompletionState::NeedsDecision,
        evidence: None,
        attention: Some(attention),
        graph_revision,
        final_metric_states: Vec::new(),
    })
}

fn serialize_completion_decision_payload(
    mut payload: CompletionDecisionPayloadV1,
) -> Result<String, CompletionEvidenceError> {
    loop {
        let json = serde_json::to_string(&payload)
            .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
        if json.len() <= ATTENTION_PAYLOAD_MAX_BYTES {
            return Ok(json);
        }
        if payload.diagnostics.pop().is_some() {
            continue;
        }
        if payload.bounded_candidates.len() > 2 {
            let penultimate = payload.bounded_candidates.len() - 2;
            payload.bounded_candidates.remove(penultimate);
            continue;
        }
        return Err(CompletionEvidenceError::Persistence(
            "minimum completion decision payload exceeds attention budget".into(),
        ));
    }
}

async fn open_artifact_recovery<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    intent: CompletionIntent,
    source_audit_ref: CompletionSourceAuditRef,
    failure: ArtifactFailure,
    final_remediation_contexts: Option<&[RemediationContextSnapshotV1]>,
) -> Result<TerminalCompletionResult, CompletionEvidenceError> {
    let expected_resolver_kind = match context.evidence_scope.artifact_subject {
        ArtifactSubjectIdentityV2::GitHeadV1 { .. } => ArtifactKind::GitHeadV1,
        _ => ArtifactKind::DocumentSha256,
    };
    let payload = ArtifactRecoveryPayloadV1 {
        version: 1,
        normalized_intent: intent,
        source_audit_ref,
        resolver_failure: failure,
        producer_scope_digest: context.evidence_scope_digest.clone(),
        producer_baseline_head: loaded
            .binding
            .producer_baseline_head
            .clone()
            .unwrap_or_default(),
        expected_resolver_kind,
        producer_task_id: loaded.run.task_id.clone(),
        producer_generation: loaded.run.generation,
    };
    let payload_json = serde_json::to_string(&payload)
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    let attention = open_attention(
        conn,
        loaded,
        AttentionKind::CompletionArtifactRecovery,
        ARTIFACT_RECOVERY_MESSAGE,
        &payload_json,
        &payload.producer_scope_digest,
    )
    .await?;
    persist_unresolved_state(
        conn,
        loaded,
        context,
        CompletionState::ArtifactRecovery,
        final_remediation_contexts,
    )
    .await?;
    let graph_revision =
        bump_completion_graph(conn, loaded, "completion_artifact_recovery_opened").await?;
    Ok(TerminalCompletionResult {
        state: CompletionState::ArtifactRecovery,
        evidence: None,
        attention: Some(attention),
        graph_revision,
        final_metric_states: Vec::new(),
    })
}

async fn persist_unresolved_state<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    state: CompletionState,
    final_remediation_contexts: Option<&[RemediationContextSnapshotV1]>,
) -> Result<(), CompletionEvidenceError> {
    let mut run: delegation_task_run::ActiveModel = loaded.run.clone().into();
    run.completion_state = Set(Some(state));
    run.completion_outcome = Set(None);
    run.completion_evidence_json = Set(None);
    if let Some(contexts) = final_remediation_contexts {
        verify_remediation_context_snapshots_v1(contexts).map_err(map_final_findings_error)?;
        run.final_remediation_contexts_json =
            Set(Some(serde_json::to_string(contexts).map_err(|error| {
                CompletionEvidenceError::Persistence(error.to_string())
            })?));
    }
    run.card_summary_json = Set(None);
    run.updated_at = Set(Utc::now());
    run.update(conn).await.map_err(db_error)?;
    update_binding_projection(conn, loaded, context, None).await
}

async fn update_binding_projection<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    artifact_digest: Option<&str>,
) -> Result<(), CompletionEvidenceError> {
    let mut binding: delegation_workflow_run_binding::ActiveModel = loaded.binding.clone().into();
    binding.evidence_scope_digest = Set(Some(context.evidence_scope_digest.clone()));
    binding.gate_lineage = Set(context.evidence_scope.gate_lineage.clone());
    binding.review_round = Set(context.evidence_scope.review_round.map(i64::from));
    binding.instruction_block_digest = Set(Some(context.instruction.digest.clone()));
    binding.material_selector_digest = Set(context.material_selector_digest.clone());
    binding.subject_material_digest = Set(context.subject_material_digest.clone());
    binding.requirements_identity = Set(context.requirements_identity.clone());
    binding.task_specification_identity = Set(context.task_specification_identity.clone());
    binding.final_findings_identity = Set(context.final_findings_identity.clone());
    if let Some(digest) = artifact_digest {
        binding.artifact_digest = Set(Some(digest.to_string()));
    }
    binding.updated_at = Set(Utc::now());
    binding.update(conn).await.map_err(db_error)?;
    Ok(())
}

async fn open_attention<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    kind: AttentionKind,
    message: &'static str,
    payload_json: &str,
    captured_scope_digest: &str,
) -> Result<CompletionAttentionCas, CompletionEvidenceError> {
    let row = open_terminal_completion_attention_txn(
        conn,
        &TerminalCompletionAttentionInput {
            task_id: &loaded.run.task_id,
            kind,
            message,
            payload_json,
            captured_scope_digest,
            node_id: &loaded.node.node_id,
            created_at: Utc::now(),
        },
    )
    .await
    .map_err(|error| CompletionEvidenceError::InvalidAttention(error.to_string()))?;
    attention_cas(&row)
}

async fn existing_result<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
) -> Result<Option<TerminalCompletionResult>, CompletionEvidenceError> {
    let Some(state) = loaded.run.completion_state.clone() else {
        return Ok(None);
    };
    let graph_revision = u64::try_from(loaded.workflow.graph_revision).map_err(|_| {
        CompletionEvidenceError::InvalidTerminalState("negative graph revision".into())
    })?;
    match state {
        CompletionState::Resolved => {
            let evidence = loaded
                .run
                .completion_evidence_json
                .as_deref()
                .ok_or_else(|| {
                    CompletionEvidenceError::InvalidTerminalState(
                        "resolved run is missing evidence".into(),
                    )
                })
                .and_then(|json| {
                    serde_json::from_str(json).map_err(|error| {
                        CompletionEvidenceError::InvalidTerminalState(error.to_string())
                    })
                })?;
            Ok(Some(TerminalCompletionResult {
                state,
                evidence: Some(evidence),
                attention: None,
                graph_revision,
                final_metric_states: Vec::new(),
            }))
        }
        CompletionState::NeedsDecision | CompletionState::ArtifactRecovery => {
            let kind = if state == CompletionState::NeedsDecision {
                AttentionKind::CompletionDecision
            } else {
                AttentionKind::CompletionArtifactRecovery
            };
            let row = delegation_attention_request::Entity::find()
                .filter(delegation_attention_request::Column::TaskId.eq(&loaded.run.task_id))
                .filter(delegation_attention_request::Column::Kind.eq(kind))
                .filter(delegation_attention_request::Column::Status.eq("open"))
                .one(conn)
                .await
                .map_err(db_error)?
                .ok_or_else(|| {
                    CompletionEvidenceError::InvalidAttention(
                        "unresolved completion has no open attention".into(),
                    )
                })?;
            Ok(Some(TerminalCompletionResult {
                state,
                evidence: None,
                attention: Some(attention_cas(&row)?),
                graph_revision,
                final_metric_states: Vec::new(),
            }))
        }
    }
}

fn completion_binding(
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
) -> Result<CompletionEvidenceBindingV2, CompletionEvidenceError> {
    Ok(CompletionEvidenceBindingV2 {
        workflow_id: loaded.workflow.workflow_id.clone(),
        task_id: loaded.run.task_id.clone(),
        node_id: loaded.node.node_id.clone(),
        role: context.scope_role.completion_role(),
        phase_id: loaded.node.phase_id.clone(),
        task_index: loaded
            .node
            .task_index
            .map(u32::try_from)
            .transpose()
            .map_err(|_| {
                CompletionEvidenceError::InvalidTerminalState("invalid task index".into())
            })?,
        gate_id: context.evidence_scope.gate_id.clone(),
        gate_lineage: context.evidence_scope.gate_lineage.clone(),
        review_round: context.evidence_scope.review_round,
        reviewed_task_id: loaded.binding.reviewed_task_id.clone(),
        reviewed_generation: loaded.binding.reviewed_implementer_generation,
        manifest_revision_observed: u64::try_from(loaded.binding.manifest_revision).map_err(
            |_| CompletionEvidenceError::InvalidTerminalState("invalid manifest revision".into()),
        )?,
    })
}

fn completion_artifact(artifact: &ResolvedArtifact) -> CompletionArtifactV2 {
    match artifact {
        ResolvedArtifact::DocumentSha256(document) => CompletionArtifactV2::DocumentSha256 {
            rel_path: document.rel_path().to_string(),
            digest: document.digest().to_string(),
        },
        ResolvedArtifact::GitHeadV1(git) => CompletionArtifactV2::GitHeadV1 {
            head: git.head.clone(),
        },
    }
}

fn source_audit_ref(
    tool_intents: &[LoadedToolIntent],
    intent: &CompletionIntent,
) -> CompletionSourceAuditRef {
    match intent.source {
        CompletionIntentSource::CompleteWork => tool_intents
            .iter()
            .max_by_key(|loaded| loaded.intent.accepted_ordinal)
            .map_or(CompletionSourceAuditRef::AssistantConclusion, |loaded| {
                CompletionSourceAuditRef::ToolIntent {
                    intent_id: loaded.intent_id.clone(),
                }
            }),
        CompletionIntentSource::AssistantConclusion => {
            CompletionSourceAuditRef::AssistantConclusion
        }
        CompletionIntentSource::Report => CompletionSourceAuditRef::Report {
            report_file: intent.report_file.clone(),
        },
        CompletionIntentSource::UserAdjudication => CompletionSourceAuditRef::UserAdjudication,
    }
}

fn legal_outcomes(role: CompletionRole) -> Vec<CompletionOutcome> {
    match role {
        CompletionRole::Reviewer => vec![
            CompletionOutcome::Approve,
            CompletionOutcome::ApproveWithMinors,
            CompletionOutcome::RequestChanges,
            CompletionOutcome::Block,
        ],
        CompletionRole::Author | CompletionRole::Implementer | CompletionRole::Fixer => vec![
            CompletionOutcome::Done,
            CompletionOutcome::DoneWithConcerns,
            CompletionOutcome::Blocked,
        ],
    }
}

fn parse_outcome(value: &str) -> Option<CompletionOutcome> {
    Some(match value {
        "approve" => CompletionOutcome::Approve,
        "approve_with_minors" => CompletionOutcome::ApproveWithMinors,
        "request_changes" => CompletionOutcome::RequestChanges,
        "block" => CompletionOutcome::Block,
        "done" => CompletionOutcome::Done,
        "done_with_concerns" => CompletionOutcome::DoneWithConcerns,
        "blocked" => CompletionOutcome::Blocked,
        _ => return None,
    })
}

fn attention_cas(
    row: &delegation_attention_request::Model,
) -> Result<CompletionAttentionCas, CompletionEvidenceError> {
    Ok(CompletionAttentionCas {
        attention_id: row.request_id.clone(),
        task_id: row.task_id.clone(),
        kind: row.kind.clone(),
        captured_scope_digest: row.captured_scope_digest.clone().ok_or_else(|| {
            CompletionEvidenceError::InvalidAttention("captured scope is missing".into())
        })?,
        latest_run_id: row.latest_run_id.clone().ok_or_else(|| {
            CompletionEvidenceError::InvalidAttention("latest run is missing".into())
        })?,
        node_id: completion_attention_public_node_id(
            row.node_id.as_deref().ok_or_else(|| {
                CompletionEvidenceError::InvalidAttention("node is missing".into())
            })?,
        ),
    })
}

fn validate_attention_cas(
    row: &delegation_attention_request::Model,
    request: &CompletionAttentionCas,
) -> Result<(), CompletionEvidenceError> {
    if row.status == "open" && attention_cas_fields_match(row, request) {
        Ok(())
    } else {
        Err(CompletionEvidenceError::InvalidAttention(
            "attention CAS mismatch".into(),
        ))
    }
}

fn attention_cas_fields_match(
    row: &delegation_attention_request::Model,
    request: &CompletionAttentionCas,
) -> bool {
    row.request_id == request.attention_id
        && row.task_id == request.task_id
        && row.kind == request.kind
        && row.captured_scope_digest.as_deref() == Some(&request.captured_scope_digest)
        && row.latest_run_id.as_deref() == Some(&request.latest_run_id)
        && row
            .node_id
            .as_deref()
            .is_some_and(|node_id| completion_attention_public_node_id(node_id) == request.node_id)
}

async fn resolve_attention_txn<C: ConnectionTrait>(
    conn: &C,
    request: &CompletionAttentionCas,
    code: &str,
    resolution_json: Option<String>,
) -> Result<(), CompletionEvidenceError> {
    let result = delegation_attention_request::Entity::update_many()
        .col_expr(
            delegation_attention_request::Column::Status,
            sea_orm::sea_query::Expr::value("resolved"),
        )
        .col_expr(
            delegation_attention_request::Column::ResolutionCode,
            sea_orm::sea_query::Expr::value(Some(code.to_string())),
        )
        .col_expr(
            delegation_attention_request::Column::ResolutionJson,
            sea_orm::sea_query::Expr::value(resolution_json),
        )
        .col_expr(
            delegation_attention_request::Column::ResolvedAt,
            sea_orm::sea_query::Expr::value(Some(Utc::now())),
        )
        .filter(delegation_attention_request::Column::RequestId.eq(&request.attention_id))
        .filter(delegation_attention_request::Column::TaskId.eq(&request.task_id))
        .filter(delegation_attention_request::Column::Kind.eq(request.kind.clone()))
        .filter(
            delegation_attention_request::Column::CapturedScopeDigest
                .eq(&request.captured_scope_digest),
        )
        .filter(delegation_attention_request::Column::LatestRunId.eq(&request.latest_run_id))
        .filter(delegation_attention_request::Column::Status.eq("open"))
        .exec(conn)
        .await
        .map_err(db_error)?;
    if result.rows_affected == 1 {
        Ok(())
    } else {
        Err(CompletionEvidenceError::InvalidAttention(
            "attention CAS lost".into(),
        ))
    }
}

async fn bump_completion_graph<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    event_kind: &str,
) -> Result<u64, CompletionEvidenceError> {
    let graph_revision = bump_graph_revision(conn, &loaded.workflow.workflow_id, Utc::now())
        .await
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    let payload_json = serde_json::to_string(&json!({
        "workflow_id": loaded.workflow.workflow_id,
        "task_id": loaded.run.task_id,
        "node_id": loaded.node.node_id,
        "graph_revision": graph_revision,
        "event_kind": event_kind,
    }))
    .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    delegation_workflow_outbox_event::ActiveModel {
        event_id: Set(uuid::Uuid::new_v4().to_string()),
        workflow_id: Set(loaded.workflow.workflow_id.clone()),
        graph_revision: Set(i64::try_from(graph_revision).map_err(|_| {
            CompletionEvidenceError::Persistence("graph revision exceeds i64".into())
        })?),
        event_kind: Set(event_kind.to_string()),
        subject_key: Set(loaded.run.task_id.clone()),
        payload_json: Set(payload_json),
        dispatch_attempts: Set(0),
        created_at: Set(Utc::now()),
        delivered_at: Set(None),
    }
    .insert(conn)
    .await
    .map_err(db_error)?;
    Ok(graph_revision)
}

fn db_error(error: sea_orm::DbErr) -> CompletionEvidenceError {
    CompletionEvidenceError::Persistence(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use async_trait::async_trait;
    use chrono::Utc;
    use sea_orm::{
        ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
    };
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::{
        materialize_terminal_completion_txn, open_design_self_review_decision_txn,
        reconcile_completion_attentions_txn, resolve_completion_decision_txn,
        resolve_design_self_review_txn, resolve_workflow_completion_attentions_txn,
        retry_completion_artifact_for_user_txn, retry_completion_artifact_txn,
        CompletionDecisionPayloadV1, CompletionMutationError, TerminalCompletionInput,
        ValidatedReportCandidate,
    };
    use crate::acp::delegation::event_emitter::{
        CompletionOutboxDispatcher, CompletionRootWakeQueue,
    };
    use crate::acp::delegation::metrics::DelegationMetrics;
    use crate::acp::delegation::run_store::{ContinueRunAdmission, ReservingRunInsert, RunStore};
    use crate::acp::delegation::store::{Settlement, TerminalTaskWrite};
    use crate::acp::delegation::workflow::completion_projection::{
        completion_projection_load_count, reset_completion_projection_load_count,
    };
    use crate::acp::delegation::workflow::evidence_scope::{
        completion_context_prepare_counts, reset_completion_context_prepare_counts,
    };
    use crate::acp::delegation::workflow::final_findings::{
        build_final_findings_package_v1, load_active_final_findings_package_v1,
        persist_final_findings_package_v1, FinalFindingInputV1, FinalFindingsPackageInputV1,
        FinalReviewerEvaluationV1, RemediationContextInputV1,
    };
    use crate::acp::delegation::workflow::store::{
        get_workflow_state_core, publish_workflow_manifest_core, PublishWorkflowRequest,
    };
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestDocument, ManifestGate, ManifestNode,
        ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState, ResolutionMode,
        WorkUnitKeyParts, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN,
        PHASE_TASKS, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use crate::acp::delegation::workflow::{
        build_work_unit_key, load_completion_projection, project_workflow_graph_core,
        resolve_document, CompletionCardState, CompletionIntentReason, CompletionIntentSource,
        CompletionOutcome,
    };
    use crate::db::entities::conversation::ConversationStatus;
    use crate::db::entities::delegation_attention_request::{self, AttentionKind};
    use crate::db::entities::delegation_completion_tool_intent;
    use crate::db::entities::delegation_task_run::{
        self, AdmissionClass, CompletionState, DelegationRunStatus,
    };
    use crate::db::entities::delegation_workflow::{self, CompletionProtocolMode};
    use crate::db::entities::{
        delegation_workflow_design_root_binding, delegation_workflow_gate_state,
        delegation_workflow_outbox_event,
    };
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::AgentType;
    use crate::web::event_bridge::EventEmitter;

    const DESIGN_REL_PATH: &str = "docs/superpowers/specs/task-10-design.md";
    const PLAN_REL_PATH: &str = "docs/superpowers/plans/task-10-plan.md";
    const DESIGN_BYTES: &[u8] = b"# Design\n\nTask 10 fixture.\n";

    async fn seed_active_final_package(fixture: &TerminalFixture) -> super::FinalFindingsPackageV1 {
        if delegation_workflow_gate_state::Entity::find_by_id((
            fixture.workflow_id.clone(),
            "final".to_string(),
        ))
        .one(&fixture.db.conn)
        .await
        .unwrap()
        .is_none()
        {
            delegation_workflow_gate_state::ActiveModel {
                workflow_id: Set(fixture.workflow_id.clone()),
                gate_id: Set("final".into()),
                gate_lineage: Set(format!("sha256:{}", "f".repeat(64))),
                current_review_round: Set(1),
                selected_node_ids_json: Set("[]".into()),
            }
            .insert(&fixture.db.conn)
            .await
            .unwrap();
        }
        let package = build_final_findings_package_v1(FinalFindingsPackageInputV1 {
            workflow_id: fixture.workflow_id.clone(),
            gate_id: "final".into(),
            gate_lineage: format!("sha256:{}", "f".repeat(64)),
            requirements_identity: format!("sha256:{}", "a".repeat(64)),
            graph_revision: 1,
            reviewer_evaluations: vec![FinalReviewerEvaluationV1 {
                reviewer_node_id: "final-reviewer".into(),
                evidence_task_id: "final-review-task".into(),
                evidence_scope_digest: format!("sha256:{}", "e".repeat(64)),
                outcome: CompletionOutcome::RequestChanges,
            }],
            findings: vec![FinalFindingInputV1 {
                reviewer_node_id: "final-reviewer".into(),
                evidence_task_id: "final-review-task".into(),
                evidence_scope_digest: format!("sha256:{}", "e".repeat(64)),
                outcome: CompletionOutcome::RequestChanges,
                target_work_unit_keys: vec!["task|1|implementer|codex|none".into()],
                remediation_route_ids: vec!["task-1-implementer".into()],
            }],
            remediation_contexts: vec![RemediationContextInputV1::available_terminal(
                "final-review-task",
                b"immutable remediation context".to_vec(),
            )],
        })
        .unwrap();
        persist_final_findings_package_v1(&fixture.db.conn, &package, 1)
            .await
            .unwrap();
        package
    }

    #[derive(Clone, Copy)]
    enum IntentFixture {
        Tool,
        AssistantText,
        Report,
        Missing,
    }

    struct TerminalFixture {
        db: Arc<AppDatabase>,
        _workspace: TempDir,
        workspace_path: std::path::PathBuf,
        task_id: String,
        workflow_id: String,
        parent_conversation_id: i32,
        input: TerminalCompletionInput,
    }

    struct RecordingRootWake {
        db: Arc<AppDatabase>,
        event_ids: tokio::sync::Mutex<HashSet<String>>,
        observed_pending: tokio::sync::Mutex<Vec<bool>>,
    }

    #[async_trait]
    impl CompletionRootWakeQueue for RecordingRootWake {
        async fn enqueue_completion_resolution(
            &self,
            event: &super::CompletionDecisionResolvedPayloadV1,
        ) -> Result<(), String> {
            let row = delegation_workflow_outbox_event::Entity::find_by_id(&event.event_id)
                .one(&self.db.conn)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "outbox row disappeared before root wake".to_string())?;
            self.observed_pending
                .lock()
                .await
                .push(row.delivered_at.is_none());
            self.event_ids.lock().await.insert(event.event_id.clone());
            Ok(())
        }
    }

    impl TerminalFixture {
        async fn new(source: IntentFixture, write_plan: bool) -> Self {
            Self::new_with_node_id(source, write_plan, "plan-author".into()).await
        }

        async fn new_before_v2_only(source: IntentFixture, write_plan: bool) -> Self {
            Self::new_with_node_id_at_migration(source, write_plan, "plan-author".into(), false)
                .await
        }

        async fn new_with_node_id(
            source: IntentFixture,
            write_plan: bool,
            author_node_id: String,
        ) -> Self {
            Self::new_with_node_id_at_migration(source, write_plan, author_node_id, true).await
        }

        async fn new_with_node_id_at_migration(
            source: IntentFixture,
            write_plan: bool,
            author_node_id: String,
            install_v2_only_triggers: bool,
        ) -> Self {
            let workspace = tempfile::tempdir().expect("workspace");
            let workspace_path = workspace.path().to_path_buf();
            let design_path = workspace_path.join(DESIGN_REL_PATH);
            std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
            std::fs::write(&design_path, DESIGN_BYTES).unwrap();
            if write_plan {
                let plan_path = workspace_path.join(PLAN_REL_PATH);
                std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
                std::fs::write(&plan_path, b"# Plan\n\nInitial.\n").unwrap();
            }

            let db = Arc::new(if install_v2_only_triggers {
                fresh_in_memory_db().await
            } else {
                crate::db::test_helpers::historical_completion_protocol_db_before_v2_only().await
            });
            let folder = seed_folder(&db, workspace_path.to_str().unwrap()).await;
            let parent = seed_conversation(&db, folder, AgentType::Codex).await;
            let child = seed_conversation(&db, folder, AgentType::Codex).await;
            let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
                rel_plan_path: PLAN_REL_PATH,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap();
            let document = skeleton_document("task-10", &author_key, &author_node_id);
            let published = publish_workflow_manifest_core(
                &db,
                &EventEmitter::Noop,
                parent,
                PublishWorkflowRequest { document },
            )
            .await
            .expect("publish workflow");
            let workflow_id = published.workflow_id;
            let header = delegation_workflow::Entity::find_by_id(workflow_id.clone())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut active: delegation_workflow::ActiveModel = header.into();
            active.completion_protocol_version = Set(2);
            active.completion_protocol_mode = Set(CompletionProtocolMode::V2Enforce);
            active.update(&db.conn).await.unwrap();

            let task_id = format!("task-10-{}", uuid::Uuid::new_v4());
            let runs = RunStore::new(db.clone());
            runs.admit_gen1_reserving(ReservingRunInsert {
                task_id: task_id.clone(),
                root_task_id: task_id.clone(),
                previous_task_id: None,
                generation: 1,
                parent_conversation_id: parent,
                parent_tool_use_id: Some(format!("tool-{task_id}")),
                child_conversation_id: child,
                agent_type: "codex".into(),
                profile_id: None,
                workspace_path: Some(workspace_path.to_string_lossy().into_owned()),
                route_fingerprint: Some("task-10-route".into()),
                launch_snapshot_version: Some("v1".into()),
                mode_id: None,
                config_values_json: Some("{}".into()),
                task_preview: Some("Task 10".into()),
                request_fingerprint: Some(format!("fp-{task_id}")),
                admission_class: AdmissionClass::NormalRevision,
                lineage_root_task_id: task_id.clone(),
                work_unit_key: Some(author_key),
                history_only: false,
                replaced_task_id: None,
                replacement_reason: None,
                started_at: Some(Utc::now()),
            })
            .await
            .expect("admit Plan Author");

            let run = delegation_task_run::Entity::find_by_id(task_id.clone())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut active: delegation_task_run::ActiveModel = run.into();
            active.status = Set(DelegationRunStatus::Completed);
            active.finished_at = Set(Some(Utc::now()));
            active.card_summary_json = Set(Some(
                r#"{"kind":"author","status":"done","plan_digest":"model"}"#.into(),
            ));
            active.update(&db.conn).await.unwrap();

            if matches!(source, IntentFixture::Tool) {
                delegation_completion_tool_intent::ActiveModel {
                    intent_id: Set(format!("intent-{task_id}")),
                    task_id: Set(task_id.clone()),
                    child_tool_call_id: Set(format!("call-{task_id}")),
                    accepted_ordinal: Set(1),
                    outcome: Set(CompletionOutcome::Done.as_str().into()),
                    summary: Set(Some("tool summary".into())),
                    report_hint: Set(None),
                    request_digest: Set("digest".into()),
                    created_at: Set(Utc::now()),
                }
                .insert(&db.conn)
                .await
                .unwrap();
            }

            let (final_assistant_text, pre_read_reports) = match source {
                IntentFixture::Tool => ("tool selected".into(), Vec::new()),
                IntentFixture::AssistantText => {
                    ("Conclusion: done\n\nassistant summary".into(), Vec::new())
                }
                IntentFixture::Report => (
                    "See the report.".into(),
                    vec![ValidatedReportCandidate {
                        path: "reports/task-10.md".into(),
                        contents: "# Conclusion\n\ndone\n".into(),
                        summary: Some("report summary".into()),
                    }],
                ),
                IntentFixture::Missing => ("No explicit conclusion.".into(), Vec::new()),
            };
            let input = TerminalCompletionInput {
                task_id: task_id.clone(),
                terminal_status: DelegationRunStatus::Completed,
                final_assistant_text,
                pre_read_reports,
                pre_read_artifact: None,
            };
            Self {
                db,
                _workspace: workspace,
                workspace_path,
                task_id,
                workflow_id,
                parent_conversation_id: parent,
                input,
            }
        }

        async fn materialize(&self) -> super::TerminalCompletionResult {
            let txn = self.db.conn.begin().await.unwrap();
            let result = materialize_terminal_completion_txn(&txn, self.input.clone())
                .await
                .unwrap();
            txn.commit().await.unwrap();
            result
        }

        async fn stored_run(&self) -> delegation_task_run::Model {
            delegation_task_run::Entity::find_by_id(self.task_id.clone())
                .one(&self.db.conn)
                .await
                .unwrap()
                .unwrap()
        }
    }

    fn reset_projection_load_counters() {
        reset_completion_projection_load_count();
        reset_completion_context_prepare_counts();
        super::reset_terminal_row_load_count();
        super::reset_preloaded_completion_validation_count();
    }

    async fn assert_projection_counter_instrumentation(fixture: &TerminalFixture) {
        reset_projection_load_counters();
        let completion = load_completion_projection(&fixture.db.conn, &fixture.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(completion.card.state, CompletionCardState::Resolved);
        assert_eq!(
            completion_projection_load_count(),
            1,
            "the standalone projection counter must observe its own loader"
        );
        assert_eq!(
            super::preloaded_completion_validation_count(),
            1,
            "the validation counter must observe resolved evidence validation"
        );
        assert_eq!(super::terminal_row_load_count(), 0);
        assert_eq!(
            completion_context_prepare_counts(),
            (1, 1),
            "the standalone loader prepares its own manifest and requirements context"
        );
    }

    fn assert_batched_projection_load_counts() {
        assert_eq!(
            completion_projection_load_count(),
            0,
            "workflow projection must not call the standalone per-task loader"
        );
        assert_eq!(
            super::terminal_row_load_count(),
            0,
            "workflow projection must reuse preloaded terminal rows"
        );
        assert_eq!(
            super::preloaded_completion_validation_count(),
            1,
            "resolved evidence must be validated exactly once and reused"
        );
        assert_eq!(
            completion_context_prepare_counts(),
            (0, 1),
            "workflow projection must reuse its normalized manifest and prepare requirements once"
        );
    }

    fn skeleton_document(token: &str, author_key: &str, author_node_id: &str) -> ManifestDocument {
        let design_key = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: DESIGN_REL_PATH,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
            plan_target_rel_path: PLAN_REL_PATH.into(),
            risk_policy_version: "b2d_task_risk_v1".into(),
            workflow_id: None,
            expected_manifest_revision: None,
            publication_token: token.into(),
            workflow_state: ManifestWorkflowState::Skeleton,
            design: Some(DocumentRef {
                rel_path: DESIGN_REL_PATH.into(),
                digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
            }),
            plan: None,
            phases: [PHASE_DESIGN, PHASE_PLAN, PHASE_TASKS, PHASE_FINAL]
                .into_iter()
                .map(|id| ManifestPhase {
                    id: id.into(),
                    kind: Some(id.into()),
                    title: None,
                })
                .collect(),
            nodes: vec![
                ManifestNode {
                    id: "design-reviewer".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some(PHASE_DESIGN.into()),
                    role: Some(ManifestNodeRole::Reviewer),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(design_key),
                    deps: Vec::new(),
                    required: Some(true),
                    node_outcome: None,
                    title: None,
                },
                ManifestNode {
                    id: author_node_id.into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some(PHASE_PLAN.into()),
                    role: Some(ManifestNodeRole::Author),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(author_key.into()),
                    deps: Vec::new(),
                    required: Some(true),
                    node_outcome: None,
                    title: None,
                },
            ],
            edges: Vec::new(),
            gates: vec![ManifestGate {
                id: "design".into(),
                reviewer_cohort_node_ids: vec!["design-reviewer".into()],
                required_reviewer_node_ids: vec!["design-reviewer".into()],
                resolution_mode: ResolutionMode::ParentAdjudication,
                gate_kind: Some(DocumentGateKind::Design),
            }],
            task_policies: Vec::new(),
        }
    }

    #[tokio::test]
    async fn resolved_graph_projection_reuses_preloaded_completion_rows_and_validation() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let materialized = fixture.materialize().await;
        assert_eq!(materialized.state, CompletionState::Resolved);
        assert_projection_counter_instrumentation(&fixture).await;

        reset_projection_load_counters();
        let snapshot = project_workflow_graph_core(&fixture.db, fixture.parent_conversation_id)
            .await
            .unwrap();
        let completion = snapshot
            .nodes
            .iter()
            .find(|node| node.latest_task_id.as_deref() == Some(fixture.task_id.as_str()))
            .and_then(|node| node.completion.as_ref())
            .expect("resolved node completion");
        assert_eq!(completion.card.state, CompletionCardState::Resolved);
        assert!(completion.card.evidence_validated);
        assert_batched_projection_load_counts();
    }

    #[tokio::test]
    async fn resolved_workflow_state_projection_reuses_preloaded_completion_rows_and_validation() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let materialized = fixture.materialize().await;
        assert_eq!(materialized.state, CompletionState::Resolved);
        assert_projection_counter_instrumentation(&fixture).await;

        reset_projection_load_counters();
        let state = get_workflow_state_core(
            &fixture.db,
            fixture.parent_conversation_id,
            Some(&fixture.workflow_id),
        )
        .await
        .unwrap();
        let completion = state
            .nodes
            .iter()
            .find(|node| node.latest_task_id.as_deref() == Some(fixture.task_id.as_str()))
            .and_then(|node| node.completion.as_ref())
            .expect("resolved node completion");
        assert_eq!(completion.card.state, CompletionCardState::Resolved);
        assert!(completion.card.evidence_validated);
        assert_batched_projection_load_counts();
    }

    #[tokio::test]
    async fn terminal_materialization_persists_identical_platform_evidence_for_each_channel() {
        let mut digests = Vec::new();
        for (source, expected) in [
            (IntentFixture::Tool, CompletionIntentSource::CompleteWork),
            (
                IntentFixture::AssistantText,
                CompletionIntentSource::AssistantConclusion,
            ),
            (IntentFixture::Report, CompletionIntentSource::Report),
        ] {
            let fixture = TerminalFixture::new(source, true).await;
            let result = fixture.materialize().await;
            assert_eq!(result.state, CompletionState::Resolved);
            let evidence = result.evidence.unwrap();
            assert_eq!(evidence.binding.workflow_id, fixture.workflow_id);
            assert_eq!(evidence.binding.task_id, fixture.task_id);
            assert_eq!(evidence.intent.source, expected);
            digests.push(evidence.artifact.digest().to_string());
            assert_eq!(fixture.stored_run().await.card_summary_json, None);
        }
        assert!(digests.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[tokio::test]
    async fn completion_v2_shared_validator_ignores_cards_and_legacy_binding_projection() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let materialized = fixture.materialize().await;
        assert_eq!(materialized.state, CompletionState::Resolved);

        let run = fixture.stored_run().await;
        let mut run: delegation_task_run::ActiveModel = run.into();
        run.card_summary_json = Set(Some("{malformed".into()));
        run.update(&fixture.db.conn).await.unwrap();

        let binding = crate::db::entities::delegation_workflow_run_binding::Entity::find_by_id(
            fixture.task_id.clone(),
        )
        .one(&fixture.db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut binding: crate::db::entities::delegation_workflow_run_binding::ActiveModel =
            binding.into();
        binding.summary_validated = Set(false);
        binding.content_fingerprint = Set(Some("rotated-legacy-fingerprint".into()));
        binding.gate_cycle = Set(Some(99));
        binding.update(&fixture.db.conn).await.unwrap();

        let validated =
            super::load_validated_completion_evidence(&fixture.db.conn, &fixture.task_id)
                .await
                .unwrap();
        assert!(validated.evidence_validated);
        assert_eq!(validated.evidence.intent.outcome, CompletionOutcome::Done);
    }

    #[tokio::test]
    async fn completion_recovery_fence_rejects_continue_and_replace_before_run_insertion() {
        for (source, write_plan, expected_code) in [
            (IntentFixture::Missing, true, "completion_decision_required"),
            (
                IntentFixture::AssistantText,
                false,
                "completion_artifact_unavailable",
            ),
        ] {
            let fixture = TerminalFixture::new(source, write_plan).await;
            fixture.materialize().await;
            let before = delegation_task_run::Entity::find()
                .all(&fixture.db.conn)
                .await
                .unwrap()
                .len();

            let error = RunStore::new(fixture.db.clone())
                .admit_continue_reserving(ContinueRunAdmission {
                    task_id: format!("{}-continue", fixture.task_id),
                    parent_conversation_id: fixture.parent_conversation_id,
                    parent_tool_use_id: format!("continue-{}", fixture.task_id),
                    target_task_id: fixture.task_id.clone(),
                    task_preview: "must be fenced".into(),
                    request_fingerprint: format!("continue-fp-{}", fixture.task_id),
                    work_unit_key: None,
                })
                .await
                .unwrap_err();
            assert_eq!(error.workflow_admission_code(), Some(expected_code));

            let source_run = fixture.stored_run().await;
            let replacement_task_id = format!("{}-replacement", fixture.task_id);
            let replacement_error = RunStore::new(fixture.db.clone())
                .admit_gen1_reserving(ReservingRunInsert {
                    task_id: replacement_task_id.clone(),
                    root_task_id: replacement_task_id.clone(),
                    previous_task_id: None,
                    generation: 1,
                    parent_conversation_id: fixture.parent_conversation_id,
                    parent_tool_use_id: Some(format!("replace-{}", fixture.task_id)),
                    child_conversation_id: source_run.child_conversation_id,
                    agent_type: source_run.agent_type,
                    profile_id: source_run.profile_id,
                    workspace_path: source_run.workspace_path,
                    route_fingerprint: source_run.route_fingerprint,
                    launch_snapshot_version: source_run.launch_snapshot_version,
                    mode_id: source_run.mode_id,
                    config_values_json: source_run.config_values_json,
                    task_preview: Some("must be fenced".into()),
                    request_fingerprint: Some(format!("replace-fp-{}", fixture.task_id)),
                    admission_class: AdmissionClass::Replacement,
                    lineage_root_task_id: source_run.lineage_root_task_id,
                    work_unit_key: source_run.work_unit_key,
                    history_only: false,
                    replaced_task_id: Some(fixture.task_id.clone()),
                    replacement_reason: Some("unresumable".into()),
                    started_at: Some(Utc::now()),
                })
                .await
                .unwrap_err();
            assert_eq!(
                replacement_error.workflow_admission_code(),
                Some(expected_code)
            );
            assert_eq!(
                delegation_task_run::Entity::find()
                    .all(&fixture.db.conn)
                    .await
                    .unwrap()
                    .len(),
                before
            );
        }

        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        fixture.materialize().await;
        let run = fixture.stored_run().await;
        let mut run: delegation_task_run::ActiveModel = run.into();
        run.workspace_path = Set(None);
        run.update(&fixture.db.conn).await.unwrap();
        let error = super::ensure_task_completion_recovery_not_fenced_txn(
            &fixture.db.conn,
            &fixture.task_id,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error,
            super::CompletionRecoveryFenceError::ArtifactUnavailable
        );

        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        fixture.materialize().await;
        let run = fixture.stored_run().await;
        let mut run: delegation_task_run::ActiveModel = run.into();
        run.completion_state = Set(None);
        run.update(&fixture.db.conn).await.unwrap();
        let error = super::ensure_task_completion_recovery_not_fenced_txn(
            &fixture.db.conn,
            &fixture.task_id,
        )
        .await
        .unwrap_err();
        assert_eq!(error, super::CompletionRecoveryFenceError::DecisionRequired);
    }

    #[tokio::test]
    async fn terminal_materialization_rehashes_documents_inside_the_write_critical_section() {
        let mut fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let pre_read = resolve_document(&fixture.workspace_path, PLAN_REL_PATH, 2 * 1024 * 1024)
            .await
            .unwrap()
            .into();
        fixture.input.pre_read_artifact = Some(pre_read);
        std::fs::write(
            fixture.workspace_path.join(PLAN_REL_PATH),
            b"# Plan\n\nChanged after pre-read.\n",
        )
        .unwrap();
        let current = resolve_document(&fixture.workspace_path, PLAN_REL_PATH, 2 * 1024 * 1024)
            .await
            .unwrap();
        let result = fixture.materialize().await;
        assert_eq!(result.evidence.unwrap().artifact.digest(), current.digest());
    }

    #[tokio::test]
    async fn unclear_completion_opens_one_terminal_decision_without_child_continuation() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let first = fixture.materialize().await;
        let replay = fixture.materialize().await;
        assert_eq!(first.state, CompletionState::NeedsDecision);
        assert_eq!(first.attention, replay.attention);
        let count = delegation_attention_request::Entity::find()
            .all(&fixture.db.conn)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.status == "open" && row.kind == AttentionKind::CompletionDecision)
            .count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn typed_completion_attention_adjudication_enforces_owner_kind_role_cas_and_replay() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let cas = fixture.materialize().await.attention.unwrap();

        let foreign = resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id + 1,
            cas.clone(),
            CompletionOutcome::Done,
            "application_user",
        )
        .await
        .unwrap_err();
        assert_eq!(foreign, CompletionMutationError::Unauthorized);

        let mut wrong_kind = cas.clone();
        wrong_kind.kind = AttentionKind::CompletionArtifactRecovery;
        let wrong_kind = resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            wrong_kind,
            CompletionOutcome::Done,
            "application_user",
        )
        .await
        .unwrap_err();
        assert_eq!(wrong_kind, CompletionMutationError::KindMismatch);

        let role_mismatch = resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas.clone(),
            CompletionOutcome::Approve,
            "application_user",
        )
        .await
        .unwrap_err();
        assert_eq!(role_mismatch, CompletionMutationError::RoleMismatch);

        for stale in [
            {
                let mut value = cas.clone();
                value.attention_id.push_str("-stale");
                value
            },
            {
                let mut value = cas.clone();
                value.task_id.push_str("-stale");
                value
            },
            {
                let mut value = cas.clone();
                value.captured_scope_digest = format!("sha256:{}", "f".repeat(64));
                value
            },
            {
                let mut value = cas.clone();
                value.latest_run_id.push_str("-stale");
                value
            },
            {
                let mut value = cas.clone();
                value.node_id.push_str("-stale");
                value
            },
        ] {
            let error = resolve_completion_decision_txn(
                &fixture.db,
                fixture.parent_conversation_id,
                stale,
                CompletionOutcome::Done,
                "application_user",
            )
            .await
            .unwrap_err();
            assert_eq!(error, CompletionMutationError::Superseded);
        }

        let first = resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas.clone(),
            CompletionOutcome::Done,
            "application_user",
        )
        .await
        .unwrap();
        assert!(!first.idempotent_replay);
        let replay = resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas.clone(),
            CompletionOutcome::Done,
            "application_user",
        )
        .await
        .unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(first.graph_revision, replay.graph_revision);
        assert_eq!(
            delegation_workflow_outbox_event::Entity::find()
                .filter(
                    delegation_workflow_outbox_event::Column::EventKind
                        .eq(super::COMPLETION_DECISION_RESOLVED_EVENT),
                )
                .all(&fixture.db.conn)
                .await
                .unwrap()
                .len(),
            1
        );

        let conflict = resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas,
            CompletionOutcome::Blocked,
            "application_user",
        )
        .await
        .unwrap_err();
        assert_eq!(conflict, CompletionMutationError::Conflict);
    }

    #[tokio::test]
    async fn long_valid_node_id_projects_a_bounded_actionable_completion_cas() {
        let raw_node_id = format!("review/path/{}", "n".repeat(9_000));
        let fixture =
            TerminalFixture::new_with_node_id(IntentFixture::Missing, true, raw_node_id.clone())
                .await;
        let cas = fixture.materialize().await.attention.unwrap();

        assert_eq!(
            cas.node_id,
            crate::acp::delegation::workflow::safe_public_id(&raw_node_id)
        );
        assert!(cas.node_id.len() <= 128);

        let resolved = resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas.clone(),
            CompletionOutcome::Done,
            "application_user",
        )
        .await
        .unwrap();
        assert_eq!(resolved.outcome, CompletionOutcome::Done);

        let event = delegation_workflow_outbox_event::Entity::find()
            .filter(
                delegation_workflow_outbox_event::Column::EventKind
                    .eq(super::COMPLETION_DECISION_RESOLVED_EVENT),
            )
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let payload: super::CompletionDecisionResolvedPayloadV1 =
            serde_json::from_str(&event.payload_json).unwrap();
        assert_eq!(payload.node_id, cas.node_id);
    }

    #[tokio::test]
    async fn typed_completion_attention_artifact_retry_is_typed_and_records_scope_invalidation() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, false).await;
        let cas = fixture.materialize().await.attention.unwrap();
        let metrics = DelegationMetrics::default();

        let foreign = retry_completion_artifact_for_user_txn(
            &fixture.db,
            fixture.parent_conversation_id + 1,
            cas.clone(),
            &metrics,
        )
        .await
        .unwrap_err();
        assert_eq!(foreign, CompletionMutationError::Unauthorized);

        let mut wrong_kind = cas.clone();
        wrong_kind.kind = AttentionKind::CompletionDecision;
        let wrong_kind = retry_completion_artifact_for_user_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            wrong_kind,
            &metrics,
        )
        .await
        .unwrap_err();
        assert_eq!(wrong_kind, CompletionMutationError::KindMismatch);

        let plan_path = fixture.workspace_path.join(PLAN_REL_PATH);
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&plan_path, b"# Plan\n\nRecovered.\n").unwrap();
        let first = retry_completion_artifact_for_user_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas.clone(),
            &metrics,
        )
        .await
        .unwrap();
        let replay = retry_completion_artifact_for_user_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas,
            &metrics,
        )
        .await
        .unwrap();
        assert!(!first.idempotent_replay);
        assert!(replay.idempotent_replay);
        assert_eq!(first.graph_revision, replay.graph_revision);
        let durable = load_completion_projection(&fixture.db.conn, &fixture.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.completion.as_ref(), Some(&durable));
        assert_eq!(replay.completion.as_ref(), Some(&durable));

        let stale = TerminalFixture::new(IntentFixture::Tool, false).await;
        let stale_cas = stale.materialize().await.attention.unwrap();
        let binding = crate::db::entities::delegation_workflow_run_binding::Entity::find_by_id(
            stale.task_id.clone(),
        )
        .one(&stale.db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut active: crate::db::entities::delegation_workflow_run_binding::ActiveModel =
            binding.into();
        active.instruction_block_digest = Set(Some(format!("sha256:{}", "f".repeat(64))));
        active.update(&stale.db.conn).await.unwrap();
        let stale_metrics = DelegationMetrics::default();
        let error = retry_completion_artifact_for_user_txn(
            &stale.db,
            stale.parent_conversation_id,
            stale_cas.clone(),
            &stale_metrics,
        )
        .await
        .unwrap_err();
        assert_eq!(error, CompletionMutationError::Superseded);
        assert_eq!(
            stale_metrics
                .snapshot()
                .completion_scope_invalidations
                .get("plan:instruction")
                .copied(),
            Some(1)
        );
        let replay_error = retry_completion_artifact_for_user_txn(
            &stale.db,
            stale.parent_conversation_id,
            stale_cas,
            &stale_metrics,
        )
        .await
        .unwrap_err();
        assert_eq!(replay_error, CompletionMutationError::Superseded);
        assert_eq!(
            stale_metrics
                .snapshot()
                .completion_scope_invalidations
                .get("plan:instruction")
                .copied(),
            Some(1),
            "replaying an already-superseded retry must not double-count"
        );
    }

    #[tokio::test]
    async fn typed_completion_attention_decision_wakes_root_when_artifact_recovery_opens() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, false).await;
        let cas = fixture.materialize().await.attention.unwrap();
        let result = resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas,
            CompletionOutcome::Done,
            "application_user",
        )
        .await
        .unwrap();
        assert_eq!(
            result.completion.as_ref().unwrap().card.state,
            CompletionCardState::Blocked
        );
        let events = delegation_workflow_outbox_event::Entity::find()
            .filter(
                delegation_workflow_outbox_event::Column::EventKind
                    .eq(super::COMPLETION_DECISION_RESOLVED_EVENT),
            )
            .all(&fixture.db.conn)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].graph_revision, result.graph_revision as i64);
    }

    #[tokio::test]
    async fn typed_completion_attention_design_self_review_is_typed_and_replayable() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        delegation_workflow_gate_state::ActiveModel {
            workflow_id: Set(fixture.workflow_id.clone()),
            gate_id: Set("design".into()),
            gate_lineage: Set("design-lineage-1".into()),
            current_review_round: Set(1),
            selected_node_ids_json: Set(r#"["design-reviewer"]"#.into()),
        }
        .insert(&fixture.db.conn)
        .await
        .unwrap();
        let binding = delegation_workflow_design_root_binding::ActiveModel {
            workflow_id: Set(fixture.workflow_id.clone()),
            gate_id: Set("design".into()),
            gate_lineage: Set("design-lineage-1".into()),
            node_id: Set("design-reviewer".into()),
            task_id: Set("design-root-task-1".into()),
            latest_run_id: Set("design-root-run-1".into()),
            design_identity: Set(format!("sha256:{}", "d".repeat(64))),
            evidence_scope_digest: Set(format!("sha256:{}", "e".repeat(64))),
            graph_revision: Set(1),
        }
        .insert(&fixture.db.conn)
        .await
        .unwrap();
        let cas = open_design_self_review_decision_txn(
            &fixture.db.conn,
            &binding,
            fixture.parent_conversation_id,
        )
        .await
        .unwrap();

        let first = resolve_design_self_review_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas.clone(),
            CompletionOutcome::Approve,
            "application_user",
        )
        .await
        .unwrap();
        let replay = resolve_design_self_review_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas,
            CompletionOutcome::Approve,
            "application_user",
        )
        .await
        .unwrap();
        assert_eq!(first.graph_revision, replay.graph_revision);
        assert!(replay.idempotent_replay);
        let durable = load_completion_projection(&fixture.db.conn, &binding.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.completion.as_ref(), Some(&durable));
        assert_eq!(replay.completion.as_ref(), Some(&durable));
        let state = get_workflow_state_core(
            &fixture.db,
            fixture.parent_conversation_id,
            Some(&fixture.workflow_id),
        )
        .await
        .unwrap();
        assert_eq!(state.completion.as_ref(), Some(&durable));
    }

    async fn set_fixture_protocol(
        fixture: &TerminalFixture,
        version: i64,
        mode: CompletionProtocolMode,
    ) {
        let workflow = delegation_workflow::Entity::find_by_id(&fixture.workflow_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut workflow: delegation_workflow::ActiveModel = workflow.into();
        workflow.completion_protocol_version = Set(version);
        workflow.completion_protocol_mode = Set(mode);
        workflow.update(&fixture.db.conn).await.unwrap();
        crate::db::test_helpers::complete_historical_completion_protocol_migrations(&fixture.db)
            .await;
    }

    async fn completion_mutation_snapshot(fixture: &TerminalFixture) -> String {
        let workflow = delegation_workflow::Entity::find_by_id(&fixture.workflow_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let run = delegation_task_run::Entity::find_by_id(&fixture.task_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let attentions = delegation_attention_request::Entity::find()
            .filter(
                delegation_attention_request::Column::ParentConversationId
                    .eq(fixture.parent_conversation_id),
            )
            .order_by_asc(delegation_attention_request::Column::RequestId)
            .all(&fixture.db.conn)
            .await
            .unwrap();
        let outbox = delegation_workflow_outbox_event::Entity::find()
            .filter(delegation_workflow_outbox_event::Column::WorkflowId.eq(&fixture.workflow_id))
            .order_by_asc(delegation_workflow_outbox_event::Column::EventId)
            .all(&fixture.db.conn)
            .await
            .unwrap();
        format!("{workflow:?}|{run:?}|{attentions:?}|{outbox:?}")
    }

    #[tokio::test]
    async fn historical_protocol_mutation_matrix_completion_evidence() {
        use CompletionProtocolMode::{V2Enforce, V2Shadow, V1};

        for (index, version, mode, expected_code) in [
            (0, 1, V1, "legacy_completion_protocol_read_only"),
            (1, 1, V2Shadow, "legacy_completion_protocol_read_only"),
            (2, 1, V2Enforce, "legacy_completion_protocol_read_only"),
            (3, 2, V1, "unsupported_completion_protocol"),
            (4, 2, V2Shadow, "unsupported_completion_protocol"),
        ] {
            let decision = TerminalFixture::new_before_v2_only(IntentFixture::Missing, true).await;
            let decision_cas = decision.materialize().await.attention.unwrap();
            set_fixture_protocol(&decision, version, mode.clone()).await;
            let before = completion_mutation_snapshot(&decision).await;
            let error = resolve_completion_decision_txn(
                &decision.db,
                decision.parent_conversation_id,
                decision_cas,
                CompletionOutcome::Done,
                "application_user",
            )
            .await
            .expect_err("rejected completion decision must not mutate state");
            assert_eq!(error.code(), expected_code, "pair index {index}");
            assert_eq!(completion_mutation_snapshot(&decision).await, before);

            let artifact =
                TerminalFixture::new_before_v2_only(IntentFixture::AssistantText, false).await;
            let artifact_cas = artifact.materialize().await.attention.unwrap();
            set_fixture_protocol(&artifact, version, mode.clone()).await;
            let before = completion_mutation_snapshot(&artifact).await;
            let error = retry_completion_artifact_for_user_txn(
                &artifact.db,
                artifact.parent_conversation_id,
                artifact_cas,
                &DelegationMetrics::default(),
            )
            .await
            .expect_err("rejected artifact retry must not mutate state");
            assert_eq!(error.code(), expected_code, "pair index {index}");
            assert_eq!(completion_mutation_snapshot(&artifact).await, before);

            let design = TerminalFixture::new_before_v2_only(IntentFixture::Missing, true).await;
            let binding = delegation_workflow_design_root_binding::ActiveModel {
                workflow_id: Set(design.workflow_id.clone()),
                gate_id: Set("design".into()),
                gate_lineage: Set(format!("task-4-design-lineage-{index}")),
                node_id: Set("design-reviewer".into()),
                task_id: Set(format!("task-4-design-root-task-{index}")),
                latest_run_id: Set(format!("task-4-design-root-run-{index}")),
                design_identity: Set(format!("sha256:{}", "d".repeat(64))),
                evidence_scope_digest: Set(format!("sha256:{}", "e".repeat(64))),
                graph_revision: Set(1),
            }
            .insert(&design.db.conn)
            .await
            .unwrap();
            set_fixture_protocol(&design, version, mode.clone()).await;
            let before = completion_mutation_snapshot(&design).await;
            let error = open_design_self_review_decision_txn(
                &design.db.conn,
                &binding,
                design.parent_conversation_id,
            )
            .await
            .expect_err("rejected Design self-review attention must not open");
            assert_eq!(error.code(), expected_code, "pair index {index}");
            assert_eq!(completion_mutation_snapshot(&design).await, before);

            let design_resolution =
                TerminalFixture::new_before_v2_only(IntentFixture::Missing, true).await;
            let binding = delegation_workflow_design_root_binding::ActiveModel {
                workflow_id: Set(design_resolution.workflow_id.clone()),
                gate_id: Set("design".into()),
                gate_lineage: Set(format!("task-4-design-resolution-lineage-{index}")),
                node_id: Set("design-reviewer".into()),
                task_id: Set(format!("task-4-design-resolution-task-{index}")),
                latest_run_id: Set(format!("task-4-design-resolution-run-{index}")),
                design_identity: Set(format!("sha256:{}", "a".repeat(64))),
                evidence_scope_digest: Set(format!("sha256:{}", "b".repeat(64))),
                graph_revision: Set(1),
            }
            .insert(&design_resolution.db.conn)
            .await
            .unwrap();
            let cas = open_design_self_review_decision_txn(
                &design_resolution.db.conn,
                &binding,
                design_resolution.parent_conversation_id,
            )
            .await
            .unwrap();
            set_fixture_protocol(&design_resolution, version, mode).await;
            let before = completion_mutation_snapshot(&design_resolution).await;
            let error = resolve_design_self_review_txn(
                &design_resolution.db,
                design_resolution.parent_conversation_id,
                cas,
                CompletionOutcome::Approve,
                "application_user",
            )
            .await
            .expect_err("rejected Design self-review resolution must not mutate state");
            assert_eq!(error.code(), expected_code, "pair index {index}");
            assert_eq!(
                completion_mutation_snapshot(&design_resolution).await,
                before
            );
        }
    }

    #[tokio::test]
    async fn corrupt_open_completion_attention_fails_closed_across_projections() {
        let scope_fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let scope_cas = scope_fixture.materialize().await.attention.unwrap();
        let scope_attention =
            delegation_attention_request::Entity::find_by_id(scope_cas.attention_id.clone())
                .one(&scope_fixture.db.conn)
                .await
                .unwrap()
                .unwrap();
        let mut active: delegation_attention_request::ActiveModel = scope_attention.into();
        active.captured_scope_digest = Set(Some(format!("sha256:{}", "f".repeat(64))));
        active.update(&scope_fixture.db.conn).await.unwrap();
        assert!(
            load_completion_projection(&scope_fixture.db.conn, &scope_fixture.task_id)
                .await
                .is_err()
        );

        let payload_fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let payload_cas = payload_fixture.materialize().await.attention.unwrap();
        let payload_attention =
            delegation_attention_request::Entity::find_by_id(payload_cas.attention_id)
                .one(&payload_fixture.db.conn)
                .await
                .unwrap()
                .unwrap();
        let mut active: delegation_attention_request::ActiveModel = payload_attention.into();
        active.payload_json = Set(Some("{}".into()));
        active.update(&payload_fixture.db.conn).await.unwrap();

        assert!(
            load_completion_projection(&payload_fixture.db.conn, &payload_fixture.task_id)
                .await
                .is_err()
        );
        assert!(project_workflow_graph_core(
            &payload_fixture.db,
            payload_fixture.parent_conversation_id,
        )
        .await
        .is_none());
        assert!(get_workflow_state_core(
            &payload_fixture.db,
            payload_fixture.parent_conversation_id,
            Some(&payload_fixture.workflow_id),
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn typed_completion_attention_outbox_replays_after_commit_and_dedupes_root_wake() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let cas = fixture.materialize().await.attention.unwrap();
        resolve_completion_decision_txn(
            &fixture.db,
            fixture.parent_conversation_id,
            cas,
            CompletionOutcome::Done,
            "application_user",
        )
        .await
        .unwrap();

        let outbox = delegation_workflow_outbox_event::Entity::find()
            .filter(
                delegation_workflow_outbox_event::Column::EventKind
                    .eq(super::COMPLETION_DECISION_RESOLVED_EVENT),
            )
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(outbox.dispatch_attempts, 0);
        assert!(outbox.delivered_at.is_none());

        let wake = Arc::new(RecordingRootWake {
            db: fixture.db.clone(),
            event_ids: tokio::sync::Mutex::new(HashSet::new()),
            observed_pending: tokio::sync::Mutex::new(Vec::new()),
        });
        let dispatcher = CompletionOutboxDispatcher::new(fixture.db.clone(), EventEmitter::Noop)
            .with_root_wake(wake.clone());
        assert_eq!(dispatcher.dispatch_pending().await.unwrap(), 2);

        let delivered = delegation_workflow_outbox_event::Entity::find_by_id(&outbox.event_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivered.dispatch_attempts, 1);
        assert!(delivered.delivered_at.is_some());
        assert_eq!(wake.event_ids.lock().await.len(), 1);
        assert!(wake
            .observed_pending
            .lock()
            .await
            .iter()
            .all(|pending| *pending));

        let mut redelivery: delegation_workflow_outbox_event::ActiveModel = delivered.into();
        redelivery.delivered_at = Set(None);
        redelivery.update(&fixture.db.conn).await.unwrap();
        assert_eq!(dispatcher.dispatch_pending().await.unwrap(), 1);
        assert_eq!(wake.event_ids.lock().await.len(), 1);
        let replayed = delegation_workflow_outbox_event::Entity::find_by_id(&outbox.event_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replayed.dispatch_attempts, 2);
        assert!(replayed.delivered_at.is_some());
    }

    #[tokio::test]
    async fn typed_completion_attention_startup_reconciliation_retains_current_and_replaces_stale()
    {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let original = fixture.materialize().await.attention.unwrap();

        let retained = reconcile_completion_attentions_txn(&fixture.db)
            .await
            .unwrap();
        assert_eq!(retained.retained, 1);
        assert_eq!(retained.superseded, 0);
        assert_eq!(retained.reopened, 0);

        let binding = crate::db::entities::delegation_workflow_run_binding::Entity::find_by_id(
            fixture.task_id.clone(),
        )
        .one(&fixture.db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut active: crate::db::entities::delegation_workflow_run_binding::ActiveModel =
            binding.into();
        active.instruction_block_digest = Set(Some(format!("sha256:{}", "f".repeat(64))));
        active.update(&fixture.db.conn).await.unwrap();

        let reconciled = reconcile_completion_attentions_txn(&fixture.db)
            .await
            .unwrap();
        assert_eq!(reconciled.retained, 0);
        assert_eq!(reconciled.superseded, 1);
        assert_eq!(reconciled.reopened, 1);
        let old = delegation_attention_request::Entity::find_by_id(&original.attention_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old.resolution_code.as_deref(), Some("superseded"));
        let replacement = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(&fixture.task_id))
            .filter(
                delegation_attention_request::Column::Kind.eq(AttentionKind::CompletionDecision),
            )
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(replacement.request_id, original.attention_id);
        assert_eq!(
            replacement.captured_scope_digest.as_deref(),
            Some(original.captured_scope_digest.as_str())
        );
    }

    #[tokio::test]
    async fn typed_completion_attention_blocked_survives_but_explicit_termination_closes() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let original = fixture.materialize().await.attention.unwrap();
        let workflow = delegation_workflow::Entity::find_by_id(&fixture.workflow_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut blocked: delegation_workflow::ActiveModel = workflow.into();
        blocked.workflow_state =
            Set(crate::db::entities::delegation_workflow::WorkflowState::Blocked);
        blocked.update(&fixture.db.conn).await.unwrap();

        let retained = reconcile_completion_attentions_txn(&fixture.db)
            .await
            .unwrap();
        assert_eq!(retained.retained, 1);
        let resolved = resolve_workflow_completion_attentions_txn(
            &fixture.db,
            &fixture.workflow_id,
            crate::acp::delegation::attention::CompletionAttentionResolutionCode::WorkflowTerminated,
        )
        .await
        .unwrap();
        assert_eq!(resolved, 1);
        let row = delegation_attention_request::Entity::find_by_id(original.attention_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.resolution_code.as_deref(), Some("workflow_terminated"));
    }

    #[tokio::test]
    async fn task14_fix_incomplete_final_evaluation_keeps_active_package() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let package = seed_active_final_package(&fixture).await;
        let loaded = super::load_terminal(&fixture.db.conn, &fixture.task_id)
            .await
            .unwrap();

        super::apply_final_findings_terminal_action(
            &fixture.db.conn,
            &loaded,
            super::FinalFindingsTerminalAction::Incomplete,
            2,
        )
        .await
        .unwrap();

        assert_eq!(
            load_active_final_findings_package_v1(
                &fixture.db.conn,
                &fixture.workflow_id,
                "final",
                &package.gate_lineage,
            )
            .await
            .unwrap()
            .unwrap()
            .package_digest,
            package.package_digest
        );
    }

    #[tokio::test]
    async fn task14_fix_terminal_cleanup_resolves_package_without_attention() {
        let fixture = TerminalFixture::new(IntentFixture::Tool, true).await;
        let package = seed_active_final_package(&fixture).await;

        let resolved = resolve_workflow_completion_attentions_txn(
            &fixture.db,
            &fixture.workflow_id,
            crate::acp::delegation::attention::CompletionAttentionResolutionCode::WorkflowTerminated,
        )
        .await
        .unwrap();

        assert_eq!(resolved, 0);
        assert!(load_active_final_findings_package_v1(
            &fixture.db.conn,
            &fixture.workflow_id,
            "final",
            &package.gate_lineage,
        )
        .await
        .unwrap()
        .is_none());
    }

    #[tokio::test]
    async fn typed_completion_attention_root_deletion_path_closes_as_workflow_deleted() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let original = fixture.materialize().await.attention.unwrap();
        let coordinator =
            crate::auto_title::AutoTitleCoordinator::new_inert_for_test(fixture.db.conn.clone());

        crate::commands::conversations::delete_conversation_core(
            &fixture.db.conn,
            coordinator.as_ref(),
            fixture.parent_conversation_id,
        )
        .await
        .unwrap();

        let row = delegation_attention_request::Entity::find_by_id(original.attention_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "resolved");
        assert_eq!(row.resolution_code.as_deref(), Some("workflow_deleted"));
    }

    #[tokio::test]
    async fn typed_completion_attention_reconcile_closes_already_deleted_root() {
        use crate::db::entities::conversation;

        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let original = fixture.materialize().await.attention.unwrap();
        let parent = conversation::Entity::find_by_id(fixture.parent_conversation_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut deleted: conversation::ActiveModel = parent.into();
        deleted.deleted_at = Set(Some(Utc::now()));
        deleted.update(&fixture.db.conn).await.unwrap();

        reconcile_completion_attentions_txn(&fixture.db)
            .await
            .unwrap();

        let row = delegation_attention_request::Entity::find_by_id(original.attention_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, "resolved");
        assert_eq!(row.resolution_code.as_deref(), Some("workflow_deleted"));
    }

    #[tokio::test]
    async fn maximum_bounded_ambiguity_commits_terminal_and_one_attention() {
        use crate::acp::delegation::attention::ATTENTION_PAYLOAD_MAX_BYTES;

        let mut fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let run = fixture.stored_run().await;
        let mut active: delegation_task_run::ActiveModel = run.into();
        active.status = Set(DelegationRunStatus::Running);
        active.finished_at = Set(None);
        active.completion_state = Set(None);
        active.completion_outcome = Set(None);
        active.completion_evidence_json = Set(None);
        active.update(&fixture.db.conn).await.unwrap();

        fixture.input.final_assistant_text.clear();
        fixture.input.pre_read_reports = (0..8)
            .map(|index| {
                let suffix = format!("{index}.md");
                let prefix = "reports/";
                let path = format!(
                    "{prefix}{}{suffix}",
                    "x".repeat(1024 - prefix.len() - suffix.len())
                );
                assert_eq!(path.len(), 1024);
                ValidatedReportCandidate {
                    path,
                    contents: "# Conclusion\n\napprove\n".into(),
                    summary: None,
                }
            })
            .collect();
        let first_path = fixture.input.pre_read_reports.first().unwrap().path.clone();
        let last_path = fixture.input.pre_read_reports.last().unwrap().path.clone();

        let runs = RunStore::new(fixture.db.clone());
        let (settlement, completion) = runs
            .settle_terminal_with_completion(
                &fixture.task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
                Some(fixture.input.clone()),
            )
            .await
            .expect("bounded ambiguity must not roll back terminal settlement");
        assert!(matches!(settlement, Settlement::Won(_)));
        assert_eq!(completion.unwrap().state, CompletionState::NeedsDecision);
        assert_eq!(
            fixture.stored_run().await.status,
            DelegationRunStatus::Completed
        );

        let attentions = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(&fixture.task_id))
            .all(&fixture.db.conn)
            .await
            .unwrap();
        assert_eq!(attentions.len(), 1);
        let payload = attentions[0].payload_json.as_deref().unwrap();
        assert!(payload.len() <= ATTENTION_PAYLOAD_MAX_BYTES);
        let payload: CompletionDecisionPayloadV1 = serde_json::from_str(payload).unwrap();
        assert_eq!(payload.reason_code, CompletionIntentReason::RoleMismatch);
        assert!(!payload.bounded_candidates.is_empty());
        assert_eq!(
            payload
                .bounded_candidates
                .first()
                .unwrap()
                .report_file
                .as_deref(),
            Some(first_path.as_str())
        );
        assert_eq!(
            payload
                .bounded_candidates
                .last()
                .unwrap()
                .report_file
                .as_deref(),
            Some(last_path.as_str())
        );
    }

    #[tokio::test]
    async fn artifact_retry_reuses_persisted_intent_and_supersedes_only_scope_changes() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, false).await;
        let opened = fixture.materialize().await;
        assert_eq!(opened.state, CompletionState::ArtifactRecovery);
        let cas = opened.attention.unwrap();
        let row = delegation_attention_request::Entity::find_by_id(cas.attention_id.clone())
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let payload: super::ArtifactRecoveryPayloadV1 =
            serde_json::from_str(row.payload_json.as_deref().unwrap()).unwrap();
        assert_eq!(payload.normalized_intent.outcome, CompletionOutcome::Done);
        assert_eq!(
            payload.normalized_intent.source,
            CompletionIntentSource::AssistantConclusion
        );

        let plan_path = fixture.workspace_path.join(PLAN_REL_PATH);
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&plan_path, b"# Plan\n\nRecovered.\n").unwrap();
        let resolved = retry_completion_artifact_txn(&fixture.db, cas.clone())
            .await
            .unwrap();
        assert_eq!(resolved.state, CompletionState::Resolved);
        assert_eq!(
            resolved.evidence.unwrap().intent.source,
            CompletionIntentSource::AssistantConclusion
        );
        let replay = retry_completion_artifact_txn(&fixture.db, cas.clone())
            .await
            .unwrap();
        assert_eq!(replay.state, CompletionState::Resolved);
        assert_eq!(
            replay.evidence.unwrap().intent.source,
            CompletionIntentSource::AssistantConclusion
        );
        let row = delegation_attention_request::Entity::find_by_id(cas.attention_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.resolution_code.as_deref(), Some("artifact_resolved"));

        let stale = TerminalFixture::new(IntentFixture::Tool, false).await;
        let opened = stale.materialize().await;
        let cas = opened.attention.unwrap();
        let binding = crate::db::entities::delegation_workflow_run_binding::Entity::find_by_id(
            stale.task_id.clone(),
        )
        .one(&stale.db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut active: crate::db::entities::delegation_workflow_run_binding::ActiveModel =
            binding.into();
        active.instruction_block_digest = Set(Some(format!("sha256:{}", "f".repeat(64))));
        active.update(&stale.db.conn).await.unwrap();
        let error = retry_completion_artifact_txn(&stale.db, cas)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "completion_decision_superseded");
    }

    #[tokio::test]
    async fn artifact_recovery_preserves_report_source_audit_reference() {
        let fixture = TerminalFixture::new(IntentFixture::Report, false).await;
        let opened = fixture.materialize().await;
        assert_eq!(opened.state, CompletionState::ArtifactRecovery);
        let cas = opened.attention.unwrap();
        let row = delegation_attention_request::Entity::find_by_id(cas.attention_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let payload: super::ArtifactRecoveryPayloadV1 =
            serde_json::from_str(row.payload_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload.source_audit_ref,
            super::CompletionSourceAuditRef::Report {
                report_file: Some("reports/task-10.md".into()),
            }
        );
    }

    #[tokio::test]
    async fn run_store_settlement_commits_v2_evidence_with_terminal_state_atomically() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let run = fixture.stored_run().await;
        let mut active: delegation_task_run::ActiveModel = run.into();
        active.status = Set(DelegationRunStatus::Running);
        active.finished_at = Set(None);
        active.completion_state = Set(None);
        active.completion_outcome = Set(None);
        active.completion_evidence_json = Set(None);
        active.update(&fixture.db.conn).await.unwrap();

        let runs = RunStore::new(fixture.db.clone());
        let (settlement, completion) = runs
            .settle_terminal_with_completion(
                &fixture.task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview)
                    .with_card_summary_json(
                        r#"{"kind":"author","status":"done","plan_digest":"model"}"#,
                    ),
                Some(fixture.input.clone()),
            )
            .await
            .unwrap();
        assert!(matches!(settlement, Settlement::Won(_)));
        assert_eq!(completion.unwrap().state, CompletionState::Resolved);
        let run = fixture.stored_run().await;
        assert_eq!(run.status, DelegationRunStatus::Completed);
        assert_eq!(run.completion_state, Some(CompletionState::Resolved));
        assert_eq!(run.card_summary_json, None);
    }

    #[tokio::test]
    async fn failed_v2_settlement_clears_cards_without_completion_evidence() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let run = fixture.stored_run().await;
        let mut active: delegation_task_run::ActiveModel = run.into();
        active.status = Set(DelegationRunStatus::Running);
        active.finished_at = Set(None);
        active.update(&fixture.db.conn).await.unwrap();

        let runs = RunStore::new(fixture.db.clone());
        let (settlement, completion) = runs
            .settle_terminal_with_completion(
                &fixture.task_id,
                TerminalTaskWrite::failed("task_failed", Utc::now(), ConversationStatus::Cancelled)
                    .with_card_summary_json(r#"{"kind":"author","status":"done"}"#),
                None,
            )
            .await
            .unwrap();

        assert!(matches!(settlement, Settlement::Won(_)));
        assert_eq!(completion, None);
        let run = fixture.stored_run().await;
        assert_eq!(run.status, DelegationRunStatus::Failed);
        assert_eq!(run.completion_state, None);
        assert_eq!(run.completion_outcome, None);
        assert_eq!(run.completion_evidence_json, None);
        assert_eq!(run.card_summary_json, None);
    }
}
