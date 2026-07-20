use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const IDX_AUTO_TITLE_JOBS_DEADLINE: &str = "idx_auto_title_jobs_deadline";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AutoTitleJobs::Table)
                    .add_column(
                        ColumnDef::new(AutoTitleJobs::FirstPromptAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name(IDX_AUTO_TITLE_JOBS_DEADLINE)
                    .table(AutoTitleJobs::Table)
                    .col(AutoTitleJobs::State)
                    .col(AutoTitleJobs::FirstPromptAt)
                    .col(AutoTitleJobs::ConversationId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .name(IDX_AUTO_TITLE_JOBS_DEADLINE)
                    .table(AutoTitleJobs::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(AutoTitleJobs::Table)
                    .drop_column(AutoTitleJobs::FirstPromptAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

/// Plural enum name so DeriveIden maps `Table` → `auto_title_jobs`.
#[derive(DeriveIden)]
enum AutoTitleJobs {
    Table,
    ConversationId,
    State,
    FirstPromptAt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use crate::db::migration::Migrator;

    const IDX_AUTO_TITLE_JOBS_QUEUE: &str = "idx_auto_title_jobs_queue";

    async fn open_stub() -> sea_orm_migration::sea_orm::DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("database");
        conn.execute_unprepared("PRAGMA foreign_keys=ON")
            .await
            .expect("foreign keys");
        // Minimal conversation + pre-migration auto_title_jobs (no first_prompt_at).
        conn.execute_unprepared(
            "CREATE TABLE conversation (
                id INTEGER PRIMARY KEY,
                auto_title_finalized BOOLEAN NOT NULL DEFAULT 0
            )",
        )
        .await
        .expect("conversation table");
        conn.execute_unprepared(
            "CREATE TABLE auto_title_jobs (
                conversation_id INTEGER PRIMARY KEY NOT NULL,
                state TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                first_user_text TEXT NULL,
                first_assistant_text TEXT NULL,
                locale TEXT NULL,
                usable_turn_seq INTEGER NOT NULL DEFAULT 0,
                attempt_turn_seq INTEGER NOT NULL DEFAULT 0,
                last_usable_turn_token TEXT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY(conversation_id) REFERENCES conversation(id) ON DELETE CASCADE
            )",
        )
        .await
        .expect("auto_title_jobs table");
        conn.execute_unprepared(
            "CREATE INDEX idx_auto_title_jobs_queue
             ON auto_title_jobs(state, updated_at, conversation_id)",
        )
        .await
        .expect("queue index");
        conn.execute_unprepared("INSERT INTO conversation (id) VALUES (7)")
            .await
            .expect("conversation row");
        conn.execute_unprepared(
            "INSERT INTO auto_title_jobs \
             (conversation_id, state, attempts, first_user_text, usable_turn_seq, \
              attempt_turn_seq, updated_at) \
             VALUES (7, 'awaiting_turn', 0, 'old task', 0, 0, '2026-01-01T00:00:00Z')",
        )
        .await
        .expect("legacy job with captured prompt");
        conn
    }

    fn has_column(columns: &[sea_orm_migration::sea_orm::QueryResult], name: &str) -> bool {
        columns
            .iter()
            .any(|row| row.try_get::<String>("", "name").ok().as_deref() == Some(name))
    }

    fn has_index(indexes: &[sea_orm_migration::sea_orm::QueryResult], name: &str) -> bool {
        indexes
            .iter()
            .any(|row| row.try_get::<String>("", "name").ok().as_deref() == Some(name))
    }

    async fn job_columns(
        conn: &sea_orm_migration::sea_orm::DatabaseConnection,
    ) -> Vec<sea_orm_migration::sea_orm::QueryResult> {
        conn.query_all(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA table_info(auto_title_jobs)".to_owned(),
        ))
        .await
        .expect("job columns")
    }

    async fn job_indexes(
        conn: &sea_orm_migration::sea_orm::DatabaseConnection,
    ) -> Vec<sea_orm_migration::sea_orm::QueryResult> {
        conn.query_all(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA index_list(auto_title_jobs)".to_owned(),
        ))
        .await
        .expect("index list")
    }

    #[tokio::test]
    async fn up_adds_first_prompt_at_and_deadline_index() {
        let conn = open_stub().await;

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("migration up");

        let columns = job_columns(&conn).await;
        assert!(
            has_column(&columns, "first_prompt_at"),
            "first_prompt_at column must be present after up"
        );

        // Legacy row with pre-existing first_user_text stays NULL on first_prompt_at.
        let first_prompt_at: Option<String> = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT first_prompt_at FROM auto_title_jobs WHERE conversation_id = 7".to_owned(),
            ))
            .await
            .expect("legacy row query")
            .expect("legacy row")
            .try_get("", "first_prompt_at")
            .expect("first_prompt_at");
        assert!(
            first_prompt_at.is_none(),
            "legacy captured prompt must keep NULL first_prompt_at"
        );

        let indexes = job_indexes(&conn).await;
        assert!(
            has_index(&indexes, IDX_AUTO_TITLE_JOBS_QUEUE),
            "queue index must be retained"
        );
        assert!(
            has_index(&indexes, IDX_AUTO_TITLE_JOBS_DEADLINE),
            "deadline index must be present after up"
        );

        let index_cols = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                format!("PRAGMA index_info({IDX_AUTO_TITLE_JOBS_DEADLINE})"),
            ))
            .await
            .expect("deadline index info");
        let col_names: Vec<String> = index_cols
            .iter()
            .map(|row| row.try_get::<String>("", "name").expect("index col"))
            .collect();
        assert_eq!(
            col_names,
            vec![
                "state".to_string(),
                "first_prompt_at".to_string(),
                "conversation_id".to_string(),
            ]
        );

        Migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("migration down");

        let columns_after = job_columns(&conn).await;
        assert!(
            !has_column(&columns_after, "first_prompt_at"),
            "first_prompt_at must be dropped on down"
        );

        let indexes_after = job_indexes(&conn).await;
        assert!(
            has_index(&indexes_after, IDX_AUTO_TITLE_JOBS_QUEUE),
            "queue index must survive down"
        );
        assert!(
            !has_index(&indexes_after, IDX_AUTO_TITLE_JOBS_DEADLINE),
            "deadline index must be dropped on down"
        );
    }

    #[tokio::test]
    async fn migrator_registers_deadline_migration() {
        let migrations = Migrator::migrations();
        assert!(
            migrations
                .iter()
                .any(|m| m.name() == "m20260719_000002_auto_title_first_prompt_at"),
            "deadline migration must be registered in Migrator"
        );
    }

    #[tokio::test]
    async fn legacy_captured_prompt_keeps_null_first_prompt_at_after_upgrade() {
        let conn = open_stub().await;

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("migration up");

        let row = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT first_user_text, first_prompt_at \
                 FROM auto_title_jobs WHERE conversation_id = 7"
                    .to_owned(),
            ))
            .await
            .expect("legacy query")
            .expect("legacy row");
        let first_user_text: Option<String> =
            row.try_get("", "first_user_text").expect("first_user_text");
        let first_prompt_at: Option<String> =
            row.try_get("", "first_prompt_at").expect("first_prompt_at");

        assert_eq!(first_user_text.as_deref(), Some("old task"));
        assert!(
            first_prompt_at.is_none(),
            "migration must not backfill first_prompt_at for pre-captured prompts; \
             later capture path must not backfill either when first_user already set"
        );
    }
}
