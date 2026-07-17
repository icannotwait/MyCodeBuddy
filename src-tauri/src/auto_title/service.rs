//! Enrollment, job cancellation, and generated-title finalization.

use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set, TransactionTrait,
};

use crate::auto_title::types::{AutoTitleClaim, FinalizeTitleOutcome};
use crate::commands::conversation_experience::load_auto_title_agent_from;
use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
use crate::db::entities::conversation;
use crate::db::error::DbError;

/// Enroll a newly created conversation for automatic titles when the setting
/// is On. Reads the agent through [`load_auto_title_agent_from`] so the Off
/// sentinel (`""`) is not treated as enabled. Returns `true` when a job row
/// was inserted.
pub async fn enroll_new_conversation<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
    now: DateTime<Utc>,
) -> Result<bool, DbError> {
    let Some(_agent) = load_auto_title_agent_from(conn).await? else {
        return Ok(false);
    };

    auto_title_job::ActiveModel {
        conversation_id: Set(conversation_id),
        state: Set(AutoTitleJobState::AwaitingTurn),
        attempts: Set(0),
        first_user_text: Set(None),
        first_assistant_text: Set(None),
        locale: Set(None),
        usable_turn_seq: Set(0),
        attempt_turn_seq: Set(0),
        last_usable_turn_token: Set(None),
        updated_at: Set(now),
    }
    .insert(conn)
    .await?;

    Ok(true)
}

/// Delete the auto-title job for `conversation_id` if present. Returns `true`
/// when a row was removed (callers must cancel in-flight work after commit).
pub async fn cancel_job<C: ConnectionTrait>(
    conn: &C,
    conversation_id: i32,
) -> Result<bool, DbError> {
    let result = auto_title_job::Entity::delete_by_id(conversation_id)
        .exec(conn)
        .await?;
    Ok(result.rows_affected > 0)
}

/// Atomically commit a generated title for the exact running claim, or cancel
/// when the conversation is locked/finalized/deleted or the job no longer
/// matches. Never bumps `updated_at`.
pub async fn finalize_generated_title(
    conn: &DatabaseConnection,
    claim: &AutoTitleClaim,
    title: &str,
) -> Result<FinalizeTitleOutcome, DbError> {
    let txn = conn.begin().await?;

    let job = auto_title_job::Entity::find_by_id(claim.conversation_id)
        .filter(auto_title_job::Column::State.eq(AutoTitleJobState::Running))
        .filter(auto_title_job::Column::Attempts.eq(claim.attempt))
        .filter(auto_title_job::Column::AttemptTurnSeq.eq(claim.attempt_turn_seq))
        .one(&txn)
        .await?;

    if job.is_none() {
        txn.commit().await?;
        return Ok(FinalizeTitleOutcome::Cancelled);
    }

    let updated = conversation::Entity::update_many()
        .col_expr(conversation::Column::Title, Expr::value(title))
        .col_expr(conversation::Column::AutoTitleFinalized, Expr::value(true))
        .filter(conversation::Column::Id.eq(claim.conversation_id))
        .filter(conversation::Column::DeletedAt.is_null())
        .filter(conversation::Column::TitleLocked.eq(false))
        .filter(conversation::Column::AutoTitleFinalized.eq(false))
        .exec(&txn)
        .await?;

    if updated.rows_affected != 1 {
        txn.rollback().await?;
        return Ok(FinalizeTitleOutcome::Cancelled);
    }

    auto_title_job::Entity::delete_by_id(claim.conversation_id)
        .exec(&txn)
        .await?;

    txn.commit().await?;
    Ok(FinalizeTitleOutcome::Committed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set};

    use crate::acp::delegation::spawner::DelegationLink;
    use crate::auto_title::types::{AutoTitleClaim, FinalizeTitleOutcome};
    use crate::commands::conversation_experience::{
        set_auto_title_agent_persisted_core, KEY_AUTO_TITLE_AGENT,
    };
    use crate::db::entities::auto_title_job::{self, AutoTitleJobState};
    use crate::db::entities::conversation;
    use crate::db::service::app_metadata_service;
    use crate::db::service::conversation_service::{
        create, create_chat, create_with_delegation, update_title,
    };
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::agent::AgentType;
    use crate::models::system::AppLocale;

    async fn seed_running_job(conn: &DatabaseConnection, conversation_id: i32, attempt: i32) {
        let now = Utc::now();
        auto_title_job::ActiveModel {
            conversation_id: Set(conversation_id),
            state: Set(AutoTitleJobState::Running),
            attempts: Set(attempt),
            first_user_text: Set(Some("task".into())),
            first_assistant_text: Set(Some("answer".into())),
            locale: Set(Some("en".into())),
            usable_turn_seq: Set(1),
            attempt_turn_seq: Set(1),
            last_usable_turn_token: Set(Some("turn-1".into())),
            updated_at: Set(now),
        }
        .insert(conn)
        .await
        .expect("seed running job");
    }

    async fn enable_auto_title(conn: &DatabaseConnection, agent: AgentType) {
        app_metadata_service::upsert_value(
            conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&agent).expect("serialize agent"),
        )
        .await
        .expect("enable auto title agent");
    }

    #[tokio::test]
    async fn enabled_creation_enrolls_root_and_delegate() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        crate::db::service::app_metadata_service::upsert_value(
            &db.conn,
            KEY_AUTO_TITLE_AGENT,
            &serde_json::to_string(&AgentType::Codex).unwrap(),
        )
        .await
        .unwrap();
        let folder = crate::db::test_helpers::seed_folder(&db, "/tmp/title-enrollment").await;
        let root = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("root");
        let child = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Gemini,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: root.id,
                parent_tool_use_id: "tool-1".into(),
                delegation_call_id: "call-1".into(),
            }),
        )
        .await
        .expect("child");

        assert!(auto_title_job::Entity::find_by_id(root.id)
            .one(&db.conn)
            .await
            .unwrap()
            .is_some());
        assert!(auto_title_job::Entity::find_by_id(child.id)
            .one(&db.conn)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn manual_rename_and_generated_commit_have_atomic_precedence() {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder = crate::db::test_helpers::seed_folder(&db, "/tmp/title-precedence").await;
        let conversation = create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            None,
            None,
        )
        .await
        .unwrap();
        seed_running_job(&db.conn, conversation.id, 1).await;
        assert!(update_title(&db.conn, conversation.id, "Manual".into())
            .await
            .expect("rename"));
        let claim = AutoTitleClaim {
            conversation_id: conversation.id,
            attempt: 1,
            agent: AgentType::Codex,
            first_user_text: "task".into(),
            first_assistant_text: "answer".into(),
            locale: AppLocale::En,
            attempt_turn_seq: 1,
        };
        let outcome = finalize_generated_title(&db.conn, &claim, "Generated")
            .await
            .expect("late result");
        assert_eq!(outcome, FinalizeTitleOutcome::Cancelled);
        let saved = conversation::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.title.as_deref(), Some("Manual"));
    }

    #[tokio::test]
    async fn create_create_chat_and_delegate_each_enroll_exactly_one_job_when_enabled() {
        let db = fresh_in_memory_db().await;
        enable_auto_title(&db.conn, AgentType::Codex).await;
        let folder = seed_folder(&db, "/tmp/title-create-paths").await;

        let regular = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        let chat = create_chat(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create_chat");
        let child = create_with_delegation(
            &db.conn,
            folder,
            AgentType::Gemini,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: regular.id,
                parent_tool_use_id: "tu-enroll".into(),
                delegation_call_id: "call-enroll".into(),
            }),
        )
        .await
        .expect("delegate");

        for id in [regular.id, chat.id, child.id] {
            let jobs = auto_title_job::Entity::find_by_id(id)
                .all(&db.conn)
                .await
                .expect("jobs");
            assert_eq!(jobs.len(), 1, "conversation {id} must have exactly one job");
            assert_eq!(jobs[0].state, AutoTitleJobState::AwaitingTurn);
        }

        let total = auto_title_job::Entity::find()
            .all(&db.conn)
            .await
            .expect("all jobs");
        assert_eq!(total.len(), 3);
    }

    #[tokio::test]
    async fn off_sentinel_does_not_enroll_even_when_metadata_row_exists() {
        let db = fresh_in_memory_db().await;
        app_metadata_service::upsert_value(&db.conn, KEY_AUTO_TITLE_AGENT, "")
            .await
            .expect("off sentinel");
        let folder = seed_folder(&db, "/tmp/title-off-sentinel").await;
        let row = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        assert!(
            auto_title_job::Entity::find_by_id(row.id)
                .one(&db.conn)
                .await
                .expect("query")
                .is_none(),
            "empty Off sentinel must not enroll"
        );
    }

    #[tokio::test]
    async fn creation_racing_disable_leaves_no_job_when_off() {
        let temp = tempfile::TempDir::new().expect("tempdir");
        let db = crate::db::init_database(temp.path(), "auto-title-create-disable-race")
            .await
            .expect("open pooled WAL database");

        enable_auto_title(&db.conn, AgentType::ClaudeCode).await;
        let folder = seed_folder(&db, "/tmp/title-create-disable-race").await;

        let (create_result, off_result) = tokio::join!(
            create(&db.conn, folder, AgentType::ClaudeCode, None, None),
            set_auto_title_agent_persisted_core(&db, None),
        );

        create_result.expect("create completed");
        let off = off_result.expect("disable completed");
        assert_eq!(off.auto_title_agent, None);
        assert!(
            auto_title_job::Entity::find()
                .all(&db.conn)
                .await
                .expect("jobs")
                .is_empty(),
            "final state must be Off with zero jobs regardless of transaction order"
        );

        drop(temp);
    }

    #[tokio::test]
    async fn finalize_commits_when_running_claim_matches_and_unlocked() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/title-finalize-ok").await;
        let conversation = create(&db.conn, folder, AgentType::ClaudeCode, None, None)
            .await
            .expect("create");
        seed_running_job(&db.conn, conversation.id, 1).await;
        let before = conversation::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();

        let claim = AutoTitleClaim {
            conversation_id: conversation.id,
            attempt: 1,
            agent: AgentType::Codex,
            first_user_text: "task".into(),
            first_assistant_text: "answer".into(),
            locale: AppLocale::En,
            attempt_turn_seq: 1,
        };
        let outcome = finalize_generated_title(&db.conn, &claim, "Generated")
            .await
            .expect("finalize");
        assert_eq!(outcome, FinalizeTitleOutcome::Committed);

        let saved = conversation::Entity::find_by_id(conversation.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.title.as_deref(), Some("Generated"));
        assert!(saved.auto_title_finalized);
        assert!(!saved.title_locked);
        assert_eq!(saved.updated_at, before.updated_at);
        assert!(
            auto_title_job::Entity::find_by_id(conversation.id)
                .one(&db.conn)
                .await
                .unwrap()
                .is_none()
        );
    }
}
