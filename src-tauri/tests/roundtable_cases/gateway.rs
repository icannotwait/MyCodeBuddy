//! P06b host model gateway. Rejection and accounting run on every host.
//! A Unix socket that cannot be mounted is `not_tested`, not a pass.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use codeg_lib::roundtable::{
    forward_to_caller_target, probe_instance_socket, relay_from_helper, scopes_convert,
    AccountingSnapshot, ApprovedOrigin, ClientPolicy, EncodedModelRequest, ExecutionScope,
    GatewayLease, HostCredential, HostModelGateway, ModelRequest, SocketProbe, SANDBOX_ENDPOINT,
};
use roundtable_protocol::{
    Epoch, ErrorCode, Fence, Hash256, MonoMs, QualifiedContextProfile, Revision, ToolExchange,
};

const SECRET: &str = "Bearer sentinel-9f3c2a7b";
const RECIPIENT: &str = "gpt-test-approved";
const ORIGIN: &str = "https://models.example.test";
const POLICY_GENERATION: u64 = 3;
const EXPIRES: MonoMs = MonoMs(10_000);
const NOW: MonoMs = MonoMs(1_000);

fn parse_id<T: std::str::FromStr>(n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    format!("00000000-0000-4000-8000-{n:012x}")
        .parse()
        .expect("id")
}

fn profile() -> QualifiedContextProfile {
    let profile = QualifiedContextProfile::proposed(
        "test-tokenizer",
        Hash256::from_bytes([0x44; 32]),
        2_000_000,
        0,
        "proof-p06b",
    );
    assert_eq!(profile.max_model_requests, 64);
    assert_eq!(profile.max_tool_calls, 128);
    assert_eq!(profile.max_tool_reply_bytes, 131_072);
    assert_eq!(profile.max_attempt_generated_utf8_bytes, 262_144);
    assert_eq!(profile.max_request_body_bytes, 1_048_576);
    assert_eq!(profile.generation_reserve_tokens, 8_192);
    profile
}

fn fence(n: u8) -> Fence {
    Fence {
        boot_epoch: Epoch(1),
        run_epoch: Epoch(1),
        phase_id: parse_id(1),
        phase_revision: Revision(1),
        attempt_id: parse_id(n),
        binding_id: parse_id(2),
        incarnation: parse_id(3),
        context_hash: Hash256::from_bytes([0x21; 32]),
        policy_hash: Hash256::from_bytes([0x22; 32]),
    }
}

fn lease(n: u8, scope: ExecutionScope, expires: MonoMs) -> GatewayLease {
    let fence = fence(n);
    GatewayLease {
        attempt_id: fence.attempt_id,
        fence,
        recipient: RECIPIENT.to_string(),
        policy_generation: POLICY_GENERATION,
        expires_at_mono: expires,
        context_profile: profile(),
        scope,
    }
}

fn open_gateway() -> HostModelGateway {
    HostModelGateway::fake(
        ApprovedOrigin::parse(ORIGIN).expect("https origin"),
        HostCredential::injected(SECRET),
        ClientPolicy::approved(),
        POLICY_GENERATION,
    )
    .expect("fake gateway")
}

fn encoded(model: &str) -> EncodedModelRequest {
    EncodedModelRequest {
        model: model.to_string(),
        method: "POST".to_string(),
        path: "/v1/responses".to_string(),
        prior_output: Vec::new(),
        adapter_bytes: 16,
        history_ref: None,
        target_url: None,
        redirect_to: None,
        declared_tools: Vec::new(),
        tool_exchanges: Vec::new(),
        caller_authorization: None,
        submission_id: None,
    }
}

fn model_request(n: u8, scope: ExecutionScope, encoded: EncodedModelRequest) -> ModelRequest {
    ModelRequest {
        encoded,
        now: NOW,
        scope,
        fence: fence(n),
    }
}

fn reason(err: &roundtable_protocol::RtError) -> &str {
    err.details.reason.as_deref().unwrap_or("")
}

fn fits(snapshot_body: u64, profile: &QualifiedContextProfile) -> bool {
    snapshot_body
        .checked_add(profile.generation_reserve_tokens)
        .is_some_and(|total| total <= profile.model_capacity_tokens)
}

fn secret_leaked(text: &str) -> bool {
    text.contains(SECRET)
        || text.to_ascii_lowercase().contains("authorization")
        || text.contains("sentinel-9f3c2a7b")
}

#[test]
fn gateway_rejects_wrong_model_method_and_redirect() {
    assert_eq!(SANDBOX_ENDPOINT, "http://127.0.0.1:39173/v1");
    let probe = probe_instance_socket();
    assert!(!probe.is_passed());
    assert_ne!(probe.status(), "passed");
    if matches!(probe, SocketProbe::NotTested { .. }) {
        assert_eq!(probe.status(), "not_tested");
        let err = relay_from_helper(&probe).expect_err("unmounted socket");
        assert_eq!(err.code, ErrorCode::PolicyUnenforceable);
        assert_eq!(reason(&err), "socket_not_tested");
        assert!(!probe.is_passed(), "not_tested is not a mount pass");
    }
    for target in [
        "/tmp/evil.sock",
        "unix:///tmp/injected.sock",
        "http://169.254.169.254/",
        SANDBOX_ENDPOINT,
        "",
    ] {
        let err = forward_to_caller_target(target, b"POST /v1/responses").expect_err(target);
        assert_eq!(err.code, ErrorCode::PolicyUnenforceable);
        assert_eq!(reason(&err), "caller_supplied_target");
    }
    let after_refusal = probe_instance_socket();
    assert!(!after_refusal.is_passed());
    assert_ne!(after_refusal.status(), "passed");

    assert!(ApprovedOrigin::parse(ORIGIN).is_ok());
    let loopback = ApprovedOrigin::parse(SANDBOX_ENDPOINT).expect_err("sandbox endpoint");
    assert_eq!(reason(&loopback), "origin_not_https");
    let userinfo =
        ApprovedOrigin::parse("https://user:pass@models.example.test").expect_err("userinfo");
    assert_eq!(reason(&userinfo), "origin_userinfo");
    let with_path = ApprovedOrigin::parse("https://models.example.test/v1").expect_err("path");
    assert_eq!(reason(&with_path), "origin_path");
    assert!(ClientPolicy::approved().build_client().is_ok());
    let redirects = ClientPolicy::approved()
        .follow_redirects(true)
        .build_client()
        .expect_err("redirects");
    assert_eq!(reason(&redirects), "redirects_enabled");
    let proxy = ClientPolicy::approved()
        .proxy("http://127.0.0.1:9")
        .build_client()
        .expect_err("proxy");
    assert_eq!(reason(&proxy), "proxy_target");
    let certs = ClientPolicy::approved()
        .verify_certs(false)
        .build_client()
        .expect_err("certs");
    assert_eq!(reason(&certs), "certs_disabled");

    let gateway = open_gateway();
    let scope = ExecutionScope::Fake;
    let handle = gateway
        .register(lease(4, scope.clone(), EXPIRES))
        .expect("register");
    assert!(!handle.targets_arbitrary_url());
    assert!(!handle.socket_label().contains("://"));
    assert!(!handle.socket_label().contains("39173"));
    assert!(!handle.socket_label().contains(SECRET));

    let mut forwarded_before = gateway.forward_count();
    for method in [
        "GET", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "TRACE", "CONNECT", "post", "PRI",
    ] {
        let mut encoded = encoded(RECIPIENT);
        encoded.method = method.to_string();
        let err = gateway
            .handle(&handle, model_request(4, scope.clone(), encoded))
            .expect_err(method);
        assert_eq!(err.code, ErrorCode::PolicyUnenforceable, "{method}");
        assert_eq!(reason(&err), "g1_failed", "{method}");
        assert_eq!(gateway.forward_count(), forwarded_before, "{method}");
    }
    for path in [
        "/v1/chat/completions",
        "/v1/responses/",
        "/v1/responses?stream=1",
        "/v1/models",
        "http://evil.example/v1/responses",
        "https://models.example.test/v1/responses",
        SANDBOX_ENDPOINT,
        "/v1/responses/extra",
        "",
    ] {
        let mut encoded = encoded(RECIPIENT);
        encoded.path = path.to_string();
        let err = gateway
            .handle(&handle, model_request(4, scope.clone(), encoded))
            .expect_err(path);
        assert_eq!(err.code, ErrorCode::PolicyUnenforceable, "{path}");
        assert_eq!(reason(&err), "g1_failed", "{path}");
        assert_eq!(gateway.forward_count(), forwarded_before, "{path}");
    }

    let mut mismatch = encoded(RECIPIENT);
    mismatch.model = "other-model".to_string();
    let err = gateway
        .handle(&handle, model_request(4, scope.clone(), mismatch))
        .expect_err("model");
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(reason(&err), "model_mismatch");
    assert_eq!(gateway.forward_count(), forwarded_before);

    let mut arbitrary = encoded(RECIPIENT);
    arbitrary.target_url = Some("https://models.example.test/v1/responses".to_string());
    let err = gateway
        .handle(&handle, model_request(4, scope.clone(), arbitrary))
        .expect_err("url");
    assert_eq!(reason(&err), "arbitrary_url");
    assert_eq!(err.code, ErrorCode::PolicyUnenforceable);
    assert_eq!(gateway.forward_count(), forwarded_before);

    let mut redirect = encoded(RECIPIENT);
    redirect.redirect_to = Some("https://evil.example/steal".to_string());
    let err = gateway
        .handle(&handle, model_request(4, scope.clone(), redirect))
        .expect_err("redirect");
    assert_eq!(reason(&err), "redirect_forbidden");
    assert_eq!(gateway.forward_count(), forwarded_before);

    let mut history = encoded(RECIPIENT);
    history.history_ref = Some("resp_remote_123".to_string());
    let err = gateway
        .handle(&handle, model_request(4, scope.clone(), history))
        .expect_err("history");
    assert_eq!(reason(&err), "remote_history");
    assert_eq!(gateway.forward_count(), forwarded_before);

    for tool in [
        "web_search",
        "browser",
        "computer_use",
        "mcp",
        "file_search",
    ] {
        let mut encoded = encoded(RECIPIENT);
        encoded.declared_tools.push(tool.to_string());
        let err = gateway
            .handle(&handle, model_request(4, scope.clone(), encoded))
            .expect_err(tool);
        assert_eq!(err.code, ErrorCode::Unauthenticated, "{tool}");
        assert_eq!(reason(&err), "unauthenticated_tool", "{tool}");
        assert_eq!(gateway.forward_count(), forwarded_before, "{tool}");
    }

    let mut caller_auth = encoded(RECIPIENT);
    caller_auth.caller_authorization = Some(SECRET.to_string());
    let err = gateway
        .handle(&handle, model_request(4, scope.clone(), caller_auth))
        .expect_err("caller auth");
    assert_eq!(err.code, ErrorCode::Unauthenticated);
    assert_eq!(reason(&err), "caller_authorization");
    assert!(!secret_leaked(&err.to_string()));
    assert_eq!(gateway.forward_count(), forwarded_before);

    gateway.set_upstream_status(302);
    gateway.set_upstream_body(b"https://evil.example/next".to_vec());
    let err = gateway
        .handle(&handle, model_request(4, scope.clone(), encoded(RECIPIENT)))
        .expect_err("upstream redirect");
    assert_eq!(reason(&err), "redirect_forbidden");
    assert_eq!(gateway.forward_count(), forwarded_before + 1);
    forwarded_before = gateway.forward_count();
    assert_eq!(
        gateway.forwards().len(),
        forwarded_before,
        "redirect was not followed"
    );

    gateway.set_upstream_status(200);
    gateway.set_upstream_body(format!("ok {SECRET} trailer").into_bytes());
    let response = gateway
        .handle(&handle, model_request(4, scope.clone(), encoded(RECIPIENT)))
        .expect("qualified post");
    assert_eq!(gateway.forward_count(), forwarded_before + 1);
    let body = String::from_utf8(response.body.clone()).expect("utf8");
    assert!(!secret_leaked(&body));
    assert!(!secret_leaked(&response.origin));
    assert_eq!(response.origin, ORIGIN);
    assert_ne!(response.origin, SANDBOX_ENDPOINT);
    let observed = gateway.forwards().last().cloned().expect("forward");
    assert_eq!(observed.method, "POST");
    assert_eq!(observed.path, "/v1/responses");
    assert_eq!(observed.model, RECIPIENT);
    assert_eq!(observed.origin, ORIGIN);
    assert!(observed.credential_injected);
    assert!(!secret_leaked(&format!("{observed:?}")));
    for line in gateway.log_lines() {
        assert!(!secret_leaked(&line), "{line}");
    }
    assert!(!secret_leaked(&format!(
        "{:?}",
        HostCredential::injected(SECRET)
    )));
    assert_eq!(
        format!("{:?}", HostCredential::injected(SECRET)),
        "HostCredential([redacted])"
    );

    let other = open_gateway();
    let foreign = other
        .register(lease(4, ExecutionScope::Fake, EXPIRES))
        .expect("other");
    let err = gateway
        .handle(&foreign, model_request(4, scope, encoded(RECIPIENT)))
        .expect_err("foreign handle");
    assert_eq!(reason(&err), "lease_capability");
    assert_eq!(gateway.forward_count(), forwarded_before + 1);
}

#[test]
fn every_request_includes_prior_output_bounds() {
    let gateway = open_gateway();
    let scope = ExecutionScope::Fake;
    let handle = gateway
        .register(lease(5, scope.clone(), EXPIRES))
        .expect("register");

    let mut loose = lease(6, scope.clone(), EXPIRES);
    loose.context_profile.max_model_requests = 65;
    let err = gateway.register(loose).expect_err("raised request cap");
    assert_eq!(err.code, ErrorCode::CapabilityUnqualified);
    assert_eq!(reason(&err), "profile_over_cap");

    let mut shrunk = lease(6, scope.clone(), EXPIRES);
    shrunk.context_profile.generation_reserve_tokens = 8_191;
    let err = gateway.register(shrunk).expect_err("reserve");
    assert_eq!(reason(&err), "reserve_too_small");

    gateway.set_upstream_body(b"alpha-output".to_vec());
    let first = gateway
        .handle(&handle, model_request(5, scope.clone(), encoded(RECIPIENT)))
        .expect("first");
    assert_eq!(first.permit.prior_output_bytes(), 0);
    assert!(fits(first.permit.body_bytes(), &profile()));
    assert_eq!(gateway.forwards()[0].prior_output, b"");

    let missing = gateway
        .handle(&handle, model_request(5, scope.clone(), encoded(RECIPIENT)))
        .expect_err("omitted prior");
    assert_eq!(reason(&missing), "prior_output_missing");
    assert_eq!(gateway.forward_count(), 1);

    let mut with_prior = encoded(RECIPIENT);
    with_prior.prior_output = b"alpha-output".to_vec();
    let second = gateway
        .handle(&handle, model_request(5, scope.clone(), with_prior))
        .expect("prior included");
    assert_eq!(
        second.permit.prior_output_bytes(),
        b"alpha-output".len() as u64
    );
    assert!(second.permit.body_bytes() >= second.permit.prior_output_bytes());
    assert!(fits(second.permit.body_bytes(), &profile()));
    assert_eq!(gateway.forwards()[1].prior_output, b"alpha-output");
    assert_eq!(second.origin, ORIGIN);

    let mut oversized_body = encoded(RECIPIENT);
    oversized_body.prior_output = b"alpha-outputalpha-output".to_vec();
    oversized_body.adapter_bytes = 1_048_576;
    let err = gateway
        .handle(&handle, model_request(5, scope.clone(), oversized_body))
        .expect_err("body cap");
    assert_eq!(err.code, ErrorCode::ContextTooLarge);
    assert_eq!(reason(&err), "context_too_large");
    assert_eq!(gateway.forward_count(), 2);

    let capped = open_gateway();
    let capped_handle = capped
        .register(lease(19, scope.clone(), EXPIRES))
        .expect("generated cap");
    capped.set_upstream_body(vec![b'x'; 262_144]);
    let accepted = capped
        .handle(
            &capped_handle,
            model_request(19, scope.clone(), encoded(RECIPIENT)),
        )
        .expect("generate to the cap");
    assert_eq!(accepted.permit.generated_utf8_bytes(), 0);
    assert!(fits(accepted.permit.body_bytes(), &profile()));
    let snap = capped
        .accounting(capped_handle.attempt_id())
        .expect("accounting");
    assert_eq!(snap.generated_utf8_bytes, 262_144);
    assert_eq!(snap.prior_output_bytes, 0);
    assert_eq!(snap.model_requests, 1);

    capped.set_upstream_body(b"y".to_vec());
    let mut overflow = encoded(RECIPIENT);
    overflow.prior_output = vec![b'x'; 262_144];
    let err = capped
        .handle(&capped_handle, model_request(19, scope.clone(), overflow))
        .expect_err("generated cap");
    assert_eq!(err.code, ErrorCode::ContextTooLarge);
    assert_eq!(reason(&err), "generated_utf8_limit");
    let snap = capped
        .accounting(capped_handle.attempt_id())
        .expect("accounting");
    assert_eq!(snap.generated_utf8_bytes, 262_144);
    assert_eq!(snap.model_requests, 2);
    let mut omitted = encoded(RECIPIENT);
    omitted.prior_output = Vec::new();
    let err = capped
        .handle(&capped_handle, model_request(19, scope.clone(), omitted))
        .expect_err("later request still includes the accepted output");
    assert_eq!(reason(&err), "prior_output_missing");
    assert_eq!(capped.forward_count(), 2);

    let fresh = open_gateway();
    let fresh_handle = fresh
        .register(lease(7, scope.clone(), EXPIRES))
        .expect("fresh");
    fresh.set_upstream_body(Vec::new());
    for index in 1..=64 {
        let response = fresh
            .handle(
                &fresh_handle,
                model_request(7, scope.clone(), encoded(RECIPIENT)),
            )
            .unwrap_or_else(|err| panic!("request {index} forwarded: {err}"));
        assert!(fits(response.permit.body_bytes(), &profile()));
        assert_eq!(response.permit.model_requests(), index);
    }
    assert_eq!(fresh.forward_count(), 64);
    let err = fresh
        .handle(
            &fresh_handle,
            model_request(7, scope.clone(), encoded(RECIPIENT)),
        )
        .expect_err("request 65");
    assert_eq!(err.code, ErrorCode::ContextTooLarge);
    assert_eq!(reason(&err), "context_too_large");
    assert_eq!(fresh.forward_count(), 64);
    assert_eq!(
        fresh
            .accounting(fresh_handle.attempt_id())
            .expect("snap")
            .model_requests,
        64
    );

    let inflight = Arc::new(open_gateway());
    let inflight_handle = inflight
        .register(lease(8, scope.clone(), EXPIRES))
        .expect("inflight");
    inflight.set_upstream_body(b"held".to_vec());
    let hold = inflight.arm_hold();
    let worker = Arc::clone(&inflight);
    let worker_handle = inflight_handle.clone();
    let worker_request = model_request(8, scope.clone(), encoded(RECIPIENT));
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let result = worker.handle(&worker_handle, worker_request);
        done_tx.send(result.map(|_| ())).ok();
    });
    assert!(
        hold.wait_entered(Duration::from_secs(5)),
        "first forward did not start"
    );
    let err = inflight
        .handle(
            &inflight_handle,
            model_request(8, scope, encoded(RECIPIENT)),
        )
        .expect_err("second forward");
    assert_eq!(err.code, ErrorCode::CommandInProgress);
    assert_eq!(reason(&err), "attempt_inflight");
    assert_eq!(inflight.forward_count(), 1);
    hold.release();
    let first = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("first finished");
    assert!(first.is_ok(), "{first:?}");
    assert_eq!(inflight.forward_count(), 1);
}

#[test]
fn all_tool_envelopes_count_toward_limit() {
    let gateway = open_gateway();
    let scope = ExecutionScope::Fake;
    let handle = gateway
        .register(lease(9, scope.clone(), EXPIRES))
        .expect("register");
    gateway.set_upstream_body(Vec::new());
    let profile = profile();

    let field = ToolExchange::submit_field_errors("summary", "missing");
    assert!(!field.reply.is_empty());
    let receipt = ToolExchange::receipt("sub-1");
    assert!(!receipt.reply.is_empty());
    let empty = ToolExchange::search_empty();
    let failed = ToolExchange::search_error();
    assert!(!empty.reply.is_empty());
    assert!(!failed.reply.is_empty());

    let mut reply_bytes = 0_u64;
    let mut expected_calls = 0_u32;
    for (index, exchange) in [field.clone(), field.clone(), field.clone()]
        .into_iter()
        .enumerate()
    {
        reply_bytes += exchange.reply.len() as u64;
        expected_calls += 1;
        let response = send_exchange(&gateway, &handle, &scope, 9, exchange, None);
        assert_eq!(response.permit.tool_calls(), expected_calls);
        assert_eq!(response.permit.model_requests(), expected_calls);
        assert_eq!(response.permit.tool_reply_bytes(), reply_bytes);
        assert!(
            fits(response.permit.body_bytes(), &profile),
            "repair {index}"
        );
        assert_eq!(gateway.forward_count() as u32, expected_calls);
    }

    let valid = send_exchange(&gateway, &handle, &scope, 9, receipt.clone(), Some("sub-1"));
    reply_bytes += receipt.reply.len() as u64;
    expected_calls += 1;
    assert_eq!(valid.permit.tool_calls(), expected_calls);
    assert!(fits(valid.permit.body_bytes(), &profile));
    let candidate = valid.candidate_id.clone().expect("candidate");

    let duplicate = send_exchange(&gateway, &handle, &scope, 9, receipt.clone(), Some("sub-1"));
    reply_bytes += receipt.reply.len() as u64;
    expected_calls += 1;
    assert_eq!(duplicate.candidate_id.as_deref(), Some(candidate.as_str()));
    assert_eq!(duplicate.permit.tool_calls(), expected_calls);
    assert_eq!(duplicate.permit.tool_reply_bytes(), reply_bytes);
    assert_eq!(gateway.forward_count() as u32, expected_calls);
    assert!(fits(duplicate.permit.body_bytes(), &profile));

    let empty_response = send_exchange(&gateway, &handle, &scope, 9, empty.clone(), None);
    reply_bytes += empty.reply.len() as u64;
    expected_calls += 1;
    assert!(empty_response.permit.tool_reply_bytes() > reply_bytes - empty.reply.len() as u64);
    assert_eq!(empty_response.permit.tool_calls(), expected_calls);
    assert!(fits(empty_response.permit.body_bytes(), &profile));

    let field_again = send_exchange(&gateway, &handle, &scope, 9, field.clone(), None);
    reply_bytes += field.reply.len() as u64;
    expected_calls += 1;
    assert_eq!(field_again.permit.tool_reply_bytes(), reply_bytes);
    assert_eq!(field_again.permit.tool_calls(), expected_calls);
    assert!(fits(field_again.permit.body_bytes(), &profile));
    assert_eq!(expected_calls, 7);

    let failed_response = send_exchange(&gateway, &handle, &scope, 9, failed, None);
    assert!(failed_response.permit.tool_calls() > expected_calls);
    assert!(fits(failed_response.permit.body_bytes(), &profile));
    expected_calls = failed_response.permit.tool_calls();

    let remaining = 128 - expected_calls;
    let mut batch = encoded(RECIPIENT);
    batch.tool_exchanges = (0..remaining).map(|_| empty.clone()).collect();
    let filled = gateway
        .handle(&handle, model_request(9, scope.clone(), batch))
        .expect("fill to 128");
    assert_eq!(filled.permit.tool_calls(), 128);
    assert!(filled.permit.model_requests() < 64);
    assert!(fits(filled.permit.body_bytes(), &profile));
    let forwarded = gateway.forward_count();

    let mut one_more = encoded(RECIPIENT);
    one_more.tool_exchanges = vec![empty];
    let err = gateway
        .handle(&handle, model_request(9, scope.clone(), one_more))
        .expect_err("tool call 129");
    assert_eq!(err.code, ErrorCode::ContextTooLarge);
    assert_eq!(reason(&err), "context_too_large");
    assert_eq!(gateway.forward_count(), forwarded);
    let snap: AccountingSnapshot = gateway.accounting(handle.attempt_id()).expect("snap");
    assert_eq!(snap.tool_calls, 128);
    assert!(snap.tool_reply_bytes >= reply_bytes);

    let replies = open_gateway();
    let replies_handle = replies
        .register(lease(10, scope.clone(), EXPIRES))
        .expect("replies");
    replies.set_upstream_body(Vec::new());
    let mut full = encoded(RECIPIENT);
    full.tool_exchanges = vec![ToolExchange {
        arguments: Vec::new(),
        reply: vec![b'r'; 131_072],
        generated_utf8_bytes: 0,
        evidence_bytes: 0,
        model_requests: 1,
        tool_calls: 1,
    }];
    let ok = replies
        .handle(&replies_handle, model_request(10, scope.clone(), full))
        .expect("128 kib");
    assert_eq!(ok.permit.tool_reply_bytes(), 131_072);
    let mut extra = encoded(RECIPIENT);
    extra.tool_exchanges = vec![ToolExchange {
        arguments: Vec::new(),
        reply: vec![b'r'],
        generated_utf8_bytes: 0,
        evidence_bytes: 0,
        model_requests: 1,
        tool_calls: 1,
    }];
    let err = replies
        .handle(&replies_handle, model_request(10, scope.clone(), extra))
        .expect_err("reply cap");
    assert_eq!(err.code, ErrorCode::ContextTooLarge);
    assert_eq!(replies.forward_count(), 1);

    let evidence_gateway = open_gateway();
    let evidence_handle = evidence_gateway
        .register(lease(11, scope.clone(), EXPIRES))
        .expect("evidence");
    evidence_gateway.set_upstream_body(Vec::new());
    let err = evidence_gateway
        .handle(
            &evidence_handle,
            exchange_request(11, &scope, ToolExchange::evidence(8_193), None),
        )
        .expect_err("evidence reply");
    assert_eq!(reason(&err), "evidence_reply_limit");
    assert_eq!(evidence_gateway.forward_count(), 0);
    for _ in 0..4 {
        evidence_gateway
            .handle(
                &evidence_handle,
                exchange_request(11, &scope, ToolExchange::evidence(8_192), None),
            )
            .expect("evidence chunk");
    }
    assert_eq!(evidence_gateway.forward_count(), 4);
    let err = evidence_gateway
        .handle(
            &evidence_handle,
            exchange_request(11, &scope, ToolExchange::evidence(1), None),
        )
        .expect_err("evidence attempt");
    assert_eq!(reason(&err), "evidence_attempt_limit");
    assert_eq!(evidence_gateway.forward_count(), 4);
    assert_eq!(
        evidence_gateway
            .accounting(evidence_handle.attempt_id())
            .expect("evidence snap")
            .evidence_attempt_bytes,
        32_768
    );
}

fn send_exchange(
    gateway: &HostModelGateway,
    handle: &codeg_lib::roundtable::GatewayHandle,
    scope: &ExecutionScope,
    n: u8,
    exchange: ToolExchange,
    submission_id: Option<&str>,
) -> codeg_lib::roundtable::ModelResponse {
    gateway
        .handle(handle, exchange_request(n, scope, exchange, submission_id))
        .expect("exchange")
}

fn exchange_request(
    n: u8,
    scope: &ExecutionScope,
    exchange: ToolExchange,
    submission_id: Option<&str>,
) -> ModelRequest {
    let mut encoded = encoded(RECIPIENT);
    encoded.tool_exchanges = vec![exchange];
    encoded.submission_id = submission_id.map(str::to_string);
    model_request(n, scope.clone(), encoded)
}

#[test]
fn revoked_or_expired_lease_never_forwards() {
    let gateway = open_gateway();
    let fake = ExecutionScope::Fake;
    let qualification = ExecutionScope::Qualification {
        approval_id: "approval-1".to_string(),
        expires_at: EXPIRES,
        attempt_limit: 2,
        spend_limit: 50,
        recipient: RECIPIENT.to_string(),
        fixture_hash: Hash256::from_bytes([0x11; 32]),
    };
    let product = ExecutionScope::Product {
        rollout_generation: POLICY_GENERATION,
    };
    assert!(!scopes_convert(&fake, &qualification));
    assert!(!scopes_convert(&qualification, &product));
    assert!(!scopes_convert(&product, &fake));
    assert!(!scopes_convert(&fake, &fake));

    let fake_handle = gateway
        .register(lease(12, fake.clone(), EXPIRES))
        .expect("fake");
    let qual_handle = gateway
        .register(lease(13, qualification.clone(), EXPIRES))
        .expect("qualification");
    let prod_handle = gateway
        .register(lease(14, product.clone(), EXPIRES))
        .expect("product");
    assert!(matches!(
        gateway.scope_of(&fake_handle).expect("scope"),
        ExecutionScope::Fake
    ));
    assert!(matches!(
        gateway.scope_of(&qual_handle).expect("scope"),
        ExecutionScope::Qualification { .. }
    ));
    assert!(matches!(
        gateway.scope_of(&prod_handle).expect("scope"),
        ExecutionScope::Product { .. }
    ));

    let mut crossed = model_request(12, product.clone(), encoded(RECIPIENT));
    crossed.scope = product.clone();
    let err = gateway
        .handle(&fake_handle, crossed)
        .expect_err("cross scope");
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert_eq!(reason(&err), "scope_mismatch");
    assert_eq!(gateway.forward_count(), 0);

    let mut wrong_generation = lease(15, product.clone(), EXPIRES);
    wrong_generation.policy_generation = POLICY_GENERATION + 1;
    let err = gateway.register(wrong_generation).expect_err("generation");
    assert_eq!(reason(&err), "generation_mismatch");
    let mut wrong_rollout = lease(15, product, EXPIRES);
    wrong_rollout.scope = ExecutionScope::Product {
        rollout_generation: 0,
    };
    let err = gateway.register(wrong_rollout).expect_err("rollout");
    assert_eq!(reason(&err), "generation_mismatch");

    gateway
        .handle(
            &qual_handle,
            model_request(13, qualification, encoded(RECIPIENT)),
        )
        .expect("qualification forward");
    assert_eq!(gateway.forwards().last().expect("qual").origin, ORIGIN);
    assert_eq!(gateway.forwards().last().expect("qual").model, RECIPIENT);

    let live = open_gateway();
    let keep = live
        .register(lease(16, fake.clone(), EXPIRES))
        .expect("keep");
    let drop_handle = live
        .register(lease(17, fake.clone(), EXPIRES))
        .expect("drop");
    live.revoke(drop_handle.attempt_id()).expect("revoke");
    let err = live
        .handle(
            &drop_handle,
            model_request(17, fake.clone(), encoded(RECIPIENT)),
        )
        .expect_err("revoked");
    assert_eq!(err.code, ErrorCode::CapabilityUnqualified);
    assert_eq!(reason(&err), "lease_revoked");
    assert_eq!(live.forward_count(), 0);
    live.handle(&keep, model_request(16, fake.clone(), encoded(RECIPIENT)))
        .expect("sibling still forwards");
    assert_eq!(live.forward_count(), 1);
    assert!(!secret_leaked(&live.log_lines().join("\n")));

    let expiring = open_gateway();
    let expiring_handle = expiring
        .register(lease(18, fake.clone(), MonoMs(1_000)))
        .expect("expiring");
    let called = Arc::new(AtomicBool::new(false));
    let called_probe = Arc::clone(&called);
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let pair_probe = Arc::clone(&pair);
    expiring.set_db_probe(Some(Arc::new(move || {
        called_probe.store(true, Ordering::SeqCst);
        let (lock, cv) = &*pair_probe;
        let mut ready = lock.lock().expect("probe lock");
        while !*ready {
            ready = cv.wait(ready).expect("probe wait");
        }
    })));
    let worker = Arc::new(expiring);
    let worker_handle = expiring_handle.clone();
    let mut expired_request = model_request(18, fake.clone(), encoded(RECIPIENT));
    expired_request.now = MonoMs(1_000);
    let (done_tx, done_rx) = mpsc::channel();
    let runner = Arc::clone(&worker);
    thread::spawn(move || {
        let result = runner.handle(&worker_handle, expired_request);
        done_tx.send(result).ok();
    });
    let expired = done_rx
        .recv_timeout(Duration::from_millis(500))
        .expect("expiry waited on the database");
    let err = expired.expect_err("expired");
    assert_eq!(err.code, ErrorCode::CapabilityUnqualified);
    assert_eq!(reason(&err), "lease_expired");
    assert!(!called.load(Ordering::SeqCst));
    assert_eq!(worker.forward_count(), 0);
    let mut earlier = model_request(18, fake, encoded(RECIPIENT));
    earlier.now = MonoMs(0);
    let err = worker
        .handle(&expiring_handle, earlier)
        .expect_err("stays revoked");
    assert_eq!(reason(&err), "lease_revoked");
    assert_eq!(worker.forward_count(), 0);
    assert!(!called.load(Ordering::SeqCst));
    {
        let (lock, cv) = &*pair;
        *lock.lock().expect("release") = true;
        cv.notify_all();
    }
}
