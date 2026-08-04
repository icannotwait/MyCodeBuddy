//! Redacted graph projection for frontend / conversation detail (Task 4).
//!
//! Overlays durable runs and gate settlements on the active manifest, or
//! synthesizes an observed-only graph from recognized A1 keys (A11). Never
//! fails conversation detail: corrupt manifests and projection errors omit the
//! graph (`None`) with a warn log.

use std::collections::{HashMap, HashSet};

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::acp::delegation::card_summary::{
    parse_and_validate_summary_json, CardSummary, ReviewVerdict, WorkStatus,
};
use crate::db::entities::delegation_task_run::{self, DelegationRunStatus};
use crate::db::entities::delegation_workflow::{self, WorkflowState};
use crate::db::entities::delegation_workflow_gate_settlement::{self, GateSettlementOutcome};
use crate::db::entities::delegation_workflow_manifest_revision;
use crate::db::entities::delegation_workflow_node_binding::{self, NodeOutcome};
use crate::db::entities::delegation_workflow_run_binding;
use crate::db::AppDatabase;

use super::dto::{
    redact_display_string, redact_optional_display, safe_public_id, sha256_hex_str,
    ProjectedNodeStatus, PublicIdAllocator, WorkflowCompatibility, WorkflowEdgeSnapshot,
    WorkflowGateSnapshot, WorkflowGraphSnapshot, WorkflowNodeSnapshot, WorkflowOverallState,
    WorkflowPhaseSnapshot, WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
};
use super::gates::{
    evaluate_execution_gate, ExecutionGateEval, ExecutionGateInput, ExecutionGateKind,
    ExecutionGateReason, ExecutionGateRunEvidence, RequiredReviewerEvidence, TerminalRunStatus,
};
use super::key::parse_recognized_work_unit_key;
use super::types::{
    ManifestDocument, ManifestNodeKind, ManifestNodeRole, ManifestTaskPolicy,
    ManifestWorkflowState, ParsedWorkUnitKey, ResolutionMode, TaskHardTriggerKind, TaskRiskLevel,
    TaskSoftSignalKind, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use super::validate::validate_manifest_document;

/// Pull display-safe model / effort from a run's allowlisted launch snapshot.
///
/// Keys match `launch_snapshot::ALLOWLISTED_CONFIG_KEYS`. Values are short
/// opaque ids (no secrets); empty / missing JSON yields `None`.
fn model_and_effort_from_config_json(
    config_values_json: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(raw) = config_values_json.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, None);
    };
    let Ok(map) = serde_json::from_str::<std::collections::BTreeMap<String, String>>(raw) else {
        return (None, None);
    };
    let lookup = |keys: &[&str]| -> Option<String> {
        for key in keys {
            if let Some(value) = map
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v.trim())
                .filter(|v| !v.is_empty())
            {
                return Some(value.to_string());
            }
        }
        None
    };
    (
        lookup(&["model", "model_id", "modelId"]),
        // Prefer Grok's ACP id first; then Codex-style / generic keys.
        lookup(&[
            "reasoning_effort",
            "effort",
            "reasoning",
            "thinking",
        ]),
    )
}

/// Card operational rollup from a durable run (elapsed / tools / edits).
/// Paths are never projected — only counts + line totals.
#[derive(Debug, Clone, Default)]
struct RunCardRuntime {
    started_at: Option<String>,
    finished_at: Option<String>,
    tool_call_count: Option<u64>,
    edit_tool_call_count: Option<u64>,
    touched_file_count: Option<u64>,
    touched_files_truncated: bool,
    additions: Option<i64>,
    deletions: Option<i64>,
    line_counts_complete: Option<bool>,
}

fn run_card_runtime(run: &delegation_task_run::Model) -> RunCardRuntime {
    let touched_file_count = run
        .touched_files_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(json).ok())
        .map(|files| files.len() as u64);
    RunCardRuntime {
        started_at: run.started_at.map(|t| t.to_rfc3339()),
        finished_at: run.finished_at.map(|t| t.to_rfc3339()),
        tool_call_count: run.tool_call_count.and_then(|c| u64::try_from(c).ok()),
        edit_tool_call_count: run
            .edit_tool_call_count
            .and_then(|c| u64::try_from(c).ok()),
        touched_file_count,
        touched_files_truncated: run.touched_files_truncated.unwrap_or(false),
        additions: run.additions,
        deletions: run.deletions,
        line_counts_complete: run.line_counts_complete,
    }
}

fn empty_run_card_runtime() -> RunCardRuntime {
    RunCardRuntime::default()
}

/// Duration of one finished run in ms. Unfinished / incomplete timestamps → None.
fn finished_run_duration_ms(run: &delegation_task_run::Model) -> Option<u64> {
    let started = run.started_at?;
    let finished = run.finished_at?;
    if finished < started {
        return None;
    }
    let ms = (finished - started).num_milliseconds();
    if ms < 0 {
        None
    } else {
        Some(ms as u64)
    }
}

/// Sum finished-run durations for a work unit. When `latest` is still in flight
/// it is excluded so the card can add live `now - latest.started_at`.
fn sum_elapsed_completed_ms(
    runs: &[&delegation_task_run::Model],
    latest: Option<&delegation_task_run::Model>,
) -> Option<u64> {
    let latest_id = latest.map(|r| r.task_id.as_str());
    let latest_open = latest.is_some_and(|r| r.finished_at.is_none());
    let mut total: u64 = 0;
    let mut any = false;
    for run in runs {
        if latest_open && latest_id == Some(run.task_id.as_str()) {
            continue;
        }
        if let Some(ms) = finished_run_duration_ms(run) {
            total = total.saturating_add(ms);
            any = true;
        }
    }
    any.then_some(total)
}

fn non_empty_trimmed(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn title_missing(title: &Option<String>) -> bool {
    title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_none()
}

/// First non-empty display-safe title from ordered candidates.
///
/// Trims each candidate, then uses [`redact_optional_display`] so path/fence
/// scrubbing that yields an empty string falls through to the next source
/// (instead of locking in `Some("")`). Pure `[redacted]` placeholders also fall
/// through so a path-only preview does not block a usable summary / child title.
fn first_display_title<'a>(candidates: impl IntoIterator<Item = Option<&'a str>>) -> Option<String> {
    let mut last_redacted: Option<String> = None;
    for candidate in candidates {
        let trimmed = candidate.map(str::trim).filter(|s| !s.is_empty());
        if let Some(t) = redact_optional_display(trimmed) {
            if t.trim() == "[redacted]" {
                last_redacted = Some(t);
                continue;
            }
            return Some(t);
        }
    }
    last_redacted
}

/// Card line-2 title for a bound/observed run lineage.
///
/// Manifest titles are often unset; prefer the durable launch `task_preview`,
/// then the validated card-summary text. Child conversation title is applied
/// later by [`enrich_nodes_display_from_children`] (and preferred when present).
fn project_run_title(
    manifest_title: Option<&str>,
    task_preview: Option<&str>,
    summary: Option<&str>,
) -> Option<String> {
    first_display_title([manifest_title, task_preview, summary])
}

/// Fill gaps in node `title` / `model` / `effort` from the child conversation
/// and, for Grok, a cheap `summary.json` peek (same source the turn footer uses).
///
/// Manifest titles are often empty; the card's second line needs the child
/// conversation title (or summary/task preview) to match what users see when
/// they open the session. When a child has a real session title, it wins over
/// a long task_preview (same title-first rule as sub-agent cards).
async fn enrich_nodes_display_from_children(
    conn: &sea_orm::DatabaseConnection,
    nodes: &mut [WorkflowNodeSnapshot],
) {
    let child_ids: Vec<i32> = nodes
        .iter()
        .filter_map(|n| n.latest_child_conversation_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if child_ids.is_empty() {
        return;
    }

    let rows = match crate::db::entities::conversation::Entity::find()
        .filter(crate::db::entities::conversation::Column::Id.is_in(child_ids.clone()))
        .all(conn)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(error = %e, "enrich display: conversation lookup failed");
            return;
        }
    };
    let by_id: HashMap<i32, crate::db::entities::conversation::Model> =
        rows.into_iter().map(|r| (r.id, r)).collect();

    // Optional first-user text while auto-title has not finalized yet.
    let job_rows = match crate::db::entities::auto_title_job::Entity::find()
        .filter(crate::db::entities::auto_title_job::Column::ConversationId.is_in(child_ids))
        .all(conn)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(error = %e, "enrich display: auto_title_job lookup failed");
            Vec::new()
        }
    };
    let first_user_by_cid: HashMap<i32, String> = job_rows
        .into_iter()
        .filter_map(|j| {
            non_empty_trimmed(j.first_user_text.as_deref()).map(|t| (j.conversation_id, t))
        })
        .collect();

    for node in nodes.iter_mut() {
        let needs_model = node.model.is_none();
        let Some(cid) = node.latest_child_conversation_id else {
            continue;
        };
        let Some(child) = by_id.get(&cid) else {
            continue;
        };

        // Prefer child session title when present (title-first, like sub-agent cards).
        if let Some(child_title) = first_display_title([child.title.as_deref()]) {
            node.title = Some(child_title);
        } else if title_missing(&node.title) {
            // Kickoff text captured for auto-title before conversation.title is set.
            if let Some(first_user) = first_user_by_cid.get(&cid) {
                // Cap so a multi-paragraph prompt does not flood the card line.
                let clipped: String = first_user.chars().take(200).collect();
                if let Some(t) = first_display_title([Some(clipped.as_str())]) {
                    node.title = Some(t);
                }
            }
        }

        if needs_model {
            node.model = non_empty_trimmed(child.model.as_deref());
        }

        let agent = child.agent_type.as_str();
        let needs_archive = title_missing(&node.title)
            || node.model.is_none()
            || node.effort.is_none();
        if needs_archive && agent.eq_ignore_ascii_case("grok") {
            if let Some(ext) = child
                .external_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let (arch_model, arch_effort) =
                    crate::parsers::grok::peek_session_model_and_effort(ext);
                if node.model.is_none() {
                    node.model = arch_model;
                }
                if node.effort.is_none() {
                    node.effort = arch_effort;
                }
            }
        }
    }
}

/// Project a redacted `WorkflowGraphSnapshot` for `parent_conversation_id`.
///
/// Returns `None` when there is no graph (no manifest and no recognized A1
/// keys), when the manifest is corrupt/unsupported, or on persistence errors.
/// Never panics; callers may attach the result to conversation detail safely.
pub async fn project_workflow_graph_core(
    db: &AppDatabase,
    parent_conversation_id: i32,
) -> Option<WorkflowGraphSnapshot> {
    soft_attach_workflow_graph(
        project_inner(&db.conn, parent_conversation_id).await,
        parent_conversation_id,
    )
}

/// Soft-fail attach for conversation detail: projection errors become `None`
/// (with warn) and never fail the detail response.
///
/// Used by `project_workflow_graph_core` and unit-tested here. Full
/// `get_folder_conversation_core` integration is intentionally not spun up in
/// this module (heavy parse/registry fixtures); soft-fail at the projector
/// boundary is the contract attachment relies on.
pub fn soft_attach_workflow_graph<E: std::fmt::Display>(
    projected: Result<Option<WorkflowGraphSnapshot>, E>,
    parent_conversation_id: i32,
) -> Option<WorkflowGraphSnapshot> {
    match projected {
        Ok(snapshot) => snapshot,
        Err(err) => {
            tracing::warn!(
                parent_conversation_id,
                error = %err,
                "workflow graph projection failed; omitting graph"
            );
            None
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ProjectError {
    #[error("persistence: {0}")]
    Persistence(String),
}

fn db_err(e: sea_orm::DbErr) -> ProjectError {
    ProjectError::Persistence(e.to_string())
}

async fn project_inner(
    conn: &sea_orm::DatabaseConnection,
    parent_conversation_id: i32,
) -> Result<Option<WorkflowGraphSnapshot>, ProjectError> {
    let header = delegation_workflow::Entity::find()
        .filter(delegation_workflow::Column::ParentConversationId.eq(parent_conversation_id))
        .filter(delegation_workflow::Column::WorkflowKind.eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY))
        .one(conn)
        .await
        .map_err(db_err)?;

    if let Some(header) = header {
        return project_manifest_mode(conn, &header).await;
    }

    // No durable header → observed-only from recognized A1 keys (A11/B7).
    project_observed_only(conn, parent_conversation_id).await
}

async fn project_manifest_mode(
    conn: &sea_orm::DatabaseConnection,
    header: &delegation_workflow::Model,
) -> Result<Option<WorkflowGraphSnapshot>, ProjectError> {
    let rev_row = delegation_workflow_manifest_revision::Entity::find_by_id((
        header.workflow_id.clone(),
        header.active_manifest_revision,
    ))
    .one(conn)
    .await
    .map_err(db_err)?;

    let Some(rev_row) = rev_row else {
        tracing::warn!(
            workflow_id = %header.workflow_id,
            revision = header.active_manifest_revision,
            "active manifest revision row missing; omitting graph"
        );
        return Ok(None);
    };

    let doc: ManifestDocument = match serde_json::from_str(&rev_row.document_json) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                workflow_id = %header.workflow_id,
                error = %e,
                "corrupt manifest json; omitting graph"
            );
            return Ok(None);
        }
    };

    let normalized = match validate_manifest_document(&doc) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                workflow_id = %header.workflow_id,
                error = %e,
                "unsupported/invalid manifest; omitting graph"
            );
            return Ok(None);
        }
    };

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

    let settlements = delegation_workflow_gate_settlement::Entity::find()
        .filter(
            delegation_workflow_gate_settlement::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .order_by_asc(delegation_workflow_gate_settlement::Column::GateCycle)
        .all(conn)
        .await
        .map_err(db_err)?;

    // All parent runs (bound + A9 orphan recognized keys without bindings).
    let parent_runs = delegation_task_run::Entity::find()
        .filter(delegation_task_run::Column::ParentConversationId.eq(header.parent_conversation_id))
        .all(conn)
        .await
        .map_err(db_err)?;
    let run_by_id: HashMap<String, &delegation_task_run::Model> =
        parent_runs.iter().map(|r| (r.task_id.clone(), r)).collect();

    // Group run bindings by node_id, ordered by lineage_ordinal desc already.
    let mut rbs_by_node: HashMap<String, Vec<&delegation_workflow_run_binding::Model>> =
        HashMap::new();
    for rb in &run_bindings {
        rbs_by_node.entry(rb.node_id.clone()).or_default().push(rb);
    }

    // Manifest node lookup for deps / titles / kinds.
    let manifest_node_by_id: HashMap<String, &super::types::NormalizedNode> =
        normalized.nodes.iter().map(|n| (n.id.clone(), n)).collect();

    let mut id_map = PublicIdAllocator::default();
    let mut nodes: Vec<WorkflowNodeSnapshot> = Vec::new();

    // Index parent runs by work_unit_key for A9 key-match without run_binding row.
    let mut runs_by_key: HashMap<String, Vec<&delegation_task_run::Model>> = HashMap::new();
    for r in &parent_runs {
        if let Some(k) = r.work_unit_key.as_deref() {
            runs_by_key.entry(k.to_string()).or_default().push(r);
        }
    }

    // Active + retained bindings drive the node set (plus any still on manifest).
    // Gate pairing only uses *active* candidates (non-retired, in-manifest or
    // cohort_frozen); retained_observed superseded history is projected but not paired.
    let mut seen_node_ids: HashSet<String> = HashSet::new();
    let mut bound_keys: HashSet<String> = HashSet::new();
    let mut gate_eligible_public: HashSet<String> = HashSet::new();
    let active_rev = header.active_manifest_revision;

    for b in &bindings {
        seen_node_ids.insert(b.node_id.clone());
        bound_keys.insert(b.work_unit_key.clone());
        let rbs = rbs_by_node
            .get(&b.node_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let mn = manifest_node_by_id.get(&b.node_id).copied();
        let in_manifest = mn.is_some();
        let key_runs = runs_by_key
            .get(&b.work_unit_key)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let snap = project_node_from_binding(b, mn, rbs, key_runs, &run_by_id, &mut id_map);
        if is_active_gate_binding(b, active_rev, in_manifest)
            && !matches!(snap.status, ProjectedNodeStatus::Superseded)
        {
            gate_eligible_public.insert(snap.node_id.clone());
        }
        nodes.push(snap);
    }

    // Manifest-only estimated nodes not yet in bindings (should be rare after publish).
    for mn in &normalized.nodes {
        if seen_node_ids.contains(&mn.id) {
            continue;
        }
        seen_node_ids.insert(mn.id.clone());
        if let Some(ref k) = mn.work_unit_key {
            bound_keys.insert(k.clone());
        }
        let snap = project_node_from_manifest_only(mn, &mut id_map);
        // In-manifest estimated work units are gate-eligible once present.
        if matches!(mn.kind, ManifestNodeKind::WorkUnit) {
            gate_eligible_public.insert(snap.node_id.clone());
        }
        nodes.push(snap);
    }

    // Build latest evidence + gate overlays on canonical nodes only (before orphans).
    let evidence_by_node = build_evidence_by_node(&nodes, &rbs_by_node, &run_by_id);
    let gate_summary = apply_execution_gate_overlays(
        &mut nodes,
        &evidence_by_node,
        &gate_eligible_public,
        &normalized.task_policies,
        &mut id_map,
    );

    ensure_author_before_plan_reviewers(&mut nodes);

    // A9 orphans: recognized keys with no binding — after pairing so they never
    // overwrite Task/Final pair candidates.
    append_orphan_observed_nodes(
        &mut nodes,
        &parent_runs,
        &bound_keys,
        &run_bindings,
        &mut id_map,
    );

    // Document gate snapshots: settlements/evidence use per-gate content
    // fingerprints; open cycle after non-approve does not re-count settled runs.
    let mut gate_snaps: Vec<WorkflowGateSnapshot> = Vec::new();
    for g in &normalized.gates {
        let gate_settlements: Vec<_> = settlements.iter().filter(|s| s.gate_id == g.id).collect();
        let current_fp = match g.gate_kind {
            super::types::DocumentGateKind::Design => header.design_fingerprint.as_str(),
            super::types::DocumentGateKind::Plan => header.plan_fingerprint.as_str(),
        };
        // Displayed settlement only when it covers current gate content fingerprint.
        let latest = gate_settlements
            .iter()
            .rfind(|s| !s.content_fingerprint.is_empty() && s.content_fingerprint == current_fp)
            .copied();
        let max_cycle = gate_settlements
            .iter()
            .map(|s| s.gate_cycle)
            .max()
            .unwrap_or(0);
        // Evidence cycle:
        // - approved settlement → that cycle's completed evidence
        // - changes_requested/blocked → open cycle (settled+1), empty until new runs
        // - no structure-matching settlement → open cycle
        let count_cycle = match latest {
            Some(s) if s.outcome == GateSettlementOutcome::Approved => s.gate_cycle,
            Some(s) => s.gate_cycle + 1,
            None => max_cycle + 1,
        };
        let expected_digest = match g.gate_kind {
            super::types::DocumentGateKind::Design => {
                normalized.design.as_ref().map(|d| d.digest.as_str())
            }
            super::types::DocumentGateKind::Plan => {
                normalized.plan.as_ref().map(|d| d.digest.as_str())
            }
        };
        let pub_required: Vec<String> = g
            .required_reviewer_node_ids
            .iter()
            .map(|nid| id_map.map_id(nid))
            .collect();
        let (returned, running, blocked) = document_gate_evidence_counts(
            g,
            &g.required_reviewer_node_ids,
            &run_bindings,
            &run_by_id,
            count_cycle,
            expected_digest,
            current_fp,
        );
        gate_snaps.push(WorkflowGateSnapshot {
            gate_id: id_map.map_id(&g.id),
            gate_kind: match g.gate_kind {
                super::types::DocumentGateKind::Design => "design".into(),
                super::types::DocumentGateKind::Plan => "plan".into(),
            },
            resolution_mode: match g.resolution_mode {
                ResolutionMode::ParentAdjudication => "parent_adjudication".into(),
                ResolutionMode::SelfReview => "self_review".into(),
            },
            required_reviewer_node_ids: pub_required,
            required_count: g.required_reviewer_node_ids.len() as u64,
            returned_count: returned,
            running_count: running,
            blocked_count: blocked,
            latest_gate_cycle: latest.map(|s| s.gate_cycle),
            latest_outcome: latest.map(|s| settlement_outcome_str(&s.outcome).to_string()),
            latest_summary: latest
                .map(|s| redact_display_string(&s.summary))
                .filter(|s| !s.is_empty()),
        });
    }

    let phases: Vec<WorkflowPhaseSnapshot> = normalized
        .phases
        .iter()
        .map(|p| WorkflowPhaseSnapshot {
            id: id_map.map_id(&p.id),
            kind: p.kind.as_deref().map(|k| id_map.map_id(k)),
            title: redact_optional_display(p.title.as_deref()),
        })
        .collect();

    let edges: Vec<WorkflowEdgeSnapshot> = normalized
        .edges
        .iter()
        .map(|e| WorkflowEdgeSnapshot {
            id: e.id.as_deref().map(|i| id_map.map_id(i)),
            from: id_map.map_id(&e.from),
            to: id_map.map_id(&e.to),
        })
        .collect();

    enrich_nodes_display_from_children(conn, &mut nodes).await;

    let (current_node_ids, current_phase_id) =
        select_current_nodes(&nodes, &gate_snaps, &settlements);
    let overall_state = derive_overall_state(&header.workflow_state, &nodes, &gate_summary);

    Ok(Some(WorkflowGraphSnapshot {
        schema_version: WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
        workflow_id: Some(id_map.map_id(&header.workflow_id)),
        workflow_kind: header.workflow_kind.clone(),
        manifest_revision: Some(header.active_manifest_revision as u64),
        graph_revision: Some(header.graph_revision as u64),
        manifest_state: Some(workflow_state_str(&header.workflow_state).to_string()),
        compatibility: WorkflowCompatibility::Manifest,
        overall_state,
        current_phase_id,
        current_node_ids,
        phases,
        nodes,
        edges,
        gates: gate_snaps,
    }))
}

// PublicIdAllocator (dto) is the collision-safe raw→public map for one projection.

/// Results of Task/Final execution-gate evaluation used for overall_state.
#[derive(Debug, Default)]
struct ExecutionGateOverlaySummary {
    /// Evaluated Task gates that currently pass.
    task_gates_passed: usize,
    /// Evaluated Task gates that fail (both roles present).
    task_gates_failed: usize,
    /// Final gate result when both sides evaluable (reviewer present).
    final_gate_passed: Option<bool>,
}

impl ExecutionGateOverlaySummary {
    fn any_failed(&self) -> bool {
        self.task_gates_failed > 0 || self.final_gate_passed == Some(false)
    }
}

fn project_node_from_binding(
    b: &delegation_workflow_node_binding::Model,
    mn: Option<&super::types::NormalizedNode>,
    rbs: &[&delegation_workflow_run_binding::Model],
    // Parent runs whose work_unit_key matches this binding (may lack run_binding).
    key_runs: &[&delegation_task_run::Model],
    run_by_id: &HashMap<String, &delegation_task_run::Model>,
    id_map: &mut PublicIdAllocator,
) -> WorkflowNodeSnapshot {
    let latest_rb = rbs.first().copied();
    let bound_task_ids: HashSet<&str> = rbs.iter().map(|rb| rb.task_id.as_str()).collect();

    // A9: runs with matching key but missing run_binding row still attach.
    let unbound_key_runs: Vec<&&delegation_task_run::Model> = key_runs
        .iter()
        .filter(|r| !bound_task_ids.contains(r.task_id.as_str()))
        .collect();

    // Latest run: prefer highest lineage among bound; also consider unbound by generation.
    let latest_bound_run = latest_rb.and_then(|rb| run_by_id.get(&rb.task_id).copied());
    let latest_unbound = unbound_key_runs.iter().copied().max_by(|a, b| {
        a.generation
            .cmp(&b.generation)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });
    let latest_run = match (latest_bound_run, latest_unbound) {
        (Some(b_run), Some(u_run)) => {
            if u_run.generation > b_run.generation
                || (u_run.generation == b_run.generation && u_run.created_at > b_run.created_at)
            {
                Some(*u_run)
            } else {
                Some(b_run)
            }
        }
        (Some(b_run), None) => Some(b_run),
        (None, Some(u_run)) => Some(*u_run),
        (None, None) => None,
    };

    // For status/summary_validated: use run_binding only when latest is bound.
    let latest_rb_for_status =
        latest_run.and_then(|run| rbs.iter().find(|rb| rb.task_id == run.task_id).copied());

    let run_count = (rbs.len() + unbound_key_runs.len()) as u64;
    let replacement_count = {
        let mut n = 0u64;
        for rb in rbs {
            if run_by_id
                .get(&rb.task_id)
                .is_some_and(|r| r.replaced_task_id.is_some())
            {
                n += 1;
            }
        }
        for r in &unbound_key_runs {
            if r.replaced_task_id.is_some() {
                n += 1;
            }
        }
        n
    };

    let (status, status_reason, summary) = project_node_status(b, latest_rb_for_status, latest_run);

    let active_child_generation = latest_run.map(|r| r.generation);
    let round_count = active_child_generation.map(|g| {
        if g > 0 {
            (g as u64).saturating_sub(1)
        } else {
            0
        }
    });

    let kind = mn
        .map(|n| node_kind_str(n.kind))
        .unwrap_or("work_unit")
        .to_string();
    let deps = mn
        .map(|n| n.deps.iter().map(|d| id_map.map_id(d)).collect())
        .unwrap_or_default();
    // Manifest title is often empty; fall back to the launch task preview, then
    // the validated card-summary text. Child session title may still upgrade this
    // in enrich_nodes_display_from_children.
    let title = project_run_title(
        mn.and_then(|n| n.title.as_deref()),
        latest_run.and_then(|r| r.task_preview.as_deref()),
        summary.as_deref(),
    );
    let required = mn.map(|n| n.required).unwrap_or(true);
    let (model, effort) =
        model_and_effort_from_config_json(latest_run.and_then(|r| r.config_values_json.as_deref()));
    let runtime = latest_run
        .map(run_card_runtime)
        .unwrap_or_else(empty_run_card_runtime);
    // All runs for this work unit (bound + A9 key-matched) for elapsed sum.
    let mut all_runs: Vec<&delegation_task_run::Model> = Vec::new();
    for rb in rbs {
        if let Some(run) = run_by_id.get(&rb.task_id).copied() {
            all_runs.push(run);
        }
    }
    for r in &unbound_key_runs {
        all_runs.push(*r);
    }
    // De-dupe by task_id (bound ∩ unbound should be empty but be safe).
    all_runs.sort_by_key(|r| r.task_id.as_str());
    all_runs.dedup_by_key(|r| r.task_id.as_str());
    let elapsed_completed_ms = sum_elapsed_completed_ms(&all_runs, latest_run);

    WorkflowNodeSnapshot {
        node_id: id_map.map_id(&b.node_id),
        kind,
        phase_id: Some(id_map.map_id(&b.phase_id)),
        role: Some(id_map.map_id(&b.role)),
        agent_type: Some(id_map.map_id(&b.agent_type)),
        model,
        effort,
        profile_id: b.profile_id.as_deref().map(|p| id_map.map_id(p)),
        task_index: b.task_index.map(|i| i as u32),
        task_risk_level: None,
        task_risk_reason_codes: vec![],
        required_reviewer_count: None,
        returned_reviewer_count: None,
        title,
        status,
        status_reason,
        run_count,
        active_child_generation,
        replacement_count,
        gate_cycle: latest_rb_for_status.and_then(|rb| rb.gate_cycle),
        round_count,
        latest_task_id: latest_run.map(|r| id_map.map_id(&r.task_id)),
        latest_child_conversation_id: latest_run.map(|r| r.child_conversation_id),
        latest_run_status: latest_run.map(|r| run_status_str(&r.status).to_string()),
        started_at: runtime.started_at,
        finished_at: runtime.finished_at,
        elapsed_completed_ms,
        tool_call_count: runtime.tool_call_count,
        edit_tool_call_count: runtime.edit_tool_call_count,
        touched_file_count: runtime.touched_file_count,
        touched_files_truncated: runtime.touched_files_truncated,
        additions: runtime.additions,
        deletions: runtime.deletions,
        line_counts_complete: runtime.line_counts_complete,
        summary,
        is_observed: b.is_observed || latest_run.is_some(),
        retained_observed: b.retained_observed,
        required,
        node_outcome: b.node_outcome.as_ref().map(|o| match o {
            NodeOutcome::Canceled => "canceled".to_string(),
        }),
        deps,
    }
}

fn project_node_from_manifest_only(
    mn: &super::types::NormalizedNode,
    id_map: &mut PublicIdAllocator,
) -> WorkflowNodeSnapshot {
    WorkflowNodeSnapshot {
        node_id: id_map.map_id(&mn.id),
        kind: node_kind_str(mn.kind).to_string(),
        phase_id: mn.phase_id.as_deref().map(|p| id_map.map_id(p)),
        role: mn.role.map(role_str).map(|s| id_map.map_id(s)),
        agent_type: mn.agent_type.as_deref().map(|s| id_map.map_id(s)),
        model: None,
        effort: None,
        profile_id: mn.profile_id.as_deref().map(|s| id_map.map_id(s)),
        task_index: mn.task_index,
        task_risk_level: None,
        task_risk_reason_codes: vec![],
        required_reviewer_count: None,
        returned_reviewer_count: None,
        title: first_display_title([mn.title.as_deref()]),
        status: ProjectedNodeStatus::Estimated,
        status_reason: None,
        run_count: 0,
        active_child_generation: None,
        replacement_count: 0,
        gate_cycle: None,
        round_count: None,
        latest_task_id: None,
        latest_child_conversation_id: None,
        latest_run_status: None,
        started_at: None,
        finished_at: None,
        elapsed_completed_ms: None,
        tool_call_count: None,
        edit_tool_call_count: None,
        touched_file_count: None,
        touched_files_truncated: false,
        additions: None,
        deletions: None,
        line_counts_complete: None,
        summary: None,
        is_observed: false,
        retained_observed: false,
        required: mn.required,
        node_outcome: mn.node_outcome.map(|_| "canceled".to_string()),
        deps: mn.deps.iter().map(|d| id_map.map_id(d)).collect(),
    }
}

fn project_node_status(
    b: &delegation_workflow_node_binding::Model,
    latest_rb: Option<&delegation_workflow_run_binding::Model>,
    latest_run: Option<&delegation_task_run::Model>,
) -> (ProjectedNodeStatus, Option<String>, Option<String>) {
    // Precedence (design):
    // 1. Durable blocked/failed with no recovery
    // 2. Required terminal without validated summary → missing_summary
    // 3. Reserving/running
    // 4. Explicit document-gate settlement (handled at gate level)
    // 5. Terminal + validated summary
    // 6. Retained observed
    // 7. Estimated

    if matches!(b.node_outcome, Some(NodeOutcome::Canceled)) {
        return (ProjectedNodeStatus::Canceled, Some("canceled".into()), None);
    }

    let Some(run) = latest_run else {
        if b.retained_observed {
            return (ProjectedNodeStatus::Superseded, None, None);
        }
        if b.is_observed {
            return (ProjectedNodeStatus::Estimated, None, None);
        }
        return (ProjectedNodeStatus::Estimated, None, None);
    };

    let summary_json = run.card_summary_json.as_deref();
    let parsed = summary_json.and_then(parse_and_validate_summary_json);
    let summary_text = parsed.as_ref().and_then(summary_text_from_card);
    let summary_text = summary_text.map(|s| redact_display_string(&s));

    // Unbound key-matched runs (no run_binding row) still contribute status;
    // treat summary as validated when parseable card JSON is present.
    let summary_validated = latest_rb.map(|rb| rb.summary_validated).unwrap_or_else(|| {
        latest_run
            .and_then(|r| r.card_summary_json.as_deref())
            .and_then(parse_and_validate_summary_json)
            .is_some()
    });

    match run.status {
        DelegationRunStatus::Reserving => {
            return (ProjectedNodeStatus::Reserving, None, summary_text);
        }
        DelegationRunStatus::Running => {
            return (ProjectedNodeStatus::Running, None, summary_text);
        }
        DelegationRunStatus::Failed => {
            return (
                ProjectedNodeStatus::Failed,
                Some("failed".into()),
                summary_text,
            );
        }
        DelegationRunStatus::Canceled => {
            return (
                ProjectedNodeStatus::Canceled,
                Some("canceled".into()),
                summary_text,
            );
        }
        DelegationRunStatus::Completed => {}
    }

    // Terminal completed.
    if !summary_validated {
        return (
            ProjectedNodeStatus::MissingSummary,
            Some("missing_summary".into()),
            summary_text,
        );
    }

    // Validated summary: map implementer block statuses.
    if let Some(CardSummary::Implementation { status, .. }) = &parsed {
        match status {
            WorkStatus::Blocked | WorkStatus::NeedsContext => {
                return (
                    ProjectedNodeStatus::Blocked,
                    Some(work_status_str(status).into()),
                    summary_text,
                );
            }
            WorkStatus::Done | WorkStatus::DoneWithConcerns => {
                return (ProjectedNodeStatus::Completed, None, summary_text);
            }
        }
    }
    if let Some(CardSummary::Author { status, .. }) = &parsed {
        match status {
            WorkStatus::Blocked | WorkStatus::NeedsContext => {
                return (
                    ProjectedNodeStatus::Blocked,
                    Some(work_status_str(status).into()),
                    summary_text,
                );
            }
            WorkStatus::Done | WorkStatus::DoneWithConcerns => {
                return (ProjectedNodeStatus::Completed, None, summary_text);
            }
        }
    }
    if let Some(CardSummary::Review { verdict, .. }) = &parsed {
        match verdict {
            ReviewVerdict::Block => {
                return (
                    ProjectedNodeStatus::Blocked,
                    Some("block".into()),
                    summary_text,
                );
            }
            ReviewVerdict::RequestChanges => {
                return (
                    ProjectedNodeStatus::WaitingReview,
                    Some("request_changes".into()),
                    summary_text,
                );
            }
            ReviewVerdict::Approve | ReviewVerdict::ApproveWithMinors => {
                return (ProjectedNodeStatus::Completed, None, summary_text);
            }
        }
    }

    (ProjectedNodeStatus::Completed, None, summary_text)
}

fn summary_text_from_card(card: &CardSummary) -> Option<String> {
    match card {
        CardSummary::Review { summary, .. } => Some(summary.clone()),
        CardSummary::Author { summary, .. } => Some(summary.clone()),
        CardSummary::Implementation { summary, .. } => Some(summary.clone()),
    }
}

fn build_evidence_by_node(
    nodes: &[WorkflowNodeSnapshot],
    rbs_by_node: &HashMap<String, Vec<&delegation_workflow_run_binding::Model>>,
    run_by_id: &HashMap<String, &delegation_task_run::Model>,
) -> HashMap<String, ExecutionGateRunEvidence> {
    // Evidence is keyed by projected (public) node_id. Recover via latest_task_id
    // when present; otherwise walk raw bindings that map to the same public id
    // by matching against original node ids in rbs_by_node through reverse lookup.
    // rbs_by_node is keyed by raw node_id; nodes already use public ids. Match
    // via latest_task_id on the snapshot.
    let mut out = HashMap::new();
    for n in nodes {
        let Some(task_id_pub) = n.latest_task_id.as_deref() else {
            continue;
        };
        // Find raw run whose safe_public_id matches (or equals) the projected task id.
        let run = run_by_id
            .values()
            .find(|r| safe_public_id(&r.task_id) == task_id_pub || r.task_id == task_id_pub);
        let Some(run) = run else { continue };
        // Find binding for this task_id.
        let binding = rbs_by_node
            .values()
            .flatten()
            .find(|rb| rb.task_id == run.task_id);
        if let Some(binding) = binding {
            out.insert(
                n.node_id.clone(),
                evidence_from_run_and_binding(run, binding),
            );
        }
    }
    out
}

/// Active gate candidate: non-retired, in active manifest **or** cohort_frozen,
/// and not pure retained-observed superseded history.
fn is_active_gate_binding(
    b: &delegation_workflow_node_binding::Model,
    _active_manifest_revision: i64,
    in_manifest: bool,
) -> bool {
    // Retired bindings are history-only (superseded after plan revision).
    if b.retired_revision.is_some() && !b.cohort_frozen {
        return false;
    }
    // Pure retained_observed without pair_freeze and not in manifest → superseded.
    if b.retained_observed && !b.cohort_frozen && !in_manifest {
        return false;
    }
    // Active when in the active manifest, or pair-frozen (B14 continue path).
    in_manifest || b.cohort_frozen
}

/// Branch tip for Final first-pass: durable Task implementer artifact_digest.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DerivedBranchTip {
    /// Concrete tip from a completed Task implementer binding.
    Digest(String),
    /// Manifest has Task implementer nodes but no completed digest → Final cannot complete.
    Pending,
    /// No Task implementer nodes (empty plan / no tasks) — tip match not required.
    NoTasks,
}

/// Candidate for branch-tip selection (one terminal Completed Task implementer).
/// Empty digests are still candidates for **index** selection; only after the
/// winner is chosen does an empty digest become `BranchTipPending`.
/// Ordering is **not** global generation: highest `task_index` wins; within a unit,
/// generation (stand-in for lineage_ordinal / evidence clock on that unit) wins.
#[derive(Debug, Clone)]
struct BranchTipCandidate {
    task_index: u32,
    /// Per-work-unit recency only (generation / lineage ordinal on that unit).
    unit_generation: i64,
    task_id: String,
    /// Non-empty artifact digest if present; empty/missing → Pending when this wins.
    digest: Option<String>,
}

fn derive_branch_tip_digest(
    nodes: &[WorkflowNodeSnapshot],
    evidence_by_node: &HashMap<String, ExecutionGateRunEvidence>,
    gate_eligible: &HashSet<String>,
) -> DerivedBranchTip {
    let mut has_task_implementer = false;
    let mut candidates: Vec<BranchTipCandidate> = Vec::new();

    for n in nodes {
        if !gate_eligible.contains(&n.node_id) {
            continue;
        }
        if n.phase_id.as_deref() != Some("tasks") {
            continue;
        }
        if n.role.as_deref() != Some("implementer") {
            continue;
        }
        has_task_implementer = true;
        let Some(task_index) = n.task_index else {
            continue;
        };
        let Some(ev) = evidence_by_node.get(&n.node_id) else {
            continue;
        };
        // Only terminal Completed — not failed/canceled.
        if !matches!(ev.status, TerminalRunStatus::Completed) {
            continue;
        }
        // Include empty digests when selecting the winning task_index.
        let digest = ev
            .artifact_digest
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        candidates.push(BranchTipCandidate {
            task_index,
            unit_generation: ev.generation,
            task_id: ev.task_id.clone(),
            digest,
        });
    }

    if !has_task_implementer {
        return DerivedBranchTip::NoTasks;
    }
    if candidates.is_empty() {
        // Tasks exist but no terminal Completed implementer → Final tip pending.
        return DerivedBranchTip::Pending;
    }
    // 1) Highest task_index among Completed implementers (empty digests included).
    // 2) Within same task_index: highest generation for that unit only.
    // 3) Stable tie-break on task_id.
    candidates.sort_by(|a, b| {
        b.task_index
            .cmp(&a.task_index)
            .then_with(|| b.unit_generation.cmp(&a.unit_generation))
            .then_with(|| b.task_id.cmp(&a.task_id))
    });
    match &candidates[0].digest {
        Some(d) => DerivedBranchTip::Digest(d.clone()),
        None => DerivedBranchTip::Pending,
    }
}

/// Call `evaluate_execution_gate` for every **active** Task and Final pair.
/// Orphan nodes must not be present yet (appended after this call).
fn apply_execution_gate_overlays(
    nodes: &mut [WorkflowNodeSnapshot],
    evidence_by_node: &HashMap<String, ExecutionGateRunEvidence>,
    gate_eligible: &HashSet<String>,
    task_policies: &[ManifestTaskPolicy],
    id_map: &mut PublicIdAllocator,
) -> ExecutionGateOverlaySummary {
    let mut summary = ExecutionGateOverlaySummary::default();
    let branch_tip = derive_branch_tip_digest(nodes, evidence_by_node, gate_eligible);

    // Task policy routes are authoritative. Never infer a reviewer from role or
    // task index because high-risk Tasks require both routed reviewers.
    for policy in task_policies {
        let implementer_id = id_map.map_id(&policy.route.implementer_node_id);
        let reviewer_ids: Vec<String> = policy
            .route
            .reviewer_node_ids
            .iter()
            .map(|node_id| id_map.map_id(node_id))
            .collect();
        let Some(implementer_index) = nodes.iter().position(|node| node.node_id == implementer_id)
        else {
            continue;
        };
        let reviewer_indices: Vec<usize> = reviewer_ids
            .iter()
            .filter_map(|node_id| nodes.iter().position(|node| node.node_id == *node_id))
            .collect();
        if reviewer_indices.len() != reviewer_ids.len() {
            continue;
        }

        let implementer_evidence = gate_eligible
            .contains(&implementer_id)
            .then(|| evidence_by_node.get(&implementer_id).cloned())
            .flatten();
        let required_reviewers: Vec<RequiredReviewerEvidence> = reviewer_ids
            .iter()
            .map(|node_id| RequiredReviewerEvidence {
                node_id: node_id.clone(),
                evidence: gate_eligible
                    .contains(node_id)
                    .then(|| evidence_by_node.get(node_id).cloned())
                    .flatten(),
            })
            .collect();
        let individual_evals: Vec<ExecutionGateEval> = required_reviewers
            .iter()
            .map(|required| {
                evaluate_execution_gate(&ExecutionGateInput {
                    kind: ExecutionGateKind::Task,
                    implementer_or_fixer: implementer_evidence.clone(),
                    required_reviewers: vec![required.clone()],
                    branch_tip_digest: None,
                })
            })
            .collect();
        let returned_count = required_reviewers
            .iter()
            .filter(|required| {
                reviewer_returned_for_current_producer(
                    implementer_evidence.as_ref(),
                    required.evidence.as_ref(),
                )
            })
            .count() as u64;
        apply_task_policy_metadata(
            nodes,
            implementer_index,
            &reviewer_indices,
            policy,
            returned_count,
        );

        if implementer_evidence.is_none()
            && required_reviewers
                .iter()
                .all(|required| required.evidence.is_none())
        {
            if matches!(
                nodes[implementer_index].status,
                ProjectedNodeStatus::Completed
            ) {
                for reviewer_index in &reviewer_indices {
                    if matches!(
                        nodes[*reviewer_index].status,
                        ProjectedNodeStatus::Estimated | ProjectedNodeStatus::Superseded
                    ) {
                        nodes[*reviewer_index].status = ProjectedNodeStatus::WaitingReview;
                    }
                }
            }
            continue;
        }

        let eval = evaluate_execution_gate(&ExecutionGateInput {
            kind: ExecutionGateKind::Task,
            implementer_or_fixer: implementer_evidence,
            required_reviewers,
            branch_tip_digest: None,
        });
        apply_eval_to_task(
            nodes,
            implementer_index,
            &reviewer_indices,
            &individual_evals,
            &eval,
            &mut summary,
        );
    }

    // --- Final pair: phase=final reviewer + optional fixer (eligible only) ---
    let mut final_rev: Option<usize> = None;
    let mut final_fix: Option<usize> = None;
    for (i, n) in nodes.iter().enumerate() {
        if !gate_eligible.contains(&n.node_id) {
            continue;
        }
        if !n.required
            || n.status_reason.as_deref() == Some("orphan_observed")
            || matches!(n.status, ProjectedNodeStatus::Superseded)
        {
            continue;
        }
        match (n.phase_id.as_deref(), n.role.as_deref()) {
            (Some("final"), Some("reviewer")) => final_rev = Some(i),
            (Some("final"), Some("fixer")) => final_fix = Some(i),
            _ => {}
        }
    }

    if let Some(ri) = final_rev {
        let rev_id = nodes[ri].node_id.clone();
        let rev_ev = evidence_by_node.get(&rev_id).cloned();
        let fix_ev = final_fix.and_then(|fi| {
            let id = nodes[fi].node_id.clone();
            evidence_by_node.get(&id).cloned()
        });

        // Final first-pass with Task implementers but no tip digests yet → pending.
        if fix_ev.is_none() && matches!(branch_tip, DerivedBranchTip::Pending) {
            summary.final_gate_passed = Some(false);
            if matches!(
                nodes[ri].status,
                ProjectedNodeStatus::Completed | ProjectedNodeStatus::WaitingReview
            ) {
                nodes[ri].status = ProjectedNodeStatus::WaitingReview;
                nodes[ri].status_reason = Some("branch_tip_pending".into());
            } else {
                nodes[ri].status_reason = Some("branch_tip_pending".into());
            }
        } else if rev_ev.is_some() || fix_ev.is_some() {
            let tip = match &branch_tip {
                DerivedBranchTip::Digest(d) => Some(d.clone()),
                // No tasks: tip match not required (still need non-empty reviewer digest).
                DerivedBranchTip::NoTasks => None,
                // Pending handled above for first-pass; with fixer, tip unused.
                DerivedBranchTip::Pending => None,
            };
            // Never pass None when implementer digests exist (Digest branch).
            debug_assert!(
                !matches!(branch_tip, DerivedBranchTip::Digest(_)) || tip.is_some(),
                "branch tip digest must be forwarded when derived"
            );
            let eval = evaluate_execution_gate(&ExecutionGateInput {
                kind: ExecutionGateKind::Final,
                implementer_or_fixer: fix_ev,
                required_reviewers: vec![RequiredReviewerEvidence {
                    node_id: rev_id,
                    evidence: rev_ev,
                }],
                branch_tip_digest: tip,
            });
            apply_eval_to_final(&mut nodes[ri], &eval, &mut summary);
        }
    }

    summary
}

fn apply_task_policy_metadata(
    nodes: &mut [WorkflowNodeSnapshot],
    implementer_index: usize,
    reviewer_indices: &[usize],
    policy: &ManifestTaskPolicy,
    returned_count: u64,
) {
    let level = match policy.risk.level {
        TaskRiskLevel::Normal => "normal",
        TaskRiskLevel::High => "high",
    };
    let reason_codes: Vec<String> = policy
        .risk
        .hard_triggers
        .iter()
        .map(|trigger| task_hard_trigger_code(trigger.kind).to_string())
        .chain(
            policy
                .risk
                .soft_signals
                .iter()
                .map(|signal| task_soft_signal_code(signal.kind).to_string()),
        )
        .collect();
    let required_count = reviewer_indices.len() as u64;
    for node_index in std::iter::once(implementer_index).chain(reviewer_indices.iter().copied()) {
        let node = &mut nodes[node_index];
        node.task_risk_level = Some(level.to_string());
        node.task_risk_reason_codes = reason_codes.clone();
        node.required_reviewer_count = Some(required_count);
        node.returned_reviewer_count = Some(returned_count);
    }
}

fn reviewer_returned_for_current_producer(
    producer: Option<&ExecutionGateRunEvidence>,
    reviewer: Option<&ExecutionGateRunEvidence>,
) -> bool {
    let (Some(producer), Some(reviewer)) = (producer, reviewer) else {
        return false;
    };
    if !matches!(producer.status, TerminalRunStatus::Completed)
        || !producer.summary_validated
        || !matches!(
            producer.work_status,
            Some(WorkStatus::Done) | Some(WorkStatus::DoneWithConcerns)
        )
        || !matches!(reviewer.status, TerminalRunStatus::Completed)
        || !reviewer.summary_validated
        || reviewer.review_verdict.is_none()
        || reviewer.reviewed_task_id.as_deref() != Some(producer.task_id.as_str())
    {
        return false;
    }
    let producer_digest = producer
        .artifact_digest
        .as_deref()
        .map(str::trim)
        .filter(|digest| !digest.is_empty());
    let reviewer_digest = reviewer
        .artifact_digest
        .as_deref()
        .map(str::trim)
        .filter(|digest| !digest.is_empty());
    matches!((producer_digest, reviewer_digest), (Some(a), Some(b)) if a == b)
}

fn task_hard_trigger_code(kind: TaskHardTriggerKind) -> &'static str {
    match kind {
        TaskHardTriggerKind::ConcurrencyLifecycle => "concurrency_lifecycle",
        TaskHardTriggerKind::SecurityTrustBoundary => "security_trust_boundary",
        TaskHardTriggerKind::MigrationDestructivePersistence => "migration_destructive_persistence",
        TaskHardTriggerKind::PublicCompatibility => "public_compatibility",
        TaskHardTriggerKind::UnsafeFfi => "unsafe_ffi",
        TaskHardTriggerKind::UpdateRollback => "update_rollback",
    }
}

fn task_soft_signal_code(kind: TaskSoftSignalKind) -> &'static str {
    match kind {
        TaskSoftSignalKind::CrossRuntimeOrProcess => "cross_runtime_or_process",
        TaskSoftSignalKind::BroadProductionSurface => "broad_production_surface",
        TaskSoftSignalKind::MultipleOwnershipModules => "multiple_ownership_modules",
        TaskSoftSignalKind::SharedInterface => "shared_interface",
        TaskSoftSignalKind::DependencyOrBuild => "dependency_or_build",
        TaskSoftSignalKind::MultiLayerWithoutTestSeam => "multi_layer_without_test_seam",
    }
}

fn ensure_author_before_plan_reviewers(nodes: &mut Vec<WorkflowNodeSnapshot>) {
    let Some(author_index) = nodes.iter().position(|node| {
        node.phase_id.as_deref() == Some("plan") && node.role.as_deref() == Some("author")
    }) else {
        return;
    };
    let Some(first_reviewer_index) = nodes.iter().position(|node| {
        node.phase_id.as_deref() == Some("plan") && node.role.as_deref() == Some("reviewer")
    }) else {
        return;
    };
    if author_index > first_reviewer_index {
        let author = nodes.remove(author_index);
        nodes.insert(first_reviewer_index, author);
    }
}

fn apply_eval_to_task(
    nodes: &mut [WorkflowNodeSnapshot],
    implementer_index: usize,
    reviewer_indices: &[usize],
    individual_evals: &[ExecutionGateEval],
    eval: &ExecutionGateEval,
    summary: &mut ExecutionGateOverlaySummary,
) {
    if eval.passed {
        summary.task_gates_passed += 1;
        return;
    }

    summary.task_gates_failed += 1;
    let implementer_completed = matches!(
        nodes[implementer_index].status,
        ProjectedNodeStatus::Completed
    );
    for (reviewer_index, reviewer_eval) in reviewer_indices.iter().zip(individual_evals) {
        if reviewer_eval.passed {
            continue;
        }
        let reviewer = &mut nodes[*reviewer_index];
        demote_reviewer_on_gate_fail(reviewer, &reviewer_eval.reason);
        if matches!(
            reviewer_eval.reason,
            ExecutionGateReason::MissingReviewer | ExecutionGateReason::ReviewerNotTerminalPass
        ) && implementer_completed
            && matches!(
                reviewer.status,
                ProjectedNodeStatus::Estimated
                    | ProjectedNodeStatus::Superseded
                    | ProjectedNodeStatus::WaitingReview
            )
        {
            reviewer.status = ProjectedNodeStatus::WaitingReview;
        }
    }
}

fn apply_eval_to_final(
    rev_node: &mut WorkflowNodeSnapshot,
    eval: &ExecutionGateEval,
    summary: &mut ExecutionGateOverlaySummary,
) {
    if eval.passed {
        summary.final_gate_passed = Some(true);
        return;
    }
    summary.final_gate_passed = Some(false);
    demote_reviewer_on_gate_fail(rev_node, &eval.reason);
}

fn demote_reviewer_on_gate_fail(rev_node: &mut WorkflowNodeSnapshot, reason: &ExecutionGateReason) {
    let reason_str = execution_gate_reason_str(reason);
    match reason {
        ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer
        | ExecutionGateReason::ArtifactDigestMismatch => {
            // Stale approval cannot project as completed.
            if matches!(
                rev_node.status,
                ProjectedNodeStatus::Completed | ProjectedNodeStatus::WaitingReview
            ) {
                rev_node.status = ProjectedNodeStatus::Blocked;
                rev_node.status_reason = Some(reason_str.into());
            } else {
                rev_node.status_reason = Some(reason_str.into());
            }
        }
        ExecutionGateReason::ReviewerNotTerminalPass | ExecutionGateReason::MissingReviewer => {
            if matches!(rev_node.status, ProjectedNodeStatus::Completed) {
                rev_node.status = ProjectedNodeStatus::WaitingReview;
            }
            rev_node.status_reason = Some(reason_str.into());
        }
        _ => {
            rev_node.status_reason = Some(reason_str.into());
        }
    }
}

fn execution_gate_reason_str(r: &ExecutionGateReason) -> &'static str {
    match r {
        ExecutionGateReason::Passed => "passed",
        ExecutionGateReason::MissingImplementer => "missing_implementer",
        ExecutionGateReason::ImplementerNotTerminalPass => "implementer_not_terminal_pass",
        ExecutionGateReason::MissingReviewer => "missing_reviewer",
        ExecutionGateReason::ReviewerNotTerminalPass => "reviewer_not_terminal_pass",
        ExecutionGateReason::ReviewerDoesNotCoverLatestImplementer => {
            "reviewer_does_not_cover_latest_implementer"
        }
        ExecutionGateReason::ArtifactDigestMismatch => "artifact_digest_mismatch",
    }
}

/// A9: recognized A1 parent runs whose key matches no binding → orphan observed.
fn append_orphan_observed_nodes(
    nodes: &mut Vec<WorkflowNodeSnapshot>,
    parent_runs: &[delegation_task_run::Model],
    bound_keys: &HashSet<String>,
    run_bindings: &[delegation_workflow_run_binding::Model],
    id_map: &mut PublicIdAllocator,
) {
    let bound_task_ids: HashSet<&str> = run_bindings.iter().map(|rb| rb.task_id.as_str()).collect();

    let mut by_key: HashMap<String, Vec<&delegation_task_run::Model>> = HashMap::new();
    for run in parent_runs {
        if bound_task_ids.contains(run.task_id.as_str()) {
            continue;
        }
        let Some(key) = run.work_unit_key.as_deref() else {
            continue; // NULL → Sessions only
        };
        // Keys that match a binding already overlay onto that node (not orphans).
        if bound_keys.contains(key) {
            continue;
        }
        if parse_recognized_work_unit_key(key).is_none() {
            continue; // pre-A1
        }
        by_key.entry(key.to_string()).or_default().push(run);
    }

    let mut orphan_keys: Vec<(String, Vec<&delegation_task_run::Model>)> =
        by_key.into_iter().collect();
    orphan_keys.sort_by(|a, b| a.0.cmp(&b.0));
    for (key, mut key_runs) in orphan_keys {
        key_runs.sort_by(|a, b| {
            b.generation
                .cmp(&a.generation)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        let latest = key_runs[0];
        let parsed = parse_recognized_work_unit_key(&key).expect("recognized");
        let (phase_id, role, task_index) = parsed_meta(&parsed);
        let status = match latest.status {
            DelegationRunStatus::Reserving => ProjectedNodeStatus::Reserving,
            DelegationRunStatus::Running => ProjectedNodeStatus::Running,
            DelegationRunStatus::Completed => ProjectedNodeStatus::Completed,
            DelegationRunStatus::Failed => ProjectedNodeStatus::Failed,
            DelegationRunStatus::Canceled => ProjectedNodeStatus::Canceled,
        };
        let summary = latest
            .card_summary_json
            .as_deref()
            .and_then(parse_and_validate_summary_json)
            .and_then(|c| summary_text_from_card(&c))
            .map(|s| redact_display_string(&s));
        let raw_node_id = format!("orphan-{}", synthetic_node_id(&parsed, &key));
        let (model, effort) =
            model_and_effort_from_config_json(latest.config_values_json.as_deref());
        let runtime = run_card_runtime(latest);
        let orphan_title =
            project_run_title(None, latest.task_preview.as_deref(), summary.as_deref());
        nodes.push(WorkflowNodeSnapshot {
            node_id: id_map.map_id(&raw_node_id),
            kind: "work_unit".into(),
            phase_id: Some(id_map.map_id(&phase_id)),
            role: Some(id_map.map_id(&role)),
            agent_type: Some(id_map.map_id(&latest.agent_type)),
            model,
            effort,
            profile_id: latest.profile_id.as_deref().map(|p| id_map.map_id(p)),
            task_index,
            task_risk_level: None,
            task_risk_reason_codes: vec![],
            required_reviewer_count: None,
            returned_reviewer_count: None,
            title: orphan_title,
            status,
            status_reason: Some("orphan_observed".into()),
            run_count: key_runs.len() as u64,
            active_child_generation: Some(latest.generation),
            replacement_count: key_runs
                .iter()
                .filter(|r| r.replaced_task_id.is_some())
                .count() as u64,
            gate_cycle: None,
            round_count: Some(if latest.generation > 0 {
                (latest.generation as u64).saturating_sub(1)
            } else {
                0
            }),
            latest_task_id: Some(id_map.map_id(&latest.task_id)),
            latest_child_conversation_id: Some(latest.child_conversation_id),
            latest_run_status: Some(run_status_str(&latest.status).to_string()),
            started_at: runtime.started_at,
            finished_at: runtime.finished_at,
            elapsed_completed_ms: sum_elapsed_completed_ms(&key_runs, Some(latest)),
            tool_call_count: runtime.tool_call_count,
            edit_tool_call_count: runtime.edit_tool_call_count,
            touched_file_count: runtime.touched_file_count,
            touched_files_truncated: runtime.touched_files_truncated,
            additions: runtime.additions,
            deletions: runtime.deletions,
            line_counts_complete: runtime.line_counts_complete,
            summary,
            is_observed: true,
            retained_observed: true,
            required: false,
            node_outcome: None,
            deps: vec![],
        });
    }
}

fn select_current_nodes(
    nodes: &[WorkflowNodeSnapshot],
    gates: &[WorkflowGateSnapshot],
    settlements: &[delegation_workflow_gate_settlement::Model],
) -> (Vec<String>, Option<String>) {
    // 1. Blocked that prevent progress
    let blocked: Vec<String> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.status,
                ProjectedNodeStatus::Blocked | ProjectedNodeStatus::MissingSummary
            ) && n.required
        })
        .map(|n| n.node_id.clone())
        .collect();
    if !blocked.is_empty() {
        let phase = nodes
            .iter()
            .find(|n| n.node_id == blocked[0])
            .and_then(|n| n.phase_id.clone());
        return (blocked, phase);
    }

    // 2. Reserving / running
    let active: Vec<String> = nodes
        .iter()
        .filter(|n| {
            matches!(
                n.status,
                ProjectedNodeStatus::Reserving | ProjectedNodeStatus::Running
            )
        })
        .map(|n| n.node_id.clone())
        .collect();
    if !active.is_empty() {
        let phase = nodes
            .iter()
            .find(|n| n.node_id == active[0])
            .and_then(|n| n.phase_id.clone());
        return (active, phase);
    }

    // 3. Document gate waiting adjudication (required returned, no latest approve)
    for g in gates {
        if g.required_count > 0
            && g.returned_count >= g.required_count
            && g.running_count == 0
            && g.blocked_count == 0
        {
            let approved = g.latest_outcome.as_deref().is_some_and(|o| o == "approved");
            if !approved {
                // Check if any settlement approved this gate ever for latest cycle
                let has_approve = settlements.iter().any(|s| {
                    s.gate_id == g.gate_id
                        && matches!(s.outcome, GateSettlementOutcome::Approved)
                        && g.latest_gate_cycle.is_some_and(|c| s.gate_cycle == c)
                });
                if !has_approve {
                    return (
                        g.required_reviewer_node_ids.clone(),
                        Some(g.gate_kind.clone()),
                    );
                }
            }
        }
    }

    // 4. Earliest unstarted with deps satisfied
    let completed: HashSet<&str> = nodes
        .iter()
        .filter(|n| matches!(n.status, ProjectedNodeStatus::Completed))
        .map(|n| n.node_id.as_str())
        .collect();
    for n in nodes {
        if !matches!(n.status, ProjectedNodeStatus::Estimated) {
            continue;
        }
        if n.deps.iter().all(|d| completed.contains(d.as_str())) {
            return (vec![n.node_id.clone()], n.phase_id.clone());
        }
    }

    // 5. Final completion — nothing current
    (vec![], None)
}

fn derive_overall_state(
    header_state: &WorkflowState,
    nodes: &[WorkflowNodeSnapshot],
    gate_summary: &ExecutionGateOverlaySummary,
) -> WorkflowOverallState {
    // overall_state uses execution-gate evals: a failed Task/Final gate that
    // demotes a required node to blocked feeds overall blocked.
    if nodes.iter().any(|n| {
        matches!(
            n.status,
            ProjectedNodeStatus::Blocked | ProjectedNodeStatus::Failed
        ) && n.required
    }) {
        return WorkflowOverallState::Blocked;
    }
    if nodes.iter().any(|n| {
        matches!(
            n.status,
            ProjectedNodeStatus::Reserving | ProjectedNodeStatus::Running
        )
    }) {
        return WorkflowOverallState::InProgress;
    }

    let required_done = !nodes.is_empty()
        && nodes
            .iter()
            .filter(|n| n.required)
            .all(|n| matches!(n.status, ProjectedNodeStatus::Completed));

    // Never report Completed when an evaluated execution gate failed (B13/B3).
    if gate_summary.any_failed() {
        return match header_state {
            WorkflowState::Blocked => WorkflowOverallState::Blocked,
            WorkflowState::Approved => WorkflowOverallState::Approved,
            WorkflowState::Skeleton => WorkflowOverallState::Skeleton,
            WorkflowState::Estimated => WorkflowOverallState::Estimated,
        };
    }

    if required_done {
        return WorkflowOverallState::Completed;
    }

    match header_state {
        WorkflowState::Skeleton => WorkflowOverallState::Skeleton,
        WorkflowState::Estimated => WorkflowOverallState::Estimated,
        WorkflowState::Approved => WorkflowOverallState::Approved,
        WorkflowState::Blocked => WorkflowOverallState::Blocked,
    }
}

async fn project_observed_only(
    conn: &sea_orm::DatabaseConnection,
    parent_conversation_id: i32,
) -> Result<Option<WorkflowGraphSnapshot>, ProjectError> {
    let runs = delegation_task_run::Entity::find()
        .filter(delegation_task_run::Column::ParentConversationId.eq(parent_conversation_id))
        .order_by_asc(delegation_task_run::Column::CreatedAt)
        .all(conn)
        .await
        .map_err(db_err)?;

    // Group by work_unit_key; only A1-recognized keys (A11). Pre-A1 / NULL → skip.
    let mut by_key: HashMap<String, Vec<delegation_task_run::Model>> = HashMap::new();
    let mut any_recognized = false;
    for run in runs {
        let Some(key) = run.work_unit_key.as_deref() else {
            continue; // A9 NULL
        };
        let Some(parsed) = parse_recognized_work_unit_key(key) else {
            continue; // A11/B7 pre-A1 → not observed-only
        };
        any_recognized = true;
        let _ = parsed;
        by_key.entry(key.to_string()).or_default().push(run);
    }

    if !any_recognized {
        return Ok(None);
    }

    let mut nodes: Vec<WorkflowNodeSnapshot> = Vec::new();
    let mut phase_ids: HashSet<String> = HashSet::new();
    let mut id_map = PublicIdAllocator::default();

    // Deterministic key order — never HashMap iteration for synthetic ids.
    let mut sorted_keys: Vec<(String, Vec<delegation_task_run::Model>)> =
        by_key.into_iter().collect();
    sorted_keys.sort_by(|a, b| a.0.cmp(&b.0));

    for (key, mut key_runs) in sorted_keys {
        key_runs.sort_by(|a, b| {
            b.generation
                .cmp(&a.generation)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        let latest = &key_runs[0];
        let parsed = parse_recognized_work_unit_key(&key).expect("recognized above");
        let (phase_id, role, task_index) = parsed_meta(&parsed);
        phase_ids.insert(phase_id.clone());

        let run_count = key_runs.len() as u64;
        let replacement_count = key_runs
            .iter()
            .filter(|r| r.replaced_task_id.is_some())
            .count() as u64;

        let status = match latest.status {
            DelegationRunStatus::Reserving => ProjectedNodeStatus::Reserving,
            DelegationRunStatus::Running => ProjectedNodeStatus::Running,
            DelegationRunStatus::Completed => ProjectedNodeStatus::Completed,
            DelegationRunStatus::Failed => ProjectedNodeStatus::Failed,
            DelegationRunStatus::Canceled => ProjectedNodeStatus::Canceled,
        };

        let summary = latest
            .card_summary_json
            .as_deref()
            .and_then(parse_and_validate_summary_json)
            .and_then(|c| summary_text_from_card(&c))
            .map(|s| redact_display_string(&s));

        // Synthetic node_id from stable key content — never expose raw key,
        // never use HashMap order or nodes.len() ordinals for Design/Plan/Final.
        let raw_node_id = synthetic_node_id(&parsed, &key);
        let (model, effort) =
            model_and_effort_from_config_json(latest.config_values_json.as_deref());
        let runtime = run_card_runtime(latest);
        let run_refs: Vec<&delegation_task_run::Model> = key_runs.iter().collect();
        let observed_title =
            project_run_title(None, latest.task_preview.as_deref(), summary.as_deref());
        nodes.push(WorkflowNodeSnapshot {
            node_id: id_map.map_id(&raw_node_id),
            kind: "work_unit".into(),
            phase_id: Some(id_map.map_id(&phase_id)),
            role: Some(id_map.map_id(&role)),
            agent_type: Some(id_map.map_id(&latest.agent_type)),
            model,
            effort,
            profile_id: latest.profile_id.as_deref().map(|p| id_map.map_id(p)),
            task_index,
            task_risk_level: None,
            task_risk_reason_codes: vec![],
            required_reviewer_count: None,
            returned_reviewer_count: None,
            title: observed_title,
            status,
            status_reason: None,
            run_count,
            active_child_generation: Some(latest.generation),
            replacement_count,
            gate_cycle: None,
            round_count: Some(if latest.generation > 0 {
                (latest.generation as u64).saturating_sub(1)
            } else {
                0
            }),
            latest_task_id: Some(id_map.map_id(&latest.task_id)),
            latest_child_conversation_id: Some(latest.child_conversation_id),
            latest_run_status: Some(run_status_str(&latest.status).to_string()),
            started_at: runtime.started_at,
            finished_at: runtime.finished_at,
            elapsed_completed_ms: sum_elapsed_completed_ms(&run_refs, Some(latest)),
            tool_call_count: runtime.tool_call_count,
            edit_tool_call_count: runtime.edit_tool_call_count,
            touched_file_count: runtime.touched_file_count,
            touched_files_truncated: runtime.touched_files_truncated,
            additions: runtime.additions,
            deletions: runtime.deletions,
            line_counts_complete: runtime.line_counts_complete,
            summary,
            is_observed: true,
            retained_observed: false,
            required: true,
            node_outcome: None,
            deps: vec![],
        });
    }

    // Stable order by phase then task_index.
    nodes.sort_by(|a, b| {
        a.phase_id
            .cmp(&b.phase_id)
            .then_with(|| a.task_index.cmp(&b.task_index))
            .then_with(|| a.node_id.cmp(&b.node_id))
    });

    let mut phases: Vec<WorkflowPhaseSnapshot> = phase_ids
        .into_iter()
        .map(|id| WorkflowPhaseSnapshot {
            id: id_map.map_id(&id),
            kind: Some(id_map.map_id(&id)),
            title: None,
        })
        .collect();
    phases.sort_by(|a, b| a.id.cmp(&b.id));

    enrich_nodes_display_from_children(conn, &mut nodes).await;

    let (current_node_ids, current_phase_id) = select_current_nodes(&nodes, &[], &[]);

    Ok(Some(WorkflowGraphSnapshot {
        schema_version: WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
        workflow_id: None,
        workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.to_string(),
        manifest_revision: None,
        graph_revision: None,
        manifest_state: None,
        compatibility: WorkflowCompatibility::ObservedOnly,
        overall_state: WorkflowOverallState::ObservedOnly,
        current_phase_id,
        current_node_ids,
        phases,
        nodes,
        edges: vec![],
        gates: vec![],
    }))
}

fn parsed_meta(parsed: &ParsedWorkUnitKey) -> (String, String, Option<u32>) {
    match parsed {
        ParsedWorkUnitKey::Design { .. } => ("design".into(), "reviewer".into(), None),
        ParsedWorkUnitKey::PlanAuthor { .. } => ("plan".into(), "author".into(), None),
        ParsedWorkUnitKey::PlanReviewer { .. } => ("plan".into(), "reviewer".into(), None),
        ParsedWorkUnitKey::TaskImplementer { task_index, .. } => {
            ("tasks".into(), "implementer".into(), Some(*task_index))
        }
        ParsedWorkUnitKey::TaskReviewer { task_index, .. } => {
            ("tasks".into(), "reviewer".into(), Some(*task_index))
        }
        ParsedWorkUnitKey::FinalReviewer { .. } => ("final".into(), "reviewer".into(), None),
        ParsedWorkUnitKey::FinalFixer { .. } => ("final".into(), "fixer".into(), None),
    }
}

/// Stable synthetic node id from work-unit key content (not HashMap order).
fn synthetic_node_id(parsed: &ParsedWorkUnitKey, work_unit_key: &str) -> String {
    let key_tag = short_key_tag(work_unit_key);
    match parsed {
        ParsedWorkUnitKey::Design { .. } => format!("observed-design-{key_tag}"),
        ParsedWorkUnitKey::PlanAuthor { .. } => format!("observed-plan-author-{key_tag}"),
        ParsedWorkUnitKey::PlanReviewer { .. } => format!("observed-plan-reviewer-{key_tag}"),
        ParsedWorkUnitKey::TaskImplementer { task_index, .. } => {
            format!("observed-task-{task_index}-impl")
        }
        ParsedWorkUnitKey::TaskReviewer { task_index, .. } => {
            format!("observed-task-{task_index}-rev")
        }
        ParsedWorkUnitKey::FinalReviewer { .. } => format!("observed-final-rev-{key_tag}"),
        ParsedWorkUnitKey::FinalFixer { .. } => format!("observed-final-fix-{key_tag}"),
    }
}

fn short_key_tag(work_unit_key: &str) -> String {
    let hex = sha256_hex_str(work_unit_key);
    hex[..12.min(hex.len())].to_string()
}

/// Count returned/running/blocked for document gates using run_bindings
/// matching the target gate cycle, digest, and current content fingerprint
/// (stale structural-generation runs must not count).
fn document_gate_evidence_counts(
    gate: &super::types::NormalizedGate,
    required_raw_ids: &[String],
    run_bindings: &[delegation_workflow_run_binding::Model],
    run_by_id: &HashMap<String, &delegation_task_run::Model>,
    count_cycle: i64,
    expected_digest: Option<&str>,
    current_content_fingerprint: &str,
) -> (u64, u64, u64) {
    let mut returned = 0u64;
    let mut running = 0u64;
    let mut blocked = 0u64;

    for node_id in required_raw_ids {
        let matching: Vec<&delegation_workflow_run_binding::Model> = run_bindings
            .iter()
            .filter(|rb| {
                rb.node_id == *node_id
                    && rb.gate_id.as_deref() == Some(gate.id.as_str())
                    && rb.gate_cycle == Some(count_cycle)
                    && content_fingerprint_matches(
                        rb.content_fingerprint.as_deref(),
                        current_content_fingerprint,
                    )
                    && digest_matches(rb.artifact_digest.as_deref(), expected_digest)
            })
            .collect();
        // run_bindings loaded lineage_ordinal desc — first is latest for node.
        let Some(rb) = matching.first() else {
            continue;
        };
        let Some(run) = run_by_id.get(&rb.task_id).copied() else {
            continue;
        };
        match run.status {
            DelegationRunStatus::Completed => {
                if rb.summary_validated {
                    returned += 1;
                } else {
                    blocked += 1;
                }
            }
            DelegationRunStatus::Reserving | DelegationRunStatus::Running => running += 1,
            DelegationRunStatus::Failed | DelegationRunStatus::Canceled => blocked += 1,
        }
    }

    (returned, running, blocked)
}

fn content_fingerprint_matches(actual: Option<&str>, expected: &str) -> bool {
    if expected.is_empty() {
        // No fingerprint on header yet — fail closed for document-gate counts.
        return false;
    }
    actual.is_some_and(|a| !a.is_empty() && a == expected)
}

fn digest_matches(actual: Option<&str>, expected: Option<&str>) -> bool {
    match expected {
        None => true,
        Some(exp) => actual.is_some_and(|a| a == exp),
    }
}

fn node_kind_str(k: ManifestNodeKind) -> &'static str {
    match k {
        ManifestNodeKind::Milestone => "milestone",
        ManifestNodeKind::WorkUnit => "work_unit",
        ManifestNodeKind::Gate => "gate",
        ManifestNodeKind::Placeholder => "placeholder",
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

fn workflow_state_str(s: &WorkflowState) -> &'static str {
    match s {
        WorkflowState::Skeleton => "skeleton",
        WorkflowState::Estimated => "estimated",
        WorkflowState::Approved => "approved",
        WorkflowState::Blocked => "blocked",
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

fn work_status_str(s: &WorkStatus) -> &'static str {
    match s {
        WorkStatus::Done => "done",
        WorkStatus::DoneWithConcerns => "done_with_concerns",
        WorkStatus::Blocked => "blocked",
        WorkStatus::NeedsContext => "needs_context",
    }
}

/// Build gate evidence from a durable run + run-binding for callers (Task 6+).
pub fn evidence_from_run_and_binding(
    run: &delegation_task_run::Model,
    binding: &delegation_workflow_run_binding::Model,
) -> ExecutionGateRunEvidence {
    let parsed = run
        .card_summary_json
        .as_deref()
        .and_then(parse_and_validate_summary_json);
    let (work_status, review_verdict) = match parsed {
        Some(CardSummary::Implementation { status, .. })
        | Some(CardSummary::Author { status, .. }) => (Some(status), None),
        Some(CardSummary::Review { verdict, .. }) => (None, Some(verdict)),
        None => (None, None),
    };
    ExecutionGateRunEvidence {
        task_id: run.task_id.clone(),
        generation: run.generation,
        status: match run.status {
            DelegationRunStatus::Completed => TerminalRunStatus::Completed,
            DelegationRunStatus::Failed => TerminalRunStatus::Failed,
            DelegationRunStatus::Canceled => TerminalRunStatus::Canceled,
            DelegationRunStatus::Reserving | DelegationRunStatus::Running => {
                TerminalRunStatus::NonTerminal
            }
        },
        summary_validated: binding.summary_validated,
        work_status,
        review_verdict,
        artifact_digest: binding.artifact_digest.clone(),
        reviewed_task_id: binding.reviewed_task_id.clone(),
        reviewed_implementer_generation: binding.reviewed_implementer_generation,
    }
}

/// Convenience: evaluate Task gate from two (run, binding) pairs.
pub fn evaluate_task_gate_from_pairs(
    implementer: Option<(
        &delegation_task_run::Model,
        &delegation_workflow_run_binding::Model,
    )>,
    reviewer: Option<(
        &delegation_task_run::Model,
        &delegation_workflow_run_binding::Model,
    )>,
) -> super::gates::ExecutionGateEval {
    evaluate_execution_gate(&ExecutionGateInput {
        kind: ExecutionGateKind::Task,
        implementer_or_fixer: implementer.map(|(r, b)| evidence_from_run_and_binding(r, b)),
        required_reviewers: vec![RequiredReviewerEvidence {
            node_id: "reviewer".into(),
            evidence: reviewer.map(|(r, b)| evidence_from_run_and_binding(r, b)),
        }],
        branch_tip_digest: None,
    })
}

// Silence unused import if ManifestWorkflowState is only used in tests.
#[allow(dead_code)]
fn _manifest_state_wire(s: ManifestWorkflowState) -> &'static str {
    match s {
        ManifestWorkflowState::Skeleton => "skeleton",
        ManifestWorkflowState::Estimated => "estimated",
        ManifestWorkflowState::Approved => "approved",
        ManifestWorkflowState::Blocked => "blocked",
    }
}

// ---------------------------------------------------------------------------
// Tests (B10 owned by Task 4 — projection)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_elapsed_completed_ms_skips_open_latest_and_adds_finished() {
        use chrono::{TimeZone, Utc};
        let t0 = Utc.with_ymd_and_hms(2026, 7, 19, 0, 0, 0).unwrap();
        let t1 = Utc.with_ymd_and_hms(2026, 7, 19, 0, 5, 0).unwrap(); // 5m
        let t2 = Utc.with_ymd_and_hms(2026, 7, 19, 0, 10, 0).unwrap();
        let t3 = Utc.with_ymd_and_hms(2026, 7, 19, 0, 12, 0).unwrap(); // 2m

        let finished_a = delegation_task_run::Model {
            task_id: "a".into(),
            root_task_id: "a".into(),
            previous_task_id: None,
            generation: 1,
            parent_conversation_id: 1,
            parent_tool_use_id: None,
            child_conversation_id: 10,
            agent_type: "grok".into(),
            profile_id: None,
            workspace_path: None,
            route_fingerprint: None,
            launch_snapshot_version: None,
            mode_id: None,
            config_values_json: None,
            task_preview: None,
            request_fingerprint: None,
            admission_class: crate::db::entities::delegation_task_run::AdmissionClass::NormalRevision,
            reached_running_at: None,
            lineage_root_task_id: "a".into(),
            work_unit_key: None,
            legacy_parent_tool_use_id: None,
            history_only: false,
            status: DelegationRunStatus::Completed,
            error_code: None,
            termination_audit_json: None,
            started_at: Some(t0),
            finished_at: Some(t1),
            tool_call_count: None,
            edit_tool_call_count: None,
            touched_files_json: None,
            touched_files_truncated: None,
            additions: None,
            deletions: None,
            line_counts_complete: None,
            card_summary_json: None,
            child_turn_anchor: None,
            child_connection_id: None,
            replaced_task_id: None,
            replacement_reason: None,
            recovery_authorization_id: None,
            completion_state: None,
            completion_outcome: None,
            completion_evidence_json: None,
            created_at: t0,
            updated_at: t1,
        };
        let finished_b = delegation_task_run::Model {
            task_id: "b".into(),
            generation: 2,
            started_at: Some(t2),
            finished_at: Some(t3),
            status: DelegationRunStatus::Completed,
            lineage_root_task_id: "a".into(),
            root_task_id: "a".into(),
            child_conversation_id: 11,
            parent_conversation_id: 1,
            agent_type: "grok".into(),
            admission_class: crate::db::entities::delegation_task_run::AdmissionClass::NormalRevision,
            history_only: false,
            created_at: t2,
            updated_at: t3,
            ..finished_a.clone()
        };
        let open_c = delegation_task_run::Model {
            task_id: "c".into(),
            generation: 3,
            started_at: Some(t3),
            finished_at: None,
            status: DelegationRunStatus::Running,
            lineage_root_task_id: "a".into(),
            root_task_id: "a".into(),
            child_conversation_id: 12,
            parent_conversation_id: 1,
            agent_type: "grok".into(),
            admission_class: crate::db::entities::delegation_task_run::AdmissionClass::NormalRevision,
            history_only: false,
            created_at: t3,
            updated_at: t3,
            ..finished_a.clone()
        };

        let all = [&finished_a, &finished_b, &open_c];
        // Open latest excluded: 5m + 2m = 7m.
        assert_eq!(
            sum_elapsed_completed_ms(&all, Some(&open_c)),
            Some(7 * 60_000)
        );
        // All finished: 5m + 2m = 7m (no third).
        let done = [&finished_a, &finished_b];
        assert_eq!(
            sum_elapsed_completed_ms(&done, Some(&finished_b)),
            Some(7 * 60_000)
        );
    }

    #[test]
    fn model_and_effort_reads_allowlisted_config_keys() {
        let (model, effort) = model_and_effort_from_config_json(Some(
            r#"{"effort":"high","model":"gpt-5.2","permissionMode":"default"}"#,
        ));
        assert_eq!(model.as_deref(), Some("gpt-5.2"));
        assert_eq!(effort.as_deref(), Some("high"));

        let (model2, effort2) =
            model_and_effort_from_config_json(Some(r#"{"modelId":"sonnet","thinking":"xhigh"}"#));
        assert_eq!(model2.as_deref(), Some("sonnet"));
        assert_eq!(effort2.as_deref(), Some("xhigh"));

        // Grok ACP option id — the key historically stripped from snapshots.
        let (model3, effort3) = model_and_effort_from_config_json(Some(
            r#"{"model":"grok-4.5","reasoning_effort":"high"}"#,
        ));
        assert_eq!(model3.as_deref(), Some("grok-4.5"));
        assert_eq!(effort3.as_deref(), Some("high"));

        let (empty_m, empty_e) = model_and_effort_from_config_json(Some("{}"));
        assert!(empty_m.is_none());
        assert!(empty_e.is_none());
        assert!(model_and_effort_from_config_json(None).0.is_none());
    }

    #[test]
    fn project_run_title_prefers_manifest_then_preview_then_summary() {
        assert_eq!(
            project_run_title(Some("Manifest title"), Some("preview"), Some("summary")).as_deref(),
            Some("Manifest title")
        );
        assert_eq!(
            project_run_title(None, Some("  implement login flow  "), Some("summary")).as_deref(),
            Some("implement login flow")
        );
        assert_eq!(
            project_run_title(None, None, Some("card summary text")).as_deref(),
            Some("card summary text")
        );
        assert_eq!(project_run_title(None, Some("   "), None), None);
        assert_eq!(project_run_title(Some(""), Some(""), Some("")), None);
        // Empty after path-only scrub falls through to next candidate.
        assert_eq!(
            project_run_title(None, Some("D:\\secret\\only"), Some("usable summary")).as_deref(),
            Some("usable summary")
        );
    }

    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::store::{
        publish_workflow_manifest_core, PublishWorkflowRequest,
    };
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestEdge, ManifestGate, ManifestNode, ManifestPhase,
        ManifestTaskHardTrigger, ManifestTaskPolicy, ManifestTaskRisk, ManifestTaskRoute,
        ManifestTaskSoftSignal, TaskHardTriggerKind, TaskRiskLevel, TaskSoftSignalKind,
        WorkUnitKeyParts, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN,
        PHASE_TASKS,
    };
    use crate::db::entities::delegation_task_run::AdmissionClass;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;
    use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};
    use chrono::Utc;
    use sea_orm::ActiveModelTrait;
    use sea_orm::Set;
    use std::sync::Arc;

    fn emitter() -> EventEmitter {
        EventEmitter::test_web_only(Arc::new(WebEventBroadcaster::new()))
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

    fn high_risk_doc(token: &str) -> ManifestDocument {
        let mut doc = design_plan_doc(token);
        let implementer = doc
            .nodes
            .iter_mut()
            .find(|node| node.id == "task-1-impl")
            .expect("task implementer");
        implementer.agent_type = Some("codex".into());
        implementer.work_unit_key = Some(
            build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                task_index: 1,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap(),
        );

        let grok_reviewer_id = "task-1-rev-grok";
        doc.nodes.push(wu(
            grok_reviewer_id,
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
        let final_reviewer = doc
            .nodes
            .iter_mut()
            .find(|node| node.id == "final-reviewer")
            .expect("final reviewer");
        final_reviewer.deps.push(grok_reviewer_id.into());
        doc.edges.push(ManifestEdge {
            id: Some("task-1-grok-review".into()),
            from: "task-1-impl".into(),
            to: grok_reviewer_id.into(),
        });
        doc.task_policies[0] = ManifestTaskPolicy {
            task_index: 1,
            risk: ManifestTaskRisk {
                level: TaskRiskLevel::High,
                hard_triggers: vec![ManifestTaskHardTrigger {
                    kind: TaskHardTriggerKind::ConcurrencyLifecycle,
                    evidence: vec![r"D:\private\lifecycle-evidence.md".into()],
                }],
                soft_signals: vec![ManifestTaskSoftSignal {
                    kind: TaskSoftSignalKind::SharedInterface,
                    score: 1,
                    evidence: vec!["/private/shared-interface-evidence.md".into()],
                }],
                score: 1,
                reason: "private free-form reason must never project".into(),
            },
            route: ManifestTaskRoute {
                implementer_node_id: "task-1-impl".into(),
                reviewer_node_ids: vec!["task-1-rev".into(), grok_reviewer_id.into()],
            },
        };
        doc
    }

    async fn seed_parent() -> (AppDatabase, i32) {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/wf-project").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        (db, parent)
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_run(
        db: &AppDatabase,
        parent: i32,
        task_id: &str,
        work_unit_key: Option<&str>,
        status: DelegationRunStatus,
        generation: i64,
        card_summary_json: Option<&str>,
        replaced_task_id: Option<&str>,
        agent: &str,
    ) -> i32 {
        let now = Utc::now();
        let child = seed_conversation(
            db,
            seed_folder(db, &format!("/tmp/child-{task_id}")).await,
            AgentType::Grok,
        )
        .await;
        let run = delegation_task_run::ActiveModel {
            task_id: Set(task_id.to_string()),
            root_task_id: Set(task_id.to_string()),
            previous_task_id: Set(None),
            generation: Set(generation),
            parent_conversation_id: Set(parent),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child),
            agent_type: Set(agent.into()),
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
            work_unit_key: Set(work_unit_key.map(|s| s.to_string())),
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
            card_summary_json: Set(card_summary_json.map(|s| s.to_string())),
            child_turn_anchor: Set(None),
            child_connection_id: Set(None),
            replaced_task_id: Set(replaced_task_id.map(|s| s.to_string())),
            replacement_reason: Set(None),
            recovery_authorization_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        };
        run.insert(&db.conn).await.expect("insert run");
        child
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_run_binding(
        db: &AppDatabase,
        task_id: &str,
        workflow_id: &str,
        node_id: &str,
        lineage_ordinal: i64,
        summary_validated: bool,
        artifact_digest: Option<&str>,
        reviewed_task_id: Option<&str>,
    ) {
        let now = Utc::now();
        let rb = delegation_workflow_run_binding::ActiveModel {
            task_id: Set(task_id.to_string()),
            workflow_id: Set(workflow_id.to_string()),
            node_id: Set(node_id.to_string()),
            gate_id: Set(None),
            gate_cycle: Set(None),
            manifest_revision: Set(1),
            content_fingerprint: Set(None),
            artifact_digest: Set(artifact_digest.map(|s| s.to_string())),
            reviewed_task_id: Set(reviewed_task_id.map(|s| s.to_string())),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(lineage_ordinal),
            summary_validated: Set(summary_validated),
            created_at: Set(now),
            updated_at: Set(now),
        };
        rb.insert(&db.conn).await.expect("insert rb");
    }

    #[allow(clippy::too_many_arguments)]
    async fn insert_completed_task_evidence(
        db: &AppDatabase,
        parent: i32,
        workflow_id: &str,
        node_id: &str,
        task_id: &str,
        agent: &str,
        digest: &str,
        reviewed_task_id: Option<&str>,
        lineage_ordinal: i64,
    ) {
        let summary = if reviewed_task_id.is_some() {
            r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"approved"}"#
        } else {
            r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"implemented","commits":[],"concerns":[]}"#
        };
        insert_run(
            db,
            parent,
            task_id,
            None,
            DelegationRunStatus::Completed,
            lineage_ordinal,
            Some(summary),
            None,
            agent,
        )
        .await;
        insert_run_binding(
            db,
            task_id,
            workflow_id,
            node_id,
            lineage_ordinal,
            true,
            Some(digest),
            reviewed_task_id,
        )
        .await;
    }

    #[tokio::test]
    async fn task6_projects_normal_and_high_routes_with_redacted_policy_metadata() {
        for (token, doc, expected_level, expected_reviewer_count) in [
            (
                "task6-normal-projection",
                design_plan_doc("task6-normal-projection"),
                "normal",
                1,
            ),
            (
                "task6-high-projection",
                high_risk_doc("task6-high-projection"),
                "high",
                2,
            ),
        ] {
            let (db, parent) = seed_parent().await;
            publish_workflow_manifest_core(
                &db,
                &emitter(),
                parent,
                PublishWorkflowRequest { document: doc },
            )
            .await
            .unwrap_or_else(|err| panic!("publish {token}: {err}"));

            let snap = project_workflow_graph_core(&db, parent).await.unwrap();
            let task_nodes: Vec<_> = snap
                .nodes
                .iter()
                .filter(|node| node.task_index == Some(1))
                .collect();
            assert_eq!(task_nodes.len(), expected_reviewer_count + 1);
            for node in task_nodes {
                assert_eq!(node.task_risk_level.as_deref(), Some(expected_level));
                assert_eq!(
                    node.required_reviewer_count,
                    Some(expected_reviewer_count as u64)
                );
                assert_eq!(node.returned_reviewer_count, Some(0));
            }

            if expected_level == "high" {
                let implementer = snap
                    .nodes
                    .iter()
                    .find(|node| node.node_id == "task-1-impl")
                    .unwrap();
                let reviewers: Vec<_> = ["task-1-rev", "task-1-rev-grok"]
                    .into_iter()
                    .map(|id| snap.nodes.iter().find(|node| node.node_id == id).unwrap())
                    .collect();
                assert!(reviewers
                    .iter()
                    .all(|reviewer| reviewer.deps == vec![implementer.node_id.clone()]));
                assert_eq!(
                    implementer.task_risk_reason_codes,
                    vec!["concurrency_lifecycle", "shared_interface"]
                );
                let json = serde_json::to_string(&snap).unwrap();
                assert!(!json.contains("lifecycle-evidence"));
                assert!(!json.contains("shared-interface-evidence"));
                assert!(!json.contains("private free-form reason"));
            }
        }
    }

    #[tokio::test]
    async fn task6_high_route_counts_strict_and_and_invalidates_both_old_approvals() {
        let (db, parent) = seed_parent().await;
        let published = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest {
                document: high_risk_doc("task6-high-counts"),
            },
        )
        .await
        .unwrap();

        insert_completed_task_evidence(
            &db,
            parent,
            &published.workflow_id,
            "task-1-impl",
            "impl-old",
            "codex",
            "digest-old",
            None,
            1,
        )
        .await;
        let zero = project_workflow_graph_core(&db, parent).await.unwrap();
        let zero_impl = zero
            .nodes
            .iter()
            .find(|node| node.node_id == "task-1-impl")
            .unwrap();
        assert_eq!(zero_impl.returned_reviewer_count, Some(0));
        assert_ne!(zero.overall_state, WorkflowOverallState::Completed);

        insert_completed_task_evidence(
            &db,
            parent,
            &published.workflow_id,
            "task-1-rev",
            "codex-review-old",
            "codex",
            "digest-old",
            Some("impl-old"),
            1,
        )
        .await;
        let one = project_workflow_graph_core(&db, parent).await.unwrap();
        let one_impl = one
            .nodes
            .iter()
            .find(|node| node.node_id == "task-1-impl")
            .unwrap();
        assert_eq!(one_impl.returned_reviewer_count, Some(1));
        assert_ne!(one.overall_state, WorkflowOverallState::Completed);

        let request_changes_summary = r#"{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"changes required"}"#;
        insert_run(
            &db,
            parent,
            "grok-review-changes",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(request_changes_summary),
            None,
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "grok-review-changes",
            &published.workflow_id,
            "task-1-rev-grok",
            1,
            true,
            Some("digest-old"),
            Some("impl-old"),
        )
        .await;
        let changes = project_workflow_graph_core(&db, parent).await.unwrap();
        let changes_impl = changes
            .nodes
            .iter()
            .find(|node| node.node_id == "task-1-impl")
            .unwrap();
        assert_eq!(changes_impl.returned_reviewer_count, Some(2));
        assert_ne!(changes.overall_state, WorkflowOverallState::Completed);
        assert_ne!(
            changes
                .nodes
                .iter()
                .find(|node| node.node_id == "task-1-rev-grok")
                .unwrap()
                .status,
            ProjectedNodeStatus::Completed
        );

        insert_completed_task_evidence(
            &db,
            parent,
            &published.workflow_id,
            "task-1-rev-grok",
            "grok-review-old",
            "grok",
            "digest-old",
            Some("impl-old"),
            2,
        )
        .await;
        let two = project_workflow_graph_core(&db, parent).await.unwrap();
        let two_impl = two
            .nodes
            .iter()
            .find(|node| node.node_id == "task-1-impl")
            .unwrap();
        assert_eq!(two_impl.returned_reviewer_count, Some(2));
        for reviewer_id in ["task-1-rev", "task-1-rev-grok"] {
            assert_eq!(
                two.nodes
                    .iter()
                    .find(|node| node.node_id == reviewer_id)
                    .unwrap()
                    .status,
                ProjectedNodeStatus::Completed
            );
        }

        insert_completed_task_evidence(
            &db,
            parent,
            &published.workflow_id,
            "task-1-impl",
            "impl-new",
            "codex",
            "digest-new",
            None,
            2,
        )
        .await;
        let stale = project_workflow_graph_core(&db, parent).await.unwrap();
        let stale_impl = stale
            .nodes
            .iter()
            .find(|node| node.node_id == "task-1-impl")
            .unwrap();
        assert_eq!(stale_impl.returned_reviewer_count, Some(0));
        for reviewer_id in ["task-1-rev", "task-1-rev-grok"] {
            let reviewer = stale
                .nodes
                .iter()
                .find(|node| node.node_id == reviewer_id)
                .unwrap();
            assert_ne!(reviewer.status, ProjectedNodeStatus::Completed);
            assert_eq!(
                reviewer.status_reason.as_deref(),
                Some("reviewer_does_not_cover_latest_implementer")
            );
        }
        assert_ne!(stale.overall_state, WorkflowOverallState::Completed);
    }

    #[tokio::test]
    async fn task6_author_precedes_plan_reviewers_and_final_shape_is_unchanged() {
        let (db, parent) = seed_parent().await;
        publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest {
                document: high_risk_doc("task6-order-final"),
            },
        )
        .await
        .unwrap();
        let snap = project_workflow_graph_core(&db, parent).await.unwrap();
        let author_position = snap
            .nodes
            .iter()
            .position(|node| node.node_id == "plan-author")
            .unwrap();
        let reviewer_position = snap
            .nodes
            .iter()
            .position(|node| node.node_id == "plan-reviewer-1")
            .unwrap();
        assert!(author_position < reviewer_position);

        for node_id in ["final-reviewer", "final-fixer"] {
            let node = snap
                .nodes
                .iter()
                .find(|node| node.node_id == node_id)
                .unwrap();
            assert_eq!(node.task_risk_level, None);
            assert!(node.task_risk_reason_codes.is_empty());
            assert_eq!(node.required_reviewer_count, None);
            assert_eq!(node.returned_reviewer_count, None);
        }
        assert_eq!(
            snap.nodes
                .iter()
                .filter(|node| node.phase_id.as_deref() == Some(PHASE_FINAL))
                .filter(|node| node.role.as_deref() == Some("reviewer"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn project_manifest_overlay_no_work_unit_key() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-tok-1");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish");

        let impl_summary = r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"implemented task 1","commits":[],"concerns":[]}"#;
        insert_run(
            &db,
            parent,
            "impl-run-1",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(impl_summary),
            None,
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "impl-run-1",
            &pub_r.workflow_id,
            "task-1-impl",
            1,
            true,
            Some("sha-abc"),
            None,
        )
        .await;

        // Mark binding observed
        use sea_orm::EntityTrait;
        let b = delegation_workflow_node_binding::Entity::find()
            .filter(
                delegation_workflow_node_binding::Column::WorkflowId.eq(pub_r.workflow_id.clone()),
            )
            .filter(delegation_workflow_node_binding::Column::NodeId.eq("task-1-impl"))
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut am: delegation_workflow_node_binding::ActiveModel = b.into();
        am.is_observed = Set(true);
        am.update(&db.conn).await.unwrap();

        let snap = project_workflow_graph_core(&db, parent)
            .await
            .expect("snapshot present");
        assert_eq!(snap.compatibility, WorkflowCompatibility::Manifest);
        assert_eq!(
            snap.workflow_id.as_deref(),
            Some(pub_r.workflow_id.as_str())
        );
        assert!(snap.manifest_revision.is_some());
        assert!(snap.graph_revision.is_some());

        let json = serde_json::to_string(&snap).unwrap();
        assert!(
            !json.contains("work_unit_key"),
            "snapshot must not leak work_unit_key: {json}"
        );
        assert!(!json.contains("task|1|implementer"));

        let impl_node = snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-impl")
            .expect("impl node");
        assert_eq!(impl_node.status, ProjectedNodeStatus::Completed);
        assert_eq!(impl_node.run_count, 1);
        assert_eq!(impl_node.active_child_generation, Some(1));
        assert!(impl_node
            .summary
            .as_deref()
            .is_some_and(|s| s.contains("implemented")));
    }

    #[tokio::test]
    async fn corrupt_manifest_omits_graph() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-corrupt");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish");

        // Corrupt the stored document_json.
        let rev = delegation_workflow_manifest_revision::Entity::find_by_id((
            pub_r.workflow_id.clone(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut am: delegation_workflow_manifest_revision::ActiveModel = rev.into();
        am.document_json = Set("{not-valid-json".into());
        am.update(&db.conn).await.unwrap();

        let snap = project_workflow_graph_core(&db, parent).await;
        assert!(snap.is_none(), "corrupt manifest must omit graph");
    }

    #[tokio::test]
    async fn malicious_strings_redacted_in_projection() {
        let (db, parent) = seed_parent().await;
        let mut doc = design_plan_doc("proj-redact");
        // Malicious title on a node.
        if let Some(n) = doc.nodes.iter_mut().find(|n| n.id == "task-1-impl") {
            n.title = Some(
                "fix me at /home/user/secret and key task|1|implementer|grok|none ```prompt```"
                    .into(),
            );
        }
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish");

        let evil_summary = r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"see C:\\Users\\drawpeng\\secret and task|9|implementer|grok|none","commits":[],"concerns":[]}"#;
        insert_run(
            &db,
            parent,
            "evil-run",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(evil_summary),
            None,
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "evil-run",
            &pub_r.workflow_id,
            "task-1-impl",
            1,
            true,
            None,
            None,
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent)
            .await
            .expect("snapshot");
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("/home/user/secret"));
        assert!(!json.contains(r"C:\\Users\\drawpeng"));
        assert!(!json.contains("task|1|implementer"));
        assert!(!json.contains("task|9|implementer"));
        assert!(!json.contains("```prompt```"));

        let impl_node = snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-impl")
            .unwrap();
        if let Some(t) = &impl_node.title {
            assert!(t.contains("[redacted]"), "title should be redacted: {t}");
        }
        if let Some(s) = &impl_node.summary {
            assert!(s.contains("[redacted]"), "summary should be redacted: {s}");
        }
    }

    #[tokio::test]
    async fn node_title_falls_back_to_task_preview_and_child_title() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-title-fallback");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .expect("publish");

        // No card summary yet — title must still come from task_preview.
        let child_id = insert_run(
            &db,
            parent,
            "title-run",
            None,
            DelegationRunStatus::Running,
            1,
            None,
            None,
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "title-run",
            &pub_r.workflow_id,
            "task-1-impl",
            1,
            false,
            None,
            None,
        )
        .await;

        let run = delegation_task_run::Entity::find_by_id("title-run")
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut run_am: delegation_task_run::ActiveModel = run.into();
        run_am.task_preview = Set(Some("Implement the workflow title fallback".into()));
        run_am.update(&db.conn).await.unwrap();

        let snap = project_workflow_graph_core(&db, parent)
            .await
            .expect("snapshot");
        let impl_node = snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-impl")
            .expect("impl node");
        assert_eq!(
            impl_node.title.as_deref(),
            Some("Implement the workflow title fallback"),
            "running node without manifest title must project task_preview"
        );

        // Child conversation title upgrades over the longer task preview.
        let child = crate::db::entities::conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut child_am: crate::db::entities::conversation::ActiveModel = child.into();
        child_am.title = Set(Some("Short session title".into()));
        child_am.update(&db.conn).await.unwrap();

        let snap2 = project_workflow_graph_core(&db, parent)
            .await
            .expect("snapshot after child title");
        let impl_node2 = snap2
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-impl")
            .expect("impl node");
        assert_eq!(
            impl_node2.title.as_deref(),
            Some("Short session title"),
            "child conversation title should win over task_preview"
        );
    }

    #[tokio::test]
    async fn observed_only_from_a1_keys() {
        let (db, parent) = seed_parent().await;
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 2,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let summary = r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"obs only","commits":[],"concerns":[]}"#;
        insert_run(
            &db,
            parent,
            "obs-1",
            Some(&key),
            DelegationRunStatus::Completed,
            1,
            Some(summary),
            None,
            "grok",
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent)
            .await
            .expect("observed-only snapshot");
        assert_eq!(snap.compatibility, WorkflowCompatibility::ObservedOnly);
        assert!(snap.workflow_id.is_none());
        assert!(snap.manifest_revision.is_none());
        assert!(snap.graph_revision.is_none());
        assert_eq!(snap.overall_state, WorkflowOverallState::ObservedOnly);
        assert!(!snap.nodes.is_empty());
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("work_unit_key"));
        assert!(!json.contains(&key));
    }

    #[tokio::test]
    async fn pre_a1_keys_yield_none() {
        let (db, parent) = seed_parent().await;
        // Pre-A1 absolute path style key — not recognized by A11.
        insert_run(
            &db,
            parent,
            "legacy-1",
            Some(r"D:\MyCodeBuddy\docs\plan.md|implementer"),
            DelegationRunStatus::Completed,
            1,
            None,
            None,
            "grok",
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent).await;
        assert!(
            snap.is_none(),
            "pre-A1 keys must not produce observed-only graph"
        );
    }

    #[tokio::test]
    async fn null_work_unit_key_yields_none_without_manifest() {
        let (db, parent) = seed_parent().await;
        insert_run(
            &db,
            parent,
            "null-key",
            None,
            DelegationRunStatus::Completed,
            1,
            None,
            None,
            "grok",
        )
        .await;
        let snap = project_workflow_graph_core(&db, parent).await;
        assert!(snap.is_none());
    }

    #[tokio::test]
    async fn b12_vocabulary_on_nodes() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-b12");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();

        insert_run(
            &db,
            parent,
            "old-impl",
            None,
            DelegationRunStatus::Failed,
            1,
            None,
            None,
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "old-impl",
            &pub_r.workflow_id,
            "task-1-impl",
            1,
            false,
            None,
            None,
        )
        .await;
        insert_run(
            &db,
            parent,
            "new-impl",
            None,
            DelegationRunStatus::Running,
            2,
            None,
            Some("old-impl"),
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "new-impl",
            &pub_r.workflow_id,
            "task-1-impl",
            2,
            false,
            None,
            None,
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent).await.unwrap();
        let n = snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-impl")
            .unwrap();
        assert_eq!(n.run_count, 2);
        assert_eq!(n.active_child_generation, Some(2));
        assert_eq!(n.replacement_count, 1);
        assert_eq!(n.round_count, Some(1)); // generation 2 → 1 continue round
        assert_eq!(n.status, ProjectedNodeStatus::Running);
    }

    #[tokio::test]
    async fn projection_b13_stale_reviewer_not_completed() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-b13");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();

        let impl_summary = r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"impl ok","commits":[],"concerns":[]}"#;
        let rev_summary = r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"looks good"}"#;

        // Pre-replacement implementer (stale).
        insert_run(
            &db,
            parent,
            "impl-old",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(impl_summary),
            None,
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "impl-old",
            &pub_r.workflow_id,
            "task-1-impl",
            1,
            true,
            Some("digest-x"),
            None,
        )
        .await;
        // Latest implementer replacement.
        insert_run(
            &db,
            parent,
            "impl-new",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(impl_summary),
            Some("impl-old"),
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "impl-new",
            &pub_r.workflow_id,
            "task-1-impl",
            2,
            true,
            Some("digest-x"),
            None,
        )
        .await;
        // Reviewer still covers old task_id (B13 stale).
        insert_run(
            &db,
            parent,
            "rev-1",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(rev_summary),
            None,
            "codex",
        )
        .await;
        insert_run_binding(
            &db,
            "rev-1",
            &pub_r.workflow_id,
            "task-1-rev",
            1,
            true,
            Some("digest-x"),
            Some("impl-old"),
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent).await.unwrap();
        let rev = snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-rev")
            .expect("reviewer node");
        assert_ne!(
            rev.status,
            ProjectedNodeStatus::Completed,
            "B13 stale approval must not project as completed"
        );
        assert!(
            matches!(
                rev.status,
                ProjectedNodeStatus::Blocked | ProjectedNodeStatus::WaitingReview
            ),
            "got {:?}",
            rev.status
        );
        assert_eq!(
            rev.status_reason.as_deref(),
            Some("reviewer_does_not_cover_latest_implementer")
        );
        assert_ne!(snap.overall_state, WorkflowOverallState::Completed);
    }

    #[tokio::test]
    async fn a9_orphan_recognized_runs_as_observed_nodes() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-a9");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        let _ = pub_r;

        // Recognized A1 key that is NOT on any published binding.
        let orphan_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 99,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        insert_run(
            &db,
            parent,
            "orphan-run",
            Some(&orphan_key),
            DelegationRunStatus::Completed,
            1,
            None,
            None,
            "grok",
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent).await.unwrap();
        let orphan = snap
            .nodes
            .iter()
            .find(|n| n.status_reason.as_deref() == Some("orphan_observed"))
            .expect("orphan observed node");
        assert!(orphan.retained_observed);
        assert!(orphan.is_observed);
        assert!(!orphan.required);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains(&orphan_key));
        assert!(!json.contains("work_unit_key"));
    }

    #[test]
    fn soft_attach_on_error_returns_none() {
        // Soft-fail boundary used by conversation detail attachment.
        // Full get_folder_conversation_core integration is skipped here (heavy
        // parser/registry fixtures); projector soft-attach is the contract.
        let attached = soft_attach_workflow_graph(
            Result::<Option<WorkflowGraphSnapshot>, &str>::Err("injected failure"),
            42,
        );
        assert!(attached.is_none());
    }

    #[test]
    fn soft_attach_ok_passthrough() {
        let snap = WorkflowGraphSnapshot {
            schema_version: WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
            workflow_id: None,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
            manifest_revision: None,
            graph_revision: None,
            manifest_state: None,
            compatibility: WorkflowCompatibility::ObservedOnly,
            overall_state: WorkflowOverallState::ObservedOnly,
            current_phase_id: None,
            current_node_ids: vec![],
            phases: vec![],
            nodes: vec![],
            edges: vec![],
            gates: vec![],
        };
        let attached = soft_attach_workflow_graph(Ok::<_, &str>(Some(snap.clone())), 1);
        assert_eq!(attached, Some(snap));
        assert!(
            soft_attach_workflow_graph(Ok::<Option<WorkflowGraphSnapshot>, &str>(None), 1)
                .is_none()
        );
    }

    #[tokio::test]
    async fn soft_attach_corrupt_projector_path_returns_none() {
        // Corrupt active manifest → project_inner Ok(None) / soft path → None.
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-soft-corrupt");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        let rev = delegation_workflow_manifest_revision::Entity::find_by_id((
            pub_r.workflow_id.clone(),
            1,
        ))
        .one(&db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut am: delegation_workflow_manifest_revision::ActiveModel = rev.into();
        am.document_json = Set("{not-valid".into());
        am.update(&db.conn).await.unwrap();

        // project_workflow_graph_core routes through soft_attach.
        assert!(project_workflow_graph_core(&db, parent).await.is_none());
        // Explicit soft_attach on an error result (projector persistence failure path).
        assert!(soft_attach_workflow_graph(
            Result::<Option<WorkflowGraphSnapshot>, String>::Err("persistence: boom".into()),
            parent
        )
        .is_none());
    }

    #[tokio::test]
    async fn a9_key_matched_run_without_run_binding_overlays_node() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-a9-keymatch");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();

        // Binding key for task-1-impl from published manifest.
        let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        // Parent run with matching key but NO run_binding row.
        insert_run(
            &db,
            parent,
            "unbound-impl",
            Some(&key),
            DelegationRunStatus::Running,
            3,
            None,
            None,
            "grok",
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent).await.unwrap();
        let n = snap
            .nodes
            .iter()
            .find(|n| n.node_id == "task-1-impl")
            .expect("canonical impl node");
        assert_eq!(n.status, ProjectedNodeStatus::Running);
        assert!(n.run_count >= 1);
        assert_eq!(n.active_child_generation, Some(3));
        // Not an orphan row.
        assert_ne!(n.status_reason.as_deref(), Some("orphan_observed"));
        let _ = pub_r;
    }

    #[test]
    fn soft_attach_err_returns_none_without_propagating() {
        // Boundary: Err → None (never bubbles as conversation detail failure).
        let out: Option<WorkflowGraphSnapshot> = soft_attach_workflow_graph(
            Result::<Option<WorkflowGraphSnapshot>, &str>::Err("db down"),
            7,
        );
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn final_first_pass_uses_task_implementer_branch_tip() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-final-tip");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();

        let impl_summary = r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"done","commits":[],"concerns":[]}"#;
        let rev_summary = r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"ok"}"#;

        // Task implementer with tip digest HEAD-abc.
        insert_run(
            &db,
            parent,
            "impl-tip",
            None,
            DelegationRunStatus::Completed,
            2,
            Some(impl_summary),
            None,
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "impl-tip",
            &pub_r.workflow_id,
            "task-1-impl",
            1,
            true,
            Some("HEAD-abc"),
            None,
        )
        .await;

        // Final reviewer with mismatched tip → cannot pass first-pass.
        insert_run(
            &db,
            parent,
            "final-rev-bad",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(rev_summary),
            None,
            "codex",
        )
        .await;
        insert_run_binding(
            &db,
            "final-rev-bad",
            &pub_r.workflow_id,
            "final-reviewer",
            1,
            true,
            Some("WRONG-tip"),
            None,
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent).await.unwrap();
        let final_n = snap
            .nodes
            .iter()
            .find(|n| n.node_id == "final-reviewer")
            .expect("final reviewer");
        assert_ne!(
            final_n.status,
            ProjectedNodeStatus::Completed,
            "mismatched branch tip must block Final first-pass"
        );
        assert!(
            final_n
                .status_reason
                .as_deref()
                .is_some_and(|r| r.contains("artifact_digest") || r.contains("branch_tip")),
            "got reason {:?}",
            final_n.status_reason
        );
    }

    #[tokio::test]
    async fn final_first_pass_pending_when_tasks_exist_without_digest() {
        let (db, parent) = seed_parent().await;
        let doc = design_plan_doc("proj-final-pending");
        let pub_r = publish_workflow_manifest_core(
            &db,
            &emitter(),
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();

        // Task implementer present but no artifact_digest on binding.
        let impl_summary = r#"{"kind":"implementation","phase":"implementation","status":"done","summary":"done","commits":[],"concerns":[]}"#;
        insert_run(
            &db,
            parent,
            "impl-nodigest",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(impl_summary),
            None,
            "grok",
        )
        .await;
        insert_run_binding(
            &db,
            "impl-nodigest",
            &pub_r.workflow_id,
            "task-1-impl",
            1,
            true,
            None,
            None,
        )
        .await;

        let rev_summary = r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"ok"}"#;
        insert_run(
            &db,
            parent,
            "final-rev-early",
            None,
            DelegationRunStatus::Completed,
            1,
            Some(rev_summary),
            None,
            "codex",
        )
        .await;
        insert_run_binding(
            &db,
            "final-rev-early",
            &pub_r.workflow_id,
            "final-reviewer",
            1,
            true,
            Some("any-tip"),
            None,
        )
        .await;

        let snap = project_workflow_graph_core(&db, parent).await.unwrap();
        let final_n = snap
            .nodes
            .iter()
            .find(|n| n.node_id == "final-reviewer")
            .unwrap();
        assert_ne!(final_n.status, ProjectedNodeStatus::Completed);
        assert_eq!(final_n.status_reason.as_deref(), Some("branch_tip_pending"));
    }

    fn tip_impl_node(node_id: &str, task_index: u32, task_id: &str) -> WorkflowNodeSnapshot {
        WorkflowNodeSnapshot {
            node_id: node_id.into(),
            kind: "work_unit".into(),
            phase_id: Some("tasks".into()),
            role: Some("implementer".into()),
            agent_type: Some("grok".into()),
            model: None,
            effort: None,
            profile_id: None,
            task_index: Some(task_index),
            task_risk_level: None,
            task_risk_reason_codes: vec![],
            required_reviewer_count: None,
            returned_reviewer_count: None,
            title: None,
            status: ProjectedNodeStatus::Completed,
            status_reason: None,
            run_count: 1,
            active_child_generation: Some(1),
            replacement_count: 0,
            gate_cycle: None,
            round_count: None,
            latest_task_id: Some(task_id.into()),
            latest_child_conversation_id: None,
            latest_run_status: Some("completed".into()),
            started_at: None,
            finished_at: None,
            elapsed_completed_ms: None,
            tool_call_count: None,
            edit_tool_call_count: None,
            touched_file_count: None,
            touched_files_truncated: false,
            additions: None,
            deletions: None,
            line_counts_complete: None,
            summary: None,
            is_observed: true,
            retained_observed: false,
            required: true,
            node_outcome: None,
            deps: vec![],
        }
    }

    fn tip_impl_ev(task_id: &str, generation: i64, digest: &str) -> ExecutionGateRunEvidence {
        ExecutionGateRunEvidence {
            task_id: task_id.into(),
            generation,
            status: TerminalRunStatus::Completed,
            summary_validated: true,
            work_status: Some(crate::acp::delegation::card_summary::WorkStatus::Done),
            review_verdict: None,
            artifact_digest: Some(digest.into()),
            reviewed_task_id: None,
            reviewed_implementer_generation: None,
        }
    }

    #[tokio::test]
    async fn document_gate_projection_ignores_stale_settlement_and_evidence() {
        // After plan revision, cycle-1 approved on old manifest_revision must not
        // appear as the current complete gate, and returned counts must not reuse
        // old-cycle run_bindings.
        let (db, parent) = seed_parent().await;
        let em = emitter();
        let mut doc = design_plan_doc("tok-stale-gate");
        doc.workflow_state = ManifestWorkflowState::Approved;
        let r1 = publish_workflow_manifest_core(
            &db,
            &em,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();

        // Settlement on rev 1 (old plan fingerprint).
        let now = Utc::now();
        let h1 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let old_plan_fp = h1.plan_fingerprint.clone();
        let srow = delegation_workflow_gate_settlement::ActiveModel {
            workflow_id: Set(r1.workflow_id.clone()),
            gate_id: Set("plan".into()),
            gate_cycle: Set(1),
            manifest_revision: Set(1),
            structural_revision: Set(h1.structural_revision),
            content_fingerprint: Set(old_plan_fp.clone()),
            outcome: Set(GateSettlementOutcome::Approved),
            critical_count: Set(0),
            important_count: Set(0),
            minor_count: Set(0),
            summary: Set("old approve".into()),
            graph_revision_at_settle: Set(1),
            created_at: Set(now),
            ..Default::default()
        };
        srow.insert(&db.conn).await.unwrap();

        // Reviewer run_binding for cycle 1 / rev 1 (old fingerprint).
        let plan_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        insert_run(
            &db,
            parent,
            "plan-rev-old",
            Some(&plan_key),
            DelegationRunStatus::Completed,
            1,
            Some(
                r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"ok"}"#,
            ),
            None,
            "codex",
        )
        .await;
        let rb = delegation_workflow_run_binding::ActiveModel {
            task_id: Set("plan-rev-old".into()),
            workflow_id: Set(r1.workflow_id.clone()),
            node_id: Set("plan-reviewer-1".into()),
            gate_id: Set(Some("plan".into())),
            gate_cycle: Set(Some(1)),
            manifest_revision: Set(1),
            content_fingerprint: Set(Some(old_plan_fp)),
            artifact_digest: Set(Some("sha256:plan".into())),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(1),
            summary_validated: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
        };
        rb.insert(&db.conn).await.unwrap();

        // Structural plan revision → demote + new active rev.
        doc.workflow_id = Some(r1.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        doc.workflow_state = ManifestWorkflowState::Estimated;
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-v2".into();
        }
        let r2 = publish_workflow_manifest_core(
            &db,
            &em,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        assert_eq!(r2.manifest_revision, 2);

        let snap = project_workflow_graph_core(&db, parent)
            .await
            .expect("graph after plan revision");
        let plan_gate = snap
            .gates
            .iter()
            .find(|g| g.gate_id == "plan" || g.gate_id.contains("plan"))
            .expect("plan gate");
        assert!(
            plan_gate.latest_outcome.is_none(),
            "must not display rev-1 approved as current: {plan_gate:?}"
        );
        assert_eq!(
            plan_gate.returned_count, 0,
            "must not count stale cycle-1 reviewer evidence on new revision"
        );
    }

    #[tokio::test]
    async fn changes_requested_opens_next_cycle_without_recounting_settled() {
        // After changes_requested on cycle N, projection counts open cycle N+1
        // (empty until new runs) and still displays the non-approve settlement.
        let (db, parent) = seed_parent().await;
        let em = emitter();
        let mut doc = design_plan_doc("tok-cr-cycle");
        doc.workflow_state = ManifestWorkflowState::Estimated;
        let r1 = publish_workflow_manifest_core(
            &db,
            &em,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();

        let now = Utc::now();
        let h1 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let plan_fp = h1.plan_fingerprint.clone();
        let srow = delegation_workflow_gate_settlement::ActiveModel {
            workflow_id: Set(r1.workflow_id.clone()),
            gate_id: Set("plan".into()),
            gate_cycle: Set(1),
            manifest_revision: Set(1),
            structural_revision: Set(h1.structural_revision),
            content_fingerprint: Set(plan_fp.clone()),
            outcome: Set(GateSettlementOutcome::ChangesRequested),
            critical_count: Set(0),
            important_count: Set(1),
            minor_count: Set(0),
            summary: Set("need changes".into()),
            graph_revision_at_settle: Set(1),
            created_at: Set(now),
            ..Default::default()
        };
        srow.insert(&db.conn).await.unwrap();

        let plan_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        insert_run(
            &db,
            parent,
            "plan-rev-c1",
            Some(&plan_key),
            DelegationRunStatus::Completed,
            1,
            Some(
                r#"{"kind":"review","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"fix"}"#,
            ),
            None,
            "codex",
        )
        .await;
        let rb = delegation_workflow_run_binding::ActiveModel {
            task_id: Set("plan-rev-c1".into()),
            workflow_id: Set(r1.workflow_id.clone()),
            node_id: Set("plan-reviewer-1".into()),
            gate_id: Set(Some("plan".into())),
            gate_cycle: Set(Some(1)),
            manifest_revision: Set(1),
            content_fingerprint: Set(Some(plan_fp)),
            artifact_digest: Set(Some("sha256:plan".into())),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(1),
            summary_validated: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
        };
        rb.insert(&db.conn).await.unwrap();

        let snap = project_workflow_graph_core(&db, parent)
            .await
            .expect("graph after changes_requested");
        let plan_gate = snap
            .gates
            .iter()
            .find(|g| g.gate_id == "plan")
            .expect("plan gate");
        assert_eq!(
            plan_gate.latest_outcome.as_deref(),
            Some("changes_requested")
        );
        assert_eq!(plan_gate.latest_gate_cycle, Some(1));
        assert_eq!(
            plan_gate.returned_count, 0,
            "open cycle N+1 must not re-count cycle-1 completed runs"
        );
        assert_eq!(plan_gate.running_count, 0);
        let _ = doc;
    }

    #[tokio::test]
    async fn stale_content_fingerprint_runs_do_not_count_after_plan_rewrite() {
        // Stale plan fingerprint runs (prior structural generation) must not
        // inflate returned_count after plan fingerprint changes.
        let (db, parent) = seed_parent().await;
        let em = emitter();
        let mut doc = design_plan_doc("tok-stale-fp");
        doc.workflow_state = ManifestWorkflowState::Estimated;
        let r1 = publish_workflow_manifest_core(
            &db,
            &em,
            parent,
            PublishWorkflowRequest {
                document: doc.clone(),
            },
        )
        .await
        .unwrap();
        let h1 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let old_fp = h1.plan_fingerprint.clone();
        let design_fp = h1.design_fingerprint.clone();

        // Old plan run stamped with old fingerprint, cycle 1.
        let now = Utc::now();
        let plan_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: "docs/superpowers/plans/p.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        insert_run(
            &db,
            parent,
            "plan-old-fp",
            Some(&plan_key),
            DelegationRunStatus::Completed,
            1,
            Some(
                r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"ok"}"#,
            ),
            None,
            "codex",
        )
        .await;
        let rb = delegation_workflow_run_binding::ActiveModel {
            task_id: Set("plan-old-fp".into()),
            workflow_id: Set(r1.workflow_id.clone()),
            node_id: Set("plan-reviewer-1".into()),
            gate_id: Set(Some("plan".into())),
            gate_cycle: Set(Some(1)),
            manifest_revision: Set(1),
            content_fingerprint: Set(Some(old_fp.clone())),
            artifact_digest: Set(Some("sha256:plan".into())),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(1),
            summary_validated: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
        };
        rb.insert(&db.conn).await.unwrap();

        // Design settlement with matching design fingerprint (should survive plan rewrite).
        let drow = delegation_workflow_gate_settlement::ActiveModel {
            workflow_id: Set(r1.workflow_id.clone()),
            gate_id: Set("design".into()),
            gate_cycle: Set(1),
            manifest_revision: Set(1),
            structural_revision: Set(1),
            content_fingerprint: Set(design_fp.clone()),
            outcome: Set(GateSettlementOutcome::Approved),
            critical_count: Set(0),
            important_count: Set(0),
            minor_count: Set(0),
            summary: Set("design ok".into()),
            graph_revision_at_settle: Set(1),
            created_at: Set(now),
            ..Default::default()
        };
        drow.insert(&db.conn).await.unwrap();

        // Plan rewrite → new plan fingerprint; design fingerprint unchanged.
        doc.workflow_id = Some(r1.workflow_id.clone());
        doc.expected_manifest_revision = Some(1);
        if let Some(ref mut plan) = doc.plan {
            plan.digest = "sha256:plan-structural-v3".into();
        }
        let r2 = publish_workflow_manifest_core(
            &db,
            &em,
            parent,
            PublishWorkflowRequest { document: doc },
        )
        .await
        .unwrap();
        assert_eq!(r2.manifest_revision, 2);
        let h2 = delegation_workflow::Entity::find_by_id(r1.workflow_id.clone())
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(h2.plan_fingerprint, old_fp);
        assert_eq!(h2.design_fingerprint, design_fp);

        // Stale cycle-2 run still on old fingerprint should not count either.
        insert_run(
            &db,
            parent,
            "plan-old-fp-c2",
            Some(&plan_key),
            DelegationRunStatus::Completed,
            2,
            Some(
                r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"stale"}"#,
            ),
            None,
            "codex",
        )
        .await;
        let rb2 = delegation_workflow_run_binding::ActiveModel {
            task_id: Set("plan-old-fp-c2".into()),
            workflow_id: Set(r1.workflow_id.clone()),
            node_id: Set("plan-reviewer-1".into()),
            gate_id: Set(Some("plan".into())),
            gate_cycle: Set(Some(2)),
            manifest_revision: Set(2),
            content_fingerprint: Set(Some(old_fp)),
            artifact_digest: Set(Some("sha256:plan-structural-v3".into())),
            reviewed_task_id: Set(None),
            reviewed_implementer_generation: Set(None),
            lineage_ordinal: Set(2),
            summary_validated: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
        };
        rb2.insert(&db.conn).await.unwrap();

        let snap = project_workflow_graph_core(&db, parent)
            .await
            .expect("graph");
        let plan_gate = snap.gates.iter().find(|g| g.gate_id == "plan").unwrap();
        assert!(plan_gate.latest_outcome.is_none());
        assert_eq!(
            plan_gate.returned_count, 0,
            "stale fingerprint cycle-2 runs must not count after structural rev"
        );
        let design_gate = snap.gates.iter().find(|g| g.gate_id == "design").unwrap();
        assert_eq!(
            design_gate.latest_outcome.as_deref(),
            Some("approved"),
            "design settlement must remain valid when only plan fingerprint changes"
        );
    }

    #[tokio::test]
    async fn observed_only_synthetic_ids_are_deterministic_from_key() {
        let (db, parent) = seed_parent().await;
        let design_key = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: "docs/superpowers/specs/x.md",
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let final_key = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        insert_run(
            &db,
            parent,
            "d1",
            Some(&design_key),
            DelegationRunStatus::Completed,
            1,
            None,
            None,
            "codex",
        )
        .await;
        insert_run(
            &db,
            parent,
            "f1",
            Some(&final_key),
            DelegationRunStatus::Completed,
            1,
            None,
            None,
            "codex",
        )
        .await;

        let snap1 = project_workflow_graph_core(&db, parent)
            .await
            .expect("observed-only");
        let snap2 = project_workflow_graph_core(&db, parent)
            .await
            .expect("observed-only replay");
        let mut ids1: Vec<_> = snap1.nodes.iter().map(|n| n.node_id.clone()).collect();
        let mut ids2: Vec<_> = snap2.nodes.iter().map(|n| n.node_id.clone()).collect();
        ids1.sort();
        ids2.sort();
        assert_eq!(
            ids1, ids2,
            "synthetic ids must be stable across projections"
        );

        // Design/Final ids must embed key hash, not ordinal 0/1.
        let expected_design = format!("observed-design-{}", &sha256_hex_str(&design_key)[..12]);
        let expected_final = format!("observed-final-rev-{}", &sha256_hex_str(&final_key)[..12]);
        // PublicIdAllocator may pass through safe ids unchanged.
        assert!(
            ids1.iter().any(|id| id == &expected_design
                || id.contains(&expected_design[0..20.min(expected_design.len())])),
            "expected design id like {expected_design}, got {ids1:?}"
        );
        assert!(
            ids1.iter()
                .any(|id| id == &expected_final || id.contains("observed-final-rev")),
            "expected final id like {expected_final}, got {ids1:?}"
        );
    }

    #[test]
    fn derive_branch_tip_digest_unit() {
        let mut evidence = HashMap::new();
        let mut eligible = HashSet::new();
        let nodes = vec![tip_impl_node("task-1-impl", 1, "t1")];
        eligible.insert("task-1-impl".into());
        evidence.insert("task-1-impl".into(), tip_impl_ev("t1", 2, "tip-sha"));
        assert_eq!(
            derive_branch_tip_digest(&nodes, &evidence, &eligible),
            DerivedBranchTip::Digest("tip-sha".into())
        );
        // No eligible implementers → NoTasks.
        assert_eq!(
            derive_branch_tip_digest(&nodes, &evidence, &HashSet::new()),
            DerivedBranchTip::NoTasks
        );
        // Eligible implementer without digest → Pending.
        evidence.get_mut("task-1-impl").unwrap().artifact_digest = None;
        assert_eq!(
            derive_branch_tip_digest(&nodes, &evidence, &eligible),
            DerivedBranchTip::Pending
        );
    }

    #[test]
    fn derive_branch_tip_prefers_highest_task_index_not_cross_unit_generation() {
        // Regression: Task1 gen2 digest A, Task2 gen1 digest B → tip is B
        // (must NOT pick A via higher generation across work units).
        let nodes = vec![
            tip_impl_node("task-1-impl", 1, "t1"),
            tip_impl_node("task-2-impl", 2, "t2"),
        ];
        let mut eligible = HashSet::new();
        eligible.insert("task-1-impl".into());
        eligible.insert("task-2-impl".into());
        let mut evidence = HashMap::new();
        evidence.insert("task-1-impl".into(), tip_impl_ev("t1", 2, "digest-A"));
        evidence.insert("task-2-impl".into(), tip_impl_ev("t2", 1, "digest-B"));

        assert_eq!(
            derive_branch_tip_digest(&nodes, &evidence, &eligible),
            DerivedBranchTip::Digest("digest-B".into()),
            "highest task_index wins over higher generation on an earlier task"
        );
    }

    #[test]
    fn derive_branch_tip_ignores_failed_implementer() {
        let nodes = vec![tip_impl_node("task-1-impl", 1, "t1")];
        let mut eligible = HashSet::new();
        eligible.insert("task-1-impl".into());
        let mut evidence = HashMap::new();
        let mut ev = tip_impl_ev("t1", 1, "digest-fail");
        ev.status = TerminalRunStatus::Failed;
        evidence.insert("task-1-impl".into(), ev);
        assert_eq!(
            derive_branch_tip_digest(&nodes, &evidence, &eligible),
            DerivedBranchTip::Pending
        );
    }

    #[test]
    fn derive_branch_tip_highest_completed_empty_digest_is_pending_not_earlier() {
        // Regression: Task1 digest A completed, Task2 completed empty digest
        // → Pending (must NOT fall back to A by skipping empty-digest Task2).
        let nodes = vec![
            tip_impl_node("task-1-impl", 1, "t1"),
            tip_impl_node("task-2-impl", 2, "t2"),
        ];
        let mut eligible = HashSet::new();
        eligible.insert("task-1-impl".into());
        eligible.insert("task-2-impl".into());
        let mut evidence = HashMap::new();
        evidence.insert("task-1-impl".into(), tip_impl_ev("t1", 1, "digest-A"));
        let mut t2 = tip_impl_ev("t2", 1, "ignored");
        t2.artifact_digest = None;
        evidence.insert("task-2-impl".into(), t2);

        assert_eq!(
            derive_branch_tip_digest(&nodes, &evidence, &eligible),
            DerivedBranchTip::Pending,
            "highest completed task_index wins even with empty digest → Pending, not earlier A"
        );
    }
}
