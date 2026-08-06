use sea_orm::entity::prelude::*;

/// Immutable classifier proof authorizing the active corrective Plan round.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_plan_round_authorizations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub gate_id: String,
    pub gate_lineage: String,
    pub review_round: i64,
    pub author_task_id: String,
    #[sea_orm(column_type = "Text")]
    pub authorization_json: String,
    pub authorization_digest: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
