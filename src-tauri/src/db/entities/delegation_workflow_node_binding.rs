use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Optional terminal outcome stored on a node binding (v1: canceled only).
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum NodeOutcome {
    #[sea_orm(string_value = "canceled")]
    Canceled,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflow_node_bindings")]
pub struct Model {
    /// Composite PK with `node_id` (matches unique index idx_dwnb_workflow_node).
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub node_id: String,
    pub work_unit_key: String,
    pub role: String,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub phase_id: String,
    pub task_index: Option<i64>,
    pub introduced_revision: i64,
    pub retired_revision: Option<i64>,
    pub is_observed: bool,
    pub retained_observed: bool,
    pub pair_frozen: bool,
    pub node_outcome: Option<NodeOutcome>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
