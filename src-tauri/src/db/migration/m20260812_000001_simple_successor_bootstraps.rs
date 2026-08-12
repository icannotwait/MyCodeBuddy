//! Durable post-connect bootstrap admission for archived-to-Simple successors.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in [
            r#"CREATE TABLE simple_successor_bootstraps (
               id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
               successor_conversation_id INTEGER NOT NULL,
               source_workflow_id TEXT NOT NULL,
               client_request_token TEXT NOT NULL,
               prompt TEXT NOT NULL,
               admitted_prompt TEXT NULL,
               status TEXT NOT NULL CHECK(status IN ('pending', 'admitted')),
               admitted_at TEXT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY(successor_conversation_id)
                 REFERENCES simple_workflows(parent_conversation_id) ON DELETE CASCADE,
               FOREIGN KEY(source_workflow_id)
                 REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
             )"#,
            "CREATE UNIQUE INDEX idx_simple_successor_bootstrap_successor \
             ON simple_successor_bootstraps(successor_conversation_id)",
            "CREATE UNIQUE INDEX idx_simple_successor_bootstrap_source \
             ON simple_successor_bootstraps(source_workflow_id)",
        ] {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE IF EXISTS simple_successor_bootstraps")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{
        ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, QueryResult, Statement,
    };

    use crate::commands::simple_workflow::test_support::seed_archived_workflow;
    use crate::db::entities::{conversation, simple_workflow};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::AgentType;

    fn insert_bootstrap(successor: i32, source: &str, token: &str) -> Statement {
        Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO simple_successor_bootstraps \
             (successor_conversation_id, source_workflow_id, client_request_token, prompt, \
              admitted_prompt, status, admitted_at, created_at, updated_at) \
             VALUES (?, ?, ?, 'prompt', NULL, 'pending', NULL, \
                     '2026-08-12T00:00:00Z', '2026-08-12T00:00:00Z')",
            vec![
                successor.into(),
                source.to_string().into(),
                token.to_string().into(),
            ],
        )
    }

    async fn count_rows(db: &crate::db::AppDatabase) -> i64 {
        let row: QueryResult = db
            .conn
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM simple_successor_bootstraps".to_string(),
            ))
            .await
            .expect("count bootstraps")
            .expect("count row");
        row.try_get("", "count").expect("count")
    }

    #[tokio::test]
    async fn simple_workflow_migration_bootstrap_enforces_identity_and_cascade_cleanup() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/simple-bootstrap-migration").await;
        let source_parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let successor = seed_conversation(&db, folder, AgentType::Codex).await;
        let duplicate = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            source_parent,
            "workflow-bootstrap-migration",
            "docs/plan.md",
            None,
            2,
            crate::db::entities::delegation_workflow::CompletionProtocolMode::V2Enforce,
        )
        .await;
        let now = chrono::Utc::now();
        simple_workflow::ActiveModel {
            parent_conversation_id: sea_orm::Set(successor),
            plan_rel_path: sea_orm::Set("docs/plan.md".into()),
            progress_rel_path: sea_orm::Set(".superpowers/sdd/progress.md".into()),
            source_workflow_id: sea_orm::Set(Some("workflow-bootstrap-migration".into())),
            created_at: sea_orm::Set(now),
            updated_at: sea_orm::Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("successor descriptor");

        db.conn
            .execute(insert_bootstrap(
                successor,
                "workflow-bootstrap-migration",
                "token-a",
            ))
            .await
            .expect("insert bootstrap");
        assert!(db
            .conn
            .execute(insert_bootstrap(
                duplicate,
                "workflow-bootstrap-migration",
                "token-b",
            ))
            .await
            .is_err());
        assert!(db
            .conn
            .execute(insert_bootstrap(
                successor,
                "workflow-bootstrap-migration",
                "token-c",
            ))
            .await
            .is_err());

        conversation::Entity::delete_by_id(successor)
            .exec(&db.conn)
            .await
            .expect("delete successor");
        assert_eq!(count_rows(&db).await, 0);
    }
}
