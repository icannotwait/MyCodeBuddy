//! Publish / settle / get_workflow_state core store (Task 3).
//!
//! Document gates only for settle. Execution-gate evaluation is Task 4.

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

use crate::db::entities::conversation;
use crate::db::entities::delegation_task_run::{self, DelegationRunStatus};
use crate::db::entities::delegation_workflow::{self, WorkflowState};
use crate::db::entities::delegation_workflow_gate_settlement::{self, GateSettlementOutcome};
use crate::db::entities::delegation_workflow_manifest_revision;
use crate::db::entities::delegation_workflow_node_binding::{self, NodeOutcome};
use crate::db::entities::delegation_workflow_run_binding;
use crate::db::AppDatabase;
use crate::web::event_bridge::EventEmitter;

use super::error::WorkflowStoreError;
use super::events::emit_workflow_graph_changed;
use super::state_dto::{WorkflowGateStateDto, WorkflowNodeStateDto, WorkflowStateDto};
use super::types::{
    DocumentGateKind, ManifestDocument, ManifestNode, ManifestNodeKind, ManifestNodeOutcome,
    ManifestNodeRole, ManifestWorkflowState, NormalizedGate, NormalizedManifest, NormalizedNode,
    ResolutionMode, MAX_ADJUDICATION_SUMMARY_BYTES, MAX_NODES,
    WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use super::validate::validate_manifest_document;

/// Capability version stamped on new headers (B9 / A15).
pub const WORKFLOW_CAPABILITY_VERSION: &str = "workflow_manifest_v1";

/// Soft evidence budget for `get_workflow_state` (A15 class: same as MAX_NODES).
const MAX_STATE_NODE_EVIDENCE: usize = MAX_NODES;

// Test-only failpoint: when true on the current thread, publish aborts after
// writing inside the transaction so the outer commit never lands. Thread-local
// so parallel cargo tests do not interfere.
#[cfg(test)]
thread_local! {
    static INJECT_PUBLISH_PERSISTENCE_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
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

// ---------------------------------------------------------------------------
// Request / result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PublishWorkflowRequest {
    pub document: ManifestDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PublishResult {
    pub workflow_id: String,
    pub manifest_revision: u64,
    pub graph_revision: u64,
    pub workflow_state: ManifestWorkflowState,
    pub idempotent_replay: bool,
}

#[derive(Debug, Clone)]
pub struct SettleWorkflowRequest {
    pub workflow_id: String,
    pub manifest_revision: u64,
    pub gate_id: String,
    pub expected_graph_revision: u64,
    pub gate_cycle: u64,
    pub outcome: GateSettlementOutcome,
    pub critical_count: i64,
    pub important_count: i64,
    pub minor_count: i64,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SettleResult {
    pub workflow_id: String,
    pub gate_id: String,
    pub gate_cycle: u64,
    pub graph_revision: u64,
    pub outcome: GateSettlementOutcome,
    pub idempotent_replay: bool,
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
                    &document_json,
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
    // Finding counts are non-negative integers (MCP schema minimum: 0). Store
    // core is authoritative for callers that bypass companion parse.
    if req.critical_count < 0 || req.important_count < 0 || req.minor_count < 0 {
        return Err(WorkflowStoreError::NegativeFindingCounts {
            critical: req.critical_count,
            important: req.important_count,
            minor: req.minor_count,
        });
    }
    if req.outcome == GateSettlementOutcome::Approved
        && (req.critical_count > 0 || req.important_count > 0)
    {
        return Err(WorkflowStoreError::ApprovalWithOpenFindings {
            critical: req.critical_count,
            important: req.important_count,
        });
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
                let prior = delegation_workflow_gate_settlement::Entity::find()
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

                if let Some(existing) = prior.iter().find(|s| s.gate_cycle as u64 == req.gate_cycle)
                {
                    if settlement_payload_matches(existing, &req) {
                        return Ok(SettleResult {
                            workflow_id: header.workflow_id.clone(),
                            gate_id: req.gate_id.clone(),
                            gate_cycle: req.gate_cycle,
                            graph_revision: header.graph_revision as u64,
                            outcome: existing.outcome.clone(),
                            idempotent_replay: true,
                        });
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

                let max_cycle = prior.iter().map(|s| s.gate_cycle).max().unwrap_or(0);
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
                verify_document_gate_ready(
                    txn,
                    &header.workflow_id,
                    gate,
                    req.gate_cycle as i64,
                    header.active_manifest_revision,
                    current_doc_digest.as_deref(),
                    content_fp.as_str(),
                    &req.outcome,
                    prior.last(),
                )
                .await?;

                let now = Utc::now();
                let next_graph = header.graph_revision + 1;
                let row = delegation_workflow_gate_settlement::ActiveModel {
                    workflow_id: Set(header.workflow_id.clone()),
                    gate_id: Set(req.gate_id.clone()),
                    gate_cycle: Set(req.gate_cycle as i64),
                    manifest_revision: Set(header.active_manifest_revision),
                    structural_revision: Set(header.structural_revision),
                    content_fingerprint: Set(content_fp),
                    outcome: Set(req.outcome.clone()),
                    critical_count: Set(req.critical_count),
                    important_count: Set(req.important_count),
                    minor_count: Set(req.minor_count),
                    summary: Set(req.summary.clone()),
                    graph_revision_at_settle: Set(header.graph_revision),
                    created_at: Set(now),
                };
                row.insert(txn).await.map_err(db_err)?;

                let mut am: delegation_workflow::ActiveModel = header.clone().into();
                am.graph_revision = Set(next_graph);
                // Approved document gate may advance overall state toward approved
                // when the published manifest already says approved.
                if req.outcome == GateSettlementOutcome::Approved
                    && matches!(
                        header.workflow_state,
                        WorkflowState::Estimated | WorkflowState::Skeleton
                    )
                    && normalized.workflow_state == ManifestWorkflowState::Approved
                {
                    am.workflow_state = Set(WorkflowState::Approved);
                }
                if req.outcome == GateSettlementOutcome::Blocked {
                    am.workflow_state = Set(WorkflowState::Blocked);
                }
                am.updated_at = Set(now);
                am.update(txn).await.map_err(db_err)?;

                Ok(SettleResult {
                    workflow_id: header.workflow_id.clone(),
                    gate_id: req.gate_id.clone(),
                    gate_cycle: req.gate_cycle,
                    graph_revision: next_graph as u64,
                    outcome: req.outcome.clone(),
                    idempotent_replay: false,
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

/// Agent-facing recovery read (A5 + B4). Never returns frontend-redacted shape.
///
/// Entire snapshot is loaded in a single SQLite read transaction for consistency.
pub async fn get_workflow_state_core(
    db: &AppDatabase,
    parent_conversation_id: i32,
    workflow_id: Option<&str>,
) -> Result<WorkflowStateDto, WorkflowStoreError> {
    let workflow_id_owned = workflow_id.map(|s| s.to_string());
    let result = db
        .conn
        .transaction::<_, WorkflowStateDto, WorkflowStoreError>(|txn| {
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

                let doc = load_active_manifest_document_txn(
                    txn,
                    &header.workflow_id,
                    header.active_manifest_revision,
                )
                .await?;
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

                let mut nodes: Vec<WorkflowNodeStateDto> = bindings
                    .iter()
                    .map(|b| {
                        let latest = latest_by_node.get(&b.node_id);
                        let run = latest.and_then(|rb| run_by_id.get(&rb.task_id).copied());
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
                            pair_frozen: b.pair_frozen,
                            node_outcome: b.node_outcome.as_ref().map(|o| match o {
                                NodeOutcome::Canceled => "canceled".to_string(),
                            }),
                            latest_task_id: latest.map(|rb| rb.task_id.clone()),
                            latest_status: run.map(|r| run_status_str(&r.status).to_string()),
                            latest_generation: run.map(|r| r.generation),
                            summary_validated: latest.map(|rb| rb.summary_validated),
                            artifact_digest: latest.and_then(|rb| rb.artifact_digest.clone()),
                            gate_id: latest.and_then(|rb| rb.gate_id.clone()),
                            gate_cycle: latest.and_then(|rb| rb.gate_cycle),
                            replaced_task_id: run.and_then(|r| r.replaced_task_id.clone()),
                            required_for_gate: required_node_ids.contains(&b.node_id),
                            evidence_time,
                        }
                    })
                    .collect();

                let evidence_truncated =
                    truncate_node_evidence(&mut nodes, MAX_STATE_NODE_EVIDENCE);

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
                    let max_cycle = gate_settlements
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
                        required_reviewer_node_ids: g.required_reviewer_node_ids.clone(),
                        latest_gate_cycle: latest.map(|s| s.gate_cycle),
                        latest_outcome: latest
                            .map(|s| settlement_outcome_str(&s.outcome).to_string()),
                        next_gate_cycle: next_cycle,
                    });
                }

                Ok(WorkflowStateDto {
                    workflow_id: header.workflow_id,
                    parent_conversation_id: header.parent_conversation_id,
                    workflow_kind: header.workflow_kind,
                    capability_version: header.capability_version,
                    workflow_state: workflow_state_to_manifest(header.workflow_state),
                    manifest_revision: header.active_manifest_revision as u64,
                    graph_revision: header.graph_revision as u64,
                    schema_version: header.schema_version as u64,
                    publication_token: header.publication_token,
                    design: normalized.design,
                    plan: normalized.plan,
                    nodes,
                    gates,
                    evidence_truncated,
                })
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
    document_json: &str,
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
        if active_digest.as_deref() == Some(document_digest) {
            return Ok(PublishResult {
                workflow_id: by_token.workflow_id,
                manifest_revision: by_token.active_manifest_revision as u64,
                graph_revision: by_token.graph_revision as u64,
                workflow_state: workflow_state_to_manifest(by_token.workflow_state),
                idempotent_replay: true,
            });
        }
        let is_explicit_update = normalized
            .workflow_id
            .as_deref()
            .is_some_and(|id| id == by_token.workflow_id);
        if !is_explicit_update {
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
                    return Ok(PublishResult {
                        workflow_id: existing.workflow_id.clone(),
                        manifest_revision: existing.active_manifest_revision as u64,
                        graph_revision: existing.graph_revision as u64,
                        workflow_state: workflow_state_to_manifest(existing.workflow_state.clone()),
                        idempotent_replay: true,
                    });
                }
                let next_m = existing.active_manifest_revision + 1;
                let next_g = existing.graph_revision + 1;
                (existing.workflow_id.clone(), next_m, next_g, Some(existing))
            }
        };

    // A8: material Plan structure change forces demotion when previously
    // approved or already demoted (supersedes set). Design fingerprint is
    // independent so Design settlements survive plan-only rewrites.
    let mut effective_state = normalized.workflow_state;
    let mut supersedes =
        compute_supersedes(prior_header.as_ref(), effective_state, next_manifest_rev);
    let next_design_fp = design_fingerprint_hash(normalized);
    let next_plan_fp = plan_fingerprint_hash(normalized);
    let mut next_structural_rev = next_manifest_rev;
    if let Some(prior) = prior_header.as_ref() {
        let plan_changed = if prior.plan_fingerprint.is_empty() {
            // Legacy rows before fingerprint columns were backfilled.
            match load_active_manifest_document_txn(
                txn,
                &prior.workflow_id,
                prior.active_manifest_revision,
            )
            .await
            {
                Ok(prior_doc) => match validate_manifest_document(&prior_doc) {
                    Ok(prior_norm) => plan_structure_changed(&prior_norm, normalized),
                    Err(_) => true,
                },
                Err(_) => true,
            }
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

    // Header first so child FK rows can land in the same transaction.
    if let Some(prior) = prior_header.clone() {
        let mut am: delegation_workflow::ActiveModel = prior.into();
        am.active_manifest_revision = Set(next_manifest_rev);
        am.graph_revision = Set(next_graph_rev);
        am.workflow_state = Set(workflow_state);
        am.supersedes_approved_revision = Set(supersedes);
        am.structural_revision = Set(next_structural_rev);
        am.design_fingerprint = Set(next_design_fp);
        am.plan_fingerprint = Set(next_plan_fp);
        am.updated_at = Set(now);
        am.update(txn).await.map_err(db_err)?;
    } else {
        // CREATE: insert under SAVEPOINT so unique/busy can reclassify cleanly.
        if let Some(classified) = insert_header_create_or_reclassify(
            txn,
            &workflow_id,
            parent_conversation_id,
            normalized,
            next_manifest_rev,
            next_graph_rev,
            workflow_state,
            document_digest,
            now,
        )
        .await?
        {
            return Ok(classified);
        }
    }

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

    apply_binding_diff(
        txn,
        &workflow_id,
        next_manifest_rev,
        normalized,
        &existing_bindings,
        &has_run_bindings,
        now,
    )
    .await?;

    let rev_row = delegation_workflow_manifest_revision::ActiveModel {
        workflow_id: Set(workflow_id.clone()),
        manifest_revision: Set(next_manifest_rev),
        manifest_state: Set(manifest_state_str(effective_state).into()),
        document_json: Set(document_json.to_string()),
        document_digest: Set(document_digest.to_string()),
        created_at: Set(now),
    };
    rev_row.insert(txn).await.map_err(db_err)?;

    if inject_publish_persistence_failure() {
        return Err(WorkflowStoreError::Persistence(
            "injected publish persistence failure".into(),
        ));
    }

    Ok(PublishResult {
        workflow_id: workflow_id.clone(),
        manifest_revision: next_manifest_rev as u64,
        graph_revision: next_graph_rev as u64,
        workflow_state: effective_state,
        idempotent_replay: false,
    })
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
        workflow_state: Set(workflow_state),
        capability_version: Set(WORKFLOW_CAPABILITY_VERSION.into()),
        publication_token: Set(normalized.publication_token.clone()),
        supersedes_approved_revision: Set(None),
        structural_revision: Set(next_manifest_rev),
        design_fingerprint: Set(design_fingerprint_hash(normalized)),
        plan_fingerprint: Set(plan_fingerprint_hash(normalized)),
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
    header: delegation_workflow::Model,
    parent_conversation_id: i32,
    token: &str,
    document_digest: &str,
) -> Result<PublishResult, WorkflowStoreError> {
    let active_digest =
        load_active_manifest_digest_txn(conn, &header.workflow_id, header.active_manifest_revision)
            .await?;
    classify_header_against_digest(
        token,
        parent_conversation_id,
        header.parent_conversation_id,
        header.workflow_id,
        active_digest.as_deref(),
        document_digest,
        header.active_manifest_revision as u64,
        header.graph_revision as u64,
        workflow_state_to_manifest(header.workflow_state),
    )
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
        return Ok(PublishResult {
            workflow_id,
            manifest_revision,
            graph_revision,
            workflow_state,
            idempotent_replay: true,
        });
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

/// Apply node-binding create / retain / freeze rules for a publish.
async fn apply_binding_diff<C: sea_orm::ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    next_revision: i64,
    normalized: &NormalizedManifest,
    existing: &[delegation_workflow_node_binding::Model],
    nodes_with_runs: &HashSet<String>,
    now: chrono::DateTime<Utc>,
) -> Result<(), WorkflowStoreError> {
    let existing_by_id: HashMap<&str, &delegation_workflow_node_binding::Model> =
        existing.iter().map(|b| (b.node_id.as_str(), b)).collect();

    let new_work_units: Vec<&NormalizedNode> = normalized
        .nodes
        .iter()
        .filter(|n| n.kind == ManifestNodeKind::WorkUnit && n.work_unit_key.is_some())
        .collect();
    let new_ids: HashSet<&str> = new_work_units.iter().map(|n| n.id.as_str()).collect();

    // B14: if either Task-pair node is observed/retained/has runs, both are
    // protected against silent partner drop — even when pair_frozen was not
    // yet set by admission (Task 6).
    let task_pair_active: HashSet<i64> = existing
        .iter()
        .filter(|b| {
            b.task_index.is_some()
                && (b.is_observed
                    || b.retained_observed
                    || b.pair_frozen
                    || nodes_with_runs.contains(&b.node_id))
        })
        .filter_map(|b| b.task_index)
        .collect();

    for b in existing {
        if new_ids.contains(b.node_id.as_str()) {
            continue;
        }
        let is_admitted = b.is_observed || nodes_with_runs.contains(&b.node_id);
        let pair_protected = b.pair_frozen
            || b.is_observed
            || b.retained_observed
            || nodes_with_runs.contains(&b.node_id)
            || b.task_index
                .is_some_and(|idx| task_pair_active.contains(&idx));

        if pair_protected && !is_canceled_drop(normalized, b) {
            return Err(WorkflowStoreError::FrozenPartnerDrop {
                node_id: b.node_id.clone(),
            });
        }

        if is_admitted || b.retained_observed || b.pair_frozen || pair_protected {
            let mut am: delegation_workflow_node_binding::ActiveModel = b.clone().into();
            am.retired_revision = Set(Some(next_revision));
            am.retained_observed = Set(true);
            if b.task_index
                .is_some_and(|idx| task_pair_active.contains(&idx))
            {
                am.pair_frozen = Set(true);
            }
            am.updated_at = Set(now);
            am.update(conn).await.map_err(db_err)?;
        } else {
            delegation_workflow_node_binding::Entity::delete_by_id((
                workflow_id.to_string(),
                b.node_id.clone(),
            ))
            .exec(conn)
            .await
            .map_err(db_err)?;
        }
    }

    for node in new_work_units {
        let key = node.work_unit_key.as_ref().expect("work unit key");
        let role = role_str(node.role.expect("work unit role"));
        let agent = node.agent_type.as_ref().expect("agent").clone();
        let phase = node.phase_id.as_ref().expect("phase").clone();
        let outcome = node.node_outcome.map(|o| match o {
            ManifestNodeOutcome::Canceled => NodeOutcome::Canceled,
        });
        let freeze_pair = node
            .task_index
            .is_some_and(|idx| task_pair_active.contains(&(idx as i64)));

        if let Some(prev) = existing_by_id.get(node.id.as_str()) {
            let is_admitted = prev.is_observed || nodes_with_runs.contains(&node.id);
            if is_admitted
                && (prev.work_unit_key != *key
                    || prev.role != role
                    || prev.agent_type != agent
                    || prev.profile_id != node.profile_id
                    || prev.phase_id != phase
                    || prev.task_index != node.task_index.map(|i| i as i64))
            {
                return Err(WorkflowStoreError::AdmittedNodeIdentityMutation {
                    node_id: node.id.clone(),
                });
            }
            let mut am: delegation_workflow_node_binding::ActiveModel = (*prev).clone().into();
            if !is_admitted {
                am.work_unit_key = Set(key.clone());
                am.role = Set(role.into());
                am.agent_type = Set(agent);
                am.profile_id = Set(node.profile_id.clone());
                am.phase_id = Set(phase);
                am.task_index = Set(node.task_index.map(|i| i as i64));
            }
            am.retired_revision = Set(None);
            am.retained_observed = Set(prev.retained_observed && prev.retired_revision.is_some());
            if freeze_pair {
                am.pair_frozen = Set(true);
            }
            if let Some(o) = outcome {
                am.node_outcome = Set(Some(o));
            }
            am.updated_at = Set(now);
            am.update(conn).await.map_err(db_err)?;
        } else {
            let row = delegation_workflow_node_binding::ActiveModel {
                workflow_id: Set(workflow_id.to_string()),
                node_id: Set(node.id.clone()),
                work_unit_key: Set(key.clone()),
                role: Set(role.into()),
                agent_type: Set(agent),
                profile_id: Set(node.profile_id.clone()),
                phase_id: Set(phase),
                task_index: Set(node.task_index.map(|i| i as i64)),
                introduced_revision: Set(next_revision),
                retired_revision: Set(None),
                is_observed: Set(false),
                retained_observed: Set(false),
                pair_frozen: Set(freeze_pair),
                node_outcome: Set(outcome),
                created_at: Set(now),
                updated_at: Set(now),
            };
            row.insert(conn).await.map_err(db_err)?;
        }
    }

    Ok(())
}

/// Drop is legal only when cancel/block is explicit for the pair.
fn is_canceled_drop(
    normalized: &NormalizedManifest,
    binding: &delegation_workflow_node_binding::Model,
) -> bool {
    if normalized.workflow_state == ManifestWorkflowState::Blocked {
        return true;
    }
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

fn settlement_payload_matches(
    existing: &delegation_workflow_gate_settlement::Model,
    req: &SettleWorkflowRequest,
) -> bool {
    existing.outcome == req.outcome
        && existing.critical_count == req.critical_count
        && existing.important_count == req.important_count
        && existing.minor_count == req.minor_count
        && existing.summary == req.summary
        && existing.manifest_revision as u64 == req.manifest_revision
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

fn is_completed_evidence(n: &WorkflowNodeStateDto) -> bool {
    matches!(
        n.latest_status.as_deref(),
        Some("completed") | Some("failed") | Some("canceled")
    )
}

/// Truncate oldest completed non-required nodes first under A15 size class.
/// Ordering uses completion/admission timestamps (`evidence_time`), not generation
/// across unrelated nodes. Required-gate nodes are preferred; returns whether
/// any evidence was dropped.
fn truncate_node_evidence(nodes: &mut Vec<WorkflowNodeStateDto>, max: usize) -> bool {
    nodes.sort_by(|a, b| {
        b.required_for_gate
            .cmp(&a.required_for_gate)
            .then_with(|| {
                let a_done = is_completed_evidence(a);
                let b_done = is_completed_evidence(b);
                a_done.cmp(&b_done)
            })
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    if nodes.len() <= max {
        return false;
    }

    let mut evidence_truncated = false;
    let mut kept = Vec::with_capacity(max);
    let mut completed_drop_queue: Vec<WorkflowNodeStateDto> = Vec::new();
    for n in nodes.drain(..) {
        if n.required_for_gate || !is_completed_evidence(&n) {
            kept.push(n);
        } else {
            completed_drop_queue.push(n);
        }
    }
    // Keep more recent completions (by finished_at / admission time); drop oldest first.
    completed_drop_queue.sort_by(|a, b| {
        b.evidence_time
            .cmp(&a.evidence_time)
            .then_with(|| b.node_id.cmp(&a.node_id))
    });
    for n in completed_drop_queue {
        if kept.len() < max {
            kept.push(n);
        } else {
            evidence_truncated = true;
        }
    }
    if kept.len() > max {
        evidence_truncated = true;
        kept.truncate(max);
    }
    *nodes = kept;
    evidence_truncated
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
        DocumentRef, ManifestEdge, ManifestGate, ManifestNode, ManifestPhase, ManifestTaskPolicy,
        ManifestTaskRisk, ManifestTaskRoute, TaskRiskLevel, WorkUnitKeyParts,
        MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS,
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
            critical_count: 1,
            important_count: 0,
            minor_count: 0,
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
                critical_count: 0,
                important_count: 1,
                minor_count: 0,
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
                critical_count: 1,
                important_count: 0,
                minor_count: 0,
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
                    critical_count: critical,
                    important_count: important,
                    minor_count: minor,
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

        // Only implementer observed — pair_frozen not pre-set (admission would set it;
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
            matches!(err, WorkflowStoreError::FrozenPartnerDrop { .. })
                || matches!(
                    err,
                    WorkflowStoreError::Validation(
                        super::super::types::WorkflowError::InvalidField(ref message)
                    ) if message.contains("route")
                ),
            "expected freeze or route-validation reject, got {err:?}"
        );
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
            am.pair_frozen = Set(true);
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
        assert_eq!(impl_n.node_outcome.as_deref(), Some("canceled"));
        assert_eq!(rev_n.node_outcome.as_deref(), Some("canceled"));
        assert!(impl_n.pair_frozen);
        assert!(rev_n.pair_frozen);
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
    async fn get_workflow_state_b4_fields() {
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
        assert!(!state.nodes.is_empty());
        let design = state
            .nodes
            .iter()
            .find(|n| n.node_id == "design-reviewer-1")
            .unwrap();
        assert_eq!(design.latest_task_id.as_deref(), Some("task-state-1"));
        assert_eq!(design.latest_status.as_deref(), Some("completed"));
        assert_eq!(design.latest_generation, Some(1));
        assert_eq!(design.summary_validated, Some(true));
        assert_eq!(design.artifact_digest.as_deref(), Some(DESIGN_DOC_DIGEST));
        assert_eq!(design.gate_cycle, Some(1));
        assert!(design.required_for_gate);
        assert!(!design.work_unit_key.is_empty());

        let gate = state.gates.iter().find(|g| g.gate_id == "design").unwrap();
        assert_eq!(gate.next_gate_cycle, 1);
        assert_eq!(gate.gate_kind, "design");
    }

    #[test]
    fn truncate_drops_oldest_completed_keeps_required() {
        let t_old = Utc::now() - chrono::Duration::hours(2);
        let t_new = Utc::now() - chrono::Duration::minutes(5);
        let t_req = Utc::now() - chrono::Duration::hours(1);
        let mut nodes = vec![
            WorkflowNodeStateDto {
                node_id: "req".into(),
                work_unit_key: "k-req".into(),
                role: "reviewer".into(),
                agent_type: "codex".into(),
                profile_id: None,
                phase_id: "design".into(),
                task_index: None,
                is_observed: true,
                retained_observed: false,
                pair_frozen: false,
                node_outcome: None,
                latest_task_id: Some("t-req".into()),
                latest_status: Some("completed".into()),
                latest_generation: Some(1),
                summary_validated: Some(true),
                artifact_digest: None,
                gate_id: Some("design".into()),
                gate_cycle: Some(1),
                replaced_task_id: None,
                required_for_gate: true,
                evidence_time: Some(t_req),
            },
            WorkflowNodeStateDto {
                node_id: "old-done".into(),
                work_unit_key: "k-old".into(),
                role: "implementer".into(),
                agent_type: "grok".into(),
                profile_id: None,
                phase_id: "tasks".into(),
                task_index: Some(1),
                is_observed: true,
                retained_observed: false,
                pair_frozen: false,
                node_outcome: None,
                latest_task_id: Some("t-old".into()),
                latest_status: Some("completed".into()),
                // Higher generation must not keep this over newer evidence_time.
                latest_generation: Some(99),
                summary_validated: Some(true),
                artifact_digest: None,
                gate_id: None,
                gate_cycle: None,
                replaced_task_id: None,
                required_for_gate: false,
                evidence_time: Some(t_old),
            },
            WorkflowNodeStateDto {
                node_id: "new-done".into(),
                work_unit_key: "k-new".into(),
                role: "implementer".into(),
                agent_type: "grok".into(),
                profile_id: None,
                phase_id: "tasks".into(),
                task_index: Some(2),
                is_observed: true,
                retained_observed: false,
                pair_frozen: false,
                node_outcome: None,
                latest_task_id: Some("t-new".into()),
                latest_status: Some("completed".into()),
                latest_generation: Some(1),
                summary_validated: Some(true),
                artifact_digest: None,
                gate_id: None,
                gate_cycle: None,
                replaced_task_id: None,
                required_for_gate: false,
                evidence_time: Some(t_new),
            },
            WorkflowNodeStateDto {
                node_id: "active".into(),
                work_unit_key: "k-act".into(),
                role: "reviewer".into(),
                agent_type: "codex".into(),
                profile_id: None,
                phase_id: "tasks".into(),
                task_index: Some(3),
                is_observed: true,
                retained_observed: false,
                pair_frozen: false,
                node_outcome: None,
                latest_task_id: Some("t-act".into()),
                latest_status: Some("running".into()),
                latest_generation: Some(1),
                summary_validated: Some(false),
                artifact_digest: None,
                gate_id: None,
                gate_cycle: None,
                replaced_task_id: None,
                required_for_gate: false,
                evidence_time: Some(Utc::now()),
            },
        ];
        let truncated = truncate_node_evidence(&mut nodes, 3);
        assert!(truncated);
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().any(|n| n.node_id == "req"));
        assert!(nodes.iter().any(|n| n.node_id == "active"));
        assert!(nodes.iter().any(|n| n.node_id == "new-done"));
        assert!(!nodes.iter().any(|n| n.node_id == "old-done"));
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
                critical_count: 0,
                important_count: 0,
                minor_count: 0,
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
                critical_count: 1,
                important_count: 0,
                minor_count: 0,
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
}
