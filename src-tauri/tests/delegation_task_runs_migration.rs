//! Migration + backfill tests for `delegation_task_runs` (Task 1).
//!
//! Seeds pre-migration delegate rows via raw SQL, applies the new migration,
//! and asserts every Durable Run Model backfill rule from the design.

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sea_orm_migration::MigratorTrait;

use codeg_lib::db::migration::Migrator;

fn sql(text: impl Into<String>) -> Statement {
    Statement::from_string(DbBackend::Sqlite, text.into())
}

async fn open_db() -> DatabaseConnection {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    db.execute(sql("PRAGMA foreign_keys=ON;")).await.unwrap();
    db
}

/// Apply every migration *before* `delegation_task_runs`, so seeds use the
/// pre-feature schema.
async fn migrate_before_target(db: &DatabaseConnection) {
    let migrations = <Migrator as MigratorTrait>::migrations();
    let idx = migrations
        .iter()
        .position(|m| m.name().contains("delegation_task_runs"))
        .expect("delegation_task_runs migration registered");
    Migrator::up(db, Some(idx as u32)).await.unwrap();
}

async fn migrate_rest(db: &DatabaseConnection) {
    Migrator::up(db, None).await.unwrap();
}

async fn seed_folder(db: &DatabaseConnection, id: i32, path: &str) {
    db.execute(sql(format!(
        "INSERT INTO folder \
         (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
         VALUES ({id},'repo','{path}','2026-07-01','2026-07-01','2026-07-01',1,1,'inherit','regular')"
    )))
    .await
    .expect("seed folder");
}

/// Minimal parent (regular) conversation. Soft-deleted when `deleted_at` set.
async fn seed_parent(
    db: &DatabaseConnection,
    id: i32,
    folder_id: i32,
    deleted_at: Option<&str>,
) {
    let deleted = match deleted_at {
        Some(ts) => format!("'{ts}'"),
        None => "NULL".to_string(),
    };
    db.execute(sql(format!(
        "INSERT INTO conversation \
         (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized, \
          created_at,updated_at,deleted_at) \
         VALUES ({id},{folder_id},'codex','completed','regular',0,0,0, \
                 '2026-07-01','2026-07-01',{deleted})"
    )))
    .await
    .expect("seed parent");
}

struct DelegateSeed {
    id: i32,
    folder_id: i32,
    parent_id: i32,
    agent_type: &'static str,
    status: &'static str,
    call_id: Option<&'static str>,
    parent_tool_use_id: Option<&'static str>,
    external_id: Option<&'static str>,
    task_status: Option<&'static str>,
    started_at: Option<&'static str>,
    finished_at: Option<&'static str>,
    created_at: &'static str,
    updated_at: &'static str,
    deleted_at: Option<&'static str>,
}

async fn seed_delegate(db: &DatabaseConnection, s: DelegateSeed) {
    let call_id = match s.call_id {
        Some(v) => format!("'{v}'"),
        None => "NULL".to_string(),
    };
    let parent_tool = match s.parent_tool_use_id {
        Some(v) => format!("'{v}'"),
        None => "NULL".to_string(),
    };
    let external = match s.external_id {
        Some(v) => format!("'{v}'"),
        None => "NULL".to_string(),
    };
    let task_status = match s.task_status {
        Some(v) => format!("'{v}'"),
        None => "NULL".to_string(),
    };
    let started = match s.started_at {
        Some(v) => format!("'{v}'"),
        None => "NULL".to_string(),
    };
    let finished = match s.finished_at {
        Some(v) => format!("'{v}'"),
        None => "NULL".to_string(),
    };
    let deleted = match s.deleted_at {
        Some(v) => format!("'{v}'"),
        None => "NULL".to_string(),
    };
    db.execute(sql(format!(
        "INSERT INTO conversation \
         (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized, \
          created_at,updated_at,deleted_at,parent_id,parent_tool_use_id,delegation_call_id, \
          external_id,delegation_task_status,delegation_started_at,delegation_finished_at) \
         VALUES ({id},{folder_id},'{agent}','{status}','delegate',0,0,0, \
                 '{created}','{updated}',{deleted},{parent},{parent_tool},{call_id}, \
                 {external},{task_status},{started},{finished})",
        id = s.id,
        folder_id = s.folder_id,
        agent = s.agent_type,
        status = s.status,
        created = s.created_at,
        updated = s.updated_at,
        deleted = deleted,
        parent = s.parent_id,
        parent_tool = parent_tool,
        call_id = call_id,
        external = external,
        task_status = task_status,
        started = started,
        finished = finished,
    )))
    .await
    .expect("seed delegate");
}

async fn run_count(db: &DatabaseConnection) -> i64 {
    let row = db
        .query_one(sql("SELECT COUNT(*) AS c FROM delegation_task_runs"))
        .await
        .unwrap()
        .unwrap();
    row.try_get::<i64>("", "c").unwrap()
}

async fn load_run(db: &DatabaseConnection, task_id: &str) -> Option<sea_orm::QueryResult> {
    db.query_one(sql(format!(
        "SELECT * FROM delegation_task_runs WHERE task_id = '{task_id}'"
    )))
    .await
    .unwrap()
}

async fn load_run_for_child(db: &DatabaseConnection, child_id: i32) -> Option<sea_orm::QueryResult> {
    db.query_one(sql(format!(
        "SELECT * FROM delegation_task_runs WHERE child_conversation_id = {child_id}"
    )))
    .await
    .unwrap()
}

fn col_str(row: &sea_orm::QueryResult, name: &str) -> Option<String> {
    row.try_get::<Option<String>>("", name).unwrap()
}

fn col_i64(row: &sea_orm::QueryResult, name: &str) -> Option<i64> {
    row.try_get::<Option<i64>>("", name).unwrap()
}

fn col_bool(row: &sea_orm::QueryResult, name: &str) -> bool {
    // SQLite stores bool as integer 0/1.
    row.try_get::<i64>("", name).unwrap() != 0
}

#[tokio::test]
async fn task_id_equals_delegation_call_id_for_gen1() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-call-id").await;
    seed_parent(&db, 1, 1, None).await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 10,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("call-gen1-aaa"),
            parent_tool_use_id: Some("tool-aaa"),
            external_id: Some("ext-aaa"),
            task_status: Some("completed"),
            started_at: Some("2026-07-10T10:00:00Z"),
            finished_at: Some("2026-07-10T10:05:00Z"),
            created_at: "2026-07-10T10:00:00Z",
            updated_at: "2026-07-10T10:05:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    let run = load_run(&db, "call-gen1-aaa").await.expect("run row");
    assert_eq!(col_str(&run, "task_id").as_deref(), Some("call-gen1-aaa"));
    assert_eq!(col_str(&run, "root_task_id").as_deref(), Some("call-gen1-aaa"));
    assert_eq!(col_str(&run, "previous_task_id"), None);
    assert_eq!(col_i64(&run, "generation"), Some(1));
    assert_eq!(col_i64(&run, "child_conversation_id"), Some(10));
    assert_eq!(col_i64(&run, "parent_conversation_id"), Some(1));
    assert_eq!(col_str(&run, "agent_type").as_deref(), Some("codex"));

    let gen = db
        .query_one(sql(
            "SELECT delegation_run_generation AS g FROM conversation WHERE id = 10",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gen.try_get::<Option<i64>>("", "g").unwrap(), Some(1));
}

#[tokio::test]
async fn duplicate_call_id_keeps_newest_non_deleted_only() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-dup-call").await;
    seed_parent(&db, 1, 1, None).await;

    // Older child with the same call_id — must NOT receive a run (PK would collide).
    seed_delegate(
        &db,
        DelegateSeed {
            id: 11,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("dup-call"),
            parent_tool_use_id: Some("tool-old"),
            external_id: Some("ext-old"),
            task_status: Some("completed"),
            started_at: Some("2026-07-01T00:00:00Z"),
            finished_at: Some("2026-07-01T00:01:00Z"),
            created_at: "2026-07-01T00:00:00Z",
            updated_at: "2026-07-01T00:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    // Newer child wins.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 12,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("dup-call"),
            parent_tool_use_id: Some("tool-new"),
            external_id: Some("ext-new"),
            task_status: Some("completed"),
            started_at: Some("2026-07-02T00:00:00Z"),
            finished_at: Some("2026-07-02T00:01:00Z"),
            created_at: "2026-07-02T00:00:00Z",
            updated_at: "2026-07-02T00:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    assert_eq!(run_count(&db).await, 1);
    let run = load_run(&db, "dup-call").await.expect("winner run");
    assert_eq!(col_i64(&run, "child_conversation_id"), Some(12));
    assert!(load_run_for_child(&db, 11).await.is_none());

    let loser_gen = db
        .query_one(sql(
            "SELECT delegation_run_generation AS g FROM conversation WHERE id = 11",
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        loser_gen.try_get::<Option<i64>>("", "g").unwrap(),
        None,
        "call-id losers skip run insert and keep null generation"
    );
}

#[tokio::test]
async fn status_map_from_conversation_and_existing_task_status() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-status").await;
    seed_parent(&db, 1, 1, None).await;

    // Null task status → map from conversation status.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 20,
            folder_id: 1,
            parent_id: 1,
            agent_type: "claude_code",
            status: "in_progress",
            call_id: Some("st-running"),
            parent_tool_use_id: Some("t-run"),
            external_id: Some("e-run"),
            task_status: None,
            started_at: None,
            finished_at: None,
            created_at: "2026-07-10T01:00:00Z",
            updated_at: "2026-07-10T01:00:00Z",
            deleted_at: None,
        },
    )
    .await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 21,
            folder_id: 1,
            parent_id: 1,
            agent_type: "claude_code",
            status: "pending_review",
            call_id: Some("st-pending"),
            parent_tool_use_id: Some("t-pending"),
            external_id: Some("e-pending"),
            task_status: None,
            started_at: Some("2026-07-10T02:00:00Z"),
            finished_at: Some("2026-07-10T02:05:00Z"),
            created_at: "2026-07-10T02:00:00Z",
            updated_at: "2026-07-10T02:05:00Z",
            deleted_at: None,
        },
    )
    .await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 22,
            folder_id: 1,
            parent_id: 1,
            agent_type: "claude_code",
            status: "completed",
            call_id: Some("st-completed"),
            parent_tool_use_id: Some("t-done"),
            external_id: Some("e-done"),
            task_status: None,
            started_at: Some("2026-07-10T03:00:00Z"),
            finished_at: Some("2026-07-10T03:05:00Z"),
            created_at: "2026-07-10T03:00:00Z",
            updated_at: "2026-07-10T03:05:00Z",
            deleted_at: None,
        },
    )
    .await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 23,
            folder_id: 1,
            parent_id: 1,
            agent_type: "claude_code",
            status: "cancelled",
            call_id: Some("st-canceled"),
            parent_tool_use_id: Some("t-cancel"),
            external_id: Some("e-cancel"),
            task_status: None,
            started_at: Some("2026-07-10T04:00:00Z"),
            finished_at: Some("2026-07-10T04:01:00Z"),
            created_at: "2026-07-10T04:00:00Z",
            updated_at: "2026-07-10T04:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    // Existing task status wins over conversation status.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 24,
            folder_id: 1,
            parent_id: 1,
            agent_type: "claude_code",
            status: "completed",
            call_id: Some("st-failed"),
            parent_tool_use_id: Some("t-fail"),
            external_id: Some("e-fail"),
            task_status: Some("failed"),
            started_at: Some("2026-07-10T05:00:00Z"),
            finished_at: Some("2026-07-10T05:01:00Z"),
            created_at: "2026-07-10T05:00:00Z",
            updated_at: "2026-07-10T05:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    assert_eq!(
        col_str(&load_run(&db, "st-running").await.unwrap(), "status").as_deref(),
        Some("running")
    );
    assert_eq!(
        col_str(&load_run(&db, "st-pending").await.unwrap(), "status").as_deref(),
        Some("completed")
    );
    assert_eq!(
        col_str(&load_run(&db, "st-completed").await.unwrap(), "status").as_deref(),
        Some("completed")
    );
    assert_eq!(
        col_str(&load_run(&db, "st-canceled").await.unwrap(), "status").as_deref(),
        Some("canceled")
    );
    assert_eq!(
        col_str(&load_run(&db, "st-failed").await.unwrap(), "status").as_deref(),
        Some("failed")
    );
}

#[tokio::test]
async fn empty_parent_tool_use_id_becomes_null_history_only() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-empty-tool").await;
    seed_parent(&db, 1, 1, None).await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 30,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("empty-tool"),
            parent_tool_use_id: Some(""),
            external_id: Some("ext-empty-tool"),
            task_status: Some("completed"),
            started_at: Some("2026-07-11T00:00:00Z"),
            finished_at: Some("2026-07-11T00:01:00Z"),
            created_at: "2026-07-11T00:00:00Z",
            updated_at: "2026-07-11T00:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 31,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("null-tool"),
            parent_tool_use_id: None,
            external_id: Some("ext-null-tool"),
            task_status: Some("completed"),
            started_at: Some("2026-07-11T01:00:00Z"),
            finished_at: Some("2026-07-11T01:01:00Z"),
            created_at: "2026-07-11T01:00:00Z",
            updated_at: "2026-07-11T01:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    for task in ["empty-tool", "null-tool"] {
        let run = load_run(&db, task).await.expect(task);
        assert_eq!(col_str(&run, "parent_tool_use_id"), None);
        assert!(col_bool(&run, "history_only"));
    }
}

#[tokio::test]
async fn duplicate_parent_tool_use_id_losers_history_only_with_legacy() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-dup-tool").await;
    seed_parent(&db, 1, 1, None).await;

    seed_delegate(
        &db,
        DelegateSeed {
            id: 40,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("call-tool-old"),
            parent_tool_use_id: Some("shared-tool"),
            external_id: Some("ext-tool-old"),
            task_status: Some("completed"),
            started_at: Some("2026-07-12T00:00:00Z"),
            finished_at: Some("2026-07-12T00:01:00Z"),
            created_at: "2026-07-12T00:00:00Z",
            updated_at: "2026-07-12T00:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 41,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("call-tool-new"),
            parent_tool_use_id: Some("shared-tool"),
            external_id: Some("ext-tool-new"),
            task_status: Some("completed"),
            started_at: Some("2026-07-12T02:00:00Z"),
            finished_at: Some("2026-07-12T02:01:00Z"),
            created_at: "2026-07-12T02:00:00Z",
            updated_at: "2026-07-12T02:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    assert_eq!(run_count(&db).await, 2);

    let winner = load_run(&db, "call-tool-new").await.unwrap();
    assert_eq!(
        col_str(&winner, "parent_tool_use_id").as_deref(),
        Some("shared-tool")
    );
    assert_eq!(col_str(&winner, "legacy_parent_tool_use_id"), None);

    let loser = load_run(&db, "call-tool-old").await.unwrap();
    assert_eq!(col_str(&loser, "parent_tool_use_id"), None);
    assert_eq!(
        col_str(&loser, "legacy_parent_tool_use_id").as_deref(),
        Some("shared-tool")
    );
    assert!(col_bool(&loser, "history_only"));
}

#[tokio::test]
async fn missing_external_id_is_history_only_non_continuable() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-no-ext").await;
    seed_parent(&db, 1, 1, None).await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 50,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("no-ext"),
            parent_tool_use_id: Some("tool-no-ext"),
            external_id: None,
            task_status: Some("completed"),
            started_at: Some("2026-07-13T00:00:00Z"),
            finished_at: Some("2026-07-13T00:01:00Z"),
            created_at: "2026-07-13T00:00:00Z",
            updated_at: "2026-07-13T00:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    let run = load_run(&db, "no-ext").await.unwrap();
    assert!(col_bool(&run, "history_only"));
    assert_eq!(col_str(&run, "workspace_path"), None);
    assert_eq!(col_str(&run, "route_fingerprint"), None);
    assert_eq!(col_str(&run, "mode_id"), None);
    assert_eq!(col_str(&run, "config_values_json"), None);
    assert_eq!(col_str(&run, "launch_snapshot_version"), None);
}

#[tokio::test]
async fn deleted_parent_still_backfills_child_history() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-del-parent").await;
    seed_parent(&db, 1, 1, Some("2026-07-14T00:00:00Z")).await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 60,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("del-parent-child"),
            parent_tool_use_id: Some("tool-del-parent"),
            external_id: Some("ext-del-parent"),
            task_status: Some("completed"),
            started_at: Some("2026-07-13T00:00:00Z"),
            finished_at: Some("2026-07-13T00:01:00Z"),
            created_at: "2026-07-13T00:00:00Z",
            updated_at: "2026-07-13T00:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    let run = load_run(&db, "del-parent-child")
        .await
        .expect("child history backfilled under deleted parent");
    assert_eq!(col_i64(&run, "parent_conversation_id"), Some(1));
    assert_eq!(col_i64(&run, "child_conversation_id"), Some(60));
}

#[tokio::test]
async fn missing_launch_snapshot_is_non_continuable_null_fields() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-no-snap").await;
    seed_parent(&db, 1, 1, None).await;
    // Even with external_id + parent tool, pre-feature rows have no durable
    // launch snapshot → history_only and null snapshot fields.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 70,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("no-snap"),
            parent_tool_use_id: Some("tool-no-snap"),
            external_id: Some("ext-no-snap"),
            task_status: Some("completed"),
            started_at: Some("2026-07-15T00:00:00Z"),
            finished_at: Some("2026-07-15T00:01:00Z"),
            created_at: "2026-07-15T00:00:00Z",
            updated_at: "2026-07-15T00:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    let run = load_run(&db, "no-snap").await.unwrap();
    assert!(
        col_bool(&run, "history_only"),
        "missing reconstructible launch snapshot ⇒ non-continuable"
    );
    assert_eq!(col_str(&run, "workspace_path"), None);
    assert_eq!(col_str(&run, "route_fingerprint"), None);
    assert_eq!(col_str(&run, "mode_id"), None);
    assert_eq!(col_str(&run, "config_values_json"), None);
    assert_eq!(col_str(&run, "launch_snapshot_version"), None);
    assert_eq!(col_str(&run, "task_preview"), None);
    assert_eq!(col_str(&run, "request_fingerprint"), None);
}

#[tokio::test]
async fn lineage_root_and_admission_class_for_original_gen1() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-lineage").await;
    seed_parent(&db, 1, 1, None).await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 80,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("lineage-root"),
            parent_tool_use_id: Some("tool-lineage"),
            external_id: Some("ext-lineage"),
            task_status: Some("completed"),
            started_at: Some("2026-07-16T00:00:00Z"),
            finished_at: Some("2026-07-16T00:01:00Z"),
            created_at: "2026-07-16T00:00:00Z",
            updated_at: "2026-07-16T00:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    let run = load_run(&db, "lineage-root").await.unwrap();
    assert_eq!(
        col_str(&run, "lineage_root_task_id").as_deref(),
        Some("lineage-root")
    );
    assert_eq!(
        col_str(&run, "admission_class").as_deref(),
        Some("normal_revision")
    );
}

#[tokio::test]
async fn reached_running_at_from_prior_admission_not_invented_for_non_admitted() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-reached").await;
    seed_parent(&db, 1, 1, None).await;

    // in_progress → running with started_at evidence of admission.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 90,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "in_progress",
            call_id: Some("reached-running"),
            parent_tool_use_id: Some("tool-reached"),
            external_id: Some("ext-reached"),
            task_status: Some("running"),
            started_at: Some("2026-07-17T10:00:00Z"),
            finished_at: None,
            created_at: "2026-07-17T09:59:00Z",
            updated_at: "2026-07-17T10:00:00Z",
            deleted_at: None,
        },
    )
    .await;
    // Completed with started_at.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 91,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("reached-done"),
            parent_tool_use_id: Some("tool-done"),
            external_id: Some("ext-done"),
            task_status: Some("completed"),
            started_at: Some("2026-07-17T11:00:00Z"),
            finished_at: Some("2026-07-17T11:05:00Z"),
            created_at: "2026-07-17T10:59:00Z",
            updated_at: "2026-07-17T11:05:00Z",
            deleted_at: None,
        },
    )
    .await;
    // history_only non-admitted: no external_id, no started_at — do not invent.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 92,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("no-admission"),
            parent_tool_use_id: Some("tool-no-adm"),
            external_id: None,
            task_status: None,
            started_at: None,
            finished_at: None,
            created_at: "2026-07-17T12:00:00Z",
            updated_at: "2026-07-17T12:00:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    let running = load_run(&db, "reached-running").await.unwrap();
    assert_eq!(
        col_str(&running, "reached_running_at").as_deref(),
        Some("2026-07-17T10:00:00Z")
    );

    let done = load_run(&db, "reached-done").await.unwrap();
    assert_eq!(
        col_str(&done, "reached_running_at").as_deref(),
        Some("2026-07-17T11:00:00Z")
    );

    let no_adm = load_run(&db, "no-admission").await.unwrap();
    assert!(col_bool(&no_adm, "history_only"));
    assert_eq!(
        col_str(&no_adm, "reached_running_at"),
        None,
        "never invent reached_running_at for history_only non-admitted rows"
    );
}

/// Legacy migration m20260716_000003 synthesized
/// `delegation_started_at = created_at`. That alone is not proof of prompt
/// admission — especially for history-only rows that never got an external_id.
#[tokio::test]
async fn synthetic_started_at_equals_created_at_without_external_id_leaves_reached_running_null() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-synthetic-start").await;
    seed_parent(&db, 1, 1, None).await;

    // Synthetic started_at == created_at, no external_id → do not treat as admitted.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 93,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("synthetic-start"),
            parent_tool_use_id: Some("tool-synth"),
            external_id: None,
            task_status: Some("completed"),
            started_at: Some("2026-07-17T13:00:00Z"),
            finished_at: Some("2026-07-17T13:01:00Z"),
            created_at: "2026-07-17T13:00:00Z",
            updated_at: "2026-07-17T13:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    // Real start strictly after created_at still counts even without external_id.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 94,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("real-start-no-ext"),
            parent_tool_use_id: Some("tool-real-start"),
            external_id: None,
            task_status: Some("completed"),
            started_at: Some("2026-07-17T14:01:00Z"),
            finished_at: Some("2026-07-17T14:05:00Z"),
            created_at: "2026-07-17T14:00:00Z",
            updated_at: "2026-07-17T14:05:00Z",
            deleted_at: None,
        },
    )
    .await;
    // external_id present: started_at may equal created_at and still count.
    seed_delegate(
        &db,
        DelegateSeed {
            id: 95,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("ext-synthetic-start"),
            parent_tool_use_id: Some("tool-ext-synth"),
            external_id: Some("ext-synth"),
            task_status: Some("completed"),
            started_at: Some("2026-07-17T15:00:00Z"),
            finished_at: Some("2026-07-17T15:01:00Z"),
            created_at: "2026-07-17T15:00:00Z",
            updated_at: "2026-07-17T15:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    let synth = load_run(&db, "synthetic-start").await.unwrap();
    assert!(col_bool(&synth, "history_only"));
    assert_eq!(
        col_str(&synth, "reached_running_at"),
        None,
        "synthetic started_at = created_at without external_id is not admission"
    );

    let real = load_run(&db, "real-start-no-ext").await.unwrap();
    assert_eq!(
        col_str(&real, "reached_running_at").as_deref(),
        Some("2026-07-17T14:01:00Z"),
        "started_at strictly after created_at is strong enough without external_id"
    );

    let with_ext = load_run(&db, "ext-synthetic-start").await.unwrap();
    assert_eq!(
        col_str(&with_ext, "reached_running_at").as_deref(),
        Some("2026-07-17T15:00:00Z"),
        "external_id is sufficient admission even when started_at equals created_at"
    );
}

/// Blank (whitespace-only) delegation_call_id is treated as null and skipped —
/// not inserted as a history_only run with an empty task_id.
#[tokio::test]
async fn blank_delegation_call_id_is_skipped_like_null() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    seed_folder(&db, 1, "/tmp/task-runs-blank-call").await;
    seed_parent(&db, 1, 1, None).await;

    seed_delegate(
        &db,
        DelegateSeed {
            id: 96,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some(""),
            parent_tool_use_id: Some("tool-blank-call"),
            external_id: Some("ext-blank-call"),
            task_status: Some("completed"),
            started_at: Some("2026-07-17T16:00:00Z"),
            finished_at: Some("2026-07-17T16:01:00Z"),
            created_at: "2026-07-17T16:00:00Z",
            updated_at: "2026-07-17T16:01:00Z",
            deleted_at: None,
        },
    )
    .await;
    seed_delegate(
        &db,
        DelegateSeed {
            id: 97,
            folder_id: 1,
            parent_id: 1,
            agent_type: "codex",
            status: "completed",
            call_id: Some("   "),
            parent_tool_use_id: Some("tool-ws-call"),
            external_id: Some("ext-ws-call"),
            task_status: Some("completed"),
            started_at: Some("2026-07-17T16:10:00Z"),
            finished_at: Some("2026-07-17T16:11:00Z"),
            created_at: "2026-07-17T16:10:00Z",
            updated_at: "2026-07-17T16:11:00Z",
            deleted_at: None,
        },
    )
    .await;
    migrate_rest(&db).await;

    assert_eq!(run_count(&db).await, 0);
    assert!(load_run_for_child(&db, 96).await.is_none());
    assert!(load_run_for_child(&db, 97).await.is_none());

    for child_id in [96i32, 97] {
        let gen = db
            .query_one(sql(format!(
                "SELECT delegation_run_generation AS g FROM conversation WHERE id = {child_id}"
            )))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            gen.try_get::<Option<i64>>("", "g").unwrap(),
            None,
            "blank call_id rows keep null generation"
        );
    }
}

#[tokio::test]
async fn creates_budget_tables_and_required_indexes() {
    let db = open_db().await;
    migrate_before_target(&db).await;
    migrate_rest(&db).await;

    // conversation column present
    let cols = db
        .query_all(sql("PRAGMA table_info(conversation)"))
        .await
        .unwrap();
    let names: Vec<String> = cols
        .iter()
        .map(|r| r.try_get::<String>("", "name").unwrap())
        .collect();
    assert!(
        names.iter().any(|n| n == "delegation_run_generation"),
        "conversation.delegation_run_generation missing"
    );

    // Seed parent before FK-backed budget / run inserts.
    seed_folder(&db, 1, "/tmp/task-runs-indexes").await;
    seed_parent(&db, 1, 1, None).await;

    // Tables exist and accept inserts.
    db.execute(sql(
        "INSERT INTO delegation_lineage_budgets \
         (lineage_root_task_id, unexpected_continue_count, replacement_count) \
         VALUES ('root-1', 0, 0)",
    ))
    .await
    .expect("lineage budget insert");
    db.execute(sql(
        "INSERT INTO delegation_work_unit_budgets \
         (parent_conversation_id, work_unit_key, unexpected_continue_count, replacement_count) \
         VALUES (1, 'wu-1', 0, 0)",
    ))
    .await
    .expect("work-unit budget insert");
    db.execute(sql(
        "INSERT INTO conversation \
         (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized, \
          created_at,updated_at) \
         VALUES (200,1,'codex','completed','regular',0,0,0,'2026-07-18','2026-07-18')",
    ))
    .await
    .unwrap();
    db.execute(sql(
        "INSERT INTO conversation \
         (id,folder_id,agent_type,status,kind,message_count,title_locked,auto_title_finalized, \
          created_at,updated_at,parent_id) \
         VALUES (201,1,'codex','in_progress','delegate',0,0,0,'2026-07-18','2026-07-18',200)",
    ))
    .await
    .unwrap();

    let insert_run = |task_id: &str, status: &str, generation: i64| {
        sql(format!(
            "INSERT INTO delegation_task_runs \
             (task_id,root_task_id,previous_task_id,generation,parent_conversation_id, \
              parent_tool_use_id,child_conversation_id,agent_type,admission_class, \
              lineage_root_task_id,history_only,status,started_at,created_at,updated_at) \
             VALUES ('{task_id}','{task_id}',NULL,{generation},200,'tool-{task_id}',201,'codex', \
                     'normal_revision','{task_id}',0,'{status}','2026-07-18','2026-07-18','2026-07-18')"
        ))
    };
    db.execute(insert_run("idx-run-1", "running", 1))
        .await
        .unwrap();
    assert!(
        db.execute(insert_run("idx-run-2", "reserving", 2))
            .await
            .is_err(),
        "partial unique: one non-terminal run per child"
    );
}
