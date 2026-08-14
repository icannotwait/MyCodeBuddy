use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// What the last token-usage sync saw for one conversation.
///
/// `source_updated_at` mirrors `conversation.updated_at` as of that sync. A
/// conversation is stale when the two differ — exact inequality, not `>`, so a
/// sync that lands in the same millisecond as a turn is not silently skipped
/// and a row rewritten backwards (restore, clock step) still re-parses.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "token_usage_sync")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub conversation_id: i32,
    pub source_updated_at: DateTimeUtc,
    /// Fact rows written for this conversation by that sync.
    pub turn_count: i32,
    pub total_tokens: i64,
    pub synced_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::conversation::Entity",
        from = "Column::ConversationId",
        to = "super::conversation::Column::Id"
    )]
    Conversation,
}

impl Related<super::conversation::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Conversation.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
