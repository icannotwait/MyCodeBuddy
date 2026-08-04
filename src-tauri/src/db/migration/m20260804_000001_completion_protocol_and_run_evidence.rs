//! Freeze the completion protocol on workflow headers and add nullable
//! platform-generated completion projections to delegation runs.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for sql in [
            "ALTER TABLE delegation_workflows ADD COLUMN completion_protocol_version INTEGER NOT NULL DEFAULT 1 CHECK (completion_protocol_version IN (1, 2))",
            "ALTER TABLE delegation_workflows ADD COLUMN completion_protocol_mode TEXT NOT NULL DEFAULT 'v1' CHECK (completion_protocol_mode IN ('v1', 'v2_shadow', 'v2_enforce'))",
            "ALTER TABLE delegation_task_runs ADD COLUMN completion_state TEXT NULL CHECK (completion_state IS NULL OR completion_state IN ('resolved', 'needs_decision', 'artifact_recovery'))",
            "ALTER TABLE delegation_task_runs ADD COLUMN completion_outcome TEXT NULL CHECK (completion_outcome IS NULL OR completion_outcome IN ('approve', 'approve_with_minors', 'request_changes', 'block', 'done', 'done_with_concerns', 'blocked'))",
            "ALTER TABLE delegation_task_runs ADD COLUMN completion_evidence_json TEXT NULL",
        ] {
            manager.get_connection().execute_unprepared(sql).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let _ = manager;
        Ok(())
    }
}
