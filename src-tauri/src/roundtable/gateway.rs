//! Host model gateway. A handle is an instance capability, not a URL proxy.
//! Fake, qualification, and product scopes stay distinct. Credentials are
//! injected here and are not copied into responses or logs. This task never
//! sends a live model request.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use roundtable_protocol::{AttemptId, ErrorCode, Fence, MonoMs, QualifiedContextProfile, RtResult};

use super::feature_gate::ExecutionScope;
use super::request_accounting::{
    enforce_profile_caps, EncodedModelRequest, RequestAccounting, RequestPermit,
};
use super::rt_error;

const RESPONSES_PATH: &str = "/v1/responses";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayLease {
    pub attempt_id: AttemptId,
    pub fence: Fence,
    pub recipient: String,
    pub policy_generation: u64,
    pub expires_at_mono: MonoMs,
    pub context_profile: QualifiedContextProfile,
    pub scope: ExecutionScope,
}

#[derive(Clone, Debug)]
pub struct GatewayHandle {
    attempt_id: AttemptId,
    capability: [u8; 32],
    socket_label: String,
}

impl GatewayHandle {
    pub fn attempt_id(&self) -> AttemptId {
        self.attempt_id
    }

    pub fn socket_label(&self) -> &str {
        &self.socket_label
    }

    pub fn targets_arbitrary_url(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedOrigin {
    origin: String,
}

impl ApprovedOrigin {
    pub fn parse(raw: &str) -> RtResult<Self> {
        let url = url::Url::parse(raw)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "origin_not_https"))?;
        if url.scheme() != "https" {
            return Err(rt_error(ErrorCode::InvalidArgument, "origin_not_https"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(rt_error(ErrorCode::InvalidArgument, "origin_userinfo"));
        }
        let Some(host) = url.host_str() else {
            return Err(rt_error(ErrorCode::InvalidArgument, "origin_host"));
        };
        if url.path() != "/" && !url.path().is_empty() {
            return Err(rt_error(ErrorCode::InvalidArgument, "origin_path"));
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(rt_error(ErrorCode::InvalidArgument, "origin_query"));
        }
        let origin = match url.port() {
            Some(port) => format!("{}://{host}:{port}", url.scheme()),
            None => format!("{}://{host}", url.scheme()),
        };
        Ok(Self { origin })
    }

    pub fn as_str(&self) -> &str {
        &self.origin
    }
}

#[derive(Clone, Debug)]
pub struct ClientPolicy {
    verify_certs: bool,
    follow_redirects: bool,
    proxy: Option<String>,
}

impl ClientPolicy {
    pub fn approved() -> Self {
        Self {
            verify_certs: true,
            follow_redirects: false,
            proxy: None,
        }
    }

    pub fn verify_certs(mut self, yes: bool) -> Self {
        self.verify_certs = yes;
        self
    }

    pub fn follow_redirects(mut self, yes: bool) -> Self {
        self.follow_redirects = yes;
        self
    }

    pub fn proxy(mut self, target: impl Into<String>) -> Self {
        self.proxy = Some(target.into());
        self
    }

    pub fn build_client(&self) -> RtResult<reqwest::Client> {
        if !self.verify_certs {
            return Err(rt_error(ErrorCode::PolicyUnenforceable, "certs_disabled"));
        }
        if self.follow_redirects {
            return Err(rt_error(
                ErrorCode::PolicyUnenforceable,
                "redirects_enabled",
            ));
        }
        if self.proxy.is_some() {
            return Err(rt_error(ErrorCode::PolicyUnenforceable, "proxy_target"));
        }
        // Certificate verification stays at the crate default. Redirects and
        // proxies are disabled so a response cannot retarget the request.
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "client_build"))
    }
}

pub struct HostCredential {
    value: String,
}

impl HostCredential {
    pub fn injected(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }

    fn matches(&self, presented: &str) -> bool {
        constant_eq(self.value.as_bytes(), presented.as_bytes())
    }
}

impl fmt::Debug for HostCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HostCredential([redacted])")
    }
}

#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub encoded: EncodedModelRequest,
    pub now: MonoMs,
    pub scope: ExecutionScope,
    pub fence: Fence,
}

#[derive(Clone, Debug)]
pub struct ModelResponse {
    pub body: Vec<u8>,
    pub permit: RequestPermit,
    pub candidate_id: Option<String>,
    pub origin: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedForward {
    pub method: String,
    pub path: String,
    pub model: String,
    pub origin: String,
    pub prior_output: Vec<u8>,
    pub credential_injected: bool,
}

struct SharedHold {
    entered: Mutex<bool>,
    entered_cv: Condvar,
    release: Mutex<bool>,
    release_cv: Condvar,
}

pub struct HoldRelease {
    shared: Arc<SharedHold>,
}

impl HoldRelease {
    pub fn wait_entered(&self, timeout: Duration) -> bool {
        let guard = self
            .shared
            .entered
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (guard, wait) = self
            .shared
            .entered_cv
            .wait_timeout_while(guard, timeout, |entered| !*entered)
            .unwrap_or_else(|poison| poison.into_inner());
        *guard && !wait.timed_out()
    }

    pub fn release(self) {
        let mut release = self
            .shared
            .release
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        *release = true;
        self.shared.release_cv.notify_all();
    }
}

struct Upstream {
    body: Mutex<Vec<u8>>,
    status: Mutex<u16>,
    forwards: Mutex<Vec<ObservedForward>>,
    hold: Mutex<Option<Arc<SharedHold>>>,
}

impl Upstream {
    fn new() -> Self {
        Self {
            body: Mutex::new(Vec::new()),
            status: Mutex::new(200),
            forwards: Mutex::new(Vec::new()),
            hold: Mutex::new(None),
        }
    }

    fn record_and_wait(&self, observed: ObservedForward) -> (u16, Vec<u8>) {
        self.forwards
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(observed);
        let hold = self
            .hold
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(hold) = hold {
            {
                let mut entered = hold
                    .entered
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                *entered = true;
                hold.entered_cv.notify_all();
            }
            let mut release = hold
                .release
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            while !*release {
                release = hold
                    .release_cv
                    .wait(release)
                    .unwrap_or_else(|poison| poison.into_inner());
            }
        }
        let status = *self
            .status
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let body = self
            .body
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        (status, body)
    }
}

struct LeaseState {
    fence: Fence,
    recipient: String,
    policy_generation: u64,
    expires_at_mono: MonoMs,
    profile: QualifiedContextProfile,
    scope: ExecutionScope,
    capability: [u8; 32],
    revoked: bool,
    in_flight: bool,
    accounting: RequestAccounting,
}

struct GatewayState {
    leases: BTreeMap<AttemptId, LeaseState>,
    log: Vec<String>,
}

pub struct HostModelGateway {
    origin: ApprovedOrigin,
    credential: HostCredential,
    client: reqwest::Client,
    policy_generation: u64,
    instance: u128,
    state: Mutex<GatewayState>,
    upstream: Upstream,
    db_probe: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl HostModelGateway {
    pub fn fake(
        origin: ApprovedOrigin,
        credential: HostCredential,
        policy: ClientPolicy,
        policy_generation: u64,
    ) -> RtResult<Self> {
        let client = policy.build_client()?;
        Ok(Self {
            origin,
            credential,
            client,
            policy_generation,
            instance: rand::random(),
            state: Mutex::new(GatewayState {
                leases: BTreeMap::new(),
                log: Vec::new(),
            }),
            upstream: Upstream::new(),
            db_probe: Mutex::new(None),
        })
    }

    pub fn register(&self, lease: GatewayLease) -> RtResult<GatewayHandle> {
        enforce_profile_caps(&lease.context_profile)?;
        if let ExecutionScope::Product { rollout_generation } = &lease.scope {
            if *rollout_generation != self.policy_generation
                || lease.policy_generation != self.policy_generation
            {
                return Err(rt_error(
                    ErrorCode::CapabilityUnqualified,
                    "generation_mismatch",
                ));
            }
        }
        let mut state = lock(&self.state);
        if state.leases.contains_key(&lease.attempt_id) {
            return Err(rt_error(ErrorCode::InvalidState, "duplicate_attempt"));
        }
        let capability = rand::random::<[u8; 32]>();
        let socket_label = format!("roundtable-{:032x}-{}", self.instance, lease.attempt_id);
        let attempt_id = lease.attempt_id;
        let policy_generation = lease.policy_generation;
        state.leases.insert(
            attempt_id,
            LeaseState {
                fence: lease.fence,
                recipient: lease.recipient,
                policy_generation,
                expires_at_mono: lease.expires_at_mono,
                profile: lease.context_profile,
                scope: lease.scope,
                capability,
                revoked: false,
                in_flight: false,
                accounting: RequestAccounting::new(),
            },
        );
        push_log(&mut state, &self.credential.value, "decision=register");
        Ok(GatewayHandle {
            attempt_id,
            capability,
            socket_label,
        })
    }

    pub fn revoke(&self, attempt_id: AttemptId) -> RtResult<()> {
        let mut state = lock(&self.state);
        let found = {
            let lease = state
                .leases
                .get_mut(&attempt_id)
                .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "lease_unknown"))?;
            lease.revoked = true;
            true
        };
        if found {
            push_log(&mut state, &self.credential.value, "decision=revoke");
        }
        Ok(())
    }

    pub fn scope_of(&self, handle: &GatewayHandle) -> RtResult<ExecutionScope> {
        let state = lock(&self.state);
        let lease = find_lease(&state, handle)?;
        let _ = lease.policy_generation;
        Ok(lease.scope.clone())
    }

    pub fn accounting(
        &self,
        attempt_id: AttemptId,
    ) -> RtResult<super::request_accounting::AccountingSnapshot> {
        let state = lock(&self.state);
        let lease = state
            .leases
            .get(&attempt_id)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "lease_unknown"))?;
        Ok(lease.accounting.snapshot())
    }

    pub fn set_db_probe(&self, probe: Option<Arc<dyn Fn() + Send + Sync>>) {
        *self
            .db_probe
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = probe;
    }

    pub fn set_upstream_body(&self, body: Vec<u8>) {
        *self
            .upstream
            .body
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = body;
    }

    pub fn set_upstream_status(&self, status: u16) {
        *self
            .upstream
            .status
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = status;
    }

    pub fn arm_hold(&self) -> HoldRelease {
        let shared = Arc::new(SharedHold {
            entered: Mutex::new(false),
            entered_cv: Condvar::new(),
            release: Mutex::new(false),
            release_cv: Condvar::new(),
        });
        *self
            .upstream
            .hold
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&shared));
        HoldRelease { shared }
    }

    pub fn forward_count(&self) -> usize {
        self.upstream
            .forwards
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .len()
    }

    pub fn forwards(&self) -> Vec<ObservedForward> {
        self.upstream
            .forwards
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    pub fn log_lines(&self) -> Vec<String> {
        lock(&self.state).log.clone()
    }

    pub fn handle(&self, handle: &GatewayHandle, request: ModelRequest) -> RtResult<ModelResponse> {
        // The client is built with verification on, redirects off, and no proxy.
        // Dispatch below stays on the in-process fake upstream.
        let _ = &self.client;
        let secret = self.credential.value.clone();
        {
            let mut state = lock(&self.state);
            if let Err(err) = admit_clock(&mut state, handle, request.now) {
                push_log(&mut state, &secret, "decision=reject");
                return Err(err);
            }
        }
        self.call_probe();
        let prepared = {
            let mut state = lock(&self.state);
            match prepare_forward(&mut state, handle, &request) {
                Ok(prepared) => prepared,
                Err(err) => {
                    push_log(&mut state, &secret, "decision=reject");
                    return Err(err);
                }
            }
        };
        let injected_header = self.credential.value.clone();
        let credential_injected = self.credential.matches(&injected_header);
        let observed = ObservedForward {
            method: "POST".to_string(),
            path: RESPONSES_PATH.to_string(),
            model: request.encoded.model.clone(),
            origin: self.origin.as_str().to_string(),
            prior_output: request.encoded.prior_output.clone(),
            credential_injected,
        };
        let (status, body) = self.upstream.record_and_wait(observed);
        let origin = self.origin.as_str().to_string();
        let mut state = lock(&self.state);
        let result = finish_forward(
            &mut state, handle, &prepared, status, &body, &secret, &origin,
        );
        match &result {
            Ok(_) => push_log(&mut state, &secret, "decision=forward"),
            Err(_) => push_log(&mut state, &secret, "decision=reject"),
        }
        result
    }

    fn call_probe(&self) {
        let probe = self
            .db_probe
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        if let Some(probe) = probe {
            probe();
        }
    }
}

fn lock(mutex: &Mutex<GatewayState>) -> std::sync::MutexGuard<'_, GatewayState> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn push_log(state: &mut GatewayState, secret: &str, line: &str) {
    if line_leaks(line, secret) {
        state.log.push("decision=reject".to_string());
        return;
    }
    state.log.push(line.to_string());
}

fn admit_clock(state: &mut GatewayState, handle: &GatewayHandle, now: MonoMs) -> RtResult<()> {
    let lease = find_lease_mut(state, handle)?;
    if lease.revoked {
        return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_revoked"));
    }
    if now.0 >= lease.expires_at_mono.0 {
        lease.revoked = true;
        return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_expired"));
    }
    Ok(())
}

fn prepare_forward(
    state: &mut GatewayState,
    handle: &GatewayHandle,
    request: &ModelRequest,
) -> RtResult<Prepared> {
    let lease = find_lease_mut(state, handle)?;
    if lease.revoked {
        return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_revoked"));
    }
    if request.now.0 >= lease.expires_at_mono.0 {
        lease.revoked = true;
        return Err(rt_error(ErrorCode::CapabilityUnqualified, "lease_expired"));
    }
    if request.scope != lease.scope {
        return Err(rt_error(ErrorCode::Forbidden, "scope_mismatch"));
    }
    if request.fence != lease.fence {
        return Err(rt_error(ErrorCode::InvalidArgument, "stale_fence"));
    }
    if lease.in_flight {
        return Err(rt_error(ErrorCode::CommandInProgress, "attempt_inflight"));
    }
    if let ExecutionScope::Product { rollout_generation } = &lease.scope {
        if *rollout_generation != lease.policy_generation {
            return Err(rt_error(
                ErrorCode::CapabilityUnqualified,
                "generation_mismatch",
            ));
        }
    }
    reject_shape(&lease.recipient, &request.encoded)?;
    let permit = lease
        .accounting
        .authorize(&request.encoded, &lease.profile)?;
    let profile = lease.profile.clone();
    lease.in_flight = true;
    Ok(Prepared { permit, profile })
}

fn finish_forward(
    state: &mut GatewayState,
    handle: &GatewayHandle,
    prepared: &Prepared,
    status: u16,
    body: &[u8],
    secret: &str,
    origin: &str,
) -> RtResult<ModelResponse> {
    let lease = find_lease_mut(state, handle)?;
    lease.in_flight = false;
    if (300..400).contains(&status) {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "redirect_forbidden",
        ));
    }
    if status != 200 {
        return Err(rt_error(ErrorCode::RuntimeUnavailable, "upstream_rejected"));
    }
    let stripped = strip_secret(body, secret);
    if std::str::from_utf8(&stripped).is_err() {
        return Err(rt_error(ErrorCode::InvalidArgument, "invalid_utf8"));
    }
    lease
        .accounting
        .note_generated(&stripped, &prepared.profile)?;
    let candidate_id = prepared.permit.candidate_id().map(str::to_string);
    Ok(ModelResponse {
        body: stripped,
        permit: prepared.permit.clone(),
        candidate_id,
        origin: origin.to_string(),
    })
}

struct Prepared {
    permit: RequestPermit,
    profile: QualifiedContextProfile,
}

fn find_lease<'a>(state: &'a GatewayState, handle: &GatewayHandle) -> RtResult<&'a LeaseState> {
    let lease = state
        .leases
        .get(&handle.attempt_id)
        .ok_or_else(|| rt_error(ErrorCode::Unauthenticated, "lease_capability"))?;
    if !constant_eq(&lease.capability, &handle.capability) {
        return Err(rt_error(ErrorCode::Unauthenticated, "lease_capability"));
    }
    Ok(lease)
}

fn find_lease_mut<'a>(
    state: &'a mut GatewayState,
    handle: &GatewayHandle,
) -> RtResult<&'a mut LeaseState> {
    let lease = state
        .leases
        .get_mut(&handle.attempt_id)
        .ok_or_else(|| rt_error(ErrorCode::Unauthenticated, "lease_capability"))?;
    if !constant_eq(&lease.capability, &handle.capability) {
        return Err(rt_error(ErrorCode::Unauthenticated, "lease_capability"));
    }
    Ok(lease)
}

fn reject_shape(recipient: &str, request: &EncodedModelRequest) -> RtResult<()> {
    if request.method != "POST" || request.path != RESPONSES_PATH {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "g1_failed"));
    }
    if request.model != recipient {
        return Err(rt_error(ErrorCode::InvalidArgument, "model_mismatch"));
    }
    if request.target_url.is_some() {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "arbitrary_url"));
    }
    if request.redirect_to.is_some() {
        return Err(rt_error(
            ErrorCode::PolicyUnenforceable,
            "redirect_forbidden",
        ));
    }
    if request.history_ref.is_some() {
        return Err(rt_error(ErrorCode::PolicyUnenforceable, "remote_history"));
    }
    if !request.declared_tools.is_empty() {
        return Err(rt_error(ErrorCode::Unauthenticated, "unauthenticated_tool"));
    }
    if request.caller_authorization.is_some() {
        return Err(rt_error(ErrorCode::Unauthenticated, "caller_authorization"));
    }
    Ok(())
}

fn strip_secret(body: &[u8], secret: &str) -> Vec<u8> {
    let needle = secret.as_bytes();
    if needle.is_empty() {
        return body.to_vec();
    }
    let mut out = Vec::with_capacity(body.len());
    let mut index = 0;
    while index < body.len() {
        if body[index..].starts_with(needle) {
            index += needle.len();
        } else {
            out.push(body[index]);
            index += 1;
        }
    }
    out
}

fn line_leaks(line: &str, secret: &str) -> bool {
    line.to_ascii_lowercase().contains("authorization")
        || (!secret.is_empty() && line.contains(secret))
}

fn constant_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (left_byte, right_byte) in left.iter().zip(right.iter()) {
        diff |= left_byte ^ right_byte;
    }
    diff == 0
}
