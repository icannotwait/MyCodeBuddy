//! Rebuild v1 gate settlements without reinterpreting their audit fields, then
//! add the durable v2 scope owners used by completion evidence.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{DbBackend, TransactionTrait};

use super::completion_rebuild::{self, RebuildFailpoint};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        if conn.get_database_backend() != DbBackend::Sqlite {
            return Err(DbErr::Custom(
                "completion scope migration requires SQLite".to_owned(),
            ));
        }

        #[cfg(any(test, feature = "test-utils"))]
        let failpoint = completion_rebuild::configured_failpoint(conn).await?;
        #[cfg(not(any(test, feature = "test-utils")))]
        let failpoint = RebuildFailpoint::None;

        let transaction = conn.begin().await?;
        let result = migrate_in_transaction(&transaction, failpoint).await;
        match result {
            Ok(()) => transaction.commit().await,
            Err(error) => match transaction.rollback().await {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(DbErr::Custom(format!(
                    "{error}; transaction rollback also failed: {rollback_error}"
                ))),
            },
        }
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Completion evidence and scope identities are durable audit state.
        let _ = manager;
        Ok(())
    }
}

async fn migrate_in_transaction<C: ConnectionTrait>(
    conn: &C,
    failpoint: RebuildFailpoint,
) -> Result<(), DbErr> {
    completion_rebuild::rebuild_gate_settlements(conn, failpoint).await?;

    for statement in [
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN evidence_scope_digest TEXT NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN gate_lineage TEXT NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN review_round INTEGER NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN instruction_block_digest TEXT NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN material_selector_digest TEXT NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN subject_material_digest TEXT NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN requirements_identity TEXT NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN task_specification_identity TEXT NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN final_findings_identity TEXT NULL",
        "ALTER TABLE delegation_workflow_run_bindings ADD COLUMN producer_baseline_head TEXT NULL",
        r#"CREATE TABLE delegation_workflow_gate_states (
           workflow_id TEXT NOT NULL,
           gate_id TEXT NOT NULL,
           gate_lineage TEXT NOT NULL,
           current_review_round INTEGER NOT NULL,
           selected_node_ids_json TEXT NOT NULL,
           PRIMARY KEY(workflow_id, gate_id),
           FOREIGN KEY(workflow_id)
             REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
         )"#,
        r#"CREATE TABLE delegation_workflow_design_root_bindings (
           workflow_id TEXT NOT NULL,
           gate_id TEXT NOT NULL,
           gate_lineage TEXT NOT NULL,
           node_id TEXT NOT NULL,
           task_id TEXT NOT NULL UNIQUE,
           latest_run_id TEXT NOT NULL UNIQUE,
           design_identity TEXT NOT NULL,
           evidence_scope_digest TEXT NOT NULL,
           graph_revision INTEGER NOT NULL,
           PRIMARY KEY(workflow_id, gate_id, gate_lineage),
           FOREIGN KEY(workflow_id, gate_id)
             REFERENCES delegation_workflow_gate_states(workflow_id, gate_id)
             ON DELETE CASCADE,
           FOREIGN KEY(workflow_id, node_id)
             REFERENCES delegation_workflow_node_bindings(workflow_id, node_id)
             ON DELETE CASCADE
         )"#,
        r#"CREATE TABLE delegation_final_findings_packages (
           package_id TEXT PRIMARY KEY NOT NULL,
           workflow_id TEXT NOT NULL,
           gate_id TEXT NOT NULL,
           gate_lineage TEXT NOT NULL,
           source_evaluation_key TEXT NOT NULL,
           source_evidence_task_ids_json TEXT NOT NULL,
           items_json TEXT NOT NULL,
           remediation_contexts_json TEXT NOT NULL,
           package_digest TEXT NOT NULL,
           status TEXT NOT NULL CHECK(status IN ('active','superseded','resolved')),
           created_graph_revision INTEGER NOT NULL,
           resolved_graph_revision INTEGER NULL,
           FOREIGN KEY(workflow_id, gate_id)
             REFERENCES delegation_workflow_gate_states(workflow_id, gate_id)
             ON DELETE CASCADE
         )"#,
        r#"CREATE UNIQUE INDEX idx_dffp_active_gate_lineage
           ON delegation_final_findings_packages(workflow_id, gate_id, gate_lineage)
           WHERE status = 'active'"#,
    ] {
        conn.execute_unprepared(statement).await?;
    }

    completion_rebuild::verify_foreign_keys(conn).await
}
