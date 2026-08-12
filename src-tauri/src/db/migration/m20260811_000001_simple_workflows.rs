//! Locator-only identity for Plan/progress-driven Simple workflows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE TABLE simple_workflows (
               parent_conversation_id INTEGER PRIMARY KEY NOT NULL,
               plan_rel_path TEXT NOT NULL,
               progress_rel_path TEXT NOT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY(parent_conversation_id)
                 REFERENCES conversation(id) ON DELETE CASCADE
             )"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS simple_workflows")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, Statement};

    use crate::db::entities::conversation;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;

    #[derive(Debug, PartialEq, Eq)]
    struct ColumnInfo {
        name: String,
        col_type: String,
        notnull: i64,
        pk: i64,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ForeignKeyInfo {
        table: String,
        from: String,
        to: String,
        on_delete: String,
    }

    #[tokio::test]
    async fn simple_workflow_migration_is_locator_only_and_has_no_bootstrap_schema() {
        let db = fresh_in_memory_db().await;
        let columns = db
            .conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA table_info('simple_workflows')".to_string(),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| ColumnInfo {
                name: row.try_get::<String>("", "name").unwrap(),
                col_type: row.try_get::<String>("", "type").unwrap(),
                notnull: row.try_get::<i64>("", "notnull").unwrap(),
                pk: row.try_get::<i64>("", "pk").unwrap(),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            columns,
            vec![
                ColumnInfo {
                    name: "parent_conversation_id".into(),
                    col_type: "INTEGER".into(),
                    notnull: 1,
                    pk: 1,
                },
                ColumnInfo {
                    name: "plan_rel_path".into(),
                    col_type: "TEXT".into(),
                    notnull: 1,
                    pk: 0,
                },
                ColumnInfo {
                    name: "progress_rel_path".into(),
                    col_type: "TEXT".into(),
                    notnull: 1,
                    pk: 0,
                },
                ColumnInfo {
                    name: "created_at".into(),
                    col_type: "TEXT".into(),
                    notnull: 1,
                    pk: 0,
                },
                ColumnInfo {
                    name: "updated_at".into(),
                    col_type: "TEXT".into(),
                    notnull: 1,
                    pk: 0,
                },
            ]
        );
        assert!(!columns
            .iter()
            .any(|column| column.name == concat!("source_", "workflow_id")));

        let bootstrap_table = concat!("simple_successor_", "bootstraps");
        let bootstrap_count: i64 = db
            .conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                format!(
                    "SELECT COUNT(*) AS count FROM sqlite_master WHERE name = '{bootstrap_table}'"
                ),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "count")
            .unwrap();
        assert_eq!(bootstrap_count, 0);

        let foreign_keys = db
            .conn
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA foreign_key_list('simple_workflows')".to_string(),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| ForeignKeyInfo {
                table: row.try_get::<String>("", "table").unwrap(),
                from: row.try_get::<String>("", "from").unwrap(),
                to: row.try_get::<String>("", "to").unwrap(),
                on_delete: row.try_get::<String>("", "on_delete").unwrap(),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            foreign_keys,
            vec![ForeignKeyInfo {
                table: "conversation".into(),
                from: "parent_conversation_id".into(),
                to: "id".into(),
                on_delete: "CASCADE".into(),
            }]
        );

        for index_name in [
            concat!("idx_simple_workflows_", "source"),
            concat!("idx_simple_successor_", "bootstrap_successor"),
            concat!("idx_simple_successor_", "bootstrap_source"),
        ] {
            let index_count: i64 = db
                .conn
                .query_one(Statement::from_string(
                    DbBackend::Sqlite,
                    format!(
                        "SELECT COUNT(*) AS count FROM sqlite_master WHERE name = '{index_name}'"
                    ),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "count")
                .unwrap();
            assert_eq!(index_count, 0, "{index_name} must not exist");
        }

        let folder = seed_folder(&db, "/tmp/simple-workflow-migration").await;
        let cascade_parent = seed_conversation(&db, folder, AgentType::Codex).await;
        db.conn
            .execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO simple_workflows \
                 (parent_conversation_id, plan_rel_path, progress_rel_path, \
                  created_at, updated_at) \
                 VALUES (?, 'docs/plan.md', '.superpowers/sdd/progress.md', \
                         '2026-08-11T00:00:00Z', '2026-08-11T00:00:00Z')",
                [cascade_parent.into()],
            ))
            .await
            .expect("cascade descriptor");
        conversation::Entity::delete_by_id(cascade_parent)
            .exec(&db.conn)
            .await
            .expect("delete parent");
        let cascade_count: i64 = db
            .conn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM simple_workflows \
                 WHERE parent_conversation_id = ?",
                [cascade_parent.into()],
            ))
            .await
            .expect("count cascade")
            .expect("count row")
            .try_get("", "count")
            .expect("count");
        assert_eq!(cascade_count, 0, "parent delete must cascade descriptor");
    }
}
