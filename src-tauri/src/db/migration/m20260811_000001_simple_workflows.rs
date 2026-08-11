//! Locator-only identity for Plan/progress-driven Simple workflows.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in [
            r#"CREATE TABLE simple_workflows (
               parent_conversation_id INTEGER PRIMARY KEY NOT NULL,
               plan_rel_path TEXT NOT NULL,
               progress_rel_path TEXT NOT NULL,
               source_workflow_id TEXT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY(parent_conversation_id)
                 REFERENCES conversation(id) ON DELETE CASCADE,
               FOREIGN KEY(source_workflow_id)
                 REFERENCES delegation_workflows(workflow_id) ON DELETE SET NULL
             )"#,
            "CREATE UNIQUE INDEX idx_simple_workflows_source ON simple_workflows(source_workflow_id)",
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
            .execute_unprepared("DROP TABLE IF EXISTS simple_workflows")
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sea_orm::{
        ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, QueryResult, Set, Statement,
    };

    use crate::db::entities::delegation_workflow::{
        self, CompletionProtocolMode, WorkflowState,
    };
    use crate::db::entities::conversation;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::agent::AgentType;

    fn insert_simple(parent: i32, source: Option<&str>) -> Statement {
        Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO simple_workflows \
             (parent_conversation_id, plan_rel_path, progress_rel_path, \
              source_workflow_id, created_at, updated_at) \
             VALUES (?, 'docs/plan.md', '.superpowers/sdd/progress.md', ?, \
                     '2026-08-11T00:00:00Z', '2026-08-11T00:00:00Z')",
            vec![parent.into(), source.map(str::to_owned).into()],
        )
    }

    async fn source_value(row: QueryResult) -> Option<String> {
        row.try_get("", "source_workflow_id")
            .expect("source_workflow_id")
    }

    #[tokio::test]
    async fn simple_workflow_migration_enforces_relations_and_unique_source() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/simple-workflow-migration").await;
        let source_parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let successor = seed_conversation(&db, folder, AgentType::Codex).await;
        let duplicate = seed_conversation(&db, folder, AgentType::Codex).await;
        let cascade_parent = seed_conversation(&db, folder, AgentType::Codex).await;
        let now = Utc::now();

        delegation_workflow::ActiveModel {
            workflow_id: Set("workflow-source".into()),
            parent_conversation_id: Set(source_parent),
            workflow_kind: Set("brainstorm_to_delivery".into()),
            schema_version: Set(1),
            active_manifest_revision: Set(1),
            graph_revision: Set(1),
            workflow_state: Set(WorkflowState::Approved),
            capability_version: Set("workflow_manifest_v1".into()),
            publication_token: Set("simple-migration-source".into()),
            supersedes_approved_revision: Set(None),
            structural_revision: Set(1),
            design_fingerprint: Set("design".into()),
            plan_fingerprint: Set("plan".into()),
            block_cause_code: Set(None),
            block_source_manifest_revision: Set(None),
            completion_protocol_version: Set(2),
            completion_protocol_mode: Set(CompletionProtocolMode::V2Enforce),
            legacy_source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("source workflow");

        db.conn
            .execute(insert_simple(successor, Some("workflow-source")))
            .await
            .expect("simple descriptor");
        assert!(
            db.conn
                .execute(insert_simple(duplicate, Some("workflow-source")))
                .await
                .is_err(),
            "one archived workflow must not link to two live successors"
        );

        db.conn
            .execute(insert_simple(cascade_parent, None))
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

        delegation_workflow::Entity::delete_by_id("workflow-source")
            .exec(&db.conn)
            .await
            .expect("delete source workflow");
        let row = db
            .conn
            .query_one(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT source_workflow_id FROM simple_workflows \
                 WHERE parent_conversation_id = ?",
                [successor.into()],
            ))
            .await
            .expect("load descriptor")
            .expect("descriptor remains");
        assert_eq!(source_value(row).await, None, "source delete must set null");
    }
}
