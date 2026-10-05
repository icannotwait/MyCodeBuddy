//! Independent regressions for the new command and private-delivery integration.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codeg_lib::roundtable::{
    advance_private_watermark, capture_snapshot, commit_captured_manifest, migrate_roundtable,
    open_roundtable_store, DbIdentity, NewPhase, ObjectStore, ParticipantRuntime,
    PreparedRoundtableConnection, ReservationLedger, RoundtableLaunch, RoundtableService,
    RoundtableStore, RuntimeIdentity, SelectedFile, ServiceConfig, SnapshotLimits, SourceClass,
    SourceSelection,
};
use roundtable_protocol::{
    ActorContext, CleanupProof, ClientIdentity, ClientKind, ErrorCode, OperatorScope, RoomId,
    RoundtableConfigV1, RtError, RtResult,
};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;

use crate::support;

struct NoModel;

struct FailedCleanup;

struct ProvenCleanup;

#[async_trait]
impl ParticipantRuntime for ProvenCleanup {
    async fn prepare(&self, _: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection> {
        panic!("shutdown regression cannot launch a model")
    }

    async fn cancel_and_reap(&self, identity: RuntimeIdentity) -> RtResult<CleanupProof> {
        Ok(CleanupProof {
            process: roundtable_protocol::ProcessTreeProof {
                instance_id: "pre-intent-test".into(),
                incarnation: identity.incarnation,
                process_tree_empty: true,
            },
            mailbox_empty: true,
            tools_drained: true,
            ingress_drained: true,
        })
    }
}

#[async_trait]
impl ParticipantRuntime for FailedCleanup {
    async fn prepare(&self, _: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection> {
        panic!("cleanup regression cannot launch a model")
    }

    async fn cancel_and_reap(&self, _: RuntimeIdentity) -> RtResult<CleanupProof> {
        Err(runtime_error())
    }
}

async fn pre_intent_fixture(runtime: Arc<dyn ParticipantRuntime>) -> Fixture {
    let fixture = fixture_with_discovery(
        runtime,
        config(),
        Some(Arc::new(super::product_commands::EmptyDiscovery)),
    )
    .await;
    let (_, conn, store, _, room) = &fixture;
    let phase = Uuid::new_v4().to_string();
    store
        .insert_phase(&NewPhase {
            room_id: room.to_string(),
            phase_id: phase.clone(),
            phase_index: 0,
            revision: 1,
            status: "running".into(),
            snapshot_ref: "proposal".into(),
            snapshot_hash: "ab".repeat(32),
            expected: 2,
            quorum: 2,
            remaining_ms: 450000,
            manifest_id: None,
        })
        .await
        .unwrap();
    let binding = Uuid::new_v4().to_string();
    execute_sql(conn,
        "INSERT INTO rt_bindings(room_id,binding_id,speaker_id,generation,incarnation,external_session_id,policy_ref,certificate_ref,context_state,state) SELECT room_id,?,speaker_id,1,?,'pre-intent','policy','certificate','fresh','ready' FROM rt_speakers WHERE room_id=? LIMIT 1",
        vec![binding.clone().into(),Uuid::new_v4().to_string().into(),room.to_string().into()]
    ).await;
    let turn = Uuid::new_v4().to_string();
    execute_sql(conn,
        "INSERT INTO rt_turns(room_id,turn_id,phase_id,speaker_id,status,admitted_attempt_count) SELECT room_id,?,?,speaker_id,'open',0 FROM rt_bindings WHERE binding_id=?",
        vec![turn.clone().into(),phase.into(),binding.clone().into()]
    ).await;
    execute_sql(conn,
        "INSERT INTO rt_attempts(room_id,attempt_id,turn_id,attempt_no,binding_id,fence,dispatch_state,state,prompt_hash,delivery_hash,cleanup_state,residual_remote_work) VALUES(?,?,?,1,?,1,'unsent','reserved',?,?,'pending',0)",
        vec![room.to_string().into(),Uuid::new_v4().to_string().into(),turn.into(),binding.into(),"ab".repeat(32).into(),"cd".repeat(32).into()]
    ).await;
    fixture
}

#[tokio::test]
async fn product_review_shutdown_keeps_pre_intent_pending_owner_locked() {
    let (_dir, conn, _store, service, _) = pre_intent_fixture(Arc::new(FailedCleanup)).await;
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_launch_intents").await,
        0
    );
    assert!(!service.shutdown().await.unwrap().lock_released);
    assert_eq!(
        support::scalar_text(&conn, "SELECT cleanup_state FROM rt_attempts").await,
        "pending"
    );
}

#[tokio::test]
async fn product_review_shutdown_confirms_pre_intent_cleanup_before_unlock() {
    let (_dir, conn, _store, service, _) = pre_intent_fixture(Arc::new(ProvenCleanup)).await;
    assert!(service.shutdown().await.unwrap().lock_released);
    assert_eq!(
        support::scalar_text(&conn, "SELECT cleanup_state FROM rt_attempts").await,
        "confirmed"
    );
    assert_eq!(
        support::scalar_text(&conn, "SELECT state FROM rt_bindings").await,
        "retired"
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_launch_intents").await,
        0
    );
}

#[async_trait]
impl ParticipantRuntime for NoModel {
    async fn prepare(&self, _: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection> {
        panic!("review regressions must not launch a model")
    }

    async fn cancel_and_reap(&self, _: RuntimeIdentity) -> RtResult<CleanupProof> {
        panic!("review regressions have no process to reap")
    }
}

#[derive(Default)]
struct RecordingRun {
    configs: Mutex<Vec<RoundtableConfigV1>>,
    entered: tokio::sync::Notify,
}

#[async_trait]
impl ParticipantRuntime for RecordingRun {
    async fn prepare(&self, _: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection> {
        panic!("controlled command tests cannot launch a model")
    }

    async fn cancel_and_reap(&self, _: RuntimeIdentity) -> RtResult<CleanupProof> {
        panic!("controlled command tests have no OS process")
    }

    async fn preflight(&self, _: &RoundtableConfigV1) -> RtResult<Value> {
        Ok(json!({"controlled_runtime":true}))
    }

    async fn run_room(
        &self,
        _: RoundtableStore,
        _: RoomId,
        config: RoundtableConfigV1,
    ) -> RtResult<()> {
        self.configs.lock().unwrap().push(config);
        self.entered.notify_one();
        std::future::pending().await
    }
}

fn actor() -> ActorContext {
    ActorContext::from_trusted_entry(
        "00000000-0000-4000-8000-000000000001".parse().unwrap(),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "review-client".into(),
        },
    )
}

fn config() -> Value {
    json!({
        "schema_version":1,"topic":"Review frozen sources","workspace_id":"review-workspace",
        "source_refs":[],"participants":[
            {"ordinal":0,"role":"reviewer","provider_ref":"provider:1"},
            {"ordinal":1,"role":"critic","provider_ref":"provider:2"}
        ],"moderator_ordinal":0,"strategy":{"type":"phased_rounds","version":1,"critique_rounds":0},
        "concurrency":2,"strict_snapshot_v1":true,
        "budgets":{"room_budget":"900000","phase_budget":"450000"},
        "timeouts":{"attempt_timeout":"225000"},
        "quotas":{"output_byte_limit":8192,"input_byte_limit":16384,"interjection_byte_limit":16384}
    })
}

fn runtime_error() -> RtError {
    RtError {
        code: ErrorCode::RuntimeUnavailable,
        message: "Test runtime failed".into(),
        retryable: false,
        current_revision: None,
        details: roundtable_protocol::ErrorDetails {
            reason: Some("runtime_failed".into()),
            field_errors: vec![],
        },
    }
}

type Fixture = (
    TempDir,
    DatabaseConnection,
    RoundtableStore,
    Arc<RoundtableService>,
    RoomId,
);

async fn fixture() -> Fixture {
    fixture_with_runtime(Arc::new(NoModel), config()).await
}

async fn fixture_with_runtime(runtime: Arc<dyn ParticipantRuntime>, config: Value) -> Fixture {
    fixture_with_discovery(runtime, config, None).await
}

async fn fixture_with_discovery(
    runtime: Arc<dyn ParticipantRuntime>,
    config: Value,
    discover: Option<Arc<dyn codeg_lib::roundtable::IsolationProvider + Send + Sync>>,
) -> Fixture {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn.clone()).await.unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("product-review").unwrap(),
            discover,
        },
        store.clone(),
        runtime,
    )
    .await
    .unwrap();
    let created = service
        .execute_fake_command(
            &actor(),
            "roundtable_create",
            json!({
                "request_id":Uuid::new_v4(),"config":config
            }),
        )
        .await
        .unwrap();
    (
        dir,
        conn,
        store,
        service,
        created["room_id"].as_str().unwrap().parse().unwrap(),
    )
}

async fn execute_sql(conn: &DatabaseConnection, sql: &str, values: Vec<sea_orm::Value>) {
    conn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        sql,
        values,
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn product_review_async_cleanup_failure_is_durably_blocked() {
    let (_dir, conn, store, service, room) =
        fixture_with_runtime(Arc::new(FailedCleanup), config()).await;
    let incarnation = Uuid::new_v4();
    execute_sql(&conn,
        "INSERT INTO rt_bindings(room_id,binding_id,speaker_id,generation,incarnation,external_session_id,policy_ref,certificate_ref,context_state,state) SELECT room_id,?,speaker_id,1,?,'cleanup-test','policy','certificate','fresh','ready' FROM rt_speakers WHERE room_id=? LIMIT 1",
        vec![Uuid::new_v4().to_string().into(),incarnation.to_string().into(),room.to_string().into()]
    ).await;
    store
        .record_launch_for_test(codeg_lib::roundtable::LaunchIntent {
            db: DbIdentity::new("product-review").unwrap(),
            boot_epoch: roundtable_protocol::Epoch(service.boot_epoch()),
            incarnation: incarnation.to_string().parse().unwrap(),
            owner_label: "cleanup-test".into(),
            image_digest: "sha256:controlled-test".into(),
            plan_hash: roundtable_protocol::Hash256::sha256(b"cleanup-test"),
            spawned: None,
            reaped: false,
        })
        .await
        .unwrap();
    execute_sql(
        &conn,
        "UPDATE rt_rooms SET status='running' WHERE room_id=?",
        vec![room.to_string().into()],
    )
    .await;
    let request = json!({"room_id":room,"request_id":Uuid::new_v4(),"expected_revision":"1","reason":"cleanup regression"});
    let ack = service
        .execute_fake_command(&actor(), "roundtable_pause", request.clone())
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    while support::scalar_text(&conn, "SELECT status FROM rt_control_operations").await != "blocked"
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        support::scalar_text(&conn, "SELECT status FROM rt_control_operations").await,
        "blocked"
    );
    assert_eq!(
        support::scalar_text(&conn, "SELECT blocked_reason FROM rt_control_operations").await,
        "runtime_failed"
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT reaped FROM rt_launch_intents").await,
        0
    );
    let snapshot = service
        .execute_command(&actor(), "roundtable_get", json!({"room_id":room}))
        .await
        .unwrap();
    assert_eq!(
        snapshot["projection"]["body"]["blocked_reason"],
        "recovery_required"
    );
    assert_eq!(
        service
            .execute_fake_command(&actor(), "roundtable_pause", request)
            .await
            .unwrap(),
        ack
    );
    assert!(!service.shutdown().await.unwrap().lock_released);
}

#[test]
fn product_review_private_watermark_uses_canonical_sequence_strings() {
    let mut seq = 0;
    assert!(
        advance_private_watermark(&mut seq, &json!({"projection":{"body":{"last_seq":"1"}}}))
            .unwrap()
    );
    assert_eq!(seq, 1);
    assert!(
        advance_private_watermark(&mut seq, &json!({"projection":{"body":{"last_seq":"2"}}}))
            .unwrap()
    );
    assert_eq!(seq, 2);
    assert!(
        !advance_private_watermark(&mut seq, &json!({"projection":{"body":{"last_seq":"2"}}}))
            .unwrap()
    );
    assert!(advance_private_watermark(
        &mut seq,
        &json!({"projection":{"body":{"last_seq":"bad"}}})
    )
    .is_err());
    assert_eq!(seq, 2);
}

#[tokio::test]
async fn product_review_current_runtime_failure_leaves_a_readable_paused_projection() {
    let (_dir, conn, _store, service, room) = fixture().await;
    execute_sql(
        &conn,
        "UPDATE rt_rooms SET status='running',run_epoch=2 WHERE room_id=?",
        vec![room.to_string().into()],
    )
    .await;
    let error = runtime_error();
    service
        .record_run_failure_for_test(room, 2, &error)
        .await
        .unwrap();
    let snapshot = service
        .execute_command(&actor(), "roundtable_get", json!({"room_id":room}))
        .await
        .unwrap();
    assert_eq!(snapshot["projection"]["body"]["status"], "paused");
    assert_eq!(
        snapshot["projection"]["body"]["blocked_reason"],
        "recovery_required"
    );
    assert_eq!(snapshot["projection"]["body"]["last_seq"], "2");
}

#[tokio::test]
async fn product_review_old_runtime_failure_cannot_pause_a_resumed_epoch() {
    let (_dir, conn, _store, service, room) = fixture().await;
    execute_sql(
        &conn,
        "UPDATE rt_rooms SET status='running',run_epoch=2 WHERE room_id=?",
        vec![room.to_string().into()],
    )
    .await;
    service
        .record_run_failure_for_test(room, 1, &runtime_error())
        .await
        .unwrap();
    assert_eq!(
        support::scalar_text(&conn, "SELECT status FROM rt_rooms").await,
        "running"
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT run_epoch FROM rt_rooms").await,
        2
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT last_seq FROM rt_rooms").await,
        1
    );
}

#[tokio::test]
async fn product_review_final_synthesis_rejects_next_phase_input_atomically() {
    let (_dir, conn, store, service, room) = fixture().await;
    let phase = Uuid::new_v4().to_string();
    store.insert_phase(&NewPhase {
        room_id: room.to_string(), phase_id: phase.clone(), phase_index: 1, revision: 1,
        status: "running".into(), snapshot_ref: json!({
            "schema_version":1,"phase_id":phase,"phase_index":1,"revision":"1","kind":"synthesis",
            "config_version":"1","question_version":"1","interjection_version":"0",
            "source_manifest_id":Uuid::new_v4(),"source_manifest_hash":"ab".repeat(32),
            "published_messages":[],"members":[],"mandatory_targets":[],"policy_hash":"cd".repeat(32),
            "output_byte_limit":8192,"tool_quota":{"per_call_bytes":8192,"per_attempt_bytes":32768}
        }).to_string(), snapshot_hash: "ab".repeat(32), expected: 1, quorum: 1,
        remaining_ms: 450000, manifest_id: None,
    }).await.unwrap();
    execute_sql(
        &conn,
        "UPDATE rt_rooms SET status='running',current_phase_id=? WHERE room_id=?",
        vec![phase.into(), room.to_string().into()],
    )
    .await;
    let error = service.execute_fake_command(&actor(), "roundtable_interject", json!({
        "room_id":room,"request_id":Uuid::new_v4(),"expected_revision":"1","text":"Too late","mode":"next_phase"
    })).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NoNextPhase);
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_user_inputs").await,
        0
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT last_seq FROM rt_rooms").await,
        1
    );
}

#[tokio::test]
async fn product_review_clone_keeps_the_original_frozen_source_bytes() {
    let (dir, _conn, store, service, room) = fixture().await;
    let root = dir.path().join("selected");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("source.txt"), "frozen contents\n").unwrap();
    let objects = ObjectStore::open(
        dir.path().join("roundtable/objects"),
        Arc::new(ReservationLedger::new(1_000_000)),
        actor().principal_id(),
    )
    .unwrap();
    let manifest = capture_snapshot(
        SourceSelection {
            root: root.clone(),
            room_id: room,
            version: 1,
            base_commit: Some("base-commit".into()),
            files: vec![SelectedFile {
                relative_path: "source.txt".into(),
                class: SourceClass::Tracked,
            }],
            mutate_while_open: None,
        },
        SnapshotLimits {
            estimated_bytes: 1000,
            max_file_bytes: 10000,
            max_total_bytes: 10000,
            max_files: 1,
        },
        &objects,
    )
    .await
    .unwrap();
    commit_captured_manifest(&store, &objects, &manifest)
        .await
        .unwrap();
    let mut selected = config();
    selected["source_refs"] =
        json!([{"snapshot_id":manifest.manifest_id,"base_commit":"base-commit"}]);
    service.execute_fake_command(&actor(),"roundtable_update_draft",json!({"room_id":room,"request_id":Uuid::new_v4(),"expected_revision":"1","config":selected.clone()})).await.unwrap();
    std::fs::write(root.join("source.txt"), "changed after capture\n").unwrap();
    let cloned = service.execute_fake_command(&actor(),"roundtable_clone",json!({"room_id":room,"request_id":Uuid::new_v4(),"expected_revision":"2","carry_published_context":false})).await.unwrap();
    let snapshot = service
        .execute_command(
            &actor(),
            "roundtable_get",
            json!({"room_id":cloned["room_id"]}),
        )
        .await
        .unwrap();
    let preflight = service
        .execute_command(
            &actor(),
            "roundtable_preflight",
            json!({"room_id":cloned["room_id"],"revision":"1","config":snapshot["config"]}),
        )
        .await
        .unwrap();
    assert_eq!(preflight["source_manifests"].as_array().unwrap().len(), 1);
    assert_eq!(
        preflight["source_manifests"][0]["manifest"]["room_id"],
        cloned["room_id"]
    );
    assert_ne!(
        preflight["source_manifests"][0]["manifest"]["manifest_hash"],
        manifest.manifest_hash.to_hex()
    );
    assert_eq!(
        preflight["source_manifests"][0]["manifest"]["entries"][0]["content_hash"],
        manifest.entries[0].content_hash.to_hex()
    );
    assert_eq!(
        preflight["source_manifests"][0]["manifest"]["base_commit"],
        "base-commit"
    );
    assert_eq!(
        objects
            .get_verified(&manifest.entries[0].object)
            .await
            .unwrap(),
        b"frozen contents\n"
    );
}

#[tokio::test]
async fn product_review_default_stop_field_does_not_change_idempotent_response() {
    let (_dir, _conn, _store, service, room) = fixture().await;
    let request = json!({"room_id":room,"request_id":Uuid::new_v4(),"expected_revision":"1"});
    let first = service
        .execute_fake_command(&actor(), "roundtable_stop", request.clone())
        .await
        .unwrap();
    let mut explicit = request;
    explicit["force_latest"] = json!(false);
    let replay = service
        .execute_fake_command(&actor(), "roundtable_stop", explicit)
        .await
        .unwrap();
    assert_eq!(first, replay);
}

#[tokio::test]
async fn product_review_resume_applies_concurrency_to_persistence_and_dispatch() {
    let runtime = Arc::new(RecordingRun::default());
    let mut roomy = config();
    roomy["budgets"] = json!({"room_budget":"1800000","phase_budget":"900000"});
    let (_dir, conn, _store, service, room) = fixture_with_runtime(runtime.clone(), roomy).await;
    execute_sql(
        &conn,
        "UPDATE rt_rooms SET status='paused' WHERE room_id=?",
        vec![room.to_string().into()],
    )
    .await;
    service
        .execute_fake_command(
            &actor(),
            "roundtable_resume",
            json!({
                "room_id":room,"request_id":Uuid::new_v4(),"expected_revision":"1",
                "concurrency":1,"recovery_consent":true
            }),
        )
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.entered.notified(),
    )
    .await
    .expect("resume must dispatch the controlled runtime");
    let config: Value =
        serde_json::from_str(&support::scalar_text(&conn, "SELECT config_ref FROM rt_rooms").await)
            .unwrap();
    assert_eq!(config["concurrency"], 1);
    assert_eq!(runtime.configs.lock().unwrap()[0].concurrency, 1);
    let snapshot = service
        .execute_command(&actor(), "roundtable_get", json!({"room_id":room}))
        .await
        .unwrap();
    assert_eq!(
        snapshot["projection"]["body"]["replay"]["config"]["concurrency"],
        1
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn product_review_resume_rejects_zero_concurrency_before_mutation() {
    let runtime = Arc::new(RecordingRun::default());
    let (_dir, conn, _store, service, room) = fixture_with_runtime(runtime.clone(), config()).await;
    execute_sql(
        &conn,
        "UPDATE rt_rooms SET status='paused' WHERE room_id=?",
        vec![room.to_string().into()],
    )
    .await;
    let error = service
        .execute_fake_command(
            &actor(),
            "roundtable_resume",
            json!({
                "room_id":room,"request_id":Uuid::new_v4(),"expected_revision":"1",
                "concurrency":0,"recovery_consent":true
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        support::scalar_text(&conn, "SELECT status FROM rt_rooms").await,
        "paused"
    );
    assert_eq!(
        support::scalar_i64(&conn, "SELECT last_seq FROM rt_rooms").await,
        1
    );
    assert!(runtime.configs.lock().unwrap().is_empty());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn product_review_retry_synthesis_dispatches_without_an_extra_resume() {
    let runtime = Arc::new(RecordingRun::default());
    let (_dir, conn, store, service, room) = fixture_with_runtime(runtime.clone(), config()).await;
    let phase = Uuid::new_v4().to_string();
    store
        .insert_phase(&NewPhase {
            room_id: room.to_string(),
            phase_id: phase.clone(),
            phase_index: 1,
            revision: 1,
            status: "failed".into(),
            snapshot_ref: "synthesis".into(),
            snapshot_hash: "ab".repeat(32),
            expected: 1,
            quorum: 1,
            remaining_ms: 450000,
            manifest_id: None,
        })
        .await
        .unwrap();
    execute_sql(
        &conn,
        "UPDATE rt_rooms SET status='paused',blocked_reason='synthesis_failed',current_phase_id=? WHERE room_id=?",
        vec![phase.clone().into(), room.to_string().into()],
    )
    .await;
    let request = json!({"room_id":room,"request_id":Uuid::new_v4(),"expected_revision":"1"});
    let ack = service
        .execute_fake_command(&actor(), "roundtable_retry_synthesis", request.clone())
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        runtime.entered.notified(),
    )
    .await
    .expect("an accepted synthesis retry must dispatch without another resume command");
    assert_eq!(
        support::scalar_text(&conn, "SELECT status FROM rt_rooms").await,
        "running"
    );
    assert_eq!(
        support::scalar_i64(
            &conn,
            "SELECT COUNT(*) FROM rt_phases WHERE phase_index=1 AND revision=2"
        )
        .await,
        1
    );
    assert_ne!(
        support::scalar_text(&conn, "SELECT current_phase_id FROM rt_rooms").await,
        phase
    );
    assert_eq!(
        service
            .execute_fake_command(&actor(), "roundtable_retry_synthesis", request)
            .await
            .unwrap(),
        ack
    );
    assert_eq!(runtime.configs.lock().unwrap().len(), 1);
    service.shutdown().await.unwrap();
}
