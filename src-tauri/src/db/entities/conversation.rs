use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum ConversationStatus {
    #[sea_orm(string_value = "in_progress")]
    InProgress,
    #[sea_orm(string_value = "pending_review")]
    PendingReview,
    #[sea_orm(string_value = "completed")]
    Completed,
    #[sea_orm(string_value = "cancelled")]
    Cancelled,
}

/// What kind of row this conversation is — drives sidebar visibility and
/// grouping. `regular` renders under its folder group; `chat` renders in the
/// flat "Chat" section; `loop` belongs to the Loop Engineering workbench and is
/// excluded from the sidebar list entirely (no write path yet — reserved for
/// the loop engine); `delegate` is a delegation child nested under its
/// parent's tool-call view. Invariant: `kind == Delegate` ⟺ `parent_id IS NOT
/// NULL`. Written once at insert, never updated.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum ConversationKind {
    #[sea_orm(string_value = "regular")]
    Regular,
    #[sea_orm(string_value = "chat")]
    Chat,
    #[sea_orm(string_value = "loop")]
    Loop,
    #[sea_orm(string_value = "delegate")]
    Delegate,
}

/// Durable delegation task lifecycle for Broker-backed child rows.
/// `unknown` and `stalled` are runtime-only views — never persisted.
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::None)")]
#[serde(rename_all = "snake_case")]
pub enum DelegationTaskStatus {
    #[sea_orm(string_value = "running")]
    Running,
    #[sea_orm(string_value = "completed")]
    Completed,
    #[sea_orm(string_value = "failed")]
    Failed,
    #[sea_orm(string_value = "canceled")]
    Canceled,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "conversation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub folder_id: i32,
    pub title: Option<String>,
    /// True once the user manually renamed this conversation. Gates the
    /// per-turn auto-title backfill (see `get_folder_conversation_core`) so a
    /// hand-set title is never overwritten by a parsed session-file title.
    pub title_locked: bool,
    pub agent_type: String,
    pub status: ConversationStatus,
    pub kind: ConversationKind,
    pub model: Option<String>,
    pub git_branch: Option<String>,
    pub external_id: Option<String>,
    pub parent_id: Option<i32>,
    pub parent_tool_use_id: Option<String>,
    pub delegation_call_id: Option<String>,
    /// Root-only session route override (`codeg` / `native`). Stored as raw
    /// text; typed as [`crate::acp::delegation::route::DelegationRoutePolicy`]
    /// on the summary. Broker children keep this null (origin forces Codeg).
    pub delegation_route_override: Option<String>,
    /// Durable task lifecycle for delegate rows. Independent of
    /// [`ConversationStatus`] so cold-load never re-infers task truth.
    pub delegation_task_status: Option<DelegationTaskStatus>,
    pub delegation_error_code: Option<String>,
    pub delegation_started_at: Option<DateTimeUtc>,
    pub delegation_finished_at: Option<DateTimeUtc>,
    pub message_count: i32,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub deleted_at: Option<DateTimeUtc>,
    /// When the user pinned this conversation; `None` means not pinned. Drives
    /// the sidebar's "Pinned" section (sorted by this timestamp descending).
    /// Pinning never bumps `updated_at` — it is a view preference, not activity.
    pub pinned_at: Option<DateTimeUtc>,
    /// Opaque generation token for race-safe awaiting-reply CAS. `None` means
    /// not currently awaiting a reply generation (historical/default).
    pub awaiting_reply_token: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::folder::Entity",
        from = "Column::FolderId",
        to = "super::folder::Column::Id"
    )]
    Folder,
}

impl Related<super::folder::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Folder.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
