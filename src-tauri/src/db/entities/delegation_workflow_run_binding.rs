use sea_orm::entity::prelude::*;

/// Per-run association to a workflow node / optional document-gate cycle.
///
/// Authority for Task/Final execution-gate artifact coverage lives here
/// (reviewed_task_id, reviewed_implementer_generation, artifact_digest),
/// not in free-text card summaries (Contract Amendment B3).
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflow_run_bindings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub task_id: String,
    pub workflow_id: String,
    pub node_id: String,
    pub gate_id: Option<String>,
    pub gate_cycle: Option<i64>,
    pub manifest_revision: i64,
    /// Design/plan content fingerprint at document-gate admission; NULL for Task/Final.
    pub content_fingerprint: Option<String>,
    pub evidence_scope_digest: Option<String>,
    pub gate_lineage: Option<String>,
    pub review_round: Option<i64>,
    pub instruction_block_digest: Option<String>,
    pub material_selector_digest: Option<String>,
    pub subject_material_digest: Option<String>,
    pub requirements_identity: Option<String>,
    pub task_specification_identity: Option<String>,
    pub final_findings_identity: Option<String>,
    pub producer_baseline_head: Option<String>,
    pub artifact_digest: Option<String>,
    pub reviewed_task_id: Option<String>,
    pub reviewed_implementer_generation: Option<i64>,
    pub lineage_ordinal: i64,
    pub summary_validated: bool,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
