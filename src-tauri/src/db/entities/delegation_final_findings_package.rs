use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum FinalFindingsPackageStatus {
    #[sea_orm(string_value = "active")]
    Active,
    #[sea_orm(string_value = "superseded")]
    Superseded,
    #[sea_orm(string_value = "resolved")]
    Resolved,
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_final_findings_packages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub package_id: String,
    pub workflow_id: String,
    pub gate_id: String,
    pub gate_lineage: String,
    pub source_evaluation_key: String,
    #[sea_orm(column_type = "Text")]
    pub source_evidence_task_ids_json: String,
    #[sea_orm(column_type = "Text")]
    pub items_json: String,
    #[sea_orm(column_type = "Text")]
    pub remediation_contexts_json: String,
    pub package_digest: String,
    pub status: FinalFindingsPackageStatus,
    pub created_graph_revision: i64,
    pub resolved_graph_revision: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
