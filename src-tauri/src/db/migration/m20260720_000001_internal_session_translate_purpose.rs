use sea_orm_migration::prelude::*;

/// Rebuild `internal_agent_sessions` so `purpose` CHECK allows
/// `title | translate` while preserving existing title rows.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // SQLite cannot ALTER a CHECK constraint in place — rebuild the table.
        conn.execute_unprepared(
            "CREATE TABLE internal_agent_sessions_new (\
               agent_type TEXT NOT NULL, \
               external_id TEXT NOT NULL, \
               purpose TEXT NOT NULL CHECK (purpose IN ('title', 'translate')), \
               created_at TEXT NOT NULL, \
               PRIMARY KEY (agent_type, external_id)\
             )",
        )
        .await?;
        conn.execute_unprepared(
            "INSERT INTO internal_agent_sessions_new \
               (agent_type, external_id, purpose, created_at) \
             SELECT agent_type, external_id, purpose, created_at \
             FROM internal_agent_sessions",
        )
        .await?;
        conn.execute_unprepared("DROP TABLE internal_agent_sessions")
            .await?;
        conn.execute_unprepared(
            "ALTER TABLE internal_agent_sessions_new RENAME TO internal_agent_sessions",
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // Title-only CHECK: drop translate rows before rebuild.
        conn.execute_unprepared(
            "CREATE TABLE internal_agent_sessions_new (\
               agent_type TEXT NOT NULL, \
               external_id TEXT NOT NULL, \
               purpose TEXT NOT NULL CHECK (purpose IN ('title')), \
               created_at TEXT NOT NULL, \
               PRIMARY KEY (agent_type, external_id)\
             )",
        )
        .await?;
        conn.execute_unprepared(
            "INSERT INTO internal_agent_sessions_new \
               (agent_type, external_id, purpose, created_at) \
             SELECT agent_type, external_id, purpose, created_at \
             FROM internal_agent_sessions \
             WHERE purpose = 'title'",
        )
        .await?;
        conn.execute_unprepared("DROP TABLE internal_agent_sessions")
            .await?;
        conn.execute_unprepared(
            "ALTER TABLE internal_agent_sessions_new RENAME TO internal_agent_sessions",
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    async fn open_title_only_table() -> sea_orm_migration::sea_orm::DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("database");
        conn.execute_unprepared(
            "CREATE TABLE internal_agent_sessions (\
               agent_type TEXT NOT NULL, \
               external_id TEXT NOT NULL, \
               purpose TEXT NOT NULL CHECK (purpose IN ('title')), \
               created_at TEXT NOT NULL, \
               PRIMARY KEY (agent_type, external_id)\
             )",
        )
        .await
        .expect("title-only table");
        conn
    }

    #[tokio::test]
    async fn up_preserves_title_rows_and_allows_translate() {
        let conn = open_title_only_table().await;
        conn.execute_unprepared(
            "INSERT INTO internal_agent_sessions \
               (agent_type, external_id, purpose, created_at) \
             VALUES ('claude_code', 'ext-title', 'title', '2026-01-01T00:00:00Z')",
        )
        .await
        .expect("seed title row");

        // Pre-migration: translate purpose rejected by CHECK.
        let pre = conn
            .execute_unprepared(
                "INSERT INTO internal_agent_sessions \
                   (agent_type, external_id, purpose, created_at) \
                 VALUES ('claude_code', 'ext-pre', 'translate', '2026-01-01T00:00:00Z')",
            )
            .await;
        assert!(pre.is_err(), "title-only CHECK must reject translate");

        Migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("migration up");

        let preserved: i64 = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM internal_agent_sessions \
                 WHERE external_id = 'ext-title' AND purpose = 'title'"
                    .to_owned(),
            ))
            .await
            .expect("query")
            .expect("row")
            .try_get("", "count")
            .expect("count");
        assert_eq!(preserved, 1, "existing title row must survive rebuild");

        conn.execute_unprepared(
            "INSERT INTO internal_agent_sessions \
               (agent_type, external_id, purpose, created_at) \
             VALUES ('codex', 'ext-tr', 'translate', '2026-01-02T00:00:00Z')",
        )
        .await
        .expect("translate insert must succeed after migration");

        let garbage = conn
            .execute_unprepared(
                "INSERT INTO internal_agent_sessions \
                   (agent_type, external_id, purpose, created_at) \
                 VALUES ('codex', 'ext-bad', 'garbage', '2026-01-03T00:00:00Z')",
            )
            .await;
        assert!(garbage.is_err(), "garbage purpose must still be rejected");

        // down drops translate rows and restores title-only CHECK.
        Migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("migration down");

        let after_down: i64 = conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM internal_agent_sessions".to_owned(),
            ))
            .await
            .expect("count query")
            .expect("count row")
            .try_get("", "count")
            .expect("count");
        assert_eq!(after_down, 1, "only title rows remain after down");

        let translate_blocked = conn
            .execute_unprepared(
                "INSERT INTO internal_agent_sessions \
                   (agent_type, external_id, purpose, created_at) \
                 VALUES ('codex', 'ext-tr2', 'translate', '2026-01-04T00:00:00Z')",
            )
            .await;
        assert!(
            translate_blocked.is_err(),
            "down must restore title-only CHECK"
        );
    }
}
