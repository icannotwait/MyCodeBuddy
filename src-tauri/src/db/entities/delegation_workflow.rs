use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Header lifecycle / publication state for `delegation_workflows`.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum WorkflowState {
    #[sea_orm(string_value = "skeleton")]
    Skeleton,
    #[sea_orm(string_value = "estimated")]
    Estimated,
    #[sea_orm(string_value = "approved")]
    Approved,
    #[sea_orm(string_value = "blocked")]
    Blocked,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflows")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: String,
    pub parent_conversation_id: i32,
    pub workflow_kind: String,
    pub schema_version: i64,
    pub active_manifest_revision: i64,
    pub graph_revision: i64,
    pub workflow_state: WorkflowState,
    pub capability_version: String,
    pub publication_token: String,
    pub supersedes_approved_revision: Option<i64>,
    /// Plan-content identity clock. State-only CAS bumps keep this unchanged;
    /// material Plan structure changes set it to the new manifest revision.
    pub structural_revision: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
