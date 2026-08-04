use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Immutable document-gate cycle outcome.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum GateSettlementOutcome {
    #[sea_orm(string_value = "approved")]
    Approved,
    #[sea_orm(string_value = "changes_requested")]
    ChangesRequested,
    #[sea_orm(string_value = "blocked")]
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewScope {
    #[sea_orm(string_value = "full")]
    Full,
    #[sea_orm(string_value = "scoped")]
    Scoped,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum PlanRevisionKind {
    #[sea_orm(string_value = "initial")]
    Initial,
    #[sea_orm(string_value = "localized")]
    Localized,
    #[sea_orm(string_value = "material")]
    Material,
    #[sea_orm(string_value = "holistic_rewrite")]
    HolisticRewrite,
}

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewNextAction {
    #[sea_orm(string_value = "continue_review")]
    ContinueReview,
    #[sea_orm(string_value = "holistic_rewrite_required")]
    HolisticRewriteRequired,
    #[sea_orm(string_value = "user_decision_required")]
    UserDecisionRequired,
    #[sea_orm(string_value = "approved")]
    Approved,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflow_gate_settlements")]
pub struct Model {
    /// Composite PK with gate_id + gate_cycle (matches idx_dwgs_gate_cycle).
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub gate_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub gate_cycle: i64,
    pub manifest_revision: i64,
    /// Header structural_revision at settle time (plan clock audit).
    pub structural_revision: i64,
    /// Design or plan fingerprint covered by this settlement (gate-kind-specific).
    pub content_fingerprint: String,
    pub evidence_scope_digest: Option<String>,
    pub gate_lineage: Option<String>,
    pub review_round: Option<i64>,
    #[sea_orm(column_type = "Text", nullable)]
    pub required_node_set_json: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub required_evidence_task_ids_json: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub evidence_scope_digests_json: Option<String>,
    pub localized_change_digest: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub plan_round_state_v2_json: Option<String>,
    pub outcome: GateSettlementOutcome,
    pub critical_count: Option<i64>,
    pub important_count: Option<i64>,
    pub minor_count: Option<i64>,
    #[sea_orm(column_type = "Text")]
    pub summary: String,
    pub graph_revision_at_settle: i64,
    pub review_scope: Option<PlanReviewScope>,
    pub revision_kind: Option<PlanRevisionKind>,
    #[sea_orm(column_type = "Text", nullable)]
    pub scope_reason: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub required_reviewer_node_ids_json: Option<String>,
    pub covered_author_task_id: Option<String>,
    pub covered_plan_digest: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub finding_ledger_json: Option<String>,
    pub net_improvement: Option<bool>,
    pub stagnation_count: i64,
    pub rewrite_used: bool,
    pub next_action: Option<PlanReviewNextAction>,
    #[sea_orm(column_type = "Text", nullable)]
    pub report_files_json: Option<String>,
    pub lineage_reset_authorization_id: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
