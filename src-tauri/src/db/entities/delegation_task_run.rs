use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Durable per-run lifecycle status for `delegation_task_runs`.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum DelegationRunStatus {
    #[sea_orm(string_value = "reserving")]
    Reserving,
    #[sea_orm(string_value = "running")]
    Running,
    #[sea_orm(string_value = "completed")]
    Completed,
    #[sea_orm(string_value = "failed")]
    Failed,
    #[sea_orm(string_value = "canceled")]
    Canceled,
}

/// Set at reserving insert; authoritative for which recovery counter (if any)
/// is charged at prompt-admission success.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum AdmissionClass {
    #[sea_orm(string_value = "normal_revision")]
    NormalRevision,
    #[sea_orm(string_value = "unexpected_continue")]
    UnexpectedContinue,
    #[sea_orm(string_value = "replacement")]
    Replacement,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum CompletionState {
    #[sea_orm(string_value = "resolved")]
    Resolved,
    #[sea_orm(string_value = "needs_decision")]
    NeedsDecision,
    #[sea_orm(string_value = "artifact_recovery")]
    ArtifactRecovery,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_task_runs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub task_id: String,
    pub root_task_id: String,
    pub previous_task_id: Option<String>,
    pub generation: i64,
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: Option<String>,
    pub child_conversation_id: i32,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub workspace_path: Option<String>,
    pub route_fingerprint: Option<String>,
    pub launch_snapshot_version: Option<String>,
    pub mode_id: Option<String>,
    pub config_values_json: Option<String>,
    pub task_preview: Option<String>,
    pub request_fingerprint: Option<String>,
    pub admission_class: AdmissionClass,
    pub reached_running_at: Option<DateTimeUtc>,
    pub lineage_root_task_id: String,
    pub work_unit_key: Option<String>,
    pub legacy_parent_tool_use_id: Option<String>,
    pub history_only: bool,
    pub status: DelegationRunStatus,
    pub error_code: Option<String>,
    pub termination_audit_json: Option<String>,
    pub started_at: Option<DateTimeUtc>,
    pub finished_at: Option<DateTimeUtc>,
    pub tool_call_count: Option<i64>,
    pub edit_tool_call_count: Option<i64>,
    pub touched_files_json: Option<String>,
    pub touched_files_truncated: Option<bool>,
    pub additions: Option<i64>,
    pub deletions: Option<i64>,
    pub line_counts_complete: Option<bool>,
    pub card_summary_json: Option<String>,
    pub child_turn_anchor: Option<String>,
    pub child_connection_id: Option<String>,
    pub replaced_task_id: Option<String>,
    pub replacement_reason: Option<String>,
    pub recovery_authorization_id: Option<String>,
    pub completion_state: Option<CompletionState>,
    pub completion_outcome: Option<String>,
    pub completion_evidence_json: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
