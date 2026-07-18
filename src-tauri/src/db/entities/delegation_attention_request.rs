use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_attention_requests")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub request_id: String,
    pub task_id: String,
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub child_tool_call_id: String,
    pub status: String,
    pub message: String,
    pub reply: Option<String>,
    pub resolution_code: Option<String>,
    pub created_at: DateTimeUtc,
    pub resolved_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
