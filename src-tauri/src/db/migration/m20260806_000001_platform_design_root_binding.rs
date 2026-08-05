//! Decouple platform-owned Design-root CAS subjects from manifest node bindings.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{DbBackend, TransactionTrait};

use super::completion_rebuild;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        if conn.get_database_backend() != DbBackend::Sqlite {
            return Err(DbErr::Custom(
                "platform Design-root migration requires SQLite".to_owned(),
            ));
        }

        let transaction = conn.begin().await?;
        let result = migrate_in_transaction(&transaction).await;
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
        // Platform Design-root rows cannot be represented by the former FK.
        let _ = manager;
        Ok(())
    }
}

async fn migrate_in_transaction<C: ConnectionTrait>(conn: &C) -> Result<(), DbErr> {
    for statement in [
        r#"CREATE TABLE delegation_workflow_design_root_bindings_platform (
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
             ON DELETE CASCADE
         )"#,
        r#"INSERT INTO delegation_workflow_design_root_bindings_platform (
             workflow_id, gate_id, gate_lineage, node_id, task_id, latest_run_id,
             design_identity, evidence_scope_digest, graph_revision
           )
           SELECT workflow_id, gate_id, gate_lineage, node_id, task_id, latest_run_id,
                  design_identity, evidence_scope_digest, graph_revision
           FROM delegation_workflow_design_root_bindings"#,
        "DROP TABLE delegation_workflow_design_root_bindings",
        "ALTER TABLE delegation_workflow_design_root_bindings_platform RENAME TO delegation_workflow_design_root_bindings",
    ] {
        conn.execute_unprepared(statement).await?;
    }

    completion_rebuild::verify_foreign_keys(conn).await
}
