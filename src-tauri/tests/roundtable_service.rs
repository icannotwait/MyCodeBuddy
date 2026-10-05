//! Coordinator ownership. Two processes share one database and one lock.

#[path = "roundtable_cases/roundtable_budget_runtime.rs"]
mod roundtable_budget_runtime;

#[path = "roundtable_cases/roundtable_control_recovery.rs"]
mod roundtable_control_recovery;

#[path = "roundtable_cases/roundtable_rollout.rs"]
mod roundtable_rollout;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use roundtable_protocol::{
    CleanupProof, Epoch, ErrorCode, Hash256, IncarnationId, PermitReleaseProof, ProcessTreeProof,
    RoomId,
};
use sea_orm::DatabaseConnection;
use tokio::sync::oneshot;

use codeg_lib::db::{open_configured_sqlite, DbOpenOptions};
use codeg_lib::roundtable::{
    lock_path_for, matches_instance, migrate_roundtable, open_roundtable_store, shutdown_order,
    DbIdentity, IsolationProvider, LaunchIntent, OwnedProcess, ParticipantRuntime,
    PreparedRoundtableConnection, PreparedSandbox, RoomActor, RoomMessage, RoomMessageKind,
    RoundtableLaunch, RoundtableService, RoundtableSlot, RuntimeIdentity, SandboxInstance,
    ServiceConfig, ServiceReadiness,
};

const ROLE: &str = "ROUNDTABLE_P13_ROLE";
const DIR: &str = "ROUNDTABLE_P13_DIR";
const RESULT: &str = "ROUNDTABLE_P13_RESULT";

struct FakeRuntime {
    reaped: AtomicBool,
    prepare_entered: AtomicBool,
    hold: Mutex<Option<oneshot::Receiver<()>>>,
}

impl FakeRuntime {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            reaped: AtomicBool::new(false),
            prepare_entered: AtomicBool::new(false),
            hold: Mutex::new(None),
        })
    }
}

#[async_trait]
impl ParticipantRuntime for FakeRuntime {
    async fn prepare(
        &self,
        _launch: RoundtableLaunch,
    ) -> roundtable_protocol::RtResult<PreparedRoundtableConnection> {
        self.prepare_entered.store(true, Ordering::Relaxed);
        let hold = self.hold.lock().expect("hold").take();
        if let Some(hold) = hold {
            let _ = hold.await;
        }
        Err(roundtable_protocol::RtError {
            code: ErrorCode::RuntimeUnavailable,
            message: "prepare stays outside the gate".to_string(),
            retryable: false,
            current_revision: None,
            details: roundtable_protocol::ErrorDetails {
                reason: Some("prepare_pending".to_string()),
                field_errors: Vec::new(),
            },
        })
    }

    async fn cancel_and_reap(
        &self,
        identity: RuntimeIdentity,
    ) -> roundtable_protocol::RtResult<CleanupProof> {
        self.reaped.store(true, Ordering::Relaxed);
        Ok(proof_for(&identity.incarnation))
    }
}

struct ListedDiscovery {
    instances: Vec<SandboxInstance>,
}

#[async_trait]
impl IsolationProvider for ListedDiscovery {
    async fn prepare(
        &self,
        _plan: &codeg_lib::roundtable::SandboxPlan,
    ) -> roundtable_protocol::RtResult<PreparedSandbox> {
        Err(unavailable("discover_only"))
    }

    async fn spawn(
        &self,
        _prepared: &PreparedSandbox,
        _intent: &LaunchIntent,
    ) -> roundtable_protocol::RtResult<SandboxInstance> {
        Err(unavailable("discover_only"))
    }

    async fn discover_owned(
        &self,
        _db: &DbIdentity,
    ) -> roundtable_protocol::RtResult<Vec<SandboxInstance>> {
        Ok(self.instances.clone())
    }

    async fn reap(
        &self,
        _instance: &SandboxInstance,
    ) -> roundtable_protocol::RtResult<ProcessTreeProof> {
        Err(unavailable("discover_only"))
    }
}

fn unavailable(reason: &str) -> roundtable_protocol::RtError {
    roundtable_protocol::RtError {
        code: ErrorCode::RuntimeUnavailable,
        message: reason.to_string(),
        retryable: false,
        current_revision: None,
        details: roundtable_protocol::ErrorDetails {
            reason: Some(reason.to_string()),
            field_errors: Vec::new(),
        },
    }
}

fn proof_for(incarnation: &IncarnationId) -> CleanupProof {
    CleanupProof {
        process: ProcessTreeProof {
            instance_id: "instance".to_string(),
            incarnation: *incarnation,
            process_tree_empty: true,
        },
        mailbox_empty: true,
        tools_drained: true,
        ingress_drained: true,
    }
}

fn id(nibble: char) -> String {
    format!("00000000-0000-4000-8000-00000000000{nibble}")
}

async fn open_db(dir: &Path) -> DatabaseConnection {
    let path = dir.join("roundtable.db");
    let url = format!(
        "sqlite:{}?mode=rwc",
        urlencoding::encode(path.to_string_lossy().as_ref())
    );
    let conn = open_configured_sqlite(&DbOpenOptions {
        url,
        max_connections: 1,
        min_connections: 1,
        connect_timeout: Duration::from_secs(10),
        idle_timeout: None,
    })
    .await
    .expect("sqlite");
    migrate_roundtable(&conn).await.expect("migrate");
    conn
}

fn config(dir: &Path, discover: Option<Arc<dyn IsolationProvider + Send + Sync>>) -> ServiceConfig {
    ServiceConfig {
        data_dir: dir.to_path_buf(),
        db_path: dir.join("roundtable.db"),
        db_identity: DbIdentity::new("roundtable-service-db").expect("identity"),
        discover,
    }
}

fn listed(instances: Vec<SandboxInstance>) -> Arc<dyn IsolationProvider + Send + Sync> {
    Arc::new(ListedDiscovery { instances })
}

#[tokio::test]
async fn two_processes_one_coordinator() {
    if std::env::var(ROLE).ok().as_deref() == Some("peer") {
        peer().await;
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    let parent = RoundtableService::open(
        config(dir.path(), Some(listed(Vec::new()))),
        store,
        FakeRuntime::new(),
    )
    .await
    .expect("parent");
    assert!(parent.writable());
    let old_boot = parent.boot_epoch();
    let result = dir.path().join("peer-result.txt");
    let output = Command::new(std::env::current_exe().expect("exe"))
        .arg("two_processes_one_coordinator")
        .arg("--exact")
        .arg("--test-threads=1")
        .env(ROLE, "peer")
        .env(DIR, dir.path())
        .env(RESULT, &result)
        .output()
        .expect("peer process");
    assert!(
        output.status.success(),
        "peer stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let child_writable: i32 = std::fs::read_to_string(&result)
        .expect("peer result")
        .trim()
        .parse()
        .expect("peer flag");
    let writable_services = i32::from(parent.writable()) + child_writable;
    assert_eq!(writable_services, 1);
    let automation = dir.path().join("codeg.db.lock");
    assert_ne!(
        lock_path_for(
            dir.path(),
            &DbIdentity::new("roundtable-service-db").expect("id")
        ),
        automation
    );
    parent.shutdown().await.expect("shutdown");
    assert_eq!(parent.shutdown_steps(), shutdown_order());
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    let restarted = RoundtableService::open(
        config(dir.path(), Some(listed(Vec::new()))),
        store,
        FakeRuntime::new(),
    )
    .await
    .expect("restart");
    let new_boot = restarted.boot_epoch();
    assert!(new_boot > old_boot);
    let room = RoomId::from_str(&id('1')).expect("room");
    let old_callback = restarted.callback(&room, old_boot);
    assert_eq!(old_callback.code, ErrorCode::InvalidState);
    assert_eq!(old_callback.reason.as_deref(), Some("stale_fence"));
    let actor = restarted.actor(&room);
    assert!(!actor.stopped());
    restarted.close_observer();
    assert!(!restarted.observer_open());
    assert!(!actor.stopped());
}

async fn peer() {
    let dir = PathBuf::from(std::env::var(DIR).expect("dir"));
    let result = PathBuf::from(std::env::var(RESULT).expect("result"));
    let conn = open_db(&dir).await;
    let store = open_roundtable_store(conn).await.expect("store");
    let service = RoundtableService::open(
        config(&dir, Some(listed(Vec::new()))),
        store,
        FakeRuntime::new(),
    )
    .await
    .expect("peer open");
    let room = RoomId::from_str(&id('2')).expect("room");
    let start = service.start(room).await.expect("start");
    let resume = service
        .send(
            room,
            RoomMessage {
                room_id: RoomId::from_str(&id('2')).expect("room"),
                kind: RoomMessageKind::Resume,
                prepared: None,
            },
        )
        .await
        .expect("resume");
    assert_eq!(start.code, ErrorCode::Forbidden);
    assert_eq!(resume.code, ErrorCode::Forbidden);
    let flag = i32::from(service.writable());
    std::fs::write(result, flag.to_string()).expect("write result");
}

#[tokio::test]
async fn actor_gate_enforces_fake_races_on_real_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    let runtime = FakeRuntime::new();
    let (tx, rx) = oneshot::channel();
    *runtime.hold.lock().expect("hold") = Some(rx);
    let service = RoundtableService::open(config(dir.path(), Some(listed(Vec::new()))), store, {
        let runtime: Arc<dyn ParticipantRuntime> = runtime.clone();
        runtime
    })
    .await
    .expect("service");
    let room = RoomId::from_str(&id('3')).expect("room");
    let actor = RoomActor::new(room, service.boot_epoch());
    let first = actor.admit();
    let second = actor.admit();
    assert!(first);
    assert!(!second);
    actor.finish_admission();
    actor.stop();
    let before = actor.admissions();
    assert!(!actor.admit());
    let post_stop_new_admissions = actor.admissions().saturating_sub(before);
    assert_eq!(post_stop_new_admissions, 0);
    let pending = tokio::spawn({
        let runtime = Arc::clone(&runtime);
        async move {
            runtime.prepare_entered.store(true, Ordering::Relaxed);
            let hold = runtime.hold.lock().expect("hold").take();
            if let Some(hold) = hold {
                let _ = hold.await;
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(runtime.prepare_entered.load(Ordering::Relaxed));
    let cleaned = service
        .cleanup_owned(RuntimeIdentity {
            incarnation: IncarnationId::from_str(&id('5')).expect("incarnation"),
            pid: 44,
        })
        .await
        .expect("cleanup");
    assert!(cleaned.process.process_tree_empty);
    assert!(runtime.reaped.load(Ordering::Relaxed));
    let _ = tx.send(());
    pending.await.expect("prepare task");
}

#[tokio::test]
async fn crash_recovery_blocks_other_room_until_cleanup() {
    let dir = tempfile::tempdir().expect("tempdir");
    let incarnation = IncarnationId::from_str(&id('6')).expect("incarnation");
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    store
        .record_launch_for_test(intent(dir.path(), incarnation))
        .await
        .expect("intent");
    let service = RoundtableService::open(
        config(dir.path(), Some(listed(Vec::new()))),
        store,
        FakeRuntime::new(),
    )
    .await
    .expect("service");
    let other = RoomId::from_str(&id('7')).expect("room");
    let blocked = service.start(other).await.expect("start");
    assert!(matches!(
        blocked.code,
        ErrorCode::CapacityLimited | ErrorCode::RuntimeUnavailable
    ));
    let partial = PermitReleaseProof {
        lease_id: "lease".to_string(),
        proofs: std::collections::BTreeMap::from([(
            incarnation,
            CleanupProof {
                process: ProcessTreeProof {
                    instance_id: "instance".to_string(),
                    incarnation,
                    process_tree_empty: false,
                },
                mailbox_empty: true,
                tools_drained: true,
                ingress_drained: true,
            },
        )]),
    };
    assert_ne!(
        service
            .acknowledge_cleanup(&partial)
            .await
            .expect("partial"),
        ServiceReadiness::Ready
    );
    let mut full_proof = proof_for(&incarnation);
    full_proof.process.process_tree_empty = true;
    let full = PermitReleaseProof {
        lease_id: "lease".to_string(),
        proofs: std::collections::BTreeMap::from([(incarnation, full_proof)]),
    };
    assert_eq!(
        service.acknowledge_cleanup(&full).await.expect("full"),
        ServiceReadiness::Ready
    );
}

#[tokio::test]
async fn spawn_succeeded_but_registration_ack_lost_is_discovered() {
    let dir = tempfile::tempdir().expect("tempdir");
    let incarnation = IncarnationId::from_str(&id('8')).expect("incarnation");
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    store
        .record_launch_for_test(intent(dir.path(), incarnation))
        .await
        .expect("intent");
    let service = RoundtableService::open(
        config(dir.path(), Some(listed(Vec::new()))),
        store,
        FakeRuntime::new(),
    )
    .await
    .expect("service");
    let reply = service
        .start(RoomId::from_str(&id('9')).expect("room"))
        .await
        .expect("start");
    assert_eq!(reply.code, ErrorCode::CapacityLimited);
}

#[tokio::test]
async fn partial_cleanup_cannot_release_bundle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let incarnation = IncarnationId::from_str(&id('a')).expect("incarnation");
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    store
        .record_launch_for_test(intent(dir.path(), incarnation))
        .await
        .expect("intent");
    let service = RoundtableService::open(
        config(dir.path(), Some(listed(Vec::new()))),
        store,
        FakeRuntime::new(),
    )
    .await
    .expect("service");
    let partial = PermitReleaseProof {
        lease_id: "lease".to_string(),
        proofs: std::collections::BTreeMap::new(),
    };
    assert_eq!(
        service
            .acknowledge_cleanup(&partial)
            .await
            .expect("partial"),
        ServiceReadiness::Blocked {
            reason: "partial_cleanup",
        }
    );
}

#[tokio::test]
async fn pid_reuse_does_not_match_instance() {
    let owned = OwnedProcess {
        pid: 42,
        incarnation: IncarnationId::from_str(&id('b')).expect("owned"),
    };
    let reused = IncarnationId::from_str(&id('c')).expect("reused");
    assert!(!matches_instance(owned.pid, &reused, &owned));
    assert!(matches_instance(99, &owned.incarnation, &owned));
}

#[tokio::test]
async fn one_slot_serves_desktop_and_web() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = open_db(dir.path()).await;
    let store = open_roundtable_store(conn).await.expect("store");
    let slot = RoundtableSlot::new();
    let service = RoundtableService::open(
        config(dir.path(), Some(listed(Vec::new()))),
        store,
        FakeRuntime::new(),
    )
    .await
    .expect("service");
    let desktop = slot.install(service);
    let web = slot.install(desktop.clone());
    assert!(Arc::ptr_eq(&desktop, &web));
}

fn intent(_dir: &Path, incarnation: IncarnationId) -> LaunchIntent {
    let db = DbIdentity::new("roundtable-service-db").expect("identity");
    LaunchIntent {
        db: db.clone(),
        boot_epoch: Epoch(1),
        incarnation,
        owner_label: format!("db=roundtable-service-db;boot=1;incarnation={incarnation}"),
        image_digest: "sha256:intent".to_string(),
        plan_hash: Hash256::sha256(b"plan"),
        spawned: None,
        reaped: false,
    }
}
