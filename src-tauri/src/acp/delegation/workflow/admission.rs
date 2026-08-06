//! Workflow run admission enforcement + graph-revision hooks.
//!
//! - B2/A14: active vs retained-observed binding; role/agent/profile match
//! - A8.3: block **new** Task first-dispatch while Plan re-open / not approved
//! - B6: Final reviewer / fixer / re-review readiness via `evaluate_execution_gate`
//! - Task 5: independent child sessions + complete routed-cohort freezing
//! - B5/A10: run_binding + graph_revision same transaction; post-commit emit
//! - A1 key without manifest: compatibility_nudge only
//! - Non-workflow / non-A1 key: no workflow write

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, JoinType, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use sha2::{Digest, Sha256};
use std::path::Path as FsPath;

use crate::acp::delegation::card_summary::{
    card_summary_to_json, extract_card_summary_with_report_fallback,
    parse_and_validate_summary_json, CardSummary, ReviewVerdict, WorkStatus,
};
use crate::acp::delegation::runtime_stats::DelegationTouchedFile;
use crate::acp::delegation::store::{
    classify_sqlite_transient, extract_sqlite_codes, TaskStoreError,
};
use crate::db::entities::delegation_completion_tool_intent;
use crate::db::entities::delegation_task_run::{self, AdmissionClass, DelegationRunStatus};
use crate::db::entities::delegation_workflow::{self, WorkflowState};
use crate::db::entities::delegation_workflow_gate_settlement::{self, GateSettlementOutcome};
use crate::db::entities::delegation_workflow_gate_state;
use crate::db::entities::delegation_workflow_manifest_revision;
use crate::db::entities::delegation_workflow_node_binding;
use crate::db::entities::delegation_workflow_run_binding;
use crate::db::AppDatabase;
use crate::web::event_bridge::EventEmitter;

use super::artifact_resolver::{
    resolve_git_head_clean, resolve_producer_completion, resolve_reviewer_completion,
    ArtifactError, ArtifactFailure, ResolvedArtifact,
};
use super::completion_evidence::load_validated_completion_evidence;
use super::error::{CompletionEvidenceError, WorkflowAdmissionRecoveryError};
use super::events::{emit_workflow_compatibility_nudge, emit_workflow_graph_changed};
use super::evidence_scope::{
    build_admission_completion_context, AdmissionCandidate, EvidenceScopeError, WorkflowStore,
};
use super::final_findings::{load_active_final_findings_package_v1, FinalFindingsError};
use super::gates::{
    evaluate_execution_gate, ExecutionGateInput, ExecutionGateKind, ExecutionGateRunEvidence,
    RequiredReviewerEvidence, TerminalRunStatus,
};
use super::key::parse_recognized_work_unit_key;
use super::project::{evidence_from_run_and_binding, evidence_from_run_binding_and_validated};
use super::recovery_policy::decide_workflow_recovery;
use super::store::load_workflow_recovery_snapshot_conn;
use super::types::{
    AcceptedToolIntent, CompleteWorkRequest, DocumentGateKind, InstructionBlockV1,
    ManifestDocument, ParsedWorkUnitKey, WorkflowChildMcpBinding,
    COMPLETE_WORK_REPORT_FILE_MAX_BYTES, COMPLETE_WORK_SUMMARY_MAX_BYTES, PHASE_FINAL, PHASE_TASKS,
    WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use super::validate::validate_manifest_document;
use super::{CompletionOutcome, CompletionRole};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Whether this admission is generation-1 first dispatch or continue/replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDispatchKind {
    /// Generation-1 with no established lineage (B2: active binding required).
    FirstDispatch,
    /// Continue or legal replacement on existing lineage (B2: retained_observed OK).
    ContinueOrReplacement,
}

/// Post-commit workflow event to fire after the run transaction commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowTxnSideEffect {
    None,
    CompatibilityNudge {
        parent_conversation_id: i32,
    },
    GraphChanged {
        parent_conversation_id: i32,
        workflow_id: String,
        graph_revision: u64,
    },
}

impl WorkflowTxnSideEffect {
    pub fn emit(&self, emitter: &EventEmitter) {
        match self {
            Self::None => {}
            Self::CompatibilityNudge {
                parent_conversation_id,
            } => {
                emit_workflow_compatibility_nudge(emitter, *parent_conversation_id);
            }
            Self::GraphChanged {
                parent_conversation_id,
                workflow_id,
                graph_revision,
            } => {
                emit_workflow_graph_changed(
                    emitter,
                    *parent_conversation_id,
                    workflow_id,
                    *graph_revision,
                );
            }
        }
    }
}

/// Inputs for a new-run workflow admission (gen-1 or continue insert).
#[derive(Debug, Clone)]
pub struct WorkflowAdmitInput<'a> {
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub task_id: &'a str,
    pub work_unit_key: Option<&'a str>,
    pub agent_type: &'a str,
    pub profile_id: Option<&'a str>,
    pub lineage_root_task_id: &'a str,
    pub generation: i64,
    pub kind: AdmissionDispatchKind,
    /// Durable admission class of the **new** run (A6: unexpected_continue vs
    /// normal_revision re-review).
    pub admission_class: AdmissionClass,
    /// Workspace path of the admitting run (for Final first-pass tip fallback).
    pub workspace_path: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompleteWorkError {
    #[error("completion tool is not authorized for this live child")]
    Unauthorized,
    #[error("completion tool call id was reused with different arguments")]
    CallConflict,
    #[error("completion outcome is incompatible with the workflow node role")]
    RoleMismatch,
    #[error("invalid complete_work arguments: {0}")]
    InvalidArguments(String),
    #[error("completion intent persistence failed: {0}")]
    Persistence(String),
}

impl CompleteWorkError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "completion_tool_unauthorized",
            Self::CallConflict => "completion_tool_call_conflict",
            Self::RoleMismatch => "completion_outcome_role_mismatch",
            Self::InvalidArguments(_) => "invalid_arguments",
            Self::Persistence(_) => "persistence",
        }
    }
}

fn completion_role(value: &str) -> Option<CompletionRole> {
    match value {
        "reviewer" => Some(CompletionRole::Reviewer),
        "author" => Some(CompletionRole::Author),
        "implementer" => Some(CompletionRole::Implementer),
        "fixer" => Some(CompletionRole::Fixer),
        _ => None,
    }
}

fn completion_outcome(value: &str) -> Option<CompletionOutcome> {
    serde_json::from_value(serde_json::Value::String(value.to_string())).ok()
}

fn accepted_tool_intent(
    model: delegation_completion_tool_intent::Model,
) -> Result<AcceptedToolIntent, CompleteWorkError> {
    let outcome = completion_outcome(&model.outcome).ok_or_else(|| {
        CompleteWorkError::Persistence("stored completion outcome is invalid".into())
    })?;
    Ok(AcceptedToolIntent {
        intent_id: model.intent_id,
        task_id: model.task_id,
        child_tool_call_id: model.child_tool_call_id,
        accepted_ordinal: model.accepted_ordinal,
        outcome,
        summary: model.summary,
        report_file: model.report_hint,
    })
}

const COMPLETE_WORK_TXN_MAX_ATTEMPTS: u8 = 10;

enum CompleteWorkAttemptError {
    Contract(CompleteWorkError),
    Database {
        error: sea_orm::DbErr,
        retry_safe: bool,
    },
}

fn completion_constraint_race_codes(primary: i32, extended: i32) -> bool {
    const SQLITE_CONSTRAINT: i32 = 19;
    const SQLITE_CONSTRAINT_PRIMARYKEY: i32 = 1555;
    const SQLITE_CONSTRAINT_UNIQUE: i32 = 2067;

    primary == SQLITE_CONSTRAINT
        && matches!(
            extended,
            SQLITE_CONSTRAINT_PRIMARYKEY | SQLITE_CONSTRAINT_UNIQUE
        )
}

fn completion_write_race(error: &sea_orm::DbErr) -> bool {
    classify_sqlite_transient(error).is_some()
        || extract_sqlite_codes(error)
            .is_some_and(|codes| completion_constraint_race_codes(codes.primary, codes.extended))
}

fn completion_retry_delay(attempt: u8) -> std::time::Duration {
    std::time::Duration::from_millis(u64::from(attempt) * 2)
}

#[cfg(test)]
pub(crate) struct CompleteWorkTestControl {
    snapshot_barrier: Option<std::sync::Arc<tokio::sync::Barrier>>,
    transient_body_failures: std::sync::atomic::AtomicUsize,
    commit_failures: std::sync::atomic::AtomicUsize,
    attempts: std::sync::atomic::AtomicUsize,
    rollbacks: std::sync::atomic::AtomicUsize,
    retries: std::sync::atomic::AtomicUsize,
}

#[cfg(not(test))]
struct CompleteWorkTestControl;

struct CompleteWorkAttemptContext<'a> {
    _attempt: u8,
    control: Option<&'a CompleteWorkTestControl>,
}

#[cfg(test)]
impl CompleteWorkTestControl {
    pub(crate) fn snapshot_race(parties: usize) -> Self {
        Self {
            snapshot_barrier: Some(std::sync::Arc::new(tokio::sync::Barrier::new(parties))),
            transient_body_failures: std::sync::atomic::AtomicUsize::new(0),
            commit_failures: std::sync::atomic::AtomicUsize::new(0),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            rollbacks: std::sync::atomic::AtomicUsize::new(0),
            retries: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub(crate) fn transient_body_failures(count: usize) -> Self {
        Self::with_failures(count, 0)
    }

    pub(crate) fn commit_failures(count: usize) -> Self {
        Self::with_failures(0, count)
    }

    fn with_failures(transient_body_failures: usize, commit_failures: usize) -> Self {
        Self {
            snapshot_barrier: None,
            transient_body_failures: std::sync::atomic::AtomicUsize::new(transient_body_failures),
            commit_failures: std::sync::atomic::AtomicUsize::new(commit_failures),
            attempts: std::sync::atomic::AtomicUsize::new(0),
            rollbacks: std::sync::atomic::AtomicUsize::new(0),
            retries: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn consume(counter: &std::sync::atomic::AtomicUsize) -> bool {
        counter
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| (remaining > 0).then(|| remaining - 1),
            )
            .is_ok()
    }

    fn begin_attempt(&self) {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn fail_body(&self) -> bool {
        Self::consume(&self.transient_body_failures)
    }

    fn fail_commit(&self) -> bool {
        Self::consume(&self.commit_failures)
    }

    async fn wait_after_snapshot(&self, attempt: u8) {
        if attempt == 1 {
            if let Some(barrier) = self.snapshot_barrier.as_ref() {
                barrier.wait().await;
            }
        }
    }

    fn record_rollback(&self) {
        self.rollbacks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn record_retry(&self) {
        self.retries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn attempts(&self) -> usize {
        self.attempts.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn rollbacks(&self) -> usize {
        self.rollbacks.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn retries(&self) -> usize {
        self.retries.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Load the immutable workflow binding after run admission commits and before
/// a forced child is launched. A missing run binding means a non-workflow
/// child; a dangling committed workflow reference is corruption and fails closed.
pub async fn load_workflow_child_mcp_binding(
    db: &AppDatabase,
    task_id: &str,
) -> Result<Option<WorkflowChildMcpBinding>, CompleteWorkError> {
    let Some(binding) = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(&db.conn)
        .await
        .map_err(|error| CompleteWorkError::Persistence(error.to_string()))?
    else {
        return Ok(None);
    };
    let Some(workflow) = delegation_workflow::Entity::find_by_id(binding.workflow_id.clone())
        .one(&db.conn)
        .await
        .map_err(|error| CompleteWorkError::Persistence(error.to_string()))?
    else {
        return Err(CompleteWorkError::Persistence(format!(
            "workflow {} referenced by task {task_id} is missing",
            binding.workflow_id
        )));
    };
    Ok(Some(WorkflowChildMcpBinding {
        task_id: task_id.to_string(),
        workflow_id: binding.workflow_id,
        protocol_version: workflow.completion_protocol_version,
        node_id: binding.node_id,
    }))
}

/// Re-authorize and persist one child completion call in a single write
/// transaction. The request digest excludes all platform-owned identity.
pub async fn accept_complete_work_txn(
    db: &AppDatabase,
    task_id: &str,
    child_connection_id: &str,
    child_tool_call_id: &str,
    request: &CompleteWorkRequest,
) -> Result<AcceptedToolIntent, CompleteWorkError> {
    accept_complete_work_txn_inner(
        db,
        task_id,
        child_connection_id,
        child_tool_call_id,
        request,
        None,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn accept_complete_work_txn_with_test_control(
    db: &AppDatabase,
    task_id: &str,
    child_connection_id: &str,
    child_tool_call_id: &str,
    request: &CompleteWorkRequest,
    control: std::sync::Arc<CompleteWorkTestControl>,
) -> Result<AcceptedToolIntent, CompleteWorkError> {
    accept_complete_work_txn_inner(
        db,
        task_id,
        child_connection_id,
        child_tool_call_id,
        request,
        Some(control.as_ref()),
    )
    .await
}

async fn accept_complete_work_txn_inner(
    db: &AppDatabase,
    task_id: &str,
    child_connection_id: &str,
    child_tool_call_id: &str,
    request: &CompleteWorkRequest,
    control: Option<&CompleteWorkTestControl>,
) -> Result<AcceptedToolIntent, CompleteWorkError> {
    if child_tool_call_id.is_empty() {
        return Err(CompleteWorkError::InvalidArguments(
            "child tool call identity is empty".into(),
        ));
    }
    if request
        .summary
        .as_ref()
        .is_some_and(|value| value.len() > COMPLETE_WORK_SUMMARY_MAX_BYTES)
        || request
            .report_file
            .as_ref()
            .is_some_and(|value| value.len() > COMPLETE_WORK_REPORT_FILE_MAX_BYTES)
    {
        return Err(CompleteWorkError::InvalidArguments(
            "complete_work string exceeds its schema bound".into(),
        ));
    }
    let canonical = serde_json::to_vec(request)
        .map_err(|error| CompleteWorkError::InvalidArguments(error.to_string()))?;
    let request_digest = format!("{:x}", Sha256::digest(canonical));

    for attempt in 1..=COMPLETE_WORK_TXN_MAX_ATTEMPTS {
        #[cfg(test)]
        if let Some(control) = control {
            control.begin_attempt();
        }
        match accept_complete_work_once(
            db,
            task_id,
            child_connection_id,
            child_tool_call_id,
            request,
            &request_digest,
            CompleteWorkAttemptContext {
                _attempt: attempt,
                control,
            },
        )
        .await
        {
            Ok(intent) => return Ok(intent),
            Err(CompleteWorkAttemptError::Contract(error)) => return Err(error),
            Err(CompleteWorkAttemptError::Database {
                error,
                retry_safe: true,
            }) if completion_write_race(&error) && attempt < COMPLETE_WORK_TXN_MAX_ATTEMPTS => {
                #[cfg(test)]
                if let Some(control) = control {
                    control.record_retry();
                }
                tokio::time::sleep(completion_retry_delay(attempt)).await;
            }
            Err(CompleteWorkAttemptError::Database { error, .. }) => {
                return Err(CompleteWorkError::Persistence(error.to_string()));
            }
        }
    }

    unreachable!("bounded complete_work retry loop always returns")
}

async fn accept_complete_work_once(
    db: &AppDatabase,
    task_id: &str,
    child_connection_id: &str,
    child_tool_call_id: &str,
    request: &CompleteWorkRequest,
    request_digest: &str,
    attempt_context: CompleteWorkAttemptContext<'_>,
) -> Result<AcceptedToolIntent, CompleteWorkAttemptError> {
    let control = attempt_context.control;
    let txn = db
        .conn
        .begin()
        .await
        .map_err(|error| CompleteWorkAttemptError::Database {
            error,
            retry_safe: true,
        })?;

    let result = async {
        #[cfg(test)]
        if control.is_some_and(CompleteWorkTestControl::fail_body) {
            return Err(CompleteWorkAttemptError::Database {
                error: sea_orm::DbErr::Custom("database is locked".into()),
                retry_safe: true,
            });
        }
        let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
            .one(&txn)
            .await
            .map_err(|error| CompleteWorkAttemptError::Database {
                error,
                retry_safe: true,
            })?
            .ok_or(CompleteWorkAttemptError::Contract(
                CompleteWorkError::Unauthorized,
            ))?;
        if run.status != DelegationRunStatus::Running
            || run.child_connection_id.as_deref() != Some(child_connection_id)
        {
            return Err(CompleteWorkAttemptError::Contract(
                CompleteWorkError::Unauthorized,
            ));
        }
        let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
            .one(&txn)
            .await
            .map_err(|error| CompleteWorkAttemptError::Database {
                error,
                retry_safe: true,
            })?
            .ok_or(CompleteWorkAttemptError::Contract(
                CompleteWorkError::Unauthorized,
            ))?;
        let workflow = delegation_workflow::Entity::find_by_id(binding.workflow_id.clone())
            .one(&txn)
            .await
            .map_err(|error| CompleteWorkAttemptError::Database {
                error,
                retry_safe: true,
            })?
            .ok_or(CompleteWorkAttemptError::Contract(
                CompleteWorkError::Unauthorized,
            ))?;
        if workflow.completion_protocol_version != 2 {
            return Err(CompleteWorkAttemptError::Contract(
                CompleteWorkError::Unauthorized,
            ));
        }
        let node = delegation_workflow_node_binding::Entity::find_by_id((
            binding.workflow_id.clone(),
            binding.node_id.clone(),
        ))
        .one(&txn)
        .await
        .map_err(|error| CompleteWorkAttemptError::Database {
            error,
            retry_safe: true,
        })?
        .ok_or(CompleteWorkAttemptError::Contract(
            CompleteWorkError::Unauthorized,
        ))?;
        let role = completion_role(&node.role).ok_or(CompleteWorkAttemptError::Contract(
            CompleteWorkError::Unauthorized,
        ))?;
        if !role.accepts(request.outcome) {
            return Err(CompleteWorkAttemptError::Contract(
                CompleteWorkError::RoleMismatch,
            ));
        }

        if let Some(existing) = delegation_completion_tool_intent::Entity::find()
            .filter(delegation_completion_tool_intent::Column::TaskId.eq(task_id.to_string()))
            .filter(
                delegation_completion_tool_intent::Column::ChildToolCallId
                    .eq(child_tool_call_id.to_string()),
            )
            .one(&txn)
            .await
            .map_err(|error| CompleteWorkAttemptError::Database {
                error,
                retry_safe: true,
            })?
        {
            if existing.request_digest != request_digest {
                return Err(CompleteWorkAttemptError::Contract(
                    CompleteWorkError::CallConflict,
                ));
            }
            return accepted_tool_intent(existing).map_err(CompleteWorkAttemptError::Contract);
        }

        let accepted_ordinal = delegation_completion_tool_intent::Entity::find()
            .filter(delegation_completion_tool_intent::Column::TaskId.eq(task_id.to_string()))
            .order_by_desc(delegation_completion_tool_intent::Column::AcceptedOrdinal)
            .one(&txn)
            .await
            .map_err(|error| CompleteWorkAttemptError::Database {
                error,
                retry_safe: true,
            })?
            .map_or(1, |row| row.accepted_ordinal + 1);
        #[cfg(test)]
        if let Some(control) = control {
            control.wait_after_snapshot(attempt_context._attempt).await;
        }
        let model = delegation_completion_tool_intent::ActiveModel {
            intent_id: Set(format!("platform:{task_id}:{accepted_ordinal}")),
            task_id: Set(task_id.to_string()),
            child_tool_call_id: Set(child_tool_call_id.to_string()),
            accepted_ordinal: Set(accepted_ordinal),
            outcome: Set(request.outcome.as_str().to_string()),
            summary: Set(request.summary.clone()),
            report_hint: Set(request.report_file.clone()),
            request_digest: Set(request_digest.to_string()),
            created_at: Set(Utc::now()),
        }
        .insert(&txn)
        .await
        .map_err(|error| CompleteWorkAttemptError::Database {
            error,
            retry_safe: true,
        })?;
        accepted_tool_intent(model).map_err(CompleteWorkAttemptError::Contract)
    }
    .await;

    match result {
        Ok(intent) => {
            #[cfg(test)]
            if control.is_some_and(CompleteWorkTestControl::fail_commit) {
                return rollback_complete_work_attempt(
                    txn,
                    CompleteWorkAttemptError::Database {
                        error: sea_orm::DbErr::Custom("forced commit failure".into()),
                        retry_safe: false,
                    },
                    control,
                )
                .await;
            }
            txn.commit()
                .await
                .map_err(|error| CompleteWorkAttemptError::Database {
                    error,
                    retry_safe: false,
                })?;
            Ok(intent)
        }
        Err(error) => rollback_complete_work_attempt(txn, error, control).await,
    }
}

async fn rollback_complete_work_attempt(
    txn: sea_orm::DatabaseTransaction,
    error: CompleteWorkAttemptError,
    _control: Option<&CompleteWorkTestControl>,
) -> Result<AcceptedToolIntent, CompleteWorkAttemptError> {
    match txn.rollback().await {
        Ok(()) => {
            #[cfg(test)]
            if let Some(control) = _control {
                control.record_rollback();
            }
            Err(error)
        }
        Err(rollback_error) => Err(CompleteWorkAttemptError::Database {
            error: rollback_error,
            retry_safe: false,
        }),
    }
}

// ---------------------------------------------------------------------------
// Errors → TaskStoreError
// ---------------------------------------------------------------------------

fn admission_err(code: &str, msg: impl Into<String>) -> TaskStoreError {
    TaskStoreError::WorkflowAdmission {
        code: code.to_string(),
        message: msg.into(),
    }
}

fn artifact_admission_err(error: ArtifactError) -> TaskStoreError {
    admission_err(error.code(), error.to_string())
}

fn evidence_admission_err(error: EvidenceScopeError) -> TaskStoreError {
    admission_err(error.code(), error.to_string())
}

fn completion_evidence_admission_error(error: CompletionEvidenceError) -> TaskStoreError {
    admission_err(error.code(), error.to_string())
}

async fn resolve_v2_admission_head(workspace_path: Option<&str>) -> Result<String, TaskStoreError> {
    let workspace_path = workspace_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            artifact_admission_err(ArtifactError::Unavailable(
                ArtifactFailure::WorkspaceUnavailable,
            ))
        })?;
    resolve_git_head_clean(FsPath::new(workspace_path))
        .await
        .map(|artifact| artifact.head)
        .map_err(artifact_admission_err)
}

async fn revalidate_v2_reviewer_head(
    workspace_path: Option<&str>,
    expected_head: &str,
) -> Result<(), TaskStoreError> {
    let workspace_path = workspace_path
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            artifact_admission_err(ArtifactError::Unavailable(
                ArtifactFailure::WorkspaceUnavailable,
            ))
        })?;
    resolve_reviewer_completion(FsPath::new(workspace_path), expected_head)
        .await
        .map(|_| ())
        .map_err(artifact_admission_err)
}

fn map_db(e: sea_orm::DbErr) -> TaskStoreError {
    TaskStoreError::Permanent(format!("workflow admission db: {e}"))
}

pub async fn load_admitted_completion_instruction<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<Option<InstructionBlockV1>, TaskStoreError> {
    let Some(binding) = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(map_db)?
    else {
        return Ok(None);
    };
    let workflow = delegation_workflow::Entity::find_by_id(binding.workflow_id.clone())
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| {
            admission_err(
                "completion_instruction_binding_failed",
                "admitted completion workflow is missing",
            )
        })?;
    if workflow.completion_protocol_version != 2 {
        return Ok(None);
    }
    let node = delegation_workflow_node_binding::Entity::find_by_id((
        binding.workflow_id.clone(),
        binding.node_id.clone(),
    ))
    .one(conn)
    .await
    .map_err(map_db)?
    .ok_or_else(|| {
        admission_err(
            "completion_instruction_binding_failed",
            "admitted completion node is missing",
        )
    })?;
    let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;
    let workspace_path = run.workspace_path.as_deref().ok_or_else(|| {
        admission_err(
            "completion_instruction_binding_failed",
            "v2 admitted run has no workspace path",
        )
    })?;
    let store = WorkflowStore::new(conn, FsPath::new(workspace_path));
    let context = build_admission_completion_context(
        &store,
        &AdmissionCandidate {
            workflow: &workflow,
            node: &node,
            task_id,
            artifact_digest: binding.artifact_digest.as_deref(),
            reviewed_task_id: binding.reviewed_task_id.as_deref(),
            reviewed_generation: binding.reviewed_implementer_generation,
            producer_baseline_head: binding.producer_baseline_head.as_deref(),
        },
    )
    .await
    .map_err(evidence_admission_err)?;
    let persisted_round = binding
        .review_round
        .map(u32::try_from)
        .transpose()
        .map_err(|_| {
            admission_err(
                "completion_instruction_binding_failed",
                "persisted review round exceeds u32",
            )
        })?;
    if binding.gate_id != context.evidence_scope.gate_id
        || binding.gate_lineage != context.evidence_scope.gate_lineage
        || persisted_round != context.evidence_scope.review_round
        || binding.instruction_block_digest.as_deref() != Some(context.instruction.digest.as_str())
        || binding.evidence_scope_digest.as_deref() != Some(context.evidence_scope_digest.as_str())
        || binding.material_selector_digest != context.material_selector_digest
        || binding.subject_material_digest != context.subject_material_digest
        || binding.requirements_identity != context.requirements_identity
        || binding.task_specification_identity != context.task_specification_identity
        || binding.final_findings_identity != context.final_findings_identity
    {
        return Err(admission_err(
            "completion_instruction_binding_failed",
            "persisted completion instruction scope does not match durable admission inputs",
        ));
    }
    Ok(Some(context.instruction))
}

pub async fn append_admitted_completion_instruction<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
    parent_prose: &str,
) -> Result<String, TaskStoreError> {
    match load_admitted_completion_instruction(conn, task_id).await? {
        Some(instruction) => Ok(format!("{parent_prose}\n{}", instruction.canonical_utf8)),
        None => Ok(parent_prose.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Admit (new run insert path)
// ---------------------------------------------------------------------------

/// Validate binding + insert run_binding + B14 freeze + bump graph_revision.
///
/// Call **after** the run row is inserted in the same transaction.
pub async fn admit_workflow_run_txn<C: ConnectionTrait>(
    conn: &C,
    input: &WorkflowAdmitInput<'_>,
) -> Result<WorkflowTxnSideEffect, TaskStoreError> {
    let Some(key) = input.work_unit_key.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(WorkflowTxnSideEffect::None);
    };

    let recognized = parse_recognized_work_unit_key(key);
    let header = load_workflow_header(conn, input.parent_conversation_id).await?;

    let Some(header) = header else {
        // No durable manifest: A1 keys nudge only; non-A1 / legacy no-op.
        if recognized.is_some() {
            return Ok(WorkflowTxnSideEffect::CompatibilityNudge {
                parent_conversation_id: input.parent_conversation_id,
            });
        }
        return Ok(WorkflowTxnSideEffect::None);
    };

    // Workflow header present but key not A1 grammar → ignore (Sessions only).
    let Some(parsed) = recognized else {
        return Ok(WorkflowTxnSideEffect::None);
    };

    let binding = find_node_binding(conn, &header.workflow_id, key).await?;
    let Some(binding) = binding else {
        return Err(admission_err(
            "workflow_binding_missing",
            format!(
                "work_unit_key {key} is not bound on workflow {}",
                header.workflow_id
            ),
        ));
    };

    // B2: first dispatch needs active (not retired); continue/replacement also
    // allows retained_observed.
    let is_active = binding.retired_revision.is_none();
    let is_retained = binding.retained_observed;
    match input.kind {
        AdmissionDispatchKind::FirstDispatch => {
            if !is_active {
                return Err(admission_err(
                    "workflow_binding_not_active",
                    format!(
                        "first dispatch requires active binding for key {key} (node {})",
                        binding.node_id
                    ),
                ));
            }
        }
        AdmissionDispatchKind::ContinueOrReplacement => {
            if !is_active && !is_retained {
                return Err(admission_err(
                    "workflow_binding_retired",
                    format!(
                        "continue/replacement rejected: node {} is fully retired",
                        binding.node_id
                    ),
                ));
            }
        }
    }

    // A14: role/agent/profile must match binding (and key grammar).
    validate_identity_match(&binding, input.agent_type, input.profile_id, &parsed)?;

    // Canceled nodes cannot admit.
    if binding.node_outcome.is_some() {
        return Err(admission_err(
            "workflow_node_canceled",
            format!("node {} is canceled", binding.node_id),
        ));
    }

    ensure_child_conversation_independent(
        conn,
        &header.workflow_id,
        &binding.node_id,
        input.child_conversation_id,
    )
    .await?;

    let task_route = resolve_task_route(conn, &header, &binding, &parsed).await?;

    // A8.3 / B6 readiness for Task and Final (first-dispatch and Final re-entry).
    enforce_phase_readiness(
        conn,
        &header,
        &binding,
        &parsed,
        input.kind,
        &input.admission_class,
    )
    .await?;

    let now = Utc::now();
    let lineage_ordinal =
        next_lineage_ordinal(conn, &header.workflow_id, input.lineage_root_task_id).await?;

    let (
        gate_id,
        gate_cycle,
        artifact_digest,
        reviewed_task_id,
        reviewed_impl_gen,
        producer_baseline_head,
    ) = stamp_admission_fields(conn, &header, &binding, &parsed, input.workspace_path).await?;

    let completion_context = if header.completion_protocol_version == 2 {
        let workspace_path = input.workspace_path.ok_or_else(|| {
            admission_err(
                "completion_instruction_binding_failed",
                "v2 workflow admission requires a workspace path",
            )
        })?;
        let store = WorkflowStore::new(conn, FsPath::new(workspace_path));
        let context = build_admission_completion_context(
            &store,
            &AdmissionCandidate {
                workflow: &header,
                node: &binding,
                task_id: input.task_id,
                artifact_digest: artifact_digest.as_deref(),
                reviewed_task_id: reviewed_task_id.as_deref(),
                reviewed_generation: reviewed_impl_gen,
                producer_baseline_head: producer_baseline_head.as_deref(),
            },
        )
        .await
        .map_err(evidence_admission_err)?;
        if context.evidence_scope.gate_id != gate_id {
            return Err(admission_err(
                "completion_instruction_binding_failed",
                "completion gate identity disagrees with admission stamp",
            ));
        }
        Some(context)
    } else {
        None
    };

    let content_fingerprint = document_gate_content_fingerprint(&header, &parsed);
    let rb = delegation_workflow_run_binding::ActiveModel {
        task_id: Set(input.task_id.to_string()),
        workflow_id: Set(header.workflow_id.clone()),
        node_id: Set(binding.node_id.clone()),
        gate_id: Set(gate_id),
        gate_cycle: Set(gate_cycle),
        manifest_revision: Set(header.active_manifest_revision),
        content_fingerprint: Set(content_fingerprint),
        evidence_scope_digest: Set(completion_context
            .as_ref()
            .map(|context| context.evidence_scope_digest.clone())),
        gate_lineage: Set(completion_context
            .as_ref()
            .and_then(|context| context.evidence_scope.gate_lineage.clone())),
        review_round: Set(completion_context
            .as_ref()
            .and_then(|context| context.evidence_scope.review_round)
            .map(i64::from)),
        instruction_block_digest: Set(completion_context
            .as_ref()
            .map(|context| context.instruction.digest.clone())),
        material_selector_digest: Set(completion_context
            .as_ref()
            .and_then(|context| context.material_selector_digest.clone())),
        subject_material_digest: Set(completion_context
            .as_ref()
            .and_then(|context| context.subject_material_digest.clone())),
        requirements_identity: Set(completion_context
            .as_ref()
            .and_then(|context| context.requirements_identity.clone())),
        task_specification_identity: Set(completion_context
            .as_ref()
            .and_then(|context| context.task_specification_identity.clone())),
        final_findings_identity: Set(completion_context
            .as_ref()
            .and_then(|context| context.final_findings_identity.clone())),
        producer_baseline_head: Set(producer_baseline_head),
        artifact_digest: Set(artifact_digest),
        reviewed_task_id: Set(reviewed_task_id),
        reviewed_implementer_generation: Set(reviewed_impl_gen),
        lineage_ordinal: Set(lineage_ordinal),
        summary_validated: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
    };
    rb.insert(conn).await.map_err(map_db)?;

    mark_node_observed(conn, &binding, now).await?;
    if let Some((task_index, route_node_ids)) = task_route {
        mark_observed_and_freeze_cohort(conn, &header.workflow_id, task_index, &route_node_ids)
            .await?;
    }

    let next_rev = bump_graph_revision(conn, &header.workflow_id, now).await?;

    Ok(WorkflowTxnSideEffect::GraphChanged {
        parent_conversation_id: input.parent_conversation_id,
        workflow_id: header.workflow_id,
        graph_revision: next_rev,
    })
}

// ---------------------------------------------------------------------------
// Lifecycle hooks (existing run transitions)
// ---------------------------------------------------------------------------

/// After promote_running / terminal settle / provisional abandon that changes
/// projected state for a mapped run: bump graph_revision.
pub async fn on_mapped_run_transition_txn<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
    parent_conversation_id: i32,
) -> Result<WorkflowTxnSideEffect, TaskStoreError> {
    let Some(rb) = load_run_binding(conn, task_id).await? else {
        // Not mapped — maybe A1 key without binding (nudge only on admit).
        return Ok(WorkflowTxnSideEffect::None);
    };
    let now = Utc::now();
    let next_rev = bump_graph_revision(conn, &rb.workflow_id, now).await?;
    Ok(WorkflowTxnSideEffect::GraphChanged {
        parent_conversation_id,
        workflow_id: rb.workflow_id,
        graph_revision: next_rev,
    })
}

/// Terminal settle: stamp summary_validated + digests on run_binding, bump clock.
///
/// **Implementer / Final-fixer artifact digest (B3):**
/// - Prefer workspace `HEAD` commit id (`git rev-parse HEAD` in `workspace_path`)
/// - When HEAD is unavailable, leave `artifact_digest` **empty** — do **not**
///   copy free-text / card-summary commit SHAs. Gate coverage then relies on
///   generation / `reviewed_task_id` binding fields only.
pub async fn on_terminal_settle_txn<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
    parent_conversation_id: i32,
    card_summary_json: Option<&str>,
    run_status: &DelegationRunStatus,
    workspace_path: Option<&str>,
) -> Result<WorkflowTxnSideEffect, TaskStoreError> {
    let Some(rb) = load_run_binding(conn, task_id).await? else {
        return Ok(WorkflowTxnSideEffect::None);
    };

    let now = Utc::now();
    let summary = card_summary_json.and_then(parse_and_validate_summary_json);
    let node = delegation_workflow_node_binding::Entity::find_by_id((
        rb.workflow_id.clone(),
        rb.node_id.clone(),
    ))
    .one(conn)
    .await
    .map_err(map_db)?;
    let (validated, author_digest) = match (node.as_ref(), summary.as_ref()) {
        (Some(node), Some(CardSummary::Author { plan_digest, .. }))
            if node.role == "author" && node.phase_id == "plan" =>
        {
            (true, Some(plan_digest.clone()))
        }
        (Some(node), Some(CardSummary::Review { report_file, .. })) if node.role == "reviewer" => {
            let required_report = node.phase_id == "plan" || node.phase_id == PHASE_TASKS;
            let has_report = report_file
                .as_deref()
                .is_some_and(|path| !path.trim().is_empty());
            (!required_report || has_report, None)
        }
        (Some(node), Some(CardSummary::Implementation { .. }))
            if node.role == "implementer" || node.role == "fixer" =>
        {
            (true, None)
        }
        _ => (false, None),
    };

    let mut am: delegation_workflow_run_binding::ActiveModel = rb.clone().into();
    am.summary_validated = Set(validated);
    am.updated_at = Set(now);

    if let Some(digest) = author_digest.as_ref() {
        am.artifact_digest = Set(Some(digest.clone()));
    }

    if matches!(
        run_status,
        DelegationRunStatus::Completed
            | DelegationRunStatus::Failed
            | DelegationRunStatus::Canceled
    ) && rb.artifact_digest.is_none()
        && author_digest.is_none()
    {
        if let Some(digest) = workspace_head_commit(workspace_path) {
            am.artifact_digest = Set(Some(digest));
        }
        // B3: no card-summary SHA fallback when HEAD is unavailable.
    }

    am.update(conn).await.map_err(map_db)?;

    let next_rev = bump_graph_revision(conn, &rb.workflow_id, now).await?;
    Ok(WorkflowTxnSideEffect::GraphChanged {
        parent_conversation_id,
        workflow_id: rb.workflow_id,
        graph_revision: next_rev,
    })
}

/// Resolve and stamp the code artifact selected by an already-normalized v2
/// completion outcome. Task 10 calls this inside its evidence transaction;
/// semantic intent selection deliberately remains outside this Task 7 seam.
pub async fn resolve_and_stamp_terminal_artifact_txn<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
    outcome: CompletionOutcome,
) -> Result<Option<ResolvedArtifact>, TaskStoreError> {
    let Some(binding) = load_run_binding(conn, task_id).await? else {
        return Ok(None);
    };
    let header = delegation_workflow::Entity::find_by_id(binding.workflow_id.clone())
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| {
            TaskStoreError::Permanent(format!(
                "workflow {} referenced by task {task_id} is missing",
                binding.workflow_id
            ))
        })?;
    if header.completion_protocol_version != 2 {
        return Ok(None);
    }
    let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;
    let node = delegation_workflow_node_binding::Entity::find_by_id((
        binding.workflow_id.clone(),
        binding.node_id.clone(),
    ))
    .one(conn)
    .await
    .map_err(map_db)?
    .ok_or_else(|| {
        TaskStoreError::Permanent(format!(
            "workflow node {} referenced by task {task_id} is missing",
            binding.node_id
        ))
    })?;
    if (node.role == "implementer" && node.phase_id == PHASE_TASKS)
        || (node.role == "fixer" && node.phase_id == PHASE_FINAL)
    {
        if !matches!(
            outcome,
            CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns
        ) {
            let mut active: delegation_workflow_run_binding::ActiveModel = binding.into();
            active.artifact_digest = Set(None);
            active.updated_at = Set(Utc::now());
            active.update(conn).await.map_err(map_db)?;
            return Ok(None);
        }
        let workspace = required_artifact_workspace(&run)?;
        let baseline = binding
            .producer_baseline_head
            .as_deref()
            .map(str::trim)
            .filter(|head| !head.is_empty())
            .ok_or_else(|| {
                artifact_admission_err(ArtifactError::Unavailable(
                    ArtifactFailure::ExpectedArtifactInvalid,
                ))
            })?;
        let allow_noop_verification = if node.role == "implementer" {
            durable_task_allows_noop(conn, &header, node.task_index).await?
        } else {
            false
        };
        let artifact = resolve_producer_completion(
            FsPath::new(workspace),
            outcome,
            baseline,
            allow_noop_verification,
        )
        .await
        .map_err(artifact_admission_err)?;
        let mut active: delegation_workflow_run_binding::ActiveModel = binding.into();
        active.artifact_digest = Set(artifact
            .as_ref()
            .map(|resolved| resolved.digest().to_string()));
        active.updated_at = Set(Utc::now());
        active.update(conn).await.map_err(map_db)?;
        return Ok(artifact);
    }

    if node.role == "reviewer" && (node.phase_id == PHASE_TASKS || node.phase_id == PHASE_FINAL) {
        let workspace = required_artifact_workspace(&run)?;
        let expected = binding
            .artifact_digest
            .as_deref()
            .map(str::trim)
            .filter(|head| !head.is_empty())
            .ok_or_else(|| {
                artifact_admission_err(ArtifactError::Unavailable(
                    ArtifactFailure::ExpectedArtifactInvalid,
                ))
            })?;
        return resolve_reviewer_completion(FsPath::new(workspace), expected)
            .await
            .map(Some)
            .map_err(artifact_admission_err);
    }

    Ok(None)
}

fn required_artifact_workspace(run: &delegation_task_run::Model) -> Result<&str, TaskStoreError> {
    run.workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            artifact_admission_err(ArtifactError::Unavailable(
                ArtifactFailure::WorkspaceUnavailable,
            ))
        })
}

async fn durable_task_allows_noop<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    task_index: Option<i64>,
) -> Result<bool, TaskStoreError> {
    let Some(task_index) = task_index.and_then(|index| u32::try_from(index).ok()) else {
        return Ok(false);
    };
    let Some(document) = load_active_manifest_doc(conn, header).await? else {
        return Ok(false);
    };
    let Ok(normalized) = validate_manifest_document(&document) else {
        return Ok(false);
    };
    Ok(normalized
        .task_policies
        .iter()
        .find(|policy| policy.task_index == task_index)
        .is_some_and(|policy| policy.allow_noop_verification))
}

/// Read `git rev-parse HEAD` from a workspace path. Returns `None` on any failure
/// (missing git, not a repo, empty path). Synchronous by design for settle hooks.
fn workspace_head_commit(workspace_path: Option<&str>) -> Option<String> {
    let path = workspace_path.map(str::trim).filter(|s| !s.is_empty())?;
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() || s.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    Some(s)
}

/// Provisional abandon: delete run_binding (if any) and bump graph clock.
pub async fn on_provisional_abandon_txn<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
    parent_conversation_id: i32,
) -> Result<WorkflowTxnSideEffect, TaskStoreError> {
    let Some(rb) = load_run_binding(conn, task_id).await? else {
        return Ok(WorkflowTxnSideEffect::None);
    };
    let workflow_id = rb.workflow_id.clone();
    delegation_workflow_run_binding::Entity::delete_by_id(task_id.to_string())
        .exec(conn)
        .await
        .map_err(map_db)?;

    let now = Utc::now();
    let next_rev = bump_graph_revision(conn, &workflow_id, now).await?;
    Ok(WorkflowTxnSideEffect::GraphChanged {
        parent_conversation_id,
        workflow_id,
        graph_revision: next_rev,
    })
}

// ---------------------------------------------------------------------------
// Identity / readiness
// ---------------------------------------------------------------------------

fn validate_identity_match(
    binding: &delegation_workflow_node_binding::Model,
    agent_type: &str,
    profile_id: Option<&str>,
    parsed: &ParsedWorkUnitKey,
) -> Result<(), TaskStoreError> {
    // Agent / profile from run must match binding.
    if binding.agent_type != agent_type {
        return Err(admission_err(
            "workflow_agent_mismatch",
            format!(
                "agent_type {agent_type} does not match binding {}",
                binding.agent_type
            ),
        ));
    }
    let bind_profile = binding.profile_id.as_deref();
    let run_profile = profile_id;
    if bind_profile != run_profile {
        return Err(admission_err(
            "workflow_profile_mismatch",
            format!("profile_id {run_profile:?} does not match binding {bind_profile:?}"),
        ));
    }

    // Role from key vs binding.
    let (expected_role, expected_phase) = match parsed {
        ParsedWorkUnitKey::Design { .. } => ("reviewer", "design"),
        ParsedWorkUnitKey::PlanAuthor { .. } => ("author", "plan"),
        ParsedWorkUnitKey::PlanReviewer { .. } => ("reviewer", "plan"),
        ParsedWorkUnitKey::TaskImplementer { .. } => ("implementer", PHASE_TASKS),
        ParsedWorkUnitKey::TaskReviewer { .. } => ("reviewer", PHASE_TASKS),
        ParsedWorkUnitKey::FinalReviewer { .. } => ("reviewer", PHASE_FINAL),
        ParsedWorkUnitKey::FinalFixer { .. } => ("fixer", PHASE_FINAL),
    };
    if binding.role != expected_role {
        return Err(admission_err(
            "workflow_role_mismatch",
            format!(
                "role {} does not match key role {expected_role}",
                binding.role
            ),
        ));
    }
    if binding.phase_id != expected_phase {
        return Err(admission_err(
            "workflow_phase_mismatch",
            format!(
                "phase {} does not match key phase {expected_phase}",
                binding.phase_id
            ),
        ));
    }
    Ok(())
}

async fn ensure_child_conversation_independent<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    node_id: &str,
    child_conversation_id: i32,
) -> Result<(), TaskStoreError> {
    let run_relation =
        delegation_workflow_run_binding::Entity::belongs_to(delegation_task_run::Entity)
            .from(delegation_workflow_run_binding::Column::TaskId)
            .to(delegation_task_run::Column::TaskId)
            .into();
    let conflict = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .filter(delegation_workflow_run_binding::Column::NodeId.ne(node_id.to_string()))
        .join(JoinType::InnerJoin, run_relation)
        .filter(delegation_task_run::Column::ChildConversationId.eq(child_conversation_id))
        .one(conn)
        .await
        .map_err(map_db)?;
    if let Some(binding) = conflict {
        return Err(admission_err(
            "reviewer_not_independent",
            format!(
                "child conversation {child_conversation_id} is already bound to workflow node {}",
                binding.node_id
            ),
        ));
    }
    Ok(())
}

pub(crate) async fn ensure_workflow_child_conversation_independent<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
    work_unit_key: Option<&str>,
    child_conversation_id: i32,
) -> Result<(), TaskStoreError> {
    let Some(key) = work_unit_key.map(str::trim).filter(|key| !key.is_empty()) else {
        return Ok(());
    };
    if parse_recognized_work_unit_key(key).is_none() {
        return Ok(());
    }
    let Some(header) = load_workflow_header(conn, parent_conversation_id).await? else {
        return Ok(());
    };
    let Some(binding) = find_node_binding(conn, &header.workflow_id, key).await? else {
        return Ok(());
    };
    ensure_child_conversation_independent(
        conn,
        &header.workflow_id,
        &binding.node_id,
        child_conversation_id,
    )
    .await
}

async fn resolve_task_route<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    binding: &delegation_workflow_node_binding::Model,
    parsed: &ParsedWorkUnitKey,
) -> Result<Option<(i64, Vec<String>)>, TaskStoreError> {
    let task_index = match parsed {
        ParsedWorkUnitKey::TaskImplementer { task_index, .. }
        | ParsedWorkUnitKey::TaskReviewer { task_index, .. } => *task_index,
        _ => return Ok(None),
    };
    let doc = load_active_manifest_doc(conn, header)
        .await?
        .ok_or_else(|| admission_err("task_route_mismatch", "active manifest is missing"))?;
    let normalized = validate_manifest_document(&doc).map_err(|err| {
        admission_err(
            "task_route_mismatch",
            format!("active manifest route is invalid: {err}"),
        )
    })?;
    let policy = normalized
        .task_policies
        .iter()
        .find(|policy| policy.task_index == task_index)
        .ok_or_else(|| {
            admission_err(
                "task_route_mismatch",
                format!("Task {task_index} has no active risk policy/route"),
            )
        })?;
    let mut route_node_ids = Vec::with_capacity(policy.route.reviewer_node_ids.len() + 1);
    route_node_ids.push(policy.route.implementer_node_id.clone());
    route_node_ids.extend(policy.route.reviewer_node_ids.iter().cloned());
    let admitted_on_route = match parsed {
        ParsedWorkUnitKey::TaskImplementer { .. } => {
            policy.route.implementer_node_id == binding.node_id
        }
        ParsedWorkUnitKey::TaskReviewer { .. } => {
            policy.route.reviewer_node_ids.contains(&binding.node_id)
        }
        _ => unreachable!("Task route parsed above"),
    };
    if !admitted_on_route {
        return Err(admission_err(
            "task_route_mismatch",
            format!(
                "node {} is not assigned to its role in Task {task_index} active route",
                binding.node_id
            ),
        ));
    }
    Ok(Some((task_index as i64, route_node_ids)))
}

async fn enforce_phase_readiness<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    binding: &delegation_workflow_node_binding::Model,
    parsed: &ParsedWorkUnitKey,
    kind: AdmissionDispatchKind,
    admission_class: &AdmissionClass,
) -> Result<(), TaskStoreError> {
    match parsed {
        ParsedWorkUnitKey::PlanAuthor { .. } => Ok(()),
        // Document reviewers: only published Design/Plan nodes (already bound).
        ParsedWorkUnitKey::Design { .. } | ParsedWorkUnitKey::PlanReviewer { .. } => Ok(()),

        ParsedWorkUnitKey::TaskImplementer { task_index, .. }
        | ParsedWorkUnitKey::TaskReviewer { task_index, .. } => {
            // A8.3: **new** Task first-dispatch blocked while plan re-open / not approved.
            if matches!(kind, AdmissionDispatchKind::FirstDispatch) {
                ensure_plan_approved_for_new_tasks(conn, header).await?;
                // Dependency: prior Task gates must pass for task_index > 1.
                ensure_prior_task_gates_pass(conn, header, *task_index).await?;
            }
            // Task reviewer first-dispatch: implementer need not be terminal yet
            // (B14 unstarted reviewer still admit-able after implementer start).
            let _ = binding;
            Ok(())
        }

        ParsedWorkUnitKey::FinalReviewer { .. } => {
            enforce_final_reviewer_readiness(conn, header, kind, admission_class).await
        }
        ParsedWorkUnitKey::FinalFixer { .. } => {
            enforce_final_fixer_readiness(conn, header, kind).await
        }
    }
}

/// A8.3: Plan gate must be approved (latest settlement Approved) for new Tasks.
async fn ensure_plan_approved_for_new_tasks<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<(), TaskStoreError> {
    // Blocked workflows reject new work.
    if header.workflow_state == WorkflowState::Blocked {
        let recovery = decide_workflow_recovery(
            &load_workflow_recovery_snapshot_conn(conn, header, None)
                .await
                .map_err(|error| {
                    TaskStoreError::Permanent(format!(
                        "load blocked workflow recovery projection: {error}"
                    ))
                })?,
        )
        .projection();
        let message = WorkflowAdmissionRecoveryError {
            message: "workflow is blocked; new Task admissions rejected".into(),
            recovery,
        }
        .encode()
        .map_err(|error| {
            TaskStoreError::Permanent(format!(
                "serialize blocked workflow recovery projection: {error}"
            ))
        })?;
        return Err(admission_err("workflow_blocked", message));
    }

    // Prefer durable Plan gate settlement over header state alone.
    let plan_gate_id = find_plan_gate_id(conn, header).await?;
    if let Some(gate_id) = plan_gate_id {
        let latest = delegation_workflow_gate_settlement::Entity::find()
            .filter(
                delegation_workflow_gate_settlement::Column::WorkflowId
                    .eq(header.workflow_id.clone()),
            )
            .filter(delegation_workflow_gate_settlement::Column::GateId.eq(gate_id.clone()))
            .order_by_desc(delegation_workflow_gate_settlement::Column::GateCycle)
            .one(conn)
            .await
            .map_err(map_db)?;
        match latest {
            Some(s) if s.outcome == GateSettlementOutcome::Approved => {
                // If header was demoted after that settlement (plan re-open),
                // supersedes_approved_revision is set and state is estimated → block.
                if header.workflow_state != WorkflowState::Approved {
                    return Err(admission_err(
                        "plan_gate_reopen",
                        "plan gate re-opened / not re-approved; new Task first-dispatch blocked (A8.3)",
                    ));
                }
                // Settlement is valid only when it covers the current plan fingerprint.
                // State-only estimated→approved republish keeps plan_fingerprint.
                if s.content_fingerprint != header.plan_fingerprint
                    || s.content_fingerprint.is_empty()
                {
                    return Err(admission_err(
                        "plan_gate_reopen",
                        "plan gate settlement content_fingerprint mismatch; re-approve required (A8)",
                    ));
                }
                return Ok(());
            }
            Some(_) => {
                return Err(admission_err(
                    "plan_gate_not_approved",
                    "plan gate latest settlement is not approved; new Task first-dispatch blocked (A8.3)",
                ));
            }
            None => {
                return Err(admission_err(
                    "plan_gate_not_approved",
                    "plan gate has no approved settlement; new Task first-dispatch blocked (A8.3)",
                ));
            }
        }
    }

    // No plan gate in active manifest: require Approved header state.
    if header.workflow_state != WorkflowState::Approved {
        return Err(admission_err(
            "plan_gate_not_approved",
            "workflow not approved; new Task first-dispatch blocked (A8.3)",
        ));
    }
    Ok(())
}

async fn find_plan_gate_id<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<Option<String>, TaskStoreError> {
    let Some(doc) = load_active_manifest_doc(conn, header).await? else {
        return Ok(None);
    };
    let normalized = validate_manifest_document(&doc).map_err(|e| {
        admission_err(
            "workflow_manifest_invalid",
            format!("active manifest invalid: {e}"),
        )
    })?;
    Ok(normalized
        .gates
        .iter()
        .find(|g| g.gate_kind == DocumentGateKind::Plan)
        .map(|g| g.id.clone()))
}

async fn ensure_prior_task_gates_pass<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    task_index: u32,
) -> Result<(), TaskStoreError> {
    if task_index <= 1 {
        return Ok(());
    }
    for prior in 1..task_index {
        let eval = evaluate_task_index_gate(conn, header, prior).await?;
        if !eval.passed {
            return Err(admission_err(
                "prior_task_gate_not_ready",
                format!(
                    "Task {task_index} admission requires Task {prior} execution gate to pass ({:?})",
                    eval.reason
                ),
            ));
        }
    }
    Ok(())
}

async fn enforce_final_reviewer_readiness<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    kind: AdmissionDispatchKind,
    admission_class: &AdmissionClass,
) -> Result<(), TaskStoreError> {
    // Final requires post-plan approved lifecycle (reject Blocked / skeleton /
    // estimated, including zero-task estimated).
    ensure_workflow_approved_for_final(header)?;

    // Final first-pass: all active Task gates must pass (B6).
    let task_indices = active_task_indices(conn, header).await?;
    for idx in &task_indices {
        let eval = evaluate_task_index_gate(conn, header, *idx).await?;
        if !eval.passed {
            return Err(admission_err(
                "final_early",
                format!(
                    "Final reviewer blocked: Task {idx} execution gate not passed ({:?})",
                    eval.reason
                ),
            ));
        }
    }

    // A6/B6: scoped Final **re-review** (normal_revision continue after
    // request_changes) requires Final fixer terminal pass. Unexpected-continue
    // / interruption recovery continues MUST be allowed without a fixer.
    if matches!(kind, AdmissionDispatchKind::ContinueOrReplacement)
        && matches!(admission_class, AdmissionClass::NormalRevision)
    {
        match evaluate_final_fixer_terminal_pass(conn, header).await? {
            Some(true) => {}
            Some(false) | None => {
                return Err(admission_err(
                    "final_rereview_before_fixer_pass",
                    "Final re-review blocked: Final fixer has not reached terminal pass for this cycle",
                ));
            }
        }
    }
    Ok(())
}

async fn enforce_final_fixer_readiness<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    _kind: AdmissionDispatchKind,
) -> Result<(), TaskStoreError> {
    ensure_workflow_approved_for_final(header)?;

    // B6: Final fixer only after Final reviewer terminal request_changes / block.
    // Failed/canceled alone does **not** open a fix cycle.
    let rev = load_latest_final_reviewer_evidence(conn, header).await?;
    let Some(mut rev) = rev else {
        return Err(admission_err(
            "final_fixer_before_non_pass",
            "Final fixer blocked: no Final reviewer terminal yet",
        ));
    };
    if !reviewer_is_request_changes_or_block(&rev) {
        // Defense in depth for already-settled runs that wrote a valid card
        // only into a report file (not chat). Re-harvest and persist once.
        if matches!(rev.status, TerminalRunStatus::Completed) && !rev.summary_validated {
            if let Some(repaired) =
                reharvest_final_reviewer_card_if_missing(conn, header, &rev).await?
            {
                rev = repaired;
            }
        }
    }
    if !reviewer_is_request_changes_or_block(&rev) {
        let detail = if matches!(rev.status, TerminalRunStatus::Completed) && !rev.summary_validated
        {
            "Final fixer blocked: Final reviewer completed without a validated request_changes/block card summary (chat or report harvest)"
        } else {
            "Final fixer blocked: Final reviewer has not terminal request_changes/block"
        };
        return Err(admission_err("final_fixer_before_non_pass", detail));
    }
    if header.completion_protocol_version == 2 {
        ensure_final_findings_package(conn, header).await?;
    }
    Ok(())
}

async fn ensure_final_findings_package<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<(), TaskStoreError> {
    let gate = delegation_workflow_gate_state::Entity::find_by_id((
        header.workflow_id.clone(),
        "final".to_string(),
    ))
    .one(conn)
    .await
    .map_err(map_db)?
    .ok_or_else(|| {
        admission_err(
            "completion_instruction_binding_failed",
            "Final Fixer requires a durable Final gate state",
        )
    })?;
    let package = load_active_final_findings_package_v1(
        conn,
        &header.workflow_id,
        &gate.gate_id,
        &gate.gate_lineage,
    )
    .await
    .map_err(map_final_findings_admission_error)?;
    if package.is_none() {
        return Err(admission_err(
            "completion_remediation_context_required",
            "Final Fixer requires one active current findings package",
        ));
    }
    Ok(())
}

fn map_final_findings_admission_error(error: FinalFindingsError) -> TaskStoreError {
    match error {
        FinalFindingsError::RemediationContextRequired => {
            admission_err("completion_remediation_context_required", error.to_string())
        }
        FinalFindingsError::EvidenceCorrupt => {
            admission_err("completion_evidence_corrupt", error.to_string())
        }
        FinalFindingsError::InvalidField(_)
        | FinalFindingsError::BoundsExceeded(_)
        | FinalFindingsError::Persistence(_) => {
            admission_err("completion_evidence_corrupt", error.to_string())
        }
    }
}

/// If Final reviewer completed without a validated chat card, try harvesting
/// from touched report paths and persist onto the run + run_binding.
async fn reharvest_final_reviewer_card_if_missing<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    rev: &ExecutionGateRunEvidence,
) -> Result<Option<ExecutionGateRunEvidence>, TaskStoreError> {
    if header.completion_protocol_version == 2 {
        return Ok(None);
    }
    let run = delegation_task_run::Entity::find_by_id(rev.task_id.clone())
        .one(conn)
        .await
        .map_err(map_db)?;
    let Some(run) = run else {
        return Ok(None);
    };
    if run
        .card_summary_json
        .as_deref()
        .and_then(parse_and_validate_summary_json)
        .is_some()
    {
        // Binding said unvalidated but JSON exists — re-apply settle validation.
        return finalize_reharvested_reviewer_summary(conn, header, &run, None).await;
    }

    let mut paths = Vec::new();
    if let Some(json) = run.touched_files_json.as_deref() {
        if let Ok(files) = serde_json::from_str::<Vec<DelegationTouchedFile>>(json) {
            for f in files {
                let lower = f.path.to_ascii_lowercase();
                if lower.ends_with(".md") || lower.ends_with(".markdown") {
                    paths.push(std::path::PathBuf::from(f.path));
                }
            }
        }
    }
    let workspace = run
        .workspace_path
        .as_deref()
        .map(std::path::Path::new)
        .map(|p| {
            // Strip Windows extended path prefix for join stability.
            let s = p.to_string_lossy();
            if let Some(rest) = s.strip_prefix(r"\\?\") {
                std::path::PathBuf::from(rest)
            } else {
                p.to_path_buf()
            }
        });
    let summary = extract_card_summary_with_report_fallback("", &paths, workspace.as_deref());
    let Some(summary) = summary else {
        return Ok(None);
    };
    finalize_reharvested_reviewer_summary(conn, header, &run, Some(summary)).await
}

async fn finalize_reharvested_reviewer_summary<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    run: &delegation_task_run::Model,
    harvested: Option<CardSummary>,
) -> Result<Option<ExecutionGateRunEvidence>, TaskStoreError> {
    let summary = match harvested {
        Some(s) => s,
        None => match run
            .card_summary_json
            .as_deref()
            .and_then(parse_and_validate_summary_json)
        {
            Some(s) => s,
            None => return Ok(None),
        },
    };
    // Only Review cards unlock Final fixer; ignore impl/author harvests.
    if !matches!(summary, CardSummary::Review { .. }) {
        return Ok(None);
    }

    let json = card_summary_to_json(&summary).map_err(|e| {
        admission_err(
            "card_summary_serialize",
            format!("failed to serialize reharvested card summary: {e}"),
        )
    })?;

    let mut run_am: delegation_task_run::ActiveModel = run.clone().into();
    run_am.card_summary_json = Set(Some(json));
    run_am.updated_at = Set(Utc::now());
    let run = run_am.update(conn).await.map_err(map_db)?;

    // Mirror on_terminal_settle_txn reviewer validation for Final (report optional).
    let rb = delegation_workflow_run_binding::Entity::find_by_id(run.task_id.clone())
        .one(conn)
        .await
        .map_err(map_db)?;
    let Some(rb) = rb else {
        return Ok(None);
    };
    let mut rb_am: delegation_workflow_run_binding::ActiveModel = rb.into();
    rb_am.summary_validated = Set(true);
    rb_am.updated_at = Set(Utc::now());
    let rb = rb_am.update(conn).await.map_err(map_db)?;

    // Bump graph so UI/projection see the repaired evidence.
    let _ = bump_graph_revision(conn, &header.workflow_id, Utc::now()).await?;

    Ok(Some(evidence_from_run_and_binding(&run, &rb)))
}

/// Design/Plan admission stamps the gate content fingerprint for evidence filtering.
fn document_gate_content_fingerprint(
    header: &delegation_workflow::Model,
    parsed: &ParsedWorkUnitKey,
) -> Option<String> {
    match parsed {
        ParsedWorkUnitKey::Design { .. } => Some(header.design_fingerprint.clone()),
        ParsedWorkUnitKey::PlanReviewer { .. } => Some(header.plan_fingerprint.clone()),
        _ => None,
    }
}

/// Final reviewer/fixer require `workflow_state == approved` (post-plan).
fn ensure_workflow_approved_for_final(
    header: &delegation_workflow::Model,
) -> Result<(), TaskStoreError> {
    if header.workflow_state == WorkflowState::Blocked {
        return Err(admission_err(
            "workflow_blocked",
            "workflow is blocked; Final admissions rejected",
        ));
    }
    if header.workflow_state != WorkflowState::Approved {
        return Err(admission_err(
            "final_before_plan_approved",
            "Final admissions require workflow_state=approved (post-plan); estimated/skeleton/zero-task pre-approval blocked",
        ));
    }
    Ok(())
}

/// B6: only completed + validated `request_changes` / `block` open a Final fix cycle.
fn reviewer_is_request_changes_or_block(ev: &ExecutionGateRunEvidence) -> bool {
    if ev.completion_protocol_version == 2 {
        return matches!(ev.status, TerminalRunStatus::Completed)
            && ev.completion_state
                == Some(crate::db::entities::delegation_task_run::CompletionState::Resolved)
            && ev.completion_evidence_validated
            && matches!(
                ev.completion_outcome,
                Some(CompletionOutcome::RequestChanges | CompletionOutcome::Block)
            );
    }
    matches!(ev.status, TerminalRunStatus::Completed)
        && ev.summary_validated
        && matches!(
            ev.review_verdict,
            Some(ReviewVerdict::RequestChanges) | Some(ReviewVerdict::Block)
        )
}

async fn evaluate_final_fixer_terminal_pass<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<Option<bool>, TaskStoreError> {
    let fixer = load_latest_role_evidence(conn, header, PHASE_FINAL, "fixer", None).await?;
    let Some(fixer) = fixer else {
        return Ok(None);
    };
    if fixer.completion_protocol_version == 2 {
        return Ok(Some(
            matches!(fixer.status, TerminalRunStatus::Completed)
                && fixer.completion_state
                    == Some(crate::db::entities::delegation_task_run::CompletionState::Resolved)
                && fixer.completion_evidence_validated
                && matches!(
                    fixer.completion_outcome,
                    Some(CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns)
                ),
        ));
    }
    let pass = matches!(fixer.status, TerminalRunStatus::Completed)
        && fixer.summary_validated
        && matches!(
            fixer.work_status,
            Some(WorkStatus::Done) | Some(WorkStatus::DoneWithConcerns)
        );
    Ok(Some(pass))
}

// ---------------------------------------------------------------------------
// Gate evaluation helpers (reuse Task 4 evaluate_execution_gate)
// ---------------------------------------------------------------------------

async fn evaluate_task_index_gate<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    task_index: u32,
) -> Result<super::gates::ExecutionGateEval, TaskStoreError> {
    let doc = load_active_manifest_doc(conn, header)
        .await?
        .ok_or_else(|| admission_err("workflow_manifest_missing", "active manifest missing"))?;
    let normalized = validate_manifest_document(&doc).map_err(|err| {
        admission_err(
            "workflow_manifest_invalid",
            format!("active manifest invalid: {err}"),
        )
    })?;
    let policy = normalized
        .task_policies
        .iter()
        .find(|policy| policy.task_index == task_index)
        .ok_or_else(|| {
            admission_err(
                "task_policy_missing",
                format!("Task {task_index} has no active policy route"),
            )
        })?;
    let impl_ev =
        load_latest_node_evidence(conn, header, &policy.route.implementer_node_id).await?;
    let mut required_reviewers = Vec::with_capacity(policy.route.reviewer_node_ids.len());
    for node_id in &policy.route.reviewer_node_ids {
        required_reviewers.push(RequiredReviewerEvidence {
            node_id: node_id.clone(),
            evidence: load_latest_node_evidence(conn, header, node_id).await?,
        });
    }
    Ok(evaluate_execution_gate(&ExecutionGateInput {
        kind: ExecutionGateKind::Task,
        implementer_or_fixer: impl_ev,
        required_reviewers,
        branch_tip_digest: None,
    }))
}

async fn active_task_indices<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<Vec<u32>, TaskStoreError> {
    let rows = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .filter(delegation_workflow_node_binding::Column::PhaseId.eq(PHASE_TASKS.to_string()))
        .filter(delegation_workflow_node_binding::Column::RetiredRevision.is_null())
        .all(conn)
        .await
        .map_err(map_db)?;
    let mut indices: Vec<u32> = rows
        .iter()
        .filter_map(|b| b.task_index.map(|i| i as u32))
        .collect();
    indices.sort_unstable();
    indices.dedup();
    Ok(indices)
}

async fn load_latest_final_reviewer_evidence<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<Option<ExecutionGateRunEvidence>, TaskStoreError> {
    load_latest_role_evidence(conn, header, PHASE_FINAL, "reviewer", None).await
}

/// Latest role evidence by `lineage_ordinal` for gate / readiness evaluation.
///
/// **Never fall back past a newer non-terminal (reserving/running) run.**
/// If the newest binding's run is non-terminal (or missing), return `None`
/// so callers treat the role as not ready (Final/fixers, Task gates).
async fn load_latest_role_evidence<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    phase: &str,
    role: &str,
    task_index: Option<i64>,
) -> Result<Option<ExecutionGateRunEvidence>, TaskStoreError> {
    let mut q = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .filter(delegation_workflow_node_binding::Column::PhaseId.eq(phase.to_string()))
        .filter(delegation_workflow_node_binding::Column::Role.eq(role.to_string()))
        .filter(delegation_workflow_node_binding::Column::RetiredRevision.is_null())
        .order_by_desc(delegation_workflow_node_binding::Column::IntroducedRevision)
        .order_by_asc(delegation_workflow_node_binding::Column::NodeId);
    if let Some(idx) = task_index {
        q = q.filter(delegation_workflow_node_binding::Column::TaskIndex.eq(idx));
    }
    let binding = q.one(conn).await.map_err(map_db)?;
    let Some(binding) = binding else {
        return Ok(None);
    };

    load_latest_node_evidence(conn, header, &binding.node_id).await
}

async fn load_latest_node_evidence<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    node_id: &str,
) -> Result<Option<ExecutionGateRunEvidence>, TaskStoreError> {
    let active = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .filter(delegation_workflow_node_binding::Column::NodeId.eq(node_id.to_string()))
        .filter(delegation_workflow_node_binding::Column::RetiredRevision.is_null())
        .one(conn)
        .await
        .map_err(map_db)?;
    if active.is_none() {
        return Ok(None);
    }

    let rbs = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(node_id.to_string()))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .all(conn)
        .await
        .map_err(map_db)?;

    let Some(rb) = rbs.into_iter().next() else {
        return Ok(None);
    };
    let run = delegation_task_run::Entity::find_by_id(rb.task_id.clone())
        .one(conn)
        .await
        .map_err(map_db)?;
    let Some(run) = run else {
        // Newest binding points at a missing run — not ready.
        return Ok(None);
    };
    if matches!(
        run.status,
        DelegationRunStatus::Reserving | DelegationRunStatus::Running
    ) {
        // Newer non-terminal blocks older terminals for gate readiness.
        return Ok(None);
    }
    if header.completion_protocol_version == 2 {
        let validated = load_validated_completion_evidence(conn, &run.task_id)
            .await
            .map_err(completion_evidence_admission_error)?;
        return Ok(Some(evidence_from_run_binding_and_validated(
            &run,
            &rb,
            2,
            Some(&validated),
        )));
    }
    Ok(Some(evidence_from_run_and_binding(&run, &rb)))
}

// ---------------------------------------------------------------------------
// Stamp digests / gate cycle (A2)
// ---------------------------------------------------------------------------

async fn stamp_admission_fields<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    binding: &delegation_workflow_node_binding::Model,
    parsed: &ParsedWorkUnitKey,
    workspace_path: Option<&str>,
) -> Result<
    (
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<String>,
    ),
    TaskStoreError,
> {
    match parsed {
        ParsedWorkUnitKey::PlanAuthor { .. } => Ok((None, None, None, None, None, None)),
        ParsedWorkUnitKey::Design { .. } => {
            let (gate_id, cycle, digest) =
                document_gate_stamp(conn, header, binding, parsed).await?;
            Ok((gate_id, cycle, digest, None, None, None))
        }
        ParsedWorkUnitKey::PlanReviewer { .. } => {
            let (gate_id, cycle, digest) =
                document_gate_stamp(conn, header, binding, parsed).await?;
            let plan_digest = digest
                .as_deref()
                .map(str::trim)
                .filter(|digest| !digest.is_empty())
                .ok_or_else(|| {
                    admission_err(
                        "plan_digest_missing",
                        "Plan reviewer admission requires a published Plan digest",
                    )
                })?;
            let (author_run, author_binding) =
                load_latest_plan_author_binding(conn, header).await?;
            if header.completion_protocol_version == 2 {
                let validated = load_validated_completion_evidence(conn, &author_run.task_id)
                    .await
                    .map_err(completion_evidence_admission_error)?;
                if !matches!(
                    validated.evidence.intent.outcome,
                    CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns
                ) || validated.evidence.artifact.digest() != plan_digest
                {
                    return Err(admission_err(
                        "plan_author_stale",
                        "latest active Plan Author evidence does not cover the exact current Plan",
                    ));
                }
                return Ok((
                    gate_id,
                    cycle,
                    Some(plan_digest.to_string()),
                    Some(author_run.task_id),
                    Some(author_run.generation),
                    None,
                ));
            }
            let author_digest = author_binding
                .artifact_digest
                .as_deref()
                .map(str::trim)
                .filter(|digest| !digest.is_empty());
            let author_summary_matches = author_run
                .card_summary_json
                .as_deref()
                .and_then(parse_and_validate_summary_json)
                .is_some_and(|summary| {
                    matches!(
                        summary,
                        CardSummary::Author { plan_digest: ref digest, .. }
                            if digest == plan_digest
                    )
                });
            if author_run.status != DelegationRunStatus::Completed
                || !author_binding.summary_validated
                || author_digest != Some(plan_digest)
                || !author_summary_matches
            {
                return Err(admission_err(
                    "plan_author_stale",
                    "latest active Plan Author must be completed and cover the exact Plan digest",
                ));
            }
            Ok((
                gate_id,
                cycle,
                Some(plan_digest.to_string()),
                Some(author_run.task_id),
                Some(author_run.generation),
                None,
            ))
        }
        ParsedWorkUnitKey::TaskReviewer { task_index, .. } => {
            let impl_pair =
                load_latest_implementer_binding(conn, header, *task_index as i64).await?;
            let Some((run, rb)) = impl_pair else {
                if header.completion_protocol_version == 2 {
                    return Err(artifact_admission_err(ArtifactError::Unavailable(
                        ArtifactFailure::ExpectedArtifactInvalid,
                    )));
                }
                return Err(admission_err(
                    "producer_artifact_missing",
                    format!("Task {task_index} has no implementer run to review"),
                ));
            };
            if header.completion_protocol_version == 2 {
                let validated = load_validated_completion_evidence(conn, &run.task_id)
                    .await
                    .map_err(completion_evidence_admission_error)?;
                if !matches!(
                    validated.evidence.intent.outcome,
                    CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns
                ) {
                    return Err(artifact_admission_err(ArtifactError::Unavailable(
                        ArtifactFailure::ExpectedArtifactInvalid,
                    )));
                }
                let digest = validated.evidence.artifact.digest().to_string();
                return Ok((
                    None,
                    None,
                    Some(digest),
                    Some(run.task_id),
                    Some(run.generation),
                    None,
                ));
            }
            let digest = rb
                .artifact_digest
                .as_deref()
                .map(str::trim)
                .filter(|digest| !digest.is_empty());
            let legacy_summary_valid = header.completion_protocol_version == 2
                || (rb.summary_validated
                    && run
                        .card_summary_json
                        .as_deref()
                        .and_then(parse_and_validate_summary_json)
                        .is_some_and(|summary| {
                            matches!(summary, CardSummary::Implementation { .. })
                        }));
            if run.status != DelegationRunStatus::Completed
                || digest.is_none()
                || !legacy_summary_valid
            {
                if header.completion_protocol_version == 2 {
                    return Err(artifact_admission_err(ArtifactError::Unavailable(
                        ArtifactFailure::ExpectedArtifactInvalid,
                    )));
                }
                return Err(admission_err(
                    "producer_artifact_missing",
                    format!(
                        "Task {task_index} reviewer requires the latest completed implementer task and non-empty artifact digest"
                    ),
                ));
            }
            if header.completion_protocol_version == 2 {
                revalidate_v2_reviewer_head(
                    workspace_path,
                    digest.expect("validated non-empty producer digest"),
                )
                .await?;
            }
            Ok((
                None,
                None,
                digest.map(str::to_string),
                Some(run.task_id),
                Some(run.generation),
                None,
            ))
        }
        ParsedWorkUnitKey::FinalReviewer { .. } => {
            // Prefer covering latest fixer if present; else first-pass:
            // stamp branch tip digest (same digest Final gate needs) or workspace HEAD.
            if let Some((run, rb)) = load_latest_fixer_binding(conn, header).await? {
                if header.completion_protocol_version == 2 {
                    let validated = load_validated_completion_evidence(conn, &run.task_id)
                        .await
                        .map_err(completion_evidence_admission_error)?;
                    if !matches!(
                        validated.evidence.intent.outcome,
                        CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns
                    ) {
                        return Err(artifact_admission_err(ArtifactError::Unavailable(
                            ArtifactFailure::ExpectedArtifactInvalid,
                        )));
                    }
                    return Ok((
                        Some("final".into()),
                        None,
                        Some(validated.evidence.artifact.digest().to_string()),
                        Some(run.task_id),
                        Some(run.generation),
                        None,
                    ));
                }
                Ok((
                    Some("final".into()),
                    None,
                    rb.artifact_digest,
                    Some(run.task_id),
                    Some(run.generation),
                    None,
                ))
            } else {
                let tip = if header.completion_protocol_version == 2 {
                    Some(resolve_v2_admission_head(workspace_path).await?)
                } else {
                    let durable_tip = derive_admission_branch_tip_digest(conn, header).await?;
                    durable_tip.or_else(|| workspace_head_commit(workspace_path))
                };
                Ok((Some("final".into()), None, tip, None, None, None))
            }
        }
        ParsedWorkUnitKey::TaskImplementer { .. } => {
            let baseline = if header.completion_protocol_version == 2 {
                Some(resolve_v2_admission_head(workspace_path).await?)
            } else {
                None
            };
            Ok((None, None, None, None, None, baseline))
        }
        ParsedWorkUnitKey::FinalFixer { .. } => {
            let baseline = if header.completion_protocol_version == 2 {
                Some(resolve_v2_admission_head(workspace_path).await?)
            } else {
                None
            };
            Ok((Some("final".into()), None, None, None, None, baseline))
        }
    }
}

/// Branch tip for Final first-pass admission: highest active Task implementer
/// completed digest (mirrors projection `derive_branch_tip_digest` index rules).
async fn derive_admission_branch_tip_digest<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<Option<String>, TaskStoreError> {
    let indices = active_task_indices(conn, header).await?;
    if indices.is_empty() {
        return Ok(None);
    }
    // Highest task_index first (same as projection tip selection).
    let mut sorted = indices;
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    for idx in sorted {
        let pair = load_latest_implementer_binding(conn, header, idx as i64).await?;
        let Some((run, rb)) = pair else {
            continue;
        };
        if run.status != DelegationRunStatus::Completed {
            continue;
        }
        if let Some(d) = rb
            .artifact_digest
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Ok(Some(d.to_string()));
        }
        // Winning index has empty digest → tip pending (no earlier fallback).
        return Ok(None);
    }
    Ok(None)
}

async fn document_gate_stamp<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    binding: &delegation_workflow_node_binding::Model,
    parsed: &ParsedWorkUnitKey,
) -> Result<(Option<String>, Option<i64>, Option<String>), TaskStoreError> {
    let Some(doc) = load_active_manifest_doc(conn, header).await? else {
        return Ok((None, None, None));
    };
    let normalized = match validate_manifest_document(&doc) {
        Ok(n) => n,
        Err(_) => return Ok((None, None, None)),
    };
    let kind = match parsed {
        ParsedWorkUnitKey::Design { .. } => DocumentGateKind::Design,
        ParsedWorkUnitKey::PlanReviewer { .. } => DocumentGateKind::Plan,
        _ => return Ok((None, None, None)),
    };
    let gate = normalized.gates.iter().find(|g| g.gate_kind == kind);
    let Some(gate) = gate else {
        return Ok((None, None, None));
    };
    // Only associate if this node is a required reviewer for the gate.
    if !gate.required_reviewer_node_ids.contains(&binding.node_id) {
        return Ok((None, None, None));
    }
    let latest = delegation_workflow_gate_settlement::Entity::find()
        .filter(
            delegation_workflow_gate_settlement::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .filter(delegation_workflow_gate_settlement::Column::GateId.eq(gate.id.clone()))
        .order_by_desc(delegation_workflow_gate_settlement::Column::GateCycle)
        .one(conn)
        .await
        .map_err(map_db)?;
    // A2: open cycle is 1 when none; otherwise max settled cycle + 1.
    let cycle = match latest {
        None => 1_i64,
        Some(s) => s.gate_cycle + 1,
    };
    let digest = match kind {
        DocumentGateKind::Design => normalized.design.as_ref().map(|d| d.digest.clone()),
        DocumentGateKind::Plan => normalized.plan.as_ref().map(|d| d.digest.clone()),
    };
    Ok((Some(gate.id.clone()), Some(cycle), digest))
}

async fn load_latest_plan_author_binding<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<
    (
        delegation_task_run::Model,
        delegation_workflow_run_binding::Model,
    ),
    TaskStoreError,
> {
    let author = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .filter(delegation_workflow_node_binding::Column::PhaseId.eq("plan"))
        .filter(delegation_workflow_node_binding::Column::Role.eq("author"))
        .filter(delegation_workflow_node_binding::Column::RetiredRevision.is_null())
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| {
            admission_err(
                "plan_author_missing",
                "Plan reviewer admission requires an active Plan Author",
            )
        })?;
    let binding = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(author.node_id))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .order_by_desc(delegation_workflow_run_binding::Column::CreatedAt)
        .order_by_desc(delegation_workflow_run_binding::Column::TaskId)
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| {
            admission_err(
                "plan_author_missing",
                "Plan reviewer admission requires a completed Plan Author run",
            )
        })?;
    let run = delegation_task_run::Entity::find_by_id(binding.task_id.clone())
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| {
            admission_err(
                "plan_author_missing",
                "latest Plan Author binding points to a missing run",
            )
        })?;
    Ok((run, binding))
}

async fn load_latest_implementer_binding<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    task_index: i64,
) -> Result<
    Option<(
        delegation_task_run::Model,
        delegation_workflow_run_binding::Model,
    )>,
    TaskStoreError,
> {
    load_latest_role_run_binding(
        conn,
        header,
        PHASE_TASKS,
        "implementer",
        Some(task_index),
        None,
    )
    .await
}

async fn load_latest_fixer_binding<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<
    Option<(
        delegation_task_run::Model,
        delegation_workflow_run_binding::Model,
    )>,
    TaskStoreError,
> {
    let gate_scope = if header.completion_protocol_version == 2 {
        let state = delegation_workflow_gate_state::Entity::find_by_id((
            header.workflow_id.clone(),
            "final".to_string(),
        ))
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| {
            admission_err(
                "completion_instruction_binding_failed",
                "Final Reviewer admission requires a durable current Final gate state",
            )
        })?;
        Some((state.gate_id, state.gate_lineage))
    } else {
        None
    };
    load_latest_role_run_binding(
        conn,
        header,
        PHASE_FINAL,
        "fixer",
        None,
        gate_scope
            .as_ref()
            .map(|(gate_id, lineage)| (gate_id.as_str(), lineage.as_str())),
    )
    .await
}

async fn load_latest_role_run_binding<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    phase: &str,
    role: &str,
    task_index: Option<i64>,
    gate_scope: Option<(&str, &str)>,
) -> Result<
    Option<(
        delegation_task_run::Model,
        delegation_workflow_run_binding::Model,
    )>,
    TaskStoreError,
> {
    let mut q = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .filter(delegation_workflow_node_binding::Column::PhaseId.eq(phase.to_string()))
        .filter(delegation_workflow_node_binding::Column::Role.eq(role.to_string()));
    if let Some(idx) = task_index {
        q = q.filter(delegation_workflow_node_binding::Column::TaskIndex.eq(idx));
    }
    let binding = q.one(conn).await.map_err(map_db)?;
    let Some(node) = binding else {
        return Ok(None);
    };
    let mut run_bindings = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(header.workflow_id.clone()))
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(node.node_id))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal);
    if let Some((gate_id, gate_lineage)) = gate_scope {
        run_bindings = run_bindings
            .filter(delegation_workflow_run_binding::Column::GateId.eq(gate_id))
            .filter(delegation_workflow_run_binding::Column::GateLineage.eq(gate_lineage));
    }
    let rbs = run_bindings.all(conn).await.map_err(map_db)?;
    for rb in rbs {
        if let Some(run) = delegation_task_run::Entity::find_by_id(rb.task_id.clone())
            .one(conn)
            .await
            .map_err(map_db)?
        {
            return Ok(Some((run, rb)));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------

async fn load_workflow_header<C: ConnectionTrait>(
    conn: &C,
    parent_conversation_id: i32,
) -> Result<Option<delegation_workflow::Model>, TaskStoreError> {
    delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::ParentConversationId.eq(parent_conversation_id))
        .filter(
            delegation_workflow::Column::WorkflowKind
                .eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.to_string()),
        )
        .one(conn)
        .await
        .map_err(map_db)
}

async fn find_node_binding<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    work_unit_key: &str,
) -> Result<Option<delegation_workflow_node_binding::Model>, TaskStoreError> {
    delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .filter(delegation_workflow_node_binding::Column::WorkUnitKey.eq(work_unit_key.to_string()))
        .one(conn)
        .await
        .map_err(map_db)
}

async fn load_run_binding<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<Option<delegation_workflow_run_binding::Model>, TaskStoreError> {
    delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(map_db)
}

async fn load_active_manifest_doc<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<Option<ManifestDocument>, TaskStoreError> {
    let rev = delegation_workflow_manifest_revision::Entity::find_by_id((
        header.workflow_id.clone(),
        header.active_manifest_revision,
    ))
    .one(conn)
    .await
    .map_err(map_db)?;
    let Some(rev) = rev else {
        return Ok(None);
    };
    match serde_json::from_str::<ManifestDocument>(&rev.document_json) {
        Ok(doc) => Ok(Some(doc)),
        Err(_) => Ok(None),
    }
}

async fn next_lineage_ordinal<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    lineage_root_task_id: &str,
) -> Result<i64, TaskStoreError> {
    // Monotonic per lineage_root: max ordinal among runs sharing lineage_root
    // that already have run_bindings under this workflow, else max for workflow + 1.
    let bound = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .all(conn)
        .await
        .map_err(map_db)?;

    let mut max_for_lineage: Option<i64> = None;
    let mut max_global: i64 = 0;
    for rb in &bound {
        max_global = max_global.max(rb.lineage_ordinal);
        let run = delegation_task_run::Entity::find_by_id(rb.task_id.clone())
            .one(conn)
            .await
            .map_err(map_db)?;
        if let Some(run) = run {
            if run.lineage_root_task_id == lineage_root_task_id {
                max_for_lineage = Some(max_for_lineage.unwrap_or(0).max(rb.lineage_ordinal));
            }
        }
    }
    Ok(match max_for_lineage {
        Some(m) => m + 1,
        None => max_global + 1,
    })
}

async fn mark_node_observed<C: ConnectionTrait>(
    conn: &C,
    binding: &delegation_workflow_node_binding::Model,
    now: chrono::DateTime<Utc>,
) -> Result<(), TaskStoreError> {
    let mut am: delegation_workflow_node_binding::ActiveModel = binding.clone().into();
    am.is_observed = Set(true);
    am.updated_at = Set(now);
    am.update(conn).await.map_err(map_db)?;
    Ok(())
}

async fn mark_observed_and_freeze_cohort<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    task_index: i64,
    route_node_ids: &[String],
) -> Result<(), TaskStoreError> {
    let route_nodes = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .filter(delegation_workflow_node_binding::Column::TaskIndex.eq(task_index))
        .filter(delegation_workflow_node_binding::Column::PhaseId.eq(PHASE_TASKS.to_string()))
        .filter(delegation_workflow_node_binding::Column::NodeId.is_in(route_node_ids.to_vec()))
        .all(conn)
        .await
        .map_err(map_db)?;
    if route_nodes.len() != route_node_ids.len() {
        return Err(admission_err(
            "task_route_mismatch",
            format!(
                "Task {task_index} active route has {} ids but {} durable nodes",
                route_node_ids.len(),
                route_nodes.len()
            ),
        ));
    }
    let now = Utc::now();
    for node in route_nodes {
        if node.cohort_frozen {
            continue;
        }
        let mut am: delegation_workflow_node_binding::ActiveModel = node.into();
        am.cohort_frozen = Set(true);
        am.updated_at = Set(now);
        am.update(conn).await.map_err(map_db)?;
    }
    Ok(())
}

pub(crate) async fn bump_graph_revision<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    now: chrono::DateTime<Utc>,
) -> Result<u64, TaskStoreError> {
    let header = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
        .one(conn)
        .await
        .map_err(map_db)?
        .ok_or_else(|| {
            admission_err(
                "workflow_not_found",
                format!("workflow {workflow_id} missing during revision bump"),
            )
        })?;
    let next = header.graph_revision + 1;
    let mut am: delegation_workflow::ActiveModel = header.into();
    am.graph_revision = Set(next);
    am.updated_at = Set(now);
    am.update(conn).await.map_err(map_db)?;
    Ok(next as u64)
}

/// Convenience: run admission side-effect emission after an outer transaction.
pub fn emit_workflow_side_effect(emitter: &EventEmitter, effect: &WorkflowTxnSideEffect) {
    effect.emit(emitter);
}

// ---------------------------------------------------------------------------
// Tests (B10)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_work_retries_only_expected_unique_constraint_codes() {
        assert!(completion_constraint_race_codes(19, 1555));
        assert!(completion_constraint_race_codes(19, 2067));
        assert!(!completion_constraint_race_codes(19, 787));
        assert!(!completion_constraint_race_codes(19, 1299));
        assert!(!completion_constraint_race_codes(19, 275));
    }
    use crate::acp::delegation::run_store::{Gen1AdmitOutcome, ReservingRunInsert, RunStore};
    use crate::acp::delegation::store::TerminalTaskWrite;
    use crate::acp::delegation::workflow::events::{
        WORKFLOW_GRAPH_CHANGED_EVENT, WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
    };
    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::store::{
        publish_workflow_manifest_core, PublishWorkflowRequest,
    };
    use crate::acp::delegation::workflow::types::{
        CompletionScopeRole, DocumentGateKind, DocumentRef, ManifestEdge, ManifestGate,
        ManifestNode, ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestTaskHardTrigger,
        ManifestTaskPolicy, ManifestTaskRisk, ManifestTaskRoute, ManifestWorkflowState,
        ResolutionMode, TaskHardTriggerKind, TaskRiskLevel, WorkUnitKeyParts,
        MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS,
        WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use crate::acp::delegation::workflow::WorkflowStoreError;
    use crate::db::entities::conversation::ConversationStatus;
    use crate::db::entities::delegation_task_run::{
        AdmissionClass as DbAdmissionClass, CompletionState,
    };
    use crate::db::entities::delegation_workflow::CompletionProtocolMode;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::agent::AgentType;
    use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};
    use sea_orm::{QueryOrder, Set, TransactionTrait};
    use std::path::Path;
    use std::process::Command;
    use std::sync::Arc;

    const ADMISSION_DESIGN_BYTES: &[u8] = b"# Design\n\nApproved behavior.\n";
    const ADMISSION_PLAN_BYTES: &[u8] =
        b"## Global Constraints\n\n- exact\n\n## Task 1: Build\n\nbody\n";

    fn emitter_with_rx() -> (
        EventEmitter,
        tokio::sync::broadcast::Receiver<crate::web::event_bridge::WebEvent>,
    ) {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let rx = broadcaster.subscribe();
        (EventEmitter::test_web_only(broadcaster), rx)
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

    fn sample_doc(token: &str, state: ManifestWorkflowState) -> ManifestDocument {
        let design_path = "docs/superpowers/specs/x.md";
        let plan_path = "docs/superpowers/plans/p.md";
        let design_key = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: design_path,
            agent_type: "codex",
            profile_id: None,
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
            workflow_state: state,
            design: Some(DocumentRef {
                rel_path: design_path.into(),
                digest: task9_sha256(ADMISSION_DESIGN_BYTES),
            }),
            plan: Some(DocumentRef {
                rel_path: plan_path.into(),
                digest: task9_sha256(ADMISSION_PLAN_BYTES),
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
                    "codex",
                    None,
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

    fn final_only_doc(token: &str) -> ManifestDocument {
        let mut doc = sample_doc(token, ManifestWorkflowState::Estimated);
        doc.nodes.retain(|node| node.task_index.is_none());
        for node in &mut doc.nodes {
            node.deps.retain(|dep| {
                dep != "task-1-impl" && dep != "task-1-rev" && dep != "plan-reviewer-1"
            });
        }
        doc.edges.clear();
        doc.task_policies.clear();
        doc
    }

    fn skeleton_doc(token: &str) -> ManifestDocument {
        let mut doc = sample_doc(token, ManifestWorkflowState::Skeleton);
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

    fn two_plan_reviewer_doc(token: &str) -> ManifestDocument {
        let mut doc = sample_doc(token, ManifestWorkflowState::Estimated);
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

    fn high_risk_doc(token: &str) -> ManifestDocument {
        let mut doc = sample_doc(token, ManifestWorkflowState::Estimated);
        let implementer = doc
            .nodes
            .iter_mut()
            .find(|node| node.id == "task-1-impl")
            .unwrap();
        implementer.agent_type = Some("codex".into());
        implementer.work_unit_key = Some(
            build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                task_index: 1,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap(),
        );
        doc.nodes.push(wu(
            "task-1-rev-grok",
            PHASE_TASKS,
            ManifestNodeRole::Reviewer,
            "grok",
            None,
            Some(1),
            build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
                task_index: 1,
                agent_type: "grok",
                profile_id: None,
            })
            .unwrap(),
            vec!["task-1-impl".into()],
        ));
        doc.task_policies[0] = ManifestTaskPolicy {
            task_index: 1,
            risk: ManifestTaskRisk {
                level: TaskRiskLevel::High,
                hard_triggers: vec![ManifestTaskHardTrigger {
                    kind: TaskHardTriggerKind::ConcurrencyLifecycle,
                    evidence: vec!["first admission and continuation ordering".into()],
                }],
                soft_signals: vec![],
                score: 0,
                reason: "concurrency lifecycle is a hard trigger".into(),
            },
            route: ManifestTaskRoute {
                implementer_node_id: "task-1-impl".into(),
                reviewer_node_ids: vec!["task-1-rev".into(), "task-1-rev-grok".into()],
            },
            allow_noop_verification: false,
        };
        doc
    }

    fn two_task_doc(token: &str) -> ManifestDocument {
        let mut doc = sample_doc(token, ManifestWorkflowState::Estimated);
        doc.nodes.push(wu(
            "task-2-impl",
            PHASE_TASKS,
            ManifestNodeRole::Implementer,
            "grok",
            None,
            Some(2),
            build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                task_index: 2,
                agent_type: "grok",
                profile_id: None,
            })
            .unwrap(),
            vec!["task-1-rev".into()],
        ));
        doc.nodes.push(wu(
            "task-2-rev",
            PHASE_TASKS,
            ManifestNodeRole::Reviewer,
            "codex",
            None,
            Some(2),
            build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
                task_index: 2,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap(),
            vec!["task-2-impl".into()],
        ));
        doc.task_policies.push(ManifestTaskPolicy {
            task_index: 2,
            risk: ManifestTaskRisk {
                level: TaskRiskLevel::Normal,
                hard_triggers: vec![],
                soft_signals: vec![],
                score: 0,
                reason: "second normal fixture".into(),
            },
            route: ManifestTaskRoute {
                implementer_node_id: "task-2-impl".into(),
                reviewer_node_ids: vec!["task-2-rev".into()],
            },
            allow_noop_verification: false,
        });
        doc
    }

    async fn seed_parent() -> (AppDatabase, i32) {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/wf-admit").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        (db, parent)
    }

    async fn publish_approved(
        db: &AppDatabase,
        emitter: &EventEmitter,
        parent: i32,
        token: &str,
    ) -> (String, u64) {
        let mut doc = sample_doc(token, ManifestWorkflowState::Estimated);
        let pub_r = publish_workflow_manifest_core(
            db,
            emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .expect("publish");

        // Design self-style: insert design reviewer terminal + settle design,
        // then plan reviewer + settle plan, then approve state.
        // Use settle after seeding document reviewers via store helpers would be
        // heavy; flip header to Approved via re-publish for unit focus after
        // plan settlement. For A8.3 we need real plan settlement.

        // Re-publish as approved only after we settle plan — helper settles both
        // gates with empty required when we inject settlements directly.
        seed_gate_settlement(
            db,
            &pub_r.workflow_id,
            "design",
            1,
            GateSettlementOutcome::Approved,
        )
        .await;
        seed_gate_settlement(
            db,
            &pub_r.workflow_id,
            "plan",
            1,
            GateSettlementOutcome::Approved,
        )
        .await;

        doc.workflow_id = Some(pub_r.workflow_id.clone());
        doc.expected_manifest_revision = Some(pub_r.manifest_revision);
        doc.workflow_state = ManifestWorkflowState::Approved;
        doc.publication_token = format!("{token}-upd");
        let pub2 = publish_workflow_manifest_core(
            db,
            emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("approve publish");
        seed_approved_plan_gate_state(db, &pub2.workflow_id).await;
        (pub2.workflow_id, pub2.graph_revision)
    }

    async fn publish_document_approved(
        db: &AppDatabase,
        emitter: &EventEmitter,
        parent: i32,
        mut doc: ManifestDocument,
    ) -> String {
        let published = publish_workflow_manifest_core(
            db,
            emitter,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .expect("publish estimated document");
        seed_gate_settlement(
            db,
            &published.workflow_id,
            "design",
            1,
            GateSettlementOutcome::Approved,
        )
        .await;
        seed_gate_settlement(
            db,
            &published.workflow_id,
            "plan",
            1,
            GateSettlementOutcome::Approved,
        )
        .await;
        doc.workflow_id = Some(published.workflow_id.clone());
        doc.expected_manifest_revision = Some(published.manifest_revision);
        doc.workflow_state = ManifestWorkflowState::Approved;
        doc.publication_token.push_str("-approved");
        let approved = publish_workflow_manifest_core(
            db,
            emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish approved document");
        seed_approved_plan_gate_state(db, &approved.workflow_id).await;
        approved.workflow_id
    }

    fn test_plan_gate_lineage() -> String {
        format!("sha256:{}", "9".repeat(64))
    }

    async fn seed_approved_plan_gate_state(db: &AppDatabase, workflow_id: &str) {
        use crate::db::entities::delegation_workflow_gate_state;

        let header = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let document = load_active_manifest_doc(&db.conn, &header)
            .await
            .unwrap()
            .unwrap();
        let manifest = validate_manifest_document(&document).unwrap();
        let gate = manifest
            .gates
            .iter()
            .find(|gate| gate.gate_kind == DocumentGateKind::Plan)
            .unwrap();
        delegation_workflow_gate_state::ActiveModel {
            workflow_id: Set(workflow_id.to_string()),
            gate_id: Set(gate.id.clone()),
            gate_lineage: Set(test_plan_gate_lineage()),
            current_review_round: Set(1),
            selected_node_ids_json: Set(
                serde_json::to_string(&gate.required_reviewer_node_ids).unwrap()
            ),
        }
        .insert(&db.conn)
        .await
        .unwrap();
    }

    async fn seed_gate_settlement(
        db: &AppDatabase,
        workflow_id: &str,
        gate_id: &str,
        cycle: i64,
        outcome: GateSettlementOutcome,
    ) {
        use sea_orm::ActiveModelTrait;
        let now = Utc::now();
        let header = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let content_fp = if gate_id == "plan" {
            header.plan_fingerprint.clone()
        } else {
            header.design_fingerprint.clone()
        };
        let covered_plan_digest = if gate_id == "plan" {
            load_active_manifest_doc(&db.conn, &header)
                .await
                .unwrap()
                .and_then(|document| document.plan.map(|plan| plan.digest))
        } else {
            None
        };
        let row = delegation_workflow_gate_settlement::ActiveModel {
            workflow_id: Set(workflow_id.to_string()),
            gate_id: Set(gate_id.to_string()),
            gate_cycle: Set(cycle),
            manifest_revision: Set(header.active_manifest_revision),
            structural_revision: Set(header.structural_revision),
            content_fingerprint: Set(content_fp),
            evidence_scope_digest: Set(
                (gate_id == "plan").then(|| format!("sha256:{}", "8".repeat(64)))
            ),
            gate_lineage: Set((gate_id == "plan").then(test_plan_gate_lineage)),
            review_round: Set((gate_id == "plan").then_some(cycle)),
            outcome: Set(outcome.clone()),
            critical_count: Set(Some(0)),
            important_count: Set(Some(0)),
            minor_count: Set(Some(0)),
            covered_plan_digest: Set(covered_plan_digest),
            summary: Set("ok".into()),
            graph_revision_at_settle: Set(header.graph_revision),
            created_at: Set(now),
            ..Default::default()
        };
        row.insert(&db.conn).await.expect("seed settlement");
        // Keep header approved when seeding plan approved.
        if gate_id == "plan" && matches!(outcome, GateSettlementOutcome::Approved) {
            let mut am: delegation_workflow::ActiveModel = header.into();
            am.workflow_state = Set(WorkflowState::Approved);
            am.updated_at = Set(now);
            am.update(&db.conn).await.unwrap();
        }
    }

    fn gen1_insert(
        parent: i32,
        child: i32,
        task_id: &str,
        agent: &str,
        key: Option<&str>,
        profile: Option<&str>,
    ) -> ReservingRunInsert {
        ReservingRunInsert {
            task_id: task_id.into(),
            root_task_id: task_id.into(),
            previous_task_id: None,
            generation: 1,
            parent_conversation_id: parent,
            parent_tool_use_id: Some(format!("tool-{task_id}")),
            child_conversation_id: child,
            agent_type: agent.into(),
            profile_id: profile.map(|s| s.into()),
            workspace_path: Some("/tmp/ws".into()),
            route_fingerprint: Some("rf".into()),
            launch_snapshot_version: Some("v1".into()),
            mode_id: None,
            config_values_json: Some("{}".into()),
            task_preview: Some("preview".into()),
            request_fingerprint: Some(format!("fp-{task_id}")),
            admission_class: DbAdmissionClass::NormalRevision,
            lineage_root_task_id: task_id.into(),
            work_unit_key: key.map(|s| s.into()),
            history_only: false,
            replaced_task_id: None,
            replacement_reason: None,
            started_at: Some(Utc::now()),
        }
    }

    async fn child_for(db: &AppDatabase, agent: AgentType) -> i32 {
        let folder = seed_folder(db, &format!("/tmp/child-{}", UuidLike::next())).await;
        seed_conversation(db, folder, agent).await
    }

    async fn admit_task9_bound_run(
        db: &AppDatabase,
        parent: i32,
        workspace: &Path,
        workflow_id: &str,
        node_id: &str,
        task_id: &str,
        agent: AgentType,
        agent_wire: &str,
    ) -> delegation_workflow_run_binding::Model {
        let node = delegation_workflow_node_binding::Entity::find_by_id((
            workflow_id.to_string(),
            node_id.to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let child = child_for(db, agent).await;
        let mut insert = gen1_insert(
            parent,
            child,
            task_id,
            agent_wire,
            Some(&node.work_unit_key),
            None,
        );
        insert.workspace_path = Some(workspace.to_string_lossy().into_owned());
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }));
        assert!(matches!(
            store.admit_gen1_reserving(insert).await.unwrap(),
            Gen1AdmitOutcome::Created(_)
        ));
        let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(binding.workflow_id, workflow_id);
        assert_eq!(binding.node_id, node_id);
        assert!(binding.instruction_block_digest.is_some());
        assert!(binding.evidence_scope_digest.is_some());
        let instruction = load_admitted_completion_instruction(&db.conn, task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            binding.instruction_block_digest.as_deref(),
            Some(instruction.digest.as_str())
        );
        assert_eq!(
            append_admitted_completion_instruction(&db.conn, task_id, "parent prose")
                .await
                .unwrap(),
            format!("parent prose\n{}", instruction.canonical_utf8)
        );
        binding
    }

    async fn complete_task9_admitted_run(
        db: &AppDatabase,
        task_id: &str,
        summary_json: &str,
        artifact_digest: Option<&str>,
    ) {
        let now = Utc::now();
        let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut run: delegation_task_run::ActiveModel = run.into();
        run.status = Set(DelegationRunStatus::Completed);
        run.reached_running_at = Set(Some(now));
        run.finished_at = Set(Some(now));
        run.card_summary_json = Set(Some(summary_json.to_string()));
        run.update(&db.conn).await.unwrap();

        let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut binding: delegation_workflow_run_binding::ActiveModel = binding.into();
        binding.summary_validated = Set(true);
        if let Some(digest) = artifact_digest {
            binding.artifact_digest = Set(Some(digest.to_string()));
        }
        binding.updated_at = Set(now);
        binding.update(&db.conn).await.unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_completed_bound_run(
        db: &AppDatabase,
        parent: i32,
        child: i32,
        workflow_id: &str,
        node_id: &str,
        task_id: &str,
        work_unit_key: &str,
        agent: &str,
        lineage_ordinal: i64,
        summary_json: &str,
        artifact_digest: Option<&str>,
        reviewed_task_id: Option<&str>,
        reviewed_generation: Option<i64>,
    ) {
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }));
        store
            .insert_reserving(gen1_insert(
                parent,
                child,
                task_id,
                agent,
                Some(work_unit_key),
                None,
            ))
            .await
            .expect("insert durable source run");
        let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let now = Utc::now();
        let mut run_am: delegation_task_run::ActiveModel = run.into();
        run_am.status = Set(DelegationRunStatus::Completed);
        run_am.reached_running_at = Set(Some(now));
        run_am.finished_at = Set(Some(now));
        run_am.card_summary_json = Set(Some(summary_json.to_string()));
        run_am.update(&db.conn).await.unwrap();

        let header = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        delegation_workflow_run_binding::ActiveModel {
            task_id: Set(task_id.to_string()),
            workflow_id: Set(workflow_id.to_string()),
            node_id: Set(node_id.to_string()),
            gate_id: Set(None),
            gate_cycle: Set(None),
            manifest_revision: Set(header.active_manifest_revision),
            content_fingerprint: Set(None),
            artifact_digest: Set(artifact_digest.map(str::to_string)),
            reviewed_task_id: Set(reviewed_task_id.map(str::to_string)),
            reviewed_implementer_generation: Set(reviewed_generation),
            lineage_ordinal: Set(lineage_ordinal),
            summary_validated: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("insert durable source binding");
    }

    fn author_summary(digest: &str) -> String {
        format!(
            r#"{{"kind":"author","status":"done","summary":"Plan authored","plan_digest":"{digest}","report_file":"reports/author.md"}}"#
        )
    }

    fn review_summary() -> &'static str {
        r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"approved","report_file":"reports/review.md"}"#
    }

    fn implementation_summary() -> &'static str {
        r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"implemented"}"#
    }

    /// Tiny uuid-like counter for unique paths in tests.
    struct UuidLike;
    impl UuidLike {
        fn next() -> String {
            use std::sync::atomic::{AtomicU64, Ordering};
            static C: AtomicU64 = AtomicU64::new(1);
            format!("{:x}", C.fetch_add(1, Ordering::SeqCst))
        }
    }

    struct AdmissionGitFixture {
        dir: tempfile::TempDir,
    }

    impl AdmissionGitFixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("temp admission repo");
            git_fixture_command(dir.path(), &["init", "--quiet"]);
            std::fs::write(dir.path().join("owned.txt"), b"baseline\n")
                .expect("write admission baseline");
            let design_path = dir.path().join("docs/superpowers/specs/x.md");
            let plan_path = dir.path().join("docs/superpowers/plans/p.md");
            std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
            std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
            std::fs::write(design_path, ADMISSION_DESIGN_BYTES).unwrap();
            std::fs::write(plan_path, ADMISSION_PLAN_BYTES).unwrap();
            git_fixture_command(dir.path(), &["add", "."]);
            git_fixture_command(
                dir.path(),
                &[
                    "-c",
                    "user.name=Codeg Test",
                    "-c",
                    "user.email=codeg@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "baseline",
                ],
            );
            Self { dir }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }

        fn head(&self) -> String {
            git_fixture_command(self.path(), &["rev-parse", "HEAD"])
        }

        fn commit_change(&self) {
            std::fs::write(self.path().join("owned.txt"), b"changed\n")
                .expect("write admission change");
            git_fixture_command(self.path(), &["add", "owned.txt"]);
            git_fixture_command(
                self.path(),
                &[
                    "-c",
                    "user.name=Codeg Test",
                    "-c",
                    "user.email=codeg@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "change",
                ],
            );
        }

        fn reset_hard(&self, head: &str) {
            git_fixture_command(self.path(), &["reset", "--hard", "--quiet", head]);
        }
    }

    fn git_fixture_command(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run admission git fixture command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git fixture output is UTF-8")
            .trim()
            .to_string()
    }

    async fn enable_completion_v2(db: &AppDatabase, workflow_id: &str) {
        let header = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
            .one(&db.conn)
            .await
            .expect("load workflow")
            .expect("workflow exists");
        let mut active: delegation_workflow::ActiveModel = header.into();
        active.completion_protocol_version = Set(2);
        active.completion_protocol_mode = Set(CompletionProtocolMode::V2Enforce);
        active.update(&db.conn).await.expect("enable completion v2");
    }

    fn task9_sha256(bytes: &[u8]) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    async fn task14_final_reviewer_fixture(
        token: &str,
    ) -> (AppDatabase, i32, AdmissionGitFixture, String, String) {
        use crate::db::entities::delegation_workflow_gate_state;

        let repo = AdmissionGitFixture::new();
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut document = final_only_doc(token);
        document.design.as_mut().unwrap().digest = task9_sha256(ADMISSION_DESIGN_BYTES);
        document.plan.as_mut().unwrap().digest = task9_sha256(ADMISSION_PLAN_BYTES);
        let workflow_id = publish_document_approved(&db, &emitter, parent, document).await;
        enable_completion_v2(&db, &workflow_id).await;

        delegation_workflow_gate_state::ActiveModel {
            workflow_id: Set(workflow_id.clone()),
            gate_id: Set("final".into()),
            gate_lineage: Set(format!("sha256:{}", "f".repeat(64))),
            current_review_round: Set(1),
            selected_node_ids_json: Set("[\"final-reviewer\"]".into()),
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let task_id = format!("{token}-final-reviewer");
        admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &workflow_id,
            "final-reviewer",
            &task_id,
            AgentType::Codex,
            "codex",
        )
        .await;
        complete_task9_admitted_run(
            &db,
            &task_id,
            r#"{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"changes required"}"#,
            None,
        )
        .await;
        (db, parent, repo, workflow_id, task_id)
    }

    #[tokio::test]
    async fn task14_final_completion_mints_immutable_package_before_fixer_admission() {
        let (db, parent, repo, workflow_id, task_id) =
            task14_final_reviewer_fixture("task14-final-package").await;
        let report_path = repo.path().join("reports/final.md");
        let report_bytes = b"# Verdict\n\nrequest changes\n\noriginal final report bytes\n";

        let txn = db.conn.begin().await.unwrap();
        let result = super::super::completion_evidence::materialize_terminal_completion_txn(
            &txn,
            super::super::completion_evidence::TerminalCompletionInput {
                task_id: task_id.clone(),
                terminal_status: DelegationRunStatus::Completed,
                final_assistant_text: "See the report.".into(),
                pre_read_reports: vec![
                    super::super::completion_evidence::ValidatedReportCandidate {
                        path: "reports/final.md".into(),
                        contents: String::from_utf8(report_bytes.to_vec()).unwrap(),
                        summary: Some("changes required".into()),
                    },
                ],
                pre_read_artifact: None,
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        assert_eq!(result.state, CompletionState::Resolved);

        let package = load_active_final_findings_package_v1(
            &db.conn,
            &workflow_id,
            "final",
            &format!("sha256:{}", "f".repeat(64)),
        )
        .await
        .unwrap()
        .expect("Final package must exist before Fixer admission");
        assert_eq!(package.context_bytes(0).unwrap(), report_bytes);
        let fixer_task_id = "task14-final-package-fixer";
        let fixer_binding = admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &workflow_id,
            "final-fixer",
            fixer_task_id,
            AgentType::Grok,
            "grok",
        )
        .await;
        assert_eq!(
            fixer_binding.final_findings_identity.as_deref(),
            Some(package.final_findings_identity())
        );
        let instruction = load_admitted_completion_instruction(&db.conn, fixer_task_id)
            .await
            .unwrap()
            .unwrap();
        assert!(instruction.canonical_utf8.contains(
            package.remediation_contexts[0]
                .content_base64
                .as_deref()
                .unwrap()
        ));
        std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
        std::fs::write(&report_path, b"mutated after terminal completion").unwrap();
        assert_eq!(package.context_bytes(0).unwrap(), report_bytes);
    }

    #[tokio::test]
    async fn task14_final_nonpass_without_context_opens_decision_without_package() {
        use crate::db::entities::delegation_attention_request::{self, AttentionKind};
        use crate::db::entities::delegation_completion_tool_intent;

        let (db, _parent, _repo, workflow_id, task_id) =
            task14_final_reviewer_fixture("task14-final-no-context").await;
        delegation_completion_tool_intent::ActiveModel {
            intent_id: Set(format!("intent-{task_id}")),
            task_id: Set(task_id.clone()),
            child_tool_call_id: Set(format!("call-{task_id}")),
            accepted_ordinal: Set(1),
            outcome: Set(CompletionOutcome::RequestChanges.as_str().into()),
            summary: Set(None),
            report_hint: Set(None),
            request_digest: Set("digest".into()),
            created_at: Set(Utc::now()),
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let txn = db.conn.begin().await.unwrap();
        let result = super::super::completion_evidence::materialize_terminal_completion_txn(
            &txn,
            super::super::completion_evidence::TerminalCompletionInput {
                task_id: task_id.clone(),
                terminal_status: DelegationRunStatus::Completed,
                final_assistant_text: String::new(),
                pre_read_reports: Vec::new(),
                pre_read_artifact: None,
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();

        assert_eq!(result.state, CompletionState::NeedsDecision);
        assert_eq!(
            result.attention.as_ref().unwrap().kind,
            AttentionKind::CompletionDecision
        );
        assert!(load_active_final_findings_package_v1(
            &db.conn,
            &workflow_id,
            "final",
            &format!("sha256:{}", "f".repeat(64)),
        )
        .await
        .unwrap()
        .is_none());
        let attention = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(task_id))
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert!(attention
            .payload_json
            .as_deref()
            .unwrap()
            .contains("completion_remediation_context_required"));
    }

    #[tokio::test]
    async fn task14_final_artifact_recovery_keeps_pre_read_snapshot() {
        let (db, _parent, repo, workflow_id, task_id) =
            task14_final_reviewer_fixture("task14-final-artifact-recovery").await;
        let report_path = repo.path().join("reports/final.md");
        let report_bytes = b"# Verdict\n\nrequest changes\n\nrecovery snapshot\n";
        std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
        std::fs::write(&report_path, report_bytes).unwrap();

        let txn = db.conn.begin().await.unwrap();
        let result = super::super::completion_evidence::materialize_terminal_completion_txn(
            &txn,
            super::super::completion_evidence::TerminalCompletionInput {
                task_id,
                terminal_status: DelegationRunStatus::Completed,
                final_assistant_text: "See the report.".into(),
                pre_read_reports: vec![
                    super::super::completion_evidence::ValidatedReportCandidate {
                        path: "reports/final.md".into(),
                        contents: String::from_utf8(report_bytes.to_vec()).unwrap(),
                        summary: Some("changes required".into()),
                    },
                ],
                pre_read_artifact: None,
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        assert_eq!(result.state, CompletionState::ArtifactRecovery);

        let package = load_active_final_findings_package_v1(
            &db.conn,
            &workflow_id,
            "final",
            &format!("sha256:{}", "f".repeat(64)),
        )
        .await
        .unwrap()
        .expect("artifact recovery must retain the immutable Final snapshot");
        assert_eq!(package.context_bytes(0).unwrap(), report_bytes);
    }

    #[tokio::test]
    async fn completion_v2_shared_validator_admission_ignores_legacy_card_projection() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "task12-admission-validator").await;
        enable_completion_v2(&db, &workflow_id).await;
        let repo = AdmissionGitFixture::new();
        let task_id = "12000000-0000-4000-8000-000000000001";
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let mut insert = gen1_insert(
            parent,
            child_for(&db, AgentType::Grok).await,
            task_id,
            "grok",
            Some(&key),
            None,
        );
        insert.workspace_path = Some(repo.path().display().to_string());
        RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .admit_gen1_reserving(insert)
        .await
        .unwrap();

        repo.commit_change();
        let now = Utc::now();
        let run = delegation_task_run::Entity::find_by_id(task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut run: delegation_task_run::ActiveModel = run.into();
        run.status = Set(DelegationRunStatus::Completed);
        run.reached_running_at = Set(Some(now));
        run.finished_at = Set(Some(now));
        run.update(&db.conn).await.unwrap();
        let txn = db.conn.begin().await.unwrap();
        super::super::completion_evidence::materialize_terminal_completion_txn(
            &txn,
            super::super::completion_evidence::TerminalCompletionInput {
                task_id: task_id.into(),
                terminal_status: DelegationRunStatus::Completed,
                final_assistant_text: "Conclusion: done\n\nTask complete.".into(),
                pre_read_reports: Vec::new(),
                pre_read_artifact: None,
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();

        let run = delegation_task_run::Entity::find_by_id(task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut run: delegation_task_run::ActiveModel = run.into();
        run.card_summary_json = Set(Some("{malformed".into()));
        run.update(&db.conn).await.unwrap();
        let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut binding: delegation_workflow_run_binding::ActiveModel = binding.into();
        binding.summary_validated = Set(false);
        binding.update(&db.conn).await.unwrap();

        let header = delegation_workflow::Entity::find_by_id(workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let evidence = load_latest_node_evidence(&db.conn, &header, "task-1-impl")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(evidence.completion_protocol_version, 2);
        assert_eq!(
            evidence.completion_state,
            Some(crate::db::entities::delegation_task_run::CompletionState::Resolved)
        );
        assert_eq!(evidence.completion_outcome, Some(CompletionOutcome::Done));
        assert!(evidence.completion_evidence_validated);
    }

    #[tokio::test]
    async fn all_role_instruction_scope_admission_derives_material_from_durable_sources() {
        use crate::db::entities::delegation_workflow_gate_state;

        const DESIGN_BYTES: &[u8] = b"# Design\n\nApproved behavior.\n";
        const PLAN_BYTES: &[u8] = b"## Global Constraints\n\n- exact\n\n## Task 1: Build\n\nbody\n";

        let repo = AdmissionGitFixture::new();
        let design_path = repo.path().join("docs/superpowers/specs/x.md");
        let plan_path = repo.path().join("docs/superpowers/plans/p.md");
        std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&design_path, DESIGN_BYTES).unwrap();
        std::fs::write(&plan_path, PLAN_BYTES).unwrap();

        let (db, parent) = seed_parent().await;
        let (emitter, _rx) = emitter_with_rx();
        let mut document = sample_doc("task9-scope", ManifestWorkflowState::Estimated);
        document.design.as_mut().unwrap().digest = task9_sha256(DESIGN_BYTES);
        document.plan.as_mut().unwrap().digest = task9_sha256(PLAN_BYTES);
        let published = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: document.clone(),
            },
        )
        .await
        .unwrap();
        enable_completion_v2(&db, &published.workflow_id).await;
        delegation_workflow_gate_state::ActiveModel {
            workflow_id: Set(published.workflow_id.clone()),
            gate_id: Set("design".into()),
            gate_lineage: Set(format!("sha256:{}", "d".repeat(64))),
            current_review_round: Set(1),
            selected_node_ids_json: Set("[\"design-reviewer-1\"]".into()),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        delegation_workflow_gate_state::ActiveModel {
            workflow_id: Set(published.workflow_id.clone()),
            gate_id: Set("plan".into()),
            gate_lineage: Set(format!("sha256:{}", "a".repeat(64))),
            current_review_round: Set(2),
            selected_node_ids_json: Set("[\"plan-reviewer-1\"]".into()),
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let design_task_id = "task9-design-reviewer";
        admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &published.workflow_id,
            "design-reviewer-1",
            design_task_id,
            AgentType::Codex,
            "codex",
        )
        .await;
        let author_task_id = "task9-plan-author";
        admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &published.workflow_id,
            "plan-author",
            author_task_id,
            AgentType::Codex,
            "codex",
        )
        .await;
        complete_task9_admitted_run(
            &db,
            author_task_id,
            &author_summary(document.plan.as_ref().unwrap().digest.as_str()),
            Some(document.plan.as_ref().unwrap().digest.as_str()),
        )
        .await;

        let task_id = "task9-plan-reviewer";
        let binding = admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &published.workflow_id,
            "plan-reviewer-1",
            task_id,
            AgentType::Codex,
            "codex",
        )
        .await;
        assert!(binding.material_selector_digest.is_some());
        assert!(binding.subject_material_digest.is_some());
        assert!(binding.instruction_block_digest.is_some());
        assert!(binding.evidence_scope_digest.is_some());
        assert_eq!(binding.review_round, Some(2));
        let instruction = load_admitted_completion_instruction(&db.conn, task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            binding.instruction_block_digest.as_deref(),
            Some(instruction.digest.as_str())
        );
        assert!(instruction.canonical_utf8.contains("task.1"));
        assert!(instruction.canonical_utf8.contains("request changes"));
        assert_eq!(
            append_admitted_completion_instruction(&db.conn, task_id, "parent prose")
                .await
                .unwrap(),
            format!("parent prose\n{}", instruction.canonical_utf8)
        );

        let task_node = delegation_workflow_node_binding::Entity::find_by_id((
            published.workflow_id.clone(),
            "task-1-impl".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let branch_tip = repo.head();
        let workflow_before_plan_approval =
            delegation_workflow::Entity::find_by_id(published.workflow_id.clone())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
        let context_store = WorkflowStore::new(&db.conn, repo.path());
        let missing_approval = build_admission_completion_context(
            &context_store,
            &AdmissionCandidate {
                workflow: &workflow_before_plan_approval,
                node: &task_node,
                task_id: "task9-unapproved-plan-probe",
                artifact_digest: None,
                reviewed_task_id: None,
                reviewed_generation: None,
                producer_baseline_head: Some(&branch_tip),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            missing_approval,
            EvidenceScopeError::PlanMaterialInvalid(_)
        ));

        seed_gate_settlement(
            &db,
            &published.workflow_id,
            "plan",
            1,
            GateSettlementOutcome::Approved,
        )
        .await;
        let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
            published.workflow_id.clone(),
            "plan".to_string(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut settlement: delegation_workflow_gate_settlement::ActiveModel = settlement.into();
        settlement.gate_lineage = Set(Some(format!("sha256:{}", "a".repeat(64))));
        settlement.review_round = Set(Some(2));
        settlement.evidence_scope_digest = Set(Some(format!("sha256:{}", "b".repeat(64))));
        settlement.covered_plan_digest = Set(None);
        settlement.update(&db.conn).await.unwrap();

        let workflow_without_covered_plan =
            delegation_workflow::Entity::find_by_id(published.workflow_id.clone())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
        let missing_covered_plan = build_admission_completion_context(
            &context_store,
            &AdmissionCandidate {
                workflow: &workflow_without_covered_plan,
                node: &task_node,
                task_id: "task9-uncovered-plan-probe",
                artifact_digest: None,
                reviewed_task_id: None,
                reviewed_generation: None,
                producer_baseline_head: Some(&branch_tip),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            missing_covered_plan,
            EvidenceScopeError::PlanMaterialInvalid(_)
        ));
        let settlement = delegation_workflow_gate_settlement::Entity::find_by_id((
            published.workflow_id.clone(),
            "plan".to_string(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut settlement: delegation_workflow_gate_settlement::ActiveModel = settlement.into();
        settlement.covered_plan_digest = Set(Some(document.plan.as_ref().unwrap().digest.clone()));
        settlement.update(&db.conn).await.unwrap();

        let task_implementer_task_id = "task9-task-implementer";
        admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &published.workflow_id,
            "task-1-impl",
            task_implementer_task_id,
            AgentType::Grok,
            "grok",
        )
        .await;
        complete_task9_admitted_run(
            &db,
            task_implementer_task_id,
            implementation_summary(),
            Some(&branch_tip),
        )
        .await;
        let task_reviewer_task_id = "task9-task-reviewer";
        admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &published.workflow_id,
            "task-1-rev",
            task_reviewer_task_id,
            AgentType::Codex,
            "codex",
        )
        .await;
        complete_task9_admitted_run(&db, task_reviewer_task_id, review_summary(), None).await;

        use crate::db::entities::delegation_final_findings_package::{
            self, FinalFindingsPackageStatus,
        };
        let final_lineage = format!("sha256:{}", "f".repeat(64));
        delegation_workflow_gate_state::ActiveModel {
            workflow_id: Set(published.workflow_id.clone()),
            gate_id: Set("final".into()),
            gate_lineage: Set(final_lineage.clone()),
            current_review_round: Set(1),
            selected_node_ids_json: Set("[\"final-reviewer\"]".into()),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        let final_package = super::super::final_findings::build_final_findings_package_v1(
            super::super::final_findings::FinalFindingsPackageInputV1 {
                workflow_id: published.workflow_id.clone(),
                gate_id: "final".into(),
                gate_lineage: final_lineage.clone(),
                graph_revision: 1,
                findings: vec![super::super::final_findings::FinalFindingInputV1 {
                    reviewer_node_id: "final-reviewer".into(),
                    evidence_task_id: "task9-final-source".into(),
                    evidence_scope_digest: format!("sha256:{}", "e".repeat(64)),
                    outcome: CompletionOutcome::RequestChanges,
                    target_work_unit_keys: vec!["task|1|implementer|grok|none".into()],
                    remediation_route_ids: vec!["task-1-impl".into()],
                }],
                remediation_contexts: vec![
                    super::super::final_findings::RemediationContextInputV1::available_terminal(
                        "task9-final-source",
                        b"Final changes required".to_vec(),
                    ),
                ],
            },
        )
        .unwrap();
        let encoded =
            super::super::final_findings::encode_final_findings_package_v1(&final_package).unwrap();
        delegation_final_findings_package::ActiveModel {
            package_id: Set("task9-final-package".into()),
            workflow_id: Set(published.workflow_id.clone()),
            gate_id: Set("final".into()),
            gate_lineage: Set(final_lineage.clone()),
            source_evaluation_key: Set(final_package.source_evaluation_key.clone()),
            source_evidence_task_ids_json: Set(encoded.source_evidence_task_ids_json),
            items_json: Set(encoded.items_json),
            remediation_contexts_json: Set(encoded.remediation_contexts_json),
            package_digest: Set(final_package.package_digest.clone()),
            status: Set(FinalFindingsPackageStatus::Active),
            created_graph_revision: Set(1),
            resolved_graph_revision: Set(None),
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let final_reviewer_task_id = "task9-final-reviewer";
        admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &published.workflow_id,
            "final-reviewer",
            final_reviewer_task_id,
            AgentType::Codex,
            "codex",
        )
        .await;
        complete_task9_admitted_run(
            &db,
            final_reviewer_task_id,
            r#"{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"changes required","report_file":"reports/final.md"}"#,
            None,
        )
        .await;
        let final_fixer_task_id = "task9-final-fixer";
        admit_task9_bound_run(
            &db,
            parent,
            repo.path(),
            &published.workflow_id,
            "final-fixer",
            final_fixer_task_id,
            AgentType::Grok,
            "grok",
        )
        .await;

        let cases = [
            (
                CompletionScopeRole::DesignReviewer,
                "design-reviewer-1",
                design_task_id,
            ),
            (
                CompletionScopeRole::PlanAuthor,
                "plan-author",
                author_task_id,
            ),
            (
                CompletionScopeRole::PlanReviewer,
                "plan-reviewer-1",
                task_id,
            ),
            (
                CompletionScopeRole::TaskImplementer,
                "task-1-impl",
                task_implementer_task_id,
            ),
            (
                CompletionScopeRole::TaskReviewer,
                "task-1-rev",
                task_reviewer_task_id,
            ),
            (
                CompletionScopeRole::FinalFixer,
                "final-fixer",
                final_fixer_task_id,
            ),
            (
                CompletionScopeRole::FinalReviewer,
                "final-reviewer",
                final_reviewer_task_id,
            ),
        ];
        let mut admitted_roles = Vec::new();
        for (expected_role, node_id, admitted_task_id) in cases {
            let binding =
                delegation_workflow_run_binding::Entity::find_by_id(admitted_task_id.to_string())
                    .one(&db.conn)
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(binding.node_id, node_id);
            assert!(binding.instruction_block_digest.is_some());
            assert!(binding.evidence_scope_digest.is_some());
            let instruction = load_admitted_completion_instruction(&db.conn, admitted_task_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                append_admitted_completion_instruction(&db.conn, admitted_task_id, "parent prose")
                    .await
                    .unwrap(),
                format!("parent prose\n{}", instruction.canonical_utf8)
            );
            if matches!(
                expected_role,
                CompletionScopeRole::FinalFixer | CompletionScopeRole::FinalReviewer
            ) {
                assert_eq!(binding.gate_id.as_deref(), Some("final"));
                assert_eq!(
                    binding.gate_lineage.as_deref(),
                    Some(final_lineage.as_str())
                );
                assert_eq!(
                    binding.review_round,
                    (expected_role == CompletionScopeRole::FinalReviewer).then_some(1_i64)
                );
            }
            admitted_roles.push(expected_role);
        }
        assert_eq!(
            admitted_roles,
            [
                CompletionScopeRole::DesignReviewer,
                CompletionScopeRole::PlanAuthor,
                CompletionScopeRole::PlanReviewer,
                CompletionScopeRole::TaskImplementer,
                CompletionScopeRole::TaskReviewer,
                CompletionScopeRole::FinalFixer,
                CompletionScopeRole::FinalReviewer,
            ]
        );

        repo.commit_change();
        let fixer_head = repo.head();
        complete_task9_admitted_run(
            &db,
            final_fixer_task_id,
            implementation_summary(),
            Some(&fixer_head),
        )
        .await;
        let final_state = delegation_workflow_gate_state::Entity::find_by_id((
            published.workflow_id.clone(),
            "final".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut final_state: delegation_workflow_gate_state::ActiveModel = final_state.into();
        final_state.gate_lineage = Set(format!("sha256:{}", "1".repeat(64)));
        final_state.update(&db.conn).await.unwrap();

        let final_reviewer = delegation_workflow_node_binding::Entity::find_by_id((
            published.workflow_id.clone(),
            "final-reviewer".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let final_reviewer_key =
            parse_recognized_work_unit_key(&final_reviewer.work_unit_key).unwrap();
        let workflow = delegation_workflow::Entity::find_by_id(published.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let (gate_id, _, artifact_digest, reviewed_task_id, reviewed_generation, _) =
            stamp_admission_fields(
                &db.conn,
                &workflow,
                &final_reviewer,
                &final_reviewer_key,
                repo.path().to_str(),
            )
            .await
            .unwrap();
        assert_eq!(gate_id.as_deref(), Some("final"));
        assert_eq!(artifact_digest.as_deref(), Some(fixer_head.as_str()));
        assert_eq!(reviewed_task_id, None);
        assert_eq!(reviewed_generation, None);

        delegation_workflow_gate_state::Entity::delete_by_id((
            published.workflow_id.clone(),
            "final".to_string(),
        ))
        .exec(&db.conn)
        .await
        .unwrap();
        let missing_state = load_latest_fixer_binding(&db.conn, &workflow)
            .await
            .unwrap_err();
        assert_eq!(
            missing_state.workflow_admission_code(),
            Some("completion_instruction_binding_failed")
        );
    }

    #[tokio::test]
    async fn all_role_instruction_scope_admission_rejects_malformed_durable_plan_before_binding() {
        use crate::db::entities::delegation_workflow_gate_state;

        const DESIGN_BYTES: &[u8] = b"# Design\n\nApproved behavior.\n";
        const MALFORMED_PLAN_BYTES: &[u8] =
            b"## Task 1: First\n\nbody\n\n## Task 1: Duplicate\n\nother\n";

        let repo = AdmissionGitFixture::new();
        let design_path = repo.path().join("docs/superpowers/specs/x.md");
        let plan_path = repo.path().join("docs/superpowers/plans/p.md");
        std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&design_path, DESIGN_BYTES).unwrap();
        std::fs::write(&plan_path, MALFORMED_PLAN_BYTES).unwrap();

        let (db, parent) = seed_parent().await;
        let (emitter, _rx) = emitter_with_rx();
        let mut document = sample_doc("task9-malformed-plan", ManifestWorkflowState::Estimated);
        document.design.as_mut().unwrap().digest = task9_sha256(DESIGN_BYTES);
        document.plan.as_mut().unwrap().digest = task9_sha256(MALFORMED_PLAN_BYTES);
        let published = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: document.clone(),
            },
        )
        .await
        .unwrap();
        enable_completion_v2(&db, &published.workflow_id).await;
        delegation_workflow_gate_state::ActiveModel {
            workflow_id: Set(published.workflow_id.clone()),
            gate_id: Set("plan".into()),
            gate_lineage: Set(format!("sha256:{}", "a".repeat(64))),
            current_review_round: Set(1),
            selected_node_ids_json: Set("[\"plan-reviewer-1\"]".into()),
        }
        .insert(&db.conn)
        .await
        .unwrap();

        let author_node = delegation_workflow_node_binding::Entity::find_by_id((
            published.workflow_id.clone(),
            "plan-author".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let author_child = child_for(&db, AgentType::Codex).await;
        seed_completed_bound_run(
            &db,
            parent,
            author_child,
            &published.workflow_id,
            "plan-author",
            "task9-malformed-author",
            &author_node.work_unit_key,
            "codex",
            1,
            &author_summary(document.plan.as_ref().unwrap().digest.as_str()),
            Some(document.plan.as_ref().unwrap().digest.as_str()),
            None,
            None,
        )
        .await;

        let reviewer_node = delegation_workflow_node_binding::Entity::find_by_id((
            published.workflow_id.clone(),
            "plan-reviewer-1".to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let reviewer_child = child_for(&db, AgentType::Codex).await;
        let task_id = "task9-malformed-reviewer";
        let mut insert = gen1_insert(
            parent,
            reviewer_child,
            task_id,
            "codex",
            Some(&reviewer_node.work_unit_key),
            None,
        );
        insert.workspace_path = Some(repo.path().to_string_lossy().into_owned());
        let run_store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }));
        let error = run_store.admit_gen1_reserving(insert).await.unwrap_err();
        assert!(matches!(
            error,
            TaskStoreError::WorkflowAdmission { ref code, .. }
                if code == "completion_plan_material_invalid"
        ));
        assert!(
            delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
                .one(&db.conn)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn completion_artifact_contract_producer_admission_is_clean_and_persists_baseline() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "tok-task7-producer-baseline").await;
        enable_completion_v2(&db, &workflow_id).await;
        let repo = AdmissionGitFixture::new();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();

        std::fs::write(repo.path().join("untracked.txt"), b"dirty\n")
            .expect("dirty producer workspace");
        let dirty_task = "70000000-0000-4000-8000-000000000001";
        let mut dirty = gen1_insert(
            parent,
            child_for(&db, AgentType::Grok).await,
            dirty_task,
            "grok",
            Some(&key),
            None,
        );
        dirty.workspace_path = Some(repo.path().display().to_string());
        let error = store
            .admit_gen1_reserving(dirty)
            .await
            .expect_err("dirty v2 producer admission must fail");
        assert_eq!(
            error.workflow_admission_code(),
            Some("completion_artifact_unavailable")
        );
        assert!(
            delegation_task_run::Entity::find_by_id(dirty_task)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none(),
            "failed admission must roll back the reserving row"
        );

        std::fs::remove_file(repo.path().join("untracked.txt")).expect("restore clean repo");
        let clean_task = "70000000-0000-4000-8000-000000000002";
        let mut clean = gen1_insert(
            parent,
            child_for(&db, AgentType::Grok).await,
            clean_task,
            "grok",
            Some(&key),
            None,
        );
        clean.workspace_path = Some(repo.path().display().to_string());
        store
            .admit_gen1_reserving(clean)
            .await
            .expect("clean producer admission");
        let binding = delegation_workflow_run_binding::Entity::find_by_id(clean_task)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let expected_head = repo.head();
        assert_eq!(
            binding.producer_baseline_head.as_deref(),
            Some(expected_head.as_str())
        );
    }

    #[tokio::test]
    async fn completion_artifact_contract_task_reviewer_rejects_commit_drift_before_binding() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "tok-task7-reviewer-drift").await;
        enable_completion_v2(&db, &workflow_id).await;
        let repo = AdmissionGitFixture::new();
        let producer_head = repo.head();
        let implementer_task = "70000000-0000-4000-8000-000000000003";
        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            child_for(&db, AgentType::Grok).await,
            &workflow_id,
            "task-1-impl",
            implementer_task,
            &implementer_key,
            "grok",
            1,
            implementation_summary(),
            Some(&producer_head),
            None,
            None,
        )
        .await;
        repo.commit_change();

        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let reviewer_task = "70000000-0000-4000-8000-000000000004";
        let mut reviewer = gen1_insert(
            parent,
            child_for(&db, AgentType::Codex).await,
            reviewer_task,
            "codex",
            Some(&reviewer_key),
            None,
        );
        reviewer.workspace_path = Some(repo.path().display().to_string());
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let error = store
            .admit_gen1_reserving(reviewer)
            .await
            .expect_err("reviewer must reject a different clean commit");
        assert_eq!(
            error.workflow_admission_code(),
            Some("completion_scope_changed")
        );
        assert!(
            delegation_workflow_run_binding::Entity::find_by_id(reviewer_task)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn completion_artifact_contract_final_reviewer_binds_only_delivered_head() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "tok-task7-final-drift").await;
        enable_completion_v2(&db, &workflow_id).await;
        let final_lineage = format!("sha256:{}", "7".repeat(64));
        delegation_workflow_gate_state::ActiveModel {
            workflow_id: Set(workflow_id.clone()),
            gate_id: Set("final".into()),
            gate_lineage: Set(final_lineage.clone()),
            current_review_round: Set(1),
            selected_node_ids_json: Set("[\"final-reviewer\"]".into()),
        }
        .insert(&db.conn)
        .await
        .unwrap();
        let repo = AdmissionGitFixture::new();
        let producer_head = repo.head();
        let implementer_task = "70000000-0000-4000-8000-000000000005";
        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            child_for(&db, AgentType::Grok).await,
            &workflow_id,
            "task-1-impl",
            implementer_task,
            &implementer_key,
            "grok",
            1,
            implementation_summary(),
            Some(&producer_head),
            None,
            None,
        )
        .await;
        let task_reviewer = "70000000-0000-4000-8000-000000000006";
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            child_for(&db, AgentType::Codex).await,
            &workflow_id,
            "task-1-rev",
            task_reviewer,
            &reviewer_key,
            "codex",
            2,
            review_summary(),
            Some(&producer_head),
            Some(implementer_task),
            Some(1),
        )
        .await;
        repo.commit_change();
        let delivered_head = repo.head();

        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let mut final_reviewer = gen1_insert(
            parent,
            child_for(&db, AgentType::Codex).await,
            "70000000-0000-4000-8000-000000000007",
            "codex",
            Some(&final_key),
            None,
        );
        final_reviewer.workspace_path = Some(repo.path().display().to_string());
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let outcome = store
            .admit_gen1_reserving(final_reviewer)
            .await
            .expect("Final reviewer must bind the clean post-aggregation HEAD");
        assert!(matches!(outcome, Gen1AdmitOutcome::Created(_)));
        let binding = delegation_workflow_run_binding::Entity::find_by_id(
            "70000000-0000-4000-8000-000000000007".to_string(),
        )
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            binding.artifact_digest.as_deref(),
            Some(delivered_head.as_str())
        );
        assert_eq!(binding.reviewed_task_id, None);
        assert_eq!(binding.reviewed_implementer_generation, None);
        assert_eq!(binding.gate_id.as_deref(), Some("final"));
        assert_eq!(
            binding.gate_lineage.as_deref(),
            Some(final_lineage.as_str())
        );
        assert_eq!(binding.review_round, Some(1));
    }

    #[tokio::test]
    async fn completion_artifact_contract_terminal_producer_uses_durable_noop_policy() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut document = sample_doc(
            "tok-task7-terminal-producer",
            ManifestWorkflowState::Estimated,
        );
        document.task_policies[0].allow_noop_verification = true;
        let workflow_id = publish_document_approved(&db, &emitter, parent, document).await;
        enable_completion_v2(&db, &workflow_id).await;
        let repo = AdmissionGitFixture::new();
        let task_id = "70000000-0000-4000-8000-000000000010";
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let mut producer = gen1_insert(
            parent,
            child_for(&db, AgentType::Grok).await,
            task_id,
            "grok",
            Some(&key),
            None,
        );
        producer.workspace_path = Some(repo.path().display().to_string());
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        store
            .admit_gen1_reserving(producer)
            .await
            .expect("admit no-op producer");

        let artifact = store
            .resolve_and_stamp_workflow_terminal_artifact(task_id, CompletionOutcome::Done)
            .await
            .expect("durable Task policy authorizes no-op")
            .expect("passing producer has artifact");
        assert_eq!(artifact.digest(), repo.head());
        let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            binding.artifact_digest.as_deref(),
            Some(repo.head().as_str())
        );

        std::fs::write(repo.path().join("untracked.txt"), b"non-pass dirt\n")
            .expect("dirty non-pass workspace");
        let producer_run = delegation_task_run::Entity::find_by_id(task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut producer_run_am: delegation_task_run::ActiveModel = producer_run.into();
        producer_run_am.workspace_path = Set(None);
        producer_run_am.update(&db.conn).await.unwrap();
        assert_eq!(
            store
                .resolve_and_stamp_workflow_terminal_artifact(task_id, CompletionOutcome::Blocked,)
                .await
                .expect("non-pass bypasses artifact resolution"),
            None
        );
        let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(binding.artifact_digest, None);
    }

    #[tokio::test]
    async fn completion_artifact_contract_terminal_reviewer_revalidates_clean_bound_head() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "tok-task7-terminal-reviewer").await;
        enable_completion_v2(&db, &workflow_id).await;
        let repo = AdmissionGitFixture::new();
        let producer_head = repo.head();
        let implementer_task = "70000000-0000-4000-8000-000000000011";
        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            child_for(&db, AgentType::Grok).await,
            &workflow_id,
            "task-1-impl",
            implementer_task,
            &implementer_key,
            "grok",
            1,
            implementation_summary(),
            Some(&producer_head),
            None,
            None,
        )
        .await;
        let producer_run = delegation_task_run::Entity::find_by_id(implementer_task)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut producer_run_am: delegation_task_run::ActiveModel = producer_run.into();
        producer_run_am.card_summary_json = Set(None);
        producer_run_am.update(&db.conn).await.unwrap();
        let producer_binding =
            delegation_workflow_run_binding::Entity::find_by_id(implementer_task)
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
        let mut producer_binding_am: delegation_workflow_run_binding::ActiveModel =
            producer_binding.into();
        producer_binding_am.summary_validated = Set(false);
        producer_binding_am.update(&db.conn).await.unwrap();

        let reviewer_task = "70000000-0000-4000-8000-000000000012";
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let mut reviewer = gen1_insert(
            parent,
            child_for(&db, AgentType::Codex).await,
            reviewer_task,
            "codex",
            Some(&reviewer_key),
            None,
        );
        reviewer.workspace_path = Some(repo.path().display().to_string());
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        store
            .admit_gen1_reserving(reviewer)
            .await
            .expect("reviewer admits on producer commit");

        repo.commit_change();
        let drift = store
            .resolve_and_stamp_workflow_terminal_artifact(reviewer_task, CompletionOutcome::Approve)
            .await
            .expect_err("terminal commit drift must fail");
        assert_eq!(
            drift.workflow_admission_code(),
            Some("completion_scope_changed")
        );

        repo.reset_hard(&producer_head);
        std::fs::write(repo.path().join("untracked.txt"), b"dirty\n")
            .expect("dirty reviewer workspace");
        let dirty = store
            .resolve_and_stamp_workflow_terminal_artifact(
                reviewer_task,
                CompletionOutcome::RequestChanges,
            )
            .await
            .expect_err("terminal dirt must fail");
        assert_eq!(
            dirty.workflow_admission_code(),
            Some("completion_artifact_unavailable")
        );

        std::fs::remove_file(repo.path().join("untracked.txt")).expect("restore clean repo");
        let artifact = store
            .resolve_and_stamp_workflow_terminal_artifact(
                reviewer_task,
                CompletionOutcome::ApproveWithMinors,
            )
            .await
            .expect("clean terminal reviewer revalidation")
            .expect("code reviewer returns bound artifact");
        assert_eq!(artifact.digest(), producer_head);
    }

    #[tokio::test]
    async fn task5_plan_author_admits_on_skeleton_before_plan_digest_exists() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: skeleton_doc("tok-task5-author-skeleton"),
            },
        )
        .await
        .expect("publish skeleton");
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Codex).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();

        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "50000000-0000-4000-8000-000000000001",
                "codex",
                Some(&key),
                None,
            ))
            .await
            .expect("Plan Author must admit before a Plan digest exists");
    }

    #[tokio::test]
    async fn task5_plan_author_rejects_non_codex_identity() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: skeleton_doc("tok-task5-author-agent"),
            },
        )
        .await
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "50000000-0000-4000-8000-000000000002",
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect_err("Plan Author identity is Codex-only");
        assert_eq!(
            err.workflow_admission_code(),
            Some("workflow_agent_mismatch")
        );
    }

    #[tokio::test]
    async fn task5_plan_author_continuation_reuses_its_own_conversation() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: skeleton_doc("tok-task5-author-continue"),
            },
        )
        .await
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Codex).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let first_task = "50000000-0000-4000-8000-000000000027";
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                first_task,
                "codex",
                Some(&key),
                None,
            ))
            .await
            .expect("first Author admission");
        let now = Utc::now();
        let run = delegation_task_run::Entity::find_by_id(first_task)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut run_am: delegation_task_run::ActiveModel = run.into();
        run_am.status = Set(DelegationRunStatus::Completed);
        run_am.reached_running_at = Set(Some(now));
        run_am.finished_at = Set(Some(now));
        run_am.card_summary_json = Set(Some(author_summary("sha256:first")));
        run_am.update(&db.conn).await.unwrap();
        let rb = delegation_workflow_run_binding::Entity::find_by_id(first_task)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut rb_am: delegation_workflow_run_binding::ActiveModel = rb.into();
        rb_am.summary_validated = Set(true);
        rb_am.artifact_digest = Set(Some("sha256:first".into()));
        rb_am.update(&db.conn).await.unwrap();

        let next_task = "50000000-0000-4000-8000-000000000028";
        let mut next = gen1_insert(parent, child, next_task, "codex", Some(&key), None);
        next.root_task_id = first_task.into();
        next.previous_task_id = Some(first_task.into());
        next.generation = 2;
        next.lineage_root_task_id = first_task.into();
        next.parent_tool_use_id = Some("tool-author-continuation".into());
        store.insert_reserving(next).await.unwrap();
        admit_workflow_run_txn(
            &db.conn,
            &WorkflowAdmitInput {
                parent_conversation_id: parent,
                child_conversation_id: child,
                task_id: next_task,
                work_unit_key: Some(&key),
                agent_type: "codex",
                profile_id: None,
                lineage_root_task_id: first_task,
                generation: 2,
                kind: AdmissionDispatchKind::ContinueOrReplacement,
                admission_class: DbAdmissionClass::NormalRevision,
                workspace_path: Some("/tmp/ws"),
            },
        )
        .await
        .expect("same-work-unit Author continuation");
    }

    #[tokio::test]
    async fn task5_plan_reviewer_requires_latest_author_and_stamps_exact_plan() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: sample_doc(
                    "tok-task5-reviewer-author",
                    ManifestWorkflowState::Estimated,
                ),
            },
        )
        .await
        .unwrap();
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let no_author_child = child_for(&db, AgentType::Codex).await;
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                no_author_child,
                "50000000-0000-4000-8000-000000000003",
                "codex",
                Some(&reviewer_key),
                None,
            ))
            .await
            .expect_err("reviewer before Author must reject");
        assert_eq!(err.workflow_admission_code(), Some("plan_author_missing"));

        let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        for (task_id, digest, ordinal) in [
            ("50000000-0000-4000-8000-000000000004", "sha256:old-plan", 1),
            ("50000000-0000-4000-8000-000000000005", "sha256:plan", 2),
        ] {
            let child = child_for(&db, AgentType::Codex).await;
            seed_completed_bound_run(
                &db,
                parent,
                child,
                &published.workflow_id,
                "plan-author",
                task_id,
                &author_key,
                "codex",
                ordinal,
                &author_summary(digest),
                Some(digest),
                None,
                None,
            )
            .await;
        }

        let reviewer_child = child_for(&db, AgentType::Codex).await;
        let reviewer_task = "50000000-0000-4000-8000-000000000006";
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                reviewer_child,
                reviewer_task,
                "codex",
                Some(&reviewer_key),
                None,
            ))
            .await
            .expect("reviewer after current Author");
        let binding = delegation_workflow_run_binding::Entity::find_by_id(reviewer_task)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            binding.reviewed_task_id.as_deref(),
            Some("50000000-0000-4000-8000-000000000005")
        );
        assert_eq!(binding.artifact_digest.as_deref(), Some("sha256:plan"));
    }

    #[tokio::test]
    async fn task5_author_and_plan_reviewer_cannot_share_child_conversation() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: sample_doc(
                    "tok-task5-independent-author",
                    ManifestWorkflowState::Estimated,
                ),
            },
        )
        .await
        .unwrap();
        let shared_child = child_for(&db, AgentType::Codex).await;
        let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            shared_child,
            &published.workflow_id,
            "plan-author",
            "50000000-0000-4000-8000-000000000007",
            &author_key,
            "codex",
            1,
            &author_summary("sha256:plan"),
            Some("sha256:plan"),
            None,
            None,
        )
        .await;
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                shared_child,
                "50000000-0000-4000-8000-000000000008",
                "codex",
                Some(&reviewer_key),
                None,
            ))
            .await
            .expect_err("Author/reviewer child reuse must reject");
        assert_eq!(
            err.workflow_admission_code(),
            Some("reviewer_not_independent")
        );
    }

    #[tokio::test]
    async fn task5_two_plan_reviewers_cannot_share_child_conversation() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let published = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: two_plan_reviewer_doc("tok-task5-independent-plan-reviewers"),
            },
        )
        .await
        .unwrap();
        let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            child_for(&db, AgentType::Codex).await,
            &published.workflow_id,
            "plan-author",
            "50000000-0000-4000-8000-000000000009",
            &author_key,
            "codex",
            1,
            &author_summary("sha256:plan"),
            Some("sha256:plan"),
            None,
            None,
        )
        .await;
        let shared_child = child_for(&db, AgentType::Grok).await;
        let reviewer_one_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            shared_child,
            &published.workflow_id,
            "plan-reviewer-1",
            "50000000-0000-4000-8000-000000000010",
            &reviewer_one_key,
            "codex",
            2,
            review_summary(),
            Some("sha256:plan"),
            Some("50000000-0000-4000-8000-000000000009"),
            None,
        )
        .await;
        let reviewer_two_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                shared_child,
                "50000000-0000-4000-8000-000000000011",
                "grok",
                Some(&reviewer_two_key),
                None,
            ))
            .await
            .expect_err("Plan reviewers must use independent children");
        assert_eq!(
            err.workflow_admission_code(),
            Some("reviewer_not_independent")
        );
    }

    #[tokio::test]
    async fn task5_implementer_and_task_reviewer_cannot_share_child_conversation() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "tok-task5-independent-task").await;
        let shared_child = child_for(&db, AgentType::Grok).await;
        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let implementer_task = "50000000-0000-4000-8000-000000000012";
        seed_completed_bound_run(
            &db,
            parent,
            shared_child,
            &workflow_id,
            "task-1-impl",
            implementer_task,
            &implementer_key,
            "grok",
            1,
            implementation_summary(),
            Some("producer-digest"),
            None,
            None,
        )
        .await;
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                shared_child,
                "50000000-0000-4000-8000-000000000013",
                "codex",
                Some(&reviewer_key),
                None,
            ))
            .await
            .expect_err("implementer/reviewer child reuse must reject");
        assert_eq!(
            err.workflow_admission_code(),
            Some("reviewer_not_independent")
        );
    }

    #[tokio::test]
    async fn task5_high_risk_reviewers_cannot_share_child_and_route_freezes_three_nodes() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let workflow_id =
            publish_document_approved(&db, &emitter, parent, high_risk_doc("tok-task5-high-route"))
                .await;
        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let implementer_task = "50000000-0000-4000-8000-000000000014";
        seed_completed_bound_run(
            &db,
            parent,
            child_for(&db, AgentType::Codex).await,
            &workflow_id,
            "task-1-impl",
            implementer_task,
            &implementer_key,
            "codex",
            1,
            implementation_summary(),
            Some("high-producer-digest"),
            None,
            None,
        )
        .await;
        let shared_child = child_for(&db, AgentType::Grok).await;
        let codex_reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            shared_child,
            &workflow_id,
            "task-1-rev",
            "50000000-0000-4000-8000-000000000015",
            &codex_reviewer_key,
            "codex",
            2,
            review_summary(),
            Some("high-producer-digest"),
            Some(implementer_task),
            Some(1),
        )
        .await;
        let grok_reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter.clone());
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                shared_child,
                "50000000-0000-4000-8000-000000000016",
                "grok",
                Some(&grok_reviewer_key),
                None,
            ))
            .await
            .expect_err("high-risk reviewers must use independent children");
        assert_eq!(
            err.workflow_admission_code(),
            Some("reviewer_not_independent")
        );

        let fresh_child = child_for(&db, AgentType::Grok).await;
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                fresh_child,
                "50000000-0000-4000-8000-000000000017",
                "grok",
                Some(&grok_reviewer_key),
                None,
            ))
            .await
            .expect("independent high-risk reviewer admits");
        let nodes = delegation_workflow_node_binding::Entity::find()
            .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(workflow_id))
            .filter(delegation_workflow_node_binding::Column::TaskIndex.eq(1_i64))
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().all(|node| node.cohort_frozen));
    }

    #[tokio::test]
    async fn task5_policy_revision_is_allowed_before_admission_but_frozen_afterward() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let first = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: sample_doc("tok-task5-policy-before", ManifestWorkflowState::Estimated),
            },
        )
        .await
        .expect("initial normal policy");
        let mut high = high_risk_doc("tok-task5-policy-material");
        high.workflow_id = Some(first.workflow_id.clone());
        high.expected_manifest_revision = Some(first.manifest_revision);
        let revised = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: high.clone(),
            },
        )
        .await
        .expect("material risk/route revision is legal before admission");

        seed_gate_settlement(
            &db,
            &revised.workflow_id,
            "plan",
            1,
            GateSettlementOutcome::Approved,
        )
        .await;
        let header = delegation_workflow::Entity::find_by_id(revised.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut header_am: delegation_workflow::ActiveModel = header.into();
        header_am.workflow_state = Set(WorkflowState::Approved);
        header_am.update(&db.conn).await.unwrap();

        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter.clone());
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child_for(&db, AgentType::Codex).await,
                "50000000-0000-4000-8000-000000000018",
                "codex",
                Some(&implementer_key),
                None,
            ))
            .await
            .expect("first cohort admission");

        let before = delegation_workflow_node_binding::Entity::find()
            .filter(
                delegation_workflow_node_binding::Column::WorkflowId
                    .eq(revised.workflow_id.clone()),
            )
            .filter(delegation_workflow_node_binding::Column::TaskIndex.eq(1_i64))
            .order_by_asc(delegation_workflow_node_binding::Column::NodeId)
            .all(&db.conn)
            .await
            .unwrap();
        high.workflow_id = Some(revised.workflow_id.clone());
        high.expected_manifest_revision = Some(revised.manifest_revision);
        high.publication_token = "tok-task5-policy-after".into();
        high.task_policies[0].risk.reason = "post-admission policy mutation".into();
        let err = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: high },
        )
        .await
        .expect_err("any frozen policy mutation must reject");
        assert!(
            matches!(err, WorkflowStoreError::CohortFrozen { .. }),
            "got {err:?}"
        );
        let after = delegation_workflow_node_binding::Entity::find()
            .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(revised.workflow_id))
            .filter(delegation_workflow_node_binding::Column::TaskIndex.eq(1_i64))
            .order_by_asc(delegation_workflow_node_binding::Column::NodeId)
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(after, before, "rejected publish must be atomic");

        let mut removed_route =
            sample_doc("tok-task5-route-removal", ManifestWorkflowState::Estimated);
        removed_route.workflow_id = Some(first.workflow_id);
        removed_route.expected_manifest_revision = Some(revised.manifest_revision);
        let err = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest {
                document: removed_route,
            },
        )
        .await
        .expect_err("frozen route removal must reject");
        assert!(
            matches!(err, WorkflowStoreError::CohortFrozen { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn task5_task_and_final_work_units_cannot_share_child_conversation() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "tok-task5-task-final").await;
        let implementer_task = "50000000-0000-4000-8000-000000000019";
        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            child_for(&db, AgentType::Grok).await,
            &workflow_id,
            "task-1-impl",
            implementer_task,
            &implementer_key,
            "grok",
            1,
            implementation_summary(),
            Some("task-final-digest"),
            None,
            None,
        )
        .await;
        let shared_child = child_for(&db, AgentType::Codex).await;
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            shared_child,
            &workflow_id,
            "task-1-rev",
            "50000000-0000-4000-8000-000000000020",
            &reviewer_key,
            "codex",
            2,
            review_summary(),
            Some("task-final-digest"),
            Some(implementer_task),
            Some(1),
        )
        .await;
        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                shared_child,
                "50000000-0000-4000-8000-000000000021",
                "codex",
                Some(&final_key),
                None,
            ))
            .await
            .expect_err("Task/Final child reuse must reject");
        assert_eq!(
            err.workflow_admission_code(),
            Some("reviewer_not_independent")
        );
    }

    #[tokio::test]
    async fn task5_different_task_work_units_cannot_share_child_conversation() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let workflow_id =
            publish_document_approved(&db, &emitter, parent, two_task_doc("tok-task5-two-tasks"))
                .await;
        let implementer_task = "50000000-0000-4000-8000-000000000022";
        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            child_for(&db, AgentType::Grok).await,
            &workflow_id,
            "task-1-impl",
            implementer_task,
            &implementer_key,
            "grok",
            1,
            implementation_summary(),
            Some("task-one-digest"),
            None,
            None,
        )
        .await;
        let shared_child = child_for(&db, AgentType::Codex).await;
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        seed_completed_bound_run(
            &db,
            parent,
            shared_child,
            &workflow_id,
            "task-1-rev",
            "50000000-0000-4000-8000-000000000023",
            &reviewer_key,
            "codex",
            2,
            review_summary(),
            Some("task-one-digest"),
            Some(implementer_task),
            Some(1),
        )
        .await;
        let task_two_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 2,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                shared_child,
                "50000000-0000-4000-8000-000000000024",
                "grok",
                Some(&task_two_key),
                None,
            ))
            .await
            .expect_err("different Task work units must not share a child");
        assert_eq!(
            err.workflow_admission_code(),
            Some("reviewer_not_independent")
        );
    }

    #[tokio::test]
    async fn task5_task_reviewer_requires_completed_producer_artifact_digest() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_approved(&db, &emitter, parent, "tok-task5-producer-digest").await;
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let implementer_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child_for(&db, AgentType::Grok).await,
                "50000000-0000-4000-8000-000000000025",
                "grok",
                Some(&implementer_key),
                None,
            ))
            .await
            .expect("producer reserves");
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child_for(&db, AgentType::Codex).await,
                "50000000-0000-4000-8000-000000000026",
                "codex",
                Some(&reviewer_key),
                None,
            ))
            .await
            .expect_err("reviewer must not admit before producer artifact exists");
        assert_eq!(
            err.workflow_admission_code(),
            Some("producer_artifact_missing")
        );
    }

    #[tokio::test]
    async fn wrong_key_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_approved(&db, &emitter, parent, "tok-wrong-key").await;

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let bad_key = "task|99|implementer|grok|none";
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0001",
                "grok",
                Some(bad_key),
                None,
            ))
            .await
            .expect_err("wrong key must reject");
        assert!(
            matches!(err, TaskStoreError::WorkflowAdmission { ref code, .. } if code == "workflow_binding_missing"),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn non_workflow_no_op() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let out = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0002",
                "grok",
                Some("unit-preboot"),
                None,
            ))
            .await
            .expect("non-workflow admit");
        assert!(matches!(out, Gen1AdmitOutcome::Created(_)));
        assert!(rx.try_recv().is_err(), "no workflow events");
        let bindings = delegation_workflow_run_binding::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert!(bindings.is_empty());
    }

    #[tokio::test]
    async fn a1_key_no_manifest_nudge() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0003",
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect("a1 no-manifest admit");
        let evt = rx.try_recv().expect("compatibility_nudge");
        assert_eq!(evt.channel, WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT);
        assert_eq!(
            evt.payload["parent_conversation_id"].as_i64(),
            Some(parent as i64)
        );
        let bindings = delegation_workflow_run_binding::Entity::find()
            .all(&db.conn)
            .await
            .unwrap();
        assert!(
            bindings.is_empty(),
            "no durable run_binding without manifest"
        );
    }

    #[tokio::test]
    async fn final_early_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_approved(&db, &emitter, parent, "tok-final-early").await;
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Codex).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0004",
                "codex",
                Some(&key),
                None,
            ))
            .await
            .expect_err("final early");
        assert!(
            matches!(err, TaskStoreError::WorkflowAdmission { ref code, .. } if code == "final_early"),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn final_fixer_before_non_pass_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_approved(&db, &emitter, parent, "tok-fixer-early").await;
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0005",
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect_err("fixer early");
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. } if code == "final_fixer_before_non_pass"
            ),
            "got {err:?}"
        );
    }

    /// Session-2534 regression: Final reviewer completed with request_changes
    /// only in a report file (chat had no card → summary_validated=false).
    /// Final fixer admission must reharvest and open the fix cycle.
    #[tokio::test]
    async fn final_fixer_admits_after_report_file_card_reharvest() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-fixer-reharvest").await;
        seed_task_gate_passed(&db, parent, &wf_id).await;

        let dir = std::env::temp_dir().join(format!(
            "codeg-final-reharvest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let report = dir.join("final-review.md");
        std::fs::write(
            &report,
            r#"# Final

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"build fails TS","report_file":".superpowers/sdd/final-review.md"}
-->
"#,
        )
        .unwrap();

        let key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let c = child_for(&db, AgentType::Codex).await;
        let reviewer_task = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00d1";
        // Empty card + unvalidated binding, but report path in touched_files.
        insert_completed_run_with_binding(
            &db,
            parent,
            c,
            reviewer_task,
            &wf_id,
            "final-reviewer",
            &key,
            "codex",
            "{}", // not a valid summary shape
            false,
        )
        .await;
        let run = delegation_task_run::Entity::find_by_id(reviewer_task.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut am: delegation_task_run::ActiveModel = run.into();
        am.card_summary_json = Set(None);
        am.touched_files_json = Set(Some(format!(
            r#"[{{"path":"{}","outside_workspace":false}}]"#,
            report.display().to_string().replace('\\', "/")
        )));
        am.workspace_path = Set(Some(dir.display().to_string()));
        am.update(&db.conn).await.unwrap();

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let fixer_key = build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00d2",
                "grok",
                Some(&fixer_key),
                None,
            ))
            .await
            .expect("final fixer should admit after report reharvest");

        // Reharvest must durable-write validated card for projection/gates.
        let repaired = delegation_task_run::Entity::find_by_id(reviewer_task.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let card = repaired
            .card_summary_json
            .as_deref()
            .and_then(parse_and_validate_summary_json)
            .expect("persisted card");
        match card {
            CardSummary::Review {
                verdict: ReviewVerdict::RequestChanges,
                ..
            } => {}
            other => panic!("expected request_changes review, got {other:?}"),
        }
        let rb = delegation_workflow_run_binding::Entity::find_by_id(reviewer_task.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert!(rb.summary_validated);

        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn replace_with_active_final_binding(
        db: &AppDatabase,
        workflow_id: &str,
        retired_node_id: &str,
        replacement_node_id: &str,
        role: &str,
        agent: &str,
    ) {
        let now = Utc::now();
        let binding = delegation_workflow_node_binding::Entity::find_by_id((
            workflow_id.to_string(),
            retired_node_id.to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .expect("published Final binding");
        let mut retired: delegation_workflow_node_binding::ActiveModel = binding.into();
        retired.retired_revision = Set(Some(2));
        retired.retained_observed = Set(true);
        retired.updated_at = Set(now);
        retired
            .update(&db.conn)
            .await
            .expect("retire Final binding");

        delegation_workflow_node_binding::ActiveModel {
            workflow_id: Set(workflow_id.to_string()),
            node_id: Set(replacement_node_id.to_string()),
            work_unit_key: Set(format!("replacement-final|{role}|{replacement_node_id}")),
            role: Set(role.to_string()),
            agent_type: Set(agent.to_string()),
            profile_id: Set(None),
            phase_id: Set(PHASE_FINAL.to_string()),
            task_index: Set(None),
            introduced_revision: Set(2),
            retired_revision: Set(None),
            is_observed: Set(false),
            retained_observed: Set(false),
            cohort_frozen: Set(false),
            node_outcome: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("insert replacement Final binding");
    }

    async fn workflow_header(db: &AppDatabase, workflow_id: &str) -> delegation_workflow::Model {
        delegation_workflow::Entity::find_by_id(workflow_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .expect("workflow header")
    }

    #[tokio::test]
    async fn task6_active_final_evidence_ignores_retired_reviewer_binding() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "task6-active-final-reviewer").await;
        replace_with_active_final_binding(
            &db,
            &workflow_id,
            "final-reviewer",
            "final-reviewer-replacement",
            "reviewer",
            "codex",
        )
        .await;
        let reviewer_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        insert_completed_run_with_binding(
            &db,
            parent,
            child_for(&db, AgentType::Codex).await,
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00b1",
            &workflow_id,
            "final-reviewer-replacement",
            &reviewer_key,
            "codex",
            r#"{"kind":"review","verdict":"request_changes","critical":1,"important":0,"minor":0,"summary":"fix"}"#,
            true,
        )
        .await;

        let evidence = load_latest_final_reviewer_evidence(
            &db.conn,
            &workflow_header(&db, &workflow_id).await,
        )
        .await
        .unwrap()
        .expect("authoritative active Final reviewer evidence");

        assert_eq!(evidence.task_id, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00b1");
        assert!(reviewer_is_request_changes_or_block(&evidence));
    }

    #[tokio::test]
    async fn task6_active_final_evidence_ignores_retired_fixer_binding() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (workflow_id, _) =
            publish_approved(&db, &emitter, parent, "task6-active-final-fixer").await;
        replace_with_active_final_binding(
            &db,
            &workflow_id,
            "final-fixer",
            "final-fixer-replacement",
            "fixer",
            "grok",
        )
        .await;
        let fixer_key = build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        insert_completed_run_with_binding(
            &db,
            parent,
            child_for(&db, AgentType::Grok).await,
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00c2",
            &workflow_id,
            "final-fixer-replacement",
            &fixer_key,
            "grok",
            r#"{"kind":"implementation","phase":"fix","status":"done","summary":"fixed"}"#,
            true,
        )
        .await;

        assert_eq!(
            evaluate_final_fixer_terminal_pass(
                &db.conn,
                &workflow_header(&db, &workflow_id).await,
            )
            .await
            .unwrap(),
            Some(true)
        );
    }

    #[tokio::test]
    async fn task_first_dispatch_blocked_when_plan_not_approved() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        // Estimated only — no plan settlement.
        let doc = sample_doc("tok-a83", ManifestWorkflowState::Estimated);
        publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish estimated");
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0006",
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect_err("A8.3");
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. }
                    if code == "plan_gate_not_approved" || code == "plan_gate_reopen"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn task_first_dispatch_blocked_returns_typed_projection_without_authorization_id() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let doc = sample_doc("task7-blocked-admission", ManifestWorkflowState::Blocked);
        let published = publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish blocked workflow");
        let header = delegation_workflow::Entity::find_by_id(published.workflow_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let snapshot = load_workflow_recovery_snapshot_conn(&db.conn, &header, None)
            .await
            .unwrap();
        assert!(
            !snapshot.contradictory_durable_state,
            "fresh blocked publication snapshot: {snapshot:#?}"
        );
        let state = crate::acp::delegation::workflow::store::get_workflow_state_core(
            &db,
            parent,
            Some(&header.workflow_id),
        )
        .await
        .unwrap();
        assert_eq!(
            state.recovery,
            Some(decide_workflow_recovery(&snapshot).projection())
        );
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child_for(&db, AgentType::Grok).await,
                "70000000-0000-4000-8000-000000000007",
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect_err("blocked workflow remains fail-closed");

        let TaskStoreError::WorkflowAdmission { code, message } = err else {
            panic!("expected workflow admission rejection");
        };
        assert_eq!(code, "workflow_blocked");
        let body = crate::acp::delegation::workflow::error::WorkflowAdmissionRecoveryError::decode(
            &message,
        )
        .expect("typed recovery projection");
        assert_eq!(
            body.message,
            "workflow is blocked; new Task admissions rejected"
        );
        assert_eq!(body.recovery.disposition, "confirmation_required");
        assert_eq!(
            body.recovery.proposed_action.as_deref(),
            Some("recover_workflow")
        );
        assert_eq!(
            body.recovery.target_state,
            Some(ManifestWorkflowState::Estimated)
        );
        assert_eq!(body.recovery.cause_code, "explicit_manifest_block");
        assert_eq!(body.recovery.risk_class, "normal");
        assert!(body.recovery.authorization_required);
        assert!(body.recovery.blockers.is_empty());

        let serialized = serde_json::to_string(&body).unwrap().to_ascii_lowercase();
        assert!(!serialized.contains("authorization_id"));
        assert!(!serialized.contains("authorizationid"));
        assert!(!serialized.contains("receipt"));
    }

    #[tokio::test]
    async fn routed_cohort_freezes_before_reviewer_producer_readiness() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        // Drain publish events.
        let _ = publish_approved(&db, &emitter, parent, "tok-b14").await;
        while rx.try_recv().is_ok() {}

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter.clone());
        let child = child_for(&db, AgentType::Grok).await;
        let impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0010",
                "grok",
                Some(&impl_key),
                None,
            ))
            .await
            .expect("impl admit");

        let evt = rx.try_recv().expect("graph changed on admit");
        assert_eq!(evt.channel, WORKFLOW_GRAPH_CHANGED_EVENT);

        // The complete normal-risk route freezes on first admission.
        let nodes = delegation_workflow_node_binding::Entity::find()
            .filter(delegation_workflow_node_binding::Column::TaskIndex.eq(1_i64))
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|n| n.cohort_frozen));

        // Freeze does not make an unready reviewer admissible.
        let child2 = child_for(&db, AgentType::Codex).await;
        let rev_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child2,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0011",
                "codex",
                Some(&rev_key),
                None,
            ))
            .await
            .expect_err("reviewer waits for completed producer artifact");
        assert_eq!(
            err.workflow_admission_code(),
            Some("producer_artifact_missing")
        );

        let run = delegation_task_run::Entity::find_by_id(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0010".to_string(),
        )
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut run_am: delegation_task_run::ActiveModel = run.into();
        run_am.status = Set(DelegationRunStatus::Completed);
        run_am.reached_running_at = Set(Some(Utc::now()));
        run_am.finished_at = Set(Some(Utc::now()));
        run_am.card_summary_json = Set(Some(implementation_summary().into()));
        run_am.update(&db.conn).await.unwrap();
        let rb = delegation_workflow_run_binding::Entity::find_by_id(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0010".to_string(),
        )
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut rb_am: delegation_workflow_run_binding::ActiveModel = rb.into();
        rb_am.summary_validated = Set(true);
        rb_am.artifact_digest = Set(Some("producer-ready".into()));
        rb_am.update(&db.conn).await.unwrap();
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child2,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0011",
                "codex",
                Some(&rev_key),
                None,
            ))
            .await
            .expect("independent reviewer admits after producer completion");
    }

    #[tokio::test]
    async fn promote_running_projects_workflow_transition_after_commit() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let (workflow_id, _) = publish_approved(&db, &emitter, parent, "tok-promote").await;
        while rx.try_recv().is_ok() {}

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let task_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0012";
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                task_id,
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect("admit mapped implementer");
        while rx.try_recv().is_ok() {}

        let before = delegation_workflow::Entity::find_by_id(workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .graph_revision;
        store
            .bind_child_connection_while_reserving(task_id, "conn-promote")
            .await
            .expect("bind promote owner");
        store
            .promote_running(task_id, "conn-promote", Utc::now())
            .await
            .expect("promote mapped run");

        let after = delegation_workflow::Entity::find_by_id(workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .graph_revision;
        assert_eq!(after, before + 1, "promote must advance graph revision");
        let event = rx.try_recv().expect("graph change after promote commit");
        assert_eq!(event.channel, WORKFLOW_GRAPH_CHANGED_EVENT);
        assert_eq!(
            event.payload["workflow_id"].as_str(),
            Some(workflow_id.as_str())
        );
        assert_eq!(event.payload["graph_revision"].as_u64(), Some(after as u64));
    }

    #[tokio::test]
    async fn terminal_settle_projects_workflow_transition_after_commit() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let (workflow_id, _) = publish_approved(&db, &emitter, parent, "tok-terminal").await;
        while rx.try_recv().is_ok() {}

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let task_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0013";
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                task_id,
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect("admit mapped implementer");
        store
            .bind_child_connection_while_reserving(task_id, "conn-terminal")
            .await
            .expect("bind terminal owner");
        store
            .promote_running(task_id, "conn-terminal", Utc::now())
            .await
            .expect("promote before settle");
        while rx.try_recv().is_ok() {}

        let before = delegation_workflow::Entity::find_by_id(workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .graph_revision;
        store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .expect("settle mapped run");

        let after = delegation_workflow::Entity::find_by_id(workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .graph_revision;
        assert_eq!(
            after,
            before + 1,
            "terminal settle must advance graph revision"
        );
        let event = rx.try_recv().expect("graph change after terminal commit");
        assert_eq!(event.channel, WORKFLOW_GRAPH_CHANGED_EVENT);
        assert_eq!(
            event.payload["workflow_id"].as_str(),
            Some(workflow_id.as_str())
        );
        assert_eq!(event.payload["graph_revision"].as_u64(), Some(after as u64));
    }

    #[tokio::test]
    async fn pre_admission_settle_projects_workflow_transition_after_commit() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let (workflow_id, _) = publish_approved(&db, &emitter, parent, "tok-pre-admission").await;
        while rx.try_recv().is_ok() {}

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let task_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0014";
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                task_id,
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect("admit mapped implementer");
        while rx.try_recv().is_ok() {}

        let before = delegation_workflow::Entity::find_by_id(workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .graph_revision;
        let settlement = store
            .settle_pre_admission_failure_if_owned(
                task_id,
                "conn-pre-admission",
                TerminalTaskWrite::failed(
                    "spawn_failed",
                    Utc::now(),
                    ConversationStatus::Cancelled,
                ),
            )
            .await
            .expect("settle mapped reserving run")
            .expect("owned settlement");
        assert!(matches!(
            settlement,
            crate::acp::delegation::store::Settlement::Won(_)
        ));

        let after = delegation_workflow::Entity::find_by_id(workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap()
            .graph_revision;
        assert_eq!(
            after,
            before + 1,
            "pre-admission settle must advance graph revision"
        );
        let event = rx
            .try_recv()
            .expect("graph change after pre-admission terminal commit");
        assert_eq!(event.channel, WORKFLOW_GRAPH_CHANGED_EVENT);
        assert_eq!(
            event.payload["workflow_id"].as_str(),
            Some(workflow_id.as_str())
        );
        assert_eq!(event.payload["graph_revision"].as_u64(), Some(after as u64));
    }

    #[tokio::test]
    async fn continue_retained_observed_after_plan_revision() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-ret").await;
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter.clone());

        let child = child_for(&db, AgentType::Grok).await;
        let impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let task_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0020";
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                task_id,
                "grok",
                Some(&impl_key),
                None,
            ))
            .await
            .expect("impl");

        // Mark implementer terminal so continue path is legal for thread, then
        // plan-revision retain the binding.
        let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut am: delegation_task_run::ActiveModel = run.into();
        am.status = Set(DelegationRunStatus::Completed);
        am.reached_running_at = Set(Some(Utc::now()));
        am.finished_at = Set(Some(Utc::now()));
        am.card_summary_json = Set(Some(
            r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"ok"}"#
                .into(),
        ));
        am.update(&db.conn).await.unwrap();
        let rb = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut rba: delegation_workflow_run_binding::ActiveModel = rb.into();
        rba.summary_validated = Set(true);
        rba.artifact_digest = Set(Some("abc".into()));
        rba.update(&db.conn).await.unwrap();

        // Retire node as retained_observed (plan revision).
        let node = delegation_workflow_node_binding::Entity::find()
            .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(wf_id.clone()))
            .filter(delegation_workflow_node_binding::Column::WorkUnitKey.eq(impl_key.clone()))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut nam: delegation_workflow_node_binding::ActiveModel = node.into();
        nam.retired_revision = Set(Some(2));
        nam.retained_observed = Set(true);
        nam.cohort_frozen = Set(true);
        nam.updated_at = Set(Utc::now());
        nam.update(&db.conn).await.unwrap();

        // Header estimated (plan re-open).
        let header = delegation_workflow::Entity::find_by_id(wf_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut ham: delegation_workflow::ActiveModel = header.into();
        ham.workflow_state = Set(WorkflowState::Estimated);
        ham.supersedes_approved_revision = Set(Some(1));
        ham.update(&db.conn).await.unwrap();

        // First-dispatch against retained (retired) must reject (B2).
        let child2 = child_for(&db, AgentType::Grok).await;
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child2,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0021",
                "grok",
                Some(&impl_key),
                None,
            ))
            .await
            .expect_err("first dispatch on retained must reject");
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. }
                    if code == "workflow_binding_not_active"
                        || code == "workflow_binding_missing"
            ) || matches!(err, TaskStoreError::InvalidReplacement(_)),
            "got {err:?}"
        );

        // Continue/replacement admission against retained_observed must succeed (B2).
        // Exercise the workflow txn helper directly (continue eligibility is separate).
        let cont_task = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0022";
        let cont_child = child;
        // Insert a reserving run row then admit workflow as ContinueOrReplacement.
        let insert = gen1_insert(parent, cont_child, cont_task, "grok", Some(&impl_key), None);
        db.conn
            .transaction::<_, (), TaskStoreError>(|txn| {
                let insert = insert.clone();
                Box::pin(async move {
                    // Minimal direct insert of reserving row via ActiveModel.
                    let now = Utc::now();
                    let model = delegation_task_run::ActiveModel {
                        task_id: Set(insert.task_id.clone()),
                        root_task_id: Set(insert.root_task_id.clone()),
                        previous_task_id: Set(Some(task_id.to_string())),
                        generation: Set(2),
                        parent_conversation_id: Set(insert.parent_conversation_id),
                        parent_tool_use_id: Set(insert.parent_tool_use_id.clone()),
                        child_conversation_id: Set(insert.child_conversation_id),
                        agent_type: Set(insert.agent_type.clone()),
                        profile_id: Set(insert.profile_id.clone()),
                        workspace_path: Set(insert.workspace_path.clone()),
                        route_fingerprint: Set(insert.route_fingerprint.clone()),
                        launch_snapshot_version: Set(insert.launch_snapshot_version.clone()),
                        mode_id: Set(None),
                        config_values_json: Set(insert.config_values_json.clone()),
                        task_preview: Set(insert.task_preview.clone()),
                        request_fingerprint: Set(insert.request_fingerprint.clone()),
                        admission_class: Set(DbAdmissionClass::UnexpectedContinue),
                        reached_running_at: Set(None),
                        lineage_root_task_id: Set(task_id.to_string()),
                        work_unit_key: Set(insert.work_unit_key.clone()),
                        legacy_parent_tool_use_id: Set(None),
                        history_only: Set(false),
                        status: Set(DelegationRunStatus::Reserving),
                        error_code: Set(None),
                        termination_audit_json: Set(None),
                        started_at: Set(Some(now)),
                        finished_at: Set(None),
                        tool_call_count: Set(Some(0)),
                        edit_tool_call_count: Set(Some(0)),
                        touched_files_json: Set(Some("[]".into())),
                        touched_files_truncated: Set(Some(false)),
                        additions: Set(None),
                        deletions: Set(None),
                        line_counts_complete: Set(Some(false)),
                        card_summary_json: Set(None),
                        child_turn_anchor: Set(None),
                        child_connection_id: Set(None),
                        replaced_task_id: Set(None),
                        replacement_reason: Set(None),
                        recovery_authorization_id: Set(None),
                        created_at: Set(now),
                        updated_at: Set(now),
                        ..Default::default()
                    };
                    model.insert(txn).await.map_err(map_db)?;
                    admit_workflow_run_txn(
                        txn,
                        &WorkflowAdmitInput {
                            parent_conversation_id: parent,
                            child_conversation_id: cont_child,
                            task_id: cont_task,
                            work_unit_key: Some(&impl_key),
                            agent_type: "grok",
                            profile_id: None,
                            lineage_root_task_id: task_id,
                            generation: 2,
                            kind: AdmissionDispatchKind::ContinueOrReplacement,
                            admission_class: DbAdmissionClass::UnexpectedContinue,
                            workspace_path: Some("/tmp"),
                        },
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .expect("continue retained_observed workflow admit");
    }

    #[tokio::test]
    async fn provisional_abandon_bumps_clock() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        let (wf_id, g0) = publish_approved(&db, &emitter, parent, "tok-abandon").await;
        while rx.try_recv().is_ok() {}

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let task_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0030";
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                task_id,
                "grok",
                Some(&impl_key),
                None,
            ))
            .await
            .expect("admit");
        while rx.try_recv().is_ok() {}

        let header_before = delegation_workflow::Entity::find_by_id(wf_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let rev_before = header_before.graph_revision;

        let abandoned = store
            .abandon_reserving_claim(task_id)
            .await
            .expect("abandon");
        assert!(abandoned);

        let header_after = delegation_workflow::Entity::find_by_id(wf_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert!(
            header_after.graph_revision > rev_before,
            "provisional abandon must bump graph_revision"
        );
        let evt = rx.try_recv().expect("changed on abandon");
        assert_eq!(evt.channel, WORKFLOW_GRAPH_CHANGED_EVENT);
        let _ = g0;
    }

    #[tokio::test]
    async fn agent_profile_mismatch_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_approved(&db, &emitter, parent, "tok-agent").await;
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Codex).await;
        let impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        // Wrong agent_type on run vs binding/key material.
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0040",
                "codex",
                Some(&impl_key),
                None,
            ))
            .await
            .expect_err("agent mismatch");
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. }
                    if code == "workflow_agent_mismatch" || code == "workflow_role_mismatch"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn task5_route_requires_exact_profile_identity() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let mut doc = sample_doc("tok-task5-profile-route", ManifestWorkflowState::Estimated);
        let implementer = doc
            .nodes
            .iter_mut()
            .find(|node| node.id == "task-1-impl")
            .unwrap();
        implementer.profile_id = Some("grok-profile".into());
        implementer.work_unit_key = Some(
            build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                task_index: 1,
                agent_type: "grok",
                profile_id: Some("grok-profile"),
            })
            .unwrap(),
        );
        publish_document_approved(&db, &emitter, parent, doc).await;
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: Some("grok-profile"),
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child_for(&db, AgentType::Grok).await,
                "50000000-0000-4000-8000-000000000029",
                "grok",
                Some(&key),
                None,
            ))
            .await
            .expect_err("omitted profile must not match routed profile");
        assert_eq!(
            err.workflow_admission_code(),
            Some("workflow_profile_mismatch")
        );
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child_for(&db, AgentType::Grok).await,
                "50000000-0000-4000-8000-000000000030",
                "grok",
                Some(&key),
                Some("grok-profile"),
            ))
            .await
            .expect("exact routed profile admits");
    }

    #[tokio::test]
    async fn task5_empty_profile_does_not_match_unprofiled_route() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_approved(&db, &emitter, parent, "tok-task5-empty-profile").await;
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child_for(&db, AgentType::Grok).await,
                "50000000-0000-4000-8000-000000000033",
                "grok",
                Some(&key),
                Some(""),
            ))
            .await
            .expect_err("empty profile must not match a route with no profile");
        assert_eq!(
            err.workflow_admission_code(),
            Some("workflow_profile_mismatch")
        );
    }

    #[tokio::test]
    async fn re_review_before_fixer_pass_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-rerev").await;

        // Seed all task gates passed + final reviewer non-pass + fixer non-pass terminal.
        seed_task_gate_passed(&db, parent, &wf_id).await;
        seed_final_reviewer_non_pass(&db, parent, &wf_id).await;
        seed_final_fixer_non_pass(&db, parent, &wf_id).await;

        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let cont_task = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0051";
        let cont_child = child_for(&db, AgentType::Codex).await;

        let err = db
            .conn
            .transaction::<_, (), TaskStoreError>(|txn| {
                Box::pin(async move {
                    let now = Utc::now();
                    let model = delegation_task_run::ActiveModel {
                        task_id: Set(cont_task.into()),
                        root_task_id: Set(cont_task.into()),
                        previous_task_id: Set(None),
                        generation: Set(2),
                        parent_conversation_id: Set(parent),
                        parent_tool_use_id: Set(Some("tool-rerev".into())),
                        child_conversation_id: Set(cont_child),
                        agent_type: Set("codex".into()),
                        profile_id: Set(None),
                        workspace_path: Set(Some("/tmp".into())),
                        route_fingerprint: Set(Some("rf".into())),
                        launch_snapshot_version: Set(Some("v1".into())),
                        mode_id: Set(None),
                        config_values_json: Set(Some("{}".into())),
                        task_preview: Set(Some("rerev".into())),
                        request_fingerprint: Set(Some("fp-rerev".into())),
                        admission_class: Set(DbAdmissionClass::UnexpectedContinue),
                        reached_running_at: Set(None),
                        lineage_root_task_id: Set(cont_task.into()),
                        work_unit_key: Set(Some(final_key.clone())),
                        legacy_parent_tool_use_id: Set(None),
                        history_only: Set(false),
                        status: Set(DelegationRunStatus::Reserving),
                        error_code: Set(None),
                        termination_audit_json: Set(None),
                        started_at: Set(Some(now)),
                        finished_at: Set(None),
                        tool_call_count: Set(Some(0)),
                        edit_tool_call_count: Set(Some(0)),
                        touched_files_json: Set(Some("[]".into())),
                        touched_files_truncated: Set(Some(false)),
                        additions: Set(None),
                        deletions: Set(None),
                        line_counts_complete: Set(Some(false)),
                        card_summary_json: Set(None),
                        child_turn_anchor: Set(None),
                        child_connection_id: Set(None),
                        replaced_task_id: Set(None),
                        replacement_reason: Set(None),
                        recovery_authorization_id: Set(None),
                        created_at: Set(now),
                        updated_at: Set(now),
                        ..Default::default()
                    };
                    model.insert(txn).await.map_err(map_db)?;
                    admit_workflow_run_txn(
                        txn,
                        &WorkflowAdmitInput {
                            parent_conversation_id: parent,
                            child_conversation_id: cont_child,
                            task_id: cont_task,
                            work_unit_key: Some(&final_key),
                            agent_type: "codex",
                            profile_id: None,
                            lineage_root_task_id: cont_task,
                            generation: 2,
                            kind: AdmissionDispatchKind::ContinueOrReplacement,
                            // Scoped re-review after request_changes = normal_revision.
                            admission_class: DbAdmissionClass::NormalRevision,
                            workspace_path: Some("/tmp"),
                        },
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .expect_err("re-review before fixer pass");
        let err = match err {
            sea_orm::TransactionError::Transaction(e) => e,
            other => panic!("unexpected txn err {other:?}"),
        };
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. }
                    if code == "final_rereview_before_fixer_pass"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn re_review_continue_with_no_fixer_rejects() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-nofixer").await;
        seed_task_gate_passed(&db, parent, &wf_id).await;
        seed_final_reviewer_non_pass(&db, parent, &wf_id).await;
        // Intentionally no fixer terminal.

        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let cont_task = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0060";
        let cont_child = child_for(&db, AgentType::Codex).await;
        let err = db
            .conn
            .transaction::<_, (), TaskStoreError>(|txn| {
                Box::pin(async move {
                    let now = Utc::now();
                    let model = delegation_task_run::ActiveModel {
                        task_id: Set(cont_task.into()),
                        root_task_id: Set(cont_task.into()),
                        previous_task_id: Set(None),
                        generation: Set(2),
                        parent_conversation_id: Set(parent),
                        parent_tool_use_id: Set(Some("tool-nofixer".into())),
                        child_conversation_id: Set(cont_child),
                        agent_type: Set("codex".into()),
                        profile_id: Set(None),
                        workspace_path: Set(Some("/tmp".into())),
                        route_fingerprint: Set(Some("rf".into())),
                        launch_snapshot_version: Set(Some("v1".into())),
                        mode_id: Set(None),
                        config_values_json: Set(Some("{}".into())),
                        task_preview: Set(Some("rerev".into())),
                        request_fingerprint: Set(Some("fp-nofixer".into())),
                        admission_class: Set(DbAdmissionClass::UnexpectedContinue),
                        reached_running_at: Set(None),
                        lineage_root_task_id: Set(cont_task.into()),
                        work_unit_key: Set(Some(final_key.clone())),
                        legacy_parent_tool_use_id: Set(None),
                        history_only: Set(false),
                        status: Set(DelegationRunStatus::Reserving),
                        error_code: Set(None),
                        termination_audit_json: Set(None),
                        started_at: Set(Some(now)),
                        finished_at: Set(None),
                        tool_call_count: Set(Some(0)),
                        edit_tool_call_count: Set(Some(0)),
                        touched_files_json: Set(Some("[]".into())),
                        touched_files_truncated: Set(Some(false)),
                        additions: Set(None),
                        deletions: Set(None),
                        line_counts_complete: Set(Some(false)),
                        card_summary_json: Set(None),
                        child_turn_anchor: Set(None),
                        child_connection_id: Set(None),
                        replaced_task_id: Set(None),
                        replacement_reason: Set(None),
                        recovery_authorization_id: Set(None),
                        created_at: Set(now),
                        updated_at: Set(now),
                        ..Default::default()
                    };
                    model.insert(txn).await.map_err(map_db)?;
                    admit_workflow_run_txn(
                        txn,
                        &WorkflowAdmitInput {
                            parent_conversation_id: parent,
                            child_conversation_id: cont_child,
                            task_id: cont_task,
                            work_unit_key: Some(&final_key),
                            agent_type: "codex",
                            profile_id: None,
                            lineage_root_task_id: cont_task,
                            generation: 2,
                            kind: AdmissionDispatchKind::ContinueOrReplacement,
                            admission_class: DbAdmissionClass::NormalRevision,
                            workspace_path: Some("/tmp"),
                        },
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .expect_err("no fixer");
        let err = match err {
            sea_orm::TransactionError::Transaction(e) => e,
            other => panic!("unexpected {other:?}"),
        };
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. }
                    if code == "final_rereview_before_fixer_pass"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn unexpected_continue_final_reviewer_allowed_without_fixer() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-uc-final").await;
        seed_task_gate_passed(&db, parent, &wf_id).await;
        // No Final fixer — unexpected_continue recovery must still admit.

        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let cont_task = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee0070";
        let cont_child = child_for(&db, AgentType::Codex).await;
        db.conn
            .transaction::<_, (), TaskStoreError>(|txn| {
                Box::pin(async move {
                    let now = Utc::now();
                    let model = delegation_task_run::ActiveModel {
                        task_id: Set(cont_task.into()),
                        root_task_id: Set(cont_task.into()),
                        previous_task_id: Set(None),
                        generation: Set(2),
                        parent_conversation_id: Set(parent),
                        parent_tool_use_id: Set(Some("tool-uc-final".into())),
                        child_conversation_id: Set(cont_child),
                        agent_type: Set("codex".into()),
                        profile_id: Set(None),
                        workspace_path: Set(Some("/tmp".into())),
                        route_fingerprint: Set(Some("rf".into())),
                        launch_snapshot_version: Set(Some("v1".into())),
                        mode_id: Set(None),
                        config_values_json: Set(Some("{}".into())),
                        task_preview: Set(Some("uc".into())),
                        request_fingerprint: Set(Some("fp-uc-final".into())),
                        admission_class: Set(DbAdmissionClass::UnexpectedContinue),
                        reached_running_at: Set(None),
                        lineage_root_task_id: Set(cont_task.into()),
                        work_unit_key: Set(Some(final_key.clone())),
                        legacy_parent_tool_use_id: Set(None),
                        history_only: Set(false),
                        status: Set(DelegationRunStatus::Reserving),
                        error_code: Set(None),
                        termination_audit_json: Set(None),
                        started_at: Set(Some(now)),
                        finished_at: Set(None),
                        tool_call_count: Set(Some(0)),
                        edit_tool_call_count: Set(Some(0)),
                        touched_files_json: Set(Some("[]".into())),
                        touched_files_truncated: Set(Some(false)),
                        additions: Set(None),
                        deletions: Set(None),
                        line_counts_complete: Set(Some(false)),
                        card_summary_json: Set(None),
                        child_turn_anchor: Set(None),
                        child_connection_id: Set(None),
                        replaced_task_id: Set(None),
                        replacement_reason: Set(None),
                        recovery_authorization_id: Set(None),
                        created_at: Set(now),
                        updated_at: Set(now),
                        ..Default::default()
                    };
                    model.insert(txn).await.map_err(map_db)?;
                    admit_workflow_run_txn(
                        txn,
                        &WorkflowAdmitInput {
                            parent_conversation_id: parent,
                            child_conversation_id: cont_child,
                            task_id: cont_task,
                            work_unit_key: Some(&final_key),
                            agent_type: "codex",
                            profile_id: None,
                            lineage_root_task_id: cont_task,
                            generation: 2,
                            kind: AdmissionDispatchKind::ContinueOrReplacement,
                            admission_class: DbAdmissionClass::UnexpectedContinue,
                            workspace_path: Some("/tmp"),
                        },
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .expect("unexpected_continue Final reviewer without fixer");
    }

    #[tokio::test]
    async fn final_admission_rejects_estimated_and_blocked() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        // Estimated with full graph still must not admit Final (lifecycle gate).
        let doc = sample_doc("tok-final-est", ManifestWorkflowState::Estimated);
        publish_workflow_manifest_core(
            &db,
            &emitter,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish estimated");

        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Codex).await;
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00e1",
                "codex",
                Some(&final_key),
                None,
            ))
            .await
            .expect_err("estimated must block Final");
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. }
                    if code == "final_before_plan_approved"
            ),
            "got {err:?}"
        );

        // Blocked path.
        let (db2, parent2) = seed_parent().await;
        let (emitter2, _) = emitter_with_rx();
        let blocked = sample_doc("tok-final-blocked", ManifestWorkflowState::Blocked);
        publish_workflow_manifest_core(
            &db2,
            &emitter2,
            parent2,
            PublishWorkflowRequest { document: blocked },
        )
        .await
        .expect("publish blocked");
        let store2 = RunStore::new(Arc::new(AppDatabase {
            conn: db2.conn.clone(),
        }))
        .with_workflow_emitter(emitter2);
        let child2 = child_for(&db2, AgentType::Codex).await;
        let key2 = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let err2 = store2
            .admit_gen1_reserving(gen1_insert(
                parent2,
                child2,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00e2",
                "codex",
                Some(&key2),
                None,
            ))
            .await
            .expect_err("blocked must reject Final");
        assert!(
            matches!(
                err2,
                TaskStoreError::WorkflowAdmission { ref code, .. }
                    if code == "workflow_blocked" || code == "final_before_plan_approved"
            ),
            "got {err2:?}"
        );
    }

    #[tokio::test]
    async fn final_fixer_rejects_when_reviewer_only_failed() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-fail-only").await;
        seed_task_gate_passed(&db, parent, &wf_id).await;

        // Final reviewer terminal Failed (not request_changes/block).
        let key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let c = child_for(&db, AgentType::Codex).await;
        let task_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00f1";
        insert_completed_run_with_binding(
            &db,
            parent,
            c,
            task_id,
            &wf_id,
            "final-reviewer",
            &key,
            "codex",
            r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"x"}"#,
            true,
        )
        .await;
        // Force Failed status (failed alone must not open fix cycle).
        let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut am: delegation_task_run::ActiveModel = run.into();
        am.status = Set(DelegationRunStatus::Failed);
        am.update(&db.conn).await.unwrap();

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Grok).await;
        let fixer_key = build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00f2",
                "grok",
                Some(&fixer_key),
                None,
            ))
            .await
            .expect_err("failed alone");
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. }
                    if code == "final_fixer_before_non_pass"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn newer_nonterminal_blocks_older_terminal_evidence() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-nonterm").await;
        seed_task_gate_passed(&db, parent, &wf_id).await;

        // Insert a newer reserving implementer run binding (higher lineage ordinal).
        let impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let child = child_for(&db, AgentType::Grok).await;
        let newer = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00d1";
        let now = Utc::now();
        let run = delegation_task_run::ActiveModel {
            task_id: Set(newer.into()),
            root_task_id: Set(newer.into()),
            previous_task_id: Set(None),
            generation: Set(2),
            parent_conversation_id: Set(parent),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set("grok".into()),
            profile_id: Set(None),
            workspace_path: Set(Some("/tmp".into())),
            route_fingerprint: Set(Some("rf".into())),
            launch_snapshot_version: Set(Some("v1".into())),
            mode_id: Set(None),
            config_values_json: Set(Some("{}".into())),
            task_preview: Set(None),
            request_fingerprint: Set(None),
            admission_class: Set(DbAdmissionClass::NormalRevision),
            reached_running_at: Set(None),
            lineage_root_task_id: Set(newer.into()),
            work_unit_key: Set(Some(impl_key)),
            legacy_parent_tool_use_id: Set(None),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Running),
            error_code: Set(None),
            termination_audit_json: Set(None),
            started_at: Set(Some(now)),
            finished_at: Set(None),
            tool_call_count: Set(Some(0)),
            edit_tool_call_count: Set(Some(0)),
            touched_files_json: Set(Some("[]".into())),
            touched_files_truncated: Set(Some(false)),
            additions: Set(None),
            deletions: Set(None),
            line_counts_complete: Set(Some(false)),
            card_summary_json: Set(None),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(None),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        run.insert(&db.conn).await.unwrap();
        let max = delegation_workflow_run_binding::Entity::find()
            .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(wf_id.clone()))
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
            .one(&db.conn)
            .await
            .unwrap()
            .map(|r| r.lineage_ordinal)
            .unwrap_or(0);
        let rb = delegation_workflow_run_binding::ActiveModel {
            task_id: Set(newer.into()),
            workflow_id: Set(wf_id.clone()),
            node_id: Set("task-1-impl".into()),
            gate_id: Set(None),
            gate_cycle: Set(None),
            manifest_revision: Set(1),
            content_fingerprint: Set(None),
            artifact_digest: Set(None),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(max + 10),
            summary_validated: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        rb.insert(&db.conn).await.unwrap();

        // Final first-pass must fail (task gate no longer ready).
        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child2 = child_for(&db, AgentType::Codex).await;
        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let err = store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child2,
                "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00d2",
                "codex",
                Some(&final_key),
                None,
            ))
            .await
            .expect_err("nonterminal blocks");
        assert!(
            matches!(
                err,
                TaskStoreError::WorkflowAdmission { ref code, .. } if code == "final_early"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn final_first_pass_stamps_branch_tip_digest() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-tip").await;
        seed_task_gate_passed(&db, parent, &wf_id).await;

        let store = RunStore::new(Arc::new(AppDatabase {
            conn: db.conn.clone(),
        }))
        .with_workflow_emitter(emitter);
        let child = child_for(&db, AgentType::Codex).await;
        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let task_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00e1";
        store
            .admit_gen1_reserving(gen1_insert(
                parent,
                child,
                task_id,
                "codex",
                Some(&final_key),
                None,
            ))
            .await
            .expect("final first-pass after tasks pass");
        let rb = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .expect("run binding");
        assert_eq!(
            rb.artifact_digest.as_deref(),
            Some("deadbeef"),
            "first-pass Final must stamp branch tip from Task implementer"
        );
        assert!(rb.reviewed_task_id.is_none());
    }

    #[test]
    fn implementer_digest_no_card_summary_fallback_when_head_missing() {
        // B3: without a real git repo, HEAD is unavailable → empty (not card SHA).
        assert!(workspace_head_commit(Some("/no/such/workspace")).is_none());
        assert!(workspace_head_commit(None).is_none());
        assert!(workspace_head_commit(Some("")).is_none());
    }

    async fn seed_task_gate_passed(db: &AppDatabase, parent: i32, wf_id: &str) {
        let impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let rev_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let c1 = child_for(db, AgentType::Grok).await;
        let c2 = child_for(db, AgentType::Codex).await;
        let impl_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00a1";
        let rev_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00a2";
        insert_completed_run_with_binding(
            db,
            parent,
            c1,
            impl_id,
            wf_id,
            "task-1-impl",
            &impl_key,
            "grok",
            r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"ok","commits":[{"sha":"deadbeef","subject":"x"}]}"#,
            true,
        )
        .await;
        // Patch reviewed fields on reviewer binding after insert.
        insert_completed_run_with_binding(
            db,
            parent,
            c2,
            rev_id,
            wf_id,
            "task-1-rev",
            &rev_key,
            "codex",
            r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"ok"}"#,
            true,
        )
        .await;
        let rb = delegation_workflow_run_binding::Entity::find_by_id(rev_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut am: delegation_workflow_run_binding::ActiveModel = rb.into();
        am.reviewed_task_id = Set(Some(impl_id.into()));
        am.reviewed_implementer_generation = Set(Some(1));
        am.artifact_digest = Set(Some("deadbeef".into()));
        am.update(&db.conn).await.unwrap();
        let irb = delegation_workflow_run_binding::Entity::find_by_id(impl_id.to_string())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut iam: delegation_workflow_run_binding::ActiveModel = irb.into();
        iam.artifact_digest = Set(Some("deadbeef".into()));
        iam.update(&db.conn).await.unwrap();
    }

    async fn seed_final_reviewer_non_pass(db: &AppDatabase, parent: i32, wf_id: &str) {
        let key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let c = child_for(db, AgentType::Codex).await;
        insert_completed_run_with_binding(
            db,
            parent,
            c,
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00b1",
            wf_id,
            "final-reviewer",
            &key,
            "codex",
            r#"{"kind":"review","verdict":"request_changes","critical":1,"important":0,"minor":0,"summary":"fix"}"#,
            true,
        )
        .await;
    }

    async fn seed_final_fixer_non_pass(db: &AppDatabase, parent: i32, wf_id: &str) {
        let key = build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let c = child_for(db, AgentType::Grok).await;
        insert_completed_run_with_binding(
            db,
            parent,
            c,
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeee00c1",
            wf_id,
            "final-fixer",
            &key,
            "grok",
            r#"{"kind":"implementation","phase":"fix","status":"blocked","summary":"stuck"}"#,
            true,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_completed_run_with_binding(
        db: &AppDatabase,
        parent: i32,
        child: i32,
        task_id: &str,
        wf_id: &str,
        node_id: &str,
        key: &str,
        agent: &str,
        summary_json: &str,
        summary_validated: bool,
    ) {
        let now = Utc::now();
        let run = delegation_task_run::ActiveModel {
            task_id: Set(task_id.to_string()),
            root_task_id: Set(task_id.to_string()),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(parent),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set(agent.into()),
            profile_id: Set(None),
            workspace_path: Set(Some("/tmp".into())),
            route_fingerprint: Set(Some("rf".into())),
            launch_snapshot_version: Set(Some("v1".into())),
            mode_id: Set(None),
            config_values_json: Set(Some("{}".into())),
            task_preview: Set(None),
            request_fingerprint: Set(None),
            admission_class: Set(DbAdmissionClass::NormalRevision),
            reached_running_at: Set(Some(now)),
            lineage_root_task_id: Set(task_id.to_string()),
            work_unit_key: Set(Some(key.into())),
            legacy_parent_tool_use_id: Set(None),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            error_code: Set(None),
            termination_audit_json: Set(None),
            started_at: Set(Some(now)),
            finished_at: Set(Some(now)),
            tool_call_count: Set(Some(0)),
            edit_tool_call_count: Set(Some(0)),
            touched_files_json: Set(Some("[]".into())),
            touched_files_truncated: Set(Some(false)),
            additions: Set(None),
            deletions: Set(None),
            line_counts_complete: Set(Some(false)),
            card_summary_json: Set(Some(summary_json.into())),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(None),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        run.insert(&db.conn).await.expect("run");

        // lineage ordinal: max+1
        let max = delegation_workflow_run_binding::Entity::find()
            .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(wf_id.to_string()))
            .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
            .one(&db.conn)
            .await
            .unwrap()
            .map(|r| r.lineage_ordinal)
            .unwrap_or(0);

        let rb = delegation_workflow_run_binding::ActiveModel {
            task_id: Set(task_id.to_string()),
            workflow_id: Set(wf_id.to_string()),
            node_id: Set(node_id.to_string()),
            gate_id: Set(None),
            gate_cycle: Set(None),
            manifest_revision: Set(1),
            content_fingerprint: Set(None),
            artifact_digest: Set(None),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(max + 1),
            summary_validated: Set(summary_validated),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        rb.insert(&db.conn).await.expect("rb");

        // Observe node.
        if let Some(n) = delegation_workflow_node_binding::Entity::find_by_id((
            wf_id.to_string(),
            node_id.to_string(),
        ))
        .one(&db.conn)
        .await
        .unwrap()
        {
            let mut am: delegation_workflow_node_binding::ActiveModel = n.into();
            am.is_observed = Set(true);
            am.updated_at = Set(now);
            am.update(&db.conn).await.unwrap();
        }
    }
}
