#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{Duration, Utc};
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    use super::*;
    use crate::acp::delegation::continuation::types::{
        ContinuationFailureCode, ContinuationState, ContinuationTaskIds,
    };

    fn new_continuation(id: &str, conversation_id: i32) -> NewContinuation {
        let armed_at = Utc::now();
        NewContinuation {
            continuation_id: id.to_string(),
            parent_conversation_id: conversation_id,
            parent_session_id: "parent-session".to_string(),
            parent_connection_id: "parent-connection".to_string(),
            parent_turn_generation: 1,
            task_ids: ContinuationTaskIds(vec!["task-b".to_string(), "task-a".to_string()]),
            armed_at,
            wake_at: armed_at + Duration::minutes(4),
            internal_prompt_id: format!("prompt-{id}"),
            internal_prompt_marker: format!("marker-{id}"),
        }
    }

    async fn sqlite_store() -> (DbContinuationStore, crate::db::AppDatabase) {
        let db = crate::db::test_helpers::fresh_in_memory_db().await;
        let folder_id = crate::db::test_helpers::seed_folder(&db, "C:/repo").await;
        crate::db::test_helpers::seed_conversation(
            &db,
            folder_id,
            crate::models::agent::AgentType::Codex,
        )
        .await;
        (DbContinuationStore::new(db.conn.clone()), db)
    }

    async fn sqlite_parent_status(db: &crate::db::AppDatabase, conversation_id: i32) -> String {
        db.conn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT status FROM conversation WHERE id = ?",
                [conversation_id.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "status")
            .unwrap()
    }

    #[tokio::test]
    async fn continuation_cleanup_startup_fails_row_and_parent_atomically_with_store_parity() {
        let finished_at = Utc::now();
        let memory = InMemoryContinuationStore::default();
        memory.seed_parent_status(1, "in_progress").await;
        memory
            .insert_arming(new_continuation("memory-startup", 1))
            .await
            .unwrap();

        let memory_winners = memory
            .fail_non_terminal_on_startup(finished_at)
            .await
            .unwrap();
        assert_eq!(memory_winners.len(), 1);
        assert_eq!(memory_winners[0].state, ContinuationState::Failed);
        assert_eq!(
            memory_winners[0].failure_code,
            Some(ContinuationFailureCode::ParentConnectionLost)
        );
        assert_eq!(memory_winners[0].finished_at, Some(finished_at));
        assert_eq!(memory.parent_status(1).await.as_deref(), Some("cancelled"));
        assert!(memory
            .fail_non_terminal_on_startup(finished_at + Duration::seconds(1))
            .await
            .unwrap()
            .is_empty());

        let (sqlite, db) = sqlite_store().await;
        sqlite
            .insert_arming(new_continuation("sqlite-startup", 1))
            .await
            .unwrap();
        let sqlite_winners = sqlite
            .fail_non_terminal_on_startup(finished_at)
            .await
            .unwrap();
        assert_eq!(sqlite_winners.len(), 1);
        assert_eq!(sqlite_winners[0].state, ContinuationState::Failed);
        assert_eq!(
            sqlite_winners[0].failure_code,
            Some(ContinuationFailureCode::ParentConnectionLost)
        );
        assert_eq!(sqlite_winners[0].finished_at, Some(finished_at));
        assert_eq!(sqlite_parent_status(&db, 1).await, "cancelled");
        assert!(sqlite
            .fail_non_terminal_on_startup(finished_at + Duration::seconds(1))
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn continuation_cleanup_startup_cas_loser_reports_no_winner_with_store_parity() {
        let finished_at = Utc::now();
        let memory = Arc::new(InMemoryContinuationStore::default());
        memory.seed_parent_status(1, "in_progress").await;
        memory
            .insert_arming(new_continuation("memory-startup-race", 1))
            .await
            .unwrap();
        let (first, second) = tokio::join!(
            memory.fail_non_terminal_on_startup(finished_at),
            memory.fail_non_terminal_on_startup(finished_at)
        );
        assert_eq!(first.unwrap().len() + second.unwrap().len(), 1);
        assert_eq!(memory.parent_status(1).await.as_deref(), Some("cancelled"));

        let (sqlite, db) = sqlite_store().await;
        let sqlite = Arc::new(sqlite);
        sqlite
            .insert_arming(new_continuation("sqlite-startup-race", 1))
            .await
            .unwrap();
        let (first, second) = tokio::join!(
            sqlite.fail_non_terminal_on_startup(finished_at),
            sqlite.fail_non_terminal_on_startup(finished_at)
        );
        assert_eq!(first.unwrap().len() + second.unwrap().len(), 1);
        assert_eq!(sqlite_parent_status(&db, 1).await, "cancelled");
    }

    #[tokio::test]
    async fn continuation_cleanup_startup_parent_write_error_rolls_back_row() {
        let (sqlite, db) = sqlite_store().await;
        let record = sqlite
            .insert_arming(new_continuation("sqlite-startup-rollback", 1))
            .await
            .unwrap();
        db.conn
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                "CREATE TRIGGER fail_startup_parent_update BEFORE UPDATE OF status ON conversation BEGIN SELECT RAISE(ABORT, 'parent update failed'); END".to_string(),
            ))
            .await
            .unwrap();

        assert!(sqlite
            .fail_non_terminal_on_startup(Utc::now())
            .await
            .is_err());
        let unchanged = sqlite.load(&record.continuation_id).await.unwrap().unwrap();
        assert_eq!(unchanged.state, ContinuationState::Arming);
        assert_eq!(unchanged.version, record.version);
        assert_eq!(sqlite_parent_status(&db, 1).await, "in_progress");
    }

    #[tokio::test]
    async fn continuation_cleanup_startup_in_memory_error_rolls_back_row_and_parent() {
        let memory = InMemoryContinuationStore::default();
        memory.seed_parent_status(1, "in_progress").await;
        let record = memory
            .insert_arming(new_continuation("memory-startup-rollback", 1))
            .await
            .unwrap();
        memory.fail_next_startup_before_commit().await;

        assert!(memory
            .fail_non_terminal_on_startup(Utc::now())
            .await
            .is_err());
        let unchanged = memory.load(&record.continuation_id).await.unwrap().unwrap();
        assert_eq!(unchanged.state, ContinuationState::Arming);
        assert_eq!(unchanged.version, record.version);
        assert_eq!(
            memory.parent_status(1).await.as_deref(),
            Some("in_progress")
        );
    }

    #[tokio::test]
    async fn continuation_cleanup_latest_failure_requires_newest_failed_and_cancelled_parent() {
        let finished_at = Utc::now();
        let memory = InMemoryContinuationStore::default();
        memory.seed_parent_status(1, "in_progress").await;
        memory
            .insert_arming(new_continuation("memory-failed", 1))
            .await
            .unwrap();
        memory
            .fail_non_terminal_on_startup(finished_at)
            .await
            .unwrap();
        assert!(memory
            .load_latest_failure_for_conversation(1)
            .await
            .unwrap()
            .is_some());
        let newer = memory
            .insert_arming(new_continuation("memory-newer", 1))
            .await
            .unwrap();
        memory
            .cas_transition(
                &newer.continuation_id,
                newer.generation,
                newer.version,
                newer.state,
                transition(ContinuationState::Cancelled),
            )
            .await
            .unwrap();
        assert!(memory
            .load_latest_failure_for_conversation(1)
            .await
            .unwrap()
            .is_none());

        let (sqlite, db) = sqlite_store().await;
        sqlite
            .insert_arming(new_continuation("sqlite-failed", 1))
            .await
            .unwrap();
        sqlite
            .fail_non_terminal_on_startup(finished_at)
            .await
            .unwrap();
        assert!(sqlite
            .load_latest_failure_for_conversation(1)
            .await
            .unwrap()
            .is_some());
        db.conn
            .execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "UPDATE conversation SET status = 'in_progress' WHERE id = ?",
                [1.into()],
            ))
            .await
            .unwrap();
        assert!(sqlite
            .load_latest_failure_for_conversation(1)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn continuation_store_rejects_second_active_parent() {
        let memory = InMemoryContinuationStore::default();
        memory
            .insert_arming(new_continuation("memory-one", 1))
            .await
            .unwrap();
        assert!(matches!(
            memory
                .insert_arming(new_continuation("memory-two", 1))
                .await,
            Err(ContStoreError::ActiveExists)
        ));

        let (sqlite, db) = sqlite_store().await;
        sqlite
            .insert_arming(new_continuation("sqlite-one", 1))
            .await
            .unwrap();
        assert!(matches!(
            sqlite
                .insert_arming(new_continuation("sqlite-two", 1))
                .await,
            Err(ContStoreError::ActiveExists)
        ));
        drop(db);
    }

    #[tokio::test]
    async fn continuation_store_allocates_monotonic_generation() {
        let memory = InMemoryContinuationStore::default();
        let first = memory
            .insert_arming(new_continuation("memory-one", 1))
            .await
            .unwrap();
        memory
            .cas_transition(
                &first.continuation_id,
                first.generation,
                first.version,
                ContinuationState::Arming,
                transition(ContinuationState::Cancelled),
            )
            .await
            .unwrap();
        let second = memory
            .insert_arming(new_continuation("memory-two", 1))
            .await
            .unwrap();
        assert_eq!((first.generation, second.generation), (1, 2));

        let (sqlite, db) = sqlite_store().await;
        let first = sqlite
            .insert_arming(new_continuation("sqlite-one", 1))
            .await
            .unwrap();
        sqlite
            .cas_transition(
                &first.continuation_id,
                first.generation,
                first.version,
                ContinuationState::Arming,
                transition(ContinuationState::Cancelled),
            )
            .await
            .unwrap();
        let second = sqlite
            .insert_arming(new_continuation("sqlite-two", 1))
            .await
            .unwrap();
        assert_eq!((first.generation, second.generation), (1, 2));
        drop(db);
    }

    #[tokio::test]
    async fn continuation_store_cas_has_one_winner() {
        let memory = Arc::new(InMemoryContinuationStore::default());
        let record = memory
            .insert_arming(new_continuation("memory", 1))
            .await
            .unwrap();
        let one = memory.cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            ContinuationState::Arming,
            transition(ContinuationState::Waiting),
        );
        let two = memory.cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            ContinuationState::Arming,
            transition(ContinuationState::Waiting),
        );
        let (one, two) = tokio::join!(one, two);
        assert_eq!(
            [one.unwrap().is_some(), two.unwrap().is_some()]
                .into_iter()
                .filter(|won| *won)
                .count(),
            1
        );

        let (sqlite, db) = sqlite_store().await;
        let sqlite = Arc::new(sqlite);
        let record = sqlite
            .insert_arming(new_continuation("sqlite", 1))
            .await
            .unwrap();
        let one = sqlite.cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            ContinuationState::Arming,
            transition(ContinuationState::Waiting),
        );
        let two = sqlite.cas_transition(
            &record.continuation_id,
            record.generation,
            record.version,
            ContinuationState::Arming,
            transition(ContinuationState::Waiting),
        );
        let (one, two) = tokio::join!(one, two);
        assert_eq!(
            [one.unwrap().is_some(), two.unwrap().is_some()]
                .into_iter()
                .filter(|won| *won)
                .count(),
            1
        );
        drop(db);
    }

    #[tokio::test]
    async fn continuation_store_failure_and_parent_status_commit_atomically() {
        let memory = InMemoryContinuationStore::default();
        memory.seed_parent_status(1, "in_progress").await;
        let record = memory
            .insert_arming(new_continuation("memory", 1))
            .await
            .unwrap();
        assert!(memory
            .cas_fail_and_cancel_parent(
                &record.continuation_id,
                record.generation,
                record.version + 1,
                ContinuationState::Arming,
                ContinuationFailureCode::ArmFailed,
                Utc::now()
            )
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            memory.parent_status(1).await.as_deref(),
            Some("in_progress")
        );
        assert!(memory
            .cas_fail_and_cancel_parent(
                &record.continuation_id,
                record.generation,
                record.version,
                ContinuationState::Arming,
                ContinuationFailureCode::ArmFailed,
                Utc::now()
            )
            .await
            .unwrap()
            .is_some());
        assert_eq!(memory.parent_status(1).await.as_deref(), Some("cancelled"));

        let (sqlite, db) = sqlite_store().await;
        let record = sqlite
            .insert_arming(new_continuation("sqlite", 1))
            .await
            .unwrap();
        assert!(sqlite
            .cas_fail_and_cancel_parent(
                &record.continuation_id,
                record.generation,
                record.version + 1,
                ContinuationState::Arming,
                ContinuationFailureCode::ArmFailed,
                Utc::now()
            )
            .await
            .unwrap()
            .is_none());
        let stale_status: String = db
            .conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT status FROM conversation WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "status")
            .unwrap();
        assert_eq!(stale_status, "in_progress");
        assert!(sqlite
            .cas_fail_and_cancel_parent(
                &record.continuation_id,
                record.generation,
                record.version,
                ContinuationState::Arming,
                ContinuationFailureCode::ArmFailed,
                Utc::now()
            )
            .await
            .unwrap()
            .is_some());
        let status: String = db
            .conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT status FROM conversation WHERE id = 1".to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "status")
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    #[tokio::test]
    async fn continuation_store_cleanup_claim_is_exact_and_versioned() {
        let (sqlite, db) = sqlite_store().await;
        for store in [
            Arc::new(InMemoryContinuationStore::default()) as Arc<dyn ContinuationStore>,
            Arc::new(sqlite) as Arc<dyn ContinuationStore>,
        ] {
            let record = store
                .insert_arming(new_continuation("cleanup-claim", 1))
                .await
                .unwrap();
            let claimed = store
                .cas_claim_cleanup(
                    &record.continuation_id,
                    record.generation,
                    record.version,
                    ContinuationState::Arming,
                )
                .await
                .unwrap()
                .expect("exact active cleanup claim wins");
            assert_eq!(claimed.state, ContinuationState::Arming);
            assert_eq!(claimed.version, record.version + 1);

            for (generation, version, state) in [
                (
                    claimed.generation,
                    record.version,
                    ContinuationState::Arming,
                ),
                (
                    claimed.generation + 1,
                    claimed.version,
                    ContinuationState::Arming,
                ),
                (
                    claimed.generation,
                    claimed.version,
                    ContinuationState::Waiting,
                ),
            ] {
                assert!(store
                    .cas_claim_cleanup(&claimed.continuation_id, generation, version, state,)
                    .await
                    .unwrap()
                    .is_none());
                let unchanged = store.load(&claimed.continuation_id).await.unwrap().unwrap();
                assert_eq!(unchanged.generation, claimed.generation);
                assert_eq!(unchanged.version, claimed.version);
                assert_eq!(unchanged.state, claimed.state);
            }

            let cancelled = store
                .cas_transition(
                    &claimed.continuation_id,
                    claimed.generation,
                    claimed.version,
                    ContinuationState::Arming,
                    transition(ContinuationState::Cancelled),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(store
                .cas_claim_cleanup(
                    &cancelled.continuation_id,
                    cancelled.generation,
                    cancelled.version,
                    ContinuationState::Arming,
                )
                .await
                .unwrap()
                .is_none());
            assert!(matches!(
                store
                    .cas_claim_cleanup(
                        &cancelled.continuation_id,
                        cancelled.generation,
                        cancelled.version,
                        ContinuationState::Cancelled,
                    )
                    .await,
                Err(ContStoreError::InvalidRecord(_))
            ));
            for (generation, version) in [
                (u64::MAX, cancelled.version),
                (cancelled.generation, u64::MAX),
            ] {
                assert!(matches!(
                    store
                        .cas_claim_cleanup(
                            &cancelled.continuation_id,
                            generation,
                            version,
                            ContinuationState::Arming,
                        )
                        .await,
                    Err(ContStoreError::InvalidRecord(_))
                ));
            }
        }
        drop(db);
    }

    #[test]
    fn continuation_store_only_maps_active_parent_index_conflict_to_active_exists() {
        let error = DbErr::Custom(
            "UNIQUE constraint failed: delegation_continuations.parent_conversation_id, delegation_continuations.generation".to_string(),
        );
        assert!(matches!(
            map_active_exists(error),
            ContStoreError::Database(_)
        ));
    }

    #[tokio::test]
    async fn continuation_store_rejects_illegal_or_terminal_transition() {
        for store in [
            Arc::new(InMemoryContinuationStore::default()) as Arc<dyn ContinuationStore>,
            Arc::new(sqlite_store().await.0) as Arc<dyn ContinuationStore>,
        ] {
            let record = store
                .insert_arming(new_continuation("record", 1))
                .await
                .unwrap();
            assert!(matches!(
                store
                    .cas_transition(
                        &record.continuation_id,
                        record.generation,
                        record.version,
                        ContinuationState::Arming,
                        transition(ContinuationState::Completed)
                    )
                    .await,
                Err(ContStoreError::InvalidRecord(_))
            ));
            let cancelled = store
                .cas_transition(
                    &record.continuation_id,
                    record.generation,
                    record.version,
                    ContinuationState::Arming,
                    transition(ContinuationState::Cancelled),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(
                store
                    .cas_transition(
                        &cancelled.continuation_id,
                        cancelled.generation,
                        cancelled.version,
                        ContinuationState::Cancelled,
                        transition(ContinuationState::Cancelled)
                    )
                    .await,
                Err(ContStoreError::InvalidRecord(_))
            ));
        }
    }

    #[tokio::test]
    async fn continuation_store_keep_does_not_clear_existing_fields() {
        let memory = InMemoryContinuationStore::default();
        let record = memory
            .insert_arming(new_continuation("memory", 1))
            .await
            .unwrap();
        let waiting = memory.cas_transition(&record.continuation_id, record.generation, record.version, ContinuationState::Arming, ContinuationPatch { state: ContinuationState::Waiting, wake_reason: FieldPatch::Set(crate::acp::delegation::continuation::types::ContinuationWakeReason::Checkpoint), ..keep_patch() }).await.unwrap().unwrap();
        let updated = memory
            .cas_transition(
                &waiting.continuation_id,
                waiting.generation,
                waiting.version,
                ContinuationState::Waiting,
                transition(ContinuationState::WakePending),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.wake_reason,
            Some(crate::acp::delegation::continuation::types::ContinuationWakeReason::Checkpoint)
        );

        let (sqlite, db) = sqlite_store().await;
        let record = sqlite
            .insert_arming(new_continuation("sqlite", 1))
            .await
            .unwrap();
        let waiting = sqlite.cas_transition(&record.continuation_id, record.generation, record.version, ContinuationState::Arming, ContinuationPatch { state: ContinuationState::Waiting, wake_reason: FieldPatch::Set(crate::acp::delegation::continuation::types::ContinuationWakeReason::Checkpoint), ..keep_patch() }).await.unwrap().unwrap();
        let updated = sqlite
            .cas_transition(
                &waiting.continuation_id,
                waiting.generation,
                waiting.version,
                ContinuationState::Waiting,
                transition(ContinuationState::WakePending),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            updated.wake_reason,
            Some(crate::acp::delegation::continuation::types::ContinuationWakeReason::Checkpoint)
        );
        drop(db);
    }

    #[tokio::test]
    async fn continuation_store_task_ids_json_roundtrip_preserves_order() {
        let memory = InMemoryContinuationStore::default();
        let record = memory
            .insert_arming(new_continuation("memory", 1))
            .await
            .unwrap();
        assert_eq!(record.task_ids.0, vec!["task-b", "task-a"]);

        let (sqlite, db) = sqlite_store().await;
        let record = sqlite
            .insert_arming(new_continuation("sqlite", 1))
            .await
            .unwrap();
        assert_eq!(record.task_ids.0, vec!["task-b", "task-a"]);
        drop(db);
    }

    #[tokio::test]
    async fn continuation_store_marker_requires_matching_conversation_and_admission() {
        let memory = InMemoryContinuationStore::default();
        let record = memory
            .insert_arming(new_continuation("memory", 1))
            .await
            .unwrap();
        assert!(!memory
            .matches_admitted_marker(1, &record.internal_prompt_marker)
            .await
            .unwrap());
        let resuming = memory
            .cas_transition(
                &record.continuation_id,
                record.generation,
                record.version,
                ContinuationState::Arming,
                transition(ContinuationState::WakePending),
            )
            .await
            .unwrap()
            .unwrap();
        let resuming = memory
            .cas_transition(
                &resuming.continuation_id,
                resuming.generation,
                resuming.version,
                ContinuationState::WakePending,
                ContinuationPatch {
                    state: ContinuationState::Resuming,
                    prompt_admitted_at: FieldPatch::Set(Utc::now()),
                    ..keep_patch()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(memory
            .matches_admitted_marker(1, &resuming.internal_prompt_marker)
            .await
            .unwrap());
        assert!(!memory
            .matches_admitted_marker(2, &resuming.internal_prompt_marker)
            .await
            .unwrap());

        let (sqlite, db) = sqlite_store().await;
        let record = sqlite
            .insert_arming(new_continuation("sqlite", 1))
            .await
            .unwrap();
        assert!(!sqlite
            .matches_admitted_marker(1, &record.internal_prompt_marker)
            .await
            .unwrap());
        let resuming = sqlite
            .cas_transition(
                &record.continuation_id,
                record.generation,
                record.version,
                ContinuationState::Arming,
                transition(ContinuationState::WakePending),
            )
            .await
            .unwrap()
            .unwrap();
        let resuming = sqlite
            .cas_transition(
                &resuming.continuation_id,
                resuming.generation,
                resuming.version,
                ContinuationState::WakePending,
                ContinuationPatch {
                    state: ContinuationState::Resuming,
                    prompt_admitted_at: FieldPatch::Set(Utc::now()),
                    ..keep_patch()
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert!(sqlite
            .matches_admitted_marker(1, &resuming.internal_prompt_marker)
            .await
            .unwrap());
        assert!(!sqlite
            .matches_admitted_marker(2, &resuming.internal_prompt_marker)
            .await
            .unwrap());
        drop(db);
    }

    fn keep_patch() -> ContinuationPatch {
        ContinuationPatch {
            state: ContinuationState::Arming,
            wake_reason: FieldPatch::Keep,
            suspend_requested_at: FieldPatch::Keep,
            suspended_at: FieldPatch::Keep,
            wake_claimed_at: FieldPatch::Keep,
            prompt_admitted_at: FieldPatch::Keep,
            finished_at: FieldPatch::Keep,
            failure_code: FieldPatch::Keep,
        }
    }

    fn transition(state: ContinuationState) -> ContinuationPatch {
        ContinuationPatch {
            state,
            ..keep_patch()
        }
    }
}
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, QueryResult, Statement,
    TransactionTrait, TryGetable,
};

use super::types::{
    ContinuationFailureCode, ContinuationState, ContinuationTaskIds, ContinuationWakeReason,
};

#[derive(Debug, Clone)]
pub struct NewContinuation {
    pub continuation_id: String,
    pub parent_conversation_id: i32,
    pub parent_session_id: String,
    pub parent_connection_id: String,
    pub parent_turn_generation: u64,
    pub task_ids: ContinuationTaskIds,
    pub armed_at: DateTime<Utc>,
    pub wake_at: DateTime<Utc>,
    pub internal_prompt_id: String,
    pub internal_prompt_marker: String,
}

#[derive(Debug, Clone)]
pub struct ContinuationRecord {
    pub continuation_id: String,
    pub parent_conversation_id: i32,
    pub parent_session_id: String,
    pub parent_connection_id: Option<String>,
    pub generation: u64,
    pub parent_turn_generation: u64,
    pub task_ids: ContinuationTaskIds,
    pub state: ContinuationState,
    pub wake_reason: Option<ContinuationWakeReason>,
    pub armed_at: DateTime<Utc>,
    pub wake_at: DateTime<Utc>,
    pub suspend_requested_at: Option<DateTime<Utc>>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub wake_claimed_at: Option<DateTime<Utc>>,
    pub prompt_admitted_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub internal_prompt_id: String,
    pub internal_prompt_marker: String,
    pub failure_code: Option<ContinuationFailureCode>,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FieldPatch<T> {
    #[default]
    Keep,
    Set(T),
    Clear,
}

#[derive(Debug, Clone)]
pub struct ContinuationPatch {
    pub state: ContinuationState,
    pub wake_reason: FieldPatch<ContinuationWakeReason>,
    pub suspend_requested_at: FieldPatch<DateTime<Utc>>,
    pub suspended_at: FieldPatch<DateTime<Utc>>,
    pub wake_claimed_at: FieldPatch<DateTime<Utc>>,
    pub prompt_admitted_at: FieldPatch<DateTime<Utc>>,
    pub finished_at: FieldPatch<DateTime<Utc>>,
    pub failure_code: FieldPatch<ContinuationFailureCode>,
}

#[derive(Debug, thiserror::Error)]
pub enum ContStoreError {
    #[error("an active continuation already owns this conversation")]
    ActiveExists,
    #[error("invalid continuation record: {0}")]
    InvalidRecord(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Database(#[from] DbErr),
}

#[async_trait]
pub trait ContinuationStore: Send + Sync {
    async fn insert_arming(
        &self,
        new: NewContinuation,
    ) -> Result<ContinuationRecord, ContStoreError>;
    async fn load(
        &self,
        continuation_id: &str,
    ) -> Result<Option<ContinuationRecord>, ContStoreError>;
    async fn load_active_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Result<Option<ContinuationRecord>, ContStoreError>;
    async fn list_non_terminal(&self) -> Result<Vec<ContinuationRecord>, ContStoreError>;
    async fn fail_non_terminal_on_startup(
        &self,
        _finished_at: DateTime<Utc>,
    ) -> Result<Vec<ContinuationRecord>, ContStoreError> {
        Err(ContStoreError::InvalidRecord(
            "startup reconciliation is not implemented by this continuation store".to_string(),
        ))
    }
    async fn cas_transition(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
        patch: ContinuationPatch,
    ) -> Result<Option<ContinuationRecord>, ContStoreError>;
    async fn cas_claim_cleanup(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
    ) -> Result<Option<ContinuationRecord>, ContStoreError>;
    async fn cas_fail_and_cancel_parent(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
        failure_code: ContinuationFailureCode,
        finished_at: DateTime<Utc>,
    ) -> Result<Option<ContinuationRecord>, ContStoreError>;
    async fn matches_admitted_marker(
        &self,
        conversation_id: i32,
        marker: &str,
    ) -> Result<bool, ContStoreError>;
    async fn load_latest_failure_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Result<Option<ContinuationRecord>, ContStoreError>;
}

pub struct DbContinuationStore {
    db: DatabaseConnection,
}

impl DbContinuationStore {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    fn statement(sql: &str, values: Vec<sea_orm::Value>) -> Statement {
        Statement::from_sql_and_values(DbBackend::Sqlite, sql.to_string(), values)
    }
}

const INSERT_ARMING_SQL: &str = r#"
INSERT INTO delegation_continuations (
  continuation_id, parent_conversation_id, parent_session_id,
  parent_connection_id, generation, parent_turn_generation, task_ids_json,
  state, wake_reason, armed_at, wake_at, suspend_requested_at, suspended_at,
  wake_claimed_at, prompt_admitted_at, finished_at, internal_prompt_id,
  internal_prompt_marker, failure_code, version, created_at, updated_at
)
SELECT
  ?1, ?2, ?3, ?4,
  (SELECT COALESCE(MAX(generation), 0) + 1
     FROM delegation_continuations
    WHERE parent_conversation_id = ?2),
  ?5, ?6, 'arming', NULL, ?7, ?8, NULL, NULL, NULL, NULL, NULL,
  ?9, ?10, NULL, 0, ?7, ?7
WHERE NOT EXISTS (
  SELECT 1 FROM delegation_continuations
   WHERE parent_conversation_id = ?2
     AND state IN ('arming','waiting','wake_pending','resuming')
)
RETURNING *
"#;

const CAS_TRANSITION_SQL: &str = "UPDATE delegation_continuations SET \
state = ?5, \
wake_reason = CASE ?6 WHEN 1 THEN ?7 WHEN 2 THEN NULL ELSE wake_reason END, \
suspend_requested_at = CASE ?8 WHEN 1 THEN ?9 WHEN 2 THEN NULL ELSE suspend_requested_at END, \
suspended_at = CASE ?10 WHEN 1 THEN ?11 WHEN 2 THEN NULL ELSE suspended_at END, \
wake_claimed_at = CASE ?12 WHEN 1 THEN ?13 WHEN 2 THEN NULL ELSE wake_claimed_at END, \
prompt_admitted_at = CASE ?14 WHEN 1 THEN ?15 WHEN 2 THEN NULL ELSE prompt_admitted_at END, \
finished_at = CASE ?16 WHEN 1 THEN ?17 WHEN 2 THEN NULL ELSE finished_at END, \
failure_code = CASE ?18 WHEN 1 THEN ?19 WHEN 2 THEN NULL ELSE failure_code END, \
version = version + 1, updated_at = ?20 \
WHERE continuation_id = ?1 AND generation = ?2 AND version = ?3 AND state = ?4 \
RETURNING *";

const CAS_CLAIM_CLEANUP_SQL: &str = "UPDATE delegation_continuations SET \
version = version + 1, updated_at = ?5 \
WHERE continuation_id = ?1 AND generation = ?2 AND version = ?3 AND state = ?4 \
RETURNING *";

const CAS_FAIL_SQL: &str = "UPDATE delegation_continuations SET \
state = 'failed', failure_code = ?5, finished_at = ?6, version = version + 1, updated_at = ?7 \
WHERE continuation_id = ?1 AND generation = ?2 AND version = ?3 AND state = ?4 \
RETURNING *";

#[async_trait]
impl ContinuationStore for DbContinuationStore {
    async fn insert_arming(
        &self,
        new: NewContinuation,
    ) -> Result<ContinuationRecord, ContStoreError> {
        let task_ids_json = serde_json::to_string(&new.task_ids)?;
        let parent_turn_generation = to_i64(new.parent_turn_generation, "parent_turn_generation")?;
        let row = self
            .db
            .query_one(Self::statement(
                INSERT_ARMING_SQL,
                vec![
                    new.continuation_id.into(),
                    new.parent_conversation_id.into(),
                    new.parent_session_id.into(),
                    new.parent_connection_id.into(),
                    parent_turn_generation.into(),
                    task_ids_json.into(),
                    new.armed_at.into(),
                    new.wake_at.into(),
                    new.internal_prompt_id.into(),
                    new.internal_prompt_marker.into(),
                ],
            ))
            .await
            .map_err(map_active_exists)?;
        row.map(|row| record_from_row(&row))
            .transpose()?
            .ok_or(ContStoreError::ActiveExists)
    }

    async fn load(
        &self,
        continuation_id: &str,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        load_one(
            &self.db,
            "SELECT * FROM delegation_continuations WHERE continuation_id = ?",
            vec![continuation_id.into()],
        )
        .await
    }

    async fn load_active_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        load_one(&self.db, "SELECT * FROM delegation_continuations WHERE parent_conversation_id = ? AND state IN ('arming','waiting','wake_pending','resuming')", vec![conversation_id.into()]).await
    }

    async fn list_non_terminal(&self) -> Result<Vec<ContinuationRecord>, ContStoreError> {
        let rows = self.db.query_all(Self::statement(
            "SELECT * FROM delegation_continuations WHERE state NOT IN ('completed','cancelled','failed') ORDER BY parent_conversation_id, generation",
            vec![],
        )).await?;
        rows.iter().map(record_from_row).collect()
    }

    async fn fail_non_terminal_on_startup(
        &self,
        finished_at: DateTime<Utc>,
    ) -> Result<Vec<ContinuationRecord>, ContStoreError> {
        let txn = self.db.begin().await?;
        let candidates = txn
            .query_all(Self::statement(
                "SELECT * FROM delegation_continuations WHERE state IN ('arming','waiting','wake_pending','resuming') ORDER BY parent_conversation_id, generation",
                vec![],
            ))
            .await?;
        let candidates: Vec<_> = candidates
            .iter()
            .map(record_from_row)
            .collect::<Result<_, _>>()?;
        let mut winners = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let row = txn
                .query_one(Self::statement(
                    CAS_FAIL_SQL,
                    vec![
                        candidate.continuation_id.clone().into(),
                        to_i64(candidate.generation, "generation")?.into(),
                        to_i64(candidate.version, "version")?.into(),
                        candidate.state.as_str().into(),
                        ContinuationFailureCode::ParentConnectionLost
                            .as_str()
                            .into(),
                        finished_at.into(),
                        finished_at.into(),
                    ],
                ))
                .await?;
            let Some(record) = row.map(|row| record_from_row(&row)).transpose()? else {
                continue;
            };
            txn.execute(Self::statement(
                "UPDATE conversation SET status = 'cancelled' WHERE id = ? AND status = 'in_progress'",
                vec![record.parent_conversation_id.into()],
            ))
            .await?;
            winners.push(record);
        }
        txn.commit().await?;
        Ok(winners)
    }

    async fn cas_transition(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
        patch: ContinuationPatch,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        validate_transition(expected_state, &patch)?;
        let now = Utc::now();
        let row = self
            .db
            .query_one(Self::statement(
                CAS_TRANSITION_SQL,
                cas_values(
                    continuation_id,
                    generation,
                    expected_version,
                    expected_state,
                    &patch,
                    now,
                )?,
            ))
            .await?;
        row.map(|row| record_from_row(&row)).transpose()
    }

    async fn cas_claim_cleanup(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        validate_cleanup_claim(expected_state)?;
        let row = self
            .db
            .query_one(Self::statement(
                CAS_CLAIM_CLEANUP_SQL,
                vec![
                    continuation_id.into(),
                    to_i64(generation, "generation")?.into(),
                    to_i64(expected_version, "version")?.into(),
                    expected_state.as_str().into(),
                    Utc::now().into(),
                ],
            ))
            .await?;
        row.map(|row| record_from_row(&row)).transpose()
    }

    async fn cas_fail_and_cancel_parent(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
        failure_code: ContinuationFailureCode,
        finished_at: DateTime<Utc>,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        let patch = ContinuationPatch {
            state: ContinuationState::Failed,
            wake_reason: FieldPatch::Keep,
            suspend_requested_at: FieldPatch::Keep,
            suspended_at: FieldPatch::Keep,
            wake_claimed_at: FieldPatch::Keep,
            prompt_admitted_at: FieldPatch::Keep,
            finished_at: FieldPatch::Set(finished_at),
            failure_code: FieldPatch::Set(failure_code),
        };
        validate_transition(expected_state, &patch)?;
        let txn = self.db.begin().await?;
        let row = txn
            .query_one(Self::statement(
                CAS_FAIL_SQL,
                vec![
                    continuation_id.into(),
                    to_i64(generation, "generation")?.into(),
                    to_i64(expected_version, "version")?.into(),
                    expected_state.as_str().into(),
                    failure_code.as_str().into(),
                    finished_at.into(),
                    Utc::now().into(),
                ],
            ))
            .await?;
        let result = row.map(|row| record_from_row(&row)).transpose()?;
        if let Some(record) = &result {
            txn.execute(Self::statement(
                "UPDATE conversation SET status = 'cancelled' WHERE id = ? AND status = 'in_progress'",
                vec![record.parent_conversation_id.into()],
            )).await?;
        }
        txn.commit().await?;
        Ok(result)
    }

    async fn matches_admitted_marker(
        &self,
        conversation_id: i32,
        marker: &str,
    ) -> Result<bool, ContStoreError> {
        Ok(self.db.query_one(Self::statement(
            "SELECT 1 FROM delegation_continuations WHERE parent_conversation_id = ? AND internal_prompt_marker = ? AND prompt_admitted_at IS NOT NULL",
            vec![conversation_id.into(), marker.into()],
        )).await?.is_some())
    }

    async fn load_latest_failure_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        let latest = load_one(
            &self.db,
            "SELECT dc.* FROM delegation_continuations dc JOIN conversation c ON c.id = dc.parent_conversation_id WHERE dc.parent_conversation_id = ? AND c.status = 'cancelled' ORDER BY dc.generation DESC LIMIT 1",
            vec![conversation_id.into()],
        )
        .await?;
        Ok(latest.filter(|record| record.state == ContinuationState::Failed))
    }
}

fn map_active_exists(err: DbErr) -> ContStoreError {
    let message = err.to_string().to_ascii_lowercase();
    let unique_target = message.rsplit_once(':').map(|(_, target)| target.trim());
    if message.contains("idx_cont_one_active_per_parent")
        || unique_target == Some("delegation_continuations.parent_conversation_id")
    {
        ContStoreError::ActiveExists
    } else {
        ContStoreError::Database(err)
    }
}

async fn load_one<C: ConnectionTrait>(
    conn: &C,
    sql: &str,
    values: Vec<sea_orm::Value>,
) -> Result<Option<ContinuationRecord>, ContStoreError> {
    conn.query_one(DbContinuationStore::statement(sql, values))
        .await?
        .map(|row| record_from_row(&row))
        .transpose()
}

fn to_i64(value: u64, field: &str) -> Result<i64, ContStoreError> {
    i64::try_from(value).map_err(|_| ContStoreError::InvalidRecord(format!("{field} exceeds i64")))
}

fn to_u64(value: i64, field: &str) -> Result<u64, ContStoreError> {
    u64::try_from(value).map_err(|_| ContStoreError::InvalidRecord(format!("{field} is negative")))
}

fn record_from_row(row: &QueryResult) -> Result<ContinuationRecord, ContStoreError> {
    let task_ids_json: String = row_get(row, "task_ids_json")?;
    let task_ids = decode_task_ids(&task_ids_json)?;
    let state: String = row_get(row, "state")?;
    let wake_reason: Option<String> = row_get(row, "wake_reason")?;
    let failure_code: Option<String> = row_get(row, "failure_code")?;
    Ok(ContinuationRecord {
        continuation_id: row_get(row, "continuation_id")?,
        parent_conversation_id: row_get(row, "parent_conversation_id")?,
        parent_session_id: row_get(row, "parent_session_id")?,
        parent_connection_id: row_get(row, "parent_connection_id")?,
        generation: to_u64(row_get(row, "generation")?, "generation")?,
        parent_turn_generation: to_u64(
            row_get(row, "parent_turn_generation")?,
            "parent_turn_generation",
        )?,
        task_ids,
        state: ContinuationState::from_str(&state)
            .map_err(|error| ContStoreError::InvalidRecord(error.to_string()))?,
        wake_reason: wake_reason
            .map(|value| {
                ContinuationWakeReason::from_str(&value)
                    .map_err(|error| ContStoreError::InvalidRecord(error.to_string()))
            })
            .transpose()?,
        armed_at: row_get(row, "armed_at")?,
        wake_at: row_get(row, "wake_at")?,
        suspend_requested_at: row_get(row, "suspend_requested_at")?,
        suspended_at: row_get(row, "suspended_at")?,
        wake_claimed_at: row_get(row, "wake_claimed_at")?,
        prompt_admitted_at: row_get(row, "prompt_admitted_at")?,
        finished_at: row_get(row, "finished_at")?,
        internal_prompt_id: row_get(row, "internal_prompt_id")?,
        internal_prompt_marker: row_get(row, "internal_prompt_marker")?,
        failure_code: failure_code
            .map(|value| {
                ContinuationFailureCode::from_str(&value)
                    .map_err(|error| ContStoreError::InvalidRecord(error.to_string()))
            })
            .transpose()?,
        version: to_u64(row_get(row, "version")?, "version")?,
        created_at: row_get(row, "created_at")?,
        updated_at: row_get(row, "updated_at")?,
    })
}

fn row_get<T: TryGetable>(row: &QueryResult, column: &str) -> Result<T, ContStoreError> {
    row.try_get("", column)
        .map_err(|error| ContStoreError::InvalidRecord(error.to_string()))
}

fn decode_task_ids(value: &str) -> Result<ContinuationTaskIds, ContStoreError> {
    let json: serde_json::Value = serde_json::from_str(value)?;
    let Some(items) = json.as_array() else {
        return Err(ContStoreError::InvalidRecord(
            "task_ids_json must be an array".to_string(),
        ));
    };
    if items.iter().any(|item| !item.is_string()) {
        return Err(ContStoreError::InvalidRecord(
            "task_ids_json must contain only strings".to_string(),
        ));
    }
    serde_json::from_str(value).map_err(ContStoreError::Json)
}

fn patch_control<T>(patch: &FieldPatch<T>) -> i64 {
    match patch {
        FieldPatch::Keep => 0,
        FieldPatch::Set(_) => 1,
        FieldPatch::Clear => 2,
    }
}

fn datetime_patch_value(patch: &FieldPatch<DateTime<Utc>>) -> sea_orm::Value {
    match patch {
        FieldPatch::Set(value) => value.clone().into(),
        FieldPatch::Keep | FieldPatch::Clear => Option::<String>::None.into(),
    }
}

fn wake_reason_patch_value(patch: &FieldPatch<ContinuationWakeReason>) -> sea_orm::Value {
    match patch {
        FieldPatch::Set(value) => value.as_str().into(),
        FieldPatch::Keep | FieldPatch::Clear => Option::<String>::None.into(),
    }
}

fn failure_code_patch_value(patch: &FieldPatch<ContinuationFailureCode>) -> sea_orm::Value {
    match patch {
        FieldPatch::Set(value) => value.as_str().into(),
        FieldPatch::Keep | FieldPatch::Clear => Option::<String>::None.into(),
    }
}

fn cas_values(
    continuation_id: &str,
    generation: u64,
    expected_version: u64,
    expected_state: ContinuationState,
    patch: &ContinuationPatch,
    updated_at: DateTime<Utc>,
) -> Result<Vec<sea_orm::Value>, ContStoreError> {
    Ok(vec![
        continuation_id.into(),
        to_i64(generation, "generation")?.into(),
        to_i64(expected_version, "version")?.into(),
        expected_state.as_str().into(),
        patch.state.as_str().into(),
        patch_control(&patch.wake_reason).into(),
        wake_reason_patch_value(&patch.wake_reason),
        patch_control(&patch.suspend_requested_at).into(),
        datetime_patch_value(&patch.suspend_requested_at),
        patch_control(&patch.suspended_at).into(),
        datetime_patch_value(&patch.suspended_at),
        patch_control(&patch.wake_claimed_at).into(),
        datetime_patch_value(&patch.wake_claimed_at),
        patch_control(&patch.prompt_admitted_at).into(),
        datetime_patch_value(&patch.prompt_admitted_at),
        patch_control(&patch.finished_at).into(),
        datetime_patch_value(&patch.finished_at),
        patch_control(&patch.failure_code).into(),
        failure_code_patch_value(&patch.failure_code),
        updated_at.into(),
    ])
}

fn validate_transition(
    expected_state: ContinuationState,
    patch: &ContinuationPatch,
) -> Result<(), ContStoreError> {
    let allowed = matches!(
        (expected_state, patch.state),
        (
            ContinuationState::Arming,
            ContinuationState::Waiting
                | ContinuationState::WakePending
                | ContinuationState::Cancelled
                | ContinuationState::Failed
        ) | (
            ContinuationState::Waiting,
            ContinuationState::WakePending
                | ContinuationState::Cancelled
                | ContinuationState::Failed
        ) | (
            ContinuationState::WakePending,
            ContinuationState::Resuming | ContinuationState::Cancelled | ContinuationState::Failed
        ) | (
            ContinuationState::Resuming,
            ContinuationState::Completed | ContinuationState::Cancelled | ContinuationState::Failed
        )
    );
    let self_write = match expected_state {
        ContinuationState::Arming if patch.state == ContinuationState::Arming => {
            matches!(patch.suspend_requested_at, FieldPatch::Set(_))
                && matches!(patch.wake_reason, FieldPatch::Keep)
                && matches!(patch.suspended_at, FieldPatch::Keep)
                && matches!(patch.wake_claimed_at, FieldPatch::Keep)
                && matches!(patch.prompt_admitted_at, FieldPatch::Keep)
                && matches!(patch.finished_at, FieldPatch::Keep)
                && matches!(patch.failure_code, FieldPatch::Keep)
        }
        ContinuationState::WakePending if patch.state == ContinuationState::WakePending => {
            matches!(patch.suspended_at, FieldPatch::Set(_))
                && matches!(patch.wake_reason, FieldPatch::Keep)
                && matches!(patch.suspend_requested_at, FieldPatch::Keep)
                && matches!(patch.wake_claimed_at, FieldPatch::Keep)
                && matches!(patch.prompt_admitted_at, FieldPatch::Keep)
                && matches!(patch.finished_at, FieldPatch::Keep)
                && matches!(patch.failure_code, FieldPatch::Keep)
        }
        ContinuationState::Resuming if patch.state == ContinuationState::Resuming => {
            matches!(
                patch.prompt_admitted_at,
                FieldPatch::Set(_) | FieldPatch::Clear
            ) && matches!(patch.wake_reason, FieldPatch::Keep)
                && matches!(patch.suspend_requested_at, FieldPatch::Keep)
                && matches!(patch.suspended_at, FieldPatch::Keep)
                && matches!(patch.wake_claimed_at, FieldPatch::Keep)
                && matches!(patch.finished_at, FieldPatch::Keep)
                && matches!(patch.failure_code, FieldPatch::Keep)
        }
        _ => false,
    };
    if allowed || self_write {
        Ok(())
    } else {
        Err(ContStoreError::InvalidRecord(format!(
            "illegal continuation transition {} -> {}",
            expected_state.as_str(),
            patch.state.as_str()
        )))
    }
}

fn validate_cleanup_claim(state: ContinuationState) -> Result<(), ContStoreError> {
    if is_active(state) {
        Ok(())
    } else {
        Err(ContStoreError::InvalidRecord(format!(
            "cannot claim terminal continuation state {} for cleanup",
            state.as_str()
        )))
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Default)]
pub struct InMemoryContinuationStore {
    inner: tokio::sync::Mutex<InMemoryState>,
}

#[cfg(any(test, feature = "test-utils"))]
#[derive(Clone, Default)]
struct InMemoryState {
    records: std::collections::HashMap<String, ContinuationRecord>,
    parent_statuses: std::collections::HashMap<i32, String>,
    fail_next_startup_before_commit: bool,
}

#[cfg(any(test, feature = "test-utils"))]
impl InMemoryContinuationStore {
    #[cfg(test)]
    pub(crate) async fn seed_parent_status(&self, conversation_id: i32, status: &str) {
        self.inner
            .lock()
            .await
            .parent_statuses
            .insert(conversation_id, status.to_string());
    }

    #[cfg(test)]
    pub(crate) async fn parent_status(&self, conversation_id: i32) -> Option<String> {
        self.inner
            .lock()
            .await
            .parent_statuses
            .get(&conversation_id)
            .cloned()
    }

    #[cfg(test)]
    async fn fail_next_startup_before_commit(&self) {
        self.inner.lock().await.fail_next_startup_before_commit = true;
    }
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait]
impl ContinuationStore for InMemoryContinuationStore {
    async fn insert_arming(
        &self,
        new: NewContinuation,
    ) -> Result<ContinuationRecord, ContStoreError> {
        let mut inner = self.inner.lock().await;
        if inner.records.values().any(|record| {
            record.parent_conversation_id == new.parent_conversation_id && is_active(record.state)
        }) {
            return Err(ContStoreError::ActiveExists);
        }
        let generation = inner
            .records
            .values()
            .filter(|record| record.parent_conversation_id == new.parent_conversation_id)
            .map(|record| record.generation)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| ContStoreError::InvalidRecord("generation overflow".to_string()))?;
        let record = ContinuationRecord {
            continuation_id: new.continuation_id,
            parent_conversation_id: new.parent_conversation_id,
            parent_session_id: new.parent_session_id,
            parent_connection_id: Some(new.parent_connection_id),
            generation,
            parent_turn_generation: new.parent_turn_generation,
            task_ids: new.task_ids,
            state: ContinuationState::Arming,
            wake_reason: None,
            armed_at: new.armed_at,
            wake_at: new.wake_at,
            suspend_requested_at: None,
            suspended_at: None,
            wake_claimed_at: None,
            prompt_admitted_at: None,
            finished_at: None,
            internal_prompt_id: new.internal_prompt_id,
            internal_prompt_marker: new.internal_prompt_marker,
            failure_code: None,
            version: 0,
            created_at: new.armed_at,
            updated_at: new.armed_at,
        };
        inner
            .records
            .insert(record.continuation_id.clone(), record.clone());
        Ok(record)
    }

    async fn load(
        &self,
        continuation_id: &str,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        Ok(self
            .inner
            .lock()
            .await
            .records
            .get(continuation_id)
            .cloned())
    }

    async fn load_active_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        Ok(self
            .inner
            .lock()
            .await
            .records
            .values()
            .find(|record| {
                record.parent_conversation_id == conversation_id && is_active(record.state)
            })
            .cloned())
    }

    async fn list_non_terminal(&self) -> Result<Vec<ContinuationRecord>, ContStoreError> {
        let mut records: Vec<_> = self
            .inner
            .lock()
            .await
            .records
            .values()
            .filter(|record| is_active(record.state))
            .cloned()
            .collect();
        records.sort_by_key(|record| (record.parent_conversation_id, record.generation));
        Ok(records)
    }

    async fn fail_non_terminal_on_startup(
        &self,
        finished_at: DateTime<Utc>,
    ) -> Result<Vec<ContinuationRecord>, ContStoreError> {
        let mut inner = self.inner.lock().await;
        let fail_before_commit = std::mem::take(&mut inner.fail_next_startup_before_commit);
        let mut transaction = inner.clone();
        let mut keys: Vec<_> = transaction
            .records
            .iter()
            .filter(|(_, record)| is_active(record.state))
            .map(|(id, record)| (id.clone(), record.parent_conversation_id, record.generation))
            .collect();
        keys.sort_by_key(|(_, conversation_id, generation)| (*conversation_id, *generation));
        let mut winners = Vec::with_capacity(keys.len());
        for (continuation_id, conversation_id, _) in keys {
            let Some(record) = transaction.records.get_mut(&continuation_id) else {
                continue;
            };
            if !is_active(record.state) {
                continue;
            }
            record.state = ContinuationState::Failed;
            record.failure_code = Some(ContinuationFailureCode::ParentConnectionLost);
            record.finished_at = Some(finished_at);
            record.version += 1;
            record.updated_at = finished_at;
            winners.push(record.clone());
            if transaction
                .parent_statuses
                .get(&conversation_id)
                .is_some_and(|status| status == "in_progress")
            {
                transaction
                    .parent_statuses
                    .insert(conversation_id, "cancelled".to_string());
            }
        }
        if fail_before_commit {
            return Err(ContStoreError::InvalidRecord(
                "injected in-memory startup transaction failure".to_string(),
            ));
        }
        *inner = transaction;
        Ok(winners)
    }

    async fn cas_transition(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
        patch: ContinuationPatch,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        validate_transition(expected_state, &patch)?;
        let mut inner = self.inner.lock().await;
        let Some(record) = inner.records.get_mut(continuation_id) else {
            return Ok(None);
        };
        if record.generation != generation
            || record.version != expected_version
            || record.state != expected_state
        {
            return Ok(None);
        }
        apply_patch(record, patch);
        Ok(Some(record.clone()))
    }

    async fn cas_claim_cleanup(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        validate_cleanup_claim(expected_state)?;
        to_i64(generation, "generation")?;
        to_i64(expected_version, "version")?;
        let mut inner = self.inner.lock().await;
        let Some(record) = inner.records.get_mut(continuation_id) else {
            return Ok(None);
        };
        if record.generation != generation
            || record.version != expected_version
            || record.state != expected_state
        {
            return Ok(None);
        }
        record.version += 1;
        record.updated_at = Utc::now();
        Ok(Some(record.clone()))
    }

    async fn cas_fail_and_cancel_parent(
        &self,
        continuation_id: &str,
        generation: u64,
        expected_version: u64,
        expected_state: ContinuationState,
        failure_code: ContinuationFailureCode,
        finished_at: DateTime<Utc>,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        let patch = ContinuationPatch {
            state: ContinuationState::Failed,
            wake_reason: FieldPatch::Keep,
            suspend_requested_at: FieldPatch::Keep,
            suspended_at: FieldPatch::Keep,
            wake_claimed_at: FieldPatch::Keep,
            prompt_admitted_at: FieldPatch::Keep,
            finished_at: FieldPatch::Set(finished_at),
            failure_code: FieldPatch::Set(failure_code),
        };
        validate_transition(expected_state, &patch)?;
        let mut inner = self.inner.lock().await;
        let Some(record) = inner.records.get_mut(continuation_id) else {
            return Ok(None);
        };
        if record.generation != generation
            || record.version != expected_version
            || record.state != expected_state
        {
            return Ok(None);
        }
        apply_patch(record, patch);
        let record = record.clone();
        if inner
            .parent_statuses
            .get(&record.parent_conversation_id)
            .is_some_and(|status| status == "in_progress")
        {
            inner
                .parent_statuses
                .insert(record.parent_conversation_id, "cancelled".to_string());
        }
        Ok(Some(record))
    }

    async fn matches_admitted_marker(
        &self,
        conversation_id: i32,
        marker: &str,
    ) -> Result<bool, ContStoreError> {
        Ok(self.inner.lock().await.records.values().any(|record| {
            record.parent_conversation_id == conversation_id
                && record.internal_prompt_marker == marker
                && record.prompt_admitted_at.is_some()
        }))
    }

    async fn load_latest_failure_for_conversation(
        &self,
        conversation_id: i32,
    ) -> Result<Option<ContinuationRecord>, ContStoreError> {
        let inner = self.inner.lock().await;
        if !inner
            .parent_statuses
            .get(&conversation_id)
            .is_some_and(|status| status == "cancelled")
        {
            return Ok(None);
        }
        Ok(inner
            .records
            .values()
            .filter(|record| record.parent_conversation_id == conversation_id)
            .max_by_key(|record| record.generation)
            .filter(|record| record.state == ContinuationState::Failed)
            .cloned())
    }
}

#[cfg(any(test, feature = "test-utils"))]
fn apply_patch(record: &mut ContinuationRecord, patch: ContinuationPatch) {
    record.state = patch.state;
    apply_option_patch(&mut record.wake_reason, patch.wake_reason);
    apply_option_patch(&mut record.suspend_requested_at, patch.suspend_requested_at);
    apply_option_patch(&mut record.suspended_at, patch.suspended_at);
    apply_option_patch(&mut record.wake_claimed_at, patch.wake_claimed_at);
    apply_option_patch(&mut record.prompt_admitted_at, patch.prompt_admitted_at);
    apply_option_patch(&mut record.finished_at, patch.finished_at);
    apply_option_patch(&mut record.failure_code, patch.failure_code);
    record.version += 1;
    record.updated_at = Utc::now();
}

#[cfg(any(test, feature = "test-utils"))]
fn apply_option_patch<T>(target: &mut Option<T>, patch: FieldPatch<T>) {
    match patch {
        FieldPatch::Keep => {}
        FieldPatch::Set(value) => *target = Some(value),
        FieldPatch::Clear => *target = None,
    }
}

fn is_active(state: ContinuationState) -> bool {
    matches!(
        state,
        ContinuationState::Arming
            | ContinuationState::Waiting
            | ContinuationState::WakePending
            | ContinuationState::Resuming
    )
}
