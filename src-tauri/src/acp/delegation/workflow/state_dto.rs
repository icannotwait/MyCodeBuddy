//! Agent-facing workflow recovery DTO (`get_workflow_state`).
//!
//! May include `work_unit_key`. Must **not** be reused as the redacted
//! frontend `WorkflowGraphSnapshot` (Task 4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::plan_review::PlanReviewRoundState;
use super::types::{DocumentRef, ManifestTaskPolicy, ManifestWorkflowState};

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

#[cfg(test)]
mod tests {
    use sea_orm::Iterable;

    use super::*;
    use crate::db::entities::delegation_workflow_node_binding;

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
}
