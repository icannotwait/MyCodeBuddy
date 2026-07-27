//! Manifest-v2 workflow persistence: cohort freezing and immutable Plan
//! review-cycle evidence.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        for statement in [
            "ALTER TABLE delegation_workflow_node_bindings \
             RENAME COLUMN pair_frozen TO cohort_frozen",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN review_scope TEXT NULL CHECK (\
               review_scope IS NULL OR review_scope IN ('full','scoped')\
             )",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN revision_kind TEXT NULL CHECK (\
               revision_kind IS NULL OR revision_kind IN (\
                 'initial','localized','material','holistic_rewrite'\
               )\
             )",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN scope_reason TEXT NULL",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN required_reviewer_node_ids_json TEXT NULL",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN covered_author_task_id TEXT NULL",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN covered_plan_digest TEXT NULL",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN finding_ledger_json TEXT NULL",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN net_improvement INTEGER NULL CHECK (\
               net_improvement IS NULL OR net_improvement IN (0, 1)\
             )",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN stagnation_count INTEGER NOT NULL DEFAULT 0 CHECK (\
               stagnation_count >= 0\
             )",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN rewrite_used INTEGER NOT NULL DEFAULT 0 CHECK (\
               rewrite_used IN (0, 1)\
             )",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN next_action TEXT NULL CHECK (\
               next_action IS NULL OR next_action IN (\
                 'continue_review','holistic_rewrite_required',\
                 'user_decision_required','approved'\
               )\
             )",
            "ALTER TABLE delegation_workflow_gate_settlements \
             ADD COLUMN report_files_json TEXT NULL",
        ] {
            conn.execute_unprepared(statement).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let _ = manager;
        Ok(())
    }
}
