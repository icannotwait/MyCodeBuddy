//! Historical repair for soft-deleted provisional-admission orphans.
//!
//! Design: `docs/superpowers/specs/2026-07-24-delegation-exact-correlation-and-provisional-cleanup-design.md`
//! (Historical Repair Migration).
//!
//! Only rows that provably match the broker provisional-admission orphan shape
//! are relabeled: soft-deleted, still-running, no finish, broker provenance
//! (parent_id + nonblank call id), synthetic start only, blank external id,
//! no run row, zero runtime rollups / messages / generation. Matches become
//! `failed` + `provisional_admission_rejected` with `finished_at = deleted_at`.
//!
//! Intentionally irreversible: down is a no-op (never restore invalid running).
//! Registered after `m20260723_000001_delegation_task_runs`.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

/// Wire-stable error code written onto repaired provisional orphan rows.
const PROVISIONAL_ADMISSION_REJECTED: &str = "provisional_admission_rejected";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        // Predicate mirrors design SQL exactly (broker provenance + synthetic
        // start + zero activity + no run + soft-deleted running). SQLite TRIM
        // strips spaces; strip tab/LF/CR before emptiness checks so blank
        // whitespace-only external_id / call_id match Rust-ish "all whitespace".
        conn.execute_unprepared(&format!(
            r#"UPDATE conversation
             SET
               delegation_task_status = 'failed',
               delegation_error_code = '{PROVISIONAL_ADMISSION_REJECTED}',
               delegation_finished_at = deleted_at
             WHERE kind = 'delegate'
               AND deleted_at IS NOT NULL
               AND delegation_task_status = 'running'
               AND delegation_finished_at IS NULL
               AND parent_id IS NOT NULL
               AND (
                 delegation_call_id IS NOT NULL
                 AND TRIM(REPLACE(REPLACE(REPLACE(delegation_call_id,
                     char(9), ''), char(10), ''), char(13), '')) != ''
               )
               AND (
                 external_id IS NULL
                 OR TRIM(REPLACE(REPLACE(REPLACE(external_id,
                     char(9), ''), char(10), ''), char(13), '')) = ''
               )
               AND (
                 delegation_started_at IS NULL
                 OR delegation_started_at <= created_at
               )
               AND delegation_run_generation IS NULL
               AND (
                 delegation_tool_call_count IS NULL
                 OR delegation_tool_call_count = 0
               )
               AND (
                 delegation_edit_tool_call_count IS NULL
                 OR delegation_edit_tool_call_count = 0
               )
               AND (
                 delegation_touched_files_json IS NULL
                 OR delegation_touched_files_json = ''
                 OR delegation_touched_files_json = '[]'
               )
               AND (
                 delegation_touched_files_truncated IS NULL
                 OR delegation_touched_files_truncated = 0
               )
               AND (
                 delegation_additions IS NULL
                 OR delegation_additions = 0
               )
               AND (
                 delegation_deletions IS NULL
                 OR delegation_deletions = 0
               )
               AND (
                 delegation_line_counts_complete IS NULL
                 OR delegation_line_counts_complete = 0
               )
               AND message_count = 0
               AND NOT EXISTS (
                 SELECT 1
                 FROM delegation_task_runs AS r
                 WHERE r.child_conversation_id = conversation.id
               )"#
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Irreversible data repair: never restore soft-deleted running orphans.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    use sea_orm_migration::{MigrationTrait, MigratorTrait, SchemaManager};

    use crate::db::migration::Migrator;

    fn sql(text: impl Into<String>) -> Statement {
        Statement::from_string(DbBackend::Sqlite, text.into())
    }

    /// Candidate-count preflight SQL: same fences as the repair UPDATE WHERE.
    const PREFLIGHT_CANDIDATE_SQL: &str = r#"
        SELECT COUNT(*) AS count FROM conversation
        WHERE kind = 'delegate'
          AND deleted_at IS NOT NULL
          AND delegation_task_status = 'running'
          AND delegation_finished_at IS NULL
          AND parent_id IS NOT NULL
          AND (
            delegation_call_id IS NOT NULL
            AND TRIM(REPLACE(REPLACE(REPLACE(delegation_call_id,
                char(9), ''), char(10), ''), char(13), '')) != ''
          )
          AND (
            external_id IS NULL
            OR TRIM(REPLACE(REPLACE(REPLACE(external_id,
                char(9), ''), char(10), ''), char(13), '')) = ''
          )
          AND (
            delegation_started_at IS NULL
            OR delegation_started_at <= created_at
          )
          AND delegation_run_generation IS NULL
          AND (
            delegation_tool_call_count IS NULL
            OR delegation_tool_call_count = 0
          )
          AND (
            delegation_edit_tool_call_count IS NULL
            OR delegation_edit_tool_call_count = 0
          )
          AND (
            delegation_touched_files_json IS NULL
            OR delegation_touched_files_json = ''
            OR delegation_touched_files_json = '[]'
          )
          AND (
            delegation_touched_files_truncated IS NULL
            OR delegation_touched_files_truncated = 0
          )
          AND (
            delegation_additions IS NULL
            OR delegation_additions = 0
          )
          AND (
            delegation_deletions IS NULL
            OR delegation_deletions = 0
          )
          AND (
            delegation_line_counts_complete IS NULL
            OR delegation_line_counts_complete = 0
          )
          AND message_count = 0
          AND NOT EXISTS (
            SELECT 1
            FROM delegation_task_runs AS r
            WHERE r.child_conversation_id = conversation.id
          )
    "#;

    /// Seeded fixture set expects exactly these ids as repair candidates.
    const EXPECTED_PREFLIGHT_CANDIDATES: i64 = 2;
    const MATCH_PURE_ORPHAN_ID: i32 = 100;
    const MATCH_BLANK_EXTERNAL_ID: i32 = 101;
    const NEAR_NON_SYNTHETIC_START_ID: i32 = 200;
    const NEAR_NONZERO_ROLLUP_ID: i32 = 201;
    const NEAR_MESSAGE_COUNT_ID: i32 = 202;
    const NEAR_RUN_GENERATION_ID: i32 = 203;
    const NEAR_NULL_PARENT_ID: i32 = 204;
    const NEAR_BLANK_CALL_ID: i32 = 205;
    const NEAR_NULL_CALL_ID: i32 = 209;
    const NEAR_HAS_RUN_ID: i32 = 206;
    const NEAR_NONBLANK_EXTERNAL_ID: i32 = 207;
    const NEAR_NOT_SOFT_DELETED_ID: i32 = 208;

    async fn open_through_prior_migrations() -> sea_orm::DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.expect("database");
        let migrations = Migrator::migrations();
        let idx = migrations
            .iter()
            .position(|m| m.name().contains("provisional_orphan_repair"))
            .expect("provisional_orphan_repair migration registered");
        // Apply every migration before this one (SeaORM steps = count of pending).
        Migrator::up(&db, Some(idx as u32))
            .await
            .expect("prior migrations");
        db
    }

    async fn seed_folder_and_parent(db: &sea_orm::DatabaseConnection) {
        db.execute(sql(
            "INSERT INTO folder \
             (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
             VALUES (1,'repo','C:/repo','2026-07-24','2026-07-24','2026-07-24',1,1,'inherit','regular')",
        ))
        .await
        .expect("folder");
        db.execute(sql(
            "INSERT INTO conversation \
             (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,\
              created_at,updated_at) \
             VALUES (1,1,'codex','completed','regular',0,0,0,'2026-07-24T00:00:00Z','2026-07-24T00:00:00Z')",
        ))
        .await
        .expect("parent conversation");
    }

    /// Insert a soft-deleted running delegate with birth-shape defaults, then
    /// apply optional column overrides via a follow-up UPDATE.
    async fn seed_delegate_shell(
        db: &sea_orm::DatabaseConnection,
        id: i32,
        call_id: Option<&str>,
        parent_id: Option<i32>,
        deleted: bool,
    ) {
        let call = match call_id {
            Some(c) => format!("'{c}'"),
            None => "NULL".to_string(),
        };
        let parent = match parent_id {
            Some(p) => p.to_string(),
            None => "NULL".to_string(),
        };
        let deleted_at = if deleted {
            "'2026-07-24T12:00:00Z'"
        } else {
            "NULL"
        };
        // Birth-shape provisional defaults from create_inner + soft-deleted running.
        db.execute(sql(format!(
            "INSERT INTO conversation \
             (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized,\
              created_at,updated_at,deleted_at,parent_id,delegation_call_id,\
              external_id,delegation_task_status,delegation_error_code,\
              delegation_started_at,delegation_finished_at,delegation_run_generation,\
              delegation_tool_call_count,delegation_edit_tool_call_count,\
              delegation_touched_files_json,delegation_touched_files_truncated,\
              delegation_additions,delegation_deletions,delegation_line_counts_complete) \
             VALUES \
             ({id},1,'codex','in_progress','delegate',0,0,0,\
              '2026-07-24T11:00:00Z','2026-07-24T11:00:00Z',{deleted_at},\
              {parent},{call},\
              NULL,'running',NULL,\
              '2026-07-24T11:00:00Z',NULL,NULL,\
              0,0,'[]',0,NULL,NULL,0)"
        )))
        .await
        .unwrap_or_else(|e| panic!("seed delegate shell {id}: {e}"));
    }

    async fn apply_repair(db: &sea_orm::DatabaseConnection) {
        Migration
            .up(&SchemaManager::new(db))
            .await
            .expect("repair migration up");
    }

    async fn preflight_count(db: &sea_orm::DatabaseConnection) -> i64 {
        db.query_one(sql(PREFLIGHT_CANDIDATE_SQL))
            .await
            .expect("preflight query")
            .expect("preflight row")
            .try_get("", "count")
            .expect("count")
    }

    struct Projection {
        status: Option<String>,
        error_code: Option<String>,
        finished_at: Option<String>,
        deleted_at: Option<String>,
    }

    async fn load_projection(db: &sea_orm::DatabaseConnection, id: i32) -> Projection {
        let row = db
            .query_one(sql(format!(
                "SELECT delegation_task_status AS status, \
                        delegation_error_code AS error_code, \
                        delegation_finished_at AS finished_at, \
                        deleted_at AS deleted_at \
                 FROM conversation WHERE id = {id}"
            )))
            .await
            .expect("query")
            .unwrap_or_else(|| panic!("missing conversation {id}"));
        Projection {
            status: row.try_get("", "status").expect("status"),
            error_code: row.try_get("", "error_code").expect("error_code"),
            finished_at: row.try_get("", "finished_at").expect("finished_at"),
            deleted_at: row.try_get("", "deleted_at").expect("deleted_at"),
        }
    }

    fn assert_repaired(p: &Projection, id: i32) {
        assert_eq!(
            p.status.as_deref(),
            Some("failed"),
            "id {id} must be failed after repair"
        );
        assert_eq!(
            p.error_code.as_deref(),
            Some(PROVISIONAL_ADMISSION_REJECTED),
            "id {id} must carry provisional_admission_rejected"
        );
        assert_eq!(
            p.finished_at, p.deleted_at,
            "id {id}: finished_at must equal deleted_at"
        );
        assert!(
            p.deleted_at.is_some(),
            "id {id}: repaired row must remain soft-deleted"
        );
    }

    fn assert_unchanged_running_soft_deleted(p: &Projection, id: i32) {
        assert_eq!(
            p.status.as_deref(),
            Some("running"),
            "id {id} near-miss must stay running"
        );
        assert_eq!(
            p.error_code, None,
            "id {id} near-miss must not gain error_code"
        );
        assert_eq!(
            p.finished_at, None,
            "id {id} near-miss must not gain finished_at"
        );
        assert!(
            p.deleted_at.is_some(),
            "id {id} near-miss soft-delete must remain"
        );
    }

    async fn seed_all_fixtures(db: &sea_orm::DatabaseConnection) {
        seed_folder_and_parent(db).await;

        // ---- matches (2) ----------------------------------------------------
        // Pure provisional orphan birth shape.
        seed_delegate_shell(
            db,
            MATCH_PURE_ORPHAN_ID,
            Some("call-orphan-pure"),
            Some(1),
            true,
        )
        .await;
        // Blank / whitespace-only external_id treated as absent.
        seed_delegate_shell(
            db,
            MATCH_BLANK_EXTERNAL_ID,
            Some("call-orphan-blank-ext"),
            Some(1),
            true,
        )
        .await;
        db.execute(sql(format!(
            "UPDATE conversation SET external_id = '  \t\n\r  ' WHERE id = {MATCH_BLANK_EXTERNAL_ID}"
        )))
        .await
        .expect("blank external_id");

        // ---- near-misses (unchanged) ----------------------------------------
        // Non-synthetic start (started_at > created_at).
        seed_delegate_shell(
            db,
            NEAR_NON_SYNTHETIC_START_ID,
            Some("call-near-start"),
            Some(1),
            true,
        )
        .await;
        db.execute(sql(format!(
            "UPDATE conversation SET delegation_started_at = '2026-07-24T11:00:01Z' \
             WHERE id = {NEAR_NON_SYNTHETIC_START_ID}"
        )))
        .await
        .expect("non-synthetic start");

        // Nonzero runtime rollups.
        seed_delegate_shell(
            db,
            NEAR_NONZERO_ROLLUP_ID,
            Some("call-near-rollup"),
            Some(1),
            true,
        )
        .await;
        db.execute(sql(format!(
            "UPDATE conversation SET delegation_tool_call_count = 3 WHERE id = {NEAR_NONZERO_ROLLUP_ID}"
        )))
        .await
        .expect("nonzero rollup");

        // message_count > 0.
        seed_delegate_shell(
            db,
            NEAR_MESSAGE_COUNT_ID,
            Some("call-near-msg"),
            Some(1),
            true,
        )
        .await;
        db.execute(sql(format!(
            "UPDATE conversation SET message_count = 2 WHERE id = {NEAR_MESSAGE_COUNT_ID}"
        )))
        .await
        .expect("message_count");

        // Non-null delegation_run_generation.
        seed_delegate_shell(
            db,
            NEAR_RUN_GENERATION_ID,
            Some("call-near-gen"),
            Some(1),
            true,
        )
        .await;
        db.execute(sql(format!(
            "UPDATE conversation SET delegation_run_generation = 1 WHERE id = {NEAR_RUN_GENERATION_ID}"
        )))
        .await
        .expect("run generation");

        // Null parent_id (not proven broker-linked).
        seed_delegate_shell(db, NEAR_NULL_PARENT_ID, Some("call-near-parent"), None, true).await;

        // Blank whitespace-only delegation_call_id (TRIM+control-char strip).
        seed_delegate_shell(db, NEAR_BLANK_CALL_ID, Some("   \t\n  "), Some(1), true).await;
        // Null delegation_call_id (no broker call identity).
        seed_delegate_shell(db, NEAR_NULL_CALL_ID, None, Some(1), true).await;

        // Has a delegation_task_runs row.
        seed_delegate_shell(db, NEAR_HAS_RUN_ID, Some("call-near-run"), Some(1), true).await;
        db.execute(sql(format!(
            "INSERT INTO delegation_task_runs (\
               task_id, root_task_id, previous_task_id, generation,\
               parent_conversation_id, parent_tool_use_id, child_conversation_id,\
               agent_type, admission_class, lineage_root_task_id,\
               history_only, status, created_at, updated_at\
             ) VALUES (\
               'task-near-run','task-near-run',NULL,1,\
               1,NULL,{NEAR_HAS_RUN_ID},\
               'codex','normal_revision','task-near-run',\
               1,'failed','2026-07-24T11:00:00Z','2026-07-24T11:00:00Z'\
             )"
        )))
        .await
        .expect("seed run row");

        // Nonblank external_id (session evidence).
        seed_delegate_shell(
            db,
            NEAR_NONBLANK_EXTERNAL_ID,
            Some("call-near-ext"),
            Some(1),
            true,
        )
        .await;
        db.execute(sql(format!(
            "UPDATE conversation SET external_id = 'session-real' WHERE id = {NEAR_NONBLANK_EXTERNAL_ID}"
        )))
        .await
        .expect("nonblank external");

        // Not soft-deleted (still visible running provisional).
        seed_delegate_shell(
            db,
            NEAR_NOT_SOFT_DELETED_ID,
            Some("call-near-live"),
            Some(1),
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn migrator_registers_provisional_orphan_repair_after_task_runs() {
        let migrations = Migrator::migrations();
        let names: Vec<&str> = migrations.iter().map(|m| m.name()).collect();
        let orphan_idx = names
            .iter()
            .position(|n| n.contains("provisional_orphan_repair"))
            .expect("provisional_orphan_repair must be registered");
        let task_runs_idx = names
            .iter()
            .position(|n| n.contains("delegation_task_runs"))
            .expect("delegation_task_runs must be registered");
        assert!(
            orphan_idx > task_runs_idx,
            "orphan repair must run after m20260723_000001_delegation_task_runs"
        );
        assert_eq!(
            names[orphan_idx], "m20260724_000001_provisional_orphan_repair",
            "migration DeriveMigrationName must match file stem"
        );
    }

    #[tokio::test]
    async fn preflight_candidate_count_matches_seeded_fixture_set() {
        let db = open_through_prior_migrations().await;
        seed_all_fixtures(&db).await;
        assert_eq!(
            preflight_count(&db).await,
            EXPECTED_PREFLIGHT_CANDIDATES,
            "seeded fixture set must yield exactly {EXPECTED_PREFLIGHT_CANDIDATES} repair candidates"
        );
    }

    #[tokio::test]
    async fn repairs_broker_linked_provisional_orphans_and_excludes_near_misses() {
        let db = open_through_prior_migrations().await;
        seed_all_fixtures(&db).await;
        assert_eq!(preflight_count(&db).await, EXPECTED_PREFLIGHT_CANDIDATES);

        apply_repair(&db).await;

        // Matches repaired.
        assert_repaired(&load_projection(&db, MATCH_PURE_ORPHAN_ID).await, MATCH_PURE_ORPHAN_ID);
        assert_repaired(
            &load_projection(&db, MATCH_BLANK_EXTERNAL_ID).await,
            MATCH_BLANK_EXTERNAL_ID,
        );
        // finished_at equals the soft-delete timestamp used by the seed.
        let pure = load_projection(&db, MATCH_PURE_ORPHAN_ID).await;
        assert_eq!(
            pure.finished_at.as_deref(),
            Some("2026-07-24T12:00:00Z"),
            "finished_at must copy deleted_at exactly"
        );

        // Near-misses unchanged.
        for id in [
            NEAR_NON_SYNTHETIC_START_ID,
            NEAR_NONZERO_ROLLUP_ID,
            NEAR_MESSAGE_COUNT_ID,
            NEAR_RUN_GENERATION_ID,
            NEAR_NULL_PARENT_ID,
            NEAR_BLANK_CALL_ID,
            NEAR_NULL_CALL_ID,
            NEAR_HAS_RUN_ID,
            NEAR_NONBLANK_EXTERNAL_ID,
        ] {
            assert_unchanged_running_soft_deleted(&load_projection(&db, id).await, id);
        }

        // Live (not soft-deleted) provisional stays running with no finish.
        let live = load_projection(&db, NEAR_NOT_SOFT_DELETED_ID).await;
        assert_eq!(live.status.as_deref(), Some("running"));
        assert_eq!(live.error_code, None);
        assert_eq!(live.finished_at, None);
        assert_eq!(live.deleted_at, None);

        // After repair, preflight candidates drop to zero (idempotent shape).
        assert_eq!(
            preflight_count(&db).await,
            0,
            "repaired rows leave the candidate set"
        );
    }

    #[tokio::test]
    async fn repair_is_idempotent_on_re_run() {
        let db = open_through_prior_migrations().await;
        seed_all_fixtures(&db).await;
        apply_repair(&db).await;
        let after_first = load_projection(&db, MATCH_PURE_ORPHAN_ID).await;
        apply_repair(&db).await;
        let after_second = load_projection(&db, MATCH_PURE_ORPHAN_ID).await;
        assert_eq!(after_first.status, after_second.status);
        assert_eq!(after_first.error_code, after_second.error_code);
        assert_eq!(after_first.finished_at, after_second.finished_at);
        assert_repaired(&after_second, MATCH_PURE_ORPHAN_ID);
        // Near-miss still untouched after second pass.
        assert_unchanged_running_soft_deleted(
            &load_projection(&db, NEAR_NON_SYNTHETIC_START_ID).await,
            NEAR_NON_SYNTHETIC_START_ID,
        );
    }

    #[tokio::test]
    async fn down_is_noop_and_does_not_restore_invalid_running() {
        let db = open_through_prior_migrations().await;
        seed_all_fixtures(&db).await;
        apply_repair(&db).await;
        let repaired = load_projection(&db, MATCH_PURE_ORPHAN_ID).await;
        assert_repaired(&repaired, MATCH_PURE_ORPHAN_ID);

        Migration
            .down(&SchemaManager::new(&db))
            .await
            .expect("down must succeed as no-op");

        let after_down = load_projection(&db, MATCH_PURE_ORPHAN_ID).await;
        assert_eq!(
            after_down.status.as_deref(),
            Some("failed"),
            "down must not restore soft-deleted running state"
        );
        assert_eq!(
            after_down.error_code.as_deref(),
            Some(PROVISIONAL_ADMISSION_REJECTED)
        );
        assert_eq!(after_down.finished_at, repaired.finished_at);
    }
}
