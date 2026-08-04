use sea_orm::entity::prelude::*;

/// Sole mutable owner of the current lineage and review round for a gate.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_workflow_gate_states")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub gate_id: String,
    pub gate_lineage: String,
    pub current_review_round: i64,
    #[sea_orm(column_type = "Text")]
    pub selected_node_ids_json: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
