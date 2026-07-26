//! Delegation workflow graph tables: headers, immutable manifests, node
//! bindings, document-gate settlements, and per-run workflow associations.
//!
//! Design: `docs/superpowers/specs/2026-07-26-brainstorm-to-delivery-workflow-graph-design.md`
//! (persistence model + Contract Amendments A2/A3/B3).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        for statement in [
            // ---- workflow header --------------------------------------------
            r#"CREATE TABLE delegation_workflows (
               workflow_id TEXT PRIMARY KEY NOT NULL,
               parent_conversation_id INTEGER NOT NULL,
               workflow_kind TEXT NOT NULL,
               schema_version INTEGER NOT NULL,
               active_manifest_revision INTEGER NOT NULL,
               graph_revision INTEGER NOT NULL,
               workflow_state TEXT NOT NULL CHECK (workflow_state IN (
                 'skeleton','estimated','approved','blocked'
               )),
               capability_version TEXT NOT NULL,
               publication_token TEXT NOT NULL,
               supersedes_approved_revision INTEGER NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY(parent_conversation_id)
                 REFERENCES conversation(id) ON DELETE CASCADE
             )"#,
            "CREATE UNIQUE INDEX idx_dw_parent_kind ON delegation_workflows(parent_conversation_id, workflow_kind)",
            "CREATE UNIQUE INDEX idx_dw_publication_token ON delegation_workflows(publication_token)",
            // ---- immutable manifest revisions -------------------------------
            r#"CREATE TABLE delegation_workflow_manifest_revisions (
               workflow_id TEXT NOT NULL,
               manifest_revision INTEGER NOT NULL,
               manifest_state TEXT NOT NULL,
               document_json TEXT NOT NULL,
               document_digest TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY (workflow_id, manifest_revision),
               FOREIGN KEY(workflow_id)
                 REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
             )"#,
            // ---- node bindings ----------------------------------------------
            r#"CREATE TABLE delegation_workflow_node_bindings (
               workflow_id TEXT NOT NULL,
               node_id TEXT NOT NULL,
               work_unit_key TEXT NOT NULL,
               role TEXT NOT NULL,
               agent_type TEXT NOT NULL,
               profile_id TEXT NULL,
               phase_id TEXT NOT NULL,
               task_index INTEGER NULL,
               introduced_revision INTEGER NOT NULL,
               retired_revision INTEGER NULL,
               is_observed INTEGER NOT NULL DEFAULT 0 CHECK (is_observed IN (0, 1)),
               retained_observed INTEGER NOT NULL DEFAULT 0 CHECK (retained_observed IN (0, 1)),
               pair_frozen INTEGER NOT NULL DEFAULT 0 CHECK (pair_frozen IN (0, 1)),
               node_outcome TEXT NULL CHECK (
                 node_outcome IS NULL OR node_outcome IN ('canceled')
               ),
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY(workflow_id)
                 REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
             )"#,
            "CREATE UNIQUE INDEX idx_dwnb_workflow_node ON delegation_workflow_node_bindings(workflow_id, node_id)",
            "CREATE UNIQUE INDEX idx_dwnb_workflow_key ON delegation_workflow_node_bindings(workflow_id, work_unit_key)",
            // ---- document-gate settlements ----------------------------------
            r#"CREATE TABLE delegation_workflow_gate_settlements (
               workflow_id TEXT NOT NULL,
               gate_id TEXT NOT NULL,
               gate_cycle INTEGER NOT NULL,
               manifest_revision INTEGER NOT NULL,
               outcome TEXT NOT NULL CHECK (outcome IN (
                 'approved','changes_requested','blocked'
               )),
               critical_count INTEGER NOT NULL,
               important_count INTEGER NOT NULL,
               minor_count INTEGER NOT NULL,
               summary TEXT NOT NULL CHECK (length(summary) <= 4096),
               graph_revision_at_settle INTEGER NOT NULL,
               created_at TEXT NOT NULL,
               FOREIGN KEY(workflow_id)
                 REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
             )"#,
            "CREATE UNIQUE INDEX idx_dwgs_gate_cycle ON delegation_workflow_gate_settlements(workflow_id, gate_id, gate_cycle)",
            // ---- run ↔ workflow associations (admission / review coverage) --
            r#"CREATE TABLE delegation_workflow_run_bindings (
               task_id TEXT PRIMARY KEY NOT NULL,
               workflow_id TEXT NOT NULL,
               node_id TEXT NOT NULL,
               gate_id TEXT NULL,
               gate_cycle INTEGER NULL,
               manifest_revision INTEGER NOT NULL,
               artifact_digest TEXT NULL,
               reviewed_task_id TEXT NULL,
               reviewed_implementer_generation INTEGER NULL,
               lineage_ordinal INTEGER NOT NULL,
               summary_validated INTEGER NOT NULL DEFAULT 0 CHECK (summary_validated IN (0, 1)),
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY(workflow_id)
                 REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
             )"#,
            "CREATE UNIQUE INDEX idx_dwrb_task ON delegation_workflow_run_bindings(task_id)",
        ] {
            conn.execute_unprepared(statement).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for statement in [
            "DROP TABLE IF EXISTS delegation_workflow_run_bindings",
            "DROP TABLE IF EXISTS delegation_workflow_gate_settlements",
            "DROP TABLE IF EXISTS delegation_workflow_node_bindings",
            "DROP TABLE IF EXISTS delegation_workflow_manifest_revisions",
            "DROP TABLE IF EXISTS delegation_workflows",
        ] {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }
}
