use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

/// One usage-bearing turn, materialized for the token dashboard.
///
/// Written only by the sync pass in `commands::token_usage`, which replaces a
/// conversation's whole row set inside one transaction — so a row is never
/// updated in place and never partially refreshed.
///
/// `folder_id` / `agent_type` are intentionally absent: the dashboard reaches
/// them by joining `conversation`, so folder moves, agent changes and soft
/// deletes take effect without a re-sync. See the migration for the rationale.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "token_usage_turn")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub conversation_id: i32,
    /// The parser's `MessageTurn::id` — provenance for debugging, not a key.
    pub turn_key: String,
    /// When the spend happened: the turn's `completed_at` when the parser knows
    /// it, otherwise its `timestamp`.
    pub occurred_at: DateTimeUtc,
    pub model: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    /// Sum of the four counters above.
    pub total_tokens: i64,
    pub duration_ms: i64,
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
