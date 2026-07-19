use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// Durable automatic-title job states. Terminal completion is represented by
/// deleting the job row and setting `conversation.auto_title_finalized`; there
/// is no `done` state.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum AutoTitleJobState {
    #[sea_orm(string_value = "awaiting_turn")]
    AwaitingTurn,
    #[sea_orm(string_value = "ready")]
    Ready,
    #[sea_orm(string_value = "running")]
    Running,
    #[sea_orm(string_value = "retry_wait")]
    RetryWait,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "auto_title_jobs")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub conversation_id: i32,
    pub state: AutoTitleJobState,
    pub attempts: i32,
    #[sea_orm(column_type = "Text", nullable)]
    pub first_user_text: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub first_assistant_text: Option<String>,
    /// Instant when `first_user_text` was first written; deadline origin.
    /// NULL for pre-migration rows that already had a captured prompt (end-turn only).
    pub first_prompt_at: Option<DateTimeUtc>,
    pub locale: Option<String>,
    pub usable_turn_seq: i32,
    pub attempt_turn_seq: i32,
    pub last_usable_turn_token: Option<String>,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::conversation::Entity",
        from = "Column::ConversationId",
        to = "super::conversation::Column::Id",
        on_delete = "Cascade"
    )]
    Conversation,
}

impl Related<super::conversation::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Conversation.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
