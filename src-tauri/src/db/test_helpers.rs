//! Test scaffolding: fresh in-memory SQLite database + minimal seed helpers.
//! Used by manager + lifecycle tests that need a real DB without touching the
//! filesystem.

use std::collections::BTreeSet;
use std::path::Path;

use sea_orm::{
    ActiveModelTrait, ConnectOptions, ConnectionTrait, Database, DbBackend, Set, Statement,
};
use sea_orm_migration::{MigrationTrait, MigratorTrait};

use crate::db::entities::delegation_workflow::{self, CompletionProtocolMode, WorkflowState};
use crate::db::error::DbError;
use crate::db::migration::Migrator;
use crate::db::service::{conversation_service, folder_service};
use crate::db::AppDatabase;
use crate::models::agent::AgentType;

const PRE_COMPLETION_PROTOCOL_V2_ONLY_MIGRATION: &str = "m20260806_000004_legacy_restart_context";

struct BeforeCompletionProtocolV2Only;

#[async_trait::async_trait]
impl MigratorTrait for BeforeCompletionProtocolV2Only {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        let mut selected = Vec::new();
        for migration in Migrator::migrations() {
            let is_predecessor = migration.name() == PRE_COMPLETION_PROTOCOL_V2_ONLY_MIGRATION;
            selected.push(migration);
            if is_predecessor {
                return selected;
            }
        }
        panic!("missing {PRE_COMPLETION_PROTOCOL_V2_ONLY_MIGRATION}");
    }
}

#[derive(Clone, Debug)]
pub struct HistoricalWorkflowSeed {
    pub workflow_id: String,
    pub parent_conversation_id: i32,
    pub version: i64,
    pub mode: CompletionProtocolMode,
    pub legacy_source_workflow_id: Option<String>,
}

/// On-disk SQLite DB mirroring `init_database` essentials. Backup tests need a
/// real file because `VACUUM INTO` against a `sqlite::memory:` pool routes the
/// snapshot to a separate, empty connection.
pub async fn fresh_disk_db(dir: &Path) -> AppDatabase {
    let path = dir.join("source.db");
    let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
    let conn = Database::connect(url).await.expect("open disk db");
    conn.execute(Statement::from_string(
        DbBackend::Sqlite,
        "PRAGMA journal_mode=WAL;".to_owned(),
    ))
    .await
    .expect("wal pragma");
    conn.execute(Statement::from_string(
        DbBackend::Sqlite,
        "PRAGMA foreign_keys=ON;".to_owned(),
    ))
    .await
    .expect("foreign_keys pragma");
    Migrator::up(&conn, None)
        .await
        .map_err(|e| DbError::Migration(e.to_string()))
        .expect("run migrations");
    AppDatabase { conn }
}

pub async fn fresh_in_memory_db() -> AppDatabase {
    // Bare `sqlite::memory:` is a *private* DB per connection. Even with a
    // pool of 1, some SeaORM/sqlx paths can open a second handle and see an
    // empty schema. Use a unique shared-cache name so every pool connection
    // for this fixture shares one in-memory database, and keep the pool small.
    let name = format!(
        "sqlite:file:codeg-test-{}?mode=memory&cache=shared",
        uuid::Uuid::new_v4()
    );
    let mut opts = ConnectOptions::new(name);
    opts.max_connections(1).min_connections(1);
    let conn = Database::connect(opts)
        .await
        .expect("open in-memory sqlite");
    // Match the production pragma set as closely as needed for migrations.
    conn.execute(Statement::from_string(
        DbBackend::Sqlite,
        "PRAGMA foreign_keys=ON;".to_owned(),
    ))
    .await
    .expect("foreign_keys pragma");
    Migrator::up(&conn, None)
        .await
        .map_err(|e| DbError::Migration(e.to_string()))
        .expect("run migrations");
    AppDatabase { conn }
}

/// Open a database at the last schema version before v2-only triggers are installed.
pub async fn historical_completion_protocol_db_before_v2_only() -> AppDatabase {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("open historical in-memory sqlite");
    conn.execute(Statement::from_string(
        DbBackend::Sqlite,
        "PRAGMA foreign_keys=ON;".to_owned(),
    ))
    .await
    .expect("foreign_keys pragma");
    BeforeCompletionProtocolV2Only::up(&conn, None)
        .await
        .expect("run predecessor migrations");
    AppDatabase { conn }
}

/// Install all migrations that follow the historical fixture boundary.
pub async fn complete_historical_completion_protocol_migrations(db: &AppDatabase) {
    Migrator::up(&db.conn, None)
        .await
        .map_err(|error| DbError::Migration(error.to_string()))
        .expect("run remaining migrations");
}

/// Seed immutable historical workflow headers before installing v2-only triggers.
pub async fn historical_completion_protocol_db(seeds: &[HistoricalWorkflowSeed]) -> AppDatabase {
    let db = historical_completion_protocol_db_before_v2_only().await;

    db.conn
        .execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO folder \
         (id,name,path,last_opened_at,created_at,updated_at,is_open,sort_order,color,kind) \
         VALUES (1,'historical','C:/completion-protocol-history','2026-08-09T00:00:00Z',\
                 '2026-08-09T00:00:00Z','2026-08-09T00:00:00Z',1,1,'inherit','regular')"
                .to_owned(),
        ))
        .await
        .expect("seed historical folder");

    let parent_conversation_ids = seeds
        .iter()
        .map(|seed| seed.parent_conversation_id)
        .collect::<BTreeSet<_>>();
    for parent_conversation_id in parent_conversation_ids {
        db.conn
            .execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO conversation (\
               id,folder_id,agent_type,status,kind,message_count,title_locked,\
               auto_title_finalized,parent_id,created_at,updated_at\
             ) VALUES (?,1,'codex','completed','regular',0,0,0,NULL,\
                       '2026-08-09T00:00:00Z','2026-08-09T00:00:00Z')",
                vec![parent_conversation_id.into()],
            ))
            .await
            .expect("seed historical parent conversation");
    }

    let now = chrono::Utc::now();
    let contains_corrupt_version = seeds.iter().any(|seed| !matches!(seed.version, 1 | 2));
    if contains_corrupt_version {
        db.conn
            .execute_unprepared("PRAGMA ignore_check_constraints = ON")
            .await
            .expect("allow corrupt historical protocol fixtures");
    }
    for seed in seeds {
        delegation_workflow::ActiveModel {
            workflow_id: Set(seed.workflow_id.clone()),
            parent_conversation_id: Set(seed.parent_conversation_id),
            workflow_kind: Set("brainstorm_to_delivery".into()),
            schema_version: Set(1),
            active_manifest_revision: Set(1),
            graph_revision: Set(1),
            workflow_state: Set(WorkflowState::Approved),
            capability_version: Set("workflow_manifest_v1".into()),
            publication_token: Set(format!("historical-{}", seed.workflow_id)),
            supersedes_approved_revision: Set(None),
            structural_revision: Set(1),
            design_fingerprint: Set("historical-design".into()),
            plan_fingerprint: Set("historical-plan".into()),
            block_cause_code: Set(None),
            block_source_manifest_revision: Set(None),
            completion_protocol_version: Set(seed.version),
            completion_protocol_mode: Set(seed.mode.clone()),
            legacy_source_workflow_id: Set(seed.legacy_source_workflow_id.clone()),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("seed historical workflow");
    }
    if contains_corrupt_version {
        db.conn
            .execute_unprepared("PRAGMA ignore_check_constraints = OFF")
            .await
            .expect("restore historical check constraints");
    }

    complete_historical_completion_protocol_migrations(&db).await;
    db
}

pub async fn seed_folder(db: &AppDatabase, path: &str) -> i32 {
    folder_service::add_folder(&db.conn, path)
        .await
        .expect("seed folder")
        .id
}

pub async fn seed_conversation(db: &AppDatabase, folder_id: i32, agent_type: AgentType) -> i32 {
    conversation_service::create(&db.conn, folder_id, agent_type, None, None)
        .await
        .expect("seed conversation")
        .id
}
