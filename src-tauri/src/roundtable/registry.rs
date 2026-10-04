//! Roundtable session discovery registry.
//!
//! Qualification uses a temporary [`RegistryStore`] whose directory is owned by
//! [`super::QualificationHarness`]. Production does not migrate
//! `internal_agent_sessions` and does not write `roundtable` into that table's
//! purpose check. P09b is the persistent `rt_internal_bindings` adapter.
//!
//! An older binary cannot understand roundtable bindings. Ordinary history
//! stays readable and roundtable itself is unavailable. Downgrade only after
//! disable and drain, by restoring a backup taken before upgrade. An old build
//! must not import a reserved roundtable root. There is no silent
//! compatibility mode for that data directory.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use roundtable_protocol::{BindingId, ErrorCode, IncarnationId, RoomId, RtResult};
use serde::{Deserialize, Serialize};

use crate::auto_title::internal_sessions::{
    self, DiscoveryGate, SharedDiscoveryPermit,
};
use crate::models::AgentType;
use crate::parsers::normalize_path_for_matching;

use super::rt_error;

const FILE_VERSION: u32 = 1;

/// Agent session id as known to a parser. Not a protocol UUID.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExternalId(String);

impl ExternalId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ExternalId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for ExternalId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// One service-owned member session. The row binds the room, the binding, the
/// incarnation, and the reserved cwd root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalBindingRecord {
    pub room_id: RoomId,
    pub binding_id: BindingId,
    pub incarnation: IncarnationId,
    pub agent: AgentType,
    pub external_id: ExternalId,
    pub reserved_root: PathBuf,
}

/// Binding loaded back after a parser or process restart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredBinding {
    pub room_id: RoomId,
    pub binding_id: BindingId,
    pub incarnation: IncarnationId,
    pub agent: AgentType,
    pub external_id: ExternalId,
    pub reserved_root: PathBuf,
    pub running: bool,
}

#[derive(Clone, Debug)]
pub struct StoredBinding {
    pub room_id: RoomId,
    pub binding_id: BindingId,
    pub incarnation: IncarnationId,
    pub agent: AgentType,
    pub external_id: String,
    pub reserved_root: PathBuf,
    pub running: bool,
}

/// Store behind [`RoundtableSessionRegistry`]. P09b implements this on
/// `rt_internal_bindings`. Qualification must not use that table.
pub trait RegistryStore: Send + Sync {
    fn load(&self) -> RtResult<Vec<StoredBinding>>;
    fn upsert(&self, row: &StoredBinding) -> RtResult<()>;
}

/// `false`. Direct downgrade of a roundtable data directory is unsupported.
pub fn downgrade_is_silent_compatible() -> bool {
    false
}

/// Hide predicate used by ordinary discovery, recovery, direct lookup, and import.
pub fn hidden_from_ordinary_discovery(
    agent: AgentType,
    external_id: Option<&str>,
    working_dir: Option<&str>,
) -> bool {
    internal_sessions::extra_session_hidden(agent, external_id, working_dir)
}

struct ReservedRoot {
    lease_id: u64,
    path: PathBuf,
    persisted: bool,
}

struct State {
    roots: Vec<ReservedRoot>,
    bindings: Vec<StoredBinding>,
    discovery_agent: Option<AgentType>,
}

impl State {
    fn from_loaded(bindings: Vec<StoredBinding>) -> Self {
        let roots = bindings
            .iter()
            .map(|row| ReservedRoot {
                lease_id: 0,
                path: row.reserved_root.clone(),
                persisted: true,
            })
            .collect();
        Self {
            roots,
            bindings,
            discovery_agent: None,
        }
    }
}

struct Inner {
    store: Arc<dyn RegistryStore>,
    state: Mutex<State>,
    discovery_lock: Arc<tokio::sync::RwLock<()>>,
    hide_id: AtomicU64,
    next_lease: AtomicU64,
}

impl Drop for Inner {
    fn drop(&mut self) {
        let id = self.hide_id.load(Ordering::Relaxed);
        if id != 0 {
            internal_sessions::remove_extra_discovery_hide(id);
        }
    }
}

impl Inner {
    fn hidden(&self, agent: AgentType, external_id: Option<&str>, working_dir: Option<&str>) -> bool {
        let state = lock_state(&self.state);
        if let Some(id) = external_id.filter(|id| !id.is_empty()) {
            if state
                .bindings
                .iter()
                .any(|row| row.agent == agent && row.external_id == id)
            {
                return true;
            }
        }
        let Some(path) = working_dir.filter(|path| !path.is_empty()) else {
            return false;
        };
        let under_root = state.roots.iter().any(|root| is_lexically_below(path, &root.path))
            || state
                .bindings
                .iter()
                .any(|row| is_lexically_below(path, &row.reserved_root));
        // A discovery lease does not hide an ordinary cwd. It blocks
        // `shared_filter` until register ack; the reserved root is the
        // pre-ack content filter.
        under_root
    }
}

/// Cwd reservation. Dropping it before [`RoundtableSessionRegistry::register`]
/// releases the root. After register, the binding keeps the root.
#[must_use = "dropping the root lease releases an unregistered reservation"]
pub struct RootLease {
    id: u64,
    inner: Arc<Inner>,
}

impl Drop for RootLease {
    fn drop(&mut self) {
        let mut state = lock_state(&self.inner.state);
        state
            .roots
            .retain(|root| root.lease_id != self.id || root.persisted);
    }
}

/// Exclusive handshake lease for one agent. Shared discovery waits until this
/// is dropped, which is after register ack when the caller holds it across
/// [`RoundtableSessionRegistry::register`].
#[must_use = "dropping the discovery lease lets ordinary scans proceed"]
pub struct DiscoveryLease {
    agent: AgentType,
    inner: Arc<Inner>,
    _guard: tokio::sync::OwnedRwLockWriteGuard<()>,
}

impl Drop for DiscoveryLease {
    fn drop(&mut self) {
        let mut state = lock_state(&self.inner.state);
        if state.discovery_agent == Some(self.agent) {
            state.discovery_agent = None;
        }
    }
}

/// View of a service-owned run. Closing it does not unregister the session
/// and does not stop the run.
pub struct ObserverWindow {
    _inner: Arc<Inner>,
}

impl ObserverWindow {
    pub fn close(self) {}
}

impl Drop for ObserverWindow {
    fn drop(&mut self) {
        // The run is service-owned. An observer window is not its lifetime.
    }
}

/// In-memory until P09b, or a temporary directory for qualification.
#[derive(Clone)]
pub struct RoundtableSessionRegistry {
    inner: Arc<Inner>,
}

impl RoundtableSessionRegistry {
    pub fn in_memory() -> Self {
        Self::open(Arc::new(MemoryStore::default())).expect("memory registry store")
    }

    /// Qualification store. `dir` must outlive the registry; the harness holds it.
    pub fn temporary(dir: &Path) -> RtResult<Self> {
        std::fs::create_dir_all(dir).map_err(|_| {
            rt_error(ErrorCode::StorageUnavailable, "registry_store_unavailable")
        })?;
        let path = dir.join("roundtable-registry.json");
        Self::open(Arc::new(FileRegistryStore::new(path)))
    }

    fn open(store: Arc<dyn RegistryStore>) -> RtResult<Self> {
        let loaded = store.load()?;
        let inner = Arc::new(Inner {
            store,
            state: Mutex::new(State::from_loaded(loaded)),
            discovery_lock: Arc::new(tokio::sync::RwLock::new(())),
            hide_id: AtomicU64::new(0),
            next_lease: AtomicU64::new(1),
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        let hide_id = internal_sessions::install_extra_discovery_hide(Arc::new(
            move |agent, external_id, working_dir| {
                weak.upgrade()
                    .is_some_and(|inner| inner.hidden(agent, external_id, working_dir))
            },
        ));
        inner.hide_id.store(hide_id, Ordering::Relaxed);
        Ok(Self { inner })
    }

    /// Install the process placeholder on the shared internal-session registry.
    /// The placeholder is not the P09b table.
    pub fn install_process_discovery(internal: &crate::auto_title::InternalAgentSessionRegistry) {
        let mut slot = process_slot().lock().unwrap_or_else(|err| err.into_inner());
        if let Some(existing) = slot.as_ref() {
            existing.attach_discovery(internal);
            return;
        }
        let registry = Self::in_memory();
        registry.attach_discovery(internal);
        *slot = Some(registry);
    }

    pub fn attach_discovery(&self, internal: &crate::auto_title::InternalAgentSessionRegistry) {
        let gate: Arc<dyn DiscoveryGate> = Arc::new(self.clone());
        internal.retain_discovery_gate(gate);
    }

    pub fn reserve_root(&self, path: PathBuf) -> RootLease {
        let id = self.inner.next_lease.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = lock_state(&self.inner.state);
            state.roots.push(ReservedRoot {
                lease_id: id,
                path,
                persisted: false,
            });
        }
        RootLease {
            id,
            inner: Arc::clone(&self.inner),
        }
    }

    pub async fn begin_discovery(&self, agent: AgentType) -> DiscoveryLease {
        let guard = self.inner.discovery_lock.clone().write_owned().await;
        {
            let mut state = lock_state(&self.inner.state);
            state.discovery_agent = Some(agent);
        }
        DiscoveryLease {
            agent,
            inner: Arc::clone(&self.inner),
            _guard: guard,
        }
    }

    pub fn discovery_lease_held(&self, agent: AgentType) -> bool {
        lock_state(&self.inner.state).discovery_agent == Some(agent)
    }

    /// Persist the binding. This is the register ack: after `Ok`, discovery
    /// hides the external id even when the cwd is no longer under the root.
    pub fn register(&self, record: InternalBindingRecord) -> RtResult<()> {
        if record.external_id.as_str().is_empty() {
            return Err(rt_error(ErrorCode::InvalidArgument, "external_id_empty"));
        }
        let mut state = lock_state(&self.inner.state);
        let reserved = state
            .roots
            .iter()
            .any(|root| same_root(&root.path, &record.reserved_root));
        if !reserved {
            return Err(rt_error(ErrorCode::InvalidArgument, "root_not_reserved"));
        }
        if let Some(existing) = state.bindings.iter().find(|row| {
            row.agent == record.agent && row.external_id == record.external_id.as_str()
        }) {
            if same_binding(existing, &record) {
                return Ok(());
            }
            return Err(rt_error(ErrorCode::InvalidState, "binding_conflict"));
        }
        let stored = StoredBinding {
            room_id: record.room_id,
            binding_id: record.binding_id,
            incarnation: record.incarnation,
            agent: record.agent,
            external_id: record.external_id.as_str().to_owned(),
            reserved_root: record.reserved_root.clone(),
            running: true,
        };
        self.inner.store.upsert(&stored)?;
        for root in &mut state.roots {
            if same_root(&root.path, &record.reserved_root) {
                root.persisted = true;
            }
        }
        state.bindings.push(stored);
        Ok(())
    }

    pub fn is_roundtable(&self, agent: AgentType, external_id: &ExternalId, path: &Path) -> bool {
        self.inner.hidden(
            agent,
            Some(external_id.as_str()),
            Some(path.to_string_lossy().as_ref()),
        )
    }

    pub fn registered(&self, room_id: RoomId) -> Option<RegisteredBinding> {
        lock_state(&self.inner.state)
            .bindings
            .iter()
            .find(|row| row.room_id == room_id)
            .map(registered_from)
    }

    pub fn run_is_active(&self, room_id: RoomId) -> bool {
        lock_state(&self.inner.state)
            .bindings
            .iter()
            .any(|row| row.room_id == room_id && row.running)
    }

    pub fn open_observer(&self, room_id: RoomId) -> RtResult<ObserverWindow> {
        let known = lock_state(&self.inner.state)
            .bindings
            .iter()
            .any(|row| row.room_id == room_id);
        if !known {
            return Err(rt_error(ErrorCode::InvalidArgument, "room_not_registered"));
        }
        Ok(ObserverWindow {
            _inner: Arc::clone(&self.inner),
        })
    }
}

impl DiscoveryGate for RoundtableSessionRegistry {
    fn wait_shared(&self) -> Pin<Box<dyn Future<Output = SharedDiscoveryPermit> + Send + '_>> {
        let lock = Arc::clone(&self.inner.discovery_lock);
        Box::pin(async move { SharedDiscoveryPermit::hold(lock.read_owned().await) })
    }
}

fn process_slot() -> &'static Mutex<Option<RoundtableSessionRegistry>> {
    static PROCESS: Mutex<Option<RoundtableSessionRegistry>> = Mutex::new(None);
    &PROCESS
}

fn lock_state(state: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|err| err.into_inner())
}

fn registered_from(row: &StoredBinding) -> RegisteredBinding {
    RegisteredBinding {
        room_id: row.room_id,
        binding_id: row.binding_id,
        incarnation: row.incarnation,
        agent: row.agent,
        external_id: ExternalId::from(row.external_id.clone()),
        reserved_root: row.reserved_root.clone(),
        running: row.running,
    }
}

fn same_binding(existing: &StoredBinding, record: &InternalBindingRecord) -> bool {
    existing.room_id == record.room_id
        && existing.binding_id == record.binding_id
        && existing.incarnation == record.incarnation
        && same_root(&existing.reserved_root, &record.reserved_root)
}

fn same_root(left: &Path, right: &Path) -> bool {
    normalize_path_for_matching(&left.to_string_lossy())
        == normalize_path_for_matching(&right.to_string_lossy())
}

fn is_lexically_below(path: &str, root: &Path) -> bool {
    crate::auto_title::internal_sessions::is_lexically_below(path, root)
}

#[derive(Default)]
struct MemoryStore {
    rows: Mutex<Vec<StoredBinding>>,
}

impl RegistryStore for MemoryStore {
    fn load(&self) -> RtResult<Vec<StoredBinding>> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone())
    }

    fn upsert(&self, row: &StoredBinding) -> RtResult<()> {
        let mut rows = self.rows.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(existing) = rows
            .iter_mut()
            .find(|item| item.agent == row.agent && item.external_id == row.external_id)
        {
            *existing = row.clone();
        } else {
            rows.push(row.clone());
        }
        Ok(())
    }
}

struct FileRegistryStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileRegistryStore {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }
}

impl RegistryStore for FileRegistryStore {
    fn load(&self) -> RtResult<Vec<StoredBinding>> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        read_file(&self.path)
    }

    fn upsert(&self, row: &StoredBinding) -> RtResult<()> {
        let _guard = self.lock.lock().unwrap_or_else(|err| err.into_inner());
        let mut rows = read_file(&self.path)?;
        if let Some(existing) = rows
            .iter_mut()
            .find(|item| item.agent == row.agent && item.external_id == row.external_id)
        {
            *existing = row.clone();
        } else {
            rows.push(row.clone());
        }
        write_file(&self.path, &rows)
    }
}

#[derive(Serialize, Deserialize)]
struct FileBody {
    version: u32,
    bindings: Vec<FileRow>,
}

#[derive(Serialize, Deserialize)]
struct FileRow {
    room_id: String,
    binding_id: String,
    incarnation: String,
    agent: String,
    external_id: String,
    reserved_root: String,
    running: bool,
}

fn read_file(path: &Path) -> RtResult<Vec<StoredBinding>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(path)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "registry_store_unreadable"))?;
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let body: FileBody = serde_json::from_slice(&bytes)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "registry_store_corrupt"))?;
    if body.version != FILE_VERSION {
        return Err(rt_error(
            ErrorCode::StorageUnavailable,
            "registry_store_version",
        ));
    }
    body.bindings
        .into_iter()
        .map(|row| row.into_stored())
        .collect()
}

fn write_file(path: &Path, rows: &[StoredBinding]) -> RtResult<()> {
    let body = FileBody {
        version: FILE_VERSION,
        bindings: rows.iter().map(FileRow::from_stored).collect(),
    };
    let bytes = serde_json::to_vec(&body)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "registry_store_encode"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "registry_store_unwritable"))?;
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&tmp, path)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "registry_store_unwritable"))?;
    }
    Ok(())
}

impl FileRow {
    fn from_stored(row: &StoredBinding) -> Self {
        Self {
            room_id: row.room_id.to_string(),
            binding_id: row.binding_id.to_string(),
            incarnation: row.incarnation.to_string(),
            agent: row.agent.as_wire().into_owned(),
            external_id: row.external_id.clone(),
            reserved_root: row.reserved_root.to_string_lossy().into_owned(),
            running: row.running,
        }
    }

    fn into_stored(self) -> RtResult<StoredBinding> {
        let agent = AgentType::from_wire(&self.agent)
            .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "registry_store_agent"))?;
        Ok(StoredBinding {
            room_id: parse_id(&self.room_id)?,
            binding_id: parse_id(&self.binding_id)?,
            incarnation: parse_id(&self.incarnation)?,
            agent,
            external_id: self.external_id,
            reserved_root: PathBuf::from(self.reserved_root),
            running: self.running,
        })
    }
}

fn parse_id<T: std::str::FromStr>(text: &str) -> RtResult<T> {
    text.parse()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "registry_store_id"))
}
