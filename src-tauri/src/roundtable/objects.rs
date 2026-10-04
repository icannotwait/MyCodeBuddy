//! Content-addressed snapshot objects.
//!
//! `put` writes a temp file, fsyncs it, renames it to the content hash, then
//! fsyncs the directory. A failure before that sequence finishes does not
//! leave a committed object reference. The storage reservation is released
//! only after an unreferenced partial is confirmed gone.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use roundtable_protocol::{ErrorCode, Hash256, PrincipalId, RtResult, MAX_SAFE_INTEGER};
use serde::{Deserialize, Serialize};

use super::feature_gate::fsync_dir;
use super::rt_error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectFault {
    None,
    Write,
    FileFsync,
    Rename,
    DirFsync,
    Hash,
    DbCommit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub object_id: String,
    pub content_hash: Hash256,
    pub total_bytes: u64,
}

struct LedgerState {
    reserved: u64,
    used: u64,
    by_principal: BTreeMap<PrincipalId, u64>,
}

/// Global capture quota. Reservations are checked and recorded under one lock
/// so two preflights cannot both pass and then exceed the quota.
pub struct ReservationLedger {
    quota: u64,
    state: Mutex<LedgerState>,
}

impl ReservationLedger {
    pub fn new(quota_bytes: u64) -> Self {
        Self {
            quota: quota_bytes,
            state: Mutex::new(LedgerState {
                reserved: 0,
                used: 0,
                by_principal: BTreeMap::new(),
            }),
        }
    }

    pub fn reserved_bytes(&self) -> u64 {
        self.lock().reserved
    }

    pub fn used_bytes(&self) -> u64 {
        self.lock().used
    }

    /// Reserve the full estimate before any source read or temp write.
    pub fn reserve_capture(
        self: &Arc<Self>,
        principal: PrincipalId,
        estimated_bytes: u64,
    ) -> RtResult<StorageLease> {
        let mut state = self.lock();
        let reserved = state.reserved.checked_add(estimated_bytes).ok_or_else(|| {
            rt_error(ErrorCode::InvalidArgument, "overflow")
        })?;
        let total = reserved
            .checked_add(state.used)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        if total > self.quota {
            return Err(rt_error(ErrorCode::CapacityLimited, "storage_quota"));
        }
        state.reserved = reserved;
        state
            .by_principal
            .entry(principal)
            .and_modify(|held| *held = held.saturating_add(estimated_bytes))
            .or_insert(estimated_bytes);
        drop(state);
        Ok(StorageLease {
            ledger: Arc::clone(self),
            principal,
            bytes: estimated_bytes,
            settled: false,
        })
    }

    pub(crate) fn release_reserved(&self, principal: PrincipalId, bytes: u64) {
        let mut state = self.lock();
        state.reserved = state.reserved.saturating_sub(bytes);
        if let Some(held) = state.by_principal.get_mut(&principal) {
            *held = held.saturating_sub(bytes);
        }
    }

    pub(crate) fn commit_reserved_as_used(&self, principal: PrincipalId, reserved: u64, actual: u64) {
        let mut state = self.lock();
        state.reserved = state.reserved.saturating_sub(reserved);
        state.used = state.used.saturating_add(actual);
        if let Some(held) = state.by_principal.get_mut(&principal) {
            *held = held.saturating_sub(reserved);
        }
    }

    pub(crate) fn revert_used(&self, actual: u64) -> RtResult<()> {
        let mut state = self.lock();
        if state.used < actual {
            return Err(rt_error(ErrorCode::StorageUnavailable, "storage_accounting"));
        }
        state.used -= actual;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        self.state.lock().unwrap_or_else(|err| err.into_inner())
    }
}

pub struct StorageLease {
    ledger: Arc<ReservationLedger>,
    principal: PrincipalId,
    bytes: u64,
    settled: bool,
}

impl StorageLease {
    /// Dropping a lease does not release the reservation. Call this only after
    /// the unreferenced partial is confirmed removed.
    pub fn release_after_partial_removed(mut self, partial_removed: bool) -> RtResult<()> {
        if !partial_removed {
            return Err(rt_error(ErrorCode::StorageUnavailable, "partial_remains"));
        }
        if !self.settled {
            self.ledger.release_reserved(self.principal, self.bytes);
            self.settled = true;
        }
        Ok(())
    }

    pub fn commit_as_used(mut self, actual_bytes: u64) {
        if !self.settled {
            self.ledger
                .commit_reserved_as_used(self.principal, self.bytes, actual_bytes);
            self.settled = true;
        }
    }
}

pub struct ObjectStore {
    root: PathBuf,
    ledger: Arc<ReservationLedger>,
    principal: PrincipalId,
    fault: Mutex<ObjectFault>,
    committed: Mutex<BTreeSet<String>>,
    written: Mutex<BTreeSet<String>>,
    orphans: Mutex<Vec<PathBuf>>,
    seq: AtomicU64,
}

impl ObjectStore {
    pub fn open(
        root: impl Into<PathBuf>,
        ledger: Arc<ReservationLedger>,
        principal: PrincipalId,
    ) -> RtResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "object_root"))?;
        Ok(Self {
            root,
            ledger,
            principal,
            fault: Mutex::new(ObjectFault::None),
            committed: Mutex::new(BTreeSet::new()),
            written: Mutex::new(BTreeSet::new()),
            orphans: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
        })
    }

    pub fn set_fault(&self, fault: ObjectFault) {
        *self.fault_lock() = fault;
    }

    pub fn committed_ids(&self) -> BTreeSet<String> {
        self.committed.lock().unwrap_or_else(|err| err.into_inner()).clone()
    }

    pub fn remaining_files(&self) -> Vec<String> {
        let mut names = Vec::new();
        let Ok(read) = fs::read_dir(&self.root) else {
            return names;
        };
        for entry in read.flatten() {
            if entry.path().is_file() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        names
    }

    pub async fn put(&self, bytes: &[u8]) -> RtResult<ObjectRef> {
        let total_bytes = u64::try_from(bytes.len())
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        if total_bytes > MAX_SAFE_INTEGER {
            return Err(rt_error(ErrorCode::InvalidArgument, "overflow"));
        }
        let content_hash = Hash256::sha256(bytes);
        let object_id = content_hash.to_hex();
        let fault = *self.fault_lock();
        if fault == ObjectFault::Hash {
            return Err(rt_error(ErrorCode::StorageUnavailable, "object_hash"));
        }
        let final_path = self.root.join(&object_id);
        if fault == ObjectFault::None && self.reusable(&final_path, content_hash, total_bytes) {
            self.written_lock().insert(object_id.clone());
            return Ok(ObjectRef {
                object_id,
                content_hash,
                total_bytes,
            });
        }

        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(".partial-{seq}-{}", std::process::id()));
        if fault == ObjectFault::Write {
            let created = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp);
            if let Ok(mut file) = created {
                let _ = file.write_all(bytes);
            }
            self.remove_partial(&tmp);
            return Err(rt_error(ErrorCode::StorageUnavailable, "object_write"));
        }
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "object_write"))?;
            file.write_all(bytes)
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "object_write"))?;
            if fault == ObjectFault::FileFsync {
                drop(file);
                self.remove_partial(&tmp);
                return Err(rt_error(ErrorCode::StorageUnavailable, "object_fsync"));
            }
            if let Err(_err) = file.sync_all() {
                drop(file);
                self.remove_partial(&tmp);
                return Err(rt_error(ErrorCode::StorageUnavailable, "object_fsync"));
            }
        }
        if fault == ObjectFault::Rename {
            self.remove_partial(&tmp);
            return Err(rt_error(ErrorCode::StorageUnavailable, "object_rename"));
        }
        if let Err(_err) = replace_file(&tmp, &final_path) {
            self.remove_partial(&tmp);
            return Err(rt_error(ErrorCode::StorageUnavailable, "object_rename"));
        }
        if fault == ObjectFault::DirFsync {
            return self.fail_dir_fsync(&final_path, &object_id);
        }
        if fsync_dir(&self.root).is_err() {
            let _ = self.fail_dir_fsync(&final_path, &object_id);
            return Err(rt_error(ErrorCode::StorageUnavailable, "object_dir_fsync"));
        }
        self.written_lock().insert(object_id.clone());
        Ok(ObjectRef {
            object_id,
            content_hash,
            total_bytes,
        })
    }

    pub async fn get_verified(&self, object: &ObjectRef) -> RtResult<Vec<u8>> {
        let path = self.root.join(&object.object_id);
        let bytes = fs::read(&path).map_err(|_| rt_error(ErrorCode::StorageUnavailable, "object_read"))?;
        let size = u64::try_from(bytes.len())
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        if size != object.total_bytes {
            return Err(rt_error(ErrorCode::InvalidArgument, "object_size"));
        }
        if Hash256::sha256(&bytes) != object.content_hash {
            return Err(rt_error(ErrorCode::InvalidArgument, "object_hash"));
        }
        Ok(bytes)
    }

    pub(crate) fn reserve_estimated(&self, estimated_bytes: u64) -> RtResult<StorageLease> {
        self.ledger.reserve_capture(self.principal, estimated_bytes)
    }

    pub(crate) fn manifest_commit_fault(&self) -> bool {
        *self.fault_lock() == ObjectFault::DbCommit
    }

    pub(crate) fn discard_ids(&self, ids: &[String]) -> bool {
        let mut ok = true;
        let committed = self.committed.lock().unwrap_or_else(|err| err.into_inner());
        let mut written = self.written.lock().unwrap_or_else(|err| err.into_inner());
        for id in ids {
            if committed.contains(id) {
                continue;
            }
            let path = self.root.join(id);
            if !self.try_remove(&path) {
                self.note_orphan(path);
                ok = false;
            }
            written.remove(id);
        }
        ok
    }

    pub(crate) fn orphans_cleared(&self) -> bool {
        self.orphans
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .iter()
            .all(|path| !path.exists())
    }

    pub(crate) fn release_accounted(&self, bytes: u64, removed: bool) -> RtResult<()> {
        if !removed {
            return Err(rt_error(ErrorCode::StorageUnavailable, "partial_remains"));
        }
        self.ledger.revert_used(bytes)
    }

    pub(crate) fn mark_committed(&self, ids: &[String]) {
        let mut committed = self.committed.lock().unwrap_or_else(|err| err.into_inner());
        let mut written = self.written.lock().unwrap_or_else(|err| err.into_inner());
        for id in ids {
            committed.insert(id.clone());
            written.remove(id);
        }
    }

    fn reusable(&self, path: &Path, hash: Hash256, total_bytes: u64) -> bool {
        let Ok(existing) = fs::read(path) else {
            return false;
        };
        u64::try_from(existing.len()).ok() == Some(total_bytes) && Hash256::sha256(&existing) == hash
    }

    fn fail_dir_fsync(&self, final_path: &Path, object_id: &str) -> RtResult<ObjectRef> {
        if !self.is_retained(object_id) {
            self.remove_partial(final_path);
        }
        Err(rt_error(ErrorCode::StorageUnavailable, "object_dir_fsync"))
    }

    fn is_retained(&self, object_id: &str) -> bool {
        self.committed
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .contains(object_id)
            || self
                .written
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .contains(object_id)
    }

    fn remove_partial(&self, path: &Path) {
        if !self.try_remove(path) {
            self.note_orphan(path.to_path_buf());
        }
    }

    fn try_remove(&self, path: &Path) -> bool {
        match fs::remove_file(path) {
            Ok(()) => !path.exists(),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        }
    }

    fn note_orphan(&self, path: PathBuf) {
        self.orphans
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(path);
    }

    fn fault_lock(&self) -> std::sync::MutexGuard<'_, ObjectFault> {
        self.fault.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn written_lock(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.written.lock().unwrap_or_else(|err| err.into_inner())
    }
}

fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::remove_file(to)?;
            fs::rename(from, to)
        }
        Err(err) => Err(err),
    }
}
