//! Durable, bounded original-request context for immutable legacy restarts.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"CREATE TABLE delegation_workflow_restart_contexts (
                   conversation_id INTEGER PRIMARY KEY NOT NULL,
                   original_conversation_id INTEGER NOT NULL,
                   original_request_id TEXT NOT NULL,
                   original_request_text TEXT NOT NULL,
                   original_request_digest TEXT NOT NULL,
                   agent_type TEXT NOT NULL,
                   profile_id TEXT NULL,
                   created_at TEXT NOT NULL,
                   FOREIGN KEY(conversation_id) REFERENCES conversation(id) ON DELETE CASCADE,
                   FOREIGN KEY(original_conversation_id) REFERENCES conversation(id) ON DELETE RESTRICT
                 )"#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let _ = manager;
        Ok(())
    }
}
