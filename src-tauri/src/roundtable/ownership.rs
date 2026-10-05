//! Coordinator file lock. Separate from the automation engine lock.
//!
//! The handle stays open until cleanup finishes. Holding it proves this
//! process owns the coordinator. It does not prove the process tree is idle.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::Duration;

use roundtable_protocol::{ErrorCode, IncarnationId, RtResult};
use sea_orm::{ConnectionTrait, DatabaseConnection};

use super::rt_error;
use super::sandbox::DbIdentity;
use crate::db::{open_configured_sqlite, DbOpenOptions};

pub struct CoordinatorLock {
    file: File,
    path: PathBuf,
}

pub enum LockAttempt {
    Exclusive(CoordinatorLock),
    Taken,
    Unavailable,
}

pub struct OwnedProcess {
    pub pid: u32,
    pub incarnation: IncarnationId,
}

/// A pid can be reused by an unrelated process. Only the incarnation matches.
pub fn matches_instance(observed_pid: u32, observed: &IncarnationId, owned: &OwnedProcess) -> bool {
    let _ = (observed_pid, owned.pid);
    observed == &owned.incarnation
}

pub fn lock_path_for(data_dir: &Path, db: &DbIdentity) -> PathBuf {
    let mut name = String::from("roundtable-");
    for byte in db.canonical().bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            name.push(byte as char);
        } else {
            name.push('_');
        }
    }
    name.push_str(".coordinator.lock");
    data_dir.join(name)
}

pub fn try_acquire(path: &Path) -> LockAttempt {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return LockAttempt::Unavailable;
        }
    }
    let file = match open_lock(path) {
        Ok(file) => file,
        Err(LockAttempt::Taken) => return LockAttempt::Taken,
        Err(LockAttempt::Unavailable) => return LockAttempt::Unavailable,
        Err(LockAttempt::Exclusive(_)) => return LockAttempt::Unavailable,
    };
    LockAttempt::Exclusive(CoordinatorLock {
        file,
        path: path.to_path_buf(),
    })
}

impl CoordinatorLock {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&self) -> &File {
        &self.file
    }
}

fn open_lock(path: &Path) -> Result<File, LockAttempt> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path)
            .map_err(|err| {
                if err.raw_os_error() == Some(32) || err.raw_os_error() == Some(33) {
                    LockAttempt::Taken
                } else {
                    LockAttempt::Unavailable
                }
            })
    }
    #[cfg(not(windows))]
    {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|_| LockAttempt::Unavailable)?;
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&file);
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(LockAttempt::Taken);
        }
        Ok(file)
    }
}

/// History reads when the coordinator lock is already held. No mutation methods.
pub struct RoundtableReadService {
    conn: DatabaseConnection,
}

impl RoundtableReadService {
    pub(crate) fn connection(&self) -> &DatabaseConnection {
        &self.conn
    }
    pub async fn open(db_path: &Path) -> RtResult<Self> {
        let url = format!(
            "sqlite:{}?mode=ro",
            urlencoding::encode(db_path.to_string_lossy().as_ref())
        );
        let conn = open_configured_sqlite(&DbOpenOptions {
            url,
            max_connections: 1,
            min_connections: 1,
            connect_timeout: Duration::from_secs(10),
            idle_timeout: None,
        })
        .await
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "read_only_store"))?;
        Ok(Self { conn })
    }

    pub async fn boot_epoch(&self) -> RtResult<u64> {
        let row = self
            .conn
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT coordinator_boot FROM rt_schema_meta WHERE singleton = 1",
                vec![],
            ))
            .await
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "read_only_store"))?
            .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "read_only_store"))?;
        let boot: i64 = row
            .try_get_by_index(0)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "read_only_store"))?;
        u64::try_from(boot).map_err(|_| rt_error(ErrorCode::InvalidArgument, "boot_epoch"))
    }
}
