use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const IDX_AUTO_TITLE_JOBS_QUEUE: &str = "idx_auto_title_jobs_queue";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Durable guard: once true, automatic title generation never runs again
        // for this conversation. Legacy rows default to false but have no job
        // row, so they remain ineligible until a job is explicitly created.
        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .add_column(
                        ColumnDef::new(Conversation::AutoTitleFinalized)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        // One job row per conversation while automatic titling is in flight.
        // State is one of the four durable queue states (never a terminal
        // "done" — terminal is represented by deleting the job and setting
        // auto_title_finalized).
        manager
            .create_table(
                Table::create()
                    .table(AutoTitleJobs::Table)
                    .col(
                        ColumnDef::new(AutoTitleJobs::ConversationId)
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AutoTitleJobs::State).string().not_null())
                    .col(
                        ColumnDef::new(AutoTitleJobs::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(AutoTitleJobs::FirstUserText).text().null())
                    .col(
                        ColumnDef::new(AutoTitleJobs::FirstAssistantText)
                            .text()
                            .null(),
                    )
                    .col(ColumnDef::new(AutoTitleJobs::Locale).string().null())
                    .col(
                        ColumnDef::new(AutoTitleJobs::UsableTurnSeq)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AutoTitleJobs::AttemptTurnSeq)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(AutoTitleJobs::LastUsableTurnToken)
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(AutoTitleJobs::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(AutoTitleJobs::Table, AutoTitleJobs::ConversationId)
                            .to(Conversation::Table, Conversation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .check(Expr::col(AutoTitleJobs::State).is_in([
                        "awaiting_turn",
                        "ready",
                        "running",
                        "retry_wait",
                    ]))
                    .check(Expr::col(AutoTitleJobs::Attempts).gte(0))
                    .check(Expr::col(AutoTitleJobs::Attempts).lte(2))
                    .check(Expr::col(AutoTitleJobs::UsableTurnSeq).gte(0))
                    .check(Expr::col(AutoTitleJobs::AttemptTurnSeq).gte(0))
                    .to_owned(),
            )
            .await?;

        // Queue poll order: state → updated_at → conversation_id.
        manager
            .create_index(
                Index::create()
                    .name(IDX_AUTO_TITLE_JOBS_QUEUE)
                    .table(AutoTitleJobs::Table)
                    .col(AutoTitleJobs::State)
                    .col(AutoTitleJobs::UpdatedAt)
                    .col(AutoTitleJobs::ConversationId)
                    .to_owned(),
            )
            .await?;

        // Registry of external agent sessions spawned solely for internal
        // purposes (currently only automatic title generation). Composite PK
        // prevents double-registration of the same external session.
        manager
            .create_table(
                Table::create()
                    .table(InternalAgentSessions::Table)
                    .col(
                        ColumnDef::new(InternalAgentSessions::AgentType)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(InternalAgentSessions::ExternalId)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(InternalAgentSessions::Purpose)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(InternalAgentSessions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(InternalAgentSessions::AgentType)
                            .col(InternalAgentSessions::ExternalId),
                    )
                    .check(Expr::col(InternalAgentSessions::Purpose).is_in(["title"]))
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Reverse dependency order: registry → jobs (index drops with table) →
        // conversation column.
        manager
            .drop_table(
                Table::drop()
                    .table(InternalAgentSessions::Table)
                    .to_owned(),
            )
            .await?;

        manager
            .drop_table(Table::drop().table(AutoTitleJobs::Table).to_owned())
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Conversation::Table)
                    .drop_column(Conversation::AutoTitleFinalized)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum Conversation {
    Table,
    Id,
    AutoTitleFinalized,
}

/// Plural enum name so DeriveIden maps `Table` → `auto_title_jobs`.
#[derive(DeriveIden)]
enum AutoTitleJobs {
    Table,
    ConversationId,
    State,
    Attempts,
    FirstUserText,
    FirstAssistantText,
    Locale,
    UsableTurnSeq,
    AttemptTurnSeq,
    LastUsableTurnToken,
    UpdatedAt,
}

/// Plural enum name so DeriveIden maps `Table` → `internal_agent_sessions`.
#[derive(DeriveIden)]
enum InternalAgentSessions {
    Table,
    AgentType,
    ExternalId,
    Purpose,
    CreatedAt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    async fn open_stub() -> sea_orm_migration::sea_orm::DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("database");
        conn.execute_unprepared("PRAGMA foreign_keys=ON")
            .await
            .expect("foreign keys");
        conn.execute_unprepared(
            "CREATE TABLE conversation (id INTEGER PRIMARY KEY, title_locked BOOLEAN NOT NULL DEFAULT 0)",
        )
        .await
        .expect("conversation table");
        conn.execute_unprepared("INSERT INTO conversation (id) VALUES (7)")
            .await
            .expect("legacy row");
        conn
    }

    fn has_column(
        columns: &[sea_orm_migration::sea_orm::QueryResult],
        name: &str,
    ) -> bool {
        columns.iter().any(|row| {
            row.try_get::<String>("", "name").ok().as_deref() == Some(name)
        })
    }

    #[tokio::test]
    async fn up_adds_guard_jobs_and_internal_session_registry() {
        let conn = open_stub().await;

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("migration");

        // Column + legacy default false; no job row for pre-existing rows.
        let columns = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(conversation)".to_owned(),
            ))
            .await
            .expect("columns");
        assert!(has_column(&columns, "auto_title_finalized"));
        let finalized: bool = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT auto_title_finalized FROM conversation WHERE id = 7".to_owned(),
            ))
            .await
            .expect("legacy guard query")
            .expect("legacy row")
            .try_get("", "auto_title_finalized")
            .expect("legacy guard");
        assert!(!finalized);
        let legacy_job_count: i64 = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM auto_title_jobs".to_owned(),
            ))
            .await
            .expect("job count query")
            .expect("job count row")
            .try_get("", "count")
            .expect("count");
        assert_eq!(legacy_job_count, 0);

        // Table definitions present with expected columns.
        let job_columns = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(auto_title_jobs)".to_owned(),
            ))
            .await
            .expect("job columns");
        for name in [
            "conversation_id",
            "state",
            "attempts",
            "first_user_text",
            "first_assistant_text",
            "locale",
            "usable_turn_seq",
            "attempt_turn_seq",
            "last_usable_turn_token",
            "updated_at",
        ] {
            assert!(
                has_column(&job_columns, name),
                "auto_title_jobs missing column {name}"
            );
        }

        let session_columns = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(internal_agent_sessions)".to_owned(),
            ))
            .await
            .expect("session columns");
        for name in ["agent_type", "external_id", "purpose", "created_at"] {
            assert!(
                has_column(&session_columns, name),
                "internal_agent_sessions missing column {name}"
            );
        }

        // Queue index on (state, updated_at, conversation_id).
        let indexes = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA index_list(auto_title_jobs)".to_owned(),
            ))
            .await
            .expect("index list");
        assert!(
            indexes.iter().any(|row| {
                row.try_get::<String>("", "name").ok().as_deref()
                    == Some(IDX_AUTO_TITLE_JOBS_QUEUE)
            }),
            "queue index missing"
        );
        let index_cols = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                format!("PRAGMA index_info({IDX_AUTO_TITLE_JOBS_QUEUE})"),
            ))
            .await
            .expect("index info");
        let col_names: Vec<String> = index_cols
            .iter()
            .map(|row| row.try_get::<String>("", "name").expect("index col"))
            .collect();
        assert_eq!(
            col_names,
            vec![
                "state".to_string(),
                "updated_at".to_string(),
                "conversation_id".to_string(),
            ]
        );

        // CHECK: reject state "done".
        let bad_state = conn
            .execute_unprepared(
                "INSERT INTO auto_title_jobs \
                 (conversation_id, state, attempts, usable_turn_seq, attempt_turn_seq, updated_at) \
                 VALUES (7, 'done', 0, 0, 0, '2026-01-01T00:00:00Z')",
            )
            .await;
        assert!(bad_state.is_err(), "state 'done' must be rejected");

        // CHECK: reject attempts = 3.
        let bad_attempts = conn
            .execute_unprepared(
                "INSERT INTO auto_title_jobs \
                 (conversation_id, state, attempts, usable_turn_seq, attempt_turn_seq, updated_at) \
                 VALUES (7, 'ready', 3, 0, 0, '2026-01-01T00:00:00Z')",
            )
            .await;
        assert!(bad_attempts.is_err(), "attempts=3 must be rejected");

        // CHECK: reject negative sequence values.
        let bad_usable = conn
            .execute_unprepared(
                "INSERT INTO auto_title_jobs \
                 (conversation_id, state, attempts, usable_turn_seq, attempt_turn_seq, updated_at) \
                 VALUES (7, 'ready', 0, -1, 0, '2026-01-01T00:00:00Z')",
            )
            .await;
        assert!(bad_usable.is_err(), "negative usable_turn_seq must be rejected");
        let bad_attempt_seq = conn
            .execute_unprepared(
                "INSERT INTO auto_title_jobs \
                 (conversation_id, state, attempts, usable_turn_seq, attempt_turn_seq, updated_at) \
                 VALUES (7, 'ready', 0, 0, -1, '2026-01-01T00:00:00Z')",
            )
            .await;
        assert!(
            bad_attempt_seq.is_err(),
            "negative attempt_turn_seq must be rejected"
        );

        // Valid job for cascade + composite-key tests.
        conn.execute_unprepared(
            "INSERT INTO auto_title_jobs \
             (conversation_id, state, attempts, usable_turn_seq, attempt_turn_seq, updated_at) \
             VALUES (7, 'awaiting_turn', 0, 0, 0, '2026-01-01T00:00:00Z')",
        )
        .await
        .expect("valid job insert");

        // Composite PK rejects duplicate (agent_type, external_id).
        conn.execute_unprepared(
            "INSERT INTO internal_agent_sessions \
             (agent_type, external_id, purpose, created_at) \
             VALUES ('claude_code', 'ext-1', 'title', '2026-01-01T00:00:00Z')",
        )
        .await
        .expect("session insert");
        let dup = conn
            .execute_unprepared(
                "INSERT INTO internal_agent_sessions \
                 (agent_type, external_id, purpose, created_at) \
                 VALUES ('claude_code', 'ext-1', 'title', '2026-01-02T00:00:00Z')",
            )
            .await;
        assert!(dup.is_err(), "duplicate composite key must be rejected");

        // Deleting conversation cascades its job.
        conn.execute_unprepared("DELETE FROM conversation WHERE id = 7")
            .await
            .expect("delete conversation");
        let after_cascade: i64 = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM auto_title_jobs".to_owned(),
            ))
            .await
            .expect("cascade count query")
            .expect("cascade count row")
            .try_get("", "count")
            .expect("count");
        assert_eq!(after_cascade, 0, "job must cascade-delete with conversation");

        // down removes all three schema additions.
        // Re-insert conversation so down only needs to drop schema objects
        // (conversation table itself stays as the stub).
        conn.execute_unprepared("INSERT INTO conversation (id) VALUES (7)")
            .await
            .expect("reinsert conversation for down");
        Migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("migration down");

        let columns_after = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(conversation)".to_owned(),
            ))
            .await
            .expect("columns after down");
        assert!(
            !has_column(&columns_after, "auto_title_finalized"),
            "auto_title_finalized must be dropped"
        );
        let jobs_gone = conn
            .execute_unprepared("SELECT 1 FROM auto_title_jobs")
            .await;
        assert!(jobs_gone.is_err(), "auto_title_jobs must be dropped");
        let sessions_gone = conn
            .execute_unprepared("SELECT 1 FROM internal_agent_sessions")
            .await;
        assert!(
            sessions_gone.is_err(),
            "internal_agent_sessions must be dropped"
        );
    }
}
