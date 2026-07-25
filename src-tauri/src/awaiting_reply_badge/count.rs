//! Pure COUNT of conversations in durable awaiting-reply state.
//!
//! Predicate (aligned with sidebar red-dot eligibility):
//! `deleted_at IS NULL` AND `status = pending_review` AND `awaiting_reply_token IS NOT NULL`.

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter};

use crate::db::entities::conversation;
use crate::db::error::DbError;

/// Count local conversations that need a user reply (taskbar badge number).
pub async fn count_awaiting_reply(conn: &DatabaseConnection) -> Result<u32, DbError> {
    let n = conversation::Entity::find()
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::Status.eq(conversation::ConversationStatus::PendingReview))
        .filter(conversation::Column::AwaitingReplyToken.is_not_null())
        .count(conn)
        .await?;
    Ok(n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};

    use crate::db::entities::conversation::{self, ConversationStatus};
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::agent::AgentType;

    async fn seed_conv(
        db: &crate::db::AppDatabase,
        folder_id: i32,
    ) -> conversation::Model {
        conversation_service::create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
            .await
            .expect("create conversation")
    }

    async fn patch_conv(
        conn: &DatabaseConnection,
        id: i32,
        status: ConversationStatus,
        token: Option<&str>,
        soft_delete: bool,
    ) {
        let conv = conversation::Entity::find_by_id(id)
            .one(conn)
            .await
            .expect("find")
            .expect("exists");
        let mut active: conversation::ActiveModel = conv.into();
        active.status = Set(status);
        active.awaiting_reply_token = Set(token.map(str::to_string));
        if soft_delete {
            active.deleted_at = Set(Some(Utc::now()));
        }
        active.update(conn).await.expect("update");
    }

    #[tokio::test]
    async fn count_awaiting_reply_empty_is_zero() {
        let db = fresh_in_memory_db().await;
        let n = count_awaiting_reply(&db.conn).await.expect("count");
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn count_awaiting_reply_one_eligible() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/badge-count-one").await;
        let c = seed_conv(&db, folder).await;
        patch_conv(
            &db.conn,
            c.id,
            ConversationStatus::PendingReview,
            Some("tok-1"),
            false,
        )
        .await;

        let n = count_awaiting_reply(&db.conn).await.expect("count");
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn count_awaiting_reply_two_eligible() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/badge-count-two").await;
        for tok in ["tok-a", "tok-b"] {
            let c = seed_conv(&db, folder).await;
            patch_conv(
                &db.conn,
                c.id,
                ConversationStatus::PendingReview,
                Some(tok),
                false,
            )
            .await;
        }

        let n = count_awaiting_reply(&db.conn).await.expect("count");
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn count_awaiting_reply_excludes_ineligible_rows() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/badge-count-exclude").await;

        // Cleared token (pending_review but token null) — not counted.
        let cleared = seed_conv(&db, folder).await;
        patch_conv(
            &db.conn,
            cleared.id,
            ConversationStatus::PendingReview,
            None,
            false,
        )
        .await;

        // In progress with a stale token — not counted.
        let in_progress = seed_conv(&db, folder).await;
        patch_conv(
            &db.conn,
            in_progress.id,
            ConversationStatus::InProgress,
            Some("stale"),
            false,
        )
        .await;

        // Soft-deleted eligible shape — not counted.
        let deleted = seed_conv(&db, folder).await;
        patch_conv(
            &db.conn,
            deleted.id,
            ConversationStatus::PendingReview,
            Some("gone"),
            true,
        )
        .await;

        // Tokenless child (pending_review, no token) — not counted.
        let parent = seed_conv(&db, folder).await;
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(crate::acp::delegation::spawner::DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: "tu-1".into(),
                delegation_call_id: "call-1".into(),
            }),
        )
        .await
        .expect("child");
        patch_conv(
            &db.conn,
            child.id,
            ConversationStatus::PendingReview,
            None,
            false,
        )
        .await;

        let n = count_awaiting_reply(&db.conn).await.expect("count");
        assert_eq!(n, 0, "no ineligible row may contribute to the badge count");
    }
}
