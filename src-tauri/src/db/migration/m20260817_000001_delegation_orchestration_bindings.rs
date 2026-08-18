//! Immutable optional orchestration identity for generic delegation runs.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in [
            "ALTER TABLE delegation_task_runs ADD COLUMN orchestration_schema_version INTEGER NULL",
            "ALTER TABLE delegation_task_runs ADD COLUMN orchestration_namespace TEXT NULL",
            "ALTER TABLE delegation_task_runs ADD COLUMN orchestration_generation INTEGER NULL",
            "ALTER TABLE delegation_task_runs ADD COLUMN orchestration_route_fingerprint TEXT NULL",
            r#"CREATE TRIGGER trg_dtr_orchestration_binding_shape
               BEFORE INSERT ON delegation_task_runs
               WHEN (NEW.orchestration_schema_version IS NOT NULL) +
                    (NEW.orchestration_namespace IS NOT NULL) +
                    (NEW.orchestration_generation IS NOT NULL) +
                    (NEW.orchestration_route_fingerprint IS NOT NULL) NOT IN (0, 4)
               BEGIN
                 SELECT RAISE(ABORT, 'trg_dtr_orchestration_binding_shape');
               END"#,
            r#"CREATE TRIGGER trg_dtr_orchestration_binding_immutable
               BEFORE UPDATE OF orchestration_schema_version,
                                orchestration_namespace,
                                orchestration_generation,
                                orchestration_route_fingerprint
               ON delegation_task_runs
               WHEN OLD.orchestration_schema_version IS NOT NEW.orchestration_schema_version
                 OR OLD.orchestration_namespace IS NOT NEW.orchestration_namespace
                 OR OLD.orchestration_generation IS NOT NEW.orchestration_generation
                 OR OLD.orchestration_route_fingerprint IS NOT NEW.orchestration_route_fingerprint
               BEGIN
                 SELECT RAISE(ABORT, 'trg_dtr_orchestration_binding_immutable');
               END"#,
            r#"CREATE INDEX idx_dtr_parent_orchestration_created_task
               ON delegation_task_runs(
                 parent_conversation_id,
                 orchestration_namespace,
                 created_at,
                 task_id
               )"#,
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
            "DROP INDEX IF EXISTS idx_dtr_parent_orchestration_created_task",
            "DROP TRIGGER IF EXISTS trg_dtr_orchestration_binding_immutable",
            "DROP TRIGGER IF EXISTS trg_dtr_orchestration_binding_shape",
            "ALTER TABLE delegation_task_runs DROP COLUMN orchestration_route_fingerprint",
            "ALTER TABLE delegation_task_runs DROP COLUMN orchestration_generation",
            "ALTER TABLE delegation_task_runs DROP COLUMN orchestration_namespace",
            "ALTER TABLE delegation_task_runs DROP COLUMN orchestration_schema_version",
        ] {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }
}

/// Historical completion-protocol tests intentionally hold the schema before
/// the v2-only migration while exercising run-store writes. Install this later
/// independent migration out of order and record it so the normal migrator
/// does not apply it twice when those fixtures advance.
#[cfg(test)]
pub(crate) async fn install_for_historical_completion_fixture(
    db: &crate::db::AppDatabase,
) -> Result<(), DbErr> {
    use std::time::{SystemTime, UNIX_EPOCH};

    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let manager = SchemaManager::new(&db.conn);
    if !manager
        .has_column(
            "delegation_task_runs",
            "orchestration_schema_version",
        )
        .await?
    {
        Migration.up(&manager).await?;
    }

    let applied_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after Unix epoch")
        .as_secs() as i64;
    db.conn
        .execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT OR IGNORE INTO seaql_migrations (version, applied_at) VALUES (?, ?)",
            vec![
                "m20260817_000001_delegation_orchestration_bindings".into(),
                applied_at.into(),
            ],
        ))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    use super::*;
    use crate::db::migration::Migrator;

    const PRIOR_MIGRATION: &str = "m20260811_000001_simple_workflows";
    const BINDING_MIGRATION: &str =
        "m20260817_000001_delegation_orchestration_bindings";
    const ZERO_FINGERPRINT: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

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

    async fn open_with_binding_migration() -> sea_orm::DatabaseConnection {
        let db = open_through_prior_migration().await;
        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("apply orchestration binding migration");
        db
    }

    async fn insert_run(
        db: &sea_orm::DatabaseConnection,
        task_id: &str,
        child_conversation_id: i32,
        binding_columns: &str,
        binding_values: &str,
    ) -> Result<(), DbErr> {
        let columns = if binding_columns.is_empty() {
            String::new()
        } else {
            format!(", {binding_columns}")
        };
        let values = if binding_values.is_empty() {
            String::new()
        } else {
            format!(", {binding_values}")
        };
        db.execute(sql(format!(
            "INSERT INTO delegation_task_runs (\
             task_id, root_task_id, generation, parent_conversation_id, \
             child_conversation_id, agent_type, admission_class, \
             lineage_root_task_id, history_only, status, created_at, updated_at\
             {columns}) VALUES (\
             '{task_id}', '{task_id}', 1, 1, {child_conversation_id}, 'codex', 'normal_revision', \
             '{task_id}', 0, 'reserving', '2026-08-17T00:00:00Z', \
             '2026-08-17T00:00:00Z'{values})"
        )))
        .await
        .map(|_| ())
    }

    async fn binding_values(
        db: &sea_orm::DatabaseConnection,
        task_id: &str,
    ) -> (Option<i64>, Option<String>, Option<i64>, Option<String>) {
        let row = db
            .query_one(sql(format!(
                "SELECT orchestration_schema_version, orchestration_namespace, \
                 orchestration_generation, orchestration_route_fingerprint \
                 FROM delegation_task_runs WHERE task_id = '{task_id}'"
            )))
            .await
            .expect("query binding")
            .expect("run exists");
        (
            row.try_get("", "orchestration_schema_version")
                .expect("schema version"),
            row.try_get("", "orchestration_namespace")
                .expect("namespace"),
            row.try_get("", "orchestration_generation")
                .expect("generation"),
            row.try_get("", "orchestration_route_fingerprint")
                .expect("route fingerprint"),
        )
    }

    fn assert_trigger(error: DbErr, trigger_name: &str) {
        let message = error.to_string();
        assert!(
            message.contains(trigger_name),
            "expected {trigger_name} in SQLite error, got {message}"
        );
    }

    #[tokio::test]
    async fn delegation_orchestration_bindings_preserve_legacy_rows_without_backfill() {
        let db = open_through_prior_migration().await;
        insert_run(&db, "legacy-run", 1, "", "")
            .await
            .expect("insert legacy run before migration");

        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("apply orchestration binding migration");

        assert_eq!(
            binding_values(&db, "legacy-run").await,
            (None, None, None, None)
        );
    }

    #[tokio::test]
    async fn delegation_orchestration_bindings_register_after_simple_workflows() {
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
            Some(BINDING_MIGRATION)
        );
    }

    #[tokio::test]
    async fn delegation_orchestration_bindings_have_exact_columns_and_index_order() {
        let db = open_with_binding_migration().await;
        let columns = db
            .query_all(sql("PRAGMA table_info('delegation_task_runs')"))
            .await
            .expect("table info")
            .into_iter()
            .filter_map(|row| {
                let name: String = row.try_get("", "name").expect("column name");
                name.starts_with("orchestration_").then(|| {
                    (
                        name,
                        (
                            row.try_get::<String>("", "type").expect("column type"),
                            row.try_get::<i64>("", "notnull").expect("nullability"),
                        ),
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            columns,
            BTreeMap::from([
                ("orchestration_generation".to_string(), ("INTEGER".to_string(), 0)),
                ("orchestration_namespace".to_string(), ("TEXT".to_string(), 0)),
                (
                    "orchestration_route_fingerprint".to_string(),
                    ("TEXT".to_string(), 0),
                ),
                (
                    "orchestration_schema_version".to_string(),
                    ("INTEGER".to_string(), 0),
                ),
            ])
        );

        let index_columns = db
            .query_all(sql(
                "PRAGMA index_info('idx_dtr_parent_orchestration_created_task')",
            ))
            .await
            .expect("index info")
            .into_iter()
            .map(|row| row.try_get::<String>("", "name").expect("index column"))
            .collect::<Vec<_>>();
        assert_eq!(
            index_columns,
            [
                "parent_conversation_id",
                "orchestration_namespace",
                "created_at",
                "task_id",
            ]
        );
    }

    #[tokio::test]
    async fn delegation_orchestration_bindings_reject_partial_insert_and_every_update() {
        let db = open_with_binding_migration().await;
        insert_run(&db, "unbound", 1, "", "")
            .await
            .expect("all-null binding accepted");
        insert_run(
            &db,
            "bound",
            2,
            "orchestration_schema_version, orchestration_namespace, \
             orchestration_generation, orchestration_route_fingerprint",
            &format!("1, 'brainstorm-to-delivery', 1, '{ZERO_FINGERPRINT}'"),
        )
        .await
        .expect("all-set binding accepted");

        let partial = insert_run(
            &db,
            "partial",
            3,
            "orchestration_schema_version",
            "1",
        )
        .await
        .expect_err("partial binding must be rejected");
        assert_trigger(partial, "trg_dtr_orchestration_binding_shape");

        let add = db
            .execute(sql(format!(
                "UPDATE delegation_task_runs SET \
                 orchestration_schema_version = 1, \
                 orchestration_namespace = 'brainstorm-to-delivery', \
                 orchestration_generation = 1, \
                 orchestration_route_fingerprint = '{ZERO_FINGERPRINT}' \
                 WHERE task_id = 'unbound'"
            )))
            .await
            .expect_err("adding a binding after insert must fail");
        assert_trigger(add, "trg_dtr_orchestration_binding_immutable");

        let change = db
            .execute(sql(
                "UPDATE delegation_task_runs SET orchestration_generation = 2 \
                 WHERE task_id = 'bound'",
            ))
            .await
            .expect_err("changing a binding after insert must fail");
        assert_trigger(change, "trg_dtr_orchestration_binding_immutable");

        let clear = db
            .execute(sql(
                "UPDATE delegation_task_runs SET \
                 orchestration_schema_version = NULL, \
                 orchestration_namespace = NULL, \
                 orchestration_generation = NULL, \
                 orchestration_route_fingerprint = NULL \
                 WHERE task_id = 'bound'",
            ))
            .await
            .expect_err("clearing a binding after insert must fail");
        assert_trigger(clear, "trg_dtr_orchestration_binding_immutable");

        db.execute(sql(
            "UPDATE delegation_task_runs SET status = 'running' WHERE task_id = 'bound'",
        ))
        .await
        .expect("status-only update succeeds");
        assert_eq!(
            binding_values(&db, "bound").await,
            (
                Some(1),
                Some("brainstorm-to-delivery".to_string()),
                Some(1),
                Some(ZERO_FINGERPRINT.to_string()),
            )
        );
    }
}
