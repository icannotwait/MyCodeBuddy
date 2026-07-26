//! Add `structural_revision` clocks for plan-content identity separate from
//! state-only manifest CAS bumps (final-review fix wave 2).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // Header: defaults to active_manifest_revision for existing rows.
        conn.execute_unprepared(
            "ALTER TABLE delegation_workflows \
             ADD COLUMN structural_revision INTEGER NOT NULL DEFAULT 1",
        )
        .await?;
        conn.execute_unprepared(
            "UPDATE delegation_workflows \
             SET structural_revision = active_manifest_revision",
        )
        .await?;

        // Settlements: stamp content identity for validity across state-only bumps.
        conn.execute_unprepared(
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN structural_revision INTEGER NOT NULL DEFAULT 1",
        )
        .await?;
        conn.execute_unprepared(
            "UPDATE delegation_workflow_gate_settlements \
             SET structural_revision = manifest_revision",
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // SQLite cannot DROP COLUMN portably on older versions; leave columns.
        let _ = manager;
        Ok(())
    }
}
