//! Shared pool and SQL helpers for roundtable persistence tests.
//!
//! Pools are opened only through the P09a helper. These helpers do not set
//! SQLite pragmas.

use std::time::Duration;

use codeg_lib::db::{open_configured_sqlite, DbOpenOptions};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use tempfile::TempDir;

pub async fn open_pool(max_connections: u32) -> (TempDir, DatabaseConnection) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("roundtable.db");
    let url = format!(
        "sqlite:{}?mode=rwc",
        urlencoding::encode(path.to_string_lossy().as_ref())
    );
    let conn = open_configured_sqlite(&DbOpenOptions {
        url,
        max_connections,
        min_connections: 1,
        connect_timeout: Duration::from_secs(10),
        idle_timeout: None,
    })
    .await
    .expect("open configured sqlite");
    (dir, conn)
}

pub async fn scalar_i64(conn: &DatabaseConnection, sql: &str) -> i64 {
    let row = conn
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            sql.to_owned(),
        ))
        .await
        .expect("query")
        .expect("row");
    row.try_get_by_index(0).expect("i64")
}

pub async fn scalar_text(conn: &DatabaseConnection, sql: &str) -> String {
    let row = conn
        .query_one(Statement::from_string(
            DatabaseBackend::Sqlite,
            sql.to_owned(),
        ))
        .await
        .expect("query")
        .expect("row");
    row.try_get_by_index(0).expect("text")
}

pub async fn rt_table_count(conn: &DatabaseConnection) -> i64 {
    scalar_i64(
        conn,
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name GLOB 'rt_*'",
    )
    .await
}

pub async fn table_sql(conn: &DatabaseConnection, table: &str) -> String {
    scalar_text(
        conn,
        &format!("SELECT sql FROM sqlite_master WHERE type = 'table' AND name = '{table}'"),
    )
    .await
}

pub const LEGACY_SESSIONS: &str = "CREATE TABLE internal_agent_sessions (
    agent_type TEXT NOT NULL,
    external_id TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('title', 'translate')),
    created_at TEXT NOT NULL,
    PRIMARY KEY (agent_type, external_id)
)";

pub async fn install_legacy_sessions(conn: &DatabaseConnection) {
    conn.execute_unprepared(LEGACY_SESSIONS)
        .await
        .expect("legacy sessions");
    conn.execute_unprepared(
        "INSERT INTO internal_agent_sessions (agent_type, external_id, purpose, created_at)
         VALUES ('codex', 'ordinary-session', 'title', '2026-01-01T00:00:00Z')",
    )
    .await
    .expect("legacy row");
}

pub async fn legacy_session_visible(conn: &DatabaseConnection) -> bool {
    scalar_i64(
        conn,
        "SELECT COUNT(*) FROM internal_agent_sessions
         WHERE external_id = 'ordinary-session' AND purpose = 'title'",
    )
    .await
        == 1
}
