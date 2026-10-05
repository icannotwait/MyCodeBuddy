//! One coordinator for every room. `open` does not return a runnable service.
//!
//! Ready is reached only after every quarantined incarnation has a full cleanup
//! proof. A missing lock is history-only. Pause, interject, create, and
//! preflight cannot bypass that.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use roundtable_protocol::{
    CleanupProof, ErrorCode, IncarnationId, PermitReleaseProof, RoomId, RtResult,
};

use super::actor::RoomActor;
use super::ownership::{
    lock_path_for, try_acquire, CoordinatorLock, LockAttempt, RoundtableReadService,
};
use super::resources::ResourceAllocator;
use super::rt_error;
#[cfg(any(test, feature = "test-utils"))]
use super::runtime::try_enqueue;
use super::runtime::{PreparedPrompt, PreparedRoundtableConnection, RoundtableLaunch};
use super::sandbox::{DbIdentity, IsolationProvider};
use super::store::RoundtableStore;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceReadiness {
    ReadOnly,
    Recovering,
    Ready,
    Blocked { reason: &'static str },
}

pub struct ServiceConfig {
    pub data_dir: std::path::PathBuf,
    pub db_path: std::path::PathBuf,
    pub db_identity: DbIdentity,
    pub discover: Option<Arc<dyn IsolationProvider + Send + Sync>>,
}

pub struct RoomMessage {
    pub room_id: RoomId,
    pub kind: RoomMessageKind,
    pub prepared: Option<PreparedPrompt>,
}

pub enum RoomMessageKind {
    ReadyToEnqueue,
    Start,
    Resume,
    Pause,
    Interject,
    Create,
    Preflight,
}

pub struct RoomReply {
    pub code: ErrorCode,
    pub uncertain: bool,
    pub reason: Option<String>,
}

fn reply(code: ErrorCode) -> RoomReply {
    RoomReply {
        code,
        uncertain: false,
        reason: None,
    }
}

pub struct ShutdownReport {
    pub steps: Vec<&'static str>,
    pub lock_released: bool,
}

pub struct RuntimeIdentity {
    pub incarnation: IncarnationId,
    pub pid: u32,
}

pub struct QuarantineLease {
    pub incarnation: IncarnationId,
    pub released: bool,
}

pub struct RoundtableSlot {
    current: Mutex<Option<Arc<RoundtableService>>>,
}

impl Default for RoundtableSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl RoundtableSlot {
    pub fn new() -> Self {
        Self {
            current: Mutex::new(None),
        }
    }

    /// Both desktop and web install into the same slot. A second install
    /// returns the service already stored.
    pub fn install(&self, service: Arc<RoundtableService>) -> Arc<RoundtableService> {
        let mut slot = self.current.lock().expect("roundtable slot");
        if let Some(existing) = slot.clone() {
            return existing;
        }
        *slot = Some(Arc::clone(&service));
        service
    }

    pub fn current(&self) -> Option<Arc<RoundtableService>> {
        self.current.lock().expect("roundtable slot").clone()
    }
}

pub enum OpenedRoundtable {
    Coordinator(Arc<RoundtableService>),
    History(RoundtableReadService),
}

#[async_trait]
pub trait ParticipantRuntime: Send + Sync {
    async fn prepare(&self, launch: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection>;
    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof>;

    /// Resolve runtime availability without accepting credentials or a certificate
    /// from a command body. A failed preflight never launches a participant.
    async fn preflight(
        &self,
        _config: &roundtable_protocol::RoundtableConfigV1,
    ) -> RtResult<serde_json::Value> {
        Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "certificate_not_passed",
        ))
    }

    async fn run_room(
        &self,
        _store: RoundtableStore,
        _room: RoomId,
        _config: roundtable_protocol::RoundtableConfigV1,
    ) -> RtResult<()> {
        Err(rt_error(
            ErrorCode::RuntimeUnavailable,
            "runtime_unavailable",
        ))
    }
}

pub struct RoundtableService {
    store: Mutex<Option<RoundtableStore>>,
    lock: Mutex<Option<CoordinatorLock>>,
    boot_epoch: AtomicU64,
    readiness: Mutex<ServiceReadiness>,
    actors: Mutex<HashMap<String, Arc<RoomActor>>>,
    quarantine: Mutex<Vec<QuarantineLease>>,
    runtime: Arc<dyn ParticipantRuntime>,
    steps: Mutex<Vec<&'static str>>,
    observer_open: AtomicBool,
    unknown_occupancy: AtomicBool,
    allocator: ResourceAllocator,
    supervisor_online: AtomicBool,
    meter: super::usage::RoomMeter,
    pub(crate) command_gate: tokio::sync::Mutex<()>,
    pub(crate) execution_gate: super::feature_gate::ExecutionGate,
    pub(crate) cursors: Mutex<super::paging::ScopedCursors>,
    pub(crate) data_dir: std::path::PathBuf,
    pub(crate) preflights: Mutex<HashMap<String, super::product::ConfirmedPreflight>>,
    pub(crate) subscription_tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    pub(crate) room_tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    pub(crate) control_tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    draining: AtomicBool,
}

impl RoundtableService {
    pub async fn open(
        config: ServiceConfig,
        store: RoundtableStore,
        runtime: Arc<dyn ParticipantRuntime>,
    ) -> RtResult<Arc<Self>> {
        let path = lock_path_for(&config.data_dir, &config.db_identity);
        let attempt = try_acquire(&path);
        let (lock, writable_store, boot, readiness) = match attempt {
            LockAttempt::Exclusive(lock) => {
                let boot = store.bump_coordinator_boot().await?;
                (Some(lock), Some(store), boot, ServiceReadiness::Recovering)
            }
            LockAttempt::Taken => {
                let history = RoundtableReadService::open(&config.db_path).await?;
                let boot = history.boot_epoch().await.unwrap_or(0);
                let reader =
                    super::store::open_roundtable_store(history.connection().clone()).await?;
                (None, Some(reader), boot, ServiceReadiness::ReadOnly)
            }
            LockAttempt::Unavailable => {
                return Err(rt_error(ErrorCode::StorageUnavailable, "coordinator_lock"));
            }
        };
        let service = Arc::new(Self {
            store: Mutex::new(writable_store),
            lock: Mutex::new(lock),
            boot_epoch: AtomicU64::new(boot),
            readiness: Mutex::new(readiness),
            actors: Mutex::new(HashMap::new()),
            quarantine: Mutex::new(Vec::new()),
            runtime,
            steps: Mutex::new(Vec::new()),
            observer_open: AtomicBool::new(true),
            unknown_occupancy: AtomicBool::new(false),
            allocator: ResourceAllocator::new(1),
            supervisor_online: AtomicBool::new(true),
            meter: super::usage::RoomMeter::frozen(),
            command_gate: tokio::sync::Mutex::new(()),
            execution_gate: super::feature_gate::ExecutionGate::open(&config.data_dir),
            cursors: Mutex::new(super::paging::ScopedCursors::default()),
            data_dir: config.data_dir.clone(),
            preflights: Mutex::new(HashMap::new()),
            subscription_tasks: Mutex::new(HashMap::new()),
            room_tasks: Mutex::new(HashMap::new()),
            control_tasks: Mutex::new(HashMap::new()),
            draining: AtomicBool::new(false),
        });
        if service.writable() {
            service.recover(&config).await?;
        }
        Ok(service)
    }

    pub fn writable(&self) -> bool {
        self.lock.lock().expect("lock").is_some()
    }

    pub(crate) fn draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }

    /// Finished subscriptions are pruned at admission; replacement does not
    /// consume another slot. A desktop window cannot retain unbounded tasks.
    #[cfg(feature = "tauri-runtime")]
    pub(crate) fn install_subscription(
        &self,
        key: String,
        task: tokio::task::JoinHandle<()>,
    ) -> RtResult<()> {
        let mut tasks = self.subscription_tasks.lock().expect("subscriptions");
        tasks.retain(|_, task| !task.is_finished());
        if self.draining() || (!tasks.contains_key(&key) && tasks.len() >= 64) {
            task.abort();
            return Err(rt_error(
                ErrorCode::CapacityLimited,
                "subscription_capacity",
            ));
        }
        if let Some(old) = tasks.insert(key, task) {
            old.abort();
        }
        Ok(())
    }

    pub(crate) fn command_store(&self) -> RtResult<RoundtableStore> {
        self.store
            .lock()
            .expect("store")
            .clone()
            .ok_or_else(|| rt_error(ErrorCode::Forbidden, "store_unavailable"))
    }

    pub(crate) fn participant_runtime(&self) -> Arc<dyn ParticipantRuntime> {
        Arc::clone(&self.runtime)
    }

    pub fn room_meter(&self) -> super::usage::RoomMeter {
        self.meter.clone()
    }

    pub fn allocator(&self) -> &ResourceAllocator {
        &self.allocator
    }

    pub fn quarantine_pending(&self) -> bool {
        self.unknown_occupancy.load(Ordering::Relaxed)
            || !self.quarantine.lock().expect("quarantine").is_empty()
    }

    pub fn supervisor_online(&self) -> bool {
        self.supervisor_online.load(Ordering::Relaxed)
    }

    pub fn set_supervisor_online(&self, online: bool) {
        self.supervisor_online.store(online, Ordering::Relaxed);
    }

    pub(crate) fn mark_ready_after_recovery(&self) -> RtResult<()> {
        let mut readiness = self.readiness.lock().expect("readiness");
        if !matches!(*readiness, ServiceReadiness::Recovering) {
            return Err(rt_error(ErrorCode::InvalidState, "not_recovering"));
        }
        *readiness = ServiceReadiness::Ready;
        Ok(())
    }

    pub fn boot_epoch(&self) -> u64 {
        self.boot_epoch.load(Ordering::Relaxed)
    }

    pub fn readiness(&self) -> ServiceReadiness {
        match *self.readiness.lock().expect("readiness") {
            ServiceReadiness::ReadOnly => ServiceReadiness::ReadOnly,
            ServiceReadiness::Recovering => ServiceReadiness::Recovering,
            ServiceReadiness::Ready => ServiceReadiness::Ready,
            ServiceReadiness::Blocked { reason } => ServiceReadiness::Blocked { reason },
        }
    }

    pub fn actor(&self, room: &RoomId) -> Arc<RoomActor> {
        let mut actors = self.actors.lock().expect("actors");
        actors
            .entry(room.to_string())
            .or_insert_with(|| Arc::new(RoomActor::new(*room, self.boot_epoch())))
            .clone()
    }

    pub fn callback(&self, room: &RoomId, boot_epoch: u64) -> RoomReply {
        match self.actor(room).callback(boot_epoch) {
            Ok(()) => reply(ErrorCode::InvalidArgument),
            Err(err) => RoomReply {
                code: err.code,
                uncertain: false,
                reason: err.details.reason,
            },
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn send(&self, room: RoomId, msg: RoomMessage) -> RtResult<RoomReply> {
        if !self.writable() {
            return Ok(reply(ErrorCode::Forbidden));
        }
        match msg.kind {
            RoomMessageKind::Start | RoomMessageKind::Resume => self.start(room).await,
            RoomMessageKind::Pause
            | RoomMessageKind::Interject
            | RoomMessageKind::Create
            | RoomMessageKind::Preflight => {
                if !matches!(self.readiness(), ServiceReadiness::Ready) {
                    return Ok(reply(ErrorCode::InvalidState));
                }
                Ok(reply(ErrorCode::InvalidState))
            }
            RoomMessageKind::ReadyToEnqueue => self.enqueue(room, msg.prepared).await,
        }
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub async fn start(&self, room: RoomId) -> RtResult<RoomReply> {
        let _ = room;
        if !self.writable() {
            return Ok(reply(ErrorCode::Forbidden));
        }
        if self.unknown_occupancy.load(Ordering::Relaxed) || self.quarantine_open() {
            return Ok(reply(ErrorCode::CapacityLimited));
        }
        if !matches!(self.readiness(), ServiceReadiness::Ready) {
            return Ok(reply(ErrorCode::RuntimeUnavailable));
        }
        Ok(reply(ErrorCode::InvalidState))
    }

    pub async fn acknowledge_cleanup(
        &self,
        proof: &PermitReleaseProof,
    ) -> RtResult<ServiceReadiness> {
        let _gate = self.command_gate.lock().await;
        if !self.writable() || self.draining() {
            return Err(rt_error(ErrorCode::Forbidden, "read_only_coordinator"));
        }
        let required: Vec<_> = self
            .quarantine
            .lock()
            .expect("quarantine")
            .iter()
            .map(|lease| lease.incarnation)
            .collect();
        if self.unknown_occupancy.load(Ordering::Relaxed) || !proof.releases(&required) {
            *self.readiness.lock().expect("readiness") = ServiceReadiness::Blocked {
                reason: "partial_cleanup",
            };
            return Ok(ServiceReadiness::Blocked {
                reason: "partial_cleanup",
            });
        }
        let store = self.command_store()?;
        for incarnation in required {
            super::store::exec(
                store.connection(),
                "UPDATE rt_launch_intents SET reaped=1 WHERE incarnation=?",
                vec![super::store::text(&incarnation.to_string())],
            )
            .await?;
        }
        store.recover_durable(self.boot_epoch()).await?;
        self.quarantine.lock().expect("quarantine").clear();
        let next = if self.supervisor_online() {
            ServiceReadiness::Ready
        } else {
            ServiceReadiness::Recovering
        };
        *self.readiness.lock().expect("readiness") = next;
        Ok(next)
    }

    pub fn close_observer(&self) {
        self.observer_open.store(false, Ordering::Relaxed);
    }

    pub fn observer_open(&self) -> bool {
        self.observer_open.load(Ordering::Relaxed)
    }

    /// Cleanup does not wait for `prepare` and does not hold the room gate.
    pub async fn cleanup_owned(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof> {
        self.runtime.cancel_and_reap(identity).await
    }

    pub async fn shutdown(&self) -> RtResult<ShutdownReport> {
        // ponytail: one global command gate; use per-room gates if command
        // throughput warrants it. It also serializes shutdown with admission.
        let _gate = self.command_gate.lock().await;
        self.draining.store(true, Ordering::Release);
        self.note("revoke");
        for (_, task) in self
            .subscription_tasks
            .lock()
            .expect("subscriptions")
            .drain()
        {
            task.abort();
        }
        let tasks: Vec<_> = self
            .room_tasks
            .lock()
            .expect("room tasks")
            .drain()
            .map(|(_, task)| task)
            .chain(
                self.control_tasks
                    .lock()
                    .expect("control tasks")
                    .drain()
                    .map(|(_, task)| task),
            )
            .collect();
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
        if self.writable() {
            use sea_orm::TransactionTrait;
            let store = self.command_store()?;
            let txn = store
                .connection()
                .begin()
                .await
                .map_err(super::store::storage_err)?;
            let rooms = super::store::rows(
                &txn,
                "SELECT room_id FROM rt_rooms WHERE status='running'",
                vec![],
            )
            .await?;
            for row in rooms {
                let room: String = super::store::column(&row, 0)?;
                super::store::exec(&txn,"UPDATE rt_rooms SET status='paused',run_epoch=run_epoch+1,revision=revision+1,last_seq=last_seq+1,blocked_reason='recovery_required' WHERE room_id=?",vec![super::store::text(&room)]).await?;
                super::store::exec(
                    &txn,
                    "DELETE FROM rt_active_time_leases WHERE room_id=?",
                    vec![super::store::text(&room)],
                )
                .await?;
                store.emit_current_in(&txn, &room, "recovery").await?;
            }
            txn.commit().await.map_err(super::store::storage_err)?;
        }
        let mut identities: Vec<RuntimeIdentity> = self
            .quarantine
            .lock()
            .expect("quarantine")
            .iter()
            .map(|lease| RuntimeIdentity {
                incarnation: lease.incarnation,
                pid: 0,
            })
            .collect();
        if self.writable() {
            let store = self.command_store()?;
            for intent in store.list_unreaped_launches().await? {
                if !identities
                    .iter()
                    .any(|identity| identity.incarnation == intent.incarnation)
                {
                    identities.push(RuntimeIdentity {
                        incarnation: intent.incarnation,
                        pid: 0,
                    });
                }
            }
            // An attempt owns broker/permit resources before its launch intent
            // is recorded. Aborting preparation does not prove those drained.
            for row in super::store::rows(store.connection(),
                "SELECT DISTINCT b.incarnation FROM rt_bindings b JOIN rt_attempts a ON a.room_id=b.room_id AND a.binding_id=b.binding_id WHERE a.cleanup_state<>'confirmed'",
                vec![]).await? {
                let incarnation = super::store::column::<String>(&row, 0)?.parse()
                    .map_err(|_| rt_error(ErrorCode::StorageUnavailable,"incarnation"))?;
                if !identities.iter().any(|identity| identity.incarnation == incarnation) {
                    identities.push(RuntimeIdentity { incarnation, pid: 0 });
                }
            }
        }
        self.note("cleanup");
        let mut cleanup_ok = !self.unknown_occupancy.load(Ordering::Relaxed);
        for identity in identities {
            let incarnation = identity.incarnation;
            match self.runtime.cancel_and_reap(identity).await {
                Ok(proof)
                    if proof.process.incarnation == incarnation
                        && proof.process.process_tree_empty
                        && proof.mailbox_empty
                        && proof.tools_drained
                        && proof.ingress_drained =>
                {
                    self.confirm_runtime_cleanup(incarnation).await?;
                }
                _ => cleanup_ok = false,
            }
        }
        for actor in self.actors.lock().expect("actors").values() {
            actor.stop();
        }
        self.note("persist");
        if !cleanup_ok && self.writable() {
            *self.readiness.lock().expect("readiness") = ServiceReadiness::Blocked {
                reason: "cleanup_unproven",
            };
            return Ok(ShutdownReport {
                steps: self.steps.lock().expect("steps").clone(),
                lock_released: false,
            });
        }
        self.note("unlock");
        let released = self.lock.lock().expect("lock").take().is_some();
        Ok(ShutdownReport {
            steps: self.steps.lock().expect("steps").clone(),
            lock_released: released || !self.writable(),
        })
    }

    pub fn shutdown_steps(&self) -> Vec<&'static str> {
        self.steps.lock().expect("steps").clone()
    }

    pub(crate) async fn stop_room_task(&self, room: RoomId) {
        let task = self
            .room_tasks
            .lock()
            .expect("room tasks")
            .remove(&room.to_string());
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
    }

    /// Only called after a matching full proof from the owned runtime.
    async fn confirm_runtime_cleanup(&self, incarnation: IncarnationId) -> RtResult<()> {
        use super::store::{column, exec, rows, storage_err, text};
        use sea_orm::TransactionTrait;
        let store = self.command_store()?;
        let txn = store.connection().begin().await.map_err(storage_err)?;
        let changed_rooms = rows(&txn,"SELECT DISTINCT b.room_id FROM rt_bindings b WHERE b.incarnation=? AND (b.state<>'retired' OR EXISTS(SELECT 1 FROM rt_attempts a WHERE a.room_id=b.room_id AND a.binding_id=b.binding_id AND a.cleanup_state<>'confirmed'))",vec![text(&incarnation.to_string())]).await?;
        exec(
            &txn,
            "UPDATE rt_launch_intents SET reaped=1 WHERE incarnation=?",
            vec![text(&incarnation.to_string())],
        )
        .await?;
        exec(&txn,"UPDATE rt_attempts SET cleanup_state='confirmed',state=CASE WHEN state IN ('reserved','launching','admitting','admitted','streaming','validating','active') THEN 'interrupted' ELSE state END WHERE EXISTS(SELECT 1 FROM rt_bindings b WHERE b.room_id=rt_attempts.room_id AND b.binding_id=rt_attempts.binding_id AND b.incarnation=?)",vec![text(&incarnation.to_string())]).await?;
        exec(
            &txn,
            "UPDATE rt_bindings SET state='retired',retire_reason='cleanup' WHERE incarnation=?",
            vec![text(&incarnation.to_string())],
        )
        .await?;
        for row in changed_rooms {
            let room: String = column(&row, 0)?;
            exec(
                &txn,
                "UPDATE rt_rooms SET revision=revision+1,last_seq=last_seq+1 WHERE room_id=?",
                vec![text(&room)],
            )
            .await?;
            store.emit_current_in(&txn, &room, "attempt").await?;
        }
        txn.commit().await.map_err(storage_err)?;
        Ok(())
    }

    pub(crate) async fn cleanup_room(&self, room: RoomId) -> RtResult<()> {
        use super::store::{column, rows, text};
        let store = self.command_store()?;
        let bindings=rows(store.connection(),"SELECT DISTINCT b.incarnation FROM rt_bindings b WHERE b.room_id=? AND (EXISTS(SELECT 1 FROM rt_attempts a WHERE a.room_id=b.room_id AND a.binding_id=b.binding_id AND a.cleanup_state<>'confirmed') OR EXISTS(SELECT 1 FROM rt_launch_intents l WHERE l.incarnation=b.incarnation AND l.reaped=0))",vec![text(&room.to_string())]).await?;
        for binding in bindings {
            let incarnation = column::<String>(&binding, 0)?
                .parse()
                .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "incarnation"))?;
            let proof = self
                .runtime
                .cancel_and_reap(RuntimeIdentity {
                    incarnation,
                    pid: 0,
                })
                .await?;
            if proof.process.incarnation != incarnation
                || !proof.process.process_tree_empty
                || !proof.mailbox_empty
                || !proof.tools_drained
                || !proof.ingress_drained
            {
                return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_unproven"));
            }
            self.confirm_runtime_cleanup(incarnation).await?;
        }
        Ok(())
    }

    /// Startup reaps quarantined owners before durable recovery. Missing
    /// enumeration or a partial proof keeps the coordinator blocked.
    pub(crate) async fn reconcile_quarantine(&self) -> RtResult<()> {
        if !self.writable()
            || self.unknown_occupancy.load(Ordering::Relaxed)
            || !self.supervisor_online()
        {
            return Ok(());
        }
        let incarnations: Vec<_> = self
            .quarantine
            .lock()
            .expect("quarantine")
            .iter()
            .map(|lease| lease.incarnation)
            .collect();
        if incarnations.is_empty() {
            return Ok(());
        }
        let mut proofs = std::collections::BTreeMap::new();
        for incarnation in incarnations {
            let proof = self
                .runtime
                .cancel_and_reap(RuntimeIdentity {
                    incarnation,
                    pid: 0,
                })
                .await?;
            if proof.process.incarnation != incarnation {
                return Err(rt_error(ErrorCode::RuntimeUnavailable, "cleanup_identity"));
            }
            proofs.insert(incarnation, proof);
        }
        self.acknowledge_cleanup(&PermitReleaseProof {
            lease_id: "startup_recovery".into(),
            proofs,
        })
        .await?;
        Ok(())
    }

    #[cfg(any(test, feature = "test-utils"))]
    async fn enqueue(&self, room: RoomId, prepared: Option<PreparedPrompt>) -> RtResult<RoomReply> {
        let actor = self.actor(&room);
        if !actor.admit() {
            return Ok(reply(ErrorCode::CapacityLimited));
        }
        let enqueued = if let Some(prepared) = prepared {
            try_enqueue(prepared).is_ok()
        } else {
            true
        };
        let recorded = self.record_admitted(&room, enqueued).await;
        if !recorded {
            return Ok(RoomReply {
                code: ErrorCode::StorageUnavailable,
                uncertain: true,
                reason: None,
            });
        }
        let _ = actor;
        Ok(reply(ErrorCode::InvalidState))
    }

    #[cfg(any(test, feature = "test-utils"))]
    async fn record_admitted(&self, room: &RoomId, enqueued: bool) -> bool {
        let _ = (room, enqueued);
        let store = self.store.lock().expect("store").clone();
        store.is_some()
    }

    async fn recover(&self, config: &ServiceConfig) -> RtResult<()> {
        let store = self.store.lock().expect("store").clone();
        let Some(store) = store else {
            return Ok(());
        };
        let intents = store.list_unreaped_launches().await?;
        let mut leases = Vec::new();
        for intent in intents {
            if intent.db.canonical() == config.db_identity.canonical() && !intent.reaped {
                leases.push(QuarantineLease {
                    incarnation: intent.incarnation,
                    released: false,
                });
            }
        }
        match &config.discover {
            Some(provider) => match provider.discover_owned(&config.db_identity).await {
                Ok(found) => {
                    for instance in found {
                        if !leases
                            .iter()
                            .any(|lease| lease.incarnation == instance.incarnation)
                        {
                            leases.push(QuarantineLease {
                                incarnation: instance.incarnation,
                                released: false,
                            });
                        }
                    }
                }
                Err(_) => {
                    self.unknown_occupancy.store(true, Ordering::Relaxed);
                    *self.readiness.lock().expect("readiness") = ServiceReadiness::Blocked {
                        reason: "enumeration_unproven",
                    };
                }
            },
            None => {
                self.unknown_occupancy.store(true, Ordering::Relaxed);
                *self.readiness.lock().expect("readiness") = ServiceReadiness::Blocked {
                    reason: "enumeration_unproven",
                };
            }
        }
        let blocked = self.unknown_occupancy.load(Ordering::Relaxed) || !leases.is_empty();
        *self.quarantine.lock().expect("quarantine") = leases;
        if !blocked && self.supervisor_online() {
            store.recover_durable(self.boot_epoch()).await?;
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-utils"))]
    fn quarantine_open(&self) -> bool {
        self.quarantine
            .lock()
            .expect("quarantine")
            .iter()
            .any(|lease| !lease.released)
    }

    fn note(&self, step: &'static str) {
        self.steps.lock().expect("steps").push(step);
    }
}

pub fn shutdown_order() -> &'static [&'static str] {
    &["revoke", "cleanup", "persist", "unlock"]
}

/// Both application runtimes install this coordinator into the same slot.
pub async fn build_production_service(
    conn: sea_orm::DatabaseConnection,
    data_dir: std::path::PathBuf,
    manager: Arc<crate::acp::manager::ConnectionManager>,
) -> RtResult<Arc<RoundtableService>> {
    let db_path = data_dir.join(crate::db::database_file_name());
    let canonical = db_path
        .canonicalize()
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "db_identity"))?;
    let runtime = Arc::new(super::OwnedParticipantRuntime::new(
        data_dir.clone(),
        manager,
    ));
    let discover = runtime.discovery();
    let store = super::open_roundtable_store(conn).await?;
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir,
            db_path,
            db_identity: DbIdentity::new(canonical.to_string_lossy().into_owned())?,
            discover,
        },
        store,
        runtime,
    )
    .await?;
    if matches!(service.readiness(), ServiceReadiness::Recovering) {
        if let Err(error) = service.reconcile_quarantine().await {
            *service.readiness.lock().expect("readiness") = ServiceReadiness::Blocked {
                reason: "cleanup_unproven",
            };
            tracing::warn!(reason=?error.details.reason,"roundtable startup cleanup blocked");
        }
        if matches!(service.readiness(), ServiceReadiness::Recovering) {
            super::recover_service(&service).await?;
        }
    }
    Ok(service)
}
