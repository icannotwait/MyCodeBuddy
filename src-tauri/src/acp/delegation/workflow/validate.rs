//! Manifest document validation (A1/A12/A15 + graph integrity).

use std::collections::{HashMap, HashSet, VecDeque};

use super::key::{build_work_unit_key, normalize_rel_path};
use super::types::{
    DocumentRef, ManifestDocument, ManifestGate, ManifestNode, ManifestNodeKind,
    ManifestNodeRole, ManifestPhase, NormalizedGate, NormalizedManifest, NormalizedNode,
    ResolutionMode, WorkUnitKeyParts, WorkflowError, MAX_EDGES, MAX_GATES, MAX_MANIFEST_JSON_BYTES,
    MAX_NODES, MAX_TASKS, MANIFEST_SCHEMA_VERSION, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
};

/// Validate a raw manifest document and return the normalized form.
pub fn validate_manifest_document(
    doc: &ManifestDocument,
) -> Result<NormalizedManifest, WorkflowError> {
    if doc.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(WorkflowError::InvalidSchemaVersion(doc.schema_version));
    }
    if doc.workflow_kind != WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY {
        return Err(WorkflowError::UnsupportedWorkflowKind(
            doc.workflow_kind.clone(),
        ));
    }
    if doc.publication_token.trim().is_empty() {
        return Err(WorkflowError::MissingField("publication_token".into()));
    }
    if doc.publication_token.contains('|') {
        return Err(WorkflowError::InvalidField(
            "publication_token must not contain '|'".into(),
        ));
    }

    // A15.2 structural bounds.
    if doc.nodes.len() > MAX_NODES {
        return Err(WorkflowError::BoundsExceeded(format!(
            "nodes {} > {MAX_NODES}",
            doc.nodes.len()
        )));
    }
    if doc.edges.len() > MAX_EDGES {
        return Err(WorkflowError::BoundsExceeded(format!(
            "edges {} > {MAX_EDGES}",
            doc.edges.len()
        )));
    }
    if doc.gates.len() > MAX_GATES {
        return Err(WorkflowError::BoundsExceeded(format!(
            "gates {} > {MAX_GATES}",
            doc.gates.len()
        )));
    }

    let json_bytes = serde_json::to_vec(doc).map_err(|e| {
        WorkflowError::InvalidField(format!("manifest not serializable: {e}"))
    })?;
    if json_bytes.len() > MAX_MANIFEST_JSON_BYTES {
        return Err(WorkflowError::BoundsExceeded(format!(
            "manifest JSON {} > {MAX_MANIFEST_JSON_BYTES}",
            json_bytes.len()
        )));
    }

    let design = normalize_document_ref(doc.design.as_ref())?;
    let plan = normalize_document_ref(doc.plan.as_ref())?;

    let mut phase_ids = HashSet::new();
    let mut phases = Vec::with_capacity(doc.phases.len());
    for phase in &doc.phases {
        if phase.id.trim().is_empty() {
            return Err(WorkflowError::InvalidField("phase id is empty".into()));
        }
        if !phase_ids.insert(phase.id.clone()) {
            return Err(WorkflowError::DuplicateId(format!("phase:{}", phase.id)));
        }
        phases.push(ManifestPhase {
            id: phase.id.clone(),
            kind: phase.kind.clone(),
            title: phase.title.clone(),
        });
    }

    let mut node_ids = HashSet::new();
    let mut work_unit_keys = HashSet::new();
    let mut task_indices = HashSet::new();
    let mut nodes = Vec::with_capacity(doc.nodes.len());

    for node in &doc.nodes {
        let normalized = normalize_node(node, design.as_ref(), plan.as_ref(), &phase_ids)?;
        if !node_ids.insert(normalized.id.clone()) {
            return Err(WorkflowError::DuplicateId(format!("node:{}", normalized.id)));
        }
        if let Some(ref key) = normalized.work_unit_key {
            if !work_unit_keys.insert(key.clone()) {
                return Err(WorkflowError::DuplicateId(format!("work_unit_key:{key}")));
            }
        }
        if let Some(idx) = normalized.task_index {
            task_indices.insert(idx);
        }
        nodes.push(normalized);
    }

    if task_indices.len() > MAX_TASKS {
        return Err(WorkflowError::BoundsExceeded(format!(
            "tasks {} > {MAX_TASKS}",
            task_indices.len()
        )));
    }
    for idx in &task_indices {
        if *idx == 0 || *idx as usize > MAX_TASKS {
            return Err(WorkflowError::InvalidTaskIndex(format!(
                "task_index {idx} out of range 1..={MAX_TASKS}"
            )));
        }
    }

    // Validate deps reference known nodes.
    let node_id_set: HashSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    for node in &nodes {
        for dep in &node.deps {
            if !node_id_set.contains(dep.as_str()) {
                return Err(WorkflowError::UnknownReference(format!(
                    "deps {dep} on node {}",
                    node.id
                )));
            }
            if dep == &node.id {
                return Err(WorkflowError::Cycle);
            }
        }
    }

    let mut edges = Vec::with_capacity(doc.edges.len());
    let mut edge_ids = HashSet::new();
    for edge in &doc.edges {
        if !node_id_set.contains(edge.from.as_str()) {
            return Err(WorkflowError::UnknownReference(format!(
                "edge.from {}",
                edge.from
            )));
        }
        if !node_id_set.contains(edge.to.as_str()) {
            return Err(WorkflowError::UnknownReference(format!(
                "edge.to {}",
                edge.to
            )));
        }
        if edge.from == edge.to {
            return Err(WorkflowError::Cycle);
        }
        if let Some(ref id) = edge.id {
            if !edge_ids.insert(id.clone()) {
                return Err(WorkflowError::DuplicateId(format!("edge:{id}")));
            }
        }
        edges.push(edge.clone());
    }

    // Acyclic: union of explicit edges + node.deps (deps means "depends on" → edge dep → node).
    ensure_acyclic(&nodes, &edges)?;

    let mut gate_ids = HashSet::new();
    let mut gates = Vec::with_capacity(doc.gates.len());
    for gate in &doc.gates {
        let normalized = normalize_gate(gate, &node_id_set, &nodes, design.as_ref())?;
        if !gate_ids.insert(normalized.id.clone()) {
            return Err(WorkflowError::DuplicateId(format!("gate:{}", normalized.id)));
        }
        gates.push(normalized);
    }

    Ok(NormalizedManifest {
        schema_version: doc.schema_version,
        workflow_kind: doc.workflow_kind.clone(),
        workflow_id: doc.workflow_id.clone(),
        expected_manifest_revision: doc.expected_manifest_revision,
        publication_token: doc.publication_token.clone(),
        workflow_state: doc.workflow_state,
        design,
        plan,
        phases,
        nodes,
        edges,
        gates,
        task_count: task_indices.len(),
    })
}

fn normalize_document_ref(
    doc: Option<&DocumentRef>,
) -> Result<Option<DocumentRef>, WorkflowError> {
    let Some(doc) = doc else {
        return Ok(None);
    };
    if doc.digest.trim().is_empty() {
        return Err(WorkflowError::MissingField("document digest".into()));
    }
    let rel_path = normalize_rel_path(&doc.rel_path)?;
    Ok(Some(DocumentRef {
        rel_path,
        digest: doc.digest.clone(),
    }))
}

fn normalize_node(
    node: &ManifestNode,
    design: Option<&DocumentRef>,
    plan: Option<&DocumentRef>,
    phase_ids: &HashSet<String>,
) -> Result<NormalizedNode, WorkflowError> {
    if node.id.trim().is_empty() {
        return Err(WorkflowError::InvalidField("node id is empty".into()));
    }
    if let Some(ref phase_id) = node.phase_id {
        if !phase_ids.is_empty() && !phase_ids.contains(phase_id) {
            return Err(WorkflowError::UnknownReference(format!(
                "phase_id {phase_id} on node {}",
                node.id
            )));
        }
    }

    let required = node.required.unwrap_or(true);

    match node.kind {
        ManifestNodeKind::WorkUnit => normalize_work_unit(node, design, plan, required),
        ManifestNodeKind::Milestone | ManifestNodeKind::Gate | ManifestNodeKind::Placeholder => {
            if node.work_unit_key.is_some() {
                return Err(WorkflowError::InvalidField(format!(
                    "non-work-unit node {} must not carry work_unit_key",
                    node.id
                )));
            }
            Ok(NormalizedNode {
                id: node.id.clone(),
                kind: node.kind,
                phase_id: node.phase_id.clone(),
                role: node.role,
                agent_type: node.agent_type.clone(),
                profile_id: node.profile_id.clone(),
                task_index: node.task_index,
                work_unit_key: None,
                deps: node.deps.clone(),
                required,
                node_outcome: node.node_outcome,
                title: node.title.clone(),
            })
        }
    }
}

fn normalize_work_unit(
    node: &ManifestNode,
    design: Option<&DocumentRef>,
    plan: Option<&DocumentRef>,
    required: bool,
) -> Result<NormalizedNode, WorkflowError> {
    let role = node.role.ok_or_else(|| {
        WorkflowError::MissingField(format!("role on work unit {}", node.id))
    })?;
    let agent_type = node
        .agent_type
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            WorkflowError::MissingField(format!("agent_type on work unit {}", node.id))
        })?;

    let profile_ref = node.profile_id.as_deref();
    let parts = classify_work_unit_parts(node, design, plan, role, agent_type, profile_ref)?;
    let expected_key = build_work_unit_key(&parts)?;
    let submitted = node.work_unit_key.as_deref().ok_or_else(|| {
        WorkflowError::MissingField(format!("work_unit_key on work unit {}", node.id))
    })?;
    if submitted != expected_key {
        return Err(WorkflowError::KeyMismatch {
            node_id: node.id.clone(),
            expected: expected_key,
            got: submitted.to_string(),
        });
    }

    Ok(NormalizedNode {
        id: node.id.clone(),
        kind: ManifestNodeKind::WorkUnit,
        phase_id: node.phase_id.clone(),
        role: Some(role),
        agent_type: Some(agent_type.to_string()),
        profile_id: node.profile_id.clone(),
        task_index: node.task_index,
        work_unit_key: Some(expected_key),
        deps: node.deps.clone(),
        required,
        node_outcome: node.node_outcome,
        title: node.title.clone(),
    })
}

fn classify_work_unit_parts<'a>(
    node: &'a ManifestNode,
    design: Option<&'a DocumentRef>,
    plan: Option<&'a DocumentRef>,
    role: ManifestNodeRole,
    agent_type: &'a str,
    profile_ref: Option<&'a str>,
) -> Result<WorkUnitKeyParts<'a>, WorkflowError> {
    let phase = node.phase_id.as_deref();

    // Task-scoped nodes take priority when task_index is set.
    if let Some(idx) = node.task_index {
        return match role {
            ManifestNodeRole::Implementer => Ok(WorkUnitKeyParts::TaskImplementer {
                task_index: idx,
                agent_type,
                profile_id: profile_ref,
            }),
            ManifestNodeRole::Reviewer => Ok(WorkUnitKeyParts::TaskReviewer {
                task_index: idx,
                agent_type,
                profile_id: profile_ref,
            }),
            ManifestNodeRole::Fixer => Err(WorkflowError::RoleMismatch(format!(
                "fixer node {} must not have task_index",
                node.id
            ))),
        };
    }

    match (role, phase) {
        (ManifestNodeRole::Reviewer, Some("design")) => {
            let design_doc = design.ok_or_else(|| {
                WorkflowError::MissingField(format!(
                    "design document required for node {}",
                    node.id
                ))
            })?;
            Ok(WorkUnitKeyParts::Design {
                rel_doc_path: &design_doc.rel_path,
                agent_type,
                profile_id: profile_ref,
            })
        }
        (ManifestNodeRole::Reviewer, Some("plan")) => {
            let plan_doc = plan.ok_or_else(|| {
                WorkflowError::MissingField(format!(
                    "plan document required for node {}",
                    node.id
                ))
            })?;
            Ok(WorkUnitKeyParts::Plan {
                rel_plan_path: &plan_doc.rel_path,
                agent_type,
                profile_id: profile_ref,
            })
        }
        (ManifestNodeRole::Reviewer, Some("final")) => Ok(WorkUnitKeyParts::FinalReviewer {
            agent_type,
            profile_id: profile_ref,
        }),
        (ManifestNodeRole::Fixer, Some("final")) => Ok(WorkUnitKeyParts::FinalFixer {
            agent_type,
            profile_id: profile_ref,
        }),
        (ManifestNodeRole::Fixer, Some(other)) => Err(WorkflowError::RoleMismatch(format!(
            "fixer node {} must be in final phase, got {other}",
            node.id
        ))),
        (ManifestNodeRole::Fixer, None) => Ok(WorkUnitKeyParts::FinalFixer {
            agent_type,
            profile_id: profile_ref,
        }),
        (ManifestNodeRole::Reviewer, None) => {
            // Disambiguate only when exactly one document is present.
            match (design, plan) {
                (Some(design_doc), None) => Ok(WorkUnitKeyParts::Design {
                    rel_doc_path: &design_doc.rel_path,
                    agent_type,
                    profile_id: profile_ref,
                }),
                (None, Some(plan_doc)) => Ok(WorkUnitKeyParts::Plan {
                    rel_plan_path: &plan_doc.rel_path,
                    agent_type,
                    profile_id: profile_ref,
                }),
                (Some(_), Some(_)) => Err(WorkflowError::RoleMismatch(format!(
                    "reviewer node {} needs phase_id to disambiguate design vs plan",
                    node.id
                ))),
                (None, None) => Err(WorkflowError::MissingField(format!(
                    "design or plan document for reviewer node {}",
                    node.id
                ))),
            }
        }
        (ManifestNodeRole::Implementer, _) => Err(WorkflowError::MissingField(format!(
            "task_index on implementer node {}",
            node.id
        ))),
        (ManifestNodeRole::Reviewer, Some(other)) => Err(WorkflowError::RoleMismatch(format!(
            "reviewer node {} has unsupported phase_id {other}",
            node.id
        ))),
    }
}

fn normalize_gate(
    gate: &ManifestGate,
    node_ids: &HashSet<&str>,
    nodes: &[NormalizedNode],
    design: Option<&DocumentRef>,
) -> Result<NormalizedGate, WorkflowError> {
    if gate.id.trim().is_empty() {
        return Err(WorkflowError::InvalidField("gate id is empty".into()));
    }

    let mut seen_reviewers = HashSet::new();
    for reviewer_id in &gate.required_reviewer_node_ids {
        if !node_ids.contains(reviewer_id.as_str()) {
            return Err(WorkflowError::UnknownReference(format!(
                "gate reviewer {reviewer_id}"
            )));
        }
        if !seen_reviewers.insert(reviewer_id.clone()) {
            return Err(WorkflowError::DuplicateId(format!(
                "gate {} reviewer {reviewer_id}",
                gate.id
            )));
        }
        // Reviewer refs should be work units with reviewer role when present.
        if let Some(node) = nodes.iter().find(|n| n.id == *reviewer_id) {
            if node.kind == ManifestNodeKind::WorkUnit
                && node.role.is_some()
                && node.role != Some(ManifestNodeRole::Reviewer)
            {
                return Err(WorkflowError::RoleMismatch(format!(
                    "gate {} reviewer {reviewer_id} is not a reviewer role",
                    gate.id
                )));
            }
        }
    }

    let gate_kind = gate.gate_kind.as_deref();
    let is_design_gate = gate_kind == Some("design")
        || gate.id == "design"
        || gate.id.starts_with("design_")
        || gate.id.starts_with("design-");
    let is_plan_gate = gate_kind == Some("plan")
        || gate.id == "plan"
        || gate.id.starts_with("plan_")
        || gate.id.starts_with("plan-");

    // A12: zero-reviewer Design only with self_review + design doc present.
    // Plan gates cannot be empty.
    if gate.required_reviewer_node_ids.is_empty() {
        if is_plan_gate {
            return Err(WorkflowError::InvalidGateShape(format!(
                "plan gate {} cannot have empty required_reviewer_node_ids",
                gate.id
            )));
        }
        if gate.resolution_mode != ResolutionMode::SelfReview {
            return Err(WorkflowError::InvalidGateShape(format!(
                "zero-reviewer gate {} requires resolution_mode=self_review",
                gate.id
            )));
        }
        if !is_design_gate && gate_kind.is_some() {
            return Err(WorkflowError::InvalidGateShape(format!(
                "zero-reviewer self_review only allowed for Design gate {}",
                gate.id
            )));
        }
        if design.is_none() {
            return Err(WorkflowError::InvalidGateShape(format!(
                "zero-reviewer Design gate {} requires design document path+digest",
                gate.id
            )));
        }
    } else if gate.resolution_mode == ResolutionMode::SelfReview {
        return Err(WorkflowError::InvalidGateShape(format!(
            "gate {} self_review requires empty required_reviewer_node_ids",
            gate.id
        )));
    }

    Ok(NormalizedGate {
        id: gate.id.clone(),
        required_reviewer_node_ids: gate.required_reviewer_node_ids.clone(),
        resolution_mode: gate.resolution_mode,
        gate_kind: gate.gate_kind.clone(),
    })
}

fn ensure_acyclic(
    nodes: &[NormalizedNode],
    edges: &[super::types::ManifestEdge],
) -> Result<(), WorkflowError> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut indegree: HashMap<&str, usize> = HashMap::new();

    for node in nodes {
        indegree.entry(node.id.as_str()).or_insert(0);
        adj.entry(node.id.as_str()).or_default();
    }

    for edge in edges {
        let from = edge.from.as_str();
        let to = edge.to.as_str();
        adj.entry(from).or_default().push(to);
        *indegree.entry(to).or_insert(0) += 1;
        indegree.entry(from).or_insert(0);
    }
    for node in nodes {
        for dep in &node.deps {
            // dep → node (node depends on dep)
            let from = dep.as_str();
            let to = node.id.as_str();
            adj.entry(from).or_default().push(to);
            *indegree.entry(to).or_insert(0) += 1;
            indegree.entry(from).or_insert(0);
        }
    }

    let mut queue: VecDeque<&str> = indegree
        .iter()
        .filter_map(|(id, deg)| if *deg == 0 { Some(*id) } else { None })
        .collect();

    let mut seen = 0usize;
    while let Some(id) = queue.pop_front() {
        seen += 1;
        if let Some(nexts) = adj.get(id) {
            for next in nexts {
                if let Some(deg) = indegree.get_mut(*next) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(*next);
                    }
                }
            }
        }
    }

    if seen != indegree.len() {
        return Err(WorkflowError::Cycle);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::workflow::types::{
        ManifestEdge, ManifestNodeOutcome, ManifestWorkflowState, ResolutionMode,
    };

    fn minimal_valid_doc() -> ManifestDocument {
        let design_path = "docs/superpowers/specs/x.md";
        let design_key = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: design_path,
            agent_type: "code_buddy",
            profile_id: Some("a1c14cde-f9c0-4fce-9d7f-66c3f8e85039"),
        })
        .unwrap();
        let plan_path = "docs/superpowers/plans/p.md";
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
            publication_token: "pub-token-1".into(),
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
                ManifestPhase {
                    id: "design".into(),
                    kind: Some("design".into()),
                    title: None,
                },
                ManifestPhase {
                    id: "plan".into(),
                    kind: Some("plan".into()),
                    title: None,
                },
                ManifestPhase {
                    id: "tasks".into(),
                    kind: Some("tasks".into()),
                    title: None,
                },
                ManifestPhase {
                    id: "final".into(),
                    kind: Some("final".into()),
                    title: None,
                },
            ],
            nodes: vec![
                ManifestNode {
                    id: "design-reviewer-1".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some("design".into()),
                    role: Some(ManifestNodeRole::Reviewer),
                    agent_type: Some("code_buddy".into()),
                    profile_id: Some("a1c14cde-f9c0-4fce-9d7f-66c3f8e85039".into()),
                    task_index: None,
                    work_unit_key: Some(design_key),
                    deps: vec![],
                    required: Some(true),
                    node_outcome: None,
                    title: None,
                },
                ManifestNode {
                    id: "plan-reviewer-1".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some("plan".into()),
                    role: Some(ManifestNodeRole::Reviewer),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(plan_key),
                    deps: vec!["design-reviewer-1".into()],
                    required: None,
                    node_outcome: None,
                    title: None,
                },
                ManifestNode {
                    id: "task-1-impl".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some("tasks".into()),
                    role: Some(ManifestNodeRole::Implementer),
                    agent_type: Some("grok".into()),
                    profile_id: None,
                    task_index: Some(1),
                    work_unit_key: Some(task_impl),
                    deps: vec!["plan-reviewer-1".into()],
                    required: None,
                    node_outcome: None,
                    title: Some("Task 1".into()),
                },
                ManifestNode {
                    id: "task-1-rev".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some("tasks".into()),
                    role: Some(ManifestNodeRole::Reviewer),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: Some(1),
                    work_unit_key: Some(task_rev),
                    deps: vec!["task-1-impl".into()],
                    required: None,
                    node_outcome: None,
                    title: None,
                },
                ManifestNode {
                    id: "final-reviewer".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some("final".into()),
                    role: Some(ManifestNodeRole::Reviewer),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(final_rev),
                    deps: vec!["task-1-rev".into()],
                    required: None,
                    node_outcome: None,
                    title: None,
                },
                ManifestNode {
                    id: "final-fixer".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some("final".into()),
                    role: Some(ManifestNodeRole::Fixer),
                    agent_type: Some("grok".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(final_fix),
                    deps: vec!["final-reviewer".into()],
                    required: None,
                    node_outcome: None,
                    title: None,
                },
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
                    gate_kind: Some("design".into()),
                },
                ManifestGate {
                    id: "plan".into(),
                    required_reviewer_node_ids: vec!["plan-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some("plan".into()),
                },
            ],
        }
    }

    #[test]
    fn accepts_minimal_valid_manifest() {
        let doc = minimal_valid_doc();
        let normalized = validate_manifest_document(&doc).expect("valid");
        assert_eq!(normalized.task_count, 1);
        assert_eq!(normalized.nodes.len(), 6);
        assert_eq!(normalized.gates.len(), 2);
    }

    #[test]
    fn rejects_duplicate_node_ids() {
        let mut doc = minimal_valid_doc();
        doc.nodes[1].id = doc.nodes[0].id.clone();
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::DuplicateId(_)));
    }

    #[test]
    fn rejects_cycle_via_deps() {
        let mut doc = minimal_valid_doc();
        // Create A→B and B→A through deps
        doc.nodes[0].deps = vec![doc.nodes[1].id.clone()];
        doc.nodes[1].deps = vec![doc.nodes[0].id.clone()];
        let err = validate_manifest_document(&doc).unwrap_err();
        assert_eq!(err, WorkflowError::Cycle);
    }

    #[test]
    fn plan_gate_requires_non_empty_reviewers() {
        let mut doc = minimal_valid_doc();
        doc.gates[1].required_reviewer_node_ids.clear();
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidGateShape(_)));
    }

    #[test]
    fn design_zero_reviewer_requires_self_review() {
        let mut doc = minimal_valid_doc();
        // Drop design reviewers from gate
        doc.gates[0].required_reviewer_node_ids.clear();
        doc.gates[0].resolution_mode = ResolutionMode::ParentAdjudication;
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidGateShape(_)));

        doc.gates[0].resolution_mode = ResolutionMode::SelfReview;
        validate_manifest_document(&doc).expect("A12 self_review design gate ok");
    }

    #[test]
    fn rejects_key_mismatch_on_work_unit() {
        let mut doc = minimal_valid_doc();
        doc.nodes[0].work_unit_key = Some("design|wrong.md|reviewer|code_buddy|none".into());
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::KeyMismatch { .. }));
    }

    #[test]
    fn rejects_bounds_on_nodes() {
        let mut doc = minimal_valid_doc();
        let template = doc.nodes[2].clone();
        for i in 0..MAX_NODES {
            let mut n = template.clone();
            n.id = format!("extra-{i}");
            n.task_index = Some(((i % MAX_TASKS) + 1) as u32);
            n.work_unit_key = Some(
                build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                    task_index: n.task_index.unwrap(),
                    agent_type: "grok",
                    profile_id: Some(&n.id),
                })
                .unwrap(),
            );
            n.profile_id = Some(n.id.clone());
            doc.nodes.push(n);
        }
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::BoundsExceeded(_)));
    }

    #[test]
    fn rejects_unsupported_schema_and_kind() {
        let mut doc = minimal_valid_doc();
        doc.schema_version = 99;
        assert!(matches!(
            validate_manifest_document(&doc).unwrap_err(),
            WorkflowError::InvalidSchemaVersion(99)
        ));
        doc.schema_version = 1;
        doc.workflow_kind = "other".into();
        assert!(matches!(
            validate_manifest_document(&doc).unwrap_err(),
            WorkflowError::UnsupportedWorkflowKind(_)
        ));
    }

    #[test]
    fn allows_canceled_node_outcome() {
        let mut doc = minimal_valid_doc();
        doc.nodes[3].node_outcome = Some(ManifestNodeOutcome::Canceled);
        validate_manifest_document(&doc).expect("canceled outcome allowed");
    }
}
