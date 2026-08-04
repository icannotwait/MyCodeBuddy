//! Persist accepted completion tool calls and link each legacy workflow to at
//! most one completion-protocol-v2 successor.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in [
            r#"CREATE TABLE delegation_completion_tool_intents (
               intent_id TEXT PRIMARY KEY NOT NULL,
               task_id TEXT NOT NULL,
               child_tool_call_id TEXT NOT NULL,
               accepted_ordinal INTEGER NOT NULL,
               outcome TEXT NOT NULL,
               summary TEXT NULL,
               report_hint TEXT NULL,
               request_digest TEXT NOT NULL,
               created_at TEXT NOT NULL,
               UNIQUE(task_id, child_tool_call_id),
               UNIQUE(task_id, accepted_ordinal)
             )"#,
            r#"CREATE INDEX idx_dcti_task_latest
               ON delegation_completion_tool_intents(task_id, accepted_ordinal DESC)"#,
            "ALTER TABLE delegation_workflows ADD COLUMN legacy_source_workflow_id TEXT NULL",
            r#"CREATE UNIQUE INDEX idx_dw_unique_legacy_successor
               ON delegation_workflows(legacy_source_workflow_id)
               WHERE legacy_source_workflow_id IS NOT NULL"#,
        ] {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Accepted tool intent audit records and restart lineage are durable.
        let _ = manager;
        Ok(())
    }
}
