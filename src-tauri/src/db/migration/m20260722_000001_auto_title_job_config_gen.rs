use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DbBackend, Statement};

/// Add `config_gen` to bind auto-title jobs to a title-config epoch.
///
/// Legacy ACP-era job rows must not survive into the API-title world. The
/// migration deletes all existing jobs before adding the NOT NULL column so
/// nonempty legacy tables upgrade safely. Runtime also runs a one-shot purge
/// (`auto_title_jobs_purged_for_api_v1`) before recover for DBs that already
/// upgraded without the recovery flag.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // Prefer delete-all then NOT NULL DEFAULT 0 over nullable backfill so
        // leftover ACP-era rows never remain claimable after upgrade.
        conn.execute_unprepared("DELETE FROM auto_title_jobs")
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(AutoTitleJobs::Table)
                    .add_column(
                        ColumnDef::new(AutoTitleJobs::ConfigGen)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;

        // Mark the one-shot runtime purge complete when `app_metadata` exists
        // (full Migrator path). Stub unit tests without that table still pass:
        // runtime purge remains the authoritative backup when the flag is missing.
        let has_app_metadata = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT 1 AS ok FROM sqlite_master \
                 WHERE type = 'table' AND name = 'app_metadata'"
                    .to_owned(),
            ))
            .await?
            .is_some();
        if has_app_metadata {
            // Soft-delete aware upsert: match app_metadata_service::upsert_value.
            conn.execute_unprepared(
                "INSERT INTO app_metadata (key, value, created_at, updated_at) \
                 VALUES ( \
                   'conversation_experience.auto_title_jobs_purged_for_api_v1', \
                   '1', \
                   datetime('now'), \
                   datetime('now') \
                 ) \
                 ON CONFLICT(key) DO UPDATE SET \
                   value = '1', \
                   updated_at = datetime('now'), \
                   deleted_at = NULL",
            )
            .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(AutoTitleJobs::Table)
                    .drop_column(AutoTitleJobs::ConfigGen)
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
    ConfigGen,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use crate::db::migration::Migrator;

    async fn open_legacy_jobs_stub() -> sea_orm_migration::sea_orm::DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("database");
        conn.execute_unprepared("PRAGMA foreign_keys=ON")
            .await
            .expect("foreign keys");
        conn.execute_unprepared(
            "CREATE TABLE conversation (
                id INTEGER PRIMARY KEY,
                auto_title_finalized BOOLEAN NOT NULL DEFAULT 0
            )",
        )
        .await
        .expect("conversation table");
        // Pre-config_gen schema (includes first_prompt_at from prior migration).
        conn.execute_unprepared(
            "CREATE TABLE auto_title_jobs (
                conversation_id INTEGER PRIMARY KEY NOT NULL,
                state TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                first_user_text TEXT NULL,
                first_assistant_text TEXT NULL,
                first_prompt_at TEXT NULL,
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

        for (id, state) in [
            (1, "awaiting_turn"),
            (2, "ready"),
            (3, "running"),
            (4, "retry_wait"),
        ] {
            conn.execute_unprepared(&format!(
                "INSERT INTO conversation (id) VALUES ({id})"
            ))
            .await
            .expect("conversation row");
            conn.execute_unprepared(&format!(
                "INSERT INTO auto_title_jobs \
                 (conversation_id, state, attempts, first_user_text, usable_turn_seq, \
                  attempt_turn_seq, updated_at) \
                 VALUES ({id}, '{state}', 0, 'legacy task', 0, 0, '2026-01-01T00:00:00Z')"
            ))
            .await
            .expect("legacy job");
        }
        conn
    }

    fn has_column(columns: &[sea_orm_migration::sea_orm::QueryResult], name: &str) -> bool {
        columns
            .iter()
            .any(|row| row.try_get::<String>("", "name").ok().as_deref() == Some(name))
    }

    async fn job_count(conn: &sea_orm_migration::sea_orm::DatabaseConnection) -> i64 {
        conn.query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count FROM auto_title_jobs".to_owned(),
        ))
        .await
        .expect("count query")
        .expect("count row")
        .try_get("", "count")
        .expect("count")
    }

    #[tokio::test]
    async fn up_deletes_all_legacy_states_and_adds_config_gen() {
        let conn = open_legacy_jobs_stub().await;
        assert_eq!(job_count(&conn).await, 4, "fixture must seed four states");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("migration up");

        assert_eq!(
            job_count(&conn).await,
            0,
            "migration must purge every legacy job state"
        );

        let columns = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(auto_title_jobs)".to_owned(),
            ))
            .await
            .expect("job columns");
        assert!(
            has_column(&columns, "config_gen"),
            "config_gen column must exist after up"
        );

        // New inserts get config_gen default 0.
        conn.execute_unprepared(
            "INSERT INTO auto_title_jobs \
             (conversation_id, state, attempts, usable_turn_seq, attempt_turn_seq, updated_at) \
             VALUES (1, 'ready', 0, 0, 0, '2026-01-01T00:00:00Z')",
        )
        .await
        .expect("insert after migration");
        let gen: i64 = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT config_gen FROM auto_title_jobs WHERE conversation_id = 1".to_owned(),
            ))
            .await
            .expect("gen query")
            .expect("gen row")
            .try_get("", "config_gen")
            .expect("config_gen");
        assert_eq!(gen, 0);

        Migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("migration down");

        let columns_after = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(auto_title_jobs)".to_owned(),
            ))
            .await
            .expect("columns after down");
        assert!(
            !has_column(&columns_after, "config_gen"),
            "config_gen must be dropped on down"
        );
    }

    #[tokio::test]
    async fn migrator_registers_config_gen_migration() {
        let migrations = Migrator::migrations();
        assert!(
            migrations
                .iter()
                .any(|m| m.name() == "m20260722_000001_auto_title_job_config_gen"),
            "config_gen migration must be registered in Migrator"
        );
    }

    #[tokio::test]
    async fn up_on_empty_table_still_adds_config_gen() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("database");
        conn.execute_unprepared(
            "CREATE TABLE conversation (
                id INTEGER PRIMARY KEY,
                auto_title_finalized BOOLEAN NOT NULL DEFAULT 0
            )",
        )
        .await
        .expect("conversation");
        conn.execute_unprepared(
            "CREATE TABLE auto_title_jobs (
                conversation_id INTEGER PRIMARY KEY NOT NULL,
                state TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                first_user_text TEXT NULL,
                first_assistant_text TEXT NULL,
                first_prompt_at TEXT NULL,
                locale TEXT NULL,
                usable_turn_seq INTEGER NOT NULL DEFAULT 0,
                attempt_turn_seq INTEGER NOT NULL DEFAULT 0,
                last_usable_turn_token TEXT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .await
        .expect("jobs");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("migration up on empty");

        assert_eq!(job_count(&conn).await, 0);
        let columns = conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info(auto_title_jobs)".to_owned(),
            ))
            .await
            .expect("columns");
        assert!(has_column(&columns, "config_gen"));
    }
}
