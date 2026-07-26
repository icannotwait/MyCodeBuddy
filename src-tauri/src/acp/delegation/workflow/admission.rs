//! Workflow run admission enforcement + graph-revision hooks (Task 6).
//!
//! - B2/A14: active vs retained-observed binding; role/agent/profile match
//! - A8.3: block **new** Task first-dispatch while Plan re-open / not approved
//! - B6: Final reviewer / fixer / re-review readiness via `evaluate_execution_gate`
//! - B14: first Task-pair admission freezes both partners
//! - B5/A10: run_binding + graph_revision same transaction; post-commit emit
//! - A1 key without manifest: compatibility_nudge only
//! - Non-workflow / non-A1 key: no workflow write

use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
};

use crate::acp::delegation::card_summary::{
    parse_and_validate_summary_json, CardSummary, ReviewVerdict, WorkStatus,
};
use crate::acp::delegation::store::TaskStoreError;
use crate::db::entities::delegation_task_run::{self, DelegationRunStatus};
use crate::db::entities::delegation_workflow::{self, WorkflowState};
use crate::db::entities::delegation_workflow_gate_settlement::{self, GateSettlementOutcome};
use crate::db::entities::delegation_workflow_manifest_revision;
use crate::db::entities::delegation_workflow_node_binding;
use crate::db::entities::delegation_workflow_run_binding;
use crate::web::event_bridge::EventEmitter;

use super::events::{emit_workflow_compatibility_nudge, emit_workflow_graph_changed};
use super::gates::{
    evaluate_execution_gate, ExecutionGateInput, ExecutionGateKind, ExecutionGateRunEvidence,
    TerminalRunStatus,
};
use super::key::parse_recognized_work_unit_key;
use super::project::evidence_from_run_and_binding;
use super::types::{
    DocumentGateKind, ManifestDocument, ParsedWorkUnitKey, PHASE_FINAL, PHASE_TASKS,
    WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use super::validate::validate_manifest_document;

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
    pub task_id: &'a str,
    pub work_unit_key: Option<&'a str>,
    pub agent_type: &'a str,
    pub profile_id: Option<&'a str>,
    pub lineage_root_task_id: &'a str,
    pub generation: i64,
    pub kind: AdmissionDispatchKind,
    /// Workspace path of the admitting run (for Final first-pass tip fallback).
    pub workspace_path: Option<&'a str>,
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

fn map_db(e: sea_orm::DbErr) -> TaskStoreError {
    TaskStoreError::Permanent(format!("workflow admission db: {e}"))
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
            format!("work_unit_key {key} is not bound on workflow {}", header.workflow_id),
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

    // A8.3 / B6 readiness for Task and Final (first-dispatch and Final re-entry).
    enforce_phase_readiness(conn, &header, &binding, &parsed, input.kind).await?;

    let now = Utc::now();
    let lineage_ordinal =
        next_lineage_ordinal(conn, &header.workflow_id, input.lineage_root_task_id).await?;

    let (gate_id, gate_cycle, artifact_digest, reviewed_task_id, reviewed_impl_gen) =
        stamp_admission_fields(conn, &header, &binding, &parsed, input.workspace_path).await?;

    let rb = delegation_workflow_run_binding::ActiveModel {
        task_id: Set(input.task_id.to_string()),
        workflow_id: Set(header.workflow_id.clone()),
        node_id: Set(binding.node_id.clone()),
        gate_id: Set(gate_id),
        gate_cycle: Set(gate_cycle),
        manifest_revision: Set(header.active_manifest_revision),
        artifact_digest: Set(artifact_digest),
        reviewed_task_id: Set(reviewed_task_id),
        reviewed_implementer_generation: Set(reviewed_impl_gen),
        lineage_ordinal: Set(lineage_ordinal),
        summary_validated: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
    };
    rb.insert(conn).await.map_err(map_db)?;

    // Mark node observed + B14 pair freeze when first Task partner admits.
    mark_observed_and_maybe_freeze_pair(conn, &header.workflow_id, &binding, now).await?;

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
/// **Implementer / Final-fixer artifact digest priority (B3):**
/// 1. Workspace `HEAD` commit id (`git rev-parse HEAD` in `workspace_path`) when available
/// 2. First commit SHA from a validated card-summary `Implementation` block (secondary)
/// 3. Leave empty when neither is available (generation-only coverage)
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
    let validated = card_summary_json
        .and_then(parse_and_validate_summary_json)
        .is_some();

    let mut am: delegation_workflow_run_binding::ActiveModel = rb.clone().into();
    am.summary_validated = Set(validated);
    am.updated_at = Set(now);

    if matches!(
        run_status,
        DelegationRunStatus::Completed | DelegationRunStatus::Failed | DelegationRunStatus::Canceled
    ) && rb.artifact_digest.is_none()
    {
        if let Some(digest) = resolve_implementer_artifact_digest(workspace_path, card_summary_json)
        {
            am.artifact_digest = Set(Some(digest));
        }
    }

    am.update(conn).await.map_err(map_db)?;

    let next_rev = bump_graph_revision(conn, &rb.workflow_id, now).await?;
    Ok(WorkflowTxnSideEffect::GraphChanged {
        parent_conversation_id,
        workflow_id: rb.workflow_id,
        graph_revision: next_rev,
    })
}

/// Prefer workspace HEAD; fall back to card-summary first commit SHA.
fn resolve_implementer_artifact_digest(
    workspace_path: Option<&str>,
    card_summary_json: Option<&str>,
) -> Option<String> {
    if let Some(head) = workspace_head_commit(workspace_path) {
        return Some(head);
    }
    if let Some(CardSummary::Implementation { commits, .. }) =
        card_summary_json.and_then(parse_and_validate_summary_json)
    {
        if let Some(first) = commits.first() {
            let sha = first.sha.trim();
            if !sha.is_empty() {
                return Some(sha.to_string());
            }
        }
    }
    None
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
    let run_profile = profile_id.filter(|s| !s.is_empty());
    if bind_profile != run_profile {
        return Err(admission_err(
            "workflow_profile_mismatch",
            format!(
                "profile_id {run_profile:?} does not match binding {bind_profile:?}"
            ),
        ));
    }

    // Role from key vs binding.
    let (expected_role, expected_phase) = match parsed {
        ParsedWorkUnitKey::Design { .. } => ("reviewer", "design"),
        ParsedWorkUnitKey::Plan { .. } => ("reviewer", "plan"),
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

async fn enforce_phase_readiness<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    binding: &delegation_workflow_node_binding::Model,
    parsed: &ParsedWorkUnitKey,
    kind: AdmissionDispatchKind,
) -> Result<(), TaskStoreError> {
    match parsed {
        // Document reviewers: only published Design/Plan nodes (already bound).
        ParsedWorkUnitKey::Design { .. } | ParsedWorkUnitKey::Plan { .. } => Ok(()),

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
            enforce_final_reviewer_readiness(conn, header, kind).await
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
        return Err(admission_err(
            "workflow_blocked",
            "workflow is blocked; new Task admissions rejected",
        ));
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
) -> Result<(), TaskStoreError> {
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

    // B6: Final **re-review continue** only after Final fixer terminal pass for
    // the current cycle. No continue when no fixer (or fixer not terminal pass).
    if matches!(kind, AdmissionDispatchKind::ContinueOrReplacement) {
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
    // B6: Final fixer only after Final reviewer terminal request_changes / block.
    // Failed/canceled alone does **not** open a fix cycle.
    let rev = load_latest_final_reviewer_evidence(conn, header).await?;
    let Some(rev) = rev else {
        return Err(admission_err(
            "final_fixer_before_non_pass",
            "Final fixer blocked: no Final reviewer terminal yet",
        ));
    };
    if !reviewer_is_request_changes_or_block(&rev) {
        return Err(admission_err(
            "final_fixer_before_non_pass",
            "Final fixer blocked: Final reviewer has not terminal request_changes/block",
        ));
    }
    Ok(())
}

/// B6: only completed + validated `request_changes` / `block` open a Final fix cycle.
fn reviewer_is_request_changes_or_block(ev: &ExecutionGateRunEvidence) -> bool {
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
    let impl_ev = load_latest_role_evidence(
        conn,
        header,
        PHASE_TASKS,
        "implementer",
        Some(task_index as i64),
    )
    .await?;
    let rev_ev =
        load_latest_role_evidence(conn, header, PHASE_TASKS, "reviewer", Some(task_index as i64))
            .await?;
    Ok(evaluate_execution_gate(&ExecutionGateInput {
        kind: ExecutionGateKind::Task,
        implementer_or_fixer: impl_ev,
        reviewer: rev_ev,
        branch_tip_digest: None,
    }))
}

async fn active_task_indices<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
) -> Result<Vec<u32>, TaskStoreError> {
    let rows = delegation_workflow_node_binding::Entity::find()
        .filter(
            delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
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
        .filter(
            delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .filter(delegation_workflow_node_binding::Column::PhaseId.eq(phase.to_string()))
        .filter(delegation_workflow_node_binding::Column::Role.eq(role.to_string()));
    if let Some(idx) = task_index {
        q = q.filter(delegation_workflow_node_binding::Column::TaskIndex.eq(idx));
    }
    let binding = q.one(conn).await.map_err(map_db)?;
    let Some(binding) = binding else {
        return Ok(None);
    };

    let rbs = delegation_workflow_run_binding::Entity::find()
        .filter(
            delegation_workflow_run_binding::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(binding.node_id.clone()))
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
    ),
    TaskStoreError,
> {
    match parsed {
        ParsedWorkUnitKey::Design { .. } | ParsedWorkUnitKey::Plan { .. } => {
            let (gate_id, cycle, digest) =
                document_gate_stamp(conn, header, binding, parsed).await?;
            Ok((gate_id, cycle, digest, None, None))
        }
        ParsedWorkUnitKey::TaskReviewer { task_index, .. } => {
            // Copy implementer binding digests / task_id for B3/B13.
            let impl_pair =
                load_latest_implementer_binding(conn, header, *task_index as i64).await?;
            let (reviewed_task_id, reviewed_gen, digest) = match impl_pair {
                Some((run, rb)) => (
                    Some(run.task_id),
                    Some(run.generation),
                    rb.artifact_digest,
                ),
                None => (None, None, None),
            };
            Ok((None, None, digest, reviewed_task_id, reviewed_gen))
        }
        ParsedWorkUnitKey::FinalReviewer { .. } => {
            // Prefer covering latest fixer if present; else first-pass:
            // stamp branch tip digest (same digest Final gate needs) or workspace HEAD.
            if let Some((run, rb)) = load_latest_fixer_binding(conn, header).await? {
                Ok((
                    None,
                    None,
                    rb.artifact_digest,
                    Some(run.task_id),
                    Some(run.generation),
                ))
            } else {
                let tip = derive_admission_branch_tip_digest(conn, header)
                    .await?
                    .or_else(|| workspace_head_commit(workspace_path));
                Ok((None, None, tip, None, None))
            }
        }
        ParsedWorkUnitKey::TaskImplementer { .. } | ParsedWorkUnitKey::FinalFixer { .. } => {
            Ok((None, None, None, None, None))
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
        ParsedWorkUnitKey::Plan { .. } => DocumentGateKind::Plan,
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
    load_latest_role_run_binding(conn, header, PHASE_TASKS, "implementer", Some(task_index)).await
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
    load_latest_role_run_binding(conn, header, PHASE_FINAL, "fixer", None).await
}

async fn load_latest_role_run_binding<C: ConnectionTrait>(
    conn: &C,
    header: &delegation_workflow::Model,
    phase: &str,
    role: &str,
    task_index: Option<i64>,
) -> Result<
    Option<(
        delegation_task_run::Model,
        delegation_workflow_run_binding::Model,
    )>,
    TaskStoreError,
> {
    let mut q = delegation_workflow_node_binding::Entity::find()
        .filter(
            delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .filter(delegation_workflow_node_binding::Column::PhaseId.eq(phase.to_string()))
        .filter(delegation_workflow_node_binding::Column::Role.eq(role.to_string()));
    if let Some(idx) = task_index {
        q = q.filter(delegation_workflow_node_binding::Column::TaskIndex.eq(idx));
    }
    let binding = q.one(conn).await.map_err(map_db)?;
    let Some(node) = binding else {
        return Ok(None);
    };
    let rbs = delegation_workflow_run_binding::Entity::find()
        .filter(
            delegation_workflow_run_binding::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(node.node_id))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .all(conn)
        .await
        .map_err(map_db)?;
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
        .filter(
            delegation_workflow::Column::ParentConversationId.eq(parent_conversation_id),
        )
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
        .filter(
            delegation_workflow_node_binding::Column::WorkUnitKey.eq(work_unit_key.to_string()),
        )
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

async fn mark_observed_and_maybe_freeze_pair<C: ConnectionTrait>(
    conn: &C,
    workflow_id: &str,
    binding: &delegation_workflow_node_binding::Model,
    now: chrono::DateTime<Utc>,
) -> Result<(), TaskStoreError> {
    let mut am: delegation_workflow_node_binding::ActiveModel = binding.clone().into();
    am.is_observed = Set(true);
    am.updated_at = Set(now);
    am.update(conn).await.map_err(map_db)?;

    // B14: Task implementer/reviewer pair — freeze both on first admission.
    let Some(task_index) = binding.task_index else {
        return Ok(());
    };
    if binding.phase_id != PHASE_TASKS {
        return Ok(());
    }
    if binding.role != "implementer" && binding.role != "reviewer" {
        return Ok(());
    }

    let partners = delegation_workflow_node_binding::Entity::find()
        .filter(delegation_workflow_node_binding::Column::WorkflowId.eq(workflow_id.to_string()))
        .filter(delegation_workflow_node_binding::Column::TaskIndex.eq(task_index))
        .filter(delegation_workflow_node_binding::Column::PhaseId.eq(PHASE_TASKS.to_string()))
        .all(conn)
        .await
        .map_err(map_db)?;

    for p in partners {
        if p.pair_frozen {
            continue;
        }
        let mut pam: delegation_workflow_node_binding::ActiveModel = p.into();
        pam.pair_frozen = Set(true);
        pam.updated_at = Set(now);
        pam.update(conn).await.map_err(map_db)?;
    }
    Ok(())
}

async fn bump_graph_revision<C: ConnectionTrait>(
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
    use crate::acp::delegation::run_store::{Gen1AdmitOutcome, ReservingRunInsert, RunStore};
    use crate::acp::delegation::workflow::events::{
        WORKFLOW_GRAPH_CHANGED_EVENT, WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
    };
    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::store::{
        publish_workflow_manifest_core, PublishWorkflowRequest,
    };
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestEdge, ManifestGate, ManifestNode, ManifestNodeKind,
        ManifestNodeRole, ManifestPhase, ManifestWorkflowState, ResolutionMode, WorkUnitKeyParts,
        PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use crate::db::entities::delegation_task_run::AdmissionClass as DbAdmissionClass;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::agent::AgentType;
    use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};
    use sea_orm::{QueryOrder, Set, TransactionTrait};
    use std::sync::Arc;

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
        let plan_key = build_work_unit_key(&WorkUnitKeyParts::Plan {
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
            schema_version: 1,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.to_string(),
            workflow_id: None,
            expected_manifest_revision: None,
            publication_token: token.into(),
            workflow_state: state,
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
            ],
            edges: vec![ManifestEdge {
                id: Some("e1".into()),
                from: "task-1-impl".into(),
                to: "task-1-rev".into(),
            }],
            gates: vec![
                ManifestGate {
                    id: "design".into(),
                    required_reviewer_node_ids: vec!["design-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some(DocumentGateKind::Design),
                },
                ManifestGate {
                    id: "plan".into(),
                    required_reviewer_node_ids: vec!["plan-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some(DocumentGateKind::Plan),
                },
            ],
        }
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
            PublishWorkflowRequest { document: doc.clone() },
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
        seed_gate_settlement(db, &pub_r.workflow_id, "design", 1, GateSettlementOutcome::Approved)
            .await;
        seed_gate_settlement(db, &pub_r.workflow_id, "plan", 1, GateSettlementOutcome::Approved)
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
        (pub2.workflow_id, pub2.graph_revision)
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
        let row = delegation_workflow_gate_settlement::ActiveModel {
            workflow_id: Set(workflow_id.to_string()),
            gate_id: Set(gate_id.to_string()),
            gate_cycle: Set(cycle),
            manifest_revision: Set(1),
            outcome: Set(outcome.clone()),
            critical_count: Set(0),
            important_count: Set(0),
            minor_count: Set(0),
            summary: Set("ok".into()),
            graph_revision_at_settle: Set(1),
            created_at: Set(now),
        };
        row.insert(&db.conn).await.expect("seed settlement");
        // Keep header approved when seeding plan approved.
        if gate_id == "plan" && matches!(outcome, GateSettlementOutcome::Approved) {
            let header = delegation_workflow::Entity::find_by_id(workflow_id.to_string())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
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
        let folder = seed_folder(db, &format!("/tmp/child-{}", UuidLike::new())).await;
        seed_conversation(db, folder, agent).await
    }

    /// Tiny uuid-like counter for unique paths in tests.
    struct UuidLike;
    impl UuidLike {
        fn new() -> String {
            use std::sync::atomic::{AtomicU64, Ordering};
            static C: AtomicU64 = AtomicU64::new(1);
            format!("{:x}", C.fetch_add(1, Ordering::SeqCst))
        }
    }

    #[tokio::test]
    async fn wrong_key_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_approved(&db, &emitter, parent, "tok-wrong-key").await;

        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
        assert!(bindings.is_empty(), "no durable run_binding without manifest");
    }

    #[tokio::test]
    async fn final_early_reject() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        publish_approved(&db, &emitter, parent, "tok-final-early").await;
        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
    async fn b14_pair_freeze_and_unstarted_reviewer_admittable() {
        let (db, parent) = seed_parent().await;
        let (emitter, mut rx) = emitter_with_rx();
        // Drain publish events.
        let _ = publish_approved(&db, &emitter, parent, "tok-b14").await;
        while rx.try_recv().is_ok() {}

        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter.clone());
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

        // Both pair nodes frozen.
        let nodes = delegation_workflow_node_binding::Entity::find()
            .filter(delegation_workflow_node_binding::Column::TaskIndex.eq(1_i64))
            .all(&db.conn)
            .await
            .unwrap();
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|n| n.pair_frozen));

        // Unstarted reviewer still admit-able (B14 / B10).
        let child2 = child_for(&db, AgentType::Codex).await;
        let rev_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
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
            .expect("reviewer admit after impl freeze");
    }

    #[tokio::test]
    async fn continue_retained_observed_after_plan_revision() {
        let (db, parent) = seed_parent().await;
        let (emitter, _) = emitter_with_rx();
        let (wf_id, _) = publish_approved(&db, &emitter, parent, "tok-ret").await;
        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter.clone());

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
        nam.pair_frozen = Set(true);
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
        let cont_child = child_for(&db, AgentType::Grok).await;
        // Insert a reserving run row then admit workflow as ContinueOrReplacement.
        let insert = gen1_insert(
            parent,
            cont_child,
            cont_task,
            "grok",
            Some(&impl_key),
            None,
        );
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
                        created_at: Set(now),
                        updated_at: Set(now),
                    };
                    model.insert(txn).await.map_err(map_db)?;
                    admit_workflow_run_txn(
                        txn,
                        &WorkflowAdmitInput {
                            parent_conversation_id: parent,
                            task_id: cont_task,
                            work_unit_key: Some(&impl_key),
                            agent_type: "grok",
                            profile_id: None,
                            lineage_root_task_id: task_id,
                            generation: 2,
                            kind: AdmissionDispatchKind::ContinueOrReplacement,
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

        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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

        let abandoned = store.abandon_reserving_claim(task_id).await.expect("abandon");
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
        let store = RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
                        created_at: Set(now),
                        updated_at: Set(now),
                    };
                    model.insert(txn).await.map_err(map_db)?;
                    admit_workflow_run_txn(
                        txn,
                        &WorkflowAdmitInput {
                            parent_conversation_id: parent,
                            task_id: cont_task,
                            work_unit_key: Some(&final_key),
                            agent_type: "codex",
                            profile_id: None,
                            lineage_root_task_id: cont_task,
                            generation: 2,
                            kind: AdmissionDispatchKind::ContinueOrReplacement,
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
                        created_at: Set(now),
                        updated_at: Set(now),
                    };
                    model.insert(txn).await.map_err(map_db)?;
                    admit_workflow_run_txn(
                        txn,
                        &WorkflowAdmitInput {
                            parent_conversation_id: parent,
                            task_id: cont_task,
                            work_unit_key: Some(&final_key),
                            agent_type: "codex",
                            profile_id: None,
                            lineage_root_task_id: cont_task,
                            generation: 2,
                            kind: AdmissionDispatchKind::ContinueOrReplacement,
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

        let store =
            RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
            created_at: Set(now),
            updated_at: Set(now),
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
            artifact_digest: Set(None),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(max + 10),
            summary_validated: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
        };
        rb.insert(&db.conn).await.unwrap();

        // Final first-pass must fail (task gate no longer ready).
        let store =
            RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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

        let store =
            RunStore::new(Arc::new(AppDatabase { conn: db.conn.clone() })).with_workflow_emitter(emitter);
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
    fn implementer_digest_prefers_workspace_head_over_card_summary() {
        // Without a real git repo, HEAD is unavailable → falls back to card summary.
        let card = r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"ok","commits":[{"sha":"from-card","subject":"x"}]}"#;
        let from_card = resolve_implementer_artifact_digest(Some("/no/such/workspace"), Some(card));
        assert_eq!(from_card.as_deref(), Some("from-card"));

        // Empty workspace + no card → None.
        assert!(resolve_implementer_artifact_digest(None, None).is_none());
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
            created_at: Set(now),
            updated_at: Set(now),
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
            artifact_digest: Set(None),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(max + 1),
            summary_validated: Set(summary_validated),
            created_at: Set(now),
            updated_at: Set(now),
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
