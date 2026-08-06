//! Agent-facing workflow recovery DTO (`get_workflow_state`).
//!
//! May include `work_unit_key`. Must **not** be reused as the redacted
//! frontend `WorkflowGraphSnapshot` (Task 4).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::plan_review::{
    FindingSeverity, FindingStatus, PlanReviewNextAction, PlanReviewRoundState, PlanReviewScope,
    PlanRevisionKind,
};
use super::recovery_policy::WorkflowRecoveryProjection;
use super::types::{DocumentRef, ManifestTaskPolicy, ManifestWorkflowState, TaskRiskLevel};

pub const INDEX_MAX_NODES: usize = 12;
pub const INDEX_MAX_FINDING_STUBS: usize = 4;
pub const DIGEST_PREFIX_HEX_CHARS: usize = 16;

/// Full agent-facing recovery payload (A5 + B4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStateDto {
    pub workflow_id: String,
    pub parent_conversation_id: i32,
    pub workflow_kind: String,
    pub capability_version: String,
    pub workflow_state: ManifestWorkflowState,
    pub manifest_revision: u64,
    pub graph_revision: u64,
    pub schema_version: u64,
    pub publication_token: String,
    pub plan_target_rel_path: String,
    pub risk_policy_version: String,
    pub completion_protocol: super::types::CompletionProtocolWorkflowProjection,
    pub task_policies: Vec<ManifestTaskPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<DocumentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<DocumentRef>,
    pub nodes: Vec<WorkflowNodeStateDto>,
    pub gates: Vec<WorkflowGateStateDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_plan_review: Option<PlanReviewRoundState>,
    /// True when oldest completed node evidence was dropped under A15 size class.
    pub evidence_truncated: bool,
}

/// Per-node recovery evidence (B4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNodeStateDto {
    pub node_id: String,
    pub work_unit_key: String,
    pub role: String,
    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub phase_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_index: Option<u32>,
    pub is_observed: bool,
    pub retained_observed: bool,
    pub cohort_frozen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_outcome: Option<String>,
    /// Latest run for this node (by lineage_ordinal / generation), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_generation: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_validated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_conversation_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_cycle: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced_task_id: Option<String>,
    /// True when this node is part of a document-gate required-run set.
    pub required_for_gate: bool,
    /// Internal B4 truncation rank: run finished_at / admission time (not serialized).
    #[serde(skip, default)]
    pub evidence_time: Option<DateTime<Utc>>,
}

/// Per document-gate recovery block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowGateStateDto {
    pub gate_id: String,
    pub gate_kind: String,
    pub resolution_mode: String,
    pub reviewer_cohort_node_ids: Vec<String>,
    pub required_reviewer_node_ids: Vec<String>,
    /// Highest settled cycle, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_gate_cycle: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_outcome: Option<String>,
    /// Next cycle the parent may settle (1-based).
    pub next_gate_cycle: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStateIndexDto {
    pub workflow_id: String,
    pub parent_conversation_id: i32,
    pub workflow_kind: String,
    pub capability_version: String,
    pub publication_token: String,
    pub workflow_state: ManifestWorkflowState,
    pub manifest_revision: u64,
    pub graph_revision: u64,
    pub schema_version: u64,
    pub plan_target_rel_path: String,
    pub risk_policy_version: String,
    pub completion_protocol: super::types::CompletionProtocolWorkflowProjection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<WorkflowRecoveryProjection>,
    pub detail: WorkflowStateDetail,
    pub inline_findings: bool,
    pub payload_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub omitted: Vec<String>,
    pub evidence_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design: Option<DocumentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<DocumentRef>,
    pub gates: Vec<WorkflowGateStateDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_plan_review: Option<PlanReviewIndexDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<WorkflowNodeIndexDto>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_policies: Vec<TaskPolicyIndexDto>,
    pub actionable_task_routes: Vec<ActionableTaskRouteDto>,
    #[serde(rename = "_codeg_omission_state")]
    pub omission_state: WorkflowIndexOmissionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStateDetail {
    Index,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNodeIndexDto {
    pub node_id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_index: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_task_id: Option<String>,
    pub required_for_gate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_conversation_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_unit_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFindingStubDto {
    pub finding_id: String,
    pub severity: FindingSeverity,
    pub status: FindingStatus,
    pub owner_reviewer_node_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRecoverySourceDto {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_conversation_id: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReviewIndexDto {
    pub scope: PlanReviewScope,
    pub revision_kind: PlanRevisionKind,
    pub covered_author_task_id: String,
    pub covered_plan_digest: String,
    pub reviewed_reviewer_node_ids: Vec<String>,
    pub next_required_reviewer_node_ids: Vec<String>,
    pub critical_count: u32,
    pub important_count: u32,
    pub minor_count: u32,
    pub net_improvement: bool,
    pub stagnation_count: u32,
    pub rewrite_used: bool,
    pub next_action: PlanReviewNextAction,
    pub finding_total_count: u32,
    pub finding_returned_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<PlanFindingStubDto>,
    pub recovery_sources: Vec<PlanRecoverySourceDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPolicyIndexDto {
    pub task_index: u32,
    pub level: TaskRiskLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionableTaskRouteDto {
    pub task_index: u32,
    pub level: TaskRiskLevel,
    pub implementer_node_id: String,
    pub reviewer_node_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowIndexOmissionState {
    pub nodes: Vec<WorkflowIndexNodeOmissionMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowIndexNodeOmissionMeta {
    pub node_id: String,
    pub evidence_time: Option<DateTime<Utc>>,
    pub active_manifest_work_unit: bool,
    pub in_actionable_route: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowIndexOmissionStep {
    PlanFindings,
    TerminalNodeEvidence,
    TaskPolicies,
    FullDigests,
    EvidenceRefs,
    NonRequiredWorkUnitKeys,
    NonActionableNodeIndex,
    NodeIndex,
}

impl WorkflowIndexOmissionStep {
    pub const ALL: [Self; 8] = [
        Self::PlanFindings,
        Self::TerminalNodeEvidence,
        Self::TaskPolicies,
        Self::FullDigests,
        Self::EvidenceRefs,
        Self::NonRequiredWorkUnitKeys,
        Self::NonActionableNodeIndex,
        Self::NodeIndex,
    ];

    pub fn token(self) -> &'static str {
        match self {
            Self::PlanFindings => "plan_findings",
            Self::TerminalNodeEvidence => "terminal_node_evidence",
            Self::TaskPolicies => "task_policies",
            Self::FullDigests => "full_digests",
            Self::EvidenceRefs => "evidence_refs",
            Self::NonRequiredWorkUnitKeys => "non_required_work_unit_keys",
            Self::NonActionableNodeIndex => "non_actionable_node_index",
            Self::NodeIndex => "node_index",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowIndexProtectedError {
    MissingOpenFindingRecoveryPointer,
}

impl WorkflowStateIndexDto {
    pub fn public_value(&self) -> Result<Value, serde_json::Error> {
        let mut value = serde_json::to_value(self)?;
        if let Some(object) = value.as_object_mut() {
            object.remove("_codeg_omission_state");
        }
        Ok(value)
    }

    pub fn apply_omission_step(&mut self, step: WorkflowIndexOmissionStep) -> bool {
        let (changed, node_loss) = match step {
            WorkflowIndexOmissionStep::PlanFindings => (self.omit_plan_findings(), false),
            WorkflowIndexOmissionStep::TerminalNodeEvidence => {
                let removed = self.omit_terminal_node_evidence();
                (removed, removed)
            }
            WorkflowIndexOmissionStep::TaskPolicies => (self.omit_task_policies(), false),
            WorkflowIndexOmissionStep::FullDigests => (self.omit_full_digests(), false),
            WorkflowIndexOmissionStep::EvidenceRefs => (self.omit_evidence_refs(), false),
            WorkflowIndexOmissionStep::NonRequiredWorkUnitKeys => {
                (self.omit_non_required_work_unit_keys(), false)
            }
            WorkflowIndexOmissionStep::NonActionableNodeIndex => {
                let before = self.nodes.len();
                let changed = self.omit_non_actionable_node_index();
                (changed, self.nodes.len() < before)
            }
            WorkflowIndexOmissionStep::NodeIndex => {
                let removed = self.omit_node_index();
                (removed, removed)
            }
        };

        if !changed {
            return false;
        }
        self.payload_truncated = true;
        if node_loss {
            self.evidence_truncated = true;
        }
        let token = step.token();
        if !self.omitted.iter().any(|existing| existing == token) {
            self.omitted.push(token.to_string());
        }
        true
    }

    pub fn validate_protected_minimum(&self) -> Result<(), WorkflowIndexProtectedError> {
        let Some(review) = self.latest_plan_review.as_ref() else {
            return Ok(());
        };
        let has_open_findings =
            review.critical_count > 0 || review.important_count > 0 || review.minor_count > 0;
        let has_pointer = review
            .recovery_sources
            .iter()
            .any(|source| source.report_file.is_some() || source.latest_task_id.is_some());
        if has_open_findings && !has_pointer {
            return Err(WorkflowIndexProtectedError::MissingOpenFindingRecoveryPointer);
        }
        Ok(())
    }

    fn omit_plan_findings(&mut self) -> bool {
        let Some(review) = self.latest_plan_review.as_mut() else {
            return false;
        };
        if review.findings.is_empty() && review.finding_returned_count == 0 {
            return false;
        }
        review.findings.clear();
        review.finding_returned_count = 0;
        true
    }

    fn omit_terminal_node_evidence(&mut self) -> bool {
        let evidence_by_id = self
            .omission_state
            .nodes
            .iter()
            .map(|meta| (meta.node_id.as_str(), meta.evidence_time))
            .collect::<BTreeMap<_, _>>();
        let mut removed_ids = self
            .nodes
            .iter()
            .filter(|node| !node.required_for_gate && is_terminal(node.latest_status.as_deref()))
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>();
        removed_ids.sort_by(|a, b| {
            evidence_by_id
                .get(a.as_str())
                .copied()
                .flatten()
                .cmp(&evidence_by_id.get(b.as_str()).copied().flatten())
                .then_with(|| a.cmp(b))
        });
        self.remove_nodes(&removed_ids)
    }

    fn omit_task_policies(&mut self) -> bool {
        if self.task_policies.is_empty() {
            return false;
        }
        self.task_policies.clear();
        true
    }

    fn omit_full_digests(&mut self) -> bool {
        let mut changed = false;
        if let Some(design) = self.design.as_mut() {
            changed |= shorten_digest(&mut design.digest);
        }
        if let Some(plan) = self.plan.as_mut() {
            changed |= shorten_digest(&mut plan.digest);
        }
        if let Some(review) = self.latest_plan_review.as_mut() {
            changed |= shorten_digest(&mut review.covered_plan_digest);
        }
        for node in &mut self.nodes {
            if let Some(digest) = node.artifact_digest.as_mut() {
                changed |= shorten_digest(digest);
            }
        }
        changed
    }

    fn omit_evidence_refs(&mut self) -> bool {
        let Some(review) = self.latest_plan_review.as_mut() else {
            return false;
        };
        let mut changed = false;
        for finding in &mut review.findings {
            changed |= finding.evidence_ref.take().is_some();
        }
        for finding in &mut review.findings {
            changed |= finding.report_file.take().is_some();
        }

        if review
            .recovery_sources
            .iter()
            .any(|source| source.latest_task_id.is_some())
        {
            for source in &mut review.recovery_sources {
                if source.latest_task_id.is_some() {
                    changed |= source.report_file.take().is_some();
                }
            }
        } else {
            let retained_report_node_id = review
                .recovery_sources
                .iter()
                .filter(|source| source.report_file.is_some())
                .map(|source| source.node_id.as_str())
                .min()
                .map(str::to_owned);
            for source in &mut review.recovery_sources {
                if source.report_file.is_some()
                    && retained_report_node_id.as_deref() != Some(source.node_id.as_str())
                {
                    source.report_file = None;
                    changed = true;
                }
            }
        }
        changed
    }

    fn omit_non_required_work_unit_keys(&mut self) -> bool {
        let meta_by_id = self
            .omission_state
            .nodes
            .iter()
            .map(|meta| (meta.node_id.clone(), meta.in_actionable_route))
            .collect::<BTreeMap<_, _>>();
        let mut changed = false;
        for node in &mut self.nodes {
            let in_actionable_route = meta_by_id
                .get(node.node_id.as_str())
                .copied()
                .unwrap_or(false);
            if !node.required_for_gate && !in_actionable_route {
                changed |= node.work_unit_key.take().is_some();
            }
        }
        changed
    }

    fn omit_non_actionable_node_index(&mut self) -> bool {
        let meta_by_id = self
            .omission_state
            .nodes
            .iter()
            .map(|meta| (meta.node_id.clone(), meta.in_actionable_route))
            .collect::<BTreeMap<_, _>>();
        let removed_ids = self
            .nodes
            .iter()
            .filter(|node| {
                let actionable = meta_by_id
                    .get(node.node_id.as_str())
                    .copied()
                    .unwrap_or(false);
                !node.required_for_gate && is_terminal(node.latest_status.as_deref()) && !actionable
            })
            .map(|node| node.node_id.clone())
            .collect::<Vec<_>>();
        let mut changed = self.remove_nodes(&removed_ids);

        for node in &mut self.nodes {
            let actionable = meta_by_id
                .get(node.node_id.as_str())
                .copied()
                .unwrap_or(false);
            changed |= node.agent_type.take().is_some();
            changed |= node.phase_id.take().is_some();
            changed |= node.child_conversation_id.take().is_some();
            changed |= node.verdict.take().is_some();
            changed |= node.report_file.take().is_some();
            changed |= node.artifact_digest.take().is_some();
            if !node.required_for_gate && !actionable {
                changed |= node.work_unit_key.take().is_some();
            }
        }
        changed
    }

    fn omit_node_index(&mut self) -> bool {
        if self.nodes.is_empty() && self.omission_state.nodes.is_empty() {
            return false;
        }
        self.nodes.clear();
        self.omission_state.nodes.clear();
        true
    }

    fn remove_nodes(&mut self, removed_ids: &[String]) -> bool {
        if removed_ids.is_empty() {
            return false;
        }
        let removed_ids = removed_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        self.nodes
            .retain(|node| !removed_ids.contains(node.node_id.as_str()));
        self.omission_state
            .nodes
            .retain(|meta| !removed_ids.contains(meta.node_id.as_str()));
        true
    }
}

fn shorten_digest(digest: &mut String) -> bool {
    let (prefix, payload) = digest
        .strip_prefix("sha256:")
        .map(|payload| ("sha256:", payload))
        .unwrap_or(("", digest.as_str()));
    if payload.len() <= DIGEST_PREFIX_HEX_CHARS
        || !payload.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return false;
    }
    *digest = format!("{prefix}{}", &payload[..DIGEST_PREFIX_HEX_CHARS]);
    true
}

pub fn project_workflow_state_index(
    state: WorkflowStateDto,
    active_manifest_node_ids: &HashSet<String>,
    task_gate_passed: &BTreeMap<u32, bool>,
) -> WorkflowStateIndexDto {
    let mut policies = state.task_policies;
    policies.sort_by_key(|policy| policy.task_index);

    let task_policies = policies
        .iter()
        .map(|policy| TaskPolicyIndexDto {
            task_index: policy.task_index,
            level: policy.risk.level,
        })
        .collect();
    let actionable_task_routes = if matches!(
        state.workflow_state,
        ManifestWorkflowState::Estimated | ManifestWorkflowState::Approved
    ) {
        project_actionable_routes(&policies, &state.nodes, task_gate_passed)
    } else {
        Vec::new()
    };
    let actionable_node_ids = actionable_task_routes
        .first()
        .into_iter()
        .flat_map(|route| {
            std::iter::once(route.implementer_node_id.as_str())
                .chain(route.reviewer_node_ids.iter().map(String::as_str))
        })
        .collect::<HashSet<_>>();

    let latest_plan_review = state
        .latest_plan_review
        .as_ref()
        .map(|round| project_plan_review(round, &state.gates, &state.nodes));
    let findings_were_capped = latest_plan_review
        .as_ref()
        .is_some_and(|round| round.finding_total_count > round.finding_returned_count);

    let original_node_count = state.nodes.len();
    let mut ranked_nodes = state.nodes;
    ranked_nodes
        .sort_by(|a, b| compare_node_rank(a, b, active_manifest_node_ids, &actionable_node_ids));
    ranked_nodes.truncate(INDEX_MAX_NODES);
    let nodes_were_capped = ranked_nodes.len() < original_node_count;

    let mut nodes = Vec::with_capacity(ranked_nodes.len());
    let mut omission_nodes = Vec::with_capacity(ranked_nodes.len());
    for node in ranked_nodes {
        let in_actionable_route = actionable_node_ids.contains(node.node_id.as_str());
        omission_nodes.push(WorkflowIndexNodeOmissionMeta {
            node_id: node.node_id.clone(),
            evidence_time: node.evidence_time,
            active_manifest_work_unit: active_manifest_node_ids.contains(&node.node_id),
            in_actionable_route,
        });
        nodes.push(WorkflowNodeIndexDto {
            node_id: node.node_id,
            role: node.role,
            agent_type: Some(node.agent_type),
            phase_id: Some(node.phase_id),
            task_index: node.task_index,
            latest_status: node.latest_status,
            latest_task_id: node.latest_task_id,
            required_for_gate: node.required_for_gate,
            child_conversation_id: node.child_conversation_id,
            verdict: node.verdict,
            report_file: node.report_file,
            artifact_digest: node.artifact_digest,
            work_unit_key: Some(node.work_unit_key),
        });
    }

    WorkflowStateIndexDto {
        workflow_id: state.workflow_id,
        parent_conversation_id: state.parent_conversation_id,
        workflow_kind: state.workflow_kind,
        capability_version: state.capability_version,
        publication_token: state.publication_token,
        workflow_state: state.workflow_state,
        manifest_revision: state.manifest_revision,
        graph_revision: state.graph_revision,
        schema_version: state.schema_version,
        plan_target_rel_path: state.plan_target_rel_path,
        risk_policy_version: state.risk_policy_version,
        completion_protocol: state.completion_protocol,
        recovery: None,
        detail: WorkflowStateDetail::Index,
        inline_findings: false,
        payload_truncated: state.evidence_truncated || findings_were_capped || nodes_were_capped,
        omitted: if findings_were_capped {
            vec!["plan_findings".to_string()]
        } else {
            Vec::new()
        },
        evidence_truncated: state.evidence_truncated || nodes_were_capped,
        design: state.design,
        plan: state.plan,
        gates: state.gates,
        latest_plan_review,
        nodes,
        task_policies,
        actionable_task_routes,
        omission_state: WorkflowIndexOmissionState {
            nodes: omission_nodes,
        },
    }
}

fn project_actionable_routes(
    policies: &[ManifestTaskPolicy],
    nodes: &[WorkflowNodeStateDto],
    task_gate_passed: &BTreeMap<u32, bool>,
) -> Vec<ActionableTaskRouteDto> {
    let active_position = policies.iter().position(|policy| {
        !task_gate_passed
            .get(&policy.task_index)
            .copied()
            .unwrap_or(false)
            && route_has_durable_evidence(policy, nodes)
    });

    let mut selected = Vec::with_capacity(2);
    if let Some(active_position) = active_position {
        selected.push(&policies[active_position]);
        let lower_tasks_passed = policies[..active_position].iter().all(|policy| {
            task_gate_passed
                .get(&policy.task_index)
                .copied()
                .unwrap_or(false)
        });
        if lower_tasks_passed {
            if let Some(candidate) = policies
                .iter()
                .skip(active_position + 1)
                .find(|policy| {
                    !task_gate_passed
                        .get(&policy.task_index)
                        .copied()
                        .unwrap_or(false)
                })
                .filter(|policy| !route_has_durable_evidence(policy, nodes))
            {
                selected.push(candidate);
            }
        }
    } else if let Some(candidate) = policies.iter().find(|policy| {
        !task_gate_passed
            .get(&policy.task_index)
            .copied()
            .unwrap_or(false)
    }) {
        selected.push(candidate);
    }

    selected
        .into_iter()
        .map(|policy| ActionableTaskRouteDto {
            task_index: policy.task_index,
            level: policy.risk.level,
            implementer_node_id: policy.route.implementer_node_id.clone(),
            reviewer_node_ids: policy.route.reviewer_node_ids.clone(),
        })
        .collect()
}

fn route_has_durable_evidence(policy: &ManifestTaskPolicy, nodes: &[WorkflowNodeStateDto]) -> bool {
    nodes.iter().any(|node| {
        node.latest_task_id.is_some()
            && (node.node_id == policy.route.implementer_node_id
                || policy.route.reviewer_node_ids.contains(&node.node_id))
    })
}

fn project_plan_review(
    round: &PlanReviewRoundState,
    gates: &[WorkflowGateStateDto],
    nodes: &[WorkflowNodeStateDto],
) -> PlanReviewIndexDto {
    let mut findings = round.findings.iter().collect::<Vec<_>>();
    findings.sort_by(|a, b| finding_rank(a).cmp(&finding_rank(b)));
    let finding_total_count = findings.len() as u32;
    let finding_stubs = findings
        .iter()
        .take(INDEX_MAX_FINDING_STUBS)
        .map(|finding| PlanFindingStubDto {
            finding_id: finding.finding_id.clone(),
            severity: finding.severity,
            status: finding.status,
            owner_reviewer_node_ids: finding.owner_reviewer_node_ids.clone(),
            report_file: Some(finding.report_file.clone()),
            evidence_ref: Some(finding.evidence_ref.clone()),
        })
        .collect::<Vec<_>>();

    let required_reviewers = gates
        .iter()
        .find(|gate| gate.gate_kind == "plan")
        .map(|gate| gate.required_reviewer_node_ids.as_slice())
        .unwrap_or_default();
    let recovery_sources = project_recovery_sources(required_reviewers, round, nodes, &findings);

    PlanReviewIndexDto {
        scope: round.scope,
        revision_kind: round.revision_kind,
        covered_author_task_id: round.covered_author_task_id.clone(),
        covered_plan_digest: round.covered_plan_digest.clone(),
        reviewed_reviewer_node_ids: round.reviewed_reviewer_node_ids.clone(),
        next_required_reviewer_node_ids: round.next_required_reviewer_node_ids.clone(),
        critical_count: round.critical_count,
        important_count: round.important_count,
        minor_count: round.minor_count,
        net_improvement: round.net_improvement,
        stagnation_count: round.stagnation_count,
        rewrite_used: round.rewrite_used,
        next_action: round.next_action,
        finding_total_count,
        finding_returned_count: finding_stubs.len() as u32,
        findings: finding_stubs,
        recovery_sources,
    }
}

fn project_recovery_sources(
    required_reviewers: &[String],
    round: &PlanReviewRoundState,
    nodes: &[WorkflowNodeStateDto],
    ranked_findings: &[&super::plan_review::PlanFindingUpdate],
) -> Vec<PlanRecoverySourceDto> {
    let mut seen = HashSet::new();
    let mut sources = Vec::new();
    for reviewer_id in required_reviewers {
        if !seen.insert(reviewer_id.as_str()) {
            continue;
        }
        let node = nodes.iter().find(|node| node.node_id == *reviewer_id);
        let finding_report = ranked_findings.iter().find_map(|finding| {
            (finding.status != FindingStatus::Resolved
                && finding.owner_reviewer_node_ids.contains(reviewer_id))
            .then(|| finding.report_file.clone())
        });
        sources.push(PlanRecoverySourceDto {
            node_id: reviewer_id.clone(),
            report_file: node
                .and_then(|node| node.report_file.clone())
                .or(finding_report),
            latest_task_id: node.and_then(|node| node.latest_task_id.clone()),
            child_conversation_id: node.and_then(|node| node.child_conversation_id),
        });
    }

    let has_open_findings = round
        .findings
        .iter()
        .any(|finding| finding.status != FindingStatus::Resolved);
    let has_pointer = sources
        .iter()
        .any(|source| source.report_file.is_some() || source.latest_task_id.is_some());
    if has_open_findings && !has_pointer {
        if let Some(finding) = ranked_findings
            .iter()
            .find(|finding| finding.status != FindingStatus::Resolved)
        {
            let mut owners = finding.owner_reviewer_node_ids.clone();
            owners.sort();
            if let Some(node_id) = owners.first() {
                sources.push(PlanRecoverySourceDto {
                    node_id: node_id.clone(),
                    report_file: Some(finding.report_file.clone()),
                    latest_task_id: None,
                    child_conversation_id: None,
                });
            }
        }
    }
    sources
}

fn finding_rank(finding: &super::plan_review::PlanFindingUpdate) -> (u8, u8, u8, &str) {
    let primary_bucket = match (finding.severity, finding.status) {
        (FindingSeverity::Critical | FindingSeverity::Important, status)
            if status != FindingStatus::Resolved =>
        {
            0
        }
        _ => 1,
    };
    let severity_rank = match finding.severity {
        FindingSeverity::Critical => 0,
        FindingSeverity::Important => 1,
        FindingSeverity::Minor => 2,
    };
    let status_rank = match finding.status {
        FindingStatus::Open => 0,
        FindingStatus::New => 1,
        FindingStatus::Reopened => 2,
        FindingStatus::Resolved => 3,
    };
    (
        primary_bucket,
        severity_rank,
        status_rank,
        finding.finding_id.as_str(),
    )
}

fn compare_node_rank(
    a: &WorkflowNodeStateDto,
    b: &WorkflowNodeStateDto,
    active_manifest_node_ids: &HashSet<String>,
    actionable_node_ids: &HashSet<&str>,
) -> Ordering {
    (!a.required_for_gate)
        .cmp(&!b.required_for_gate)
        .then_with(|| {
            (!actionable_node_ids.contains(a.node_id.as_str()))
                .cmp(&!actionable_node_ids.contains(b.node_id.as_str()))
        })
        .then_with(|| {
            is_terminal(a.latest_status.as_deref()).cmp(&is_terminal(b.latest_status.as_deref()))
        })
        .then_with(|| {
            (!active_manifest_node_ids.contains(&a.node_id))
                .cmp(&!active_manifest_node_ids.contains(&b.node_id))
        })
        .then_with(|| {
            a.task_index
                .unwrap_or(u32::MAX)
                .cmp(&b.task_index.unwrap_or(u32::MAX))
        })
        .then_with(|| b.evidence_time.cmp(&a.evidence_time))
        .then_with(|| a.node_id.cmp(&b.node_id))
}

fn is_terminal(status: Option<&str>) -> bool {
    matches!(status, Some("completed" | "failed" | "canceled"))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use chrono::TimeZone;
    use sea_orm::Iterable;

    use super::*;
    use crate::acp::delegation::workflow::plan_review::{
        FindingSeverity, FindingStatus, PlanFindingUpdate, PlanReviewNextAction, PlanReviewScope,
        PlanRevisionKind,
    };
    use crate::acp::delegation::workflow::types::{
        CompletionProtocolWorkflowProjection, ManifestTaskRisk, ManifestTaskRoute, TaskRiskLevel,
    };
    use crate::db::entities::delegation_workflow_node_binding;

    fn timestamp(second: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, second)
            .single()
            .unwrap()
    }

    fn sample_node(
        node_id: impl Into<String>,
        role: &str,
        task_index: Option<u32>,
        latest_status: &str,
        required_for_gate: bool,
        evidence_time: DateTime<Utc>,
    ) -> WorkflowNodeStateDto {
        let node_id = node_id.into();
        WorkflowNodeStateDto {
            work_unit_key: format!("task|{}|{role}|codex|none", task_index.unwrap_or(0)),
            node_id: node_id.clone(),
            role: role.to_string(),
            agent_type: "codex".to_string(),
            profile_id: Some("profile-that-must-not-leak".to_string()),
            phase_id: if task_index.is_some() {
                "tasks".to_string()
            } else {
                "plan".to_string()
            },
            task_index,
            is_observed: true,
            retained_observed: false,
            cohort_frozen: true,
            node_outcome: None,
            latest_task_id: Some(format!("task-{node_id}")),
            latest_status: Some(latest_status.to_string()),
            latest_generation: Some(7),
            summary_validated: Some(true),
            artifact_digest: Some(format!("sha256:{}", "abcdef0123456789".repeat(4))),
            child_conversation_id: Some(900),
            reviewed_task_id: Some("reviewed-task-that-must-not-leak".to_string()),
            verdict: Some("done".to_string()),
            report_file: Some(format!("reports/{node_id}.md")),
            gate_id: None,
            gate_cycle: None,
            replaced_task_id: Some("replacement-that-must-not-leak".to_string()),
            required_for_gate,
            evidence_time: Some(evidence_time),
        }
    }

    fn sample_finding(index: usize) -> PlanFindingUpdate {
        PlanFindingUpdate {
            finding_id: format!("finding-{index:02}"),
            severity: match index % 3 {
                0 => FindingSeverity::Critical,
                1 => FindingSeverity::Important,
                _ => FindingSeverity::Minor,
            },
            status: match index % 4 {
                0 => FindingStatus::Open,
                1 => FindingStatus::New,
                2 => FindingStatus::Reopened,
                _ => FindingStatus::Resolved,
            },
            owner_reviewer_node_ids: vec!["plan-reviewer-codex".to_string()],
            summary: "prose".repeat(1024),
            evidence_ref: format!("docs/plan.md#finding-{index:02}"),
            report_file: format!("reports/finding-{index:02}.md"),
        }
    }

    fn sample_policy(
        task_index: u32,
        level: TaskRiskLevel,
        implementer: &str,
        reviewer: &str,
    ) -> ManifestTaskPolicy {
        ManifestTaskPolicy {
            task_index,
            risk: ManifestTaskRisk {
                level,
                hard_triggers: Vec::new(),
                soft_signals: Vec::new(),
                score: if level == TaskRiskLevel::High { 3 } else { 0 },
                reason: "long risk reason that must not leak".to_string(),
            },
            route: ManifestTaskRoute {
                implementer_node_id: implementer.to_string(),
                reviewer_node_ids: vec![reviewer.to_string()],
            },
            allow_noop_verification: false,
        }
    }

    fn sample_full_state(node_count: usize, finding_count: usize) -> WorkflowStateDto {
        let named_nodes = [
            sample_node(
                "plan-reviewer-codex",
                "reviewer",
                None,
                "completed",
                true,
                timestamp(59),
            ),
            sample_node(
                "plan-reviewer-grok",
                "reviewer",
                None,
                "completed",
                true,
                timestamp(30),
            ),
            sample_node(
                "task-1-impl",
                "implementer",
                Some(1),
                "running",
                false,
                timestamp(50),
            ),
            sample_node(
                "task-1-review-codex",
                "reviewer",
                Some(1),
                "pending",
                false,
                timestamp(49),
            ),
            sample_node(
                "task-2-impl",
                "implementer",
                Some(2),
                "pending",
                false,
                timestamp(48),
            ),
            sample_node(
                "task-2-review-grok",
                "reviewer",
                Some(2),
                "pending",
                false,
                timestamp(47),
            ),
            sample_node(
                "rank-z-newer",
                "reviewer",
                None,
                "running",
                false,
                timestamp(46),
            ),
            sample_node(
                "rank-a-older",
                "reviewer",
                None,
                "running",
                false,
                timestamp(40),
            ),
            sample_node(
                "rank-a-tie",
                "reviewer",
                None,
                "running",
                false,
                timestamp(45),
            ),
            sample_node(
                "rank-b-tie",
                "reviewer",
                None,
                "running",
                false,
                timestamp(45),
            ),
        ];
        let mut nodes = named_nodes.into_iter().take(node_count).collect::<Vec<_>>();
        for node in &mut nodes {
            if node.task_index == Some(2) {
                node.latest_task_id = None;
            }
        }
        for index in nodes.len()..node_count {
            nodes.push(sample_node(
                format!("overflow-{index:02}"),
                "reviewer",
                None,
                "completed",
                false,
                timestamp((index % 30) as u32),
            ));
        }

        WorkflowStateDto {
            workflow_id: "068d06a4-e4b5-4b70-9c29-4ff176a67746".to_string(),
            parent_conversation_id: 42,
            workflow_kind: "brainstorm_to_delivery".to_string(),
            capability_version: "1".to_string(),
            workflow_state: ManifestWorkflowState::Estimated,
            manifest_revision: 6,
            graph_revision: 56,
            schema_version: 1,
            publication_token: "publication-token".to_string(),
            plan_target_rel_path: "docs/plans/workflow.md".to_string(),
            risk_policy_version: "risk-v1".to_string(),
            completion_protocol: CompletionProtocolWorkflowProjection {
                version: 1,
                mode: crate::db::entities::delegation_workflow::CompletionProtocolMode::V1,
                creation_mode: crate::db::entities::delegation_workflow::CompletionProtocolMode::V1,
                legacy_source: None,
                v2_successor: None,
                read_only_reason: None,
                automatic_root_wake: false,
            },
            task_policies: vec![
                sample_policy(1, TaskRiskLevel::High, "task-1-impl", "task-1-review-codex"),
                sample_policy(
                    2,
                    TaskRiskLevel::Normal,
                    "task-2-impl",
                    "task-2-review-grok",
                ),
            ],
            design: Some(DocumentRef {
                rel_path: "docs/design.md".to_string(),
                digest: format!("sha256:{}", "0123456789abcdef".repeat(4)),
            }),
            plan: Some(DocumentRef {
                rel_path: "docs/plan.md".to_string(),
                digest: format!("sha256:{}", "fedcba9876543210".repeat(4)),
            }),
            nodes,
            gates: vec![WorkflowGateStateDto {
                gate_id: "plan-gate".to_string(),
                gate_kind: "plan".to_string(),
                resolution_mode: "parent_adjudication".to_string(),
                reviewer_cohort_node_ids: vec![
                    "plan-reviewer-codex".to_string(),
                    "plan-reviewer-grok".to_string(),
                ],
                required_reviewer_node_ids: vec![
                    "plan-reviewer-codex".to_string(),
                    "plan-reviewer-grok".to_string(),
                ],
                latest_gate_cycle: Some(2),
                latest_outcome: Some("changes_requested".to_string()),
                next_gate_cycle: 3,
            }],
            latest_plan_review: Some(PlanReviewRoundState {
                scope: PlanReviewScope::Full,
                revision_kind: PlanRevisionKind::Material,
                scope_reason: "long scope reason that must not leak".to_string(),
                covered_author_task_id: "plan-author-task".to_string(),
                covered_plan_digest: format!("sha256:{}", "13579bdf02468ace".repeat(4)),
                reviewed_reviewer_node_ids: vec![
                    "plan-reviewer-codex".to_string(),
                    "plan-reviewer-grok".to_string(),
                ],
                next_required_reviewer_node_ids: vec![
                    "historical-reviewer-that-must-not-drive-recovery".to_string(),
                ],
                findings: (0..finding_count).map(sample_finding).collect(),
                lineage_reset_reason: Some(
                    "long lineage reset reason that must not leak".to_string(),
                ),
                critical_count: 5,
                important_count: 5,
                minor_count: 5,
                net_improvement: true,
                stagnation_count: 0,
                rewrite_used: false,
                next_action: PlanReviewNextAction::ContinueReview,
            }),
            evidence_truncated: false,
        }
    }

    fn projected_status_fixture<const N: usize>(
        statuses: [(&str, &str); N],
    ) -> WorkflowStateIndexDto {
        let mut state = sample_full_state(0, 0);
        state.nodes = statuses
            .into_iter()
            .enumerate()
            .map(|(index, (node_id, status))| {
                sample_node(
                    node_id,
                    "reviewer",
                    None,
                    status,
                    false,
                    timestamp(index as u32),
                )
            })
            .collect();
        project_workflow_state_index(
            state,
            &HashSet::new(),
            &BTreeMap::from([(1, false), (2, false)]),
        )
    }

    #[test]
    fn dto_cohort_frozen_contract_uses_only_active_vocabulary() {
        let dto: WorkflowNodeStateDto = serde_json::from_value(serde_json::json!({
            "node_id": "task-1-impl",
            "work_unit_key": "task|1|implementer|codex|none",
            "role": "implementer",
            "agent_type": "codex",
            "phase_id": "tasks",
            "is_observed": true,
            "retained_observed": false,
            "cohort_frozen": true,
            "required_for_gate": false
        }))
        .expect("active recovery DTO accepts cohort_frozen");

        let json = serde_json::to_value(dto).unwrap();
        assert_eq!(json.get("cohort_frozen"), Some(&serde_json::json!(true)));
        assert!(json.get(concat!("pair", "_frozen")).is_none());
    }

    #[test]
    fn entity_cohort_frozen_contract_uses_only_active_identifier() {
        let identifiers: Vec<String> = delegation_workflow_node_binding::Column::iter()
            .map(|column| format!("{column:?}"))
            .collect();

        assert!(identifiers.iter().any(|name| name == "CohortFrozen"));
        assert!(!identifiers.iter().any(|name| name == "PairFrozen"));
    }

    #[test]
    fn index_projection_caps_and_removes_rich_recovery_bodies() {
        let index = project_workflow_state_index(
            sample_full_state(20, 15),
            &HashSet::from(["task-1-impl".to_string(), "task-1-review-codex".to_string()]),
            &BTreeMap::from([(1, false), (2, false)]),
        );
        let json = index.public_value().unwrap();
        assert_eq!(json["detail"], "index");
        assert_eq!(json["inline_findings"], false);
        assert!(json["nodes"].as_array().unwrap().len() <= INDEX_MAX_NODES);
        assert!(
            json["latest_plan_review"]["findings"]
                .as_array()
                .unwrap()
                .len()
                <= INDEX_MAX_FINDING_STUBS
        );
        assert!(json
            .pointer("/latest_plan_review/findings/0/summary")
            .is_none());
        assert!(json.to_string().find(&"prose".repeat(1024)).is_none());
        assert_eq!(json["latest_plan_review"]["finding_total_count"], 15);
        assert_eq!(json["payload_truncated"], true);
        assert_eq!(json["omitted"][0], "plan_findings");
        assert!(json.get("_codeg_omission_state").is_none());
    }

    #[test]
    fn node_rank_is_independent_of_input_row_order() {
        let ordered = sample_full_state(20, 15);
        let mut reversed = ordered.clone();
        reversed.nodes.reverse();
        let active = HashSet::from(["task-1-impl".to_string()]);
        let gates = BTreeMap::from([(1, false), (2, false)]);
        let a = project_workflow_state_index(ordered, &active, &gates);
        let b = project_workflow_state_index(reversed, &active, &gates);
        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );
        assert_eq!(a.nodes[0].node_id, "plan-reviewer-codex");
        let ids = a
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<Vec<_>>();
        assert!(
            ids.iter().position(|id| *id == "rank-z-newer")
                < ids.iter().position(|id| *id == "rank-a-older")
        );
        assert!(
            ids.iter().position(|id| *id == "rank-a-tie")
                < ids.iter().position(|id| *id == "rank-b-tie")
        );
        assert!(a.evidence_truncated);
    }

    #[test]
    fn node_only_pre_cap_sets_both_truncation_flags_and_reports_omission() {
        let mut state = sample_full_state(0, 4);
        state.nodes = (0..14)
            .map(|index| {
                sample_node(
                    format!("node-{index:02}"),
                    "reviewer",
                    None,
                    "completed",
                    false,
                    timestamp(1),
                )
            })
            .collect();
        let mut reversed = state.clone();
        reversed.nodes.reverse();
        let active = HashSet::new();
        let gates = BTreeMap::from([(1, false), (2, false)]);
        let a = project_workflow_state_index(state, &active, &gates);
        let b = project_workflow_state_index(reversed, &active, &gates);
        let expected = (0..12)
            .map(|index| format!("node-{index:02}"))
            .collect::<Vec<_>>();
        assert_eq!(
            a.nodes
                .iter()
                .map(|node| node.node_id.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );
        assert!(a.payload_truncated);
        assert!(a.evidence_truncated);
        assert!(a.omitted.is_empty());
    }

    #[test]
    fn finding_pre_cap_prioritizes_non_resolved_critical_and_important() {
        let mut state = sample_full_state(4, 0);
        let findings = [
            (
                "critical-open",
                FindingSeverity::Critical,
                FindingStatus::Open,
            ),
            (
                "critical-resolved",
                FindingSeverity::Critical,
                FindingStatus::Resolved,
            ),
            (
                "important-open",
                FindingSeverity::Important,
                FindingStatus::Open,
            ),
            (
                "important-new",
                FindingSeverity::Important,
                FindingStatus::New,
            ),
            (
                "important-reopened",
                FindingSeverity::Important,
                FindingStatus::Reopened,
            ),
            ("minor-open", FindingSeverity::Minor, FindingStatus::Open),
        ]
        .into_iter()
        .map(|(finding_id, severity, status)| PlanFindingUpdate {
            finding_id: finding_id.to_string(),
            severity,
            status,
            owner_reviewer_node_ids: vec!["plan-reviewer-codex".to_string()],
            summary: "prose".repeat(1024),
            evidence_ref: format!("docs/plan.md#{finding_id}"),
            report_file: format!("reports/{finding_id}.md"),
        })
        .collect::<Vec<_>>();
        state.latest_plan_review.as_mut().unwrap().findings = findings;
        let mut reversed = state.clone();
        reversed
            .latest_plan_review
            .as_mut()
            .unwrap()
            .findings
            .reverse();
        let active = HashSet::new();
        let gates = BTreeMap::from([(1, false), (2, false)]);
        let a = project_workflow_state_index(state, &active, &gates);
        let b = project_workflow_state_index(reversed, &active, &gates);
        let retained = a
            .latest_plan_review
            .as_ref()
            .unwrap()
            .findings
            .iter()
            .map(|finding| finding.finding_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            retained,
            vec![
                "critical-open",
                "important-open",
                "important-new",
                "important-reopened",
            ]
        );
        assert_eq!(
            serde_json::to_vec(&a).unwrap(),
            serde_json::to_vec(&b).unwrap()
        );
    }

    #[test]
    fn actionable_routes_are_only_recovery_metadata_in_estimated_and_approved() {
        for workflow_state in [
            ManifestWorkflowState::Estimated,
            ManifestWorkflowState::Approved,
        ] {
            let mut state = sample_full_state(4, 0);
            state.workflow_state = workflow_state;
            let index = project_workflow_state_index(
                state,
                &HashSet::new(),
                &BTreeMap::from([(1, false), (2, false)]),
            );
            assert_eq!(
                index
                    .actionable_task_routes
                    .iter()
                    .map(|route| route.task_index)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );
        }

        for workflow_state in [
            ManifestWorkflowState::Skeleton,
            ManifestWorkflowState::Blocked,
        ] {
            let mut state = sample_full_state(4, 0);
            state.workflow_state = workflow_state;
            let index = project_workflow_state_index(
                state,
                &HashSet::new(),
                &BTreeMap::from([(1, false), (2, false)]),
            );
            assert!(index.actionable_task_routes.is_empty());
        }
    }

    #[test]
    fn actionable_routes_use_durable_evidence_and_only_protect_the_first_route() {
        let state = sample_full_state(6, 0);
        let index = project_workflow_state_index(
            state.clone(),
            &HashSet::new(),
            &BTreeMap::from([(1, false), (2, false)]),
        );
        assert_eq!(
            index
                .actionable_task_routes
                .iter()
                .map(|route| route.task_index)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(
            index
                .omission_state
                .nodes
                .iter()
                .find(|meta| meta.node_id == "task-1-impl")
                .unwrap()
                .in_actionable_route
        );
        assert!(
            !index
                .omission_state
                .nodes
                .iter()
                .find(|meta| meta.node_id == "task-2-impl")
                .unwrap()
                .in_actionable_route
        );

        let after_task_one = project_workflow_state_index(
            state,
            &HashSet::new(),
            &BTreeMap::from([(1, true), (2, false)]),
        );
        assert_eq!(
            after_task_one
                .actionable_task_routes
                .iter()
                .map(|route| route.task_index)
                .collect::<Vec<_>>(),
            vec![2]
        );
        assert!(
            after_task_one
                .omission_state
                .nodes
                .iter()
                .find(|meta| meta.node_id == "task-2-impl")
                .unwrap()
                .in_actionable_route
        );
    }

    #[test]
    fn earlier_non_passed_task_blocks_candidate_after_later_durable_evidence() {
        let mut state = sample_full_state(6, 0);
        for node in &mut state.nodes {
            if node.task_index == Some(1) {
                node.latest_task_id = None;
            } else if node.node_id == "task-2-impl" {
                node.latest_task_id = Some("durable-task-2".to_string());
            }
        }
        state.task_policies.push(sample_policy(
            3,
            TaskRiskLevel::Normal,
            "task-3-impl",
            "task-3-reviewer",
        ));

        let index = project_workflow_state_index(
            state,
            &HashSet::new(),
            &BTreeMap::from([(1, false), (2, false), (3, false)]),
        );
        assert_eq!(
            index
                .actionable_task_routes
                .iter()
                .map(|route| route.task_index)
                .collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn inherited_evidence_truncation_also_marks_payload_truncated() {
        let mut state = sample_full_state(4, 0);
        state.evidence_truncated = true;
        let index = project_workflow_state_index(
            state,
            &HashSet::new(),
            &BTreeMap::from([(1, false), (2, false)]),
        );
        assert!(index.evidence_truncated);
        assert!(index.payload_truncated);
    }

    #[test]
    fn omission_ladder_is_ordered_idempotent_and_preserves_authority() {
        let mut index = project_workflow_state_index(
            sample_full_state(12, 4),
            &HashSet::from(["task-1-impl".to_string()]),
            &BTreeMap::from([(1, false), (2, false)]),
        );
        let original_gate = index.gates[0].clone();
        let original_route = index.actionable_task_routes[0].clone();
        for step in WorkflowIndexOmissionStep::ALL {
            index.apply_omission_step(step);
            index.apply_omission_step(step);
        }
        assert_eq!(
            index.omitted,
            vec![
                "plan_findings",
                "terminal_node_evidence",
                "task_policies",
                "full_digests",
                "evidence_refs",
                "non_required_work_unit_keys",
                "non_actionable_node_index",
                "node_index",
            ]
        );
        assert_eq!(index.gates[0], original_gate);
        assert_eq!(index.actionable_task_routes[0], original_route);
        assert_eq!(
            index.design.as_ref().unwrap().digest,
            "sha256:0123456789abcdef"
        );
        assert!(index.nodes.is_empty());
    }

    #[test]
    fn open_findings_require_a_recovery_report_or_task() {
        let mut index = project_workflow_state_index(
            sample_full_state(12, 4),
            &HashSet::from(["task-1-impl".to_string()]),
            &BTreeMap::from([(1, false), (2, false)]),
        );
        index
            .latest_plan_review
            .as_mut()
            .unwrap()
            .recovery_sources
            .clear();
        assert_eq!(
            index.validate_protected_minimum(),
            Err(WorkflowIndexProtectedError::MissingOpenFindingRecoveryPointer)
        );
    }

    #[test]
    fn terminal_predicate_and_step_two_cover_all_three_statuses() {
        let mut index = projected_status_fixture([
            ("completed-node", "completed"),
            ("failed-node", "failed"),
            ("canceled-node", "canceled"),
            ("running-node", "running"),
        ]);
        index.apply_omission_step(WorkflowIndexOmissionStep::TerminalNodeEvidence);
        let ids = index
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<Vec<_>>();
        assert!(!ids.contains(&"completed-node"));
        assert!(!ids.contains(&"failed-node"));
        assert!(!ids.contains(&"canceled-node"));
        assert!(ids.contains(&"running-node"));
    }
}
