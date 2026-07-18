use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_continuations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub continuation_id: String,
    pub parent_conversation_id: i32,
    pub parent_session_id: String,
    pub parent_connection_id: Option<String>,
    pub generation: i64,
    pub parent_turn_generation: i64,
    pub task_ids_json: String,
    pub state: String,
    pub wake_reason: Option<String>,
    pub armed_at: DateTimeUtc,
    pub wake_at: DateTimeUtc,
    pub suspend_requested_at: Option<DateTimeUtc>,
    pub suspended_at: Option<DateTimeUtc>,
    pub wake_claimed_at: Option<DateTimeUtc>,
    pub prompt_admitted_at: Option<DateTimeUtc>,
    pub finished_at: Option<DateTimeUtc>,
    pub internal_prompt_id: String,
    pub internal_prompt_marker: String,
    pub failure_code: Option<String>,
    pub version: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
