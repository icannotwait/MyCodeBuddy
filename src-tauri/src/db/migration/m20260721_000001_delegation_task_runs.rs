//! Durable `delegation_task_runs` + budget tables, conversation projection
//! generation column, and legacy gen-1 backfill.
//!
//! Design: `docs/superpowers/specs/2026-07-21-delegation-session-reuse-design.md`
//! (Durable Run Model, Migration, Indexes, backfill rules).

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        for statement in [
            // ---- conversation projection fence --------------------------------
            "ALTER TABLE conversation ADD COLUMN delegation_run_generation INTEGER NULL",
            // ---- authoritative run table --------------------------------------
            r#"CREATE TABLE delegation_task_runs (
               task_id TEXT PRIMARY KEY NOT NULL,
               root_task_id TEXT NOT NULL,
               previous_task_id TEXT NULL,
               generation INTEGER NOT NULL CHECK (generation > 0),
               parent_conversation_id INTEGER NOT NULL,
               parent_tool_use_id TEXT NULL,
               child_conversation_id INTEGER NOT NULL,
               agent_type TEXT NOT NULL,
               profile_id TEXT NULL,
               workspace_path TEXT NULL,
               route_fingerprint TEXT NULL,
               launch_snapshot_version TEXT NULL,
               mode_id TEXT NULL,
               config_values_json TEXT NULL,
               task_preview TEXT NULL,
               request_fingerprint TEXT NULL,
               admission_class TEXT NOT NULL CHECK (admission_class IN (
                 'normal_revision','unexpected_continue','replacement'
               )),
               reached_running_at TEXT NULL,
               lineage_root_task_id TEXT NOT NULL,
               work_unit_key TEXT NULL,
               legacy_parent_tool_use_id TEXT NULL,
               history_only INTEGER NOT NULL DEFAULT 0 CHECK (history_only IN (0, 1)),
               status TEXT NOT NULL CHECK (status IN (
                 'reserving','running','completed','failed','canceled'
               )),
               error_code TEXT NULL,
               termination_audit_json TEXT NULL,
               started_at TEXT NULL,
               finished_at TEXT NULL,
               tool_call_count INTEGER NULL,
               edit_tool_call_count INTEGER NULL,
               touched_files_json TEXT NULL,
               touched_files_truncated INTEGER NULL,
               additions INTEGER NULL,
               deletions INTEGER NULL,
               line_counts_complete INTEGER NULL,
               card_summary_json TEXT NULL,
               child_turn_anchor TEXT NULL,
               child_connection_id TEXT NULL,
               replaced_task_id TEXT NULL,
               replacement_reason TEXT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               FOREIGN KEY(parent_conversation_id)
                 REFERENCES conversation(id) ON DELETE CASCADE,
               FOREIGN KEY(child_conversation_id)
                 REFERENCES conversation(id) ON DELETE CASCADE
             )"#,
            // unique (child, generation)
            "CREATE UNIQUE INDEX idx_dtr_child_generation ON delegation_task_runs(child_conversation_id, generation)",
            // unique (parent, parent_tool_use_id) where non-null
            r#"CREATE UNIQUE INDEX idx_dtr_parent_tool_use
               ON delegation_task_runs(parent_conversation_id, parent_tool_use_id)
               WHERE parent_tool_use_id IS NOT NULL"#,
            // lookup indexes
            "CREATE INDEX idx_dtr_parent ON delegation_task_runs(parent_conversation_id)",
            "CREATE INDEX idx_dtr_child ON delegation_task_runs(child_conversation_id)",
            "CREATE INDEX idx_dtr_root_task ON delegation_task_runs(root_task_id)",
            "CREATE INDEX idx_dtr_previous_task ON delegation_task_runs(previous_task_id)",
            "CREATE INDEX idx_dtr_lineage_root ON delegation_task_runs(lineage_root_task_id)",
            r#"CREATE INDEX idx_dtr_parent_work_unit
               ON delegation_task_runs(parent_conversation_id, work_unit_key)
               WHERE work_unit_key IS NOT NULL"#,
            // partial unique: one non-terminal run per child
            r#"CREATE UNIQUE INDEX idx_dtr_one_nonterminal_per_child
               ON delegation_task_runs(child_conversation_id)
               WHERE status IN ('reserving', 'running')"#,
            // partial unique: one non-terminal gen-1 per orchestrated key
            r#"CREATE UNIQUE INDEX idx_dtr_one_nonterminal_gen1_work_unit
               ON delegation_task_runs(parent_conversation_id, work_unit_key)
               WHERE status IN ('reserving', 'running')
                 AND generation = 1
                 AND work_unit_key IS NOT NULL"#,
            // ---- budget tables ------------------------------------------------
            r#"CREATE TABLE delegation_lineage_budgets (
               lineage_root_task_id TEXT PRIMARY KEY NOT NULL,
               unexpected_continue_count INTEGER NOT NULL DEFAULT 0
                 CHECK (unexpected_continue_count >= 0),
               replacement_count INTEGER NOT NULL DEFAULT 0
                 CHECK (replacement_count >= 0)
             )"#,
            r#"CREATE TABLE delegation_work_unit_budgets (
               parent_conversation_id INTEGER NOT NULL,
               work_unit_key TEXT NOT NULL,
               unexpected_continue_count INTEGER NOT NULL DEFAULT 0
                 CHECK (unexpected_continue_count >= 0),
               replacement_count INTEGER NOT NULL DEFAULT 0
                 CHECK (replacement_count >= 0),
               PRIMARY KEY (parent_conversation_id, work_unit_key),
               FOREIGN KEY(parent_conversation_id)
                 REFERENCES conversation(id) ON DELETE CASCADE
             )"#,
        ] {
            conn.execute_unprepared(statement).await?;
        }

        // ------------------------------------------------------------------
        // Backfill generation-1 runs for non-deleted delegate conversations
        // that have a non-null, non-empty delegation_call_id.
        //
        // Collision handling:
        //   1. Duplicate call_id → keep newest non-deleted child only.
        //   2. Duplicate (parent, parent_tool_use_id) among survivors →
        //      winner keeps the key; losers NULL the unique key and stash
        //      the original in legacy_parent_tool_use_id + history_only.
        //
        // Continuability (history_only = false) requires external_id,
        // non-empty parent_tool_use_id, and a reconstructible launch
        // snapshot. Historical rows never stored a launch snapshot, so
        // all backfilled runs are history_only = 1 with null snapshot
        // fields.
        // ------------------------------------------------------------------
        conn.execute_unprepared(
            r#"INSERT INTO delegation_task_runs (
               task_id, root_task_id, previous_task_id, generation,
               parent_conversation_id, parent_tool_use_id, child_conversation_id,
               agent_type, profile_id,
               workspace_path, route_fingerprint, launch_snapshot_version,
               mode_id, config_values_json,
               task_preview, request_fingerprint,
               admission_class, reached_running_at, lineage_root_task_id,
               work_unit_key, legacy_parent_tool_use_id, history_only,
               status, error_code, termination_audit_json,
               started_at, finished_at,
               tool_call_count, edit_tool_call_count, touched_files_json,
               touched_files_truncated, additions, deletions, line_counts_complete,
               card_summary_json, child_turn_anchor, child_connection_id,
               replaced_task_id, replacement_reason,
               created_at, updated_at
             )
             WITH candidates AS (
               SELECT
                 c.id AS child_id,
                 c.parent_id AS parent_id,
                 c.delegation_call_id AS call_id,
                 c.agent_type AS agent_type,
                 c.status AS conv_status,
                 c.external_id AS external_id,
                 c.delegation_task_status AS task_status,
                 c.delegation_started_at AS started_at,
                 c.delegation_finished_at AS finished_at,
                 c.delegation_error_code AS error_code,
                 c.delegation_tool_call_count AS tool_call_count,
                 c.delegation_edit_tool_call_count AS edit_tool_call_count,
                 c.delegation_touched_files_json AS touched_files_json,
                 c.delegation_touched_files_truncated AS touched_files_truncated,
                 c.delegation_additions AS additions,
                 c.delegation_deletions AS deletions,
                 c.delegation_line_counts_complete AS line_counts_complete,
                 c.created_at AS created_at,
                 c.updated_at AS updated_at,
                 CASE
                   WHEN c.parent_tool_use_id IS NULL THEN NULL
                   WHEN TRIM(c.parent_tool_use_id) = '' THEN NULL
                   ELSE c.parent_tool_use_id
                 END AS normalized_tool_use_id,
                 ROW_NUMBER() OVER (
                   PARTITION BY c.delegation_call_id
                   ORDER BY c.created_at DESC, c.id DESC
                 ) AS call_rank
               FROM conversation c
               WHERE c.kind = 'delegate'
                 AND c.deleted_at IS NULL
                 AND c.delegation_call_id IS NOT NULL
                 AND TRIM(c.delegation_call_id) != ''
                 AND c.parent_id IS NOT NULL
             ),
             call_winners AS (
               SELECT * FROM candidates WHERE call_rank = 1
             ),
             tool_ranked AS (
               SELECT
                 w.*,
                 ROW_NUMBER() OVER (
                   PARTITION BY w.parent_id, w.normalized_tool_use_id
                   ORDER BY w.created_at DESC, w.child_id DESC
                 ) AS tool_rank,
                 CASE
                   WHEN w.task_status IS NOT NULL THEN w.task_status
                   WHEN w.conv_status = 'in_progress' THEN 'running'
                   WHEN w.conv_status = 'pending_review' THEN 'completed'
                   WHEN w.conv_status = 'completed' THEN 'completed'
                   WHEN w.conv_status = 'cancelled' THEN 'canceled'
                   ELSE 'completed'
                 END AS mapped_status
               FROM call_winners w
             )
             SELECT
               tr.call_id AS task_id,
               tr.call_id AS root_task_id,
               NULL AS previous_task_id,
               1 AS generation,
               tr.parent_id AS parent_conversation_id,
               CASE
                 WHEN tr.normalized_tool_use_id IS NULL THEN NULL
                 WHEN tr.tool_rank = 1 THEN tr.normalized_tool_use_id
                 ELSE NULL
               END AS parent_tool_use_id,
               tr.child_id AS child_conversation_id,
               tr.agent_type AS agent_type,
               NULL AS profile_id,
               NULL AS workspace_path,
               NULL AS route_fingerprint,
               NULL AS launch_snapshot_version,
               NULL AS mode_id,
               NULL AS config_values_json,
               NULL AS task_preview,
               NULL AS request_fingerprint,
               'normal_revision' AS admission_class,
               CASE
                 WHEN tr.mapped_status IN ('running','completed','failed','canceled')
                      AND (
                        tr.external_id IS NOT NULL
                        OR tr.started_at IS NOT NULL
                      )
                 THEN COALESCE(tr.started_at, tr.created_at)
                 ELSE NULL
               END AS reached_running_at,
               tr.call_id AS lineage_root_task_id,
               NULL AS work_unit_key,
               CASE
                 WHEN tr.normalized_tool_use_id IS NOT NULL AND tr.tool_rank > 1
                 THEN tr.normalized_tool_use_id
                 ELSE NULL
               END AS legacy_parent_tool_use_id,
               1 AS history_only,
               tr.mapped_status AS status,
               tr.error_code AS error_code,
               NULL AS termination_audit_json,
               tr.started_at AS started_at,
               tr.finished_at AS finished_at,
               tr.tool_call_count AS tool_call_count,
               tr.edit_tool_call_count AS edit_tool_call_count,
               tr.touched_files_json AS touched_files_json,
               tr.touched_files_truncated AS touched_files_truncated,
               tr.additions AS additions,
               tr.deletions AS deletions,
               tr.line_counts_complete AS line_counts_complete,
               NULL AS card_summary_json,
               NULL AS child_turn_anchor,
               NULL AS child_connection_id,
               NULL AS replaced_task_id,
               NULL AS replacement_reason,
               tr.created_at AS created_at,
               tr.updated_at AS updated_at
             FROM tool_ranked tr"#,
        )
        .await?;

        // Project generation = 1 onto conversations that received a run.
        conn.execute_unprepared(
            r#"UPDATE conversation
             SET delegation_run_generation = 1
             WHERE id IN (
               SELECT child_conversation_id FROM delegation_task_runs WHERE generation = 1
             )"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for statement in [
            "DROP TABLE IF EXISTS delegation_work_unit_budgets",
            "DROP TABLE IF EXISTS delegation_lineage_budgets",
            "DROP TABLE IF EXISTS delegation_task_runs",
            "ALTER TABLE conversation DROP COLUMN delegation_run_generation",
        ] {
            conn.execute_unprepared(statement).await?;
        }
        Ok(())
    }
}
