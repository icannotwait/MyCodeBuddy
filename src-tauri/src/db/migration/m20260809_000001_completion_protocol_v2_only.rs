//! Enforce v2-only workflow creation while preserving historical headers.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in [
            r#"CREATE TRIGGER trg_delegation_workflows_v2_only_insert
               BEFORE INSERT ON delegation_workflows
               WHEN NEW.completion_protocol_version <> 2
                 OR NEW.completion_protocol_mode <> 'v2_enforce'
                 OR NEW.legacy_source_workflow_id IS NOT NULL
               BEGIN
                 SELECT RAISE(ABORT, 'completion_protocol_v2_only');
               END"#,
            r#"CREATE TRIGGER trg_delegation_workflows_protocol_frozen
               BEFORE UPDATE OF completion_protocol_version, completion_protocol_mode
               ON delegation_workflows
               WHEN NEW.completion_protocol_version IS NOT OLD.completion_protocol_version
                 OR NEW.completion_protocol_mode IS NOT OLD.completion_protocol_mode
               BEGIN
                 SELECT RAISE(ABORT, 'completion_protocol_frozen');
               END"#,
            r#"CREATE TRIGGER trg_delegation_workflows_legacy_source_frozen
               BEFORE UPDATE OF legacy_source_workflow_id ON delegation_workflows
               WHEN NOT (NEW.legacy_source_workflow_id IS OLD.legacy_source_workflow_id)
               BEGIN
                 SELECT RAISE(ABORT, 'legacy_source_workflow_frozen');
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
            "DROP TRIGGER IF EXISTS trg_delegation_workflows_v2_only_insert",
            "DROP TRIGGER IF EXISTS trg_delegation_workflows_protocol_frozen",
            "DROP TRIGGER IF EXISTS trg_delegation_workflows_legacy_source_frozen",
        ] {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }
        Ok(())
    }
}
