//! Durable logical dispatch identity for delegation runs.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in [
            "ALTER TABLE delegation_task_runs ADD COLUMN dispatch_intent_id TEXT NULL",
            r#"CREATE TRIGGER trg_dtr_dispatch_intent_shape
               BEFORE INSERT ON delegation_task_runs
               WHEN NEW.dispatch_intent_id IS NOT NULL AND (
                 length(NEW.dispatch_intent_id) != 36
                 OR NEW.dispatch_intent_id != lower(NEW.dispatch_intent_id)
                 OR substr(NEW.dispatch_intent_id, 9, 1) != '-'
                 OR substr(NEW.dispatch_intent_id, 14, 1) != '-'
                 OR substr(NEW.dispatch_intent_id, 19, 1) != '-'
                 OR substr(NEW.dispatch_intent_id, 24, 1) != '-'
                 OR length(replace(NEW.dispatch_intent_id, '-', '')) != 32
                 OR replace(NEW.dispatch_intent_id, '-', '') GLOB '*[^0-9a-f]*'
               )
               BEGIN
                 SELECT RAISE(ABORT, 'trg_dtr_dispatch_intent_shape');
               END"#,
            r#"CREATE UNIQUE INDEX idx_dtr_parent_dispatch_intent
               ON delegation_task_runs(parent_conversation_id, dispatch_intent_id)
               WHERE dispatch_intent_id IS NOT NULL"#,
            r#"CREATE TRIGGER trg_dtr_dispatch_intent_immutable
               BEFORE UPDATE OF dispatch_intent_id ON delegation_task_runs
               WHEN OLD.dispatch_intent_id IS NOT NEW.dispatch_intent_id
               BEGIN
                 SELECT RAISE(ABORT, 'trg_dtr_dispatch_intent_immutable');
               END"#,
        ] {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in [
            "DROP INDEX IF EXISTS idx_dtr_parent_dispatch_intent",
            "DROP TRIGGER IF EXISTS trg_dtr_dispatch_intent_immutable",
            "DROP TRIGGER IF EXISTS trg_dtr_dispatch_intent_shape",
            "ALTER TABLE delegation_task_runs DROP COLUMN dispatch_intent_id",
        ] {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    use super::*;
    use crate::db::migration::Migrator;

    const PRIOR_MIGRATION: &str = "m20260819_000001_turn_generation_stat";
    const INTENT_MIGRATION: &str = "m20260827_000001_delegation_dispatch_intent";
    const INTENT_ID: &str = "8f95dd45-9eca-42a8-9909-0ac00be8ad52";

    fn sql(statement: impl Into<String>) -> Statement {
        Statement::from_string(DbBackend::Sqlite, statement.into())
    }

    async fn open_through_prior_migration() -> sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("open in-memory database");
        let migrations = Migrator::migrations();
        let prior_index = migrations
            .iter()
            .position(|migration| migration.name() == PRIOR_MIGRATION)
            .expect("prior migration registered");
        Migrator::up(&db, Some((prior_index + 1) as u32))
            .await
            .expect("apply prior migrations");
        db.execute_unprepared("PRAGMA foreign_keys=OFF")
            .await
            .expect("disable unrelated foreign-key setup for raw run fixtures");
        db
    }

    async fn open_with_intent_migration() -> sea_orm::DatabaseConnection {
        let db = open_through_prior_migration().await;
        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("apply dispatch intent migration");
        db
    }

    async fn insert_run(
        db: &sea_orm::DatabaseConnection,
        task_id: &str,
        parent_conversation_id: i32,
        child_conversation_id: i32,
        dispatch_intent_id: Option<&str>,
    ) -> Result<(), DbErr> {
        let intent = dispatch_intent_id
            .map(|value| format!(", '{value}'"))
            .unwrap_or_else(|| ", NULL".to_string());
        db.execute(sql(format!(
            "INSERT INTO delegation_task_runs (\
             task_id, root_task_id, generation, parent_conversation_id, \
             child_conversation_id, agent_type, admission_class, \
             lineage_root_task_id, history_only, status, created_at, updated_at, \
             dispatch_intent_id) VALUES (\
             '{task_id}', '{task_id}', 1, {parent_conversation_id}, \
             {child_conversation_id}, 'codex', 'normal_revision', '{task_id}', 0, \
             'completed', '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z'{intent})"
        )))
        .await
        .map(|_| ())
    }

    fn assert_trigger(error: DbErr, trigger_name: &str) {
        let message = error.to_string();
        assert!(
            message.contains(trigger_name),
            "expected {trigger_name} in SQLite error, got {message}"
        );
    }

    async fn child_column_metadata(db: &sea_orm::DatabaseConnection) -> (String, i64, i64) {
        let row = db
            .query_all(sql("PRAGMA table_info('delegation_task_runs')"))
            .await
            .expect("table info")
            .into_iter()
            .find(|row| {
                row.try_get::<String>("", "name").expect("column name") == "child_conversation_id"
            })
            .expect("child_conversation_id column");
        (
            row.try_get("", "type").expect("column type"),
            row.try_get("", "notnull").expect("column nullability"),
            row.try_get("", "pk").expect("column primary-key metadata"),
        )
    }

    async fn child_foreign_key_metadata(
        db: &sea_orm::DatabaseConnection,
    ) -> (String, String, String, String) {
        let row = db
            .query_all(sql("PRAGMA foreign_key_list('delegation_task_runs')"))
            .await
            .expect("foreign-key info")
            .into_iter()
            .find(|row| {
                row.try_get::<String>("", "from")
                    .expect("foreign-key source")
                    == "child_conversation_id"
            })
            .expect("child_conversation_id foreign key");
        (
            row.try_get("", "table").expect("foreign-key table"),
            row.try_get("", "to").expect("foreign-key target"),
            row.try_get("", "on_update")
                .expect("foreign-key update action"),
            row.try_get("", "on_delete")
                .expect("foreign-key delete action"),
        )
    }

    #[tokio::test]
    async fn delegation_dispatch_intent_migration_registers_after_current_latest() {
        let names = Migrator::migrations()
            .iter()
            .map(|migration| migration.name().to_string())
            .collect::<Vec<_>>();
        let prior_index = names
            .iter()
            .position(|name| name == PRIOR_MIGRATION)
            .expect("prior migration registered");
        assert_eq!(
            names.get(prior_index + 1).map(String::as_str),
            Some(INTENT_MIGRATION)
        );
        assert_eq!(prior_index + 2, names.len());
    }

    #[tokio::test]
    async fn delegation_dispatch_intent_migration_preserves_historical_null_and_child_fk() {
        let db = open_through_prior_migration().await;
        let child_column_before = child_column_metadata(&db).await;
        let child_fk_before = child_foreign_key_metadata(&db).await;
        db.execute(sql("INSERT INTO delegation_task_runs (\
             task_id, root_task_id, generation, parent_conversation_id, \
             child_conversation_id, agent_type, admission_class, \
             lineage_root_task_id, history_only, status, created_at, updated_at) VALUES (\
             'historical-run', 'historical-run', 1, 1, 10, 'codex', \
             'normal_revision', 'historical-run', 0, 'completed', \
             '2026-08-26T00:00:00Z', '2026-08-26T00:00:00Z')"))
            .await
            .expect("insert historical run");

        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("apply dispatch intent migration");

        let row = db
            .query_one(sql("SELECT dispatch_intent_id FROM delegation_task_runs \
                 WHERE task_id = 'historical-run'"))
            .await
            .expect("query historical run")
            .expect("historical run exists");
        assert_eq!(
            row.try_get::<Option<String>>("", "dispatch_intent_id")
                .expect("dispatch intent"),
            None
        );
        assert_eq!(child_column_metadata(&db).await, child_column_before);
        assert_eq!(child_foreign_key_metadata(&db).await, child_fk_before);
        assert_eq!(child_column_before, ("INTEGER".to_string(), 1, 0));
        assert_eq!(
            child_fk_before,
            (
                "conversation".to_string(),
                "id".to_string(),
                "NO ACTION".to_string(),
                "CASCADE".to_string(),
            )
        );
    }

    #[tokio::test]
    async fn delegation_dispatch_intent_migration_accepts_only_canonical_uuid_inserts() {
        let db = open_with_intent_migration().await;
        insert_run(&db, "valid", 1, 10, Some(INTENT_ID))
            .await
            .expect("canonical UUID accepted");
        insert_run(&db, "legacy-null", 1, 11, None)
            .await
            .expect("null intent accepted");

        for (suffix, invalid) in [
            ("length", "8f95dd45-9eca-42a8-9909-0ac00be8ad5"),
            ("uppercase", "8F95DD45-9ECA-42A8-9909-0AC00BE8AD52"),
            ("hyphens", "8f95dd459-eca-42a8-9909-0ac00be8ad52"),
            ("nonhex", "8f95dd45-9eca-42a8-9909-0ac00be8adz2"),
            ("extra-hyphen", "8f95dd45-9eca-42a8-9909-0ac00be8a-52"),
        ] {
            let error = insert_run(&db, suffix, 1, 20 + suffix.len() as i32, Some(invalid))
                .await
                .expect_err("noncanonical dispatch intent must fail");
            assert_trigger(error, "trg_dtr_dispatch_intent_shape");
        }
    }

    #[tokio::test]
    async fn delegation_dispatch_intent_migration_enforces_parent_scoped_partial_uniqueness() {
        let db = open_with_intent_migration().await;
        insert_run(&db, "parent-one", 1, 10, Some(INTENT_ID))
            .await
            .expect("first parent intent accepted");
        let duplicate = insert_run(&db, "parent-one-duplicate", 1, 11, Some(INTENT_ID))
            .await
            .expect_err("same parent intent must be unique");
        assert!(
            duplicate
                .to_string()
                .contains("delegation_task_runs.parent_conversation_id"),
            "unexpected duplicate error: {duplicate}"
        );
        insert_run(&db, "parent-two", 2, 12, Some(INTENT_ID))
            .await
            .expect("different parent may reuse intent");
        insert_run(&db, "legacy-null-one", 1, 13, None)
            .await
            .expect("first null intent accepted");
        insert_run(&db, "legacy-null-two", 1, 14, None)
            .await
            .expect("second null intent accepted");

        let index = db
            .query_one(sql("SELECT sql FROM sqlite_master WHERE type = 'index' \
                 AND name = 'idx_dtr_parent_dispatch_intent'"))
            .await
            .expect("query dispatch intent index")
            .expect("dispatch intent index exists");
        let definition: String = index.try_get("", "sql").expect("index SQL");
        assert!(definition.contains("WHERE dispatch_intent_id IS NOT NULL"));
    }

    #[tokio::test]
    async fn delegation_dispatch_intent_migration_rejects_every_intent_update() {
        let db = open_with_intent_migration().await;
        insert_run(&db, "bound", 1, 10, Some(INTENT_ID))
            .await
            .expect("bound run accepted");
        insert_run(&db, "unbound", 1, 11, None)
            .await
            .expect("unbound run accepted");

        for statement in [
            "UPDATE delegation_task_runs SET dispatch_intent_id = \
             'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa' WHERE task_id = 'bound'",
            "UPDATE delegation_task_runs SET dispatch_intent_id = NULL \
             WHERE task_id = 'bound'",
            "UPDATE delegation_task_runs SET dispatch_intent_id = \
             'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa' WHERE task_id = 'unbound'",
        ] {
            let error = db
                .execute(sql(statement))
                .await
                .expect_err("dispatch intent update must fail");
            assert_trigger(error, "trg_dtr_dispatch_intent_immutable");
        }

        db.execute(sql(
            "UPDATE delegation_task_runs SET status = 'failed' WHERE task_id = 'bound'",
        ))
        .await
        .expect("unrelated update succeeds");
    }
}
