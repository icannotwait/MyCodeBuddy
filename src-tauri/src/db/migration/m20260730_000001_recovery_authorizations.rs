//! Shared one-use recovery authorizations and nullable recovery provenance.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        for statement in [
            r#"CREATE TABLE recovery_authorizations (
               authorization_id TEXT PRIMARY KEY NOT NULL,
               parent_conversation_id INTEGER NOT NULL,
               subject_kind TEXT NOT NULL,
               subject_id TEXT NOT NULL,
               source_task_id TEXT NULL,
               child_conversation_id INTEGER NULL,
               lineage_root_task_id TEXT NULL,
               work_unit_key TEXT NULL,
               source_state_fingerprint TEXT NOT NULL,
               allowed_action TEXT NOT NULL,
               action_payload_json TEXT NOT NULL,
               cause_code TEXT NOT NULL,
               risk_class TEXT NOT NULL,
               display_reason TEXT NULL,
               status TEXT NOT NULL CHECK (status IN (
                 'pending','approved','declined','consumed','expired','abandoned'
               )),
               question_id TEXT NULL,
               requested_at TEXT NOT NULL,
               approved_at TEXT NULL,
               expires_at TEXT NULL,
               consumed_at TEXT NULL,
               consumed_by_kind TEXT NULL,
               consumed_by_id TEXT NULL,
               consumer_correlation_id TEXT NULL,
               FOREIGN KEY(parent_conversation_id)
                 REFERENCES conversation(id) ON DELETE CASCADE
             )"#,
            r#"CREATE UNIQUE INDEX idx_ra_one_active_challenge
               ON recovery_authorizations(
                 parent_conversation_id, subject_kind, subject_id, source_state_fingerprint
               )
               WHERE status IN ('pending','approved')"#,
            "CREATE INDEX idx_ra_question_id ON recovery_authorizations(question_id)",
            "CREATE INDEX idx_ra_parent_status ON recovery_authorizations(parent_conversation_id, status)",
            "CREATE INDEX idx_ra_status_expires_at ON recovery_authorizations(status, expires_at)",
            "CREATE INDEX idx_ra_consumed_by ON recovery_authorizations(consumed_by_kind, consumed_by_id)",
            "ALTER TABLE conversation ADD COLUMN last_termination_audit_json TEXT NULL",
            "ALTER TABLE delegation_task_runs ADD COLUMN recovery_authorization_id TEXT NULL",
            "ALTER TABLE delegation_workflow_manifest_revisions ADD COLUMN revision_kind TEXT NULL",
            "ALTER TABLE delegation_workflow_manifest_revisions ADD COLUMN source_manifest_revision INTEGER NULL",
            "ALTER TABLE delegation_workflow_manifest_revisions ADD COLUMN recovery_authorization_id TEXT NULL",
            "ALTER TABLE delegation_workflow_manifest_revisions ADD COLUMN transition_reason_code TEXT NULL",
            "ALTER TABLE delegation_workflow_manifest_revisions ADD COLUMN consumer_correlation_id TEXT NULL",
            "ALTER TABLE delegation_workflows ADD COLUMN block_cause_code TEXT NULL",
            "ALTER TABLE delegation_workflows ADD COLUMN block_source_manifest_revision INTEGER NULL",
            "ALTER TABLE delegation_workflow_gate_settlements ADD COLUMN lineage_reset_authorization_id TEXT NULL",
        ] {
            conn.execute_unprepared(statement).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Recovery provenance is durable audit history; older binaries ignore
        // the additive table and nullable columns.
        let _ = manager;
        Ok(())
    }
}
