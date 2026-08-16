//! Shared ACP session HTTP contracts exercised through the real Axum router.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use axum_test::TestServer;
use codeg_lib::acp::connection::{RegisteredSpawnAttempt, RouteBootstrapOutcome};
use codeg_lib::acp::delegation::route::RouteDegradedReason;
use codeg_lib::acp::error::AcpError;
use codeg_lib::acp::manager::{ConnectionManager, SharedConnectLaunch, SharedSpawnDriver};
use codeg_lib::acp::session_state::SessionState;
use codeg_lib::acp::shared_session::{
    SharedDisposition, SharedSessionPhase, MAX_ACTIVE_LEASES, MAX_CLIENT_LABEL_LEN,
    MAX_CONNECT_LEDGER_ENTRIES, MAX_EXPIRED_LEASE_TOMBSTONES, MAX_PROMPT_LEDGER_ENTRIES,
    MAX_REPLACED_CONNECTION_TOMBSTONES, MAX_WAITING_BYTES, MAX_WAITING_PROMPTS,
};
use codeg_lib::app_state::AppState;
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
use codeg_lib::models::AgentType;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{oneshot, RwLock};

const TEST_TOKEN: &str = "shared-http-token";

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct AcpConnectOrAttachResponse {
    connection_id: String,
    generation: u64,
    lease_id: String,
    lease_expires_at: String,
    disposition: SharedDisposition,
    phase: SharedPublicPhase,
    event_seq: u64,
    error: Option<SharedConnectFailure>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SharedPublicPhase {
    Bootstrapping,
    Ready,
    Failed,
    Closing,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct SharedConnectFailure {
    code: String,
    retryable: bool,
    cleanup_complete: bool,
}

#[derive(Debug)]
struct HttpResponse {
    status: StatusCode,
    body: Value,
}

impl HttpResponse {
    fn assert_status(self, status: StatusCode) -> Self {
        assert_eq!(self.status, status, "response body: {}", self.body);
        self
    }

    fn assert_status_ok(self) -> Self {
        self.assert_status(StatusCode::OK)
    }

    fn assert_status_bad_request(self) -> Self {
        self.assert_status(StatusCode::BAD_REQUEST)
    }

    fn assert_status_conflict(self) -> Self {
        self.assert_status(StatusCode::CONFLICT)
    }

    fn assert_status_gone(self) -> Self {
        self.assert_status(StatusCode::GONE)
    }

    fn assert_status_too_many_requests(self) -> Self {
        self.assert_status(StatusCode::TOO_MANY_REQUESTS)
    }

    fn assert_status_unauthorized(self) -> Self {
        self.assert_status(StatusCode::UNAUTHORIZED)
    }

    fn assert_code(self, code: &str) -> Self {
        assert_eq!(self.body.get("code").and_then(Value::as_str), Some(code));
        self
    }

    fn json<T: DeserializeOwned>(self) -> T {
        serde_json::from_value(self.body).expect("typed response body")
    }
}

struct ControlledSpawnDriver {
    outcomes: Mutex<VecDeque<oneshot::Receiver<RouteBootstrapOutcome>>>,
    starts: AtomicUsize,
}

impl ControlledSpawnDriver {
    fn new(
        outcome: BootstrapOutcome,
    ) -> (Arc<Self>, Option<oneshot::Sender<RouteBootstrapOutcome>>) {
        let (tx, rx) = oneshot::channel();
        let pending = match outcome {
            BootstrapOutcome::Pending => Some(tx),
            BootstrapOutcome::Ready => {
                tx.send(RouteBootstrapOutcome::Ready)
                    .expect("ready receiver retained");
                None
            }
            BootstrapOutcome::CompanionFailure => {
                tx.send(RouteBootstrapOutcome::RouteSpecific(
                    RouteDegradedReason::CompanionInitializationFailed,
                ))
                .expect("failure receiver retained");
                None
            }
        };
        (
            Arc::new(Self {
                outcomes: Mutex::new(VecDeque::from([rx])),
                starts: AtomicUsize::new(0),
            }),
            pending,
        )
    }
}

#[async_trait::async_trait]
impl SharedSpawnDriver for ControlledSpawnDriver {
    async fn start(
        &self,
        connection_id: String,
        launch: SharedConnectLaunch,
        existing_public_state: Option<Arc<RwLock<SessionState>>>,
    ) -> Result<RegisteredSpawnAttempt, AcpError> {
        let attempt = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        let outcome = self
            .outcomes
            .lock()
            .expect("spawn outcomes lock")
            .pop_front()
            .expect("one bootstrap outcome per spawn");
        Ok(
            codeg_lib::web::handlers::acp::registered_shared_spawn_attempt_for_http_test(
                connection_id,
                format!("shared-http-incarnation-{attempt}"),
                launch,
                existing_public_state,
                outcome,
            )
            .await,
        )
    }
}

#[derive(Clone, Copy)]
enum BootstrapOutcome {
    Pending,
    Ready,
    CompanionFailure,
}

struct SharedHttpFixture {
    server: TestServer,
    state: Arc<AppState>,
    driver: Arc<ControlledSpawnDriver>,
    conversation_id: i32,
    folder_id: i32,
    working_dir: String,
    _bootstrap: Option<oneshot::Sender<RouteBootstrapOutcome>>,
    _data_dir: tempfile::TempDir,
    _static_dir: tempfile::TempDir,
    _workspace: tempfile::TempDir,
}

impl SharedHttpFixture {
    fn connect_json(&self, device_id: &str, client_instance_id: &str, request_id: &str) -> Value {
        json!({
            "conversationId": self.conversation_id,
            "agentType": "codex",
            "workingDir": self.working_dir,
            "externalSessionId": null,
            "delegationRouteOverride": null,
            "preferredModeId": null,
            "preferredConfigValues": {},
            "deviceId": device_id,
            "clientInstanceId": client_instance_id,
            "requestId": request_id,
            "retryFailedGeneration": null,
        })
    }

    async fn post_connect(
        &self,
        device_id: &str,
        client_instance_id: &str,
        request_id: &str,
    ) -> HttpResponse {
        self.post_json(
            "/acp_connect_or_attach",
            self.connect_json(device_id, client_instance_id, request_id),
        )
        .await
    }

    async fn post_json(&self, route: &str, body: Value) -> HttpResponse {
        self.post_json_with_token(route, body, Some(TEST_TOKEN))
            .await
    }

    async fn post_json_with_token(
        &self,
        route: &str,
        body: Value,
        token: Option<&str>,
    ) -> HttpResponse {
        let mut request = self.server.post(&format!("/api{route}")).json(&body);
        if let Some(token) = token {
            request = request.add_header("authorization", format!("Bearer {token}"));
        }
        let response = request.await;
        let status = response.status_code();
        let text = response.text();
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        HttpResponse { status, body }
    }

    fn spawn_count(&self) -> usize {
        self.driver.starts.load(Ordering::SeqCst)
    }

    fn manager(&self) -> &ConnectionManager {
        &self.state.connection_manager
    }
}

async fn shared_http_fixture(outcome: BootstrapOutcome) -> SharedHttpFixture {
    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let working_dir = workspace.path().to_string_lossy().into_owned();
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, &working_dir).await;
    let conversation_id = seed_conversation(&db, folder_id, AgentType::Codex).await;
    let (driver, bootstrap) = ControlledSpawnDriver::new(outcome);
    let manager = ConnectionManager::new_with_shared_spawn_driver(driver.clone());
    let mut app_state = AppState::new_for_test(db, data_dir.path().to_path_buf());
    app_state.connection_manager = manager;
    let state = Arc::new(app_state);
    let router = build_router(
        Arc::clone(&state),
        TEST_TOKEN.into(),
        static_dir.path().to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    );
    let server = TestServer::new(router).expect("shared HTTP test server");
    SharedHttpFixture {
        server,
        state,
        driver,
        conversation_id,
        folder_id,
        working_dir,
        _bootstrap: bootstrap,
        _data_dir: data_dir,
        _static_dir: static_dir,
        _workspace: workspace,
    }
}

async fn shared_http_fixture_with_pending_bootstrap() -> SharedHttpFixture {
    shared_http_fixture(BootstrapOutcome::Pending).await
}

async fn ready_shared_http_fixture() -> SharedHttpFixture {
    shared_http_fixture(BootstrapOutcome::Ready).await
}

#[tokio::test]
async fn concurrent_connect_or_attach_returns_one_connection_and_distinct_leases() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let (a, b) = tokio::join!(
        fixture.post_connect("device-a", "client-a", "request-a"),
        fixture.post_connect("device-b", "client-b", "request-b"),
    );
    let a = a.assert_status_ok().json::<AcpConnectOrAttachResponse>();
    let b = b.assert_status_ok().json::<AcpConnectOrAttachResponse>();
    assert_eq!(a.connection_id, b.connection_id);
    assert_eq!(a.generation, b.generation);
    assert_ne!(a.lease_id, b.lease_id);
    assert_eq!(fixture.spawn_count(), 1);
    assert_eq!(a.phase, SharedPublicPhase::Bootstrapping);
    assert_eq!(b.phase, SharedPublicPhase::Bootstrapping);
    assert_eq!(a.disposition, SharedDisposition::Created);
    assert_eq!(b.disposition, SharedDisposition::Attached);
    assert!(!a.lease_expires_at.is_empty());
    assert_eq!(a.error, None);
}

#[tokio::test]
async fn persisted_external_and_ephemeral_connect_retries_reuse_the_same_lease() {
    for kind in ["persisted", "external", "ephemeral"] {
        let fixture = shared_http_fixture_with_pending_bootstrap().await;
        let mut body = fixture.connect_json("retry-device", "retry-client", "retry-request");
        match kind {
            "persisted" => {}
            "external" => {
                body["conversationId"] = Value::Null;
                body["externalSessionId"] = json!("external-session-a");
            }
            "ephemeral" => {
                body["conversationId"] = Value::Null;
                body["workingDir"] = Value::Null;
            }
            _ => unreachable!(),
        }
        let first = fixture
            .post_json("/acp_connect_or_attach", body.clone())
            .await
            .assert_status_ok()
            .json::<AcpConnectOrAttachResponse>();
        let retry = fixture
            .post_json("/acp_connect_or_attach", body)
            .await
            .assert_status_ok()
            .json::<AcpConnectOrAttachResponse>();
        assert_eq!(first.connection_id, retry.connection_id, "kind={kind}");
        assert_eq!(first.generation, retry.generation, "kind={kind}");
        assert_eq!(first.lease_id, retry.lease_id, "kind={kind}");
        assert_eq!(fixture.spawn_count(), 1, "kind={kind}");
    }
}

#[tokio::test]
async fn invalid_identity_fields_reject_before_reservation_or_spawn() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let mut wrong_agent = fixture.connect_json("device", "client", "wrong-agent");
    wrong_agent["agentType"] = json!("claude_code");
    fixture
        .post_json("/acp_connect_or_attach", wrong_agent)
        .await
        .assert_status_bad_request();

    let mut wrong_folder = fixture.connect_json("device", "client", "wrong-folder");
    wrong_folder["workingDir"] = json!("/definitely/not/the/persisted/folder");
    fixture
        .post_json("/acp_connect_or_attach", wrong_folder)
        .await
        .assert_status_bad_request();

    codeg_lib::db::service::conversation_service::update_external_id(
        &fixture.state.db.conn,
        fixture.conversation_id,
        "persisted-external".into(),
    )
    .await
    .expect("persist conversation external id");
    let mut wrong_external = fixture.connect_json("device", "client", "wrong-external");
    wrong_external["externalSessionId"] = json!("different-external");
    fixture
        .post_json("/acp_connect_or_attach", wrong_external)
        .await
        .assert_status_bad_request();

    let mut missing_conversation = fixture.connect_json("device", "client", "missing-row");
    missing_conversation["conversationId"] = json!(i32::MAX);
    fixture
        .post_json("/acp_connect_or_attach", missing_conversation)
        .await
        .assert_status_bad_request();

    for (field, value) in [
        ("deviceId", "bad label"),
        ("clientInstanceId", "bad/label"),
        ("requestId", ""),
        ("requestId", &"x".repeat(MAX_CLIENT_LABEL_LEN + 1)),
    ] {
        let mut invalid = fixture.connect_json("device", "client", "request");
        invalid[field] = json!(value);
        fixture
            .post_json("/acp_connect_or_attach", invalid)
            .await
            .assert_status_bad_request()
            .assert_code("invalid_shared_session_field");
    }

    assert_eq!(fixture.spawn_count(), 0);
    let metrics = fixture
        .manager()
        .shared_session_broker()
        .metrics()
        .snapshot();
    assert_eq!(metrics.live_sessions, 0);
    assert_eq!(metrics.active_leases, 0);
}

#[tokio::test]
async fn protected_shared_routes_require_the_bearer_token() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    fixture
        .post_json_with_token(
            "/acp_connect_or_attach",
            fixture.connect_json("device", "client", "request"),
            None,
        )
        .await
        .assert_status_unauthorized();
    assert_eq!(fixture.spawn_count(), 0);
}

#[tokio::test]
async fn direct_legacy_web_connect_requires_the_shared_protocol_without_spawning() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    fixture
        .post_json(
            "/acp_connect",
            json!({
                "agentType": "codex",
                "workingDir": fixture.working_dir,
                "sessionId": null,
                "conversationId": fixture.conversation_id,
            }),
        )
        .await
        .assert_status_conflict()
        .assert_code("shared_session_protocol_required");
    assert_eq!(fixture.spawn_count(), 0);
}

#[tokio::test]
async fn release_only_drops_the_lease_and_never_disconnects_the_shared_root() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let attached = fixture
        .post_connect("release-device", "release-client", "release-request")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .post_json(
            "/acp_release_lease",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
            }),
        )
        .await
        .assert_status_ok();
    assert!(fixture
        .manager()
        .get_state(&attached.connection_id)
        .await
        .is_some());
    assert_eq!(fixture.manager().shared_teardown_count_for_test(), 0);
}

#[tokio::test]
async fn shared_mutations_distinguish_missing_and_recently_expired_leases() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    fixture
        .manager()
        .configure_shared_client_lease_ttl(Duration::from_millis(1));
    let attached = fixture
        .post_connect("lease-device", "lease-client", "lease-request")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();

    let prompt = |lease_id: &str| {
        json!({
            "connectionId": attached.connection_id,
            "generation": attached.generation,
            "leaseId": lease_id,
            "clientInstanceId": "lease-client",
            "clientRequestId": "prompt-request",
            "clientMessageId": "message-id",
            "blocks": [{"type": "text", "text": "hello"}],
            "folderId": fixture.folder_id,
            "conversationId": fixture.conversation_id,
        })
    };
    fixture
        .post_json("/acp_prompt", prompt("unknown-lease"))
        .await
        .assert_status_conflict()
        .assert_code("client_lease_missing");

    tokio::time::sleep(Duration::from_millis(5)).await;
    fixture
        .manager()
        .shared_session_broker()
        .expire_leases(tokio::time::Instant::now())
        .await;
    fixture
        .post_json("/acp_prompt", prompt(&attached.lease_id))
        .await
        .assert_status_gone()
        .assert_code("client_lease_expired");
}

#[tokio::test]
async fn queued_prompt_can_be_cancelled_by_another_valid_lease() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let first = fixture
        .post_connect("queue-device-a", "queue-client-a", "queue-connect-a")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let second = fixture
        .post_connect("queue-device-b", "queue-client-b", "queue-connect-b")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let enqueue = fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": first.connection_id,
                "generation": first.generation,
                "leaseId": first.lease_id,
                "clientInstanceId": "queue-client-a",
                "clientRequestId": "queue-prompt-a",
                "clientMessageId": "queue-message-a",
                "blocks": [{"type": "text", "text": "queued"}],
                "folderId": fixture.folder_id,
                "conversationId": fixture.conversation_id,
            }),
        )
        .await
        .assert_status_ok();
    assert_eq!(enqueue.body["state"], "queued");
    let queue_item_id = enqueue.body["queueItemId"]
        .as_str()
        .expect("queue item id")
        .to_string();
    fixture
        .post_json(
            "/acp_cancel_queued_prompt",
            json!({
                "connectionId": second.connection_id,
                "generation": second.generation,
                "leaseId": second.lease_id,
                "queueItemId": queue_item_id,
            }),
        )
        .await
        .assert_status_ok();
    assert_eq!(
        fixture
            .manager()
            .shared_session_broker()
            .metrics()
            .snapshot()
            .waiting_prompts,
        0
    );
}

#[tokio::test]
async fn persisted_shared_root_rejects_prompt_without_its_canonical_conversation() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let attached = fixture
        .post_connect("canonical-device", "canonical-client", "canonical-connect")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "clientInstanceId": "canonical-client",
                "clientRequestId": "canonical-prompt",
                "clientMessageId": "canonical-message",
                "blocks": [{"type": "text", "text": "cannot retarget"}],
                "folderId": null,
                "conversationId": null,
            }),
        )
        .await
        .assert_status_conflict()
        .assert_code("shared_session_conversation_key_conflict");
    assert_eq!(
        fixture
            .manager()
            .shared_session_broker()
            .metrics()
            .snapshot()
            .waiting_prompts,
        0
    );
}

#[tokio::test]
async fn external_and_ephemeral_roots_bind_to_one_conversation_only() {
    for kind in ["external", "ephemeral"] {
        let fixture = shared_http_fixture_with_pending_bootstrap().await;
        let mut connect = fixture.connect_json("bind-device", "bind-client", "bind-connect");
        connect["conversationId"] = Value::Null;
        if kind == "external" {
            connect["externalSessionId"] = json!("bind-external-session");
        } else {
            connect["workingDir"] = Value::Null;
        }
        let attached = fixture
            .post_json("/acp_connect_or_attach", connect)
            .await
            .assert_status_ok()
            .json::<AcpConnectOrAttachResponse>();
        let prompt = |conversation_id: i32, request_id: &str| {
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "clientInstanceId": "bind-client",
                "clientRequestId": request_id,
                "clientMessageId": format!("message-{request_id}"),
                "blocks": [{"type": "text", "text": "bind"}],
                "folderId": fixture.folder_id,
                "conversationId": conversation_id,
            })
        };
        fixture
            .post_json("/acp_prompt", prompt(fixture.conversation_id, "bind-first"))
            .await
            .assert_status_ok();
        let other_conversation =
            seed_conversation(&fixture.state.db, fixture.folder_id, AgentType::Codex).await;
        fixture
            .post_json("/acp_prompt", prompt(other_conversation, "bind-second"))
            .await
            .assert_status_conflict()
            .assert_code("shared_session_conversation_key_conflict");
        assert_eq!(
            fixture
                .manager()
                .get_state(&attached.connection_id)
                .await
                .expect("shared public state")
                .read()
                .await
                .conversation_id,
            Some(fixture.conversation_id),
            "kind={kind}"
        );
    }
}

#[tokio::test]
async fn unbound_shared_root_rejects_prompt_without_a_folder_before_enqueue() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let mut connect = fixture.connect_json("draft-device", "draft-client", "draft-connect");
    connect["conversationId"] = Value::Null;
    connect["workingDir"] = Value::Null;
    let attached = fixture
        .post_json("/acp_connect_or_attach", connect)
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "clientInstanceId": "draft-client",
                "clientRequestId": "draft-prompt",
                "clientMessageId": "draft-message",
                "blocks": [{"type": "text", "text": "missing folder"}],
                "folderId": null,
                "conversationId": null,
            }),
        )
        .await
        .assert_status_bad_request()
        .assert_code("invalid_shared_session_field");
    assert_eq!(
        fixture
            .manager()
            .shared_session_broker()
            .metrics()
            .snapshot()
            .waiting_prompts,
        0
    );
}

#[tokio::test]
async fn rejected_empty_prompt_does_not_bind_an_unbound_shared_root() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let mut connect = fixture.connect_json("empty-device", "empty-client", "empty-connect");
    connect["conversationId"] = Value::Null;
    connect["workingDir"] = Value::Null;
    let attached = fixture
        .post_json("/acp_connect_or_attach", connect)
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "clientInstanceId": "empty-client",
                "clientRequestId": "empty-prompt",
                "clientMessageId": "empty-message",
                "blocks": [],
                "folderId": fixture.folder_id,
                "conversationId": fixture.conversation_id,
            }),
        )
        .await
        .assert_status_bad_request()
        .assert_code("invalid_shared_session_field");
    assert_eq!(
        fixture
            .manager()
            .get_state(&attached.connection_id)
            .await
            .expect("shared public state")
            .read()
            .await
            .conversation_id,
        None
    );
    assert_eq!(
        fixture
            .manager()
            .shared_session_broker()
            .metrics()
            .snapshot()
            .waiting_prompts,
        0
    );
}

#[tokio::test]
async fn prompt_queue_capacity_rejects_only_new_prompt_identities() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let attached = fixture
        .post_connect(
            "prompt-cap-device",
            "prompt-cap-client",
            "prompt-cap-connect",
        )
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let prompt = |request_id: &str| {
        json!({
            "connectionId": attached.connection_id,
            "generation": attached.generation,
            "leaseId": attached.lease_id,
            "clientInstanceId": "prompt-cap-client",
            "clientRequestId": request_id,
            "clientMessageId": format!("message-{request_id}"),
            "blocks": [{"type": "text", "text": request_id}],
            "folderId": fixture.folder_id,
            "conversationId": fixture.conversation_id,
        })
    };
    let first_request = prompt("request-0");
    let first = fixture
        .post_json("/acp_prompt", first_request.clone())
        .await
        .assert_status_ok()
        .body;
    for index in 1..MAX_WAITING_PROMPTS {
        fixture
            .post_json("/acp_prompt", prompt(&format!("request-{index}")))
            .await
            .assert_status_ok();
    }
    fixture
        .post_json("/acp_prompt", prompt("request-over-capacity"))
        .await
        .assert_status_too_many_requests()
        .assert_code("prompt_queue_full");
    let retry = fixture
        .post_json("/acp_prompt", first_request)
        .await
        .assert_status_ok()
        .body;
    assert_eq!(retry, first);
}

#[tokio::test]
async fn lease_capacity_rejects_only_new_client_identities() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let first_request = fixture.connect_json(
        "lease-cap-device-0",
        "lease-cap-client-0",
        "lease-cap-request-0",
    );
    let first = fixture
        .post_json("/acp_connect_or_attach", first_request.clone())
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    for index in 1..MAX_ACTIVE_LEASES {
        fixture
            .post_connect(
                &format!("lease-cap-device-{index}"),
                &format!("lease-cap-client-{index}"),
                &format!("lease-cap-request-{index}"),
            )
            .await
            .assert_status_ok();
    }
    fixture
        .post_connect(
            "lease-cap-device-over",
            "lease-cap-client-over",
            "lease-cap-request-over",
        )
        .await
        .assert_status_too_many_requests()
        .assert_code("client_lease_capacity_exceeded");
    let retry = fixture
        .post_json("/acp_connect_or_attach", first_request)
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    assert_eq!(retry.connection_id, first.connection_id);
    assert_eq!(retry.lease_id, first.lease_id);
}

#[tokio::test]
async fn connect_ledger_capacity_rejects_only_new_request_identities() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let first_request = fixture.connect_json(
        "connect-cap-device",
        "connect-cap-client",
        "connect-cap-request-0",
    );
    let first = fixture
        .post_json("/acp_connect_or_attach", first_request.clone())
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    for index in 1..MAX_CONNECT_LEDGER_ENTRIES {
        fixture
            .post_connect(
                "connect-cap-device",
                "connect-cap-client",
                &format!("connect-cap-request-{index}"),
            )
            .await
            .assert_status_ok();
    }
    fixture
        .post_connect(
            "connect-cap-device",
            "connect-cap-client",
            "connect-cap-request-over",
        )
        .await
        .assert_status_too_many_requests()
        .assert_code("connect_idempotency_capacity_exceeded");
    let retry = fixture
        .post_json("/acp_connect_or_attach", first_request)
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    assert_eq!(retry.connection_id, first.connection_id);
    assert_eq!(retry.lease_id, first.lease_id);
}

#[tokio::test]
async fn legacy_prompt_and_disconnect_cannot_mutate_shared_root() {
    let fixture = ready_shared_http_fixture().await;
    let attached = fixture
        .post_connect("d", "c", "r")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": attached.connection_id,
                "blocks": [{"type":"text","text":"x"}],
            }),
        )
        .await
        .assert_status_conflict()
        .assert_code("shared_session_protocol_required");
    fixture
        .post_json(
            "/acp_disconnect",
            json!({
                "connectionId": attached.connection_id,
                "origin":"provider_unmount",
            }),
        )
        .await
        .assert_status_conflict();
    assert!(fixture
        .manager()
        .get_state(&attached.connection_id)
        .await
        .is_some());
}

#[tokio::test]
async fn shared_stop_and_interaction_mutations_require_generation_and_lease() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let attached = fixture
        .post_connect("control-device", "control-client", "control-connect")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    for (route, body) in [
        (
            "/acp_cancel",
            json!({"connectionId": attached.connection_id, "turnId": "turn"}),
        ),
        (
            "/acp_respond_permission",
            json!({
                "connectionId": attached.connection_id,
                "requestId": "permission",
                "optionId": "allow",
            }),
        ),
        (
            "/acp_answer_question",
            json!({
                "connectionId": attached.connection_id,
                "questionId": "question",
                "answer": {"answers": [], "declined": false},
            }),
        ),
        (
            "/acp_answer_plan_approval",
            json!({
                "connectionId": attached.connection_id,
                "approvalId": "approval",
                "answer": {"decision": "approve", "feedback": null},
            }),
        ),
    ] {
        fixture
            .post_json(route, body)
            .await
            .assert_status_conflict()
            .assert_code("shared_session_protocol_required");
    }
}

#[tokio::test]
async fn explicit_termination_requires_auth_and_the_current_generation() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let attached = fixture
        .post_connect("terminate-device", "terminate-client", "terminate-request")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let request = json!({
        "connectionId": attached.connection_id,
        "generation": attached.generation,
    });
    fixture
        .post_json_with_token("/acp_terminate_shared_session", request.clone(), None)
        .await
        .assert_status_unauthorized();
    let mut stale = request.clone();
    stale["generation"] = json!(attached.generation + 1);
    fixture
        .post_json("/acp_terminate_shared_session", stale)
        .await
        .assert_status_conflict()
        .assert_code("shared_session_generation_stale");
    fixture
        .post_json("/acp_terminate_shared_session", request)
        .await
        .assert_status_ok();
    assert!(fixture
        .manager()
        .get_state(&attached.connection_id)
        .await
        .is_none());
    assert_eq!(fixture.manager().shared_teardown_count_for_test(), 1);
}

#[tokio::test]
async fn required_companion_failure_is_typed_and_secret_safe() {
    let fixture = shared_http_fixture(BootstrapOutcome::CompanionFailure).await;
    let response = fixture
        .post_connect("failed-device", "failed-client", "failed-request")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    assert_eq!(response.phase, SharedPublicPhase::Failed);
    assert_eq!(
        response.error,
        Some(SharedConnectFailure {
            code: "companion_initialization_failed".into(),
            retryable: true,
            cleanup_complete: true,
        })
    );
}

#[test]
fn production_shared_session_limits_remain_bounded() {
    assert_eq!(MAX_WAITING_PROMPTS, 64);
    assert_eq!(MAX_WAITING_BYTES, 32 * 1024 * 1024);
    assert_eq!(MAX_ACTIVE_LEASES, 256);
    assert_eq!(MAX_CONNECT_LEDGER_ENTRIES, 4_096);
    assert_eq!(MAX_PROMPT_LEDGER_ENTRIES, 65_536);
    assert_eq!(MAX_EXPIRED_LEASE_TOMBSTONES, 1_024);
    assert_eq!(MAX_REPLACED_CONNECTION_TOMBSTONES, 4_096);
    assert_eq!(MAX_CLIENT_LABEL_LEN, 128);
}

#[test]
fn shared_session_phase_snapshot_remains_tagged_separately_from_public_phase() {
    assert_eq!(
        serde_json::to_value(SharedSessionPhase::Bootstrapping).unwrap(),
        json!({"phase": "bootstrapping"})
    );
}
