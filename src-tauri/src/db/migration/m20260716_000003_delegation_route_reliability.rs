use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Five nullable durable-delegation columns. CHECK constraints reject
        // unknown stored strings when SQLite enforces them (production enables
        // foreign_keys; CHECKs apply on write).
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE conversation ADD COLUMN delegation_route_override TEXT \
                 CHECK (delegation_route_override IS NULL \
                        OR delegation_route_override IN ('codeg', 'native'))",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE conversation ADD COLUMN delegation_task_status TEXT \
                 CHECK (delegation_task_status IS NULL \
                        OR delegation_task_status IN ('running', 'completed', 'failed', 'canceled'))",
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conversation ADD COLUMN delegation_error_code TEXT")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conversation ADD COLUMN delegation_started_at TEXT")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conversation ADD COLUMN delegation_finished_at TEXT")
            .await?;

        // Semantic backfill: only legacy `kind = 'delegate'` rows receive a
        // durable task status. Regular/chat/loop rows stay null so cold-load
        // never confuses ConversationStatus with task lifecycle.
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE conversation \
                 SET delegation_task_status = CASE status \
                   WHEN 'in_progress' THEN 'running' \
                   WHEN 'pending_review' THEN 'completed' \
                   WHEN 'completed' THEN 'completed' \
                   WHEN 'cancelled' THEN 'canceled' \
                 END, \
                 delegation_started_at = created_at, \
                 delegation_finished_at = CASE \
                   WHEN status = 'in_progress' THEN NULL \
                   ELSE updated_at \
                 END \
                 WHERE kind = 'delegate'",
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Additive-safe: drop only the five columns this migration added.
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conversation DROP COLUMN delegation_route_override")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conversation DROP COLUMN delegation_task_status")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conversation DROP COLUMN delegation_error_code")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conversation DROP COLUMN delegation_started_at")
            .await?;
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE conversation DROP COLUMN delegation_finished_at")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::MigratorTrait;

    use crate::db::migration::Migrator;

    fn sql(s: &str) -> Statement {
        Statement::from_string(DbBackend::Sqlite, s.to_owned())
    }

    /// Seed a pre-target-migration conversation row via raw SQL. Must not use
    /// production ActiveModel helpers that already require the new columns.
    async fn seed_legacy_conversation(
        conn: &sea_orm::DatabaseConnection,
        id: i32,
        kind: &str,
        status: &str,
        parent_id: Option<i32>,
    ) {
        let parent = match parent_id {
            Some(p) => p.to_string(),
            None => "NULL".to_string(),
        };
        // folder id 1 is inserted by the backfill test before seeding rows.
        conn.execute(sql(&format!(
            "INSERT INTO conversation \
             (id, folder_id, agent_type, status, kind, message_count, title_locked, \
              created_at, updated_at, parent_id) VALUES \
             ({id}, 1, 'claude_code', '{status}', '{kind}', 0, 0, \
              '2026-01-01 00:00:00', '2026-01-02 00:00:00', {parent})"
        )))
        .await
        .expect("seed legacy conversation");
    }

    async fn task_status(conn: &sea_orm::DatabaseConnection, id: i32) -> Option<String> {
        let row = conn
            .query_one(sql(&format!(
                "SELECT delegation_task_status AS s FROM conversation WHERE id = {id}"
            )))
            .await
            .expect("query")
            .expect("row");
        row.try_get::<Option<String>>("", "s").expect("column")
    }

    #[tokio::test]
    async fn backfills_only_legacy_delegate_task_status() {
        let conn = Database::connect("sqlite::memory:").await.unwrap();
        let migrations = <Migrator as MigratorTrait>::migrations();
        let idx = migrations
            .iter()
            .position(|m| m.name().contains("delegation_route_reliability"))
            .expect("migration registered");
        Migrator::up(&conn, Some(idx as u32)).await.unwrap();

        conn.execute(sql(
            "INSERT INTO folder \
             (id, name, path, last_opened_at, created_at, updated_at, is_open, sort_order, color, kind) \
             VALUES (1, 'repo', '/tmp/route-rel', '2026-01-01 00:00:00', '2026-01-01 00:00:00', \
                     '2026-01-01 00:00:00', 1, 1, 'inherit', 'regular')",
        ))
        .await
        .expect("seed folder");

        seed_legacy_conversation(&conn, 1, "regular", "completed", None).await;
        seed_legacy_conversation(&conn, 2, "delegate", "in_progress", Some(1)).await;
        seed_legacy_conversation(&conn, 3, "delegate", "pending_review", Some(1)).await;
        seed_legacy_conversation(&conn, 4, "delegate", "completed", Some(1)).await;
        seed_legacy_conversation(&conn, 5, "delegate", "cancelled", Some(1)).await;

        Migrator::up(&conn, None).await.unwrap();
        assert_eq!(task_status(&conn, 1).await, None);
        assert_eq!(task_status(&conn, 2).await.as_deref(), Some("running"));
        assert_eq!(task_status(&conn, 3).await.as_deref(), Some("completed"));
        assert_eq!(task_status(&conn, 4).await.as_deref(), Some("completed"));
        assert_eq!(task_status(&conn, 5).await.as_deref(), Some("canceled"));
    }
}
