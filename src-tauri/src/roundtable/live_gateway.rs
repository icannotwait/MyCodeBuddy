//! Live fixed-origin Responses gateway. The socket belongs to one attempt;
//! provider credentials never cross it. Rollout and expiry are rechecked on
//! every request, and the actual JSON transcript is bounded before forwarding.

use super::feature_gate::{AdmissionFacts, ExecutionGate, ExecutionScope};
use super::gateway::{ApprovedOrigin, ClientPolicy, HostCredential};
use super::request_accounting::{EncodedModelRequest, RequestAccounting};
use super::rt_error;
use futures_util::StreamExt;
use roundtable_protocol::{
    canonical_bytes, ErrorCode, MonoMs, QualifiedContextProfile, RtResult, ToolExchange,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

struct Transcript {
    accounting: RequestAccounting,
    prior_generated: Vec<u8>,
    required_input: Vec<Value>,
    instructions: Option<Value>,
}

pub(crate) struct LiveModelGateway {
    data_dir: PathBuf,
    origin: ApprovedOrigin,
    credential: HostCredential,
    model: String,
    effort: Option<String>,
    bearer: String,
    scope: ExecutionScope,
    facts: AdmissionFacts,
    expires: MonoMs,
    now: Arc<dyn Fn() -> MonoMs + Send + Sync>,
    profile: QualifiedContextProfile,
    client: reqwest::Client,
    transcript: tokio::sync::Mutex<Transcript>,
    revoked: AtomicBool,
    uncertain: AtomicBool,
    cancelled: tokio_util::sync::CancellationToken,
    #[cfg(any(test, feature = "test-utils"))]
    fixture_origin: Option<String>,
}

impl LiveModelGateway {
    pub(crate) fn new(
        data_dir: PathBuf,
        provider: (ApprovedOrigin, HostCredential, String, Option<String>),
        bearer: String,
        admission: (ExecutionScope, AdmissionFacts),
        expires: MonoMs,
        now: Arc<dyn Fn() -> MonoMs + Send + Sync>,
        profile: QualifiedContextProfile,
    ) -> RtResult<Self> {
        let (origin, credential, model, effort) = provider;
        let (scope, facts) = admission;
        if matches!(scope, ExecutionScope::Fake) {
            return Err(rt_error(ErrorCode::Forbidden, "fake_scope_not_live"));
        }
        super::request_accounting::enforce_profile_caps(&profile)?;
        ExecutionGate::open(&data_dir).check(&scope, &facts, now())?;
        Ok(Self {
            data_dir,
            origin,
            credential,
            model,
            effort,
            bearer,
            scope,
            facts,
            expires,
            now,
            profile,
            client: ClientPolicy::approved().build_client()?,
            transcript: tokio::sync::Mutex::new(Transcript {
                accounting: RequestAccounting::new(),
                prior_generated: Vec::new(),
                required_input: Vec::new(),
                instructions: None,
            }),
            revoked: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
            cancelled: tokio_util::sync::CancellationToken::new(),
            #[cfg(any(test, feature = "test-utils"))]
            fixture_origin: None,
        })
    }

    pub(crate) fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
        self.cancelled.cancel();
    }
    pub(crate) fn remote_work_uncertain(&self) -> bool {
        self.uncertain.load(Ordering::Acquire)
    }

    async fn forward(
        &self,
        headers: axum::http::HeaderMap,
        bytes: axum::body::Bytes,
    ) -> RtResult<(String, Vec<u8>)> {
        let expected = format!("Bearer {}", self.bearer);
        let supplied = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|header| header.to_str().ok())
            .unwrap_or("");
        if !constant_eq(supplied.as_bytes(), expected.as_bytes()) {
            return Err(rt_error(ErrorCode::Unauthenticated, "gateway_token"));
        }
        if self.revoked.load(Ordering::Acquire) || (self.now)().0 >= self.expires.0 {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_expired"));
        }
        ExecutionGate::open(&self.data_dir).check(&self.scope, &self.facts, (self.now)())?;
        if bytes.len() as u64 > self.profile.max_request_body_bytes {
            return Err(rt_error(ErrorCode::ContextTooLarge, "request_body_limit"));
        }
        let mut body: Value = roundtable_protocol::parse_strict_json(
            &bytes,
            &roundtable_protocol::ParseLimits::suggested_profile(),
        )?;
        let object = body
            .as_object()
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "request_shape"))?;
        let allowed = [
            "model",
            "input",
            "instructions",
            "tools",
            "tool_choice",
            "parallel_tool_calls",
            "reasoning",
            "text",
            "max_output_tokens",
            "stream",
            "store",
            "include",
            "metadata",
        ];
        if object.keys().any(|key| !allowed.contains(&key.as_str()))
            || body["model"].as_str() != Some(self.model.as_str())
            || body.get("store").and_then(Value::as_bool) != Some(false)
        {
            return Err(rt_error(ErrorCode::PolicyUnenforceable, "request_shape"));
        }
        let output_limit = match body.get("max_output_tokens") {
            None => self.profile.generation_reserve_tokens,
            Some(value) => value
                .as_u64()
                .filter(|value| *value > 0 && *value <= self.profile.generation_reserve_tokens)
                .ok_or_else(|| {
                    rt_error(ErrorCode::ContextTooLarge, "generation_reserve_exceeded")
                })?,
        };
        if self
            .effort
            .as_ref()
            .is_some_and(|effort| body["reasoning"]["effort"].as_str() != Some(effort.as_str()))
        {
            return Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "reasoning_effort_changed",
            ));
        }
        body["max_output_tokens"] = json!(output_limit);
        let encoded_body = canonical_bytes(&body)?;
        if encoded_body.len() as u64 > self.profile.max_request_body_bytes {
            return Err(rt_error(ErrorCode::ContextTooLarge, "request_body_limit"));
        }
        for tool in body
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
            if tool["type"] != "function"
                || ![
                    "read_evidence",
                    "search_evidence",
                    "submit_result",
                    "mcp__roundtable__read_evidence",
                    "mcp__roundtable__search_evidence",
                    "mcp__roundtable__submit_result",
                ]
                .contains(&name)
            {
                return Err(rt_error(
                    ErrorCode::PolicyUnenforceable,
                    "unauthorized_model_tool",
                ));
            }
        }
        let input = body
            .get("input")
            .and_then(Value::as_array)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "materialized_input_required"))?;
        let mut transcript = self.transcript.lock().await;
        // A previous HTTP call may have held this lock beyond the lease or a
        // policy change. Admission must be fresh at the actual send boundary.
        if self.revoked.load(Ordering::Acquire) || (self.now)().0 >= self.expires.0 {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_expired"));
        }
        ExecutionGate::open(&self.data_dir).check(&self.scope, &self.facts, (self.now)())?;
        if !input.starts_with(&transcript.required_input) {
            return Err(rt_error(ErrorCode::InvalidArgument, "prior_output_missing"));
        }
        if transcript
            .instructions
            .as_ref()
            .is_some_and(|instructions| body.get("instructions") != Some(instructions))
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "instructions_changed"));
        }
        let mut exchanges = Vec::new();
        for item in &input[transcript.required_input.len()..] {
            if item["type"] == "function_call_output" {
                exchanges.push(ToolExchange {
                    arguments: Vec::new(),
                    reply: canonical_bytes(item)?,
                    generated_utf8_bytes: 0,
                    evidence_bytes: 0,
                    model_requests: 0,
                    tool_calls: 1,
                });
            }
        }
        let encoded = EncodedModelRequest {
            model: self.model.clone(),
            method: "POST".into(),
            path: "/v1/responses".into(),
            prior_output: transcript.prior_generated.clone(),
            adapter_bytes: encoded_body.len() as u64,
            history_ref: None,
            target_url: None,
            redirect_to: None,
            declared_tools: Vec::new(),
            tool_exchanges: exchanges,
            caller_authorization: None,
            submission_id: None,
        };
        transcript.accounting.authorize(&encoded, &self.profile)?;
        let previously_uncertain = self.uncertain.swap(true, Ordering::AcqRel);
        let origin = self.origin.as_str();
        #[cfg(any(test, feature = "test-utils"))]
        let origin = self.fixture_origin.as_deref().unwrap_or(origin);
        let response = self
            .client
            .post(format!("{origin}/v1/responses"))
            .bearer_auth(self.credential.value_for_gateway())
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(encoded_body)
            .send()
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "upstream_transport"))?;
        if response.status().is_redirection() {
            return Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "redirect_forbidden",
            ));
        }
        if !response.status().is_success() {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "upstream_rejected"));
        }
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .unwrap_or("application/json")
            .to_owned();
        if content_type != "application/json" && !content_type.starts_with("text/event-stream") {
            return Err(rt_error(
                ErrorCode::RuntimeUnavailable,
                "upstream_content_type",
            ));
        }
        let mut stream = response.bytes_stream();
        let mut received = Vec::new();
        while let Some(chunk) = stream.next().await {
            if self.revoked.load(Ordering::Acquire) || (self.now)().0 >= self.expires.0 {
                return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_expired"));
            }
            let chunk =
                chunk.map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "upstream_transport"))?;
            transcript
                .accounting
                .consume_generated(chunk.len() as u64, &self.profile)?;
            let projected = received
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| rt_error(ErrorCode::ContextTooLarge, "generated_utf8_limit"))?;
            if projected as u64 > self.profile.max_attempt_generated_utf8_bytes {
                return Err(rt_error(ErrorCode::ContextTooLarge, "generated_utf8_limit"));
            }
            received.extend_from_slice(&chunk);
        }
        if std::str::from_utf8(&received).is_err()
            || contains(&received, self.credential.value_for_gateway().as_bytes())
        {
            return Err(rt_error(
                ErrorCode::RuntimeUnavailable,
                "unsafe_upstream_body",
            ));
        }
        let output = completed_output(&received, &content_type)?;
        transcript.accounting.commit_generated_prior(&received);
        transcript.prior_generated.extend_from_slice(&received);
        transcript.required_input = input.clone();
        transcript.required_input.extend(output);
        transcript.instructions = body.get("instructions").cloned();
        self.uncertain
            .store(previously_uncertain, Ordering::Release);
        Ok((content_type, received))
    }
}

fn completed_output(bytes: &[u8], content_type: &str) -> RtResult<Vec<Value>> {
    let response = if content_type.starts_with("text/event-stream") {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "upstream_utf8"))?;
        text.lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .find(|event| event["type"] == "response.completed")
            .and_then(|event| event.get("response").cloned())
            .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "upstream_incomplete"))?
    } else {
        serde_json::from_slice(bytes)
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "upstream_json"))?
    };
    if response["status"] != "completed" {
        return Err(rt_error(
            ErrorCode::RuntimeUnavailable,
            "upstream_incomplete",
        ));
    }
    response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "upstream_output"))
}
fn constant_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |sum, (a, b)| sum | (a ^ b))
            == 0
}
fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && bytes.windows(needle.len()).any(|part| part == needle)
}

// Cancel at the IO boundary: router handlers cannot observe cancellation while
// an HTTP connection is still parsing headers or extracting an incomplete body.
#[cfg(any(unix, test, feature = "test-utils"))]
mod connections {
    use axum::serve::Listener;
    use std::future::Future;
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

    pub(super) fn spawn<L>(
        listener: L,
        router: axum::Router,
        cancelled: CancellationToken,
    ) -> tokio::task::JoinHandle<()>
    where
        L: Listener,
        L::Addr: std::fmt::Debug,
    {
        let listener = CancelListener {
            inner: listener,
            cancelled: cancelled.clone(),
        };
        tokio::spawn(async move {
            // Axum joins every accepted connection before this future finishes.
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(cancelled.cancelled_owned())
                .await;
        })
    }

    struct CancelListener<L> {
        inner: L,
        cancelled: CancellationToken,
    }
    impl<L: Listener> Listener for CancelListener<L> {
        type Io = CancelIo<L::Io>;
        type Addr = L::Addr;

        async fn accept(&mut self) -> (Self::Io, Self::Addr) {
            let (inner, address) = self.inner.accept().await;
            (
                CancelIo {
                    inner,
                    cancelled: Box::pin(self.cancelled.clone().cancelled_owned()),
                },
                address,
            )
        }
        fn local_addr(&self) -> io::Result<Self::Addr> {
            self.inner.local_addr()
        }
    }

    struct CancelIo<I> {
        inner: I,
        cancelled: Pin<Box<WaitForCancellationFutureOwned>>,
    }
    impl<I> CancelIo<I> {
        fn check_cancelled(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
            if self.cancelled.as_mut().poll(cx).is_ready() {
                Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "gateway revoked",
                ))
            } else {
                Ok(())
            }
        }
    }
    impl<I: AsyncRead + Unpin> AsyncRead for CancelIo<I> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            self.check_cancelled(cx)?;
            Pin::new(&mut self.inner).poll_read(cx, buffer)
        }
    }
    impl<I: AsyncWrite + Unpin> AsyncWrite for CancelIo<I> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.check_cancelled(cx)?;
            Pin::new(&mut self.inner).poll_write(cx, bytes)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.check_cancelled(cx)?;
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.check_cancelled(cx)?;
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }
}

pub(crate) struct LiveGatewayServer {
    task: tokio::task::JoinHandle<()>,
    path: PathBuf,
    cancelled: tokio_util::sync::CancellationToken,
}
impl LiveGatewayServer {
    pub(crate) async fn bind(path: &Path, gateway: Arc<LiveModelGateway>) -> RtResult<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let listener = tokio::net::UnixListener::bind(path)
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "gateway_bind"))?;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "gateway_permissions"))?;
            let cancelled = gateway.cancelled.clone();
            let router = axum::Router::new()
                .route("/v1/responses", axum::routing::post(handle))
                .layer(axum::extract::DefaultBodyLimit::max(1_048_576))
                .with_state(gateway);
            let task = connections::spawn(listener, router, cancelled.clone());
            Ok(Self {
                task,
                path: path.to_path_buf(),
                cancelled,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, gateway, handle);
            Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "platform_unqualified",
            ))
        }
    }
    pub(crate) async fn shutdown(mut self) {
        self.cancelled.cancel();
        let _ = (&mut self.task).await;
    }
}
impl Drop for LiveGatewayServer {
    fn drop(&mut self) {
        self.cancelled.cancel();
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}
async fn handle(
    axum::extract::State(gateway): axum::extract::State<Arc<LiveModelGateway>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let result = tokio::select! {
        result = gateway.forward(headers, body) => result,
        _ = gateway.cancelled.cancelled() => Err(rt_error(ErrorCode::RuntimeUnavailable,"gateway_revoked")),
    };
    match result {
        Ok((content_type, bytes)) => {
            ([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        Err(error) => (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(json!({"error":error})),
        )
            .into_response(),
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub struct GatewayFixtureObservation {
    pub errors: Vec<Option<String>>,
    pub generated_bytes: u64,
    pub requests_sent: usize,
    pub uncertain: bool,
    pub forwarded_bodies: Vec<Value>,
}

/// Controlled loopback fixture invokes the production forward path. This seam
/// cannot receive an upstream URL and does not construct a qualification pass.
#[cfg(any(test, feature = "test-utils"))]
pub async fn exercise_live_gateway_fixture(
    root: &Path,
    profile: QualifiedContextProfile,
    requests: Vec<Value>,
    responses: Vec<(u16, String, Vec<u8>)>,
) -> RtResult<GatewayFixtureObservation> {
    exercise_gateway_fixture(root, profile, requests, responses, None).await
}

/// The first real HTTP call blocks while a second request waits for transcript
/// ownership. Invalidate the lease or policy before releasing the first call.
#[cfg(any(test, feature = "test-utils"))]
pub async fn exercise_queued_gateway_fixture(
    root: &Path,
    profile: QualifiedContextProfile,
    expire_lease: bool,
) -> RtResult<GatewayFixtureObservation> {
    let request = json!({"model":"fixture-model","store":false,"input":[],"tools":[],"reasoning":{"effort":"low"}});
    exercise_gateway_fixture(
        root,
        profile,
        vec![request.clone(), request],
        vec![(
            200,
            "application/json".into(),
            br#"{"status":"completed","output":[]}"#.to_vec(),
        )],
        Some(expire_lease),
    )
    .await
}

#[cfg(any(test, feature = "test-utils"))]
async fn exercise_gateway_fixture(
    root: &Path,
    profile: QualifiedContextProfile,
    requests: Vec<Value>,
    responses: Vec<(u16, String, Vec<u8>)>,
    queued_invalidation: Option<bool>,
) -> RtResult<GatewayFixtureObservation> {
    use axum::response::IntoResponse;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "fixture_bind"))?;
    let address = listener
        .local_addr()
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "fixture_bind"))?;
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        responses,
    )));
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = sent.clone();
    let forwarded = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = forwarded.clone();
    let first_started = Arc::new(tokio::sync::Notify::new());
    let first_release = Arc::new(tokio::sync::Notify::new());
    let started = first_started.clone();
    let release = first_release.clone();
    let router = axum::Router::new().route(
        "/v1/responses",
        axum::routing::post(
            move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                let queue = queue.clone();
                let count = count.clone();
                let observed = observed.clone();
                let started = started.clone();
                let release = release.clone();
                async move {
                    assert_eq!(
                        headers[axum::http::header::AUTHORIZATION],
                        "Bearer fixture-host-secret"
                    );
                    assert!(!body.is_empty());
                    observed
                        .lock()
                        .expect("fixture bodies")
                        .push(serde_json::from_slice::<Value>(&body).expect("upstream JSON"));
                    count.fetch_add(1, Ordering::Relaxed);
                    if queued_invalidation.is_some() {
                        started.notify_one();
                        release.notified().await;
                    }
                    let (status, content, body) = queue
                        .lock()
                        .expect("fixture responses")
                        .pop_front()
                        .expect("scripted response");
                    (
                        axum::http::StatusCode::from_u16(status).expect("fixture status"),
                        [(axum::http::header::CONTENT_TYPE, content)],
                        body,
                    )
                        .into_response()
                }
            },
        ),
    );
    let fixture_now = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let clock = fixture_now.clone();
    let mut gateway = fixture_gateway(root, profile)?;
    gateway.fixture_origin = Some(format!("http://{address}"));
    gateway.now = Arc::new(move || MonoMs(clock.load(Ordering::Acquire)));
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let forward = |request: Value| {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer fixture-attempt-token"),
        );
        let bytes = serde_json::to_vec(&request).expect("fixture JSON");
        gateway.forward(headers, bytes.into())
    };
    let mut errors = Vec::new();
    if let Some(expire_lease) = queued_invalidation {
        let mut requests = requests.into_iter();
        let first = forward(requests.next().expect("first request"));
        tokio::pin!(first);
        tokio::select! {
            _ = first_started.notified() => {},
            result = &mut first => panic!("first call must block: {result:?}"),
        }
        let second = forward(requests.next().expect("queued request"));
        tokio::pin!(second);
        assert!(futures_util::poll!(&mut second).is_pending());
        if expire_lease {
            fixture_now.store(u64::MAX, Ordering::Release);
        } else {
            let directory = root.join("roundtable");
            std::fs::create_dir_all(&directory).expect("fixture policy directory");
            std::fs::write(directory.join("execution-policy.json"), b"invalid policy")
                .expect("invalidate fixture policy");
        }
        first_release.notify_one();
        errors.push(first.await.err().and_then(|error| error.details.reason));
        errors.push(second.await.err().and_then(|error| error.details.reason));
    } else {
        for request in requests {
            errors.push(
                forward(request)
                    .await
                    .err()
                    .and_then(|error| error.details.reason),
            );
        }
    }
    server.abort();
    let _ = server.await;
    let generated_bytes = gateway
        .transcript
        .lock()
        .await
        .accounting
        .snapshot()
        .generated_utf8_bytes;
    let forwarded_bodies = forwarded.lock().expect("fixture bodies").clone();
    Ok(GatewayFixtureObservation {
        errors,
        generated_bytes,
        requests_sent: sent.load(Ordering::Relaxed),
        uncertain: gateway.remote_work_uncertain(),
        forwarded_bodies,
    })
}

#[cfg(any(test, feature = "test-utils"))]
fn fixture_gateway(root: &Path, profile: QualifiedContextProfile) -> RtResult<LiveModelGateway> {
    use roundtable_protocol::{Hash256, QualificationStatus};
    let zero = Hash256::from_bytes([0; 32]);
    let facts = AdmissionFacts {
        certificate: QualificationStatus::NotTested,
        presented_key: super::QualificationKey {
            os: super::OsIdentity {
                name: "fixture".into(),
                version: "0".into(),
            },
            binaries: Vec::new(),
            image_digest: String::new(),
            policy_hash: zero,
            tool_contract_hash: zero,
            core_hash: zero,
            adapter_version: String::new(),
            isolator_version: String::new(),
            plan_hash: zero,
        },
        qualification_attempts_used: 0,
        qualification_spend_used: 0,
        fixture_hash: zero,
        recipient: "fixture-model".into(),
    };
    Ok(LiveModelGateway {
        data_dir: root.to_path_buf(),
        origin: ApprovedOrigin::parse("https://fixture.invalid")?,
        credential: HostCredential::injected("fixture-host-secret"),
        model: "fixture-model".into(),
        effort: Some("low".into()),
        bearer: "fixture-attempt-token".into(),
        scope: ExecutionScope::Fake,
        facts,
        expires: MonoMs(u64::MAX),
        now: Arc::new(|| MonoMs(0)),
        profile,
        client: ClientPolicy::approved().build_client()?,
        transcript: tokio::sync::Mutex::new(Transcript {
            accounting: RequestAccounting::new(),
            prior_generated: Vec::new(),
            required_input: Vec::new(),
            instructions: None,
        }),
        revoked: AtomicBool::new(false),
        uncertain: AtomicBool::new(false),
        cancelled: tokio_util::sync::CancellationToken::new(),
        fixture_origin: None,
    })
}

/// Exercise the production handler and graceful server shutdown with real TCP
/// clients. The fake scope never acquires a qualification or upstream endpoint.
#[cfg(any(test, feature = "test-utils"))]
pub async fn exercise_gateway_shutdown_fixture(
    root: &Path,
    profile: QualifiedContextProfile,
) -> RtResult<(bool, bool)> {
    use axum::serve::ListenerExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let gateway = Arc::new(fixture_gateway(root, profile)?);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = accepted.clone();
    let listener = listener.tap_io(move |_| {
        count.fetch_add(1, Ordering::Release);
    });
    let body_started = Arc::new(tokio::sync::Notify::new());
    let started = body_started.clone();
    let router = axum::Router::new()
        .route("/v1/responses", axum::routing::post(handle))
        .layer(axum::extract::DefaultBodyLimit::max(1_048_576))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let started = started.clone();
                async move {
                    started.notify_one();
                    next.run(request).await
                }
            },
        ))
        .with_state(gateway.clone());
    let cancelled = gateway.cancelled.clone();
    let task = connections::spawn(listener, router, cancelled.clone());
    let server = LiveGatewayServer {
        task,
        path: root.join("fixture-gateway.sock"),
        cancelled,
    };
    let idle = tokio::net::TcpStream::connect(address)
        .await
        .expect("idle client");
    let mut headers = tokio::net::TcpStream::connect(address)
        .await
        .expect("header client");
    headers
        .write_all(b"POST /v1/responses HTTP/1.1\r\nHost:")
        .await
        .expect("partial header");
    let mut body = tokio::net::TcpStream::connect(address)
        .await
        .expect("body client");
    body.write_all(
        b"POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\n{",
    )
    .await
    .expect("partial body");
    tokio::time::timeout(std::time::Duration::from_secs(2), body_started.notified())
        .await
        .expect("request reaches body extraction");
    assert_eq!(accepted.load(Ordering::Acquire), 3);
    gateway.revoke();
    let shutdown = server.shutdown();
    tokio::pin!(shutdown);
    let drained = tokio::time::timeout(std::time::Duration::from_millis(500), &mut shutdown)
        .await
        .is_ok();
    let clients = [idle, headers, body];
    if !drained {
        // Even the failing fixture must release its accepted connections.
        drop(clients);
        tokio::time::timeout(std::time::Duration::from_secs(2), shutdown)
            .await
            .expect("disconnecting clients permits fixture cleanup");
        return Ok((false, false));
    }
    let mut closed = true;
    for mut client in clients {
        let mut received = Vec::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            client.read_to_end(&mut received),
        )
        .await;
        closed &= matches!(result, Ok(Ok(_)))
            || matches!(result, Ok(Err(ref error)) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted));
    }
    Ok((true, closed))
}
