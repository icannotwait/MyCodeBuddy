//! Five physical SQLite connections keep the configured profile.
//!
//! sqlx already defaults `foreign_keys=ON` and `busy_timeout` to 5s, and WAL
//! survives in the file header. This test does not treat those as proof that
//! the connect hook ran. `synchronous=NORMAL` (not the SQLite default FULL)
//! and `cache_size=-8000` are the per-connection settings that must be set
//! again after close and reopen.

use std::time::Duration;

use codeg_lib::db::{
    open_configured_sqlite, verify_connection_profile, ConnectionProfileReport, DbOpenOptions,
};
use sea_orm::sqlx::query_scalar;
use sea_orm::sqlx::Sqlite;
use sea_orm::DatabaseConnection;

#[tokio::test]
async fn five_physical_connections_keep_normal_synchronous_and_cache_size() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("profile.db");
    let url = format!(
        "sqlite:{}?mode=rwc",
        urlencoding::encode(path.to_string_lossy().as_ref())
    );
    let options = DbOpenOptions {
        url,
        max_connections: 5,
        min_connections: 1,
        connect_timeout: Duration::from_secs(10),
        idle_timeout: None,
    };

    let conn = open_configured_sqlite(&options)
        .await
        .expect("open configured sqlite");
    assert_five_connections(&conn).await;
    conn.close().await.expect("close pool");

    let conn = open_configured_sqlite(&options)
        .await
        .expect("reopen configured sqlite");
    assert_five_connections(&conn).await;
    conn.close().await.expect("close reopened pool");
}

async fn assert_five_connections(conn: &DatabaseConnection) {
    occupy_five(conn).await;
    let report = verify_connection_profile(conn)
        .await
        .expect("connection profile");
    assert_report(&report);
}

async fn occupy_five(conn: &DatabaseConnection) {
    let pool = conn.get_sqlite_connection_pool();
    let mut held = Vec::with_capacity(5);
    for index in 0..5_i64 {
        let mut connection = pool.acquire().await.expect("acquire");
        sea_orm::sqlx::query::<Sqlite>("CREATE TEMP TABLE codeg_profile_probe (n INTEGER)")
            .execute(&mut *connection)
            .await
            .expect("distinct physical connection");
        sea_orm::sqlx::query::<Sqlite>("INSERT INTO codeg_profile_probe (n) VALUES (?)")
            .bind(index)
            .execute(&mut *connection)
            .await
            .expect("probe insert");
        let probe: i64 = query_scalar::<Sqlite, i64>("SELECT n FROM codeg_profile_probe")
            .fetch_one(&mut *connection)
            .await
            .expect("probe read");
        assert_eq!(probe, index);
        assert_raw_profile(&mut *connection).await;
        held.push(connection);
    }
    assert_eq!(held.len(), 5);
}

async fn assert_raw_profile(connection: &mut sea_orm::sqlx::SqliteConnection) {
    let foreign_keys: i64 = query_scalar::<Sqlite, i64>("PRAGMA foreign_keys")
        .fetch_one(&mut *connection)
        .await
        .expect("foreign_keys");
    let busy_timeout_ms: i64 = query_scalar::<Sqlite, i64>("PRAGMA busy_timeout")
        .fetch_one(&mut *connection)
        .await
        .expect("busy_timeout");
    let journal_mode: String = query_scalar::<Sqlite, String>("PRAGMA journal_mode")
        .fetch_one(&mut *connection)
        .await
        .expect("journal_mode");
    let synchronous: i64 = query_scalar::<Sqlite, i64>("PRAGMA synchronous")
        .fetch_one(&mut *connection)
        .await
        .expect("synchronous");
    let cache_size: i64 = query_scalar::<Sqlite, i64>("PRAGMA cache_size")
        .fetch_one(&mut *connection)
        .await
        .expect("cache_size");

    assert_eq!(foreign_keys, 1);
    assert_eq!(busy_timeout_ms, 5000);
    assert!(journal_mode.eq_ignore_ascii_case("wal"));
    assert_eq!(synchronous, 1, "synchronous must be NORMAL, not FULL");
    assert_eq!(cache_size, -8000);
}

fn assert_report(report: &ConnectionProfileReport) {
    assert_eq!(report.distinct_connections, 5);
    assert_eq!(report.connections.len(), 5);
    assert!(report.connections.iter().all(|connection| {
        connection.foreign_keys
            && connection.busy_timeout_ms == 5000
            && connection.journal_mode == "WAL"
            && connection.synchronous == "NORMAL"
            && connection.cache_size == -8000
    }));
}
