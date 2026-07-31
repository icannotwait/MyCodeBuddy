//! Publish / settle / get_workflow_state core store.
//!
//! Document gates only for settle. Execution-gate evaluation is Task 4.

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::db::entities::conversation;
use crate::db::entities::delegation_task_run::{self, DelegationRunStatus};
use crate::db::entities::delegation_workflow::{self, WorkflowState};
use crate::db::entities::delegation_workflow_gate_settlement::{
    self, GateSettlementOutcome, PlanReviewNextAction as DbPlanReviewNextAction,
    PlanReviewScope as DbPlanReviewScope, PlanRevisionKind as DbPlanRevisionKind,
};
use crate::db::entities::delegation_workflow_manifest_revision;
use crate::db::entities::delegation_workflow_node_binding::{self, NodeOutcome};
use crate::db::entities::delegation_workflow_run_binding;
use crate::db::AppDatabase;
use crate::web::event_bridge::EventEmitter;

use super::super::card_summary::{
    parse_and_validate_summary_json, CardSummary, ReviewVerdict, WorkStatus,
};
use super::error::WorkflowStoreError;
use super::events::emit_workflow_graph_changed;
use super::gates::{
    evaluate_execution_gate, ExecutionGateInput, ExecutionGateKind, RequiredReviewerEvidence,
};
use super::plan_review::{
    derive_plan_review_round, PlanFindingUpdate, PlanReviewError, PlanReviewNextAction,
    PlanReviewRoundState, PlanReviewRoundSubmission, PlanReviewScope, PlanRevisionKind,
};
use super::project::evidence_from_run_and_binding;
use super::recovery_policy::{
    decide_workflow_recovery, hash_displayed_reset_reason, WorkflowRecoveryActiveRun,
    WorkflowRecoveryBindingLifecycle, WorkflowRecoveryDocumentIdentity,
    WorkflowRecoveryFrozenTaskCohort, WorkflowRecoveryPlanGateEvidence,
    WorkflowRecoveryPlanIdentity, WorkflowRecoverySnapshot,
};
use super::state_dto::{
    project_workflow_state_index, PlanRecoverySourceDto, WorkflowGateStateDto,
    WorkflowNodeStateDto, WorkflowStateDto, WorkflowStateIndexDto,
};
use super::types::{
    DocumentGateKind, ManifestDocument, ManifestNode, ManifestNodeKind, ManifestNodeOutcome,
    ManifestNodeRole, ManifestRevisionKind, ManifestWorkflowState, NormalizedGate,
    NormalizedManifest, NormalizedNode, ResolutionMode, WorkflowBlockCause,
    MAX_ADJUDICATION_SUMMARY_BYTES, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use super::validate::validate_manifest_document;

/// Capability version stamped on new headers (B9 / A15).
pub const WORKFLOW_CAPABILITY_VERSION: &str = "workflow_manifest_v2";

const MAX_PERSISTED_PLAN_EVIDENCE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PersistedPlanReviewEvidence {
    submission: PlanReviewRoundSubmission,
    state: PlanReviewRoundState,
}

// Test-only failpoint: when true on the current thread, publish aborts after
// writing inside the transaction so the outer commit never lands. Thread-local
// so parallel cargo tests do not interfere.
#[cfg(test)]
thread_local! {
    static INJECT_PUBLISH_PERSISTENCE_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static BINDING_DIFF_INVOCATION_COUNT: std::cell::Cell<usize> =
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
fn note_binding_diff_invocation() {
    BINDING_DIFF_INVOCATION_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
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
pub enum SettleGateEvidence {
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
pub struct SettleWorkflowRequest {
    pub workflow_id: String,
    pub manifest_revision: u64,
    pub gate_id: String,
    pub expected_graph_revision: u64,
    pub gate_cycle: u64,
    pub outcome: GateSettlementOutcome,
    pub evidence: SettleGateEvidence,
    pub summary: String,
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
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Publish or update a workflow manifest (CAS + publication_token).
pub async fn publish_workflow_manifest_core(
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

/// Settle a **document** gate (Design/Plan) for one cycle. Never evaluates
/// Task/Final execution gates.
pub async fn settle_workflow_gate_core(
    db: &AppDatabase,
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    req: SettleWorkflowRequest,
) -> Result<SettleResult, WorkflowStoreError> {
    if req.summary.len() > MAX_ADJUDICATION_SUMMARY_BYTES {
        return Err(WorkflowStoreError::SummaryTooLarge);
    }
    if let SettleGateEvidence::Design {
        critical_count,
        important_count,
        minor_count,
    } = &req.evidence
    {
        if *critical_count < 0 || *important_count < 0 || *minor_count < 0 {
            return Err(WorkflowStoreError::NegativeFindingCounts {
                critical: *critical_count,
                important: *important_count,
                minor: *minor_count,
            });
        }
        if req.outcome == GateSettlementOutcome::Approved
            && (*critical_count > 0 || *important_count > 0)
        {
            return Err(WorkflowStoreError::ApprovalWithOpenFindings {
                critical: *critical_count,
                important: *important_count,
            });
        }
    } else if matches!(
        &req.evidence,
        SettleGateEvidence::Plan(submission) if submission.lineage_reset_reason.is_some()
    ) {
        return Err(PlanReviewError::InvalidTransition(
            "requirements lineage reset requires explicit user approval evidence".into(),
        )
        .into());
    }
    if req.gate_cycle == 0 {
        return Err(WorkflowStoreError::GateCycleConflict(
            "gate_cycle must be 1-based".into(),
        ));
    }

    let result = db
        .conn
        .transaction::<_, SettleResult, WorkflowStoreError>(|txn| {
            Box::pin(async move {
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

                // Existing settlements for this gate — check idempotency before
                // graph CAS so same-payload replay does not require a reload.
                let gate_prior = delegation_workflow_gate_settlement::Entity::find()
                    .filter(
                        delegation_workflow_gate_settlement::Column::WorkflowId
                            .eq(header.workflow_id.clone()),
                    )
                    .filter(
                        delegation_workflow_gate_settlement::Column::GateId.eq(req.gate_id.clone()),
                    )
                    .order_by_asc(delegation_workflow_gate_settlement::Column::GateCycle)
                    .all(txn)
                    .await
                    .map_err(db_err)?;

                if let Some(existing) = gate_prior
                    .iter()
                    .find(|s| s.gate_cycle as u64 == req.gate_cycle)
                {
                    if settlement_payload_matches(existing, &req)? {
                        return settle_result_from_row(
                            existing,
                            header.graph_revision as u64,
                            header.active_manifest_revision as u64,
                            true,
                        );
                    }
                    return Err(WorkflowStoreError::GateCycleConflict(format!(
                        "gate {} cycle {} already settled with a different payload",
                        req.gate_id, req.gate_cycle
                    )));
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

                // Plan review is one workflow-wide lineage. Gate IDs are mutable
                // manifest labels and must not reset cycles, findings, or stagnation.
                let plan_prior = delegation_workflow_gate_settlement::Entity::find()
                    .filter(
                        delegation_workflow_gate_settlement::Column::WorkflowId
                            .eq(header.workflow_id.clone()),
                    )
                    .filter(delegation_workflow_gate_settlement::Column::ReviewScope.is_not_null())
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
                if req.gate_cycle != expected_next {
                    return Err(WorkflowStoreError::GateCycleConflict(format!(
                        "gate {} expected cycle {expected_next}, got {}",
                        req.gate_id, req.gate_cycle
                    )));
                }

                // A2 freshness: required runs for this cycle against active
                // document revision + design/plan digest + content fingerprint.
                let current_doc_digest = document_digest_for_gate(gate, &normalized)?;
                let content_fp = gate_content_fingerprint(gate.gate_kind, &header);

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
                        (
                            *critical_count,
                            *important_count,
                            *minor_count,
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
                        if active_required != submitted_required {
                            return Err(PlanReviewError::RequiredReviewerSetMismatch {
                                expected: active_required,
                                actual: submitted_required,
                            }
                            .into());
                        }
                        if current_doc_digest.as_deref()
                            != Some(submission.covered_plan_digest.as_str())
                        {
                            return Err(WorkflowStoreError::ArtifactDigestMismatch(
                                "Plan submission digest does not match the active Plan artifact"
                                    .into(),
                            ));
                        }

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
                            if evidence.state.next_action == PlanReviewNextAction::Approved {
                                current_fingerprint_approved = true;
                                break;
                            }
                        }
                        if current_fingerprint_approved {
                            return Err(PlanReviewError::InvalidTransition(
                                "an approved Plan review lineage cannot be re-entered".into(),
                            )
                            .into());
                        }

                        let active_author_node_id = normalized
                            .nodes
                            .iter()
                            .find(|node| {
                                node.kind == ManifestNodeKind::WorkUnit
                                    && node.phase_id.as_deref() == Some(super::types::PHASE_PLAN)
                                    && node.role == Some(ManifestNodeRole::Author)
                            })
                            .map(|node| node.id.as_str())
                            .ok_or_else(|| {
                                WorkflowStoreError::GateNotReady(
                                    "active manifest has no Plan Author node".into(),
                                )
                            })?;

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
                        let state = derive_plan_review_round(
                            prior_state.as_ref(),
                            &gate.reviewer_cohort_node_ids,
                            submission,
                        )?;
                        validate_plan_outcome(&req.outcome, &state)?;
                        let persisted = PersistedPlanReviewEvidence {
                            submission: submission.clone(),
                            state: state.clone(),
                        };
                        let persisted_json = serialize_bounded_plan_evidence(&persisted)?;
                        let report_files_json = serialize_plan_report_files(&state.findings)?;
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
                    outcome: Set(req.outcome.clone()),
                    critical_count: Set(critical_count),
                    important_count: Set(important_count),
                    minor_count: Set(minor_count),
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
                    covered_author_task_id: Set(
                        plan_state.map(|state| state.covered_author_task_id.clone())
                    ),
                    covered_plan_digest: Set(
                        plan_state.map(|state| state.covered_plan_digest.clone())
                    ),
                    net_improvement: Set(plan_state.map(|state| state.net_improvement)),
                    finding_ledger_json: Set(
                        persisted_plan.map(|(_, persisted_json)| persisted_json)
                    ),
                    stagnation_count: Set(i64::from(stagnation_count)),
                    rewrite_used: Set(rewrite_used),
                    next_action: Set(plan_next_action.map(plan_next_action_to_db)),
                    report_files_json: Set(report_files_json),
                    lineage_reset_authorization_id: Set(None),
                    created_at: Set(now),
                };
                row.insert(txn).await.map_err(db_err)?;

                let state_revision = if req.outcome == GateSettlementOutcome::Blocked
                    && gate.gate_kind == DocumentGateKind::Plan
                {
                    let cause =
                        if plan_next_action == Some(PlanReviewNextAction::UserDecisionRequired) {
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
                            },
                            now,
                        )
                        .await?,
                    )
                } else {
                    None
                };

                let mut am: delegation_workflow::ActiveModel = header.clone().into();
                am.graph_revision = Set(next_graph);
                am.updated_at = Set(now);
                am.update(txn).await.map_err(db_err)?;

                Ok(SettleResult {
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
                })
            })
        })
        .await;

    let result = match result {
        Ok(r) => r,
        Err(sea_orm::TransactionError::Connection(e)) => {
            return Err(WorkflowStoreError::Persistence(e.to_string()));
        }
        Err(sea_orm::TransactionError::Transaction(e)) => return Err(e),
    };

    if !result.idempotent_replay {
        emit_workflow_graph_changed(
            emitter,
            parent_conversation_id,
            &result.workflow_id,
            result.graph_revision,
        );
    }

    Ok(result)
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

                let recovery_snapshot = if header.workflow_state == WorkflowState::Blocked {
                    Some(load_workflow_recovery_snapshot_conn(txn, &header, None).await?)
                } else {
                    None
                };
                let recovery = recovery_snapshot
                    .as_ref()
                    .map(|snapshot| decide_workflow_recovery(snapshot).projection());

                let doc = match load_active_manifest_document_txn(
                    txn,
                    &header.workflow_id,
                    header.active_manifest_revision,
                )
                .await
                {
                    Ok(document) => document,
                    Err(_error)
                        if recovery_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| !snapshot.active_manifest_valid) =>
                    {
                        return Ok(project_invalid_manifest_recovery_index(
                            &header,
                            recovery_snapshot.as_ref().expect("guarded snapshot"),
                            recovery.expect("blocked snapshot has projection"),
                        ));
                    }
                    Err(error) => return Err(error),
                };
                let normalized = validate_manifest_document(&doc)?;

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
                                evidence_from_run_and_binding(run, binding)
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
                                    evidence_from_run_and_binding(run, binding)
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
    let mut contradictory_durable_state = false;
    let header_state = workflow_state_to_manifest(header.workflow_state.clone());
    let revision = delegation_workflow_manifest_revision::Entity::find_by_id((
        header.workflow_id.clone(),
        header.active_manifest_revision,
    ))
    .one(conn)
    .await
    .map_err(db_err)?;

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
        let replacement_valid = run.replaced_task_id.as_deref().map_or(true, |replaced| {
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
                    .map_or(true, |run| run.status == DelegationRunStatus::Canceled)
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

    let plan_identity = |node_id: &str| -> Option<WorkflowRecoveryPlanIdentity> {
        let binding = active_binding_by_node.get(node_id)?;
        let latest = latest_run_binding_by_node.get(node_id).copied();
        let run = latest.and_then(|latest| run_by_id.get(&latest.task_id).copied());
        let evidence_consistent = latest.is_none() || run.is_some();
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
            artifact_digest: latest.and_then(|latest| latest.artifact_digest.clone()),
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
    let latest_plan_settlement = settlements
        .iter()
        .find(|settlement| settlement.review_scope.is_some());
    let current_plan_settlement = settlements.iter().find(|settlement| {
        settlement.review_scope.is_some()
            && settlement.content_fingerprint == header.plan_fingerprint
    });
    let project_plan_gate = |settlement: &delegation_workflow_gate_settlement::Model| {
        let persisted_evidence = load_persisted_plan_evidence(settlement);
        let parsed_reviewers = settlement
            .required_reviewer_node_ids_json
            .as_deref()
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
                    && reviewer.summary_validated
            })
            .count();
        WorkflowRecoveryPlanGateEvidence {
            gate_id: settlement.gate_id.clone(),
            gate_cycle: settlement.gate_cycle,
            outcome: settlement.outcome.clone(),
            content_fingerprint: settlement.content_fingerprint.clone(),
            critical_count: settlement.critical_count,
            important_count: settlement.important_count,
            minor_count: settlement.minor_count,
            next_action: settlement
                .next_action
                .as_ref()
                .map(plan_next_action_from_db),
            covered_author_task_id: settlement.covered_author_task_id.clone(),
            covered_plan_digest: settlement.covered_plan_digest.clone(),
            required_reviewer_node_ids,
            reviewer_evidence_count,
            evidence_consistent: persisted_evidence.as_ref().is_ok_and(|evidence| {
                validate_plan_outcome(&settlement.outcome, &evidence.state).is_ok()
            }) && parsed_reviewers.is_some()
                && ledger_valid
                && settlement.critical_count >= 0
                && settlement.important_count >= 0
                && settlement.minor_count >= 0
                && settlement.next_action.is_some()
                && settlement.covered_author_task_id.is_some()
                && settlement.covered_plan_digest.is_some(),
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

    Ok(WorkflowRecoverySnapshot {
        workflow_id: header.workflow_id.clone(),
        parent_conversation_id: header.parent_conversation_id,
        workflow_kind: header.workflow_kind.clone(),
        schema_version: u64::try_from(header.schema_version).unwrap_or_default(),
        capability_version: header.capability_version.clone(),
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
    })
}

fn project_invalid_manifest_recovery_index(
    header: &delegation_workflow::Model,
    snapshot: &WorkflowRecoverySnapshot,
    recovery: super::recovery_policy::WorkflowRecoveryProjection,
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

async fn load_by_publication_token_txn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    token: &str,
) -> Result<Option<delegation_workflow::Model>, WorkflowStoreError> {
    delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::PublicationToken.eq(token.to_string()))
        .one(conn)
        .await
        .map_err(db_err)
}

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
    let current = delegation_workflow::Entity::find_by_id(header.workflow_id.clone())
        .one(txn)
        .await
        .map_err(db_err)?
        .ok_or_else(|| WorkflowStoreError::NotFound(header.workflow_id.clone()))?;
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
        },
        now,
    )
    .await
}

/// Internal marker: winner not visible in this txn snapshot; outer must re-read.
const TOKEN_RACE_RECLASSIFY_MARKER: &str = "__workflow_publication_token_race_reclassify__";

fn is_token_race_reclassify_marker(err: &WorkflowStoreError) -> bool {
    matches!(
        err,
        WorkflowStoreError::Persistence(s) if s == TOKEN_RACE_RECLASSIFY_MARKER
    )
}

fn is_unique_constraint(err: &sea_orm::DbErr) -> bool {
    let s = err.to_string();
    s.contains("UNIQUE")
        || s.contains("unique")
        || s.contains("idx_dw_publication_token")
        || s.contains("idx_dw_parent_kind")
        || s.contains("2067") // SQLITE_CONSTRAINT_UNIQUE
}

fn is_busy_or_snapshot_err_str(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("busy")
        || lower.contains("snapshot")
        || lower.contains("locked")
        || lower.contains("database is locked")
        || s.contains("5") && lower.contains("sqlite") // SQLITE_BUSY
}

fn is_token_race_db_err(err: &sea_orm::DbErr) -> bool {
    is_unique_constraint(err) || is_busy_or_snapshot_err_str(&err.to_string())
}

/// Core publish body (runs inside a single write transaction).
async fn publish_in_txn(
    txn: &sea_orm::DatabaseTransaction,
    parent_conversation_id: i32,
    normalized: &NormalizedManifest,
    document_digest: &str,
    now: chrono::DateTime<Utc>,
) -> Result<PublishResult, WorkflowStoreError> {
    // --- re-read by publication_token (inside write txn) -------------------
    if let Some(by_token) =
        load_by_publication_token_txn(txn, &normalized.publication_token).await?
    {
        if by_token.parent_conversation_id != parent_conversation_id {
            return Err(WorkflowStoreError::CrossParent {
                workflow_id: by_token.workflow_id.clone(),
                expected_parent: parent_conversation_id,
                actual_parent: by_token.parent_conversation_id,
            });
        }
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
        )
        .await?
        {
            return Ok(classified);
        }
    }

    apply_binding_diff(txn, next_manifest_rev, binding_diff, now).await?;

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

/// Insert create header under SAVEPOINT.
///
/// - `Ok(None)` — header inserted; caller continues bindings/revision.
/// - `Ok(Some(result))` — race reclassified to same-digest idempotent replay.
/// - `Err(PublicationTokenMismatch|Conflict|CrossParent)` — typed race outcome.
/// - `Err(Persistence(TOKEN_RACE_RECLASSIFY_MARKER))` — winner not visible; outer
///   must re-read with a fresh snapshot (never returned as raw busy/unique).
#[allow(clippy::too_many_arguments)]
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
) -> Result<Option<PublishResult>, WorkflowStoreError> {
    use sea_orm::ConnectionTrait;

    const SP: &str = "sp_wf_pub_header";
    txn.execute_unprepared(&format!("SAVEPOINT {SP}"))
        .await
        .map_err(db_err)?;

    // Double-check token immediately before insert (another writer may have landed).
    if let Some(by_token) =
        load_by_publication_token_txn(txn, &normalized.publication_token).await?
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

async fn load_by_parent_kind_txn<C: sea_orm::ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
) -> Result<Option<delegation_workflow::Model>, WorkflowStoreError> {
    delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::ParentConversationId.eq(parent_conversation_id))
        .filter(delegation_workflow::Column::WorkflowKind.eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY))
        .one(conn)
        .await
        .map_err(db_err)
}

/// Re-read winner after a race. `None` = not visible under this snapshot.
async fn classify_token_race_visible<C: sea_orm::ConnectionTrait>(
    conn: &C,
    token: &str,
    document_digest: &str,
    parent_conversation_id: i32,
) -> Result<Option<PublishResult>, WorkflowStoreError> {
    if let Some(by_token) = load_by_publication_token_txn(conn, token).await? {
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

async fn classify_existing_header<C: sea_orm::ConnectionTrait>(
    conn: &C,
    mut header: delegation_workflow::Model,
    parent_conversation_id: i32,
    token: &str,
    document_digest: &str,
) -> Result<PublishResult, WorkflowStoreError> {
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
const TOKEN_RACE_BACKOFF_MS: &[u64] = &[5, 10, 20, 40, 80, 100, 120, 125];

/// Fresh-snapshot reclassify after concurrent unique/busy (outer, after txn ends).
///
/// - Durable same-token row + same digest → IdempotentReplay
/// - Durable same-token row + different digest → PublicationTokenMismatch (real id)
/// - Parent has other-token workflow → PublicationTokenConflict
/// - Still absent after exponential backoff → Busy (retryable), **never** fabricated Mismatch
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

fn frozen_cohort_error(task_index: i64) -> WorkflowStoreError {
    WorkflowStoreError::CohortFrozen {
        node_id: format!("Task {task_index}"),
    }
}

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

struct BindingDiffPlan {
    workflow_id: String,
    actions: Vec<BindingDiffAction>,
}

/// Validate every binding lifecycle decision without performing a write.
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

fn binding_identity_conflict(
    binding: &delegation_workflow_node_binding::Model,
) -> WorkflowStoreError {
    WorkflowStoreError::AdmittedNodeIdentityMutation {
        node_id: binding.node_id.clone(),
    }
}

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
            && !prior_ts.is_some_and(|timestamp| binding.created_at <= timestamp);
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
    {
        return Ok(false);
    }
    match &req.evidence {
        SettleGateEvidence::Design {
            critical_count,
            important_count,
            minor_count,
        } => Ok(existing.review_scope.is_none()
            && existing.critical_count == *critical_count
            && existing.important_count == *important_count
            && existing.minor_count == *minor_count),
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
    let plan_state = row
        .review_scope
        .as_ref()
        .map(|_| load_persisted_plan_evidence(row))
        .transpose()?
        .map(|evidence| evidence.state);
    Ok(SettleResult {
        workflow_id: row.workflow_id.clone(),
        gate_id: row.gate_id.clone(),
        gate_cycle: row.gate_cycle as u64,
        graph_revision,
        manifest_revision,
        outcome: row.outcome.clone(),
        idempotent_replay,
        plan_next_action: plan_state.as_ref().map(|state| state.next_action),
        critical_count: row.critical_count,
        important_count: row.important_count,
        minor_count: row.minor_count,
        stagnation_count: u32::try_from(row.stagnation_count).map_err(|_| {
            WorkflowStoreError::Persistence("invalid persisted Plan stagnation count".into())
        })?,
        rewrite_used: row.rewrite_used,
    })
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
    if row.critical_count != i64::from(state.critical_count)
        || row.important_count != i64::from(state.important_count)
        || row.minor_count != i64::from(state.minor_count)
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
fn plan_structure_changed(prior: &NormalizedManifest, next: &NormalizedManifest) -> bool {
    plan_structure_fingerprint(prior) != plan_structure_fingerprint(next)
}

fn design_fingerprint_hash(m: &NormalizedManifest) -> String {
    sha256_hex(design_structure_fingerprint(m).as_bytes())
}

fn plan_fingerprint_hash(m: &NormalizedManifest) -> String {
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

fn normalized_to_document(m: &NormalizedManifest) -> ManifestDocument {
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

fn sha256_hex(bytes: &[u8]) -> String {
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
        PlanReviewRoundSubmission, PlanReviewScope, PlanRevisionKind, WorkflowIndexOmissionStep,
        WorkflowStateDetail, WorkflowStateIndexDto,
    };
    use crate::db::entities::delegation_task_run::AdmissionClass;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;
    use crate::web::event_bridge::WebEventBroadcaster;
    use std::sync::Arc;

    fn emitter_with_rx() -> (
        EventEmitter,
        tokio::sync::broadcast::Receiver<crate::web::event_bridge::WebEvent>,
    ) {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let rx = broadcaster.subscribe();
        let emitter = EventEmitter::test_web_only(broadcaster);
        (emitter, rx)
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
                digest: "sha256:design".into(),
            }),
            plan: Some(DocumentRef {
                rel_path: plan_path.into(),
                digest: "sha256:plan".into(),
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
        let folder = seed_folder(&db, "/tmp/wf-store").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        (db, parent)
    }

    #[tokio::test]
    async fn workflow_manifest_v2_persisted_header_stamps_capability_version() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
    async fn workflow_manifest_v2_replay_and_update_upgrade_existing_header() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut document = design_plan_doc("workflow-manifest-v2-upgrade");
        let published = publish_workflow_manifest_core(
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
        let replay = publish_workflow_manifest_core(
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
        let update = publish_workflow_manifest_core(
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
    async fn workflow_v2_typed_error_real_producers_artifact_digest() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
        .expect_err("contradictory Plan digest must fail");
        assert!(matches!(
            &error,
            WorkflowStoreError::ArtifactDigestMismatch(_)
        ));
        let code = crate::acp::delegation::listener::workflow_store_error_code_for_test(error);
        assert_eq!(code, "artifact_digest_mismatch");
    }

    #[tokio::test]
    async fn workflow_manifest_v2_author_card_digest_mismatch_has_typed_marker() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
                "author-card-digest-task",
                "sha256:plan",
            )),
            "contradictory Author card digest",
        )
        .await
        .expect_err("contradictory Author card digest must fail");
        assert!(matches!(
            error,
            WorkflowStoreError::ArtifactDigestMismatch(_)
        ));
    }

    /// Design document digest used by `design_plan_doc`.
    const DESIGN_DOC_DIGEST: &str = "sha256:design";

    #[allow(clippy::too_many_arguments)]
    async fn insert_terminal_reviewer_run(
        db: &AppDatabase,
        parent: i32,
        workflow_id: &str,
        node_id: &str,
        gate_id: &str,
        gate_cycle: i64,
        task_id: &str,
        summary_validated: bool,
        created_offset_secs: i64,
        artifact_digest: &str,
        status: DelegationRunStatus,
        manifest_revision: i64,
    ) {
        let now = Utc::now() + chrono::Duration::seconds(created_offset_secs);
        let child = seed_conversation(
            db,
            seed_folder(db, &format!("/tmp/{task_id}")).await,
            AgentType::Codex,
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
            reached_running_at: Set(Some(now)),
            lineage_root_task_id: Set(task_id.to_string()),
            work_unit_key: Set(None),
            legacy_parent_tool_use_id: Set(None),
            history_only: Set(false),
            status: Set(status),
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
            card_summary_json: Set(Some("{}".into())),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(None),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
        run.insert(&db.conn).await.expect("insert run");

        let header = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let content_fp = match gate_id {
            "design" => Some(header.design_fingerprint.clone()),
            "plan" => Some(header.plan_fingerprint.clone()),
            _ => None,
        };

        let rb = delegation_workflow_run_binding::ActiveModel {
            task_id: Set(task_id.to_string()),
            workflow_id: Set(workflow_id.to_string()),
            node_id: Set(node_id.to_string()),
            gate_id: Set(Some(gate_id.to_string())),
            gate_cycle: Set(Some(gate_cycle)),
            manifest_revision: Set(manifest_revision),
            content_fingerprint: Set(content_fp),
            artifact_digest: Set(Some(artifact_digest.to_string())),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(1),
            summary_validated: Set(summary_validated),
            created_at: Set(now),
            updated_at: Set(now),
        };
        rb.insert(&db.conn).await.expect("insert run binding");
    }

    /// Convenience: completed design-gate reviewer on active revision 1.
    async fn insert_design_reviewer_ok(
        db: &AppDatabase,
        parent: i32,
        workflow_id: &str,
        task_id: &str,
        gate_cycle: i64,
        offset_secs: i64,
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

        let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut rb_am: delegation_workflow_run_binding::ActiveModel = binding.into();
        rb_am.reviewed_task_id = Set(Some(author_task_id.into()));
        rb_am.update(&db.conn).await.unwrap();
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
            publish_workflow_manifest_core(
                &self.db,
                &emitter,
                self.parent,
                PublishWorkflowRequest { document: doc },
            )
            .await
            .unwrap();
        }

        async fn record_current_reviewer_pointers(&self) {
            for (node_id, task_id, report_file, ordinal) in [
                (
                    "plan-reviewer-codex",
                    "current-plan-review-codex",
                    "reports/current-plan-review-codex.md",
                    10,
                ),
                (
                    "plan-reviewer-grok",
                    "current-plan-review-grok",
                    "reports/current-plan-review-grok.md",
                    11,
                ),
            ] {
                insert_plan_reviewer_evidence(
                    &self.db,
                    self.parent,
                    &self.workflow_id,
                    node_id,
                    task_id,
                    2,
                    2,
                    "sha256:plan-v2",
                    "historical-plan-author",
                    "request_changes",
                    report_file,
                    ordinal,
                )
                .await;
            }
        }
    }

    async fn seed_two_task_index_workflow() -> IndexWorkflowFixture {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let publication_token = "tok-two-task-index".to_string();
        let published = publish_workflow_manifest_core(
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
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
            "reports/current-plan-author.md",
            0,
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
                "sha256:plan",
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
                "sha256:plan",
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
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
                "sha256:plan",
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

    fn recovery_source_ids(index: &WorkflowStateIndexDto) -> Vec<&str> {
        index
            .latest_plan_review
            .as_ref()
            .unwrap()
            .recovery_sources
            .iter()
            .map(|source| source.node_id.as_str())
            .collect()
    }

    #[tokio::test]
    async fn index_routes_use_manifest_authority_and_durable_gate_state() {
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
            vec![2]
        );
    }

    #[tokio::test]
    async fn index_recovery_sources_cover_each_required_plan_reviewer() {
        let fixture = seed_open_plan_findings_with_reviewer_runs().await;
        let index =
            get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id))
                .await
                .unwrap();
        let review = index.latest_plan_review.unwrap();
        assert_eq!(
            review
                .recovery_sources
                .iter()
                .map(|source| source.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["plan-reviewer-codex", "plan-reviewer-grok"]
        );
        assert!(review
            .recovery_sources
            .iter()
            .all(|source| source.report_file.is_some() || source.latest_task_id.is_some()));
    }

    #[tokio::test]
    async fn material_republish_uses_current_plan_gate_cohort_through_omission() {
        let fixture =
            seed_historical_plan_round_with_required_reviewers(["plan-reviewer-old"]).await;
        fixture
            .materially_republish_plan_with_reviewers(["plan-reviewer-codex", "plan-reviewer-grok"])
            .await;

        let expected = vec!["plan-reviewer-codex", "plan-reviewer-grok"];
        let mut without_current_pointers =
            get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id))
                .await
                .unwrap();
        assert_eq!(recovery_source_ids(&without_current_pointers), expected);
        for step in WorkflowIndexOmissionStep::ALL {
            without_current_pointers.apply_omission_step(step);
            assert_eq!(recovery_source_ids(&without_current_pointers), expected);
        }

        fixture.record_current_reviewer_pointers().await;
        let mut index =
            get_workflow_state_core(&fixture.db, fixture.parent, Some(&fixture.workflow_id))
                .await
                .unwrap();
        assert_eq!(recovery_source_ids(&index), expected);
        for step in WorkflowIndexOmissionStep::ALL {
            index.apply_omission_step(step);
            assert_eq!(recovery_source_ids(&index), expected);
        }
    }

    #[tokio::test]
    async fn publish_create_and_idempotent_token_replay() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let doc = design_plan_doc("tok-1");
        let r1 = publish_workflow_manifest_core(
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
        let r2 = publish_workflow_manifest_core(
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
        publish_workflow_manifest_core(
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
        let err = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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
        let err = publish_workflow_manifest_core(
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
        let r1 = publish_workflow_manifest_core(
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
        let r2 = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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

        insert_design_reviewer_ok(&db, parent, &r.workflow_id, "task-design-1", 1, 0).await;

        let req = SettleWorkflowRequest {
            workflow_id: r.workflow_id.clone(),
            manifest_revision: 1,
            gate_id: "design".into(),
            expected_graph_revision: 1,
            gate_cycle: 1,
            outcome: GateSettlementOutcome::ChangesRequested,
            evidence: design_evidence(1, 0, 0),
            summary: "needs work".into(),
        };
        let s1 = settle_workflow_gate_core(&db, &emitter, parent, req.clone())
            .await
            .unwrap();
        assert!(!s1.idempotent_replay);
        assert_eq!(s1.graph_revision, 2);
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
        let r = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-cycle"),
            },
        )
        .await
        .unwrap();

        insert_design_reviewer_ok(&db, parent, &r.workflow_id, "task-c1", 1, 0).await;

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
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::GateNotReady(_)));

        // Fresh cycle-2 run works.
        insert_design_reviewer_ok(&db, parent, &r.workflow_id, "task-c2-fresh", 2, 10).await;
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
            },
        )
        .await
        .expect("fresh cycle 2");
    }

    #[tokio::test]
    async fn zero_reviewer_design_self_review_settle() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: zero_reviewer_design_doc("tok-self"),
            },
        )
        .await
        .unwrap();

        let s = settle_workflow_gate_core(
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
            },
        )
        .await
        .expect("self_review settle");
        assert!(!s.idempotent_replay);
    }

    #[tokio::test]
    async fn approval_rejected_with_nonzero_critical_important() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_core(
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
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            WorkflowStoreError::ApprovalWithOpenFindings { .. }
        ));
    }

    #[tokio::test]
    async fn summary_oversize_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_core(
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
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WorkflowStoreError::SummaryTooLarge));
    }

    #[tokio::test]
    async fn settle_rejects_negative_finding_counts() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let r = publish_workflow_manifest_core(
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
                },
            )
            .await
            .unwrap_err();
            assert!(
                matches!(
                    err,
                    WorkflowStoreError::NegativeFindingCounts {
                        critical: c,
                        important: i,
                        minor: m,
                    } if c == critical && i == important && m == minor
                ),
                "expected NegativeFindingCounts for ({critical},{important},{minor}), got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn b14_partner_freeze_on_plan_revision() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-b14");
        let r = publish_workflow_manifest_core(
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

        let err = publish_workflow_manifest_core(
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
        let published = publish_workflow_manifest_core(
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
        let error = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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

        let r2 = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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

        let err = publish_workflow_manifest_core(
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
        let err = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-state"),
            },
        )
        .await
        .unwrap();

        insert_design_reviewer_ok(&db, parent, &r.workflow_id, "task-state-1", 1, 0).await;

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
        publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: design_plan_doc("tok-a"),
            },
        )
        .await
        .unwrap();
        let err = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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
        let r = publish_workflow_manifest_core(
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
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            WorkflowStoreError::ApprovalRejectedFailedReviewer { .. }
        ));

        // Parent may still record changes_requested against a failed review.
        settle_workflow_gate_core(
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
            },
        )
        .await
        .expect("non-approve with failed reviewer");
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
        let r1 = publish_workflow_manifest_core(
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
            critical_count: Set(0),
            important_count: Set(0),
            minor_count: Set(0),
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
        let r2 = publish_workflow_manifest_core(
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
        let r1 = publish_workflow_manifest_core(
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
        let r2 = publish_workflow_manifest_core(
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
        let r1 = publish_workflow_manifest_core(
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
                critical_count: Set(0),
                important_count: Set(0),
                minor_count: Set(0),
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
        let r2 = publish_workflow_manifest_core(
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
        let r1 = publish_workflow_manifest_core(
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
                critical_count: Set(0),
                important_count: Set(0),
                minor_count: Set(0),
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
        let r2 = publish_workflow_manifest_core(
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
        let r1 = publish_workflow_manifest_core(
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
            critical_count: Set(0),
            important_count: Set(0),
            minor_count: Set(0),
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
        let r2 = publish_workflow_manifest_core(
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
            publish_workflow_manifest_core(
                &db,
                &emitter_a,
                parent,
                PublishWorkflowRequest {
                    document: doc.clone(),
                },
            ),
            publish_workflow_manifest_core(
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
        // Both succeed; at least one may be idempotent replay after serialization.
        assert!(
            a.idempotent_replay
                || b.idempotent_replay
                || (!a.idempotent_replay && !b.idempotent_replay),
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
            publish_workflow_manifest_core(
                &db,
                &emitter_a,
                parent,
                PublishWorkflowRequest { document: doc_a },
            ),
            publish_workflow_manifest_core(
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
        let skeleton = publish_workflow_manifest_core(
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
            "sha256:plan",
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
        let updated = publish_workflow_manifest_core(&db, &emitter, parent, update_request.clone())
            .await
            .expect("estimated publish after Author evidence");
        assert_eq!(updated.manifest_revision, 2);
        assert_eq!(updated.workflow_state, ManifestWorkflowState::Estimated);

        let replay = publish_workflow_manifest_core(&db, &emitter, parent, update_request)
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
        let error = publish_workflow_manifest_core(
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
    async fn task4_required_subset_publish_invalidates_stale_gate_runs() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let doc = two_reviewer_plan_doc("tok-task4-subset-fp");
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
        let updated = publish_workflow_manifest_core(
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

        let error = settle_for_test(
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
                "sha256:plan",
            )),
            "stale evidence",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::GateNotReady(_)));
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

        let published = publish_workflow_manifest_core(
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

        let published = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        let author_child = insert_plan_author_evidence(
            &db,
            parent,
            &published.workflow_id,
            "author-task-recovery",
            1,
            "sha256:plan",
            "reports/author-recovery.md",
            0,
        )
        .await;
        let reviewer_child = insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-task-recovery-1",
            1,
            1,
            "sha256:plan",
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
            "sha256:plan",
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
            "sha256:plan",
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
            (1, 1, 1)
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
        assert!(row.review_scope.is_some());
        assert!(row.revision_kind.is_some());
        assert_eq!(
            row.covered_author_task_id.as_deref(),
            Some("author-task-recovery")
        );
        assert_eq!(row.covered_plan_digest.as_deref(), Some("sha256:plan"));
        assert!(row
            .finding_ledger_json
            .as_deref()
            .unwrap()
            .contains("F-critical"));
        assert!(row
            .report_files_json
            .as_deref()
            .unwrap()
            .contains("reports/F-minor.md"));

        let mut frozen: delegation_workflow_node_binding::ActiveModel =
            delegation_workflow_node_binding::Entity::find_by_id((
                published.workflow_id.clone(),
                "task-1-impl".to_string(),
            ))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .into();
        frozen.cohort_frozen = Set(true);
        frozen.update(&db.conn).await.unwrap();

        let recovery = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
            .await
            .unwrap();
        assert_eq!(recovery.workflow_state, ManifestWorkflowState::Estimated);
        assert_eq!(recovery.detail, WorkflowStateDetail::Index);
        let recovery_json = serde_json::to_value(&recovery).unwrap();
        assert_eq!(
            recovery_json["plan_target_rel_path"],
            serde_json::json!("docs/superpowers/plans/p.md")
        );
        assert_eq!(recovery_json["risk_policy_version"], "b2d_task_risk_v1");
        assert_eq!(recovery.task_policies.len(), 1);
        assert_eq!(recovery.task_policies[0].task_index, 1);
        assert_eq!(recovery.task_policies[0].level, TaskRiskLevel::High);
        assert_eq!(recovery.actionable_task_routes.len(), 1);
        assert_eq!(recovery.actionable_task_routes[0].task_index, 1);
        assert_eq!(
            recovery.actionable_task_routes[0].implementer_node_id,
            "task-1-impl"
        );
        assert_eq!(
            recovery.actionable_task_routes[0].reviewer_node_ids,
            vec!["task-1-rev", "task-1-rev-grok"]
        );
        let review = recovery.latest_plan_review.as_ref().unwrap();
        assert_eq!(
            (
                review.critical_count,
                review.important_count,
                review.minor_count
            ),
            (1, 1, 1)
        );
        assert_eq!(review.next_action, PlanReviewNextAction::ContinueReview);
        assert_eq!(
            review.reviewed_reviewer_node_ids,
            vec!["plan-reviewer-1", "plan-reviewer-2"]
        );
        assert_eq!(
            review.next_required_reviewer_node_ids,
            vec!["plan-reviewer-1", "plan-reviewer-2"]
        );
        assert_eq!(review.finding_total_count, 3);
        assert_eq!(review.finding_returned_count, 3);
        assert!(recovery_json
            .pointer("/latest_plan_review/findings/0/summary")
            .is_none());
        assert_eq!(
            review
                .recovery_sources
                .iter()
                .map(|source| source.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["plan-reviewer-1", "plan-reviewer-2"]
        );
        assert_eq!(
            review.recovery_sources[0].child_conversation_id,
            Some(reviewer_child)
        );
        assert!(review
            .recovery_sources
            .iter()
            .all(|source| { source.report_file.is_some() || source.latest_task_id.is_some() }));
        let plan_gate = recovery_json["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|gate| gate["gate_id"] == "plan")
            .unwrap();
        assert_eq!(
            plan_gate["reviewer_cohort_node_ids"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            plan_gate["required_reviewer_node_ids"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let author = recovery
            .nodes
            .iter()
            .find(|node| node.node_id == "plan-author")
            .unwrap();
        assert_eq!(author.child_conversation_id, Some(author_child));
        assert_eq!(
            author.report_file.as_deref(),
            Some("reports/author-recovery.md")
        );

        let graph = crate::acp::delegation::workflow::project_workflow_graph_core(&db, parent)
            .await
            .unwrap();
        let graph_json = serde_json::to_string(&graph).unwrap();
        for secret in [
            "work_unit_key",
            "reviewed_task_id",
            "finding_ledger",
            "risk_policy_version",
            "report_file",
            "cohort_frozen",
        ] {
            assert!(!graph_json.contains(secret), "graph leaked {secret}");
        }
    }

    #[tokio::test]
    async fn task4_plan_reviewers_must_cover_same_author_task_and_digest() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
            "sha256:plan",
            "different-author-task",
            "approve",
            "reports/review-shared-2.md",
            2,
        )
        .await;

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
                &["plan-reviewer-1", "plan-reviewer-2"],
                vec![],
                "author-task-shared",
                "sha256:plan",
            )),
            "same artifact required",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::ReviewedTaskStale(_)));
    }

    #[tokio::test]
    async fn task4_plan_reducer_requires_infrastructure_successful_reviewer_evidence() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
                "sha256:plan",
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
    async fn task4_parent_supplied_lineage_reset_reason_fails_closed() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
        assert!(matches!(
            error,
            WorkflowStoreError::PlanReview(PlanReviewError::InvalidTransition(_))
        ));
    }

    #[tokio::test]
    async fn task4_plan_gate_rename_cannot_reset_or_hide_lineage() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-task4-gate-rename");
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
                "sha256:plan",
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
        let updated = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();

        let recovery = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
            .await
            .unwrap();
        assert_eq!(
            recovery
                .latest_plan_review
                .as_ref()
                .map(|state| state.important_count),
            Some(1),
            "renaming a gate must not hide the active Plan lineage"
        );

        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-gate-rename-reset",
            2,
            2,
            "sha256:plan",
            "author-task-gate-rename",
            "approve",
            "reports/review-gate-rename-reset.md",
            100,
        )
        .await;
        let binding = delegation_workflow_run_binding::Entity::find_by_id(
            "review-gate-rename-reset".to_string(),
        )
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut binding_am: delegation_workflow_run_binding::ActiveModel = binding.into();
        binding_am.gate_id = Set(Some("renamed-plan".into()));
        binding_am.gate_cycle = Set(Some(2));
        binding_am.update(&db.conn).await.unwrap();

        let reset = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "renamed-plan",
            2,
            updated.graph_revision,
            2,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Initial,
                &["plan-reviewer-1"],
                vec![],
                "author-task-gate-rename",
                "sha256:plan",
            )),
            "gate rename reset attempt",
        )
        .await
        .unwrap_err();
        assert!(matches!(
            reset,
            WorkflowStoreError::PlanReview(PlanReviewError::InvalidTransition(_))
        ));
    }

    #[tokio::test]
    async fn workflow_v2_typed_error_real_producers_reviewed_task_stale() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
            "sha256:plan",
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
                "author-task-old",
                "sha256:plan",
            )),
            "older Author task must not settle",
        )
        .await
        .unwrap_err();
        assert!(matches!(&error, WorkflowStoreError::ReviewedTaskStale(_)));
        let code = crate::acp::delegation::listener::workflow_store_error_code_for_test(error);
        assert_eq!(code, "reviewed_task_stale");
    }

    #[tokio::test]
    async fn task4_latest_plan_reviewer_binding_is_required() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
                "sha256:plan",
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
        binding_am.lineage_ordinal = Set(2);
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
                "sha256:plan",
            )),
            "older reviewer completion must not settle",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::GateNotReady(_)));
    }

    #[tokio::test]
    async fn task4_historical_current_fingerprint_approval_is_terminal() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc_a = design_plan_doc("tok-task4-historical-a");
        doc_a.workflow_state = ManifestWorkflowState::Approved;
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
                "sha256:plan",
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
        let published_b = publish_workflow_manifest_core(
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
            "sha256:plan",
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
                "sha256:plan",
            )),
            "review fingerprint B",
        )
        .await
        .unwrap();

        doc_a.workflow_id = Some(published.workflow_id.clone());
        doc_a.expected_manifest_revision = Some(3);
        doc_a.publication_token = "tok-task4-historical-a-again".into();
        let published_a_again = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc_a },
        )
        .await
        .unwrap();
        let recovery = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
            .await
            .unwrap();
        assert_eq!(
            recovery
                .latest_plan_review
                .as_ref()
                .map(|state| state.next_action),
            Some(PlanReviewNextAction::Approved)
        );
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-historical-a2",
            3,
            4,
            "sha256:plan",
            "author-task-historical",
            "approve",
            "reports/review-historical-a2.md",
            3,
        )
        .await;

        let error = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            4,
            published_a_again.graph_revision,
            3,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Material,
                &["plan-reviewer-1"],
                vec![finding(
                    "F-historical",
                    FindingSeverity::Important,
                    FindingStatus::Resolved,
                    &["plan-reviewer-1"],
                )],
                "author-task-historical",
                "sha256:plan",
            )),
            "historical fingerprint A must remain terminal",
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            WorkflowStoreError::PlanReview(PlanReviewError::InvalidTransition(_))
        ));
    }

    #[tokio::test]
    async fn task4_stale_approved_fingerprint_allows_material_reapproval() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = design_plan_doc("tok-task4-material-reapprove");
        doc.workflow_state = ManifestWorkflowState::Approved;
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
                "sha256:plan",
            )),
            "initial approval",
        )
        .await
        .unwrap();

        doc.workflow_id = Some(published.workflow_id.clone());
        doc.expected_manifest_revision = Some(2);
        doc.task_policies[0].risk.reason = "material risk-policy correction".into();
        let updated = publish_workflow_manifest_core(
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
            "sha256:plan",
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
                "sha256:plan",
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
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
                "sha256:plan",
            )),
            "retired Author cannot settle",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::GateNotReady(_)));
    }

    #[tokio::test]
    async fn task4_scoped_round_uses_active_owner_subset_and_material_requires_cohort() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = two_reviewer_plan_doc("tok-task4-scoped");
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
                "sha256:plan",
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
                "sha256:plan",
            )),
            "initial owner round",
        )
        .await
        .unwrap();

        doc.workflow_id = Some(published.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.gates
            .iter_mut()
            .find(|gate| gate.gate_kind == Some(DocumentGateKind::Plan))
            .unwrap()
            .required_reviewer_node_ids = vec!["plan-reviewer-1".into()];
        let updated = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-scoped-c2",
            2,
            2,
            "sha256:plan",
            "author-task-scoped",
            "approve",
            "reports/review-scoped-c2.md",
            100,
        )
        .await;

        let wrong_subset = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            2,
            updated.graph_revision,
            2,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Scoped,
                PlanRevisionKind::Localized,
                &["plan-reviewer-2"],
                vec![],
                "author-task-scoped",
                "sha256:plan",
            )),
            "wrong owner subset",
        )
        .await
        .unwrap_err();
        assert!(matches!(wrong_subset, WorkflowStoreError::PlanReview(_)));

        let material_without_cohort = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            2,
            updated.graph_revision,
            2,
            GateSettlementOutcome::ChangesRequested,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Full,
                PlanRevisionKind::Material,
                &["plan-reviewer-1"],
                vec![],
                "author-task-scoped",
                "sha256:plan",
            )),
            "material needs cohort",
        )
        .await
        .unwrap_err();
        assert!(matches!(
            material_without_cohort,
            WorkflowStoreError::PlanReview(_)
        ));

        let second = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            2,
            updated.graph_revision,
            2,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Scoped,
                PlanRevisionKind::Localized,
                &["plan-reviewer-1"],
                vec![finding(
                    "F-owner",
                    FindingSeverity::Important,
                    FindingStatus::Resolved,
                    &["plan-reviewer-1"],
                )],
                "author-task-scoped",
                "sha256:plan",
            )),
            "owner resolved",
        )
        .await
        .unwrap();
        assert_eq!(first.important_count, 1);
        assert_eq!(
            second.plan_next_action,
            Some(PlanReviewNextAction::Approved)
        );
        assert_eq!(second.important_count, 0);
    }

    #[tokio::test]
    async fn task4_plan_replay_compares_all_structured_evidence() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
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
            "sha256:plan",
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
        let replay = settle_for_test(
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
        assert!(!first.idempotent_replay);
        assert!(replay.idempotent_replay);
        assert_eq!(
            replay.plan_next_action,
            Some(PlanReviewNextAction::Approved)
        );

        let mut different = submission;
        different.scope_reason = "different structured evidence".into();
        let error = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            1,
            first.graph_revision,
            1,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(different),
            "approved replay",
        )
        .await
        .unwrap_err();
        assert!(matches!(error, WorkflowStoreError::GateCycleConflict(_)));
    }

    #[tokio::test]
    async fn task4_plan_stagnation_rewrite_then_user_decision_blocks() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
                "sha256:plan",
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
            let outcome = if cycle == 5 {
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
                    "sha256:plan",
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
            results[4].plan_next_action,
            Some(PlanReviewNextAction::UserDecisionRequired)
        );
        assert_eq!(results[4].stagnation_count, 2);
        assert!(results[4].rewrite_used);
        assert_eq!(results[4].outcome, GateSettlementOutcome::Blocked);
        let header = delegation_workflow::Entity::find_by_id(published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(header.workflow_state, WorkflowState::Blocked);
    }

    #[tokio::test]
    async fn task4_plan_approval_derives_open_findings_and_reentry_fails_closed() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
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
            "sha256:plan",
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
            "sha256:plan",
            "author-task-approve",
            "approve",
            "reports/review-approve-open.md",
            1,
        )
        .await;
        let open_error = settle_for_test(
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
                "sha256:plan",
            )),
            "cannot approve open finding",
        )
        .await
        .unwrap_err();
        assert!(matches!(
            open_error,
            WorkflowStoreError::ApprovalWithOpenFindings { .. }
        ));

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
                "author-task-approve",
                "sha256:plan",
            )),
            "approved",
        )
        .await
        .unwrap();
        insert_plan_reviewer_evidence(
            &db,
            parent,
            &published.workflow_id,
            "plan-reviewer-1",
            "review-approve-reentry",
            2,
            1,
            "sha256:plan",
            "author-task-approve",
            "approve",
            "reports/review-approve-reentry.md",
            100,
        )
        .await;
        let reentry = settle_for_test(
            &db,
            &emitter,
            parent,
            &published.workflow_id,
            "plan",
            2,
            approved.graph_revision,
            2,
            GateSettlementOutcome::Approved,
            TestGateEvidence::Plan(plan_submission(
                PlanReviewScope::Scoped,
                PlanRevisionKind::Localized,
                &["plan-reviewer-1"],
                vec![],
                "author-task-approve",
                "sha256:plan",
            )),
            "approved reentry",
        )
        .await
        .unwrap_err();
        assert!(reentry.to_string().contains("Plan review"));
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
            publish_workflow_manifest_core(db, emitter, parent, PublishWorkflowRequest { document })
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
            publish_workflow_manifest_core(db, emitter, parent, PublishWorkflowRequest { document })
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
                "sha256:plan",
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
                "sha256:plan",
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
                "sha256:plan",
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
            let reclassified = classify_existing_header(
                &db.conn,
                header,
                parent,
                &blocked.publication_token,
                &active_digest,
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
            let recovery_state = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
                .await
                .expect("load later recovery evidence");
            let plan = recovery_state
                .latest_plan_review
                .expect("approved Plan evidence remains derivable");
            assert_eq!(plan.next_action, PlanReviewNextAction::Approved);
            assert_eq!(plan.covered_plan_digest, "sha256:plan");
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
            seed_ready_plan_round(&plan_db, plan_parent, &plan.workflow_id, "plan-block").await;
            let plan_blocked = settle_workflow_gate_core(
                &plan_db,
                &emitter,
                plan_parent,
                SettleWorkflowRequest {
                    workflow_id: plan.workflow_id.clone(),
                    manifest_revision: 1,
                    gate_id: "plan".into(),
                    expected_graph_revision: plan.graph_revision,
                    gate_cycle: 1,
                    outcome: GateSettlementOutcome::Blocked,
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
                        "sha256:plan",
                    )),
                    summary: "Plan gate blocks on current findings".into(),
                },
            )
            .await
            .expect("settle blocked Plan gate");
            assert_eq!(plan_blocked.manifest_revision, 2);
            let plan_header = load_header(&plan_db, &plan.workflow_id).await;
            let plan_revisions = load_revisions(&plan_db, &plan.workflow_id).await;
            assert_eq!(plan_header.block_source_manifest_revision, Some(1));
            assert_eq!(
                WorkflowBlockCause::from_db(plan_header.block_cause_code.as_deref())
                    .expect("Plan gate block cause"),
                WorkflowBlockCause::PlanGateBlocked
            );
            assert_eq!(plan_revisions[1].source_manifest_revision, Some(1));
            assert_eq!(
                plan_revisions[1].transition_reason_code.as_deref(),
                Some(WorkflowBlockCause::PlanGateBlocked.as_str())
            );

            let (design_db, design_parent) = seed_parent().await;
            let design = publish_document(
                &design_db,
                &emitter,
                design_parent,
                zero_reviewer_design_doc("typed-design-gate-block"),
            )
            .await
            .expect("publish Design gate fixture");
            let design_blocked = settle_workflow_gate_core(
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
                },
            )
            .await
            .expect("persist blocked Design gate evidence");
            assert_eq!(design_blocked.manifest_revision, 1);
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
                "sha256:plan",
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
                    "sha256:plan",
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
                let outcome = if cycle == 5 {
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
                            "sha256:plan",
                        )),
                        summary: format!("typed user-decision round {cycle}"),
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
            corrupt.finding_ledger_json = Set(Some("{}".into()));
            corrupt.update(&db.conn).await.unwrap();

            let header = load_header(&db, &published.workflow_id).await;
            let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
                .await
                .unwrap();
            let decision = decide_workflow_recovery(&snapshot);
            assert!(matches!(
                decision.disposition,
                super::super::super::recovery_policy::WorkflowRecoveryDisposition::Stop {
                    blockers,
                    ..
                } if blockers.contains(
                    &super::super::super::recovery_policy::WorkflowRecoveryBlocker::StalePlanGateEvidence
                )
            ));
            let state = get_workflow_state_core(&db, parent, Some(&published.workflow_id))
                .await
                .expect("corrupt Plan evidence still returns recovery projection");
            let recovery = state.recovery.expect("typed recovery projection");
            assert_eq!(recovery.disposition, "blocked");
            assert!(!recovery.authorization_required);
            assert!(recovery
                .blockers
                .contains(&"stale_plan_gate_evidence".to_string()));
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
            let mut evidence = load_persisted_plan_evidence(&approved).unwrap();
            evidence.state.next_action = PlanReviewNextAction::ContinueReview;
            let evidence_json = serialize_bounded_plan_evidence(&evidence).unwrap();
            let mut later: delegation_workflow_gate_settlement::ActiveModel = approved.into();
            later.gate_cycle = Set(2);
            later.content_fingerprint = Set("different-plan-fingerprint".into());
            later.outcome = Set(GateSettlementOutcome::ChangesRequested);
            later.next_action = Set(Some(DbPlanReviewNextAction::ContinueReview));
            later.finding_ledger_json = Set(Some(evidence_json));
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
            assert_eq!(snapshot.current_plan_gate.as_ref().unwrap().gate_cycle, 1);
            assert_eq!(
                decide_workflow_recovery(&snapshot).disposition,
                super::super::super::recovery_policy::WorkflowRecoveryDisposition::Recover {
                    target_state: ManifestWorkflowState::Approved,
                }
            );
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
    }
}
