//! Manifest document validation (A1/A12/A15 + graph integrity).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use super::key::{build_work_unit_key, normalize_rel_path, validate_agent_type};
use super::types::{
    DocumentGateKind, DocumentRef, ManifestDocument, ManifestGate, ManifestNode, ManifestNodeKind,
    ManifestNodeRole, ManifestPhase, ManifestTaskHardTrigger, ManifestTaskPolicy, ManifestTaskRisk,
    ManifestTaskSoftSignal, NormalizedGate, NormalizedManifest, NormalizedNode, ResolutionMode,
    TaskRiskLevel, TaskSoftSignalKind, WorkUnitKeyParts, WorkflowError, MANIFEST_SCHEMA_VERSION,
    MAX_ADJUDICATION_SUMMARY_BYTES, MAX_EDGES, MAX_GATES, MAX_MANIFEST_JSON_BYTES, MAX_NODES,
    MAX_TASKS, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS, TASK_RISK_POLICY_VERSION,
    WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
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
    let plan_target_rel_path = normalize_rel_path(&doc.plan_target_rel_path)?;
    if doc.risk_policy_version != TASK_RISK_POLICY_VERSION {
        return Err(WorkflowError::RiskAssessmentInvalid(Box::new(
            WorkflowError::InvalidField(format!(
                "risk_policy_version must be {TASK_RISK_POLICY_VERSION}, got {}",
                doc.risk_policy_version
            )),
        )));
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
    if let Some(plan) = &plan {
        if plan.rel_path != plan_target_rel_path {
            return Err(WorkflowError::InvalidField(format!(
                "plan rel_path {} must equal plan_target_rel_path {plan_target_rel_path}",
                plan.rel_path
            )));
        }
    }

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
        let normalized = normalize_node(
            node,
            design.as_ref(),
            plan.as_ref(),
            &plan_target_rel_path,
            &phase_ids,
        )?;
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
    // brainstorm_to_delivery Task indices remain contiguous; route-dependent
    // implementer/reviewer cardinality is validated from task_policies below.
    if !task_indices.is_empty() {
        let max = *task_indices.iter().max().expect("non-empty");
        if task_indices.len() != max as usize || !(1..=max).all(|i| task_indices.contains(&i)) {
            return Err(WorkflowError::InvalidTaskIndex(format!(
                "task indices must be contiguous 1..={max}, got {task_indices:?}"
            )));
        }
    }

    validate_author_and_skeleton(doc, &nodes, &task_indices)?;
    let task_policies = normalize_task_policies(doc, &nodes, &task_indices)?;

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
        plan_target_rel_path,
        risk_policy_version: doc.risk_policy_version.clone(),
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
        task_policies,
        task_count: task_indices.len(),
    })
}

/// Return the unique active Task-policy indices used to bind Plan material.
pub fn active_plan_material_task_indices(
    manifest: &NormalizedManifest,
) -> Result<BTreeSet<u32>, WorkflowError> {
    let mut indices = BTreeSet::new();
    for policy in &manifest.task_policies {
        if policy.task_index == 0 {
            return Err(WorkflowError::InvalidTaskIndex(
                "Plan material Task indices are 1-based".into(),
            ));
        }
        if !indices.insert(policy.task_index) {
            return Err(WorkflowError::DuplicateId(format!(
                "task_policy:{}",
                policy.task_index
            )));
        }
    }
    Ok(indices)
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
    plan_target_rel_path: &str,
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
        ManifestNodeKind::WorkUnit => {
            normalize_work_unit(node, design, plan, plan_target_rel_path, required)
        }
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
    plan_target_rel_path: &str,
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

    let parts = classify_work_unit_parts(
        node,
        design,
        plan,
        plan_target_rel_path,
        role,
        phase,
        agent_type,
    )?;
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
    plan_target_rel_path: &'a str,
    role: ManifestNodeRole,
    phase: &'a str,
    agent_type: &'a str,
) -> Result<WorkUnitKeyParts<'a>, WorkflowError> {
    let profile_ref = node.profile_id.as_deref();
    match (role, phase, node.task_index) {
        (ManifestNodeRole::Author, PHASE_PLAN, None) => Ok(WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: plan_target_rel_path,
            agent_type,
            profile_id: profile_ref,
        }),
        (ManifestNodeRole::Author, _, Some(_)) => Err(WorkflowError::RoleMismatch(format!(
            "Plan Author node {} must not have task_index",
            node.id
        ))),
        (ManifestNodeRole::Author, other, None) => Err(WorkflowError::RoleMismatch(format!(
            "Plan Author node {} must have phase_id=plan, got {other}",
            node.id
        ))),
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
            Ok(WorkUnitKeyParts::PlanReviewer {
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

fn validate_author_and_skeleton(
    doc: &ManifestDocument,
    nodes: &[NormalizedNode],
    task_indices: &HashSet<u32>,
) -> Result<(), WorkflowError> {
    let author_count = nodes
        .iter()
        .filter(|node| node.role == Some(ManifestNodeRole::Author))
        .count();
    if author_count != 1 {
        return Err(WorkflowError::InvalidField(format!(
            "manifest requires exactly one Plan Author node, got {author_count}"
        )));
    }
    let author = nodes
        .iter()
        .find(|node| node.role == Some(ManifestNodeRole::Author))
        .expect("author count was validated");
    if author.agent_type.as_deref() != Some("codex") {
        return Err(WorkflowError::InvalidField(
            "Plan Author agent_type must be codex".into(),
        ));
    }

    if doc.workflow_state == super::types::ManifestWorkflowState::Skeleton {
        if doc.plan.is_some() {
            return Err(WorkflowError::InvalidField(
                "skeleton manifest must not contain a Plan document".into(),
            ));
        }
        if !task_indices.is_empty() {
            return Err(WorkflowError::InvalidField(
                "skeleton manifest must not contain Task nodes".into(),
            ));
        }
        if !doc.task_policies.is_empty() {
            return Err(WorkflowError::InvalidField(
                "skeleton manifest must not contain Task policies".into(),
            ));
        }
    }

    Ok(())
}

fn normalize_task_policies(
    doc: &ManifestDocument,
    nodes: &[NormalizedNode],
    task_indices: &HashSet<u32>,
) -> Result<Vec<ManifestTaskPolicy>, WorkflowError> {
    let mut policy_indices = HashSet::new();
    let mut policies = Vec::with_capacity(doc.task_policies.len());

    for policy in &doc.task_policies {
        if policy.task_index == 0 || policy.task_index as usize > MAX_TASKS {
            return Err(WorkflowError::InvalidTaskIndex(format!(
                "Task policy index {} out of range 1..={MAX_TASKS}",
                policy.task_index
            )));
        }
        if !policy_indices.insert(policy.task_index) {
            return Err(WorkflowError::DuplicateId(format!(
                "Task policy index {}",
                policy.task_index
            )));
        }
        if !task_indices.contains(&policy.task_index) {
            return Err(WorkflowError::InvalidField(format!(
                "Task policy index {} has no matching Task nodes",
                policy.task_index
            )));
        }

        let risk = normalize_task_risk(policy.task_index, &policy.risk)
            .map_err(|error| WorkflowError::RiskAssessmentInvalid(Box::new(error)))?;
        validate_task_route(policy.task_index, &risk, &policy.route, nodes)
            .map_err(|error| WorkflowError::TaskRouteMismatch(Box::new(error)))?;
        policies.push(ManifestTaskPolicy {
            task_index: policy.task_index,
            risk,
            route: policy.route.clone(),
            allow_noop_verification: policy.allow_noop_verification,
        });
    }

    if policy_indices != *task_indices {
        return Err(WorkflowError::InvalidField(format!(
            "Task policy indices {policy_indices:?} must exactly match Task node indices {task_indices:?}"
        )));
    }

    Ok(policies)
}

fn normalize_task_risk(
    task_index: u32,
    risk: &ManifestTaskRisk,
) -> Result<ManifestTaskRisk, WorkflowError> {
    let mut hard_kinds = HashSet::new();
    let mut hard_triggers = Vec::with_capacity(risk.hard_triggers.len());
    for trigger in &risk.hard_triggers {
        if !hard_kinds.insert(trigger.kind) {
            return Err(WorkflowError::DuplicateId(format!(
                "Task {task_index} hard trigger {:?}",
                trigger.kind
            )));
        }
        hard_triggers.push(ManifestTaskHardTrigger {
            kind: trigger.kind,
            evidence: normalize_risk_evidence(task_index, "hard trigger", &trigger.evidence)?,
        });
    }

    let mut soft_kinds = HashSet::new();
    let mut soft_signals = Vec::with_capacity(risk.soft_signals.len());
    let mut score = 0u32;
    for signal in &risk.soft_signals {
        if !soft_kinds.insert(signal.kind) {
            return Err(WorkflowError::DuplicateId(format!(
                "Task {task_index} soft signal {:?}",
                signal.kind
            )));
        }
        let expected = task_soft_signal_weight(signal.kind);
        if signal.score != expected {
            return Err(WorkflowError::InvalidField(format!(
                "Task {task_index} soft signal {:?} score must be {expected}, got {}",
                signal.kind, signal.score
            )));
        }
        score = score.checked_add(expected).ok_or_else(|| {
            WorkflowError::InvalidField(format!("Task {task_index} soft score overflow"))
        })?;
        soft_signals.push(ManifestTaskSoftSignal {
            kind: signal.kind,
            score: expected,
            evidence: normalize_risk_evidence(task_index, "soft signal", &signal.evidence)?,
        });
    }
    if risk.score != score {
        return Err(WorkflowError::InvalidField(format!(
            "Task {task_index} submitted soft score {} must equal derived score {score}",
            risk.score
        )));
    }

    let expected_level = if !hard_triggers.is_empty() || score >= 3 {
        TaskRiskLevel::High
    } else {
        TaskRiskLevel::Normal
    };
    if risk.level != expected_level {
        return Err(WorkflowError::InvalidField(format!(
            "Task {task_index} risk level {:?} contradicts derived level {expected_level:?}",
            risk.level
        )));
    }

    let reason = risk.reason.trim();
    if reason.is_empty() || reason.len() > MAX_ADJUDICATION_SUMMARY_BYTES {
        return Err(WorkflowError::InvalidField(format!(
            "Task {task_index} risk reason must be non-empty and at most {MAX_ADJUDICATION_SUMMARY_BYTES} bytes"
        )));
    }

    Ok(ManifestTaskRisk {
        level: expected_level,
        hard_triggers,
        soft_signals,
        score,
        reason: reason.to_string(),
    })
}

const fn task_soft_signal_weight(kind: TaskSoftSignalKind) -> u32 {
    match kind {
        TaskSoftSignalKind::CrossRuntimeOrProcess => 2,
        TaskSoftSignalKind::BroadProductionSurface
        | TaskSoftSignalKind::MultipleOwnershipModules
        | TaskSoftSignalKind::SharedInterface
        | TaskSoftSignalKind::DependencyOrBuild
        | TaskSoftSignalKind::MultiLayerWithoutTestSeam => 1,
    }
}

fn normalize_risk_evidence(
    task_index: u32,
    signal_type: &str,
    evidence: &[String],
) -> Result<Vec<String>, WorkflowError> {
    if evidence.is_empty() {
        return Err(WorkflowError::InvalidField(format!(
            "Task {task_index} {signal_type} evidence must not be empty"
        )));
    }
    evidence
        .iter()
        .map(|item| {
            let item = item.trim();
            if item.is_empty() || item.len() > MAX_ADJUDICATION_SUMMARY_BYTES {
                return Err(WorkflowError::InvalidField(format!(
                    "Task {task_index} {signal_type} evidence must be non-empty and at most {MAX_ADJUDICATION_SUMMARY_BYTES} bytes"
                )));
            }
            Ok(item.to_string())
        })
        .collect()
}

fn validate_task_route(
    task_index: u32,
    risk: &ManifestTaskRisk,
    route: &super::types::ManifestTaskRoute,
    nodes: &[NormalizedNode],
) -> Result<(), WorkflowError> {
    let route_error = |message: String| {
        WorkflowError::InvalidField(format!("Task {task_index} route mismatch: {message}"))
    };
    let implementer = nodes
        .iter()
        .find(|node| node.id == route.implementer_node_id)
        .ok_or_else(|| route_error(format!("unknown implementer {}", route.implementer_node_id)))?;
    if implementer.role != Some(ManifestNodeRole::Implementer)
        || implementer.task_index != Some(task_index)
    {
        return Err(route_error(format!(
            "implementer {} must be an implementer for this Task index",
            implementer.id
        )));
    }

    let mut route_ids = HashSet::new();
    route_ids.insert(implementer.id.as_str());
    let mut reviewer_agents = Vec::with_capacity(route.reviewer_node_ids.len());
    for reviewer_id in &route.reviewer_node_ids {
        if !route_ids.insert(reviewer_id.as_str()) {
            return Err(WorkflowError::DuplicateId(format!(
                "Task {task_index} route node {reviewer_id}"
            )));
        }
        let reviewer = nodes
            .iter()
            .find(|node| node.id == *reviewer_id)
            .ok_or_else(|| route_error(format!("unknown reviewer {reviewer_id}")))?;
        if reviewer.role != Some(ManifestNodeRole::Reviewer)
            || reviewer.task_index != Some(task_index)
        {
            return Err(route_error(format!(
                "reviewer {reviewer_id} must be a reviewer for this Task index"
            )));
        }
        reviewer_agents.push(
            reviewer
                .agent_type
                .as_deref()
                .expect("validated work unit has agent_type"),
        );
    }

    let task_node_ids: HashSet<&str> = nodes
        .iter()
        .filter(|node| node.task_index == Some(task_index))
        .map(|node| node.id.as_str())
        .collect();
    if route_ids != task_node_ids {
        return Err(route_error(
            "route must contain every Task implementer and reviewer exactly once".into(),
        ));
    }

    let implementer_agent = implementer
        .agent_type
        .as_deref()
        .expect("validated work unit has agent_type");
    match risk.level {
        TaskRiskLevel::Normal => {
            if implementer_agent != "grok" || reviewer_agents.as_slice() != ["codex"] {
                return Err(route_error(
                    "normal risk requires one Grok implementer and one Codex reviewer".into(),
                ));
            }
        }
        TaskRiskLevel::High => {
            reviewer_agents.sort_unstable();
            if implementer_agent != "codex" || reviewer_agents.as_slice() != ["codex", "grok"] {
                return Err(route_error(
                    "high risk requires one Codex implementer and distinct Codex/Grok reviewers"
                        .into(),
                ));
            }
        }
    }
    Ok(())
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

    let mut seen_cohort = HashSet::new();
    for reviewer_id in &gate.reviewer_cohort_node_ids {
        if !node_ids.contains(reviewer_id.as_str()) {
            return Err(WorkflowError::UnknownReference(format!(
                "gate cohort reviewer {reviewer_id}"
            )));
        }
        if !seen_cohort.insert(reviewer_id.clone()) {
            return Err(WorkflowError::DuplicateId(format!(
                "gate {} cohort reviewer {reviewer_id}",
                gate.id
            )));
        }
        let node = nodes
            .iter()
            .find(|n| n.id == *reviewer_id)
            .expect("cohort reviewer id present in node_ids");
        validate_document_gate_reviewer(gate, node, expected_phase)?;
    }

    let complete_cohort: HashSet<String> = nodes
        .iter()
        .filter(|node| {
            node.kind == ManifestNodeKind::WorkUnit
                && node.role == Some(ManifestNodeRole::Reviewer)
                && node.phase_id.as_deref() == Some(expected_phase)
                && node.task_index.is_none()
        })
        .map(|node| node.id.clone())
        .collect();
    let design_self_review =
        gate_kind == DocumentGateKind::Design && gate.resolution_mode == ResolutionMode::SelfReview;
    if !design_self_review && seen_cohort != complete_cohort {
        return Err(WorkflowError::InvalidGateShape(format!(
            "gate {} reviewer cohort must exactly match all configured {expected_phase} reviewers",
            gate.id
        )));
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
        if !seen_cohort.contains(reviewer_id) {
            return Err(WorkflowError::InvalidGateShape(format!(
                "gate {} required reviewer {reviewer_id} is outside reviewer cohort",
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
            if gate.reviewer_cohort_node_ids.is_empty() {
                return Err(WorkflowError::InvalidGateShape(format!(
                    "plan gate {} cannot have empty reviewer cohort",
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
                if !gate.reviewer_cohort_node_ids.is_empty() {
                    return Err(WorkflowError::InvalidGateShape(format!(
                        "zero-reviewer design gate {} requires empty reviewer cohort",
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
        reviewer_cohort_node_ids: gate.reviewer_cohort_node_ids.clone(),
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
        if gate.gate_kind == Some(DocumentGateKind::Plan) {
            return Err(WorkflowError::InvalidGateShape(format!(
                "plan gate {} cannot have empty required_reviewer_node_ids",
                gate.id
            )));
        }
        if gate.resolution_mode != ResolutionMode::SelfReview {
            return Err(WorkflowError::InvalidGateShape(format!(
                "empty-reviewer gate {} requires resolution_mode=self_review (Design only)",
                gate.id
            )));
        }
        match gate.gate_kind {
            None | Some(DocumentGateKind::Design) => Ok(DocumentGateKind::Design),
            Some(DocumentGateKind::Plan) => unreachable!("handled above"),
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
    use crate::acp::delegation::workflow::types::ManifestTaskRoute;
    use serde_json::{json, Value};

    fn validate_wire(value: Value) -> Result<NormalizedManifest, String> {
        let doc: ManifestDocument = serde_json::from_value(value).map_err(|e| e.to_string())?;
        validate_manifest_document(&doc).map_err(|e| e.to_string())
    }

    fn v2_skeleton_wire() -> Value {
        json!({
            "schema_version": MANIFEST_SCHEMA_VERSION,
            "workflow_kind": WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
            "publication_token": "token-v2",
            "workflow_state": "skeleton",
            "plan_target_rel_path": "docs/superpowers/plans/p.md",
            "risk_policy_version": "b2d_task_risk_v1",
            "phases": [{ "id": "plan" }],
            "nodes": [{
                "id": "plan-author",
                "kind": "work_unit",
                "phase_id": "plan",
                "role": "author",
                "agent_type": "codex",
                "work_unit_key": "plan|docs/superpowers/plans/p.md|author|codex|none",
                "deps": []
            }],
            "edges": [],
            "gates": [],
            "task_policies": []
        })
    }

    fn soft_signal(kind: &str, score: u32) -> Value {
        json!({
            "kind": kind,
            "score": score,
            "evidence": [format!("evidence for {kind}")]
        })
    }

    fn estimated_wire(
        level: &str,
        hard_triggers: Vec<Value>,
        soft_signals: Vec<Value>,
        score: u32,
        implementer_agent: &str,
        task_reviewers: &[(&str, &str)],
    ) -> Value {
        let mut nodes = vec![
            json!({
                "id": "plan-author",
                "kind": "work_unit",
                "phase_id": "plan",
                "role": "author",
                "agent_type": "codex",
                "work_unit_key": "plan|docs/superpowers/plans/p.md|author|codex|none",
                "deps": []
            }),
            json!({
                "id": "plan-reviewer-codex",
                "kind": "work_unit",
                "phase_id": "plan",
                "role": "reviewer",
                "agent_type": "codex",
                "work_unit_key": "plan|docs/superpowers/plans/p.md|reviewer|codex|none",
                "deps": ["plan-author"]
            }),
            json!({
                "id": "plan-reviewer-grok",
                "kind": "work_unit",
                "phase_id": "plan",
                "role": "reviewer",
                "agent_type": "grok",
                "work_unit_key": "plan|docs/superpowers/plans/p.md|reviewer|grok|none",
                "deps": ["plan-author"]
            }),
            json!({
                "id": "task-1-implementer",
                "kind": "work_unit",
                "phase_id": "tasks",
                "role": "implementer",
                "agent_type": implementer_agent,
                "task_index": 1,
                "work_unit_key": format!("task|1|implementer|{implementer_agent}|none"),
                "deps": []
            }),
        ];
        let mut reviewer_ids = Vec::new();
        for (id, agent) in task_reviewers {
            reviewer_ids.push((*id).to_string());
            nodes.push(json!({
                "id": id,
                "kind": "work_unit",
                "phase_id": "tasks",
                "role": "reviewer",
                "agent_type": agent,
                "task_index": 1,
                "work_unit_key": format!("task|1|reviewer|{agent}|none"),
                "deps": ["task-1-implementer"]
            }));
        }

        json!({
            "schema_version": MANIFEST_SCHEMA_VERSION,
            "workflow_kind": WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
            "publication_token": "token-v2",
            "workflow_state": "estimated",
            "plan_target_rel_path": "docs/superpowers/plans/p.md",
            "risk_policy_version": "b2d_task_risk_v1",
            "plan": {
                "rel_path": "docs/superpowers/plans/p.md",
                "digest": "plan-digest"
            },
            "phases": [{ "id": "plan" }, { "id": "tasks" }],
            "nodes": nodes,
            "edges": [],
            "gates": [{
                "id": "plan-gate",
                "gate_kind": "plan",
                "reviewer_cohort_node_ids": [
                    "plan-reviewer-codex",
                    "plan-reviewer-grok"
                ],
                "required_reviewer_node_ids": ["plan-reviewer-codex"],
                "resolution_mode": "parent_adjudication"
            }],
            "task_policies": [{
                "task_index": 1,
                "risk": {
                    "level": level,
                    "hard_triggers": hard_triggers,
                    "soft_signals": soft_signals,
                    "score": score,
                    "reason": format!("{level} risk fixture")
                },
                "route": {
                    "implementer_node_id": "task-1-implementer",
                    "reviewer_node_ids": reviewer_ids
                }
            }]
        })
    }

    fn normal_estimated_wire() -> Value {
        estimated_wire(
            "normal",
            vec![],
            vec![],
            0,
            "grok",
            &[("task-1-reviewer-codex", "codex")],
        )
    }

    fn high_estimated_wire() -> Value {
        estimated_wire(
            "high",
            vec![json!({
                "kind": "public_compatibility",
                "evidence": ["serialized workflow contract"]
            })],
            vec![],
            0,
            "codex",
            &[
                ("task-1-reviewer-codex", "codex"),
                ("task-1-reviewer-grok", "grok"),
            ],
        )
    }
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
            risk_policy_version: TASK_RISK_POLICY_VERSION.into(),
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
                ManifestNode {
                    id: "plan-author".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some(PHASE_PLAN.into()),
                    role: Some(ManifestNodeRole::Author),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(author_key),
                    deps: vec![],
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

    #[test]
    fn accepts_minimal_valid_manifest() {
        let doc = minimal_valid_doc();
        let normalized = validate_manifest_document(&doc).expect("valid");
        assert_eq!(normalized.task_count, 1);
        assert_eq!(normalized.nodes.len(), 7);
        assert_eq!(normalized.gates.len(), 2);
    }

    #[test]
    fn task_policy_allow_noop_verification_defaults_false_and_normalizes_true() {
        let defaulted: ManifestDocument =
            serde_json::from_value(normal_estimated_wire()).expect("defaulted manifest");
        assert!(!defaulted.task_policies[0].allow_noop_verification);
        assert!(
            !validate_manifest_document(&defaulted)
                .expect("defaulted manifest valid")
                .task_policies[0]
                .allow_noop_verification
        );

        let mut explicit = normal_estimated_wire();
        explicit["task_policies"][0]["allow_noop_verification"] = serde_json::json!(true);
        let explicit: ManifestDocument =
            serde_json::from_value(explicit).expect("explicit no-op manifest");
        assert!(
            validate_manifest_document(&explicit)
                .expect("explicit manifest valid")
                .task_policies[0]
                .allow_noop_verification
        );
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
        doc.edges
            .retain(|e| e.from != "task-1-rev" && e.to != "task-1-rev");
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::TaskRouteMismatch(_)));
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
        doc.gates[0].reviewer_cohort_node_ids.clear();
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
        doc.gates[0].reviewer_cohort_node_ids.clear();
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
        doc.gates[1].reviewer_cohort_node_ids =
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
        doc.gates[1].reviewer_cohort_node_ids = vec!["milestone-1".into()];
        let err = validate_manifest_document(&doc).unwrap_err();
        assert!(matches!(err, WorkflowError::RoleMismatch(_)));

        // Design-phase reviewer cannot satisfy a Plan gate.
        doc.gates[1].required_reviewer_node_ids = vec!["design-reviewer-1".into()];
        doc.gates[1].reviewer_cohort_node_ids = vec!["design-reviewer-1".into()];
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
        doc.schema_version = MANIFEST_SCHEMA_VERSION;
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
            "schema_version": 2,
            "workflow_kind": "brainstorm_to_delivery",
            "plan_target_rel_path": "docs/p.md",
            "risk_policy_version": "b2d_task_risk_v1",
            "publication_token": "t",
            "workflow_state": "skeleton",
            "design": { "rel_path": "docs/a.md", "digest": "d" },
            "phases": [{ "id": "design" }, { "id": "plan" }],
            "nodes": [{
                "id": "plan-author",
                "kind": "work_unit",
                "phase_id": "plan",
                "role": "author",
                "agent_type": "codex",
                "work_unit_key": "plan|docs/p.md|author|codex|none",
                "deps": []
            }],
            "edges": [],
            "gates": [{
                "id": "mystery",
                "reviewer_cohort_node_ids": [],
                "required_reviewer_node_ids": [],
                "resolution_mode": "self_review"
            }],
            "task_policies": []
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
                reviewer_cohort_node_ids: vec![plan_reviewer.clone()],
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

    #[test]
    fn v1_manifest_is_rejected() {
        let mut wire = v2_skeleton_wire();
        wire["schema_version"] = json!(1);
        let err = validate_wire(wire).expect_err("schema v1 must be rejected");
        assert!(
            err.contains("schema version") && err.contains('1'),
            "expected explicit v1 schema rejection, got {err}"
        );
    }

    #[test]
    fn v2_fields_are_required_by_serde() {
        let complete = json!({
            "schema_version": MANIFEST_SCHEMA_VERSION,
            "workflow_kind": WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
            "publication_token": "serde-v2",
            "workflow_state": "skeleton",
            "plan_target_rel_path": "docs/superpowers/plans/p.md",
            "risk_policy_version": "b2d_task_risk_v1",
            "phases": [],
            "nodes": [],
            "edges": [],
            "gates": [],
            "task_policies": []
        });
        serde_json::from_value::<ManifestDocument>(complete.clone())
            .expect("complete v2 serde fixture");
        for field in [
            "plan_target_rel_path",
            "risk_policy_version",
            "task_policies",
        ] {
            let mut missing = complete.clone();
            missing
                .as_object_mut()
                .expect("manifest object")
                .remove(field);
            let result = serde_json::from_value::<ManifestDocument>(missing);
            assert!(result.is_err(), "missing {field} must fail serde");
        }

        let mut missing_cohort = complete;
        missing_cohort["gates"] = json!([{
            "id": "plan-gate",
            "reviewer_cohort_node_ids": [],
            "required_reviewer_node_ids": [],
            "resolution_mode": "parent_adjudication"
        }]);
        serde_json::from_value::<ManifestDocument>(missing_cohort.clone())
            .expect("complete v2 gate serde fixture");
        missing_cohort["gates"][0]
            .as_object_mut()
            .expect("gate object")
            .remove("reviewer_cohort_node_ids");
        assert!(
            serde_json::from_value::<ManifestDocument>(missing_cohort).is_err(),
            "missing reviewer_cohort_node_ids must fail serde"
        );
    }

    #[test]
    fn skeleton_requires_one_codex_author_and_no_plan_or_task_policies() {
        validate_wire(v2_skeleton_wire()).expect("minimal v2 skeleton");

        let mut no_author = v2_skeleton_wire();
        no_author["nodes"] = json!([]);
        let err = validate_wire(no_author).expect_err("missing Author rejected");
        assert!(err.to_lowercase().contains("author"), "got {err}");

        let mut grok_author = v2_skeleton_wire();
        grok_author["nodes"][0]["agent_type"] = json!("grok");
        grok_author["nodes"][0]["work_unit_key"] =
            json!("plan|docs/superpowers/plans/p.md|author|grok|none");
        let err = validate_wire(grok_author).expect_err("non-Codex Author rejected");
        assert!(err.to_lowercase().contains("author"), "got {err}");

        let mut with_plan = v2_skeleton_wire();
        with_plan["plan"] = json!({
            "rel_path": "docs/superpowers/plans/p.md",
            "digest": "not-yet-allowed"
        });
        let err = validate_wire(with_plan).expect_err("skeleton Plan rejected");
        assert!(err.to_lowercase().contains("skeleton"), "got {err}");

        let mut with_policy = v2_skeleton_wire();
        with_policy["task_policies"] = normal_estimated_wire()["task_policies"].clone();
        let err = validate_wire(with_policy).expect_err("skeleton policy rejected");
        assert!(err.to_lowercase().contains("skeleton"), "got {err}");
    }

    #[test]
    fn eventual_plan_path_equals_declared_target() {
        validate_wire(normal_estimated_wire()).expect("matching Plan target");

        let mut mismatch = normal_estimated_wire();
        mismatch["plan"]["rel_path"] = json!("docs/superpowers/plans/other.md");
        let err = validate_wire(mismatch).expect_err("Plan target mismatch rejected");
        assert!(err.contains("plan_target_rel_path"), "got {err}");
    }

    #[test]
    fn invalid_or_duplicate_risk_signals_are_rejected() {
        let mut unknown = normal_estimated_wire();
        unknown["task_policies"][0]["risk"]["soft_signals"] =
            json!([soft_signal("future_signal", 1)]);
        unknown["task_policies"][0]["risk"]["score"] = json!(1);
        let err = validate_wire(unknown).expect_err("unknown signal rejected");
        assert!(
            err.contains("future_signal") || err.contains("unknown variant"),
            "got {err}"
        );

        let mut duplicate = normal_estimated_wire();
        duplicate["task_policies"][0]["risk"]["soft_signals"] = json!([
            soft_signal("shared_interface", 1),
            soft_signal("shared_interface", 1)
        ]);
        duplicate["task_policies"][0]["risk"]["score"] = json!(1);
        let err = validate_wire(duplicate).expect_err("duplicate signal rejected");
        assert!(err.to_lowercase().contains("duplicate"), "got {err}");

        let mut empty_evidence = normal_estimated_wire();
        empty_evidence["task_policies"][0]["risk"]["soft_signals"] = json!([{
            "kind": "shared_interface",
            "score": 1,
            "evidence": ["   "]
        }]);
        empty_evidence["task_policies"][0]["risk"]["score"] = json!(1);
        let err = validate_wire(empty_evidence).expect_err("blank evidence rejected");
        assert!(err.to_lowercase().contains("evidence"), "got {err}");
    }

    #[test]
    fn every_hard_trigger_forces_high_risk() {
        for kind in [
            "concurrency_lifecycle",
            "security_trust_boundary",
            "migration_destructive_persistence",
            "public_compatibility",
            "unsafe_ffi",
            "update_rollback",
        ] {
            let hard = vec![json!({
                "kind": kind,
                "evidence": [format!("evidence for {kind}")]
            })];
            let high = estimated_wire(
                "high",
                hard.clone(),
                vec![],
                0,
                "codex",
                &[
                    ("task-1-reviewer-codex", "codex"),
                    ("task-1-reviewer-grok", "grok"),
                ],
            );
            validate_wire(high).unwrap_or_else(|err| panic!("{kind} high: {err}"));

            let contradictory = estimated_wire(
                "normal",
                hard,
                vec![],
                0,
                "grok",
                &[("task-1-reviewer-codex", "codex")],
            );
            let err =
                validate_wire(contradictory).expect_err("hard trigger declared normal must fail");
            assert!(err.to_lowercase().contains("risk"), "{kind}: {err}");
        }
    }

    #[test]
    fn soft_score_threshold_table_selects_risk() {
        let cases = [
            (vec![], 0, "normal"),
            (vec![soft_signal("shared_interface", 1)], 1, "normal"),
            (
                vec![soft_signal("cross_runtime_or_process", 2)],
                2,
                "normal",
            ),
            (
                vec![
                    soft_signal("cross_runtime_or_process", 2),
                    soft_signal("shared_interface", 1),
                ],
                3,
                "high",
            ),
            (
                vec![
                    soft_signal("cross_runtime_or_process", 2),
                    soft_signal("shared_interface", 1),
                    soft_signal("dependency_or_build", 1),
                ],
                4,
                "high",
            ),
        ];
        for (signals, score, level) in cases {
            let (implementer, reviewers): (&str, Vec<(&str, &str)>) = if level == "high" {
                (
                    "codex",
                    vec![
                        ("task-1-reviewer-codex", "codex"),
                        ("task-1-reviewer-grok", "grok"),
                    ],
                )
            } else {
                ("grok", vec![("task-1-reviewer-codex", "codex")])
            };
            let wire = estimated_wire(level, vec![], signals, score, implementer, &reviewers);
            validate_wire(wire)
                .unwrap_or_else(|err| panic!("score {score} expected {level}: {err}"));
        }
    }

    #[test]
    fn submitted_soft_scores_match_unique_signal_weights() {
        let mut wrong_weight = normal_estimated_wire();
        wrong_weight["task_policies"][0]["risk"]["soft_signals"] =
            json!([soft_signal("cross_runtime_or_process", 1)]);
        wrong_weight["task_policies"][0]["risk"]["score"] = json!(1);
        let err = validate_wire(wrong_weight).expect_err("wrong signal weight rejected");
        assert!(err.to_lowercase().contains("score"), "got {err}");

        let mut wrong_total = normal_estimated_wire();
        wrong_total["task_policies"][0]["risk"]["soft_signals"] =
            json!([soft_signal("cross_runtime_or_process", 2)]);
        wrong_total["task_policies"][0]["risk"]["score"] = json!(1);
        let err = validate_wire(wrong_total).expect_err("wrong total rejected");
        assert!(err.to_lowercase().contains("score"), "got {err}");
    }

    #[test]
    fn estimated_and_approved_tasks_have_exactly_one_policy() {
        for state in ["estimated", "approved"] {
            let mut missing = normal_estimated_wire();
            missing["workflow_state"] = json!(state);
            missing["task_policies"] = json!([]);
            let err = validate_wire(missing).expect_err("missing Task policy rejected");
            assert!(err.to_lowercase().contains("policy"), "{state}: {err}");

            let mut duplicate = normal_estimated_wire();
            duplicate["workflow_state"] = json!(state);
            let policy = duplicate["task_policies"][0].clone();
            duplicate["task_policies"] = json!([policy.clone(), policy]);
            let err = validate_wire(duplicate).expect_err("duplicate Task policy rejected");
            assert!(err.to_lowercase().contains("duplicate"), "{state}: {err}");

            let mut wrong_index = normal_estimated_wire();
            wrong_index["workflow_state"] = json!(state);
            wrong_index["task_policies"][0]["task_index"] = json!(2);
            let err = validate_wire(wrong_index).expect_err("wrong Task policy index rejected");
            assert!(err.to_lowercase().contains("policy"), "{state}: {err}");
        }
    }

    #[test]
    fn normal_and_high_routes_match_agent_matrix() {
        validate_wire(normal_estimated_wire()).expect("normal Grok/Codex route");
        validate_wire(high_estimated_wire()).expect("high Codex/Codex+Grok route");

        let mut normal_wrong_implementer = normal_estimated_wire();
        normal_wrong_implementer["nodes"][3]["agent_type"] = json!("codex");
        normal_wrong_implementer["nodes"][3]["work_unit_key"] =
            json!("task|1|implementer|codex|none");
        let err = validate_wire(normal_wrong_implementer).expect_err("wrong route rejected");
        assert!(err.to_lowercase().contains("route"), "got {err}");

        let mut high_one_reviewer = high_estimated_wire();
        high_one_reviewer["task_policies"][0]["route"]["reviewer_node_ids"] =
            json!(["task-1-reviewer-codex"]);
        let err = validate_wire(high_one_reviewer).expect_err("incomplete high route rejected");
        assert!(err.to_lowercase().contains("route"), "got {err}");
    }

    #[test]
    fn plan_required_reviewers_are_subset_of_complete_cohort() {
        validate_wire(normal_estimated_wire()).expect("required Plan subset");

        let mut empty_required = normal_estimated_wire();
        empty_required["gates"][0]["required_reviewer_node_ids"] = json!([]);
        let err = validate_wire(empty_required).expect_err("empty Plan subset rejected");
        assert!(err.to_lowercase().contains("plan gate"), "got {err}");

        let mut outside_cohort = normal_estimated_wire();
        outside_cohort["gates"][0]["reviewer_cohort_node_ids"] = json!(["plan-reviewer-grok"]);
        let err = validate_wire(outside_cohort).expect_err("outside cohort rejected");
        assert!(err.to_lowercase().contains("cohort"), "got {err}");

        let mut incomplete_cohort = normal_estimated_wire();
        incomplete_cohort["gates"][0]["reviewer_cohort_node_ids"] = json!(["plan-reviewer-codex"]);
        let err = validate_wire(incomplete_cohort).expect_err("incomplete cohort rejected");
        assert!(err.to_lowercase().contains("cohort"), "got {err}");

        let mut design_self_review = normal_estimated_wire();
        design_self_review["design"] = json!({
            "rel_path": "docs/superpowers/specs/d.md",
            "digest": "design-digest"
        });
        design_self_review["phases"]
            .as_array_mut()
            .expect("phases")
            .push(json!({ "id": "design" }));
        design_self_review["gates"]
            .as_array_mut()
            .expect("gates")
            .push(json!({
                "id": "design-gate",
                "gate_kind": "design",
                "reviewer_cohort_node_ids": [],
                "required_reviewer_node_ids": [],
                "resolution_mode": "self_review"
            }));
        validate_wire(design_self_review).expect("empty Design self-review sets");
    }

    #[test]
    fn task_route_cannot_omit_duplicate_or_cross_task_nodes() {
        let mut omitted = high_estimated_wire();
        omitted["task_policies"][0]["route"]["reviewer_node_ids"] =
            json!(["task-1-reviewer-codex"]);
        let err = validate_wire(omitted).expect_err("omitted route node rejected");
        assert!(err.to_lowercase().contains("route"), "got {err}");

        let mut duplicate = high_estimated_wire();
        duplicate["task_policies"][0]["route"]["reviewer_node_ids"] =
            json!(["task-1-reviewer-codex", "task-1-reviewer-codex"]);
        let err = validate_wire(duplicate).expect_err("duplicate route node rejected");
        assert!(err.to_lowercase().contains("duplicate"), "got {err}");

        let mut wrong_task = normal_estimated_wire();
        wrong_task["nodes"][4]["task_index"] = json!(2);
        wrong_task["nodes"][4]["work_unit_key"] = json!("task|2|reviewer|codex|none");
        let err = validate_wire(wrong_task).expect_err("cross-Task route node rejected");
        assert!(
            err.to_lowercase().contains("task") || err.to_lowercase().contains("route"),
            "got {err}"
        );
    }

    #[test]
    fn workflow_v2_typed_error_real_producers_risk_assessment() {
        let mut wire = normal_estimated_wire();
        wire["risk_policy_version"] = json!("not-b2d-task-risk-v1");
        let document: ManifestDocument = serde_json::from_value(wire).expect("manifest wire");
        let error = validate_manifest_document(&document).expect_err("invalid risk policy");
        assert!(matches!(&error, WorkflowError::RiskAssessmentInvalid(_)));
        let code = crate::acp::delegation::listener::workflow_store_error_code_for_test(
            crate::acp::delegation::workflow::WorkflowStoreError::Validation(error),
        );
        assert_eq!(code, "risk_assessment_invalid");
    }

    #[test]
    fn workflow_v2_typed_error_real_producers_task_route() {
        let mut wire = high_estimated_wire();
        wire["task_policies"][0]["route"]["reviewer_node_ids"] = json!(["task-1-reviewer-codex"]);
        let document: ManifestDocument = serde_json::from_value(wire).expect("manifest wire");
        let error = validate_manifest_document(&document).expect_err("incomplete high-risk route");
        assert!(matches!(&error, WorkflowError::TaskRouteMismatch(_)));
        let code = crate::acp::delegation::listener::workflow_store_error_code_for_test(
            crate::acp::delegation::workflow::WorkflowStoreError::Validation(error),
        );
        assert_eq!(code, "task_route_mismatch");
    }
}
