use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_lineage_budgets")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub lineage_root_task_id: String,
    pub unexpected_continue_count: i64,
    pub replacement_count: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
