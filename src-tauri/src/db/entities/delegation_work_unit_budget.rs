use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_work_unit_budgets")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub parent_conversation_id: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub work_unit_key: String,
    pub unexpected_continue_count: i64,
    pub replacement_count: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
