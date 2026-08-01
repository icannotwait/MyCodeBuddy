use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAuthorizationStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "approved")]
    Approved,
    #[sea_orm(string_value = "declined")]
    Declined,
    #[sea_orm(string_value = "consumed")]
    Consumed,
    #[sea_orm(string_value = "expired")]
    Expired,
    #[sea_orm(string_value = "abandoned")]
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "recovery_authorizations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub authorization_id: String,
    pub parent_conversation_id: i32,
    pub subject_kind: String,
    pub subject_id: String,
    pub source_task_id: Option<String>,
    pub child_conversation_id: Option<i32>,
    pub lineage_root_task_id: Option<String>,
    pub work_unit_key: Option<String>,
    pub source_state_fingerprint: String,
    pub allowed_action: String,
    #[sea_orm(column_type = "Text")]
    pub action_payload_json: String,
    pub cause_code: String,
    pub risk_class: String,
    #[sea_orm(column_type = "Text", nullable)]
    pub display_reason: Option<String>,
    pub status: RecoveryAuthorizationStatus,
    pub question_id: Option<String>,
    pub requested_at: DateTimeUtc,
    pub approved_at: Option<DateTimeUtc>,
    pub expires_at: Option<DateTimeUtc>,
    pub consumed_at: Option<DateTimeUtc>,
    pub consumed_by_kind: Option<String>,
    pub consumed_by_id: Option<String>,
    pub consumer_correlation_id: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::conversation::Entity",
        from = "Column::ParentConversationId",
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
