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
    redact_display_string, redact_optional_display, ProjectedNodeStatus, WorkflowCompatibility,
    WorkflowEdgeSnapshot, WorkflowGateSnapshot, WorkflowGraphSnapshot, WorkflowNodeSnapshot,
    WorkflowOverallState, WorkflowPhaseSnapshot, WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
};
use super::gates::{
    evaluate_execution_gate, ExecutionGateInput, ExecutionGateKind, ExecutionGateRunEvidence,
    TerminalRunStatus,
};
use super::key::parse_recognized_work_unit_key;
use super::types::{
    ManifestDocument, ManifestNodeKind, ManifestNodeRole, ManifestWorkflowState, ParsedWorkUnitKey,
    ResolutionMode, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};
use super::validate::validate_manifest_document;

/// Project a redacted `WorkflowGraphSnapshot` for `parent_conversation_id`.
///
/// Returns `None` when there is no graph (no manifest and no recognized A1
/// keys), when the manifest is corrupt/unsupported, or on persistence errors.
/// Never panics; callers may attach the result to conversation detail safely.
pub async fn project_workflow_graph_core(
    db: &AppDatabase,
    parent_conversation_id: i32,
) -> Option<WorkflowGraphSnapshot> {
    match project_inner(&db.conn, parent_conversation_id).await {
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
        .filter(
            delegation_workflow::Column::WorkflowKind.eq(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY),
        )
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
        .filter(
            delegation_workflow_node_binding::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .all(conn)
        .await
        .map_err(db_err)?;

    let run_bindings = delegation_workflow_run_binding::Entity::find()
        .filter(
            delegation_workflow_run_binding::Column::WorkflowId.eq(header.workflow_id.clone()),
        )
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .all(conn)
        .await
        .map_err(db_err)?;

    let settlements = delegation_workflow_gate_settlement::Entity::find()
        .filter(
            delegation_workflow_gate_settlement::Column::WorkflowId
                .eq(header.workflow_id.clone()),
        )
        .order_by_asc(delegation_workflow_gate_settlement::Column::GateCycle)
        .all(conn)
        .await
        .map_err(db_err)?;

    // All task_ids referenced by run bindings for this workflow.
    let all_task_ids: Vec<String> = run_bindings.iter().map(|rb| rb.task_id.clone()).collect();
    let runs = if all_task_ids.is_empty() {
        Vec::new()
    } else {
        delegation_task_run::Entity::find()
            .filter(delegation_task_run::Column::TaskId.is_in(all_task_ids.clone()))
            .all(conn)
            .await
            .map_err(db_err)?
    };
    let run_by_id: HashMap<String, &delegation_task_run::Model> =
        runs.iter().map(|r| (r.task_id.clone(), r)).collect();

    // Group run bindings by node_id, ordered by lineage_ordinal desc already.
    let mut rbs_by_node: HashMap<String, Vec<&delegation_workflow_run_binding::Model>> =
        HashMap::new();
    for rb in &run_bindings {
        rbs_by_node.entry(rb.node_id.clone()).or_default().push(rb);
    }

    // Manifest node lookup for deps / titles / kinds.
    let manifest_node_by_id: HashMap<String, &super::types::NormalizedNode> =
        normalized.nodes.iter().map(|n| (n.id.clone(), n)).collect();

    let mut nodes: Vec<WorkflowNodeSnapshot> = Vec::new();

    // Active + retained bindings drive the node set (plus any still on manifest).
    let mut seen_node_ids: HashSet<String> = HashSet::new();
    for b in &bindings {
        seen_node_ids.insert(b.node_id.clone());
        let rbs = rbs_by_node.get(&b.node_id).map(|v| v.as_slice()).unwrap_or(&[]);
        let mn = manifest_node_by_id.get(&b.node_id).copied();
        nodes.push(project_node_from_binding(b, mn, rbs, &run_by_id));
    }

    // Manifest-only estimated nodes not yet in bindings (should be rare after publish).
    for mn in &normalized.nodes {
        if seen_node_ids.contains(&mn.id) {
            continue;
        }
        if !matches!(mn.kind, ManifestNodeKind::WorkUnit) {
            // Still project milestones/placeholders as estimated structure.
        }
        nodes.push(project_node_from_manifest_only(mn));
    }

    // Overlay Task execution-gate derived status on task pairs.
    apply_task_execution_gate_overlay(&mut nodes);

    // Document gate snapshots.
    let mut gate_snaps: Vec<WorkflowGateSnapshot> = Vec::new();
    for g in &normalized.gates {
        let gate_settlements: Vec<_> = settlements.iter().filter(|s| s.gate_id == g.id).collect();
        let latest = gate_settlements.last();
        let mut returned = 0u64;
        let mut running = 0u64;
        let mut blocked = 0u64;
        for nid in &g.required_reviewer_node_ids {
            if let Some(n) = nodes.iter().find(|n| n.node_id == *nid) {
                match n.status {
                    ProjectedNodeStatus::Completed => returned += 1,
                    ProjectedNodeStatus::Reserving | ProjectedNodeStatus::Running => running += 1,
                    ProjectedNodeStatus::Blocked
                    | ProjectedNodeStatus::Failed
                    | ProjectedNodeStatus::MissingSummary => blocked += 1,
                    _ => {}
                }
            }
        }
        gate_snaps.push(WorkflowGateSnapshot {
            gate_id: g.id.clone(),
            gate_kind: match g.gate_kind {
                super::types::DocumentGateKind::Design => "design".into(),
                super::types::DocumentGateKind::Plan => "plan".into(),
            },
            resolution_mode: match g.resolution_mode {
                ResolutionMode::ParentAdjudication => "parent_adjudication".into(),
                ResolutionMode::SelfReview => "self_review".into(),
            },
            required_reviewer_node_ids: g.required_reviewer_node_ids.clone(),
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
            id: p.id.clone(),
            kind: p.kind.clone(),
            title: redact_optional_display(p.title.as_deref()),
        })
        .collect();

    let edges: Vec<WorkflowEdgeSnapshot> = normalized
        .edges
        .iter()
        .map(|e| WorkflowEdgeSnapshot {
            id: e.id.clone(),
            from: e.from.clone(),
            to: e.to.clone(),
        })
        .collect();

    let (current_node_ids, current_phase_id) =
        select_current_nodes(&nodes, &gate_snaps, &settlements);
    let overall_state = derive_overall_state(&header.workflow_state, &nodes);

    Ok(Some(WorkflowGraphSnapshot {
        schema_version: WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
        workflow_id: Some(header.workflow_id.clone()),
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

fn project_node_from_binding(
    b: &delegation_workflow_node_binding::Model,
    mn: Option<&super::types::NormalizedNode>,
    rbs: &[&delegation_workflow_run_binding::Model],
    run_by_id: &HashMap<String, &delegation_task_run::Model>,
) -> WorkflowNodeSnapshot {
    let latest_rb = rbs.first().copied();
    let latest_run = latest_rb.and_then(|rb| run_by_id.get(&rb.task_id).copied());

    let run_count = rbs.len() as u64;
    let replacement_count = rbs
        .iter()
        .filter_map(|rb| run_by_id.get(&rb.task_id))
        .filter(|r| r.replaced_task_id.is_some())
        .count() as u64;

    let (status, status_reason, summary) = project_node_status(b, latest_rb, latest_run);

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
    let deps = mn.map(|n| n.deps.clone()).unwrap_or_default();
    let title = mn
        .and_then(|n| n.title.as_deref())
        .map(redact_display_string)
        .or_else(|| None);
    let required = mn.map(|n| n.required).unwrap_or(true);

    WorkflowNodeSnapshot {
        node_id: b.node_id.clone(),
        kind,
        phase_id: Some(b.phase_id.clone()),
        role: Some(b.role.clone()),
        agent_type: Some(b.agent_type.clone()),
        profile_id: b.profile_id.clone(),
        task_index: b.task_index.map(|i| i as u32),
        title,
        status,
        status_reason,
        run_count,
        active_child_generation,
        replacement_count,
        gate_cycle: latest_rb.and_then(|rb| rb.gate_cycle),
        round_count,
        latest_task_id: latest_rb.map(|rb| rb.task_id.clone()),
        latest_child_conversation_id: latest_run.map(|r| r.child_conversation_id),
        latest_run_status: latest_run.map(|r| run_status_str(&r.status).to_string()),
        summary,
        is_observed: b.is_observed,
        retained_observed: b.retained_observed,
        required,
        node_outcome: b.node_outcome.as_ref().map(|o| match o {
            NodeOutcome::Canceled => "canceled".to_string(),
        }),
        deps,
    }
}

fn project_node_from_manifest_only(mn: &super::types::NormalizedNode) -> WorkflowNodeSnapshot {
    WorkflowNodeSnapshot {
        node_id: mn.id.clone(),
        kind: node_kind_str(mn.kind).to_string(),
        phase_id: mn.phase_id.clone(),
        role: mn.role.map(role_str).map(|s| s.to_string()),
        agent_type: mn.agent_type.clone(),
        profile_id: mn.profile_id.clone(),
        task_index: mn.task_index,
        title: mn.title.as_deref().map(redact_display_string),
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
        summary: None,
        is_observed: false,
        retained_observed: false,
        required: mn.required,
        node_outcome: mn.node_outcome.map(|_| "canceled".to_string()),
        deps: mn.deps.clone(),
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

    let summary_validated = latest_rb.map(|rb| rb.summary_validated).unwrap_or(false);

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
        CardSummary::Implementation { summary, .. } => Some(summary.clone()),
    }
}

/// For each Task index, if implementer+reviewer both present, evaluate the
/// execution gate and stamp status_reason on the pair when it fails.
fn apply_task_execution_gate_overlay(nodes: &mut [WorkflowNodeSnapshot]) {
    let mut by_task: HashMap<u32, (Option<usize>, Option<usize>)> = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        let Some(idx) = n.task_index else { continue };
        let entry = by_task.entry(idx).or_insert((None, None));
        match n.role.as_deref() {
            Some("implementer") => entry.0 = Some(i),
            Some("reviewer") => entry.1 = Some(i),
            _ => {}
        }
    }

    // We need evidence from latest runs — re-read from node snapshot fields is
    // insufficient for B3/B13. Overlay only marks gate-passed via completed pair;
    // full gate eval is available via `evaluate_execution_gate` for admission
    // (Task 6). Here we only use coarse completed-pair detection.
    for (_task_idx, (impl_i, rev_i)) in by_task {
        let (Some(ii), Some(ri)) = (impl_i, rev_i) else {
            continue;
        };
        let impl_done = matches!(nodes[ii].status, ProjectedNodeStatus::Completed);
        let rev_done = matches!(nodes[ri].status, ProjectedNodeStatus::Completed);
        if impl_done && !rev_done {
            if matches!(
                nodes[ri].status,
                ProjectedNodeStatus::Estimated | ProjectedNodeStatus::Superseded
            ) {
                nodes[ri].status = ProjectedNodeStatus::WaitingReview;
            }
        }
        let _ = (impl_done, rev_done);
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
            let approved = g
                .latest_outcome
                .as_deref()
                .is_some_and(|o| o == "approved");
            if !approved {
                // Check if any settlement approved this gate ever for latest cycle
                let has_approve = settlements.iter().any(|s| {
                    s.gate_id == g.gate_id
                        && matches!(s.outcome, GateSettlementOutcome::Approved)
                        && g.latest_gate_cycle
                            .is_some_and(|c| s.gate_cycle == c)
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
) -> WorkflowOverallState {
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
    if !nodes.is_empty()
        && nodes
            .iter()
            .filter(|n| n.required)
            .all(|n| matches!(n.status, ProjectedNodeStatus::Completed))
    {
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

    for (key, mut key_runs) in by_key {
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

        // Synthetic node_id from role/task — never expose raw key.
        let node_id = synthetic_node_id(&parsed, nodes.len());

        nodes.push(WorkflowNodeSnapshot {
            node_id,
            kind: "work_unit".into(),
            phase_id: Some(phase_id),
            role: Some(role),
            agent_type: Some(latest.agent_type.clone()),
            profile_id: latest.profile_id.clone(),
            task_index,
            title: None,
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
            latest_task_id: Some(latest.task_id.clone()),
            latest_child_conversation_id: Some(latest.child_conversation_id),
            latest_run_status: Some(run_status_str(&latest.status).to_string()),
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
            id: id.clone(),
            kind: Some(id),
            title: None,
        })
        .collect();
    phases.sort_by(|a, b| a.id.cmp(&b.id));

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
        ParsedWorkUnitKey::Plan { .. } => ("plan".into(), "reviewer".into(), None),
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

fn synthetic_node_id(parsed: &ParsedWorkUnitKey, ordinal: usize) -> String {
    match parsed {
        ParsedWorkUnitKey::Design { .. } => format!("observed-design-{ordinal}"),
        ParsedWorkUnitKey::Plan { .. } => format!("observed-plan-{ordinal}"),
        ParsedWorkUnitKey::TaskImplementer { task_index, .. } => {
            format!("observed-task-{task_index}-impl")
        }
        ParsedWorkUnitKey::TaskReviewer { task_index, .. } => {
            format!("observed-task-{task_index}-rev")
        }
        ParsedWorkUnitKey::FinalReviewer { .. } => format!("observed-final-rev-{ordinal}"),
        ParsedWorkUnitKey::FinalFixer { .. } => format!("observed-final-fix-{ordinal}"),
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
        Some(CardSummary::Implementation { status, .. }) => (Some(status), None),
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
    implementer: Option<(&delegation_task_run::Model, &delegation_workflow_run_binding::Model)>,
    reviewer: Option<(&delegation_task_run::Model, &delegation_workflow_run_binding::Model)>,
) -> super::gates::ExecutionGateEval {
    evaluate_execution_gate(&ExecutionGateInput {
        kind: ExecutionGateKind::Task,
        implementer_or_fixer: implementer.map(|(r, b)| evidence_from_run_and_binding(r, b)),
        reviewer: reviewer.map(|(r, b)| evidence_from_run_and_binding(r, b)),
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
    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::store::{
        publish_workflow_manifest_core, PublishWorkflowRequest,
    };
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestEdge, ManifestGate, ManifestNode, ManifestPhase,
        WorkUnitKeyParts, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS,
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
            created_at: Set(now),
            updated_at: Set(now),
        };
        run.insert(&db.conn).await.expect("insert run");
        child
    }

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
                delegation_workflow_node_binding::Column::WorkflowId
                    .eq(pub_r.workflow_id.clone()),
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
        assert_eq!(snap.workflow_id.as_deref(), Some(pub_r.workflow_id.as_str()));
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
        assert!(impl_node.summary.as_deref().is_some_and(|s| s.contains("implemented")));
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
}
