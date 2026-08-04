use sea_orm::entity::prelude::*;

/// Platform-owned Design-root CAS subject. Its IDs are not delegation runs.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflow_design_root_bindings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub gate_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub gate_lineage: String,
    pub node_id: String,
    #[sea_orm(unique)]
    pub task_id: String,
    #[sea_orm(unique)]
    pub latest_run_id: String,
    pub design_identity: String,
    pub evidence_scope_digest: String,
    pub graph_revision: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
