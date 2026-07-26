//! Per-gate design/plan content fingerprints and run-binding content stamp
//! (final residual fix wave 3).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            "ALTER TABLE delegation_workflows \
             ADD COLUMN design_fingerprint TEXT NOT NULL DEFAULT ''",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE delegation_workflows \
             ADD COLUMN plan_fingerprint TEXT NOT NULL DEFAULT ''",
        )
        .await?;

        conn.execute_unprepared(
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN content_fingerprint TEXT NOT NULL DEFAULT ''",
        )
        .await?;

        // Document-gate run bindings stamp the design/plan fingerprint at admit;
        // Task/Final rows leave this NULL.
        conn.execute_unprepared(
            "ALTER TABLE delegation_workflow_run_bindings \
             ADD COLUMN content_fingerprint TEXT NULL",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let _ = manager;
        Ok(())
    }
}
