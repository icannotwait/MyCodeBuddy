use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_completion_tool_intents")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub intent_id: String,
    pub task_id: String,
    pub child_tool_call_id: String,
    pub accepted_ordinal: i64,
    pub outcome: String,
    pub summary: Option<String>,
    pub report_hint: Option<String>,
    pub request_digest: String,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
