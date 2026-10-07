//! Live fixed-origin Responses gateway. The socket belongs to one attempt;
//! provider credentials never cross it. Rollout and expiry are rechecked on
//! every request, and the actual JSON transcript is bounded before forwarding.

use super::feature_gate::{AdmissionFacts, ExecutionGate, ExecutionScope};
use super::gateway::{ApprovedOrigin, ClientPolicy, HostCredential};
use super::request_accounting::{EncodedModelRequest, RequestAccounting};
use super::rt_error;
use futures_util::StreamExt;
use roundtable_protocol::{
    canonical_bytes, ErrorCode, MonoMs, QualifiedContextProfile, RtError, RtResult, ToolExchange,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

/// Typed owner cancellation survives body-extractor error wrapping. It is
/// distinct from genuine request/provider errors that happen during drain.
#[derive(Debug, Clone, Copy)]
struct GatewayRevoked;
impl std::fmt::Display for GatewayRevoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("gateway owner revoked")
    }
}
impl std::error::Error for GatewayRevoked {}

fn caused_by_owner_revocation(mut error: &(dyn std::error::Error + 'static)) -> bool {
    loop {
        if error.is::<GatewayRevoked>()
            || error
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
                .is_some_and(|inner| inner.is::<GatewayRevoked>())
        {
            return true;
        }
        match error.source() {
            Some(source) => error = source,
            None => return false,
        }
    }
}

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
    execution_lease: Option<Arc<super::resources::ExecutionLease>>,
    now: Arc<dyn Fn() -> MonoMs + Send + Sync>,
    profile: QualifiedContextProfile,
    client: reqwest::Client,
    transcript: tokio::sync::Mutex<Transcript>,
    revoked: AtomicBool,
    uncertain: AtomicBool,
    fatal_failure: Mutex<Option<RtError>>,
    cancelled: tokio_util::sync::CancellationToken,
    skip_admission: bool,
    native: Option<NativeRelay>,
    #[cfg(any(test, feature = "test-utils"))]
    fixture_origin: Option<String>,
}

struct NativeRelay {
    headers: Vec<(String, String)>,
    bearer: Mutex<String>,
    refresh: Option<super::host_model_auth::RefreshMaterial>,
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
        if matches!(scope, ExecutionScope::QualificationExperiment { .. }) {
            return Err(rt_error(ErrorCode::Forbidden, "experiment_scope_not_live"));
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
            execution_lease: None,
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
            fatal_failure: Mutex::new(None),
            cancelled: tokio_util::sync::CancellationToken::new(),
            skip_admission: false,
            native: None,
            #[cfg(any(test, feature = "test-utils"))]
            fixture_origin: None,
        })
    }

    pub(crate) fn with_native_upstream(
        mut self,
        upstream: super::host_model_auth::ResolvedUpstream,
    ) -> RtResult<Self> {
        self.origin = ApprovedOrigin::parse(&upstream.origin)?;
        self.credential = HostCredential::injected(upstream.bearer.clone());
        self.native = Some(NativeRelay {
            headers: upstream.headers,
            bearer: Mutex::new(upstream.bearer),
            refresh: upstream.refresh,
        });
        Ok(self)
    }

    /// Probe relay. It checks the attempt bearer and forwards to the host
    /// credential. It does not admit product scope.
    pub(crate) fn native_probe(
        upstream: super::host_model_auth::ResolvedUpstream,
        attempt_bearer: String,
    ) -> RtResult<Self> {
        let origin = ApprovedOrigin::parse(&upstream.origin)?;
        let host_bearer = upstream.bearer.clone();
        Ok(Self {
            data_dir: PathBuf::new(),
            origin,
            credential: HostCredential::injected(host_bearer.clone()),
            model: String::new(),
            effort: None,
            bearer: attempt_bearer,
            scope: ExecutionScope::Fake,
            facts: AdmissionFacts {
                certificate: roundtable_protocol::QualificationStatus::NotTested,
                presented_key: super::QualificationKey {
                    os: super::qualification::OsIdentity {
                        name: "probe".into(),
                        version: "0".into(),
                    },
                    binaries: Vec::new(),
                    image_digest: String::new(),
                    policy_hash: roundtable_protocol::Hash256::from_bytes([0; 32]),
                    tool_contract_hash: roundtable_protocol::Hash256::from_bytes([0; 32]),
                    core_hash: roundtable_protocol::Hash256::from_bytes([0; 32]),
                    adapter_version: String::new(),
                    isolator_version: String::new(),
                    plan_hash: roundtable_protocol::Hash256::from_bytes([0; 32]),
                },
                qualification_attempts_used: 0,
                qualification_spend_used: 0,
                fixture_hash: roundtable_protocol::Hash256::from_bytes([0; 32]),
                recipient: "probe".into(),
            },
            expires: MonoMs(u64::MAX / 4),
            execution_lease: None,
            now: Arc::new(|| MonoMs(0)),
            profile: QualifiedContextProfile::proposed(
                "probe",
                roundtable_protocol::Hash256::from_bytes([0; 32]),
                2_000_000,
                0,
                "probe",
            ),
            client: ClientPolicy::approved().build_client()?,
            transcript: tokio::sync::Mutex::new(Transcript {
                accounting: RequestAccounting::new(),
                prior_generated: Vec::new(),
                required_input: Vec::new(),
                instructions: None,
            }),
            revoked: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
            fatal_failure: Mutex::new(None),
            cancelled: tokio_util::sync::CancellationToken::new(),
            skip_admission: true,
            native: Some(NativeRelay {
                headers: upstream.headers,
                bearer: Mutex::new(host_bearer),
                refresh: upstream.refresh,
            }),
            #[cfg(any(test, feature = "test-utils"))]
            fixture_origin: None,
        })
    }

    pub(crate) fn with_execution_lease(
        mut self,
        lease: Option<Arc<super::resources::ExecutionLease>>,
    ) -> Self {
        self.execution_lease = lease;
        self
    }

    fn lease_expired(&self) -> bool {
        let now = (self.now)().0;
        now >= self.expires.0
            || self
                .execution_lease
                .as_ref()
                .is_some_and(|lease| lease.admit_forward(now) == 0)
    }

    pub(crate) fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
        self.cancelled.cancel();
    }
    pub(crate) fn remote_work_uncertain(&self) -> bool {
        self.uncertain.load(Ordering::Acquire)
    }

    pub(crate) fn completion_error(&self) -> Option<RtError> {
        self.fatal_failure.lock().expect("gateway failure").clone()
    }
    fn observe_failure(&self, error: &RtError) {
        // Revocation is normal during drain/cancel. It must not manufacture a
        // failed turn or erase a real provider/policy failure already observed.
        if error.details.reason.as_deref() == Some("gateway_revoked") {
            return;
        }
        let mut failure = self.fatal_failure.lock().expect("gateway failure");
        if failure.is_none() {
            *failure = Some(error.clone());
        }
    }
    async fn forward(
        &self,
        headers: axum::http::HeaderMap,
        bytes: axum::body::Bytes,
    ) -> RtResult<(String, Vec<u8>)> {
        let result = self.forward_inner(headers, bytes).await;
        if let Err(error) = &result {
            self.observe_failure(error);
        }
        result
    }

    fn admit_upstream_chunk(
        &self,
        chunk: Result<axum::body::Bytes, reqwest::Error>,
    ) -> RtResult<axum::body::Bytes> {
        // Inspect an already-yielded transport error before revocation. Owner
        // shutdown cannot relabel an observed provider failure as cancellation.
        let chunk = chunk.map_err(|_| {
            let error = rt_error(ErrorCode::RuntimeUnavailable, "upstream_transport");
            self.observe_failure(&error);
            error
        })?;
        if self.revoked.load(Ordering::Acquire) {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "gateway_revoked"));
        }
        if self.lease_expired() {
            return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_expired"));
        }
        Ok(chunk)
    }

    async fn forward_inner(
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
        if self.revoked.load(Ordering::Acquire) {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "gateway_revoked"));
        }
        if self.lease_expired() {
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
        if self.revoked.load(Ordering::Acquire) {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "gateway_revoked"));
        }
        if self.lease_expired() {
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
            let chunk = self.admit_upstream_chunk(chunk)?;
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

    fn admit_native(&self, headers: &axum::http::HeaderMap, body_len: usize) -> RtResult<()> {
        let expected = format!("Bearer {}", self.bearer);
        let supplied = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|header| header.to_str().ok())
            .unwrap_or("");
        if !constant_eq(supplied.as_bytes(), expected.as_bytes()) {
            return Err(rt_error(ErrorCode::Unauthenticated, "gateway_token"));
        }
        if self.revoked.load(Ordering::Acquire) {
            return Err(rt_error(ErrorCode::RuntimeUnavailable, "gateway_revoked"));
        }
        if !self.skip_admission {
            if self.lease_expired() {
                return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_expired"));
            }
            ExecutionGate::open(&self.data_dir).check(&self.scope, &self.facts, (self.now)())?;
        }
        if body_len > 8 * 1024 * 1024 {
            return Err(rt_error(ErrorCode::ContextTooLarge, "request_body_limit"));
        }
        Ok(())
    }

    async fn refresh_native(&self) -> RtResult<()> {
        let Some(refresh) = self
            .native
            .as_ref()
            .and_then(|native| native.refresh.clone())
        else {
            return Err(rt_error(
                ErrorCode::RuntimeUnavailable,
                "credential_refresh",
            ));
        };
        let access = refresh.refresh(&self.client).await?;
        if let Some(native) = &self.native {
            *native.bearer.lock().expect("upstream bearer") = access;
        }
        Ok(())
    }

    async fn forward_native(
        &self,
        method: axum::http::Method,
        path_and_query: &str,
        headers: axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> RtResult<(axum::http::StatusCode, String, Vec<u8>)> {
        self.admit_native(&headers, body.len())?;
        let sent = self
            .dispatch_native(method.clone(), path_and_query, &headers, body.clone())
            .await?;
        if sent.0 == axum::http::StatusCode::UNAUTHORIZED && self.refresh_native().await.is_ok() {
            return self
                .dispatch_native(method, path_and_query, &headers, body)
                .await;
        }
        Ok(sent)
    }

    async fn dispatch_native(
        &self,
        method: axum::http::Method,
        path_and_query: &str,
        headers: &axum::http::HeaderMap,
        body: axum::body::Bytes,
    ) -> RtResult<(axum::http::StatusCode, String, Vec<u8>)> {
        if !matches!(
            method,
            axum::http::Method::GET
                | axum::http::Method::POST
                | axum::http::Method::PUT
                | axum::http::Method::PATCH
                | axum::http::Method::DELETE
        ) || !native_path_allowed(path_and_query)
        {
            return Err(rt_error(ErrorCode::PolicyUnenforceable, "request_shape"));
        }
        let native = self
            .native
            .as_ref()
            .ok_or_else(|| rt_error(ErrorCode::PolicyUnenforceable, "request_shape"))?;
        let host_bearer = native.bearer.lock().expect("upstream bearer").clone();
        let origin = self.origin.as_str();
        #[cfg(any(test, feature = "test-utils"))]
        let origin = self.fixture_origin.as_deref().unwrap_or(origin);
        let url = format!("{}{path_and_query}", origin.trim_end_matches('/'));
        let previously_uncertain = self.uncertain.swap(true, Ordering::AcqRel);
        let mut request = self.client.request(method, &url);
        if let Some(content_type) = headers.get(axum::http::header::CONTENT_TYPE) {
            request = request.header(axum::http::header::CONTENT_TYPE, content_type);
        }
        if let Some(accept) = headers.get(axum::http::header::ACCEPT) {
            request = request.header(axum::http::header::ACCEPT, accept);
        }
        for (key, value) in &native.headers {
            request = request.header(key, value);
        }
        request = request.bearer_auth(&host_bearer);
        if !body.is_empty() {
            request = request.body(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "upstream_transport"))?;
        if response.status().is_redirection() {
            return Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "redirect_forbidden",
            ));
        }
        let status = response.status();
        let content_type = response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .unwrap_or("application/json")
            .to_owned();
        let mut received = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "upstream_transport"))?;
            if received.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return Err(rt_error(ErrorCode::ContextTooLarge, "generated_utf8_limit"));
            }
            received.extend_from_slice(&chunk);
        }
        if contains(&received, host_bearer.as_bytes())
            || contains(&received, self.bearer.as_bytes())
        {
            return Err(rt_error(
                ErrorCode::RuntimeUnavailable,
                "unsafe_upstream_body",
            ));
        }
        self.uncertain
            .store(previously_uncertain, Ordering::Release);
        Ok((status, content_type, received))
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
                    super::GatewayRevoked,
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
            let router = gateway_router(gateway.clone());
            let task = connections::spawn(listener, router, cancelled.clone());
            Ok(Self {
                task,
                path: path.to_path_buf(),
                cancelled,
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, gateway, handle, gateway_router);
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
fn gateway_routes(native: bool) -> axum::Router<Arc<LiveModelGateway>> {
    if native {
        axum::Router::new()
            .fallback(native_handle)
            .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
    } else {
        axum::Router::new()
            .route("/v1/responses", axum::routing::post(handle))
            .fallback(reject_gateway_route)
            .method_not_allowed_fallback(reject_gateway_method)
            .layer(axum::extract::DefaultBodyLimit::max(1_048_576))
    }
}
fn gateway_router(gateway: Arc<LiveModelGateway>) -> axum::Router {
    let native = gateway.native.is_some();
    gateway_routes(native)
        .layer(axum::middleware::from_fn_with_state(
            gateway.clone(),
            observe_http_failure,
        ))
        .with_state(gateway)
}

async fn observe_http_failure(
    axum::extract::State(gateway): axum::extract::State<Arc<LiveModelGateway>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let response = next.run(request).await;
    // A genuine rejection remains fatal even if normal draining began while
    // next.run was in flight. Exempt only an explicitly typed owner result.
    if (response.status().is_client_error() || response.status().is_server_error())
        && response.extensions().get::<GatewayRevoked>().is_none()
    {
        gateway.observe_failure(&rt_error(
            ErrorCode::RuntimeUnavailable,
            "gateway_http_error",
        ));
    }
    response
}

async fn reject_gateway_route(
    axum::extract::State(gateway): axum::extract::State<Arc<LiveModelGateway>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    gateway.observe_failure(&rt_error(
        ErrorCode::RuntimeUnavailable,
        "gateway_http_error",
    ));
    axum::http::StatusCode::NOT_FOUND.into_response()
}
async fn reject_gateway_method(
    axum::extract::State(gateway): axum::extract::State<Arc<LiveModelGateway>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    gateway.observe_failure(&rt_error(
        ErrorCode::RuntimeUnavailable,
        "gateway_http_error",
    ));
    axum::http::StatusCode::METHOD_NOT_ALLOWED.into_response()
}

fn gateway_error_response(error: RtError) -> axum::response::Response {
    use axum::response::IntoResponse;
    let revoked = error.details.reason.as_deref() == Some("gateway_revoked");
    let mut response = (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(json!({"error":error})),
    )
        .into_response();
    if revoked {
        response.extensions_mut().insert(GatewayRevoked);
    }
    response
}

async fn handle(
    axum::extract::State(gateway): axum::extract::State<Arc<LiveModelGateway>>,
    headers: axum::http::HeaderMap,
    body: Result<axum::body::Bytes, axum::extract::rejection::BytesRejection>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let body = match body {
        Ok(body) => body,
        Err(error) if caused_by_owner_revocation(&error) => {
            return gateway_error_response(rt_error(
                ErrorCode::RuntimeUnavailable,
                "gateway_revoked",
            ))
        }
        Err(error) => {
            // Observe before generating the response, not after middleware
            // yields or shutdown cancels its connection task.
            gateway.observe_failure(&rt_error(
                ErrorCode::RuntimeUnavailable,
                "gateway_http_error",
            ));
            return error.into_response();
        }
    };
    let result = tokio::select! {
        biased;
        result = gateway.forward(headers, body) => result,
        _ = gateway.cancelled.cancelled() => Err(rt_error(ErrorCode::RuntimeUnavailable,"gateway_revoked")),
    };
    match result {
        Ok((content_type, bytes)) => {
            ([(axum::http::header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        Err(error) => gateway_error_response(error),
    }
}

async fn native_handle(
    axum::extract::State(gateway): axum::extract::State<Arc<LiveModelGateway>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, 8 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) if caused_by_owner_revocation(&error) => {
            return gateway_error_response(rt_error(
                ErrorCode::RuntimeUnavailable,
                "gateway_revoked",
            ))
        }
        Err(_) => {
            gateway.observe_failure(&rt_error(
                ErrorCode::RuntimeUnavailable,
                "gateway_http_error",
            ));
            return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response();
        }
    };
    let path = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let result = tokio::select! {
        biased;
        result = gateway.forward_native(parts.method, &path, parts.headers, bytes) => result,
        _ = gateway.cancelled.cancelled() => Err(rt_error(ErrorCode::RuntimeUnavailable, "gateway_revoked")),
    };
    match result {
        Ok((status, content_type, bytes)) => {
            let mut response = (status, bytes).into_response();
            if let Ok(value) = axum::http::HeaderValue::from_str(&content_type) {
                response
                    .headers_mut()
                    .insert(axum::http::header::CONTENT_TYPE, value);
            }
            response
        }
        Err(error) => {
            gateway.observe_failure(&error);
            gateway_error_response(error)
        }
    }
}

fn native_path_allowed(path_and_query: &str) -> bool {
    let path = path_and_query.split('?').next().unwrap_or("");
    !path.is_empty()
        && path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|segment| segment != ".." && segment != ".")
}

#[cfg(any(test, feature = "test-utils"))]
pub struct GatewayFixtureObservation {
    pub errors: Vec<Option<String>>,
    pub generated_bytes: u64,
    pub requests_sent: usize,
    pub uncertain: bool,
    pub forwarded_bodies: Vec<Value>,
    pub completion: RtResult<()>,
    pub prompt_requests: usize,
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
    let cancelled_frames = [
        json!({"id":11,"method":"session/request_permission","params":{"toolCall":{"title":"run_terminal_command","kind":"execute"},"options":[{"optionId":"reject","kind":"reject_once"}]}}),
        json!({"id":5,"result":{"stopReason":"cancelled"}}),
        json!({"id":6,"result":{"stopReason":"end_turn"}}),
    ];
    let (acp, prompt_requests) =
        super::live_runtime::permission_repair_frames_fixture(&cancelled_frames, Some(&gateway))
            .await;
    gateway.revoke();
    // A late request caused by ordinary cleanup is not a fatal observation.
    let mut late_headers = axum::http::HeaderMap::new();
    late_headers.insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_static("Bearer fixture-attempt-token"),
    );
    assert_eq!(
        gateway
            .forward(late_headers, axum::body::Bytes::from_static(b"{}"))
            .await
            .unwrap_err()
            .details
            .reason
            .as_deref(),
        Some("gateway_revoked")
    );
    let completion = super::live_runtime::complete_after_gateway_drain(acp, &gateway).map(|_| ());
    Ok(GatewayFixtureObservation {
        errors,
        generated_bytes,
        requests_sent: sent.load(Ordering::Relaxed),
        uncertain: gateway.remote_work_uncertain(),
        forwarded_bodies,
        completion,
        prompt_requests,
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
        execution_lease: None,
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
        fatal_failure: Mutex::new(None),
        cancelled: tokio_util::sync::CancellationToken::new(),
        skip_admission: false,
        native: None,
        fixture_origin: None,
    })
}

/// Exercise the production handler and graceful server shutdown with real TCP
/// clients. The fake scope never acquires a qualification or upstream endpoint.
#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn uncertain_gateway_for_cleanup_fixture(
    root: &Path,
    profile: QualifiedContextProfile,
) -> RtResult<LiveModelGateway> {
    let gateway = fixture_gateway(root, profile)?;
    gateway.uncertain.store(true, Ordering::Release);
    Ok(gateway)
}

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
    let router = gateway_router(gateway.clone()).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let started = started.clone();
            async move {
                started.notify_one();
                next.run(request).await
            }
        },
    ));
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
    assert!(
        gateway.completion_error().is_none(),
        "cleanup cancellation created a failure"
    );
    Ok((true, closed))
}

/// Real loopback HTTP request through the production router, including rejection
/// before the handler/body extractor. No external service or provider is used.
#[cfg(any(test, feature = "test-utils"))]
pub async fn exercise_gateway_http_failure_fixture(
    root: &Path,
    profile: QualifiedContextProfile,
    oversized: bool,
) -> RtResult<(u16, RtResult<()>)> {
    let gateway = Arc::new(fixture_gateway(root, profile)?);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "fixture_bind"))?;
    let address = listener
        .local_addr()
        .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "fixture_bind"))?;
    let task = connections::spawn(
        listener,
        gateway_router(gateway.clone()),
        gateway.cancelled.clone(),
    );
    let client = ClientPolicy::approved().build_client()?;
    let response = if oversized {
        client
            .post(format!("http://{address}/v1/responses"))
            .bearer_auth("fixture-attempt-token")
            .body(vec![b' '; 1_048_577])
            .send()
            .await
    } else {
        client
            .get(format!("http://{address}/unsupported"))
            .bearer_auth("fixture-attempt-token")
            .send()
            .await
    }
    .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "fixture_request"))?;
    let status = response.status().as_u16();
    drop(response);
    gateway.revoke();
    let _ = task.await;
    let acp = super::live_runtime::drive_prompt_frames_fixture(&[
        json!({"id":5,"result":{"stopReason":"end_turn"}}),
    ])
    .await;
    Ok((
        status,
        super::live_runtime::complete_after_gateway_drain(acp, &gateway).map(|_| ()),
    ))
}

#[cfg(test)]
mod prepaid_tests {
    use super::*;

    #[tokio::test]
    async fn lifecycle_live_gateway_rejects_expired_room_lease_before_network() {
        let dir = tempfile::tempdir().unwrap();
        let profile = QualifiedContextProfile::proposed(
            "test-bytes",
            roundtable_protocol::Hash256::from_bytes([1; 32]),
            2_000_000,
            0,
            "prepaid-test",
        );
        let lease = Arc::new(super::super::resources::ExecutionLease::issue(0, 1000));
        let mut gateway = fixture_gateway(dir.path(), profile)
            .unwrap()
            .with_execution_lease(Some(lease));
        gateway.now = Arc::new(|| MonoMs(1000));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer fixture-attempt-token"),
        );
        let error = gateway
            .forward(headers, b"{}".as_slice().into())
            .await
            .unwrap_err();
        assert_eq!(error.details.reason.as_deref(), Some("lease_expired"));
        assert!(
            !gateway.remote_work_uncertain(),
            "no upstream request was admitted"
        );
    }
}

#[cfg(test)]
mod completion_drain_tests {
    use super::*;
    fn profile() -> QualifiedContextProfile {
        QualifiedContextProfile::proposed(
            "test-bytes",
            roundtable_protocol::Hash256::sha256(b"fake bound"),
            2_000_000,
            0,
            "test-only",
        )
    }
    fn headers() -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("Bearer fixture-attempt-token"),
        );
        headers
    }

    #[tokio::test]
    async fn revocation_between_admission_loads_is_not_reported_as_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = Arc::new_cyclic(|weak: &std::sync::Weak<LiveModelGateway>| {
            let mut gateway = fixture_gateway(dir.path(), profile()).unwrap();
            let target = weak.clone();
            gateway.now = Arc::new(move || {
                target.upgrade().unwrap().revoke();
                MonoMs(0)
            });
            gateway
        });
        let body = json!({"model":"fixture-model","store":false,"input":[],"tools":[],"reasoning":{"effort":"low"}});
        let error = gateway
            .forward(headers(), canonical_bytes(&body).unwrap().into())
            .await
            .unwrap_err();
        assert_eq!(error.details.reason.as_deref(), Some("gateway_revoked"));
        assert!(gateway.completion_error().is_none());
        assert!(!gateway.remote_work_uncertain());
    }

    #[tokio::test]
    async fn owner_io_marker_survives_actual_body_extractor() {
        use axum::extract::FromRequest;
        let dir = tempfile::tempdir().unwrap();
        let gateway = Arc::new(fixture_gateway(dir.path(), profile()).unwrap());
        let body = axum::body::Body::from_stream(futures_util::stream::once(async {
            Err::<axum::body::Bytes, _>(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                GatewayRevoked,
            ))
        }));
        let error = axum::body::Bytes::from_request(axum::extract::Request::new(body), &())
            .await
            .unwrap_err();
        assert!(caused_by_owner_revocation(&error));
        gateway.revoke();
        let response = handle(axum::extract::State(gateway.clone()), headers(), Err(error)).await;
        assert!(response.extensions().get::<GatewayRevoked>().is_some());
        assert!(gateway.completion_error().is_none());
    }

    #[tokio::test]
    async fn completed_router_and_body_rejections_remain_fatal_when_drain_starts() {
        for oversized in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let gateway = Arc::new(fixture_gateway(dir.path(), profile()).unwrap());
            let rejected = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let ready = rejected.clone();
            let go = release.clone();
            let router = gateway_routes(false)
                .layer(axum::middleware::from_fn(
                    move |request: axum::extract::Request, next: axum::middleware::Next| {
                        let ready = ready.clone();
                        let go = go.clone();
                        async move {
                            let response = next.run(request).await;
                            ready.notify_one();
                            go.notified().await;
                            response
                        }
                    },
                ))
                .layer(axum::middleware::from_fn_with_state(
                    gateway.clone(),
                    observe_http_failure,
                ))
                .with_state(gateway.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
            let server = tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(async {
                        let _ = stopped.await;
                    })
                    .await
                    .unwrap();
            });
            let request = tokio::spawn(async move {
                let client = ClientPolicy::approved().build_client().unwrap();
                if oversized {
                    client
                        .post(format!("http://{address}/v1/responses"))
                        .bearer_auth("fixture-attempt-token")
                        .body(vec![b' '; 1_048_577])
                        .send()
                        .await
                        .unwrap()
                } else {
                    client
                        .get(format!("http://{address}/unsupported"))
                        .send()
                        .await
                        .unwrap()
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), rejected.notified())
                .await
                .unwrap();
            gateway.revoke();
            release.notify_one();
            let response = request.await.unwrap();
            assert_eq!(
                response.status().as_u16(),
                if oversized { 413 } else { 404 }
            );
            drop(response);
            let _ = stop.send(());
            tokio::time::timeout(std::time::Duration::from_secs(5), server)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                gateway
                    .completion_error()
                    .unwrap()
                    .details
                    .reason
                    .as_deref(),
                Some("gateway_http_error")
            );
            assert!(
                super::super::live_runtime::complete_after_gateway_drain(Ok(1), &gateway).is_err()
            );
        }
    }

    #[tokio::test]
    async fn already_yielded_upstream_body_error_is_not_relabelled_by_revoke() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let gateway = fixture_gateway(dir.path(), profile()).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx")
                .await
                .unwrap();
            socket.shutdown().await.unwrap();
        });
        let response = ClientPolicy::approved()
            .build_client()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let mut stream = response.bytes_stream();
        let yielded = loop {
            match stream.next().await {
                Some(Err(error)) => break Err(error),
                Some(Ok(_)) => {}
                None => panic!("truncated upstream body must fail"),
            }
        };
        gateway.revoke();
        let error = gateway.admit_upstream_chunk(yielded).unwrap_err();
        assert_eq!(error.details.reason.as_deref(), Some("upstream_transport"));
        assert_eq!(
            gateway
                .completion_error()
                .unwrap()
                .details
                .reason
                .as_deref(),
            Some("upstream_transport")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn native_forward_swaps_the_attempt_bearer_for_the_host_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
        let record = seen.clone();
        tokio::spawn(async move {
            let app = axum::Router::new().route(
                "/v1/responses",
                axum::routing::post(
                    move |headers: axum::http::HeaderMap, body: axum::body::Bytes| {
                        let record = record.clone();
                        async move {
                            record.lock().expect("seen").push((
                                headers
                                    .get(axum::http::header::AUTHORIZATION)
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or("")
                                    .to_string(),
                                headers
                                    .get("x-xai-token-auth")
                                    .and_then(|value| value.to_str().ok())
                                    .unwrap_or("")
                                    .to_string(),
                                String::from_utf8_lossy(&body).into_owned(),
                            ));
                            (axum::http::StatusCode::OK, r#"{"status":"completed"}"#)
                        }
                    },
                ),
            );
            axum::serve(listener, app).await.unwrap();
        });
        let upstream = super::super::host_model_auth::ResolvedUpstream {
            origin: "https://cli-chat-proxy.grok.com".into(),
            bearer: "host-oidc-token".into(),
            headers: vec![("X-XAI-Token-Auth".into(), "xai-grok-cli".into())],
            refresh: None,
        };
        let mut gateway =
            LiveModelGateway::native_probe(upstream, "attempt-bearer".into()).unwrap();
        gateway.fixture_origin = Some(format!("http://{address}"));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer attempt-bearer".parse().unwrap(),
        );
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        let (status, _, body) = gateway
            .forward_native(
                axum::http::Method::POST,
                "/v1/responses",
                headers,
                br#"{"model":"grok-4.6","input":"ping"}"#.as_slice().into(),
            )
            .await
            .unwrap();
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(body, br#"{"status":"completed"}"#);
        let seen = seen.lock().expect("seen");
        assert_eq!(seen[0].0, "Bearer host-oidc-token");
        assert_eq!(seen[0].1, "xai-grok-cli");
        assert!(seen[0].2.contains("grok-4.6"));
        assert!(!seen[0].0.contains("attempt-bearer"));
    }
}
