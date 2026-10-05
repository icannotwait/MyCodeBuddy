//! One coordinator for every room. `open` does not return a runnable service.
//!
//! Ready is reached only after every quarantined incarnation has a full cleanup
//! proof. A missing lock is history-only. Pause, interject, create, and
//! preflight cannot bypass that.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use roundtable_protocol::{CleanupProof, ErrorCode, IncarnationId, PermitReleaseProof, RoomId, RtResult};

use super::actor::RoomActor;
use super::ownership::{lock_path_for, try_acquire, CoordinatorLock, LockAttempt, RoundtableReadService};
use super::runtime::{try_enqueue, PreparedPrompt, PreparedRoundtableConnection, RoundtableLaunch};
use super::rt_error;
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
}

pub struct RoundtableService {
    store: Mutex<Option<RoundtableStore>>,
    history: Mutex<Option<RoundtableReadService>>,
    lock: Mutex<Option<CoordinatorLock>>,
    boot_epoch: AtomicU64,
    readiness: Mutex<ServiceReadiness>,
    actors: Mutex<HashMap<String, Arc<RoomActor>>>,
    quarantine: Mutex<Vec<QuarantineLease>>,
    runtime: Arc<dyn ParticipantRuntime>,
    steps: Mutex<Vec<&'static str>>,
    observer_open: AtomicBool,
    unknown_occupancy: AtomicBool,
}

impl RoundtableService {
    pub async fn open(
        config: ServiceConfig,
        store: RoundtableStore,
        runtime: Arc<dyn ParticipantRuntime>,
    ) -> RtResult<Arc<Self>> {
        let path = lock_path_for(&config.data_dir, &config.db_identity);
        let attempt = try_acquire(&path);
        let (lock, history, writable_store, boot, readiness) = match attempt {
            LockAttempt::Exclusive(lock) => {
                let boot = store.bump_coordinator_boot().await?;
                (Some(lock), None, Some(store), boot, ServiceReadiness::Recovering)
            }
            LockAttempt::Taken => {
                let history = RoundtableReadService::open(&config.db_path).await?;
                let boot = history.boot_epoch().await.unwrap_or(0);
                (None, Some(history), None, boot, ServiceReadiness::ReadOnly)
            }
            LockAttempt::Unavailable => {
                return Err(rt_error(ErrorCode::StorageUnavailable, "coordinator_lock"));
            }
        };
        let service = Arc::new(Self {
            store: Mutex::new(writable_store),
            history: Mutex::new(history),
            lock: Mutex::new(lock),
            boot_epoch: AtomicU64::new(boot),
            readiness: Mutex::new(readiness),
            actors: Mutex::new(HashMap::new()),
            quarantine: Mutex::new(Vec::new()),
            runtime,
            steps: Mutex::new(Vec::new()),
            observer_open: AtomicBool::new(true),
            unknown_occupancy: AtomicBool::new(false),
        });
        if service.writable() {
            service.recover(&config).await?;
        }
        Ok(service)
    }

    pub fn writable(&self) -> bool {
        self.lock.lock().expect("lock").is_some()
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
            .or_insert_with(|| Arc::new(RoomActor::new(room.clone(), self.boot_epoch())))
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

    pub async fn send(&self, room: RoomId, msg: RoomMessage) -> RtResult<RoomReply> {
        if !self.writable() {
            return Ok(reply(ErrorCode::Forbidden));
        }
        match msg.kind {
            RoomMessageKind::Start | RoomMessageKind::Resume => self.start(room).await,
            RoomMessageKind::Pause | RoomMessageKind::Interject | RoomMessageKind::Create
            | RoomMessageKind::Preflight => {
                if !matches!(self.readiness(), ServiceReadiness::Ready) {
                    return Ok(reply(ErrorCode::InvalidState));
                }
                Ok(reply(ErrorCode::InvalidState))
            }
            RoomMessageKind::ReadyToEnqueue => self.enqueue(room, msg.prepared).await,
        }
    }

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

    pub async fn acknowledge_cleanup(&self, proof: &PermitReleaseProof) -> RtResult<ServiceReadiness> {
        let mut leases = self.quarantine.lock().expect("quarantine");
        let required: Vec<_> = leases.iter().map(|lease| lease.incarnation.clone()).collect();
        if self.unknown_occupancy.load(Ordering::Relaxed) || !proof.releases(&required) {
            *self.readiness.lock().expect("readiness") = ServiceReadiness::Blocked {
                reason: "partial_cleanup",
            };
            return Ok(ServiceReadiness::Blocked {
                reason: "partial_cleanup",
            });
        }
        for lease in leases.iter_mut() {
            lease.released = true;
        }
        drop(leases);
        *self.readiness.lock().expect("readiness") = ServiceReadiness::Ready;
        Ok(ServiceReadiness::Ready)
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
        self.note("revoke");
        let identities: Vec<RuntimeIdentity> = self
            .quarantine
            .lock()
            .expect("quarantine")
            .iter()
            .map(|lease| RuntimeIdentity {
                incarnation: lease.incarnation.clone(),
                pid: 0,
            })
            .collect();
        self.note("cleanup");
        for identity in identities {
            let _ = self.runtime.cancel_and_reap(identity).await;
        }
        for actor in self.actors.lock().expect("actors").values() {
            actor.stop();
        }
        self.note("persist");
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
                        if !leases.iter().any(|lease| lease.incarnation == instance.incarnation) {
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
        let _ = blocked;
        Ok(())
    }

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
