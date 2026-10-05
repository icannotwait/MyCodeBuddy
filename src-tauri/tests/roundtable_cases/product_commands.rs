//! Exercise the command service with SQLite, without a model or public bus.

use std::sync::Arc;

use async_trait::async_trait;
use codeg_lib::roundtable::{
    migrate_roundtable, open_roundtable_store, DbIdentity, ParticipantRuntime,
    PreparedRoundtableConnection, RoundtableLaunch, RoundtableService, RuntimeIdentity,
    ServiceConfig,
};
use roundtable_protocol::{
    ActorContext, CleanupProof, ClientIdentity, ClientKind, ErrorCode, OperatorScope,
    PermitReleaseProof, RtResult,
};
use serde_json::json;

use crate::support;

#[tokio::test]
async fn product_http_routes_share_persisted_commands_and_authentication() {
    use codeg_lib::app_state::AppState;
    use codeg_lib::db::test_helpers::fresh_in_memory_db;
    use codeg_lib::web::{router::build_router, shutdown::ShutdownSignal};
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("http-command-review").unwrap(),
            discover: None,
        },
        open_roundtable_store(conn).await.unwrap(),
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    let create = json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config()});
    let ack = service
        .execute_fake_command(&actor(1), "roundtable_create", create.clone())
        .await
        .unwrap();
    let room = ack["room_id"].clone();
    let view = service
        .execute_command(&actor(1), "roundtable_get", json!({"room_id":room}))
        .await
        .unwrap();
    let state = Arc::new(AppState::new_for_test(
        fresh_in_memory_db().await,
        dir.path().into(),
    ));
    state.roundtable.install(service.clone());
    let scoped = state.web_server_state.completion_authorizations().issue(1);
    let server = axum_test::TestServer::new(build_router(
        state,
        "roundtable-test-token".into(),
        dir.path().into(),
        Arc::new(ShutdownSignal::new()),
    ))
    .unwrap();
    let mutation = json!({"room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":"1"});
    let commands = vec![
        ("roundtable_create", create),
        (
            "roundtable_preflight",
            json!({"room_id":room,"revision":"1","config":config()}),
        ),
        ("roundtable_update_draft", {
            let mut body = mutation.clone();
            body["config"] = config();
            body
        }),
        ("roundtable_start", mutation.clone()),
        ("roundtable_get", json!({"room_id":room})),
        ("roundtable_list", json!({"workspace_id":"test-workspace"})),
        ("roundtable_pause", {
            let mut body = mutation.clone();
            body["reason"] = json!("review");
            body
        }),
        ("roundtable_resume", {
            let mut body = mutation.clone();
            body["recovery_consent"] = json!(true);
            body
        }),
        ("roundtable_stop", mutation.clone()),
        ("roundtable_interject", {
            let mut body = mutation.clone();
            body["mode"] = json!("next_phase");
            body["text"] = json!("review");
            body
        }),
        ("roundtable_retry_synthesis", mutation.clone()),
        (
            "roundtable_events",
            json!({"room_id":room,"after_seq":"0","through_seq":"1"}),
        ),
        (
            "roundtable_messages",
            json!({"room_id":room,"manifest_id":view["message_manifest_id"]}),
        ),
        (
            "roundtable_evidence",
            json!({"room_id":room,"evidence_id":uuid::Uuid::new_v4().to_string()}),
        ),
        (
            "roundtable_operation",
            json!({"room_id":room,"operation_id":uuid::Uuid::new_v4().to_string()}),
        ),
        ("roundtable_clone", {
            let mut body = mutation;
            body["carry_published_context"] = json!(false);
            body
        }),
        (
            "roundtable_attach",
            json!({"room_id":room,"subscription_id":uuid::Uuid::new_v4().to_string()}),
        ),
        (
            "roundtable_detach",
            json!({"room_id":room,"subscription_id":uuid::Uuid::new_v4().to_string()}),
        ),
    ];
    assert_eq!(commands.len(), 18);
    for (command, body) in commands {
        let direct = service
            .execute_command(&actor(1), command, body.clone())
            .await;
        let (status, expected) = match direct {
            Ok(body) => (200, body),
            Err(error) => (
                if error.details.reason.as_deref() == Some("not_found") {
                    404
                } else {
                    error.code.http_status()
                },
                serde_json::to_value(error).unwrap(),
            ),
        };
        let response = server
            .post(&format!("/api/{command}"))
            .add_header("authorization", "Bearer roundtable-test-token")
            .json(&json!({"request":body}))
            .await;
        assert_eq!(response.status_code().as_u16(), status, "{command}");
        assert_eq!(response.json::<serde_json::Value>(), expected, "{command}");
    }
    let missing = server
        .post("/api/roundtable_get")
        .json(&json!({"room_id":room}))
        .await;
    assert_eq!(missing.status_code(), 401);
    let scoped = server
        .post("/api/roundtable_get")
        .add_header("authorization", "Bearer roundtable-test-token")
        .add_header(codeg_lib::web::auth::COMPLETION_CONTEXT_HEADER, scoped)
        .json(&json!({"room_id":room}))
        .await;
    assert_eq!(scoped.status_code(), 403);
    let duplicate = server.post("/api/roundtable_stop")
        .add_header("authorization", "Bearer roundtable-test-token")
        .add_header("content-type", "application/json")
        .bytes(axum::body::Bytes::from(format!(
            "{{\"room_id\":{},\"request_id\":\"{}\",\"expected_revision\":\"1\",\"force_latest\":false,\"force_latest\":true}}",
            room, uuid::Uuid::new_v4()
        ))).await;
    assert_eq!(duplicate.status_code(), 400);
    let duplicate_error = duplicate.json::<roundtable_protocol::RtError>();
    assert_eq!(
        duplicate_error.details.reason.as_deref(),
        Some("duplicate_key")
    );
    service.shutdown().await.unwrap();
}

struct NoModel;

#[async_trait]
impl ParticipantRuntime for NoModel {
    async fn prepare(&self, _: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection> {
        panic!("draft/read commands cannot launch a model")
    }
    async fn cancel_and_reap(&self, _: RuntimeIdentity) -> RtResult<CleanupProof> {
        panic!("no participant was launched")
    }
}

fn actor(n: u8) -> ActorContext {
    ActorContext::from_trusted_entry(
        format!("00000000-0000-4000-8000-00000000000{n}")
            .parse()
            .unwrap(),
        OperatorScope::SingleOperator,
        ClientIdentity {
            kind: ClientKind::Web,
            session_ref: "authenticated-test-client".into(),
        },
    )
}

fn config() -> serde_json::Value {
    json!({
        "schema_version":1,"topic":"Review the implementation",
        "workspace_id":"test-workspace","source_refs":[],
        "participants":[
            {"ordinal":0,"role":"reviewer","provider_ref":"provider:a"},
            {"ordinal":1,"role":"critic","provider_ref":"provider:b"}
        ],
        "moderator_ordinal":0,
        "strategy":{"type":"phased_rounds","version":1,"critique_rounds":0},
        "concurrency":1,"strict_snapshot_v1":true,
        "budgets":{"room_budget":"900000","phase_budget":"450000"},
        "timeouts":{"attempt_timeout":"225000"},
        "quotas":{"output_byte_limit":8192,"input_byte_limit":16384,"interjection_byte_limit":16384}
    })
}

#[tokio::test]
async fn product_create_read_retry_and_restart_are_durable() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn.clone()).await.unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().to_path_buf(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("product-test").unwrap(),
            discover: None,
        },
        store,
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    let request = json!({"request_id":"00000000-0000-4000-8000-000000000020","config":config()});
    // Fake admission is explicit; it must not change the on-disk product gate.
    let created = service
        .execute_fake_command(&actor(1), "roundtable_create", request.clone())
        .await
        .unwrap();
    let retried = service
        .execute_fake_command(&actor(1), "roundtable_create", request)
        .await
        .unwrap();
    assert_eq!(created, retried);
    let room = created["room_id"].as_str().unwrap();
    let fetched = service
        .execute_command(&actor(1), "roundtable_get", json!({"room_id":room}))
        .await
        .unwrap();
    assert_eq!(fetched["config"]["topic"], "Review the implementation");
    let hidden = service
        .execute_command(&actor(2), "roundtable_get", json!({"room_id":room}))
        .await
        .unwrap_err();
    assert_eq!(hidden.code, ErrorCode::Forbidden);
    assert_eq!(
        support::scalar_i64(&conn, "SELECT COUNT(*) FROM rt_rooms").await,
        1
    );
    let projection = fetched["projection"].clone();
    service.shutdown().await.unwrap();
    let reopened = open_roundtable_store(conn).await.unwrap();
    assert_eq!(
        reopened
            .projection(&room.parse().unwrap(), None)
            .await
            .unwrap()
            .body
            .last_seq
            .0,
        projection["body"]["last_seq"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
    );
    assert!(!dir.path().join("roundtable/execution-policy.json").exists());
}

#[tokio::test]
async fn product_create_gate_and_body_principal_are_enforced() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("product-denial").unwrap(),
            discover: None,
        },
        open_roundtable_store(conn).await.unwrap(),
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    let error = service
        .execute_command(
            &actor(1),
            "roundtable_create",
            json!({"request_id":"00000000-0000-4000-8000-000000000021","config":config()}),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.code,
        ErrorCode::CapabilityUnqualified | ErrorCode::RuntimeUnavailable | ErrorCode::InvalidState
    ));
    let error = service
        .execute_command(
            &actor(1),
            "roundtable_list",
            json!({"workspace_id":"test-workspace", "principal_id":"spoof"}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    // A forged empty cleanup does not clear unproven occupancy.
    service
        .acknowledge_cleanup(&PermitReleaseProof {
            lease_id: "forged".into(),
            proofs: Default::default(),
        })
        .await
        .unwrap();
    assert!(service.quarantine_pending());
}

#[tokio::test]
async fn product_read_only_coordinator_can_read_history() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let open = || ServiceConfig {
        data_dir: dir.path().into(),
        db_path: dir.path().join("roundtable.db"),
        db_identity: DbIdentity::new("product-history").unwrap(),
        discover: None,
    };
    let writer = RoundtableService::open(
        open(),
        open_roundtable_store(conn.clone()).await.unwrap(),
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    let created = writer
        .execute_fake_command(
            &actor(1),
            "roundtable_create",
            json!({"request_id":"00000000-0000-4000-8000-000000000022","config":config()}),
        )
        .await
        .unwrap();
    let reader = RoundtableService::open(
        open(),
        open_roundtable_store(conn).await.unwrap(),
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    assert!(!reader.writable());
    let room = created["room_id"].as_str().unwrap();
    let snapshot = reader
        .execute_command(&actor(1), "roundtable_get", json!({"room_id":room}))
        .await
        .unwrap();
    assert_eq!(
        snapshot["message_manifest_id"],
        snapshot["projection"]["projection_ref"]["id"]
    );
    let messages = reader
        .execute_command(
            &actor(1),
            "roundtable_messages",
            json!({"room_id":room,"manifest_id":snapshot["message_manifest_id"]}),
        )
        .await
        .unwrap();
    assert_eq!(messages["messages"], json!([]));
    assert!(messages["cursor"].is_null());
    let denied = reader
        .execute_fake_command(
            &actor(1),
            "roundtable_create",
            json!({"request_id":"00000000-0000-4000-8000-000000000023","config":config()}),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn product_shutdown_keeps_lock_when_occupancy_is_unproven() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("product-shutdown").unwrap(),
            discover: None,
        },
        open_roundtable_store(conn).await.unwrap(),
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    let result = service.shutdown().await.unwrap();
    assert!(
        !result.lock_released,
        "an unproven process tree cannot release coordinator ownership"
    );
    assert!(service.writable());
}

pub(crate) struct EmptyDiscovery;

#[async_trait]
impl codeg_lib::roundtable::IsolationProvider for EmptyDiscovery {
    async fn prepare(
        &self,
        _: &codeg_lib::roundtable::SandboxPlan,
    ) -> RtResult<codeg_lib::roundtable::PreparedSandbox> {
        panic!("no process")
    }
    async fn spawn(
        &self,
        _: &codeg_lib::roundtable::PreparedSandbox,
        _: &codeg_lib::roundtable::LaunchIntent,
    ) -> RtResult<codeg_lib::roundtable::SandboxInstance> {
        panic!("no process")
    }
    async fn discover_owned(
        &self,
        _: &DbIdentity,
    ) -> RtResult<Vec<codeg_lib::roundtable::SandboxInstance>> {
        Ok(vec![])
    }
    async fn reap(
        &self,
        _: &codeg_lib::roundtable::SandboxInstance,
    ) -> RtResult<roundtable_protocol::ProcessTreeProof> {
        panic!("no process")
    }
}

struct PendingRun {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    after_release: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl ParticipantRuntime for PendingRun {
    async fn prepare(&self, _: RoundtableLaunch) -> RtResult<PreparedRoundtableConnection> {
        panic!("no process")
    }
    async fn cancel_and_reap(&self, _: RuntimeIdentity) -> RtResult<CleanupProof> {
        panic!("no process")
    }
    async fn preflight(
        &self,
        _: &roundtable_protocol::RoundtableConfigV1,
    ) -> RtResult<serde_json::Value> {
        Ok(json!({"qualified":"controlled-test"}))
    }
    async fn run_room(
        &self,
        _: codeg_lib::roundtable::RoundtableStore,
        _: roundtable_protocol::RoomId,
        _: roundtable_protocol::RoundtableConfigV1,
    ) -> RtResult<()> {
        self.entered.notify_one();
        self.release.notified().await;
        self.after_release
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn product_shutdown_drains_running_tasks_before_unlock() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let runtime = Arc::new(PendingRun {
        entered: Default::default(),
        release: Default::default(),
        after_release: Default::default(),
    });
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("shutdown-task-review").unwrap(),
            discover: Some(Arc::new(EmptyDiscovery)),
        },
        open_roundtable_store(conn.clone()).await.unwrap(),
        runtime.clone(),
    )
    .await
    .unwrap();
    let created = service
        .execute_fake_command(
            &actor(1),
            "roundtable_create",
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config()}),
        )
        .await
        .unwrap();
    let room = created["room_id"].clone();
    let preflight = service
        .execute_fake_command(
            &actor(1),
            "roundtable_preflight",
            json!({"room_id":room,"revision":"1","config":config()}),
        )
        .await
        .unwrap();
    service.execute_fake_command(&actor(1),"roundtable_start",json!({"room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":"1","confirmed_preflight_id":preflight["confirmed_preflight_id"]})).await.unwrap();
    runtime.entered.notified().await;
    assert!(service.shutdown().await.unwrap().lock_released);
    runtime.release.notify_one();
    tokio::task::yield_now().await;
    assert!(
        !runtime
            .after_release
            .load(std::sync::atomic::Ordering::SeqCst),
        "running task continued after coordinator unlock"
    );
    assert_eq!(
        support::scalar_text(&conn, "SELECT status FROM rt_rooms").await,
        "paused"
    );
}

#[tokio::test]
async fn product_mutation_ack_uses_protocol_decimal_strings() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("ack-wire-review").unwrap(),
            discover: None,
        },
        open_roundtable_store(conn).await.unwrap(),
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    let ack = service
        .execute_fake_command(
            &actor(1),
            "roundtable_create",
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config()}),
        )
        .await
        .unwrap();
    assert_eq!(ack["revision"], "1");
    assert_eq!(ack["run_epoch"], "0");
    assert_eq!(ack["last_seq"], "1");
}

#[tokio::test]
async fn product_cleanup_ack_persists_reaping_and_recovers_rooms() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let store = open_roundtable_store(conn.clone()).await.unwrap();
    let identity = DbIdentity::new("cleanup-ack-review").unwrap();
    let open = || ServiceConfig {
        data_dir: dir.path().into(),
        db_path: dir.path().join("roundtable.db"),
        db_identity: identity.clone(),
        discover: Some(Arc::new(EmptyDiscovery)),
    };
    let first = RoundtableService::open(open(), store.clone(), Arc::new(NoModel))
        .await
        .unwrap();
    first
        .execute_fake_command(
            &actor(1),
            "roundtable_create",
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config()}),
        )
        .await
        .unwrap();
    use sea_orm::ConnectionTrait;
    conn.execute_unprepared("UPDATE rt_rooms SET status='running',run_epoch=1")
        .await
        .unwrap();
    let incarnation = "00000000-0000-4000-8000-000000000099".parse().unwrap();
    store
        .record_launch_for_test(codeg_lib::roundtable::LaunchIntent {
            db: identity.clone(),
            boot_epoch: roundtable_protocol::Epoch(first.boot_epoch()),
            incarnation,
            owner_label: "cleanup-ack-owner".into(),
            image_digest: "sha256:controlled-test".into(),
            plan_hash: roundtable_protocol::Hash256::sha256(b"test"),
            spawned: None,
            reaped: false,
        })
        .await
        .unwrap();
    drop(first);
    let reopened = RoundtableService::open(open(), store, Arc::new(NoModel))
        .await
        .unwrap();
    assert!(reopened.quarantine_pending());
    let proof = CleanupProof {
        process: roundtable_protocol::ProcessTreeProof {
            instance_id: "test-instance".into(),
            incarnation,
            process_tree_empty: true,
        },
        mailbox_empty: true,
        tools_drained: true,
        ingress_drained: true,
    };
    reopened
        .acknowledge_cleanup(&PermitReleaseProof {
            lease_id: "test-proof".into(),
            proofs: std::collections::BTreeMap::from([(incarnation, proof)]),
        })
        .await
        .unwrap();
    assert_eq!(
        support::scalar_i64(&conn, "SELECT reaped FROM rt_launch_intents").await,
        1
    );
    assert_eq!(
        support::scalar_text(&conn, "SELECT status FROM rt_rooms").await,
        "paused"
    );
}

#[tokio::test]
async fn product_reads_return_wire_usage_metrics_and_event_envelopes() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("read-model-review").unwrap(),
            discover: None,
        },
        open_roundtable_store(conn).await.unwrap(),
        Arc::new(NoModel),
    )
    .await
    .unwrap();
    let ack = service
        .execute_fake_command(
            &actor(1),
            "roundtable_create",
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config()}),
        )
        .await
        .unwrap();
    let room = ack["room_id"].clone();
    let events = service
        .execute_command(
            &actor(1),
            "roundtable_events",
            json!({"room_id":room,"after_seq":"0","through_seq":"1"}),
        )
        .await
        .unwrap();
    assert_eq!(events["events"][0]["seq"], "1");
    assert_eq!(events["events"][0]["resulting_revision"], "1");
    assert_eq!(events["events"][0]["payload"]["cause"], "create");
    let metrics = service
        .execute_command(
            &actor(1),
            "roundtable_get",
            json!({"room_id":room,"read":{"metrics":{}}}),
        )
        .await
        .unwrap();
    let metrics: roundtable_protocol::MetricsViewV1 = serde_json::from_value(metrics).unwrap();
    assert_eq!(metrics.event_count.0, 1);
    assert!(metrics.storage_used_bytes.0 > 0);
    let usage = service
        .execute_command(
            &actor(1),
            "roundtable_get",
            json!({"room_id":room,"read":{"usage":{}}}),
        )
        .await
        .unwrap();
    assert!(usage["usage_version"].is_string());
    let typed_usage: roundtable_protocol::UsageViewV1 =
        serde_json::from_value(usage.clone()).unwrap();
    assert_eq!(typed_usage.confirmed_output_tokens, None);
    let replay = service
        .execute_command(
            &actor(1),
            "roundtable_get",
            json!({"room_id":room,"read":{"usage":{"after_usage_version":usage["usage_version"]}}}),
        )
        .await
        .unwrap();
    assert_eq!(replay["measurements"], json!([]));
}

#[tokio::test]
async fn product_completed_control_replay_does_not_abort_a_resumed_run() {
    let (dir, conn) = support::open_pool(1).await;
    migrate_roundtable(&conn).await.unwrap();
    let runtime = Arc::new(PendingRun {
        entered: Default::default(),
        release: Default::default(),
        after_release: Default::default(),
    });
    let service = RoundtableService::open(
        ServiceConfig {
            data_dir: dir.path().into(),
            db_path: dir.path().join("roundtable.db"),
            db_identity: DbIdentity::new("control-replay-review").unwrap(),
            discover: Some(Arc::new(EmptyDiscovery)),
        },
        open_roundtable_store(conn.clone()).await.unwrap(),
        runtime.clone(),
    )
    .await
    .unwrap();
    let created = service
        .execute_fake_command(
            &actor(1),
            "roundtable_create",
            json!({"request_id":uuid::Uuid::new_v4().to_string(),"config":config()}),
        )
        .await
        .unwrap();
    let room = created["room_id"].clone();
    let preflight = service
        .execute_fake_command(
            &actor(1),
            "roundtable_preflight",
            json!({"room_id":room,"revision":"1","config":config()}),
        )
        .await
        .unwrap();
    service.execute_fake_command(&actor(1),"roundtable_start",json!({"room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":"1","confirmed_preflight_id":preflight["confirmed_preflight_id"]})).await.unwrap();
    runtime.entered.notified().await;
    let pause = json!({"room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":"2","reason":"review"});
    let ack = service
        .execute_fake_command(&actor(1), "roundtable_pause", pause.clone())
        .await
        .unwrap();
    let revision = support::scalar_i64(&conn, "SELECT revision FROM rt_rooms")
        .await
        .to_string();
    service.execute_fake_command(&actor(1),"roundtable_resume",json!({"room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":revision,"recovery_consent":true})).await.unwrap();
    runtime.entered.notified().await;
    assert_eq!(
        service
            .execute_fake_command(&actor(1), "roundtable_pause", pause.clone())
            .await
            .unwrap(),
        ack
    );
    runtime.release.notify_one();
    tokio::task::yield_now().await;
    assert!(
        runtime
            .after_release
            .load(std::sync::atomic::Ordering::SeqCst),
        "old pause replay aborted the new epoch"
    );
    use sea_orm::ConnectionTrait;
    conn.execute_unprepared("INSERT INTO rt_phases(room_id,phase_id,phase_index,revision,status,snapshot_ref,snapshot_hash,expected,quorum,remaining_ms) SELECT room_id,'00000000-0000-4000-8000-0000000000f1',0,1,'ready','proposal','test',2,2,450000 FROM rt_rooms")
        .await.unwrap();
    conn.execute_unprepared(
        "UPDATE rt_rooms SET current_phase_id='00000000-0000-4000-8000-0000000000f1'",
    )
    .await
    .unwrap();
    let revision = support::scalar_i64(&conn, "SELECT revision FROM rt_rooms")
        .await
        .to_string();
    let input = json!({"room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":revision,"mode":"next_phase","text":"Please verify recovery"});
    let input_ack = service
        .execute_fake_command(&actor(1), "roundtable_interject", input.clone())
        .await
        .unwrap();
    service.shutdown().await.unwrap();
    assert_eq!(
        service
            .execute_command(&actor(1), "roundtable_interject", input)
            .await
            .unwrap(),
        input_ack,
        "completed next-phase input must remain replayable while draining"
    );
    assert_eq!(
        service
            .execute_command(&actor(1), "roundtable_pause", pause)
            .await
            .unwrap(),
        ack,
        "draining must still replay an immutable acknowledgment"
    );
    assert_eq!(
        service
            .execute_fake_command(
                &actor(1),
                "roundtable_stop",
                json!({"room_id":room,"request_id":uuid::Uuid::new_v4().to_string(),"expected_revision":"1","force_latest":true}),
            )
            .await
            .unwrap_err()
            .details
            .reason
            .as_deref(),
        Some("service_draining")
    );
}
