pub mod entities;
pub mod error;
pub mod migration;
pub mod service;

#[cfg(any(test, feature = "test-utils"))]
pub mod test_helpers;

use std::path::Path;
use std::time::Duration;

use roundtable_protocol::{ErrorCode, ErrorDetails, RtError, RtResult};
use sea_orm::sqlx::pool::PoolConnection;
use sea_orm::sqlx::query_scalar;
use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sea_orm::sqlx::Sqlite;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;

use error::DbError;
use migration::Migrator;

pub struct AppDatabase {
    pub conn: DatabaseConnection,
}

/// Pool open request for the shared SQLite file.
///
/// `url` must be a `sqlite:` URL. Limits match the existing SeaORM pool knobs.
#[derive(Debug, Clone)]
pub struct DbOpenOptions {
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub connect_timeout: Duration,
    pub idle_timeout: Option<Duration>,
}

/// One checked-out physical connection. `synchronous` stays `NORMAL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionProfile {
    pub foreign_keys: bool,
    pub busy_timeout_ms: i64,
    pub journal_mode: String,
    pub synchronous: String,
    pub cache_size: i64,
}

/// Profiles read while that many pool connections were checked out together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionProfileReport {
    pub distinct_connections: usize,
    pub connections: Vec<ConnectionProfile>,
}

/// `seaql_migrations` name for the roundtable logical model.
pub const ROUNDTABLE_MIGRATION_NAME: &str = "m20261003_000001_roundtable";

/// The roundtable tables are part of [`migration::Migrator`]. Registering them
/// does not enable the product gate and does not change `synchronous`.
pub fn roundtable_migration_registered() -> bool {
    migration::Migrator::migrations()
        .iter()
        .any(|migration| migration.name() == ROUNDTABLE_MIGRATION_NAME)
}

pub(crate) fn database_file_name() -> &'static str {
    if cfg!(all(debug_assertions, feature = "tauri-runtime")) {
        "codeg-dev.db"
    } else {
        "codeg.db"
    }
}

pub async fn init_database(
    app_data_dir: impl AsRef<Path>,
    app_version: &str,
) -> Result<AppDatabase, DbError> {
    let app_data_dir = app_data_dir.as_ref();
    std::fs::create_dir_all(app_data_dir)?;

    // Apply any pending restore BEFORE opening a connection — swapping
    // `codeg.db` under a live SQLite handle would corrupt it. A failure here
    // aborts startup loudly (leaving the safety snapshot intact) rather than
    // booting a half-restored data dir.
    match crate::commands::backup::restore::apply_pending_restore_on_startup(app_data_dir) {
        Ok(crate::commands::backup::restore::RestoreApplied::Applied { .. }) => {}
        Ok(crate::commands::backup::restore::RestoreApplied::None) => {}
        Err(e) => return Err(DbError::Io(e)),
    }
    crate::commands::backup::restore::cleanup_transient_dirs(app_data_dir);

    let db_path = app_data_dir.join(database_file_name());
    let db_url = format!(
        "sqlite:{}?mode=rwc",
        urlencoding::encode(&db_path.to_string_lossy())
    );

    // Apply migrations on a dedicated single connection. The runtime pool below
    // keeps several connections open for read concurrency, but sea-orm spreads a
    // migration's statements across whichever pooled connections are free. A
    // statement that references a column an earlier migration just added (e.g.
    // the `is_chat` → `kind` backfill) can then land on a connection whose
    // cached SQLite schema predates the `ALTER TABLE`, producing a flaky
    // `no such column: "is_chat"` under load. One connection observes every DDL
    // change in order, so the schema it compiles against is always current.
    let migrate_conn = connect_configured(&DbOpenOptions {
        url: db_url.clone(),
        max_connections: 1,
        min_connections: 1,
        connect_timeout: Duration::from_secs(10),
        idle_timeout: None,
    })
    .await?;
    Migrator::up(&migrate_conn, None)
        .await
        .map_err(|e| DbError::Migration(e.to_string()))?;
    migrate_conn.close().await?;

    // Runtime connection pool. Migrations are already applied above, so the
    // schema is stable and spreading queries across pooled connections is safe.
    let conn = connect_configured(&DbOpenOptions {
        url: db_url,
        max_connections: 5,
        min_connections: 1,
        connect_timeout: Duration::from_secs(10),
        idle_timeout: Some(Duration::from_secs(300)),
    })
    .await?;

    service::app_metadata_service::update_app_version(&conn, app_version).await?;

    crate::roundtable::migrate_roundtable(&conn)
        .await
        .map_err(|error| DbError::Migration(error.to_string()))?;

    // Publish user-registered ACP agents into the process-global launch
    // registry before anything can ask for agent metadata. This is the single
    // chokepoint every runtime (desktop, server) goes through, so custom agents
    // are live from the first `all_acp_agents()` / `get_agent_meta()` call.
    // A failure here must not block startup — the built-in agents still work.
    if let Err(e) = service::custom_agent_service::hydrate_registry(&conn).await {
        tracing::warn!("[custom-agent] failed to hydrate custom agent registry: {e}");
    }

    // Load user-authorized workspace links before any file command can run, so
    // the workspace path guard follows exactly the symlinks the user created
    // and nothing else. A failure here fails *closed* (registry stays empty:
    // linked subtrees look unreadable) rather than blocking startup.
    match crate::folder_links::hydrate(&conn).await {
        Ok(count) if count > 0 => {
            tracing::info!("[folder-link] hydrated {count} workspace link(s)");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("[folder-link] failed to hydrate workspace links: {e}"),
    }

    Ok(AppDatabase { conn })
}

/// Open a SQLite pool whose sqlx connect hook sets the profile on every
/// physical connection, including connections created after idle close.
///
/// `synchronous=NORMAL` is the process-crash bar. This does not select `FULL`.
pub async fn open_configured_sqlite(options: &DbOpenOptions) -> RtResult<DatabaseConnection> {
    connect_configured(options).await.map_err(db_error_to_rt)
}

/// Check out every slot in the pool at once and read each connection's pragmas.
///
/// A slot cannot be acquired twice, so `distinct_connections` is the number of
/// physical connections held together. The caller must not already be holding
/// the pool's connections.
pub async fn verify_connection_profile(
    conn: &DatabaseConnection,
) -> RtResult<ConnectionProfileReport> {
    read_connection_profile(conn).await.map_err(db_error_to_rt)
}

async fn connect_configured(options: &DbOpenOptions) -> Result<DatabaseConnection, DbError> {
    let mut opts = ConnectOptions::new(options.url.clone());
    opts.max_connections(options.max_connections)
        .min_connections(options.min_connections)
        .connect_timeout(options.connect_timeout)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(install_sqlite_profile);
    if let Some(idle_timeout) = options.idle_timeout {
        opts.idle_timeout(idle_timeout);
    }
    Database::connect(opts).await.map_err(Into::into)
}

/// `journal_mode=WAL` also sticks in the database header. The other settings
/// are per connection and are applied again each time sqlx opens one.
fn install_sqlite_profile(options: SqliteConnectOptions) -> SqliteConnectOptions {
    options
        .busy_timeout(Duration::from_millis(5_000))
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .pragma("cache_size", "-8000")
}

async fn read_connection_profile(
    conn: &DatabaseConnection,
) -> Result<ConnectionProfileReport, DbError> {
    let pool = conn.get_sqlite_connection_pool();
    let slots = pool.options().get_max_connections();
    let mut held = Vec::with_capacity(slots as usize);
    for _ in 0..slots {
        held.push(pool.acquire().await.map_err(sqlx_db_error)?);
    }
    let mut connections = Vec::with_capacity(held.len());
    for connection in &mut held {
        connections.push(read_one_profile(connection).await?);
    }
    let distinct_connections = connections.len();
    drop(held);
    Ok(ConnectionProfileReport {
        distinct_connections,
        connections,
    })
}

async fn read_one_profile(
    connection: &mut PoolConnection<Sqlite>,
) -> Result<ConnectionProfile, DbError> {
    let foreign_keys = pragma_i64(connection, "PRAGMA foreign_keys").await?;
    let busy_timeout_ms = pragma_i64(connection, "PRAGMA busy_timeout").await?;
    let journal_mode = pragma_text(connection, "PRAGMA journal_mode").await?;
    let synchronous_level = pragma_i64(connection, "PRAGMA synchronous").await?;
    let cache_size = pragma_i64(connection, "PRAGMA cache_size").await?;
    Ok(ConnectionProfile {
        foreign_keys: foreign_keys != 0,
        busy_timeout_ms,
        journal_mode: journal_mode.to_ascii_uppercase(),
        synchronous: synchronous_name(synchronous_level)?,
        cache_size,
    })
}

async fn pragma_i64(connection: &mut PoolConnection<Sqlite>, sql: &str) -> Result<i64, DbError> {
    let value: i64 = query_scalar::<Sqlite, i64>(sql)
        .fetch_one(&mut **connection)
        .await
        .map_err(sqlx_db_error)?;
    Ok(value)
}

async fn pragma_text(
    connection: &mut PoolConnection<Sqlite>,
    sql: &str,
) -> Result<String, DbError> {
    let value: String = query_scalar::<Sqlite, String>(sql)
        .fetch_one(&mut **connection)
        .await
        .map_err(sqlx_db_error)?;
    Ok(value)
}

fn synchronous_name(level: i64) -> Result<String, DbError> {
    let name = match level {
        0 => "OFF",
        1 => "NORMAL",
        2 => "FULL",
        3 => "EXTRA",
        _ => {
            return Err(DbError::Database(sea_orm::DbErr::Custom(format!(
                "unknown synchronous level {level}"
            ))))
        }
    };
    Ok(name.to_owned())
}

fn sqlx_db_error(error: impl std::fmt::Display) -> DbError {
    DbError::Database(sea_orm::DbErr::Custom(error.to_string()))
}

fn db_error_to_rt(error: DbError) -> RtError {
    RtError {
        code: ErrorCode::StorageUnavailable,
        message: "Storage is unavailable.".to_owned(),
        retryable: ErrorCode::StorageUnavailable.retryable(),
        current_revision: None,
        details: ErrorDetails {
            reason: Some(error.to_string()),
            field_errors: Vec::new(),
        },
    }
}
