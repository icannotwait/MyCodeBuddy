//! Manifest document validation (A1/A12/A15 + graph integrity).

use std::collections::{HashMap, HashSet, VecDeque};

use super::key::{build_work_unit_key, normalize_rel_path, validate_agent_type};
use super::types::{
    DocumentGateKind, DocumentRef, ManifestDocument, ManifestGate, ManifestNode, ManifestNodeKind,
    ManifestNodeRole, ManifestPhase, NormalizedGate, NormalizedManifest, NormalizedNode,
    ResolutionMode, WorkUnitKeyParts, WorkflowError, MANIFEST_SCHEMA_VERSION, MAX_EDGES, MAX_GATES,
    MAX_MANIFEST_JSON_BYTES, MAX_NODES, MAX_TASKS, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN,
    PHASE_TASKS, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
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
    if doc.publication_token.contains('|') || doc.publication_token.chars().any(|c| c.is_control())
    {
        return Err(WorkflowError::InvalidField(
            "publication_token contains illegal characters".into(),
        ));
    }

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

    let json_bytes = serde_json::to_vec(doc)
        .map_err(|e| WorkflowError::InvalidField(format!("manifest not serializable: {e}")))?;
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
            return Err(WorkflowError::DuplicateId(format!(
                "node:{}",
                normalized.id
            )));
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
    // brainstorm_to_delivery: contiguous 1..=N with exactly one implementer
    // and one reviewer work unit per Task index.
    if !task_indices.is_empty() {
        let max = *task_indices.iter().max().expect("non-empty");
        if task_indices.len() != max as usize
            || !(1..=max).all(|i| task_indices.contains(&i))
        {
            return Err(WorkflowError::InvalidTaskIndex(format!(
                "task indices must be contiguous 1..={max}, got {task_indices:?}"
            )));
        }
        for idx in 1..=max {
            let impl_count = nodes
                .iter()
                .filter(|n| {
                    n.task_index == Some(idx)
                        && matches!(n.role, Some(ManifestNodeRole::Implementer))
                })
                .count();
            let rev_count = nodes
                .iter()
                .filter(|n| {
                    n.task_index == Some(idx)
                        && matches!(n.role, Some(ManifestNodeRole::Reviewer))
                })
                .count();
            if impl_count != 1 || rev_count != 1 {
                return Err(WorkflowError::InvalidTaskIndex(format!(
                    "task_index {idx} requires exactly one implementer and one reviewer work unit (implementer={impl_count}, reviewer={rev_count})"
                )));
            }
        }
    }

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

    ensure_acyclic(&nodes, &edges)?;

    let mut gate_ids = HashSet::new();
    let mut gates = Vec::with_capacity(doc.gates.len());
    for gate in &doc.gates {
        let normalized = normalize_gate(gate, &node_id_set, &nodes, design.as_ref())?;
        if !gate_ids.insert(normalized.id.clone()) {
            return Err(WorkflowError::DuplicateId(format!(
                "gate:{}",
                normalized.id
            )));
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

fn normalize_document_ref(doc: Option<&DocumentRef>) -> Result<Option<DocumentRef>, WorkflowError> {
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
        if phase_ids.is_empty() || !phase_ids.contains(phase_id) {
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
            if node.work_unit_key.is_some()
                || node.role.is_some()
                || node.agent_type.is_some()
                || node.profile_id.is_some()
                || node.task_index.is_some()
            {
                return Err(WorkflowError::InvalidField(format!(
                    "non-work-unit node {} must not carry role/agent_type/profile_id/task_index/work_unit_key",
                    node.id
                )));
            }
            Ok(NormalizedNode {
                id: node.id.clone(),
                kind: node.kind,
                phase_id: node.phase_id.clone(),
                role: None,
                agent_type: None,
                profile_id: None,
                task_index: None,
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
    let role = node
        .role
        .ok_or_else(|| WorkflowError::MissingField(format!("role on work unit {}", node.id)))?;
    let agent_raw = node
        .agent_type
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            WorkflowError::MissingField(format!("agent_type on work unit {}", node.id))
        })?;
    let agent_type = validate_agent_type(agent_raw)?;

    let phase = node
        .phase_id
        .as_deref()
        .ok_or_else(|| WorkflowError::MissingField(format!("phase_id on work unit {}", node.id)))?;
    if !is_canonical_phase(phase) {
        return Err(WorkflowError::RoleMismatch(format!(
            "work unit {} phase_id must be design|plan|tasks|final, got {phase}",
            node.id
        )));
    }

    let profile_ref = node.profile_id.as_deref();
    let parts = classify_work_unit_parts(node, design, plan, role, phase, agent_type, profile_ref)?;
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
        phase_id: Some(phase.to_string()),
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

fn is_canonical_phase(phase: &str) -> bool {
    matches!(phase, PHASE_DESIGN | PHASE_PLAN | PHASE_TASKS | PHASE_FINAL)
}

fn classify_work_unit_parts<'a>(
    node: &'a ManifestNode,
    design: Option<&'a DocumentRef>,
    plan: Option<&'a DocumentRef>,
    role: ManifestNodeRole,
    phase: &'a str,
    agent_type: &'a str,
    profile_ref: Option<&'a str>,
) -> Result<WorkUnitKeyParts<'a>, WorkflowError> {
    match (role, phase, node.task_index) {
        (ManifestNodeRole::Implementer, PHASE_TASKS, Some(idx)) => {
            Ok(WorkUnitKeyParts::TaskImplementer {
                task_index: idx,
                agent_type,
                profile_id: profile_ref,
            })
        }
        (ManifestNodeRole::Reviewer, PHASE_TASKS, Some(idx)) => {
            Ok(WorkUnitKeyParts::TaskReviewer {
                task_index: idx,
                agent_type,
                profile_id: profile_ref,
            })
        }
        (ManifestNodeRole::Implementer, PHASE_TASKS, None) => Err(WorkflowError::MissingField(
            format!("task_index on implementer node {}", node.id),
        )),
        (ManifestNodeRole::Reviewer, PHASE_TASKS, None) => Err(WorkflowError::MissingField(
            format!("task_index on task reviewer node {}", node.id),
        )),
        (ManifestNodeRole::Implementer, other, _) => Err(WorkflowError::RoleMismatch(format!(
            "implementer node {} must have phase_id=tasks, got {other}",
            node.id
        ))),
        (ManifestNodeRole::Reviewer, PHASE_DESIGN, None) => {
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
        (ManifestNodeRole::Reviewer, PHASE_PLAN, None) => {
            let plan_doc = plan.ok_or_else(|| {
                WorkflowError::MissingField(format!("plan document required for node {}", node.id))
            })?;
            Ok(WorkUnitKeyParts::Plan {
                rel_plan_path: &plan_doc.rel_path,
                agent_type,
                profile_id: profile_ref,
            })
        }
        (ManifestNodeRole::Reviewer, PHASE_FINAL, None) => Ok(WorkUnitKeyParts::FinalReviewer {
            agent_type,
            profile_id: profile_ref,
        }),
        (ManifestNodeRole::Fixer, PHASE_FINAL, None) => Ok(WorkUnitKeyParts::FinalFixer {
            agent_type,
            profile_id: profile_ref,
        }),
        (ManifestNodeRole::Fixer, other, None) => Err(WorkflowError::RoleMismatch(format!(
            "fixer node {} must have phase_id=final, got {other}",
            node.id
        ))),
        (ManifestNodeRole::Fixer, _, Some(_)) => Err(WorkflowError::RoleMismatch(format!(
            "fixer node {} must not have task_index",
            node.id
        ))),
        (ManifestNodeRole::Reviewer, PHASE_DESIGN | PHASE_PLAN | PHASE_FINAL, Some(_)) => {
            Err(WorkflowError::RoleMismatch(format!(
                "document/final reviewer node {} must not have task_index",
                node.id
            )))
        }
        (ManifestNodeRole::Reviewer, other, _) => Err(WorkflowError::RoleMismatch(format!(
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

    // Resolve document-gate kind: optional wire field, fail-closed inference.
    let gate_kind = resolve_gate_kind(gate, node_ids, nodes)?;
    let expected_phase = gate_kind.expected_reviewer_phase();

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
        let node = nodes
            .iter()
            .find(|n| n.id == *reviewer_id)
            .expect("reviewer id present in node_ids");
        validate_document_gate_reviewer(gate, node, expected_phase)?;
    }

    match gate_kind {
        DocumentGateKind::Plan => {
            // Never allow empty Plan gate (including via missing kind inference).
            if gate.required_reviewer_node_ids.is_empty() {
                return Err(WorkflowError::InvalidGateShape(format!(
                    "plan gate {} cannot have empty required_reviewer_node_ids",
                    gate.id
                )));
            }
            if gate.resolution_mode != ResolutionMode::ParentAdjudication {
                return Err(WorkflowError::InvalidGateShape(format!(
                    "plan gate {} requires resolution_mode=parent_adjudication",
                    gate.id
                )));
            }
        }
        DocumentGateKind::Design => {
            if gate.required_reviewer_node_ids.is_empty() {
                // A12: zero-reviewer Design only with self_review + design doc.
                if gate.resolution_mode != ResolutionMode::SelfReview {
                    return Err(WorkflowError::InvalidGateShape(format!(
                        "zero-reviewer design gate {} requires resolution_mode=self_review",
                        gate.id
                    )));
                }
                if design.is_none() {
                    return Err(WorkflowError::InvalidGateShape(format!(
                        "zero-reviewer design gate {} requires design document path+digest",
                        gate.id
                    )));
                }
            } else if gate.resolution_mode == ResolutionMode::SelfReview {
                return Err(WorkflowError::InvalidGateShape(format!(
                    "design gate {} self_review requires empty required_reviewer_node_ids",
                    gate.id
                )));
            } else if gate.resolution_mode != ResolutionMode::ParentAdjudication {
                return Err(WorkflowError::InvalidGateShape(format!(
                    "design gate {} with reviewers requires resolution_mode=parent_adjudication",
                    gate.id
                )));
            }
        }
    }

    Ok(NormalizedGate {
        id: gate.id.clone(),
        required_reviewer_node_ids: gate.required_reviewer_node_ids.clone(),
        resolution_mode: gate.resolution_mode,
        gate_kind,
    })
}

/// Infer or validate `gate_kind` fail-closed from reviewers + optional wire field.
fn resolve_gate_kind(
    gate: &ManifestGate,
    node_ids: &HashSet<&str>,
    nodes: &[NormalizedNode],
) -> Result<DocumentGateKind, WorkflowError> {
    if gate.required_reviewer_node_ids.is_empty() {
        // Empty reviewers: only Design self_review is legal (never empty Plan).
        if gate.resolution_mode != ResolutionMode::SelfReview {
            return Err(WorkflowError::InvalidGateShape(format!(
                "empty-reviewer gate {} requires resolution_mode=self_review (Design only)",
                gate.id
            )));
        }
        match gate.gate_kind {
            None | Some(DocumentGateKind::Design) => Ok(DocumentGateKind::Design),
            Some(DocumentGateKind::Plan) => Err(WorkflowError::InvalidGateShape(format!(
                "plan gate {} cannot have empty required_reviewer_node_ids",
                gate.id
            ))),
        }
    } else {
        // Non-empty: all reviewers must share one document phase → that is the kind.
        let inferred = infer_kind_from_reviewers(gate, node_ids, nodes)?;
        match gate.gate_kind {
            None => Ok(inferred),
            Some(declared) if declared == inferred => Ok(declared),
            Some(declared) => Err(WorkflowError::InvalidGateShape(format!(
                "gate {} gate_kind={} conflicts with reviewer phase {}",
                gate.id,
                declared.as_str(),
                inferred.as_str()
            ))),
        }
    }
}

fn infer_kind_from_reviewers(
    gate: &ManifestGate,
    node_ids: &HashSet<&str>,
    nodes: &[NormalizedNode],
) -> Result<DocumentGateKind, WorkflowError> {
    let mut inferred_phase: Option<&str> = None;

    for reviewer_id in &gate.required_reviewer_node_ids {
        if !node_ids.contains(reviewer_id.as_str()) {
            return Err(WorkflowError::UnknownReference(format!(
                "gate reviewer {reviewer_id}"
            )));
        }
        let node = nodes
            .iter()
            .find(|n| n.id == *reviewer_id)
            .expect("reviewer id present in node_ids");

        // Shape checks during inference (same rules as validate_document_gate_reviewer).
        if node.kind != ManifestNodeKind::WorkUnit
            || node.role != Some(ManifestNodeRole::Reviewer)
            || node.task_index.is_some()
        {
            return Err(WorkflowError::RoleMismatch(format!(
                "gate {} reviewer {reviewer_id} must be a document-phase work_unit reviewer",
                gate.id
            )));
        }
        let phase = node.phase_id.as_deref().ok_or_else(|| {
            WorkflowError::RoleMismatch(format!(
                "gate {} reviewer {reviewer_id} missing phase_id",
                gate.id
            ))
        })?;
        match phase {
            PHASE_DESIGN | PHASE_PLAN => {}
            other => {
                return Err(WorkflowError::RoleMismatch(format!(
                    "gate {} reviewer {reviewer_id} phase_id must be design|plan, got {other}",
                    gate.id
                )));
            }
        }
        match inferred_phase {
            None => inferred_phase = Some(phase),
            Some(existing) if existing == phase => {}
            Some(existing) => {
                return Err(WorkflowError::InvalidGateShape(format!(
                    "gate {} reviewers mix document phases {existing} and {phase}",
                    gate.id
                )));
            }
        }
    }

    match inferred_phase {
        Some(PHASE_DESIGN) => Ok(DocumentGateKind::Design),
        Some(PHASE_PLAN) => Ok(DocumentGateKind::Plan),
        Some(other) => Err(WorkflowError::InvalidGateShape(format!(
            "gate {} cannot infer kind from phase {other}",
            gate.id
        ))),
        None => Err(WorkflowError::InvalidGateShape(format!(
            "gate {} has no reviewers to infer gate_kind",
            gate.id
        ))),
    }
}

fn validate_document_gate_reviewer(
    gate: &ManifestGate,
    node: &NormalizedNode,
    expected_phase: &str,
) -> Result<(), WorkflowError> {
    if node.kind != ManifestNodeKind::WorkUnit {
        return Err(WorkflowError::RoleMismatch(format!(
            "gate {} reviewer {} must be a work_unit node",
            gate.id, node.id
        )));
    }
    if node.role != Some(ManifestNodeRole::Reviewer) {
        return Err(WorkflowError::RoleMismatch(format!(
            "gate {} reviewer {} is not a reviewer role",
            gate.id, node.id
        )));
    }
    if node.phase_id.as_deref() != Some(expected_phase) {
        return Err(WorkflowError::RoleMismatch(format!(
            "gate {} reviewer {} must have phase_id={expected_phase}",
            gate.id, node.id
        )));
    }
    if node.task_index.is_some() {
        return Err(WorkflowError::RoleMismatch(format!(
            "gate {} reviewer {} must not be a task-indexed reviewer",
            gate.id, node.id
        )));
    }
    Ok(())
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
        ManifestEdge, ManifestNodeOutcome, ManifestWorkflowState,
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
                    id: PHASE_DESIGN.into(),
                    kind: Some(PHASE_DESIGN.into()),
                    title: None,
                },
                ManifestPhase {
                    id: PHASE_PLAN.into(),
                    kind: Some(PHASE_PLAN.into()),
                    title: None,
                },
                ManifestPhase {
                    id: PHASE_TASKS.into(),
                    kind: Some(PHASE_TASKS.into()),
                    title: None,
                },
                ManifestPhase {
                    id: PHASE_FINAL.into(),
                    kind: Some(PHASE_FINAL.into()),
                    title: None,
                },
            ],
            nodes: vec![
                ManifestNode {
                    id: "design-reviewer-1".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some(PHASE_DESIGN.into()),
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
                    phase_id: Some(PHASE_PLAN.into()),
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
                    phase_id: Some(PHASE_TASKS.into()),
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
                    phase_id: Some(PHASE_TASKS.into()),
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
                    phase_id: Some(PHASE_FINAL.into()),
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
                    phase_id: Some(PHASE_FINAL.into()),
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

    #[test]
    fn accepts_minimal_valid_manifest() {
        let doc = minimal_valid_doc();
        let normalized = validate_manifest_document(&doc).expect("valid");
        assert_eq!(normalized.task_count, 1);
        assert_eq!(normalized.nodes.len(), 6);
        assert_eq!(normalized.gates.len(), 2);
    }

    #[test]
    fn rejects_non_contiguous_task_indices() {
        let mut doc = minimal_valid_doc();
        // Add task 3 without task 2.
        let impl_key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 3,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let rev_key = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 3,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        doc.nodes.push(ManifestNode {
            id: "task-3-impl".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_TASKS.into()),
            role: Some(ManifestNodeRole::Implementer),
            agent_type: Some("grok".into()),
            profile_id: None,
            task_index: Some(3),
            work_unit_key: Some(impl_key),
            deps: vec![],
            required: None,
            node_outcome: None,
            title: None,
        });
        doc.nodes.push(ManifestNode {
            id: "task-3-rev".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_TASKS.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: Some(3),
            work_unit_key: Some(rev_key),
            deps: vec![],
            required: None,
            node_outcome: None,
            title: None,
        });
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(
            matches!(err, WorkflowError::InvalidTaskIndex(ref m) if m.contains("contiguous")),
            "got {err:?}"
        );
    }

    #[test]
    fn rejects_task_index_missing_reviewer_pair() {
        let mut doc = minimal_valid_doc();
        // Drop task-1 reviewer.
        doc.nodes.retain(|n| n.id != "task-1-rev");
        // Fix deps that pointed at it.
        for n in &mut doc.nodes {
            n.deps.retain(|d| d != "task-1-rev");
        }
        doc.edges.retain(|e| e.from != "task-1-rev" && e.to != "task-1-rev");
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(
            matches!(
                err,
                WorkflowError::InvalidTaskIndex(ref m)
                    if m.contains("exactly one implementer and one reviewer")
            ),
            "got {err:?}"
        );
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
        doc.gates[0].required_reviewer_node_ids.clear();
        doc.gates[0].resolution_mode = ResolutionMode::ParentAdjudication;
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidGateShape(_)));

        doc.gates[0].resolution_mode = ResolutionMode::SelfReview;
        validate_manifest_document(&doc).expect("A12 self_review design gate ok");
    }

    #[test]
    fn empty_reviewers_infer_design_only_never_plan() {
        // Missing gate_kind + empty + self_review → Design.
        let mut doc = minimal_valid_doc();
        doc.gates[0].required_reviewer_node_ids.clear();
        doc.gates[0].resolution_mode = ResolutionMode::SelfReview;
        doc.gates[0].gate_kind = None;
        let n = validate_manifest_document(&doc).expect("infer design");
        assert_eq!(n.gates[0].gate_kind, DocumentGateKind::Design);

        // Empty without self_review → reject (no fail-open Plan).
        doc.gates[0].resolution_mode = ResolutionMode::ParentAdjudication;
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidGateShape(_)));

        // Explicit Plan + empty → reject even with self_review.
        doc.gates[1].required_reviewer_node_ids.clear();
        doc.gates[1].resolution_mode = ResolutionMode::SelfReview;
        doc.gates[1].gate_kind = Some(DocumentGateKind::Plan);
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidGateShape(_)));
    }

    #[test]
    fn infers_gate_kind_from_homogeneous_document_reviewers() {
        let mut doc = minimal_valid_doc();
        doc.gates[0].gate_kind = None;
        doc.gates[1].gate_kind = None;
        let n = validate_manifest_document(&doc).expect("infer from phases");
        assert_eq!(n.gates[0].gate_kind, DocumentGateKind::Design);
        assert_eq!(n.gates[1].gate_kind, DocumentGateKind::Plan);

        // Mixed design+plan reviewers on one gate → reject.
        doc.gates[1].required_reviewer_node_ids =
            vec!["design-reviewer-1".into(), "plan-reviewer-1".into()];
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidGateShape(_)));
    }

    #[test]
    fn plan_gate_rejects_milestone_or_wrong_phase_reviewer() {
        let mut doc = minimal_valid_doc();
        doc.nodes.push(ManifestNode {
            id: "milestone-1".into(),
            kind: ManifestNodeKind::Milestone,
            phase_id: Some(PHASE_PLAN.into()),
            role: None,
            agent_type: None,
            profile_id: None,
            task_index: None,
            work_unit_key: None,
            deps: vec![],
            required: None,
            node_outcome: None,
            title: None,
        });
        doc.gates[1].required_reviewer_node_ids = vec!["milestone-1".into()];
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::RoleMismatch(_)));

        // Design-phase reviewer cannot satisfy a Plan gate.
        doc.gates[1].required_reviewer_node_ids = vec!["design-reviewer-1".into()];
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(
            matches!(err, WorkflowError::RoleMismatch(_))
                || matches!(err, WorkflowError::InvalidGateShape(_))
        );
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

    #[test]
    fn non_work_unit_must_not_carry_role_fields() {
        let mut doc = minimal_valid_doc();
        doc.nodes.push(ManifestNode {
            id: "ms".into(),
            kind: ManifestNodeKind::Milestone,
            phase_id: Some(PHASE_TASKS.into()),
            role: Some(ManifestNodeRole::Reviewer),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: None,
            deps: vec![],
            required: None,
            node_outcome: None,
            title: None,
        });
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::InvalidField(_)));
    }

    #[test]
    fn task_nodes_require_tasks_phase_and_fixer_requires_final() {
        let mut doc = minimal_valid_doc();
        doc.nodes[2].phase_id = Some(PHASE_PLAN.into());
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::RoleMismatch(_)));

        let mut doc = minimal_valid_doc();
        doc.nodes[5].phase_id = Some(PHASE_TASKS.into());
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::RoleMismatch(_)));

        let mut doc = minimal_valid_doc();
        doc.nodes[5].phase_id = None;
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::MissingField(_)));
    }

    #[test]
    fn missing_required_collection_fields_fail_serde() {
        let incomplete = r#"{
            "schema_version": 1,
            "workflow_kind": "brainstorm_to_delivery",
            "publication_token": "t",
            "workflow_state": "estimated"
        }"#;
        let err = serde_json::from_str::<ManifestDocument>(incomplete).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("phases") || msg.contains("nodes") || msg.contains("missing field"),
            "expected missing field error, got {msg}"
        );

        // gate_kind is optional on the wire (frozen fields: id, reviewers, mode).
        let optional_gate_kind = r#"{
            "schema_version": 1,
            "workflow_kind": "brainstorm_to_delivery",
            "publication_token": "t",
            "workflow_state": "estimated",
            "design": { "rel_path": "docs/a.md", "digest": "d" },
            "phases": [{ "id": "design" }],
            "nodes": [],
            "edges": [],
            "gates": [{
                "id": "mystery",
                "required_reviewer_node_ids": [],
                "resolution_mode": "self_review"
            }]
        }"#;
        let doc: ManifestDocument =
            serde_json::from_str(optional_gate_kind).expect("gate_kind optional");
        assert!(doc.gates[0].gate_kind.is_none());
        // Validation still requires design path normalize etc.
        let validated = validate_manifest_document(&doc).expect("empty self_review → Design");
        assert_eq!(validated.gates[0].gate_kind, DocumentGateKind::Design);

        let missing_deps = r#"{
            "schema_version": 1,
            "workflow_kind": "brainstorm_to_delivery",
            "publication_token": "t",
            "workflow_state": "estimated",
            "phases": [],
            "nodes": [{
                "id": "n1",
                "kind": "milestone"
            }],
            "edges": [],
            "gates": []
        }"#;
        assert!(serde_json::from_str::<ManifestDocument>(missing_deps).is_err());
    }

    #[test]
    fn rejects_invalid_agent_type_on_work_unit() {
        let mut doc = minimal_valid_doc();
        doc.nodes[4].agent_type = Some("not_real".into());
        // key still points at codex — fails agent validation before or at key build
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(
            err,
            WorkflowError::InvalidAgentType(_) | WorkflowError::KeyMismatch { .. }
        ));
    }

    #[test]
    fn rejects_a15_bounds_edges_gates_tasks() {
        // Edges.
        let mut doc = minimal_valid_doc();
        let from = doc.nodes[0].id.clone();
        let to = doc.nodes[1].id.clone();
        for i in 0..=MAX_EDGES {
            doc.edges.push(ManifestEdge {
                id: Some(format!("edge-{i}")),
                from: from.clone(),
                to: to.clone(),
            });
        }
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(
            matches!(err, WorkflowError::BoundsExceeded(ref m) if m.contains("edges")),
            "expected edges bound, got {err:?}"
        );

        // Gates.
        let mut doc = minimal_valid_doc();
        let plan_reviewer = doc.gates[1].required_reviewer_node_ids[0].clone();
        for i in 0..=MAX_GATES {
            doc.gates.push(ManifestGate {
                id: format!("extra-gate-{i}"),
                required_reviewer_node_ids: vec![plan_reviewer.clone()],
                resolution_mode: ResolutionMode::ParentAdjudication,
                gate_kind: Some(DocumentGateKind::Plan),
            });
        }
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(
            matches!(err, WorkflowError::BoundsExceeded(ref m) if m.contains("gates")),
            "expected gates bound, got {err:?}"
        );

        // Tasks (distinct task_index).
        let mut doc = minimal_valid_doc();
        // Keep existing task 1; add task_index 2..=MAX_TASKS+1 as implementers.
        for idx in 2..=(MAX_TASKS as u32 + 1) {
            let key = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
                task_index: idx,
                agent_type: "grok",
                profile_id: None,
            })
            .unwrap();
            doc.nodes.push(ManifestNode {
                id: format!("task-{idx}-impl"),
                kind: ManifestNodeKind::WorkUnit,
                phase_id: Some(PHASE_TASKS.into()),
                role: Some(ManifestNodeRole::Implementer),
                agent_type: Some("grok".into()),
                profile_id: None,
                task_index: Some(idx),
                work_unit_key: Some(key),
                deps: vec![],
                required: None,
                node_outcome: None,
                title: None,
            });
        }
        // MAX_TASKS+1 distinct indices (1 existing + 2..=MAX_TASKS+1).
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(
            matches!(
                err,
                WorkflowError::BoundsExceeded(ref m) if m.contains("tasks")
            ) || matches!(err, WorkflowError::InvalidTaskIndex(_)),
            "expected tasks bound, got {err:?}"
        );
    }

    #[test]
    fn rejects_a15_manifest_json_size_bound() {
        let mut doc = minimal_valid_doc();
        // Inflate a free-text field until serialized JSON exceeds 512 KiB.
        doc.publication_token = "x".repeat(MAX_MANIFEST_JSON_BYTES + 1024);
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(
            matches!(err, WorkflowError::BoundsExceeded(ref m) if m.contains("manifest JSON")),
            "expected JSON size bound, got {err:?}"
        );
    }
}
