use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    #[sea_orm(string_value = "child_question")]
    ChildQuestion,
    #[sea_orm(string_value = "completion_decision")]
    CompletionDecision,
    #[sea_orm(string_value = "completion_artifact_recovery")]
    CompletionArtifactRecovery,
    #[sea_orm(string_value = "design_self_review_decision")]
    DesignSelfReviewDecision,
}

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "delegation_attention_requests")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub request_id: String,
    pub task_id: String,
    pub parent_conversation_id: i32,
    pub child_conversation_id: Option<i32>,
    pub child_tool_call_id: Option<String>,
    pub status: String,
    pub message: String,
    pub reply: Option<String>,
    pub resolution_code: Option<String>,
    pub created_at: DateTimeUtc,
    pub resolved_at: Option<DateTimeUtc>,
    pub kind: AttentionKind,
    pub latest_run_id: Option<String>,
    pub node_id: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub payload_json: Option<String>,
    #[sea_orm(column_type = "Text", nullable)]
    pub resolution_json: Option<String>,
    pub captured_scope_digest: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
