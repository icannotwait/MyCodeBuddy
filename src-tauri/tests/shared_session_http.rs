//! Shared ACP session HTTP contracts exercised through the real Axum router.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use axum_test::TestServer;
use codeg_lib::acp::connection::{
    ConnectionCommand, ConnectionControl, RegisteredSpawnAttempt, RouteBootstrapOutcome,
};
use codeg_lib::acp::delegation::route::RouteDegradedReason;
use codeg_lib::acp::error::AcpError;
use codeg_lib::acp::manager::{ConnectionManager, SharedConnectLaunch, SharedSpawnDriver};
use codeg_lib::acp::question::QuestionSpec;
use codeg_lib::acp::session_state::SessionState;
use codeg_lib::acp::shared_session::{
    SharedDisposition, SharedSessionKey, SharedSessionPhase, MAX_ACTIVE_LEASES,
    MAX_CLIENT_LABEL_LEN, MAX_CONNECT_LEDGER_ENTRIES, MAX_EXPIRED_LEASE_TOMBSTONES,
    MAX_PROMPT_LEDGER_ENTRIES, MAX_REPLACED_CONNECTION_TOMBSTONES, MAX_WAITING_BYTES,
    MAX_WAITING_PROMPTS,
};
use codeg_lib::acp::termination::AcpDisconnectOrigin;
use codeg_lib::acp::types::AcpEvent;
use codeg_lib::acp::types::PermissionOptionInfo;
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

    fn assert_status_service_unavailable(self) -> Self {
        self.assert_status(StatusCode::SERVICE_UNAVAILABLE)
    }

    fn assert_code(self, code: &str) -> Self {
        assert_eq!(self.body.get("code").and_then(Value::as_str), Some(code));
        self
    }

    fn json<T: DeserializeOwned>(self) -> T {
        serde_json::from_value(self.body).expect("typed response body")
    }
}

fn assert_one_shared_control_winner(interaction: &str, left: HttpResponse, right: HttpResponse) {
    let responses = [left, right];
    assert_eq!(
        responses
            .iter()
            .filter(|response| response.status == StatusCode::OK)
            .count(),
        1,
        "exactly one {interaction} request must win: {responses:?}"
    );
    let loser = responses
        .iter()
        .find(|response| response.status != StatusCode::OK)
        .expect("one losing response");
    assert_eq!(loser.status, StatusCode::CONFLICT, "loser: {loser:?}");
    assert_eq!(
        loser.body.get("code").and_then(Value::as_str),
        Some("interaction_already_resolved")
    );
}

struct ControlledSpawnDriver {
    outcomes: Mutex<VecDeque<oneshot::Receiver<RouteBootstrapOutcome>>>,
    starts: AtomicUsize,
    agent_stderr: Option<String>,
}

impl ControlledSpawnDriver {
    fn new_with_stderr(
        outcome: BootstrapOutcome,
        agent_stderr: Option<String>,
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
                agent_stderr,
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
                self.agent_stderr.clone(),
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
    bootstrap: Mutex<Option<oneshot::Sender<RouteBootstrapOutcome>>>,
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

    async fn get_json_with_token(&self, route: &str, token: Option<&str>) -> HttpResponse {
        let mut request = self.server.get(&format!("/api{route}"));
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

    fn release_bootstrap_ready(&self) {
        self.bootstrap
            .lock()
            .expect("bootstrap gate lock")
            .take()
            .expect("pending bootstrap gate")
            .send(RouteBootstrapOutcome::Ready)
            .expect("bootstrap receiver retained");
    }

    async fn wait_for_failed_cleanup(&self, connection_id: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let phase = self
                    .manager()
                    .shared_session_broker()
                    .diagnostic_for_connection(connection_id)
                    .await
                    .map(|snapshot| snapshot.phase);
                if matches!(
                    phase,
                    Some(SharedSessionPhase::Failed {
                        cleanup_complete: true,
                        ..
                    })
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("failed shared root should complete cleanup");
    }

    async fn wait_for_pending_interaction(&self, connection_id: &str, interaction_id: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if self
                    .manager()
                    .has_pending_test_shared_interaction(connection_id, interaction_id)
                    .await
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shared runtime monitor observes pending interaction");
    }
}

async fn shared_http_fixture_with_prompt_ledger_limit(
    outcome: BootstrapOutcome,
    prompt_ledger_limit: Option<usize>,
) -> SharedHttpFixture {
    shared_http_fixture_with_options(outcome, prompt_ledger_limit, None).await
}

async fn shared_http_fixture_with_options(
    outcome: BootstrapOutcome,
    prompt_ledger_limit: Option<usize>,
    agent_stderr: Option<String>,
) -> SharedHttpFixture {
    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let working_dir = workspace.path().to_string_lossy().into_owned();
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, &working_dir).await;
    let conversation_id = seed_conversation(&db, folder_id, AgentType::Codex).await;
    let (driver, bootstrap) = ControlledSpawnDriver::new_with_stderr(outcome, agent_stderr);
    let manager = match prompt_ledger_limit {
        Some(limit) => {
            ConnectionManager::new_with_shared_spawn_driver_and_prompt_ledger_limit_for_test(
                driver.clone(),
                limit,
            )
        }
        None => ConnectionManager::new_with_shared_spawn_driver(driver.clone()),
    };
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
        bootstrap: Mutex::new(bootstrap),
        _data_dir: data_dir,
        _static_dir: static_dir,
        _workspace: workspace,
    }
}

async fn shared_http_fixture(outcome: BootstrapOutcome) -> SharedHttpFixture {
    shared_http_fixture_with_prompt_ledger_limit(outcome, None).await
}

async fn shared_http_fixture_with_pending_bootstrap() -> SharedHttpFixture {
    shared_http_fixture(BootstrapOutcome::Pending).await
}

async fn ready_shared_http_fixture() -> SharedHttpFixture {
    shared_http_fixture(BootstrapOutcome::Ready).await
}

fn assert_json_omits_secrets(value: &Value, forbidden_keys: &[&str], sentinels: &[&str]) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                assert!(
                    !forbidden_keys.contains(&key.as_str()),
                    "forbidden diagnostic key {key:?} in {fields:?}"
                );
                assert_json_omits_secrets(value, forbidden_keys, sentinels);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_json_omits_secrets(item, forbidden_keys, sentinels);
            }
        }
        Value::String(text) => {
            for sentinel in sentinels {
                assert!(
                    !text.contains(sentinel),
                    "diagnostic string reflected sentinel {sentinel:?}: {text:?}"
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[tokio::test]
async fn concurrent_connect_or_attach_returns_one_connection_and_distinct_leases() {
    for client_count in [2_usize, 10, 100] {
        let fixture = shared_http_fixture_with_pending_bootstrap().await;
        let fixture_ref = &fixture;
        let responses = futures::future::join_all((0..client_count).map(move |index| async move {
            let device_id = format!("device-{client_count}-{index}");
            let client_id = format!("client-{client_count}-{index}");
            let request_id = format!("request-{client_count}-{index}");
            fixture_ref
                .post_connect(&device_id, &client_id, &request_id)
                .await
        }))
        .await
        .into_iter()
        .map(|response| {
            response
                .assert_status_ok()
                .json::<AcpConnectOrAttachResponse>()
        })
        .collect::<Vec<_>>();
        let first = &responses[0];
        assert!(responses.iter().all(|response| {
            response.connection_id == first.connection_id
                && response.generation == first.generation
                && response.phase == SharedPublicPhase::Bootstrapping
                && response.error.is_none()
                && !response.lease_expires_at.is_empty()
        }));
        assert_eq!(
            responses
                .iter()
                .map(|response| response.lease_id.as_str())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            client_count
        );
        assert_eq!(
            responses
                .iter()
                .filter(|response| response.disposition == SharedDisposition::Created)
                .count(),
            1
        );
        assert_eq!(fixture.spawn_count(), 1);
    }
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

    for (conversation_id, request_id) in [(0, "zero-row"), (-1, "negative-row")] {
        let mut invalid = fixture.connect_json("device", "client", request_id);
        invalid["conversationId"] = json!(conversation_id);
        fixture
            .post_json("/acp_connect_or_attach", invalid)
            .await
            .assert_status_bad_request()
            .assert_code("invalid_shared_session_field");
    }

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
async fn debug_metrics_and_diagnostics_are_authenticated_complete_and_secret_safe() {
    const DEVICE_SENTINEL: &str = "task12-device-secret-sentinel";
    const CLIENT_SENTINEL: &str = "task12-client-secret-sentinel";
    const REQUEST_SENTINEL: &str = "task12-request-secret-sentinel";
    const PROMPT_SENTINEL: &str = "task12-prompt-secret-sentinel";
    const ANSWER_SENTINEL: &str = "task12-answer-secret-sentinel";
    const ENV_SENTINEL: &str = "task12-environment-secret-sentinel";
    const STDERR_SENTINEL: &str = "task12-stderr-secret-sentinel";

    let fixture = shared_http_fixture_with_options(
        BootstrapOutcome::Pending,
        None,
        Some(STDERR_SENTINEL.into()),
    )
    .await;
    let mut connect = fixture.connect_json(DEVICE_SENTINEL, CLIENT_SENTINEL, REQUEST_SENTINEL);
    connect["preferredConfigValues"] = json!({"TASK12_ENV": ENV_SENTINEL});
    let attached = fixture
        .post_json("/acp_connect_or_attach", connect)
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let _lanes = fixture
        .manager()
        .install_test_shared_connection_lanes(&attached.connection_id)
        .await
        .expect("controllable shared connection lanes");
    assert_eq!(
        fixture
            .manager()
            .get_state(&attached.connection_id)
            .await
            .expect("driver state")
            .read()
            .await
            .last_error
            .as_ref()
            .map(|error| error.message.as_str()),
        Some(STDERR_SENTINEL),
        "fake driver must seed the stderr sentinel into retained driver state"
    );
    fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "clientInstanceId": CLIENT_SENTINEL,
                "clientRequestId": "task12-prompt-request",
                "clientMessageId": "task12-prompt-message",
                "blocks": [{"type": "text", "text": PROMPT_SENTINEL}],
                "folderId": fixture.folder_id,
                "conversationId": fixture.conversation_id,
            }),
        )
        .await
        .assert_status_ok();
    let registered_question = fixture
        .manager()
        .register_question(
            &attached.connection_id,
            vec![QuestionSpec {
                id: "task12-answer-question".into(),
                question: "Provide the diagnostic exclusion sentinel".into(),
                header: "Secret".into(),
                multi_select: false,
                options: Vec::new(),
                is_secret: true,
                recovery: None,
            }],
        )
        .await
        .expect("real pending question");
    fixture
        .wait_for_pending_interaction(&attached.connection_id, &registered_question.question_id)
        .await;
    fixture
        .post_json(
            "/acp_answer_question",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "questionId": registered_question.question_id,
                "answer": {
                    "answers": [{
                        "questionId": "task12-answer-question",
                        "labels": [ANSWER_SENTINEL],
                    }],
                    "declined": false,
                },
            }),
        )
        .await
        .assert_status_ok();
    let answer_outcome = registered_question
        .answer_rx
        .await
        .expect("real pending question receives the HTTP answer");
    assert!(serde_json::to_string(&answer_outcome)
        .unwrap()
        .contains(ANSWER_SENTINEL));

    for route in ["/debug/event_metrics", "/debug/shared_sessions"] {
        fixture
            .get_json_with_token(route, None)
            .await
            .assert_status_unauthorized();
    }

    let metrics = fixture
        .get_json_with_token("/debug/event_metrics", Some(TEST_TOKEN))
        .await
        .assert_status_ok()
        .body;
    assert!(metrics.get("emitted_count").is_some());
    let broker_metrics = metrics
        .get("shared_session_broker")
        .and_then(Value::as_object)
        .expect("event metrics include nested shared-session broker snapshot");
    for field in [
        "created_total",
        "attached_total",
        "live_sessions",
        "active_leases",
        "bootstrap_ready_total",
        "bootstrap_failed_total",
        "bootstrap_duration_ms_total",
        "bootstrap_duration_samples",
        "waiting_prompts",
        "waiting_bytes",
        "enqueue_total",
        "cancel_total",
        "dispatch_total",
        "capacity_rejected_total",
        "queue_item_failed_total",
        "interaction_winner_total",
        "interaction_stale_total",
        "stale_stop_total",
        "lease_expired_total",
        "lease_released_total",
        "idle_candidate_total",
        "idle_cas_lost_total",
        "idle_reclaimed_total",
        "cleanup_duration_ms_total",
        "cleanup_duration_samples",
        "cleanup_incomplete_total",
    ] {
        assert!(broker_metrics.contains_key(field), "missing metric {field}");
    }

    let diagnostics = fixture
        .get_json_with_token("/debug/shared_sessions", Some(TEST_TOKEN))
        .await
        .assert_status_ok()
        .body;
    let sessions = diagnostics.as_array().expect("diagnostic list");
    assert_eq!(sessions.len(), 1);
    let item = sessions[0].as_object().expect("diagnostic item");
    assert_eq!(
        item.get("connection_id"),
        Some(&json!(attached.connection_id))
    );
    assert_eq!(
        item.get("conversation_id"),
        Some(&json!(fixture.conversation_id))
    );
    assert_eq!(item.get("generation"), Some(&json!(attached.generation)));
    assert_eq!(item.get("agent_category"), Some(&json!("codex")));
    assert_eq!(item.get("lease_count"), Some(&json!(1)));
    assert_eq!(item.get("queue_depth"), Some(&json!(1)));
    assert!(item.get("queue_bytes").and_then(Value::as_u64).unwrap() > 0);
    assert!(item.get("idle_blockers").is_some());
    assert!(item.get("cleanup_state").is_some());
    assert!(item.get("bootstrap_duration_ms").is_some());
    assert!(item.get("cleanup_duration_ms").is_some());

    assert_json_omits_secrets(
        &diagnostics,
        &[
            "lease_id",
            "leaseId",
            "device_id",
            "deviceId",
            "client_instance_id",
            "clientInstanceId",
            "request_id",
            "requestId",
            "client_request_id",
            "clientRequestId",
            "prompt",
            "answer",
            "working_dir",
            "workingDir",
            "path",
            "token",
            "environment",
            "stderr",
            "raw_output",
            "launch_identity",
        ],
        &[
            &attached.lease_id,
            DEVICE_SENTINEL,
            CLIENT_SENTINEL,
            REQUEST_SENTINEL,
            PROMPT_SENTINEL,
            ANSWER_SENTINEL,
            &fixture.working_dir,
            TEST_TOKEN,
            ENV_SENTINEL,
            STDERR_SENTINEL,
        ],
    );
    assert_json_omits_secrets(&metrics, &[], &[TEST_TOKEN]);
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
async fn rejected_prompt_guards_and_identities_leave_unbound_roots_unbound() {
    for case in [
        "missing-lease",
        "expired-lease",
        "invalid-client-instance",
        "invalid-client-request",
        "empty-client-message",
    ] {
        let fixture = shared_http_fixture_with_pending_bootstrap().await;
        let mut connect = fixture.connect_json("guard-device", "guard-client", case);
        connect["conversationId"] = Value::Null;
        connect["workingDir"] = Value::Null;
        if case == "expired-lease" {
            fixture
                .manager()
                .configure_shared_client_lease_ttl(Duration::from_millis(1));
        }
        let attached = fixture
            .post_json("/acp_connect_or_attach", connect)
            .await
            .assert_status_ok()
            .json::<AcpConnectOrAttachResponse>();

        if case == "expired-lease" {
            tokio::time::sleep(Duration::from_millis(5)).await;
            fixture
                .manager()
                .shared_session_broker()
                .expire_leases(tokio::time::Instant::now())
                .await;
        }
        let mut prompt = json!({
            "connectionId": attached.connection_id,
            "generation": attached.generation,
            "leaseId": attached.lease_id,
            "clientInstanceId": "guard-client",
            "clientRequestId": "guard-prompt",
            "clientMessageId": "guard-message",
            "blocks": [{"type": "text", "text": "must not bind"}],
            "folderId": fixture.folder_id,
            "conversationId": fixture.conversation_id,
        });
        let (status, code) = match case {
            "missing-lease" => {
                prompt["leaseId"] = json!("unknown-lease");
                (StatusCode::CONFLICT, "client_lease_missing")
            }
            "expired-lease" => (StatusCode::GONE, "client_lease_expired"),
            "invalid-client-instance" => {
                prompt["clientInstanceId"] = json!("bad client");
                (StatusCode::BAD_REQUEST, "invalid_shared_session_field")
            }
            "invalid-client-request" => {
                prompt["clientRequestId"] = json!("bad/request");
                (StatusCode::BAD_REQUEST, "invalid_shared_session_field")
            }
            "empty-client-message" => {
                prompt["clientMessageId"] = json!("");
                (StatusCode::BAD_REQUEST, "invalid_shared_session_field")
            }
            _ => unreachable!(),
        };
        fixture
            .post_json("/acp_prompt", prompt)
            .await
            .assert_status(status)
            .assert_code(code);

        assert_eq!(
            fixture
                .manager()
                .get_state(&attached.connection_id)
                .await
                .expect("unbound root remains registered")
                .read()
                .await
                .conversation_id,
            None,
            "public state was bound for case={case}"
        );
        assert!(
            !matches!(
                fixture
                    .manager()
                    .shared_session_broker()
                    .key_for_connection_for_test(&attached.connection_id)
                    .await,
                Some(SharedSessionKey::Conversation(_))
            ),
            "broker key was bound for case={case}"
        );
    }
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
    let accepted = futures::future::join_all((0..MAX_WAITING_PROMPTS).map(|index| {
        let body = prompt(&format!("request-{index}"));
        fixture.post_json("/acp_prompt", body)
    }))
    .await;
    let first_seq = accepted[0]
        .body
        .get("enqueueSeq")
        .and_then(Value::as_u64)
        .expect("first request has enqueue sequence");
    let mut enqueue_seqs = accepted
        .into_iter()
        .map(|response| {
            response
                .assert_status_ok()
                .body
                .get("enqueueSeq")
                .and_then(Value::as_u64)
                .expect("accepted prompt has enqueue sequence")
        })
        .collect::<Vec<_>>();
    enqueue_seqs.sort_unstable();
    assert_eq!(
        enqueue_seqs,
        (1..=MAX_WAITING_PROMPTS as u64).collect::<Vec<_>>()
    );
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
    assert_eq!(
        retry.get("enqueueSeq").and_then(Value::as_u64),
        Some(first_seq)
    );
}

#[tokio::test]
async fn concurrent_prompts_dispatch_once_in_exact_enqueue_sequence_order() {
    let fixture = ready_shared_http_fixture().await;
    let attached = fixture
        .post_connect("dispatch-device", "dispatch-client", "dispatch-connect")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .manager()
        .wait_for_shared_phase(
            &attached.connection_id,
            attached.generation,
            SharedSessionPhase::Ready,
        )
        .await
        .unwrap();
    let (mut commands, _controls) = fixture
        .manager()
        .install_test_shared_connection_lanes(&attached.connection_id)
        .await
        .expect("controllable ready driver lanes");

    let responses = futures::future::join_all((0..MAX_WAITING_PROMPTS).map(|index| {
        let request_id = format!("dispatch-{index:02}");
        let text = format!("dispatch-text-{index:02}");
        fixture.post_json(
            "/acp_prompt",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "clientInstanceId": "dispatch-client",
                "clientRequestId": request_id,
                "clientMessageId": format!("message-{index:02}"),
                "blocks": [{"type": "text", "text": text}],
                "folderId": fixture.folder_id,
                "conversationId": fixture.conversation_id,
            }),
        )
    }))
    .await;
    let expected_by_sequence = responses
        .into_iter()
        .enumerate()
        .map(|(index, response)| {
            let sequence = response
                .assert_status_ok()
                .body
                .get("enqueueSeq")
                .and_then(Value::as_u64)
                .expect("accepted prompt sequence");
            (sequence, format!("dispatch-text-{index:02}"))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(expected_by_sequence.len(), MAX_WAITING_PROMPTS);

    let mut dispatched = Vec::new();
    for _ in 0..MAX_WAITING_PROMPTS {
        let command = tokio::time::timeout(Duration::from_secs(2), commands.recv())
            .await
            .expect("dispatcher must make progress")
            .expect("driver command lane remains open");
        let text = match command {
            ConnectionCommand::Prompt { blocks, .. } => match blocks.as_slice() {
                [codeg_lib::acp::types::PromptInputBlock::Text { text }] => text.clone(),
                other => panic!("unexpected dispatched blocks: {other:?}"),
            },
            _ => panic!("unexpected command on prompt lane"),
        };
        dispatched.push(text);
        assert!(
            fixture
                .manager()
                .emit_test_shared_driver_event(
                    &attached.connection_id,
                    AcpEvent::TurnComplete {
                        session_id: "dispatch-session".into(),
                        stop_reason: "end_turn".into(),
                        agent_type: "codex".into(),
                        mark_awaiting_reply: true,
                        termination_source: None,
                        provider_turn_id: None,
                    },
                )
                .await
        );
    }
    let expected = expected_by_sequence.into_values().collect::<Vec<_>>();
    assert_eq!(dispatched, expected);
    tokio::task::yield_now().await;
    assert!(matches!(
        commands.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    let metrics = fixture
        .manager()
        .shared_session_broker()
        .metrics()
        .snapshot();
    assert_eq!(metrics.dispatch_total, MAX_WAITING_PROMPTS as u64);
}

#[tokio::test]
async fn prompt_queue_enforces_the_actual_serialized_32_mib_boundary() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let attached = fixture
        .post_connect("byte-cap-device", "byte-cap-client", "byte-cap-connect")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let prompt = |request_id: &str, text: String| {
        json!({
            "connectionId": attached.connection_id,
            "generation": attached.generation,
            "leaseId": attached.lease_id,
            "clientInstanceId": "byte-cap-client",
            "clientRequestId": request_id,
            "clientMessageId": format!("message-{request_id}"),
            "blocks": [{"type": "text", "text": text}],
            "folderId": fixture.folder_id,
            "conversationId": fixture.conversation_id,
        })
    };

    fixture
        .post_json("/acp_prompt", prompt("bytes-a", String::new()))
        .await
        .assert_status_ok();
    let first_bytes = fixture
        .manager()
        .shared_session_diagnostics()
        .await
        .pop()
        .expect("queued diagnostic")
        .queue_bytes;
    assert!(first_bytes < MAX_WAITING_BYTES / 2);

    let exact_fill = "x".repeat(MAX_WAITING_BYTES - 2 * first_bytes);
    fixture
        .post_json("/acp_prompt", prompt("bytes-b", exact_fill))
        .await
        .assert_status_ok();
    let at_limit = fixture
        .manager()
        .shared_session_diagnostics()
        .await
        .pop()
        .expect("full queue diagnostic");
    assert_eq!(at_limit.queue_depth, 2);
    assert_eq!(at_limit.queue_bytes, MAX_WAITING_BYTES);

    fixture
        .post_json("/acp_prompt", prompt("bytes-c", String::new()))
        .await
        .assert_status_too_many_requests()
        .assert_code("prompt_queue_full");
    let unchanged = fixture
        .manager()
        .shared_session_diagnostics()
        .await
        .pop()
        .expect("rejected item leaves queue intact");
    assert_eq!(unchanged.queue_depth, 2);
    assert_eq!(unchanged.queue_bytes, MAX_WAITING_BYTES);
}

#[tokio::test]
async fn prompt_ledger_capacity_rejects_only_new_identities_over_http() {
    let fixture =
        shared_http_fixture_with_prompt_ledger_limit(BootstrapOutcome::Pending, Some(2)).await;
    let attached = fixture
        .post_connect(
            "ledger-cap-device",
            "ledger-cap-client",
            "ledger-cap-connect",
        )
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let prompt = |request_id: &str| {
        json!({
            "connectionId": attached.connection_id,
            "generation": attached.generation,
            "leaseId": attached.lease_id,
            "clientInstanceId": "ledger-cap-client",
            "clientRequestId": request_id,
            "clientMessageId": format!("message-{request_id}"),
            "blocks": [{"type": "text", "text": request_id}],
            "folderId": fixture.folder_id,
            "conversationId": fixture.conversation_id,
        })
    };
    let first_request = prompt("ledger-request-0");
    let first = fixture
        .post_json("/acp_prompt", first_request.clone())
        .await
        .assert_status_ok()
        .body;
    fixture
        .post_json("/acp_prompt", prompt("ledger-request-1"))
        .await
        .assert_status_ok();
    fixture
        .post_json("/acp_prompt", prompt("ledger-request-over"))
        .await
        .assert_status_too_many_requests()
        .assert_code("prompt_idempotency_capacity_exceeded");
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
async fn queued_and_active_shared_roots_never_enter_the_legacy_fork_path() {
    let queued_fixture = shared_http_fixture_with_pending_bootstrap().await;
    let queued = queued_fixture
        .post_connect(
            "fork-queued-device",
            "fork-queued-client",
            "fork-queued-connect",
        )
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    queued_fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": queued.connection_id,
                "generation": queued.generation,
                "leaseId": queued.lease_id,
                "clientInstanceId": "fork-queued-client",
                "clientRequestId": "fork-queued-prompt",
                "clientMessageId": "fork-queued-message",
                "blocks": [{"type": "text", "text": "queued before fork"}],
                "folderId": queued_fixture.folder_id,
                "conversationId": queued_fixture.conversation_id,
            }),
        )
        .await
        .assert_status_ok();
    let queued_fork = tokio::time::timeout(
        Duration::from_millis(250),
        queued_fixture.post_json(
            "/acp_fork",
            json!({
                "connectionId": queued.connection_id,
                "conversationId": queued_fixture.conversation_id,
                "folderId": queued_fixture.folder_id,
            }),
        ),
    )
    .await
    .expect("shared fork rejection must not wait on the legacy driver");
    queued_fork
        .assert_status_conflict()
        .assert_code("shared_session_protocol_required");
    assert_eq!(
        queued_fixture
            .manager()
            .shared_session_broker()
            .diagnostic_for_connection(&queued.connection_id)
            .await
            .unwrap()
            .queue
            .len(),
        1
    );

    let active_fixture = ready_shared_http_fixture().await;
    let active = active_fixture
        .post_connect(
            "fork-active-device",
            "fork-active-client",
            "fork-active-connect",
        )
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    active_fixture
        .manager()
        .wait_for_shared_phase(
            &active.connection_id,
            active.generation,
            SharedSessionPhase::Ready,
        )
        .await
        .unwrap();
    let (mut commands, _controls) = active_fixture
        .manager()
        .install_test_shared_connection_lanes(&active.connection_id)
        .await
        .unwrap();
    active_fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": active.connection_id,
                "generation": active.generation,
                "leaseId": active.lease_id,
                "clientInstanceId": "fork-active-client",
                "clientRequestId": "fork-active-prompt",
                "clientMessageId": "fork-active-message",
                "blocks": [{"type": "text", "text": "active before fork"}],
                "folderId": active_fixture.folder_id,
                "conversationId": active_fixture.conversation_id,
            }),
        )
        .await
        .assert_status_ok();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), commands.recv())
            .await
            .unwrap(),
        Some(ConnectionCommand::Prompt { .. })
    ));
    active_fixture
        .post_json(
            "/acp_fork",
            json!({
                "connectionId": active.connection_id,
                "conversationId": active_fixture.conversation_id,
                "folderId": active_fixture.folder_id,
            }),
        )
        .await
        .assert_status_conflict()
        .assert_code("shared_session_protocol_required");
    assert!(active_fixture
        .manager()
        .shared_session_broker()
        .diagnostic_for_connection(&active.connection_id)
        .await
        .unwrap()
        .active_turn
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
async fn mode_configuration_and_goal_mutations_require_the_current_shared_guard() {
    let fixture = ready_shared_http_fixture().await;
    let attached = fixture
        .post_connect("settings-device", "settings-client", "settings-connect")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .manager()
        .wait_for_shared_phase(
            &attached.connection_id,
            attached.generation,
            SharedSessionPhase::Ready,
        )
        .await
        .unwrap();
    let (_commands, _controls) = fixture
        .manager()
        .install_test_shared_connection_lanes(&attached.connection_id)
        .await
        .unwrap();

    let families = [
        (
            "/acp_set_mode",
            json!({"connectionId": attached.connection_id, "modeId": "plan"}),
        ),
        (
            "/acp_set_config_option",
            json!({
                "connectionId": attached.connection_id,
                "configId": "model",
                "valueId": "gpt-5",
            }),
        ),
        (
            "/acp_goal_control",
            json!({"connectionId": attached.connection_id, "action": "pause"}),
        ),
    ];

    for (route, body) in &families {
        fixture
            .post_json(route, body.clone())
            .await
            .assert_status_conflict()
            .assert_code("shared_session_protocol_required");

        let mut stale_generation = body.clone();
        stale_generation["generation"] = json!(attached.generation + 1);
        stale_generation["leaseId"] = json!(attached.lease_id);
        fixture
            .post_json(route, stale_generation)
            .await
            .assert_status_conflict()
            .assert_code("shared_session_generation_stale");

        let mut wrong_lease = body.clone();
        wrong_lease["generation"] = json!(attached.generation);
        wrong_lease["leaseId"] = json!("wrong-lease");
        fixture
            .post_json(route, wrong_lease)
            .await
            .assert_status_conflict()
            .assert_code("client_lease_missing");

        let mut valid = body.clone();
        valid["generation"] = json!(attached.generation);
        valid["leaseId"] = json!(attached.lease_id);
        fixture.post_json(route, valid).await.assert_status_ok();
    }

    let released = fixture
        .post_connect(
            "settings-release-device",
            "settings-release-client",
            "settings-release-connect",
        )
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .post_json(
            "/acp_release_lease",
            json!({
                "connectionId": released.connection_id,
                "generation": released.generation,
                "leaseId": released.lease_id,
            }),
        )
        .await
        .assert_status_ok();
    for (route, body) in &families {
        let mut released_guard = body.clone();
        released_guard["generation"] = json!(released.generation);
        released_guard["leaseId"] = json!(released.lease_id);
        fixture
            .post_json(route, released_guard)
            .await
            .assert_status_conflict()
            .assert_code("client_lease_missing");
    }

    fixture
        .manager()
        .configure_shared_client_lease_ttl(Duration::from_millis(1));
    let expiring = fixture
        .post_connect(
            "settings-device",
            "settings-client",
            "settings-renew-connect",
        )
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    tokio::time::sleep(Duration::from_millis(5)).await;
    fixture
        .manager()
        .shared_session_broker()
        .expire_leases(tokio::time::Instant::now())
        .await;
    for (route, body) in &families {
        let mut expired = body.clone();
        expired["generation"] = json!(expiring.generation);
        expired["leaseId"] = json!(expiring.lease_id);
        fixture
            .post_json(route, expired)
            .await
            .assert_status_gone()
            .assert_code("client_lease_expired");
    }
}

#[tokio::test]
async fn two_clients_have_one_permission_question_and_plan_responder_winner() {
    let fixture = ready_shared_http_fixture().await;
    let first = fixture
        .post_connect("control-device-a", "control-client-a", "control-connect-a")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .manager()
        .wait_for_shared_phase(
            &first.connection_id,
            first.generation,
            SharedSessionPhase::Ready,
        )
        .await
        .unwrap();
    let second = fixture
        .post_connect("control-device-b", "control-client-b", "control-connect-b")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let (mut commands, _controls) = fixture
        .manager()
        .install_test_shared_connection_lanes(&first.connection_id)
        .await
        .expect("controllable responder lanes");

    assert!(
        fixture
            .manager()
            .emit_test_shared_driver_event(
                &first.connection_id,
                AcpEvent::PermissionRequest {
                    request_id: "permission-race".into(),
                    tool_call: json!({"title": "Permission race"}),
                    options: vec![PermissionOptionInfo {
                        option_id: "allow".into(),
                        name: "Allow".into(),
                        kind: "allow_once".into(),
                        meta: None,
                    }],
                    queued: 0,
                },
            )
            .await
    );
    fixture
        .wait_for_pending_interaction(&first.connection_id, "permission-race")
        .await;
    let permission_body = |lease_id: &str| {
        json!({
            "connectionId": first.connection_id,
            "generation": first.generation,
            "leaseId": lease_id,
            "requestId": "permission-race",
            "optionId": "allow",
        })
    };
    let (permission_a, permission_b) = tokio::join!(
        fixture.post_json("/acp_respond_permission", permission_body(&first.lease_id)),
        fixture.post_json("/acp_respond_permission", permission_body(&second.lease_id))
    );
    assert_one_shared_control_winner("permission", permission_a, permission_b);
    match tokio::time::timeout(Duration::from_secs(2), commands.recv())
        .await
        .expect("permission responder called")
        .expect("command lane open")
    {
        ConnectionCommand::RespondPermission {
            request_id,
            option_id,
        } => {
            assert_eq!(request_id, "permission-race");
            assert_eq!(option_id, "allow");
        }
        _ => panic!("unexpected permission responder command"),
    }
    assert!(
        fixture
            .manager()
            .emit_test_shared_driver_event(
                &first.connection_id,
                AcpEvent::PermissionResolved {
                    request_id: "permission-race".into(),
                },
            )
            .await
    );

    let registered_question = fixture
        .manager()
        .register_question(
            &first.connection_id,
            vec![QuestionSpec {
                id: "question-race-item".into(),
                question: "Which client wins?".into(),
                header: "Race".into(),
                multi_select: false,
                options: Vec::new(),
                is_secret: false,
                recovery: None,
            }],
        )
        .await
        .expect("real pending question");
    fixture
        .wait_for_pending_interaction(&first.connection_id, &registered_question.question_id)
        .await;
    let question_body = |lease_id: &str, label: &str| {
        json!({
            "connectionId": first.connection_id,
            "generation": first.generation,
            "leaseId": lease_id,
            "questionId": registered_question.question_id,
            "answer": {
                "answers": [{
                    "questionId": "question-race-item",
                    "labels": [label],
                }],
                "declined": false,
            },
        })
    };
    let (question_a, question_b) = tokio::join!(
        fixture.post_json(
            "/acp_answer_question",
            question_body(&first.lease_id, "client-a")
        ),
        fixture.post_json(
            "/acp_answer_question",
            question_body(&second.lease_id, "client-b")
        )
    );
    assert_one_shared_control_winner("question", question_a, question_b);
    let question_outcome = registered_question
        .answer_rx
        .await
        .expect("question responder called once");
    assert_eq!(question_outcome.answers.len(), 1);
    assert!(matches!(
        question_outcome.answers[0].selected.as_slice(),
        [winner] if winner == "client-a" || winner == "client-b"
    ));

    let registered_plan = fixture
        .manager()
        .register_plan_approval(
            &first.connection_id,
            "plan-tool-race".into(),
            "# Concurrent plan".into(),
        )
        .await
        .expect("real pending plan approval");
    fixture
        .wait_for_pending_interaction(&first.connection_id, &registered_plan.approval_id)
        .await;
    assert_eq!(
        fixture
            .manager()
            .pending_plan_approval_parent_connection_id(&registered_plan.approval_id)
            .await
            .as_deref(),
        Some(first.connection_id.as_str()),
        "plan approval registry entry remains live before the race"
    );
    let plan_body = |lease_id: &str, decision: &str| {
        json!({
            "connectionId": first.connection_id,
            "generation": first.generation,
            "leaseId": lease_id,
            "approvalId": registered_plan.approval_id,
            "answer": {"decision": decision, "feedback": null},
        })
    };
    let (plan_a, plan_b) = tokio::join!(
        fixture.post_json(
            "/acp_answer_plan_approval",
            plan_body(&first.lease_id, "approve")
        ),
        fixture.post_json(
            "/acp_answer_plan_approval",
            plan_body(&second.lease_id, "abandon")
        )
    );
    assert_one_shared_control_winner("plan approval", plan_a, plan_b);
    let plan_outcome = registered_plan
        .answer_rx
        .await
        .expect("plan responder called once");
    assert!(matches!(
        plan_outcome.decision,
        codeg_lib::acp::plan_approval::PlanApprovalDecision::Approve
            | codeg_lib::acp::plan_approval::PlanApprovalDecision::Abandon
    ));
    tokio::task::yield_now().await;
    assert!(matches!(
        commands.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    let metrics = fixture
        .manager()
        .shared_session_broker()
        .metrics()
        .snapshot();
    assert_eq!(metrics.interaction_winner_total, 3);
    assert_eq!(metrics.interaction_stale_total, 3);
}

#[tokio::test]
async fn exact_turn_concurrent_stops_call_cancel_once_and_preserve_the_queue_tail() {
    let fixture = ready_shared_http_fixture().await;
    let first = fixture
        .post_connect("stop-device-a", "stop-client-a", "stop-connect-a")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture
        .manager()
        .wait_for_shared_phase(
            &first.connection_id,
            first.generation,
            SharedSessionPhase::Ready,
        )
        .await
        .unwrap();
    let second = fixture
        .post_connect("stop-device-b", "stop-client-b", "stop-connect-b")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let (mut commands, mut controls) = fixture
        .manager()
        .install_test_shared_connection_lanes(&first.connection_id)
        .await
        .expect("controllable stop lanes");
    let prompt = |request_id: &str, text: &str| {
        json!({
            "connectionId": first.connection_id,
            "generation": first.generation,
            "leaseId": first.lease_id,
            "clientInstanceId": "stop-client-a",
            "clientRequestId": request_id,
            "clientMessageId": format!("message-{request_id}"),
            "blocks": [{"type": "text", "text": text}],
            "folderId": fixture.folder_id,
            "conversationId": fixture.conversation_id,
        })
    };
    fixture
        .post_json("/acp_prompt", prompt("stop-head", "stop-head"))
        .await
        .assert_status_ok();
    fixture
        .post_json("/acp_prompt", prompt("stop-tail", "stop-tail"))
        .await
        .assert_status_ok();
    match tokio::time::timeout(Duration::from_secs(2), commands.recv())
        .await
        .expect("head dispatched")
        .expect("command lane open")
    {
        ConnectionCommand::Prompt { blocks, .. } => assert!(matches!(
            blocks.as_slice(),
            [codeg_lib::acp::types::PromptInputBlock::Text { text }] if text == "stop-head"
        )),
        _ => panic!("unexpected head command"),
    }
    let active = fixture
        .manager()
        .shared_session_broker()
        .diagnostic_for_connection(&first.connection_id)
        .await
        .expect("active turn projection")
        .active_turn
        .expect("head is active");
    let stop_body = |lease_id: &str| {
        json!({
            "connectionId": first.connection_id,
            "generation": first.generation,
            "leaseId": lease_id,
            "turnId": active.turn_id,
        })
    };
    let mut stop_responses = Box::pin(async {
        tokio::join!(
            fixture.post_json("/acp_cancel", stop_body(&first.lease_id)),
            fixture.post_json("/acp_cancel", stop_body(&second.lease_id))
        )
    });
    let control = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::select! {
            biased;
            control = controls.recv() => control,
            _ = &mut stop_responses => panic!("stop responses completed before cancel admission was observed"),
        }
    })
    .await
    .expect("exact-turn cancel admitted")
    .expect("control lane open");
    assert!(matches!(control, ConnectionControl::Cancel));
    assert!(
        fixture
            .manager()
            .emit_test_shared_driver_event(
                &first.connection_id,
                AcpEvent::TurnComplete {
                    session_id: "stop-session".into(),
                    stop_reason: "user_stop".into(),
                    agent_type: "codex".into(),
                    mark_awaiting_reply: true,
                    termination_source: Some(codeg_lib::models::TurnTerminationSource::UserStop),
                    provider_turn_id: None,
                },
            )
            .await
    );
    let (stop_a, stop_b) = stop_responses.await;
    let stop_responses = [stop_a, stop_b];
    assert!(
        stop_responses
            .iter()
            .any(|response| response.status == StatusCode::OK),
        "the request that delivered cancel must succeed: {stop_responses:?}"
    );
    for response in &stop_responses {
        assert!(
            response.status == StatusCode::OK
                || (response.status == StatusCode::CONFLICT
                    && response.body.get("code").and_then(Value::as_str) == Some("stale_turn")),
            "only the exact-turn finalizer may fence the concurrent loser: {response:?}"
        );
    }
    tokio::task::yield_now().await;
    assert!(matches!(
        controls.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    match tokio::time::timeout(Duration::from_secs(2), commands.recv())
        .await
        .expect("queue tail dispatched after finalizer")
        .expect("command lane open")
    {
        ConnectionCommand::Prompt { blocks, .. } => assert!(matches!(
            blocks.as_slice(),
            [codeg_lib::acp::types::PromptInputBlock::Text { text }] if text == "stop-tail"
        )),
        _ => panic!("unexpected tail command"),
    }
    fixture
        .post_json("/acp_cancel", stop_body(&first.lease_id))
        .await
        .assert_status_conflict()
        .assert_code("stale_turn");
}

#[tokio::test]
async fn dispatch_racing_queue_cancel_has_exactly_one_terminal_owner() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let attached = fixture
        .post_connect(
            "dispatch-cancel-device",
            "dispatch-cancel-client",
            "dispatch-cancel-connect",
        )
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let (mut commands, _controls) = fixture
        .manager()
        .install_test_shared_connection_lanes(&attached.connection_id)
        .await
        .expect("controllable dispatch/cancel lanes");
    let admitted = fixture
        .post_json(
            "/acp_prompt",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "clientInstanceId": "dispatch-cancel-client",
                "clientRequestId": "dispatch-cancel-request",
                "clientMessageId": "dispatch-cancel-message",
                "blocks": [{"type": "text", "text": "dispatch or cancel"}],
                "folderId": fixture.folder_id,
                "conversationId": fixture.conversation_id,
            }),
        )
        .await
        .assert_status_ok()
        .body;
    let queue_item_id = admitted
        .get("queueItemId")
        .and_then(Value::as_str)
        .expect("queue item id")
        .to_string();
    let cancel = fixture.post_json(
        "/acp_cancel_queued_prompt",
        json!({
            "connectionId": attached.connection_id,
            "generation": attached.generation,
            "leaseId": attached.lease_id,
            "queueItemId": queue_item_id,
        }),
    );
    let (_, cancel_response) = tokio::join!(
        async {
            tokio::task::yield_now().await;
            fixture.release_bootstrap_ready();
            fixture
                .manager()
                .wait_for_shared_phase(
                    &attached.connection_id,
                    attached.generation,
                    SharedSessionPhase::Ready,
                )
                .await
                .unwrap();
        },
        cancel
    );
    tokio::task::yield_now().await;
    let dispatched = match commands.try_recv() {
        Ok(ConnectionCommand::Prompt { .. }) => true,
        Ok(_) => panic!("unexpected command in dispatch/cancel race"),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => false,
        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
            panic!("command lane disconnected")
        }
    };
    assert_eq!(cancel_response.status == StatusCode::OK, !dispatched);
    if dispatched {
        cancel_response
            .assert_status_conflict()
            .assert_code("queue_item_already_dispatching");
        assert!(
            fixture
                .manager()
                .emit_test_shared_driver_event(
                    &attached.connection_id,
                    AcpEvent::TurnComplete {
                        session_id: "dispatch-cancel-session".into(),
                        stop_reason: "end_turn".into(),
                        agent_type: "codex".into(),
                        mark_awaiting_reply: true,
                        termination_source: None,
                        provider_turn_id: None,
                    },
                )
                .await
        );
    } else {
        cancel_response.assert_status_ok();
    }
    let projection = fixture
        .manager()
        .shared_session_broker()
        .diagnostic_for_connection(&attached.connection_id)
        .await
        .expect("race projection");
    assert!(projection.queue.is_empty());
    assert_eq!(projection.active_turn.is_some(), dispatched);
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
async fn shutdown_fences_admission_keeps_release_available_and_restart_is_empty() {
    let fixture = shared_http_fixture_with_pending_bootstrap().await;
    let attached = fixture
        .post_connect("shutdown-device", "shutdown-client", "shutdown-connect")
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    let prompt = json!({
        "connectionId": attached.connection_id,
        "generation": attached.generation,
        "leaseId": attached.lease_id,
        "clientInstanceId": "shutdown-client",
        "clientRequestId": "shutdown-prompt",
        "clientMessageId": "shutdown-message",
        "blocks": [{"type": "text", "text": "queued before shutdown"}],
        "folderId": fixture.folder_id,
        "conversationId": fixture.conversation_id,
    });
    fixture
        .post_json("/acp_prompt", prompt.clone())
        .await
        .assert_status_ok();

    fixture.manager().begin_shutdown();
    fixture
        .post_connect(
            "shutdown-device-2",
            "shutdown-client-2",
            "shutdown-connect-2",
        )
        .await
        .assert_status_service_unavailable()
        .assert_code("server_shutting_down");

    fixture
        .manager()
        .shared_session_broker()
        .begin_shutdown()
        .await;
    assert_eq!(
        fixture
            .manager()
            .shared_session_broker()
            .diagnostic_for_connection(&attached.connection_id)
            .await
            .expect("closing record retained until cleanup")
            .phase,
        SharedSessionPhase::Closing
    );

    fixture
        .post_connect(
            "shutdown-device-3",
            "shutdown-client-3",
            "shutdown-connect-3",
        )
        .await
        .assert_status_service_unavailable()
        .assert_code("server_shutting_down");
    fixture
        .post_json("/acp_prompt", prompt)
        .await
        .assert_status_conflict()
        .assert_code("shared_session_closing");
    fixture
        .post_json(
            "/acp_answer_question",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "questionId": "shutdown-question",
                "answer": {"answers": [], "declined": true},
            }),
        )
        .await
        .assert_status_conflict()
        .assert_code("shared_session_closing");
    fixture
        .post_json(
            "/acp_cancel",
            json!({
                "connectionId": attached.connection_id,
                "generation": attached.generation,
                "leaseId": attached.lease_id,
                "turnId": "shutdown-turn",
            }),
        )
        .await
        .assert_status_conflict()
        .assert_code("shared_session_closing");
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

    fixture
        .manager()
        .drain_for_shutdown(AcpDisconnectOrigin::ApplicationShutdown)
        .await;
    assert!(fixture
        .manager()
        .shared_session_diagnostics()
        .await
        .is_empty());

    let restarted = ConnectionManager::new();
    assert!(restarted.shared_session_diagnostics().await.is_empty());
    assert_eq!(restarted.shared_spawn_count_for_test(), 0);
    let metrics = restarted.shared_session_broker().metrics().snapshot();
    assert_eq!(metrics.live_sessions, 0);
    assert_eq!(metrics.active_leases, 0);
    assert_eq!(metrics.waiting_prompts, 0);
    assert_eq!(metrics.dispatch_total, 0);
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

#[tokio::test]
async fn failed_tombstone_uses_retained_state_for_retry_release_and_termination() {
    let fixture = shared_http_fixture(BootstrapOutcome::CompanionFailure).await;
    let request = fixture.connect_json("failed-device", "failed-client", "failed-retry");
    let first = fixture
        .post_json("/acp_connect_or_attach", request.clone())
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    fixture.wait_for_failed_cleanup(&first.connection_id).await;
    assert!(
        !fixture
            .manager()
            .has_connection_map_entry_for_test(&first.connection_id)
            .await
    );

    let retry = fixture
        .post_json("/acp_connect_or_attach", request)
        .await
        .assert_status_ok()
        .json::<AcpConnectOrAttachResponse>();
    assert_eq!(retry.connection_id, first.connection_id);
    assert_eq!(retry.generation, first.generation);
    assert_eq!(retry.lease_id, first.lease_id);
    assert_eq!(retry.phase, SharedPublicPhase::Failed);
    assert!(
        retry.event_seq > 0,
        "retained failed projection must supply its current event sequence"
    );

    fixture
        .post_json(
            "/acp_release_lease",
            json!({
                "connectionId": retry.connection_id,
                "generation": retry.generation,
                "leaseId": retry.lease_id,
            }),
        )
        .await
        .assert_status_ok();
    fixture
        .post_json(
            "/acp_terminate_shared_session",
            json!({
                "connectionId": retry.connection_id,
                "generation": retry.generation,
            }),
        )
        .await
        .assert_status_ok();
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
