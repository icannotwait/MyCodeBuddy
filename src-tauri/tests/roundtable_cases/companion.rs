//! P07c service companion and the shared three-tool core.
//!
//! Legacy parent mode keeps its existing tool groups. Service mode advertises
//! and calls only `read_evidence`, `search_evidence`, and `submit_result`.
//! A fake env or broker EOF is not a qualification certificate.

use std::collections::BTreeMap;
use std::str::FromStr;

use codeg_lib::roundtable::{
    advertised_tools, bind_service_process, callable_tools, dispatch_tool,
    legacy_companion_context, parse_companion_args, plan_service_launch, service_result_schema,
    service_tool_names, service_tool_schema, service_tools_ignoring_host_flags, tool_callable,
    CompanionMode, CompanionParse, FakeBrokerTransport, InMemoryToolStore, QualificationHarness,
    RoundtableToolCall, ServiceLaunchInput, ServiceWatchState, TokenBinding, TokenRegistry,
    ATTEMPT_TOKEN_ENV, SERVICE_RESULT_SCHEMA_ID, SERVICE_TOOL_VERSION,
};
use roundtable_protocol::{
    canonical_bytes, submit_candidate, AliasVisibility, BindingId, CandidateState, DeliveryEncoder,
    Epoch, ErrorCode, EvidenceId, EvidenceRef, Fence, Hash256, ObjectKind, ObjectRefV1, PhaseId,
    PhaseKind, QualificationStatus, QualifiedContextProfile, ResultScope, Revision, RoomId,
    SafeInt, SpeakerId, SubmissionId, TokenBound, VisibleAliases, SCHEMA_VERSION,
};
use serde_json::{json, Value};

const BILLING: &str = "sk-billing-SENTINEL-9f3c";
const SOCKET_A: &str = r"\\.\pipe\roundtable-instance-a";
const SOCKET_B: &str = r"\\.\pipe\roundtable-instance-b";
const HOST_FLAGS: &[&str] = &[
    "browser",
    "browser_eval",
    "computer",
    "computer_launch",
    "computer_clipboard",
    "delegation",
    "sessions",
    "ask",
    "feedback",
    "tasks",
    "automations",
    "taskboard",
];

struct Utf8Tokens {
    capacity: u64,
}

impl TokenBound for Utf8Tokens {
    fn upper_bound(&self, utf8: &[u8]) -> roundtable_protocol::RtResult<u64> {
        Ok(utf8.len() as u64)
    }

    fn capacity_tokens(&self) -> Option<u64> {
        Some(self.capacity)
    }
}

fn id_text(n: u8) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn id<T: FromStr>(n: u8) -> T
where
    T::Err: std::fmt::Debug,
{
    id_text(n).parse().expect("id")
}

fn reason(err: &roundtable_protocol::RtError) -> &str {
    err.details.reason.as_deref().unwrap_or("")
}

fn fence(attempt: u8) -> Fence {
    Fence {
        boot_epoch: Epoch(7),
        run_epoch: Epoch(7),
        phase_id: id::<PhaseId>(1),
        phase_revision: Revision(1),
        attempt_id: id(attempt),
        binding_id: id::<BindingId>(3),
        incarnation: id(4),
        context_hash: Hash256::from_bytes([0x11; 32]),
        policy_hash: Hash256::from_bytes([0x22; 32]),
    }
}

fn profile() -> QualifiedContextProfile {
    QualifiedContextProfile::proposed(
        "test-tokenizer",
        Hash256::from_bytes([0x44; 32]),
        2_000_000,
        0,
        "proof-p07c",
    )
}

fn evidence_text() -> &'static str {
    "alpha\naxb\na.b\nsearch-me"
}

fn object_ref() -> ObjectRefV1 {
    let bytes = evidence_text().as_bytes();
    ObjectRefV1 {
        object_id: "obj-e0".to_string(),
        kind: ObjectKind::SourceExcerpt,
        content_hash: Hash256::sha256(bytes),
        total_bytes: SafeInt(bytes.len() as u64),
    }
}

fn aliases() -> VisibleAliases {
    let mut aliases = VisibleAliases::default();
    aliases.evidence.insert(
        "e0".to_string(),
        EvidenceRef {
            evidence_id: id::<EvidenceId>(9),
            visibility: AliasVisibility::Published,
        },
    );
    aliases
}

fn binding(attempt: u8) -> TokenBinding {
    let speaker = id::<SpeakerId>(8);
    let mut evidence = BTreeMap::new();
    evidence.insert("e0".to_string(), object_ref());
    TokenBinding {
        attempt_id: id(attempt),
        room_id: id::<RoomId>(5),
        fence: fence(attempt),
        speaker_id: speaker,
        tool_version: SERVICE_TOOL_VERSION.to_string(),
        aliases: aliases(),
        result_scope: ResultScope {
            phase_kind: PhaseKind::Proposal,
            speaker_id: speaker,
            aliases: aliases(),
            mandatory_targets: Vec::new(),
            published: roundtable_protocol::PublishedHistory::default(),
            quota_bytes: 8_192,
        },
        evidence,
        profile: profile(),
    }
}

fn legacy_mode() -> CompanionMode {
    let parsed = parse_companion_args([
        "--parent-connection-id",
        "parent-connection",
        "--socket-path",
        SOCKET_A,
        "--token",
        "legacy-token",
        "--parent-pid",
        "4242",
        "--features",
        "browser,browser_eval,computer,computer_launch,computer_clipboard,delegation",
        "--role",
        "root",
    ])
    .expect("legacy args");
    match parsed {
        CompanionParse::Launch(mode) => mode,
        CompanionParse::Help(_) => panic!("legacy parse returned help"),
    }
}

fn service_mode() -> CompanionMode {
    let parsed = parse_companion_args([
        "--service-roundtable",
        "--socket-path",
        SOCKET_A,
        "--incarnation",
        &id_text(4),
    ])
    .expect("service args");
    match parsed {
        CompanionParse::Launch(mode) => mode,
        CompanionParse::Help(_) => panic!("service parse returned help"),
    }
}

fn proposal() -> Value {
    json!({
        "kind": "proposal",
        "summary": "成员摘要",
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": [],
            "confidence": "low"
        }]
    })
}

fn bad_proposal() -> Value {
    json!({
        "kind": "proposal",
        "summary": "empty",
        "claims": []
    })
}

fn sid(text: &str) -> SubmissionId {
    SubmissionId::from_str(text).expect(text)
}

fn assert_no_secret(text: &str, secret: &str) {
    assert!(!text.contains(secret), "secret leaked");
    assert!(!text.contains(&secret[..8]), "secret prefix leaked");
    assert!(!text.contains(BILLING), "billing credential leaked");
}

#[tokio::test]
async fn companion_service_mode_has_exactly_three_tools() {
    let legacy = legacy_mode();
    let CompanionMode::LegacyParent(legacy_args) = &legacy else {
        panic!("legacy mode was replaced");
    };
    assert_eq!(legacy_args.parent_pid, Some(4242));
    let legacy_context = legacy_companion_context(legacy_args);
    assert!(legacy_context.allows_tool("browser_list_tabs"));
    assert!(legacy_context.allows_tool("browser_eval"));
    assert!(legacy_context.allows_tool("computer_screenshot"));
    assert!(legacy_context.allows_tool("computer_launch_app"));
    assert!(legacy_context.allows_tool("computer_clipboard_read"));
    assert!(legacy_context.allows_tool("delegate_to_agent"));
    let legacy_advertised = advertised_tools(&legacy);
    for name in [
        "browser_list_tabs",
        "browser_eval",
        "computer_screenshot",
        "computer_launch_app",
        "computer_clipboard_read",
        "delegate_to_agent",
    ] {
        assert!(tool_callable(&legacy, name), "{name}");
        assert!(legacy_advertised.iter().any(|tool| tool == name), "{name}");
    }

    let rejected = parse_companion_args([
        "--service-roundtable",
        "--socket-path",
        SOCKET_A,
        "--incarnation",
        &id_text(4),
        "--token",
        "argv-token-sentinel",
        "--features",
        "browser,computer,delegation",
    ])
    .expect_err("service argv must not carry a token or legacy groups");
    assert!(!rejected.contains("argv-token-sentinel"));

    let service = service_mode();
    let expected = service_tool_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        expected,
        vec![
            "read_evidence".to_string(),
            "search_evidence".to_string(),
            "submit_result".to_string()
        ]
    );
    assert_eq!(advertised_tools(&service), expected);
    assert_eq!(callable_tools(&service), expected);
    assert_eq!(service_tools_ignoring_host_flags(HOST_FLAGS), expected);
    for name in &expected {
        assert!(tool_callable(&service, name), "{name}");
    }
    for name in [
        "browser_list_tabs",
        "browser_eval",
        "computer_screenshot",
        "computer_launch_app",
        "computer_clipboard_read",
        "computer_clipboard_write",
        "delegate_to_agent",
        "get_session_info",
        "ask_user_question",
        "check_user_feedback",
        "create_work_task",
        "create_automation",
        "task_progress",
        "shell",
        "write_file",
    ] {
        assert!(!tool_callable(&service, name), "{name}");
        assert!(!advertised_tools(&service).iter().any(|tool| tool == name));
    }

    let store = InMemoryToolStore::new();
    store
        .insert_object(&object_ref(), evidence_text().as_bytes())
        .expect("object");
    let registry = TokenRegistry::new();
    let live = binding(2);
    let token = registry.issue(live.clone());
    let secret = token.reveal_for_same_sandbox().to_string();
    let scope = registry
        .admit(&secret, &live, "read_evidence")
        .expect("admit read");
    let read = dispatch_tool(
        &scope,
        RoundtableToolCall {
            name: "read_evidence".to_string(),
            arguments: json!({
                "file_alias": "e0",
                "start_line": 1,
                "end_line": 1
            }),
        },
        &store,
    )
    .await
    .expect("read");
    let read_body: Value = serde_json::from_slice(&read.body).expect("read json");
    assert_eq!(read_body["excerpt"], json!("alpha"));
    assert_eq!(read_body["total_lines"], json!(4));
    assert_eq!(read_body["has_more"], json!(true));

    let search = dispatch_tool(
        &scope,
        RoundtableToolCall {
            name: "search_evidence".to_string(),
            arguments: json!({
                "file_alias": "e0",
                "query": "a.b",
                "limit": 10
            }),
        },
        &store,
    )
    .await
    .expect("search");
    let search_body: Value = serde_json::from_slice(&search.body).expect("search json");
    assert_eq!(search_body["literal"], json!(true));
    assert_eq!(search_body["hits"].as_array().expect("hits").len(), 1);
    assert_eq!(search_body["hits"][0]["line"], json!(3));
    assert_eq!(search_body["hits"][0]["text"], json!("a.b"));

    let submitted = dispatch_tool(
        &scope,
        RoundtableToolCall {
            name: "submit_result".to_string(),
            arguments: json!({
                "submission_id": "seal-1",
                "result": proposal()
            }),
        },
        &store,
    )
    .await
    .expect("submit");
    let receipt = submitted.receipt.expect("receipt");
    assert_eq!(receipt.state, CandidateState::Staged);
    assert_eq!(receipt.submission_id, sid("seal-1"));

    let browser = dispatch_tool(
        &scope,
        RoundtableToolCall {
            name: "browser_list_tabs".to_string(),
            arguments: json!({}),
        },
        &store,
    )
    .await
    .expect_err("browser tool");
    assert_eq!(browser.code, ErrorCode::Forbidden);
    assert_eq!(reason(&browser), "tool_not_admitted");
    assert!(!format!("{browser:?}").contains(&secret));
}

#[tokio::test]
async fn token_is_attempt_scoped_even_if_agent_reads_it() {
    let registry = TokenRegistry::new();
    let live = binding(2);
    let token = registry.issue(live.clone());
    let secret = token.reveal_for_same_sandbox().to_string();
    assert!(secret.len() >= 32);
    assert_eq!(format!("{token:?}"), "AttemptToken([redacted])");

    let plan = plan_service_launch(&ServiceLaunchInput {
        token: &token,
        socket_path: SOCKET_A,
        incarnation: &id_text(4),
        billing_credential: BILLING,
    });
    assert_eq!(
        plan.sandbox_env.get(ATTEMPT_TOKEN_ENV).map(String::as_str),
        Some(secret.as_str())
    );
    assert_eq!(plan.sandbox_env.len(), 1);
    assert!(!plan
        .argv
        .iter()
        .any(|arg| arg == "--token" || arg == "--parent-pid"));
    assert!(!plan.argv.iter().any(|arg| arg.contains(&secret)));
    assert_no_secret(&plan.log, &secret);
    assert_no_secret(&plan.prompt, &secret);
    assert_no_secret(&plan.request_url, &secret);
    assert_no_secret(&format!("{plan:?}"), &secret);
    assert!(!plan
        .sandbox_env
        .values()
        .any(|value| value.contains(BILLING)));

    let scope = registry
        .admit(&secret, &live, "read_evidence")
        .expect("same attempt");
    let store = InMemoryToolStore::new();
    let switched = dispatch_tool(
        &scope,
        RoundtableToolCall {
            name: "read_evidence".to_string(),
            arguments: json!({
                "file_alias": "e0",
                "start_line": 1,
                "end_line": 1,
                "room_id": id_text(1),
                "attempt_id": id_text(9)
            }),
        },
        &store,
    )
    .await
    .expect_err("identity argument");
    assert_eq!(switched.code, ErrorCode::Forbidden);
    assert_eq!(reason(&switched), "identity_not_selectable");
    assert!(!format!("{switched:?}").contains(&secret));

    let mut other_attempt = live.clone();
    other_attempt.attempt_id = id(9);
    let attempt = registry
        .admit(&secret, &other_attempt, "read_evidence")
        .expect_err("attempt");
    assert_eq!(attempt.code, ErrorCode::Forbidden);
    assert_eq!(reason(&attempt), "attempt_mismatch");

    let mut other_room = live.clone();
    other_room.room_id = id::<RoomId>(9);
    let room = registry
        .admit(&secret, &other_room, "search_evidence")
        .expect_err("room");
    assert_eq!(room.code, ErrorCode::Forbidden);
    assert_eq!(reason(&room), "room_mismatch");

    let mut other_fence = live.clone();
    other_fence.fence.policy_hash = Hash256::from_bytes([0x33; 32]);
    let fence_err = registry
        .admit(&secret, &other_fence, "submit_result")
        .expect_err("fence");
    assert_eq!(fence_err.code, ErrorCode::Forbidden);
    assert_eq!(reason(&fence_err), "fence_mismatch");

    let mut other_version = live.clone();
    other_version.tool_version.push_str("-other");
    let version = registry
        .admit(&secret, &other_version, "read_evidence")
        .expect_err("tool version");
    assert_eq!(reason(&version), "tool_version_mismatch");

    let mut other_aliases = live.clone();
    other_aliases.aliases.evidence.insert(
        "e1".to_string(),
        EvidenceRef {
            evidence_id: id::<EvidenceId>(1),
            visibility: AliasVisibility::PeerStaged,
        },
    );
    let alias = registry
        .admit(&secret, &other_aliases, "read_evidence")
        .expect_err("aliases");
    assert_eq!(reason(&alias), "alias_mismatch");

    let other_tool = registry
        .admit(&secret, &live, "browser_list_tabs")
        .expect_err("other tool");
    assert_eq!(other_tool.code, ErrorCode::Forbidden);
    assert_eq!(reason(&other_tool), "tool_not_admitted");

    registry.revoke(&secret).expect("revoke");
    let revoked = registry
        .admit(&secret, &live, "read_evidence")
        .expect_err("revoked");
    assert_eq!(revoked.code, ErrorCode::Unauthenticated);
    assert_eq!(reason(&revoked), "token_revoked");
    assert!(!format!("{revoked:?}{attempt:?}{room:?}{fence_err:?}").contains(&secret));
}

#[tokio::test]
async fn service_watchdog_uses_socket_eof() {
    let source = include_str!("../../src/roundtable/companion.rs");
    let tool_core = include_str!("../../src/roundtable/tool_core.rs");
    let harness_source = include_str!("../../src/roundtable/qualification_harness.rs");
    for source in [source, tool_core, harness_source] {
        assert!(!source.contains("parent_alive"));
        assert!(!source.contains("wait_for_parent_exit"));
        assert!(!source.contains("OpenProcess"));
        assert!(!source.contains("GetCurrentProcessId"));
    }

    let parent_pid = parse_companion_args([
        "--service-roundtable",
        "--socket-path",
        SOCKET_A,
        "--incarnation",
        &id_text(4),
        "--parent-pid",
        "4242",
    ])
    .expect_err("service mode does not accept a host parent pid");
    assert!(!parent_pid.contains("4242"));
    assert!(legacy_mode_parent_pid_still_parses());

    let transport = FakeBrokerTransport::open(SOCKET_A, id_text(4));
    let other = FakeBrokerTransport::open(SOCKET_B, id_text(5));
    let watch = transport.watch();
    assert!(!watch.reads_host_parent_pid());
    assert_eq!(watch.parent_pid(), None);
    assert_eq!(watch.socket_path(), SOCKET_A);
    assert_eq!(watch.incarnation(), id_text(4));
    assert!(matches!(watch.poll(), ServiceWatchState::Running));
    assert_eq!(
        transport.certificate_status(),
        QualificationStatus::NotTested
    );
    assert!(!transport.is_certificate());

    other.close();
    assert!(
        matches!(watch.poll(), ServiceWatchState::Running),
        "another instance EOF must not end this watch"
    );
    transport.close();
    assert_eq!(
        watch.poll(),
        ServiceWatchState::Terminated {
            reason: "broker_eof"
        }
    );
    assert_eq!(
        transport.certificate_status(),
        QualificationStatus::NotTested
    );
    assert!(!transport.is_certificate());

    let mut env = std::collections::HashMap::new();
    env.insert(ATTEMPT_TOKEN_ENV.to_string(), "env-token".to_string());
    env.insert("CODEG_PARENT_PID".to_string(), "4242".to_string());
    let service = match service_mode() {
        CompanionMode::ServiceRoundtable {
            socket_path,
            incarnation,
        } => (socket_path, incarnation),
        CompanionMode::LegacyParent(_) => panic!("service mode missing"),
    };
    let process = bind_service_process(&service.0, &service.1, &env).expect("bind");
    assert!(!process.reads_host_parent_pid());
    assert_eq!(process.parent_pid(), None);
    assert!(process.token_is_present());
    assert_eq!(process.certificate_status(), QualificationStatus::NotTested);
    assert!(!format!("{process:?}").contains("env-token"));
    assert!(!format!("{process:?}").contains("4242"));
}

fn legacy_mode_parent_pid_still_parses() -> bool {
    matches!(
        legacy_mode(),
        CompanionMode::LegacyParent(args) if args.parent_pid == Some(4242)
    )
}

#[tokio::test]
async fn qualification_uses_production_encoder_and_validator() {
    let tool_core = include_str!("../../src/roundtable/tool_core.rs");
    let harness_source = include_str!("../../src/roundtable/qualification_harness.rs");
    assert!(tool_core.contains("submit_candidate("));
    assert!(tool_core.contains("validate_result("));
    assert!(!tool_core.contains("fn submit_candidate"));
    assert!(!tool_core.contains("fn validate_result"));
    assert!(harness_source.contains("DeliveryEncoder::encode"));
    assert!(harness_source.contains("InMemoryToolStore"));
    assert!(!harness_source.contains("DurableToolStore"));

    let harness = QualificationHarness::open(BILLING);
    let secret = harness.sandbox_visible_token().to_string();
    let tokens = Utf8Tokens {
        capacity: harness.profile().model_capacity_tokens,
    };
    let manifest = harness.delivery_manifest(&tokens).expect("delivery");
    let direct = DeliveryEncoder::encode(
        harness.phase(),
        harness.role(),
        harness.binding_id(),
        &tokens,
        harness.profile(),
    )
    .expect("production encoder");
    assert_eq!(manifest, direct);
    assert_eq!(harness.role().tool_text, service_tool_schema());
    assert_eq!(harness.role().tool_version, SERVICE_TOOL_VERSION);
    assert_eq!(harness.role().schema_text, service_result_schema());
    assert_eq!(harness.role().schema_id, SERVICE_RESULT_SCHEMA_ID);
    assert_eq!(harness.phase().schema_version, SCHEMA_VERSION);
    let prompt =
        DeliveryEncoder::prompt_utf8(harness.phase(), harness.role(), harness.binding_id())
            .expect("prompt");
    assert_eq!(manifest.prompt_bytes.0, prompt.len() as u64);
    assert_eq!(manifest.prompt_hash, Hash256::sha256(&prompt));
    let prompt_text = String::from_utf8(prompt).expect("utf8 prompt");
    assert_no_secret(&prompt_text, &secret);
    assert!(!harness.store_is_durable());
    assert!(QualificationHarness::open(BILLING)
        .sealed_receipt()
        .is_none());

    assert!(harness.allow_normal_completion().is_err());
    let scope = harness.result_scope();
    let bad = canonical_bytes(&bad_proposal()).expect("bad canonical");
    let mut local = roundtable_protocol::SubmissionState::open();
    let bad_decision = harness
        .submit_result("bad-1", &bad_proposal())
        .await
        .expect("bad");
    let direct_bad = submit_candidate(&local, &sid("bad-1"), &bad, scope);
    assert_eq!(bad_decision, direct_bad);
    local = direct_bad.next_state;
    assert_eq!(harness.tool_calls(), 1);
    assert!(harness.tool_reply_bytes() > 0);
    assert!(harness.sealed_receipt().is_none());

    let good = canonical_bytes(&proposal()).expect("good canonical");
    let good_decision = harness
        .submit_result("seal-1", &proposal())
        .await
        .expect("good");
    let direct_good = submit_candidate(&local, &sid("seal-1"), &good, scope);
    assert_eq!(good_decision, direct_good);
    let receipt = match &good_decision.outcome {
        roundtable_protocol::DecisionKind::Staged(receipt) => receipt.clone(),
        other => panic!("expected staged receipt, got {other:?}"),
    };
    assert_eq!(receipt.state, CandidateState::Staged);
    local = direct_good.next_state;
    assert!(harness.allow_normal_completion().is_err());
    let calls_after_submit = harness.tool_calls();
    let bytes_after_submit = harness.tool_reply_bytes();

    let retry = harness
        .submit_result("seal-1", &proposal())
        .await
        .expect("retry");
    let direct_retry = submit_candidate(&local, &sid("seal-1"), &good, scope);
    assert_eq!(retry, direct_retry);
    assert_eq!(retry.outcome, good_decision.outcome);
    assert!(harness.tool_calls() > calls_after_submit);
    assert!(harness.tool_reply_bytes() > bytes_after_submit);
    harness.deliver_receipt_to_cli(&receipt).expect("deliver");
    harness
        .allow_normal_completion()
        .expect("completion after receipt");

    let closing = QualificationHarness::open(BILLING);
    let closing_scope = closing.result_scope().clone();
    let mut closing_local = roundtable_protocol::SubmissionState::open();
    for index in 1..=9 {
        let id = format!("bad-{index}");
        let decision = closing
            .submit_result(&id, &bad_proposal())
            .await
            .expect("invalid");
        let direct = submit_candidate(&closing_local, &sid(&id), &bad, &closing_scope);
        assert_eq!(decision, direct);
        closing_local = direct.next_state;
    }
    assert!(closing_local.closed);
    assert_eq!(closing_local.shape_invalid_count, 9);
    assert_eq!(closing_local.invalid_count, 0);
    assert!(closing_local.sealed.is_none());
    assert_eq!(closing.tool_calls(), 9);

    closing.close_fake_broker();
    assert_eq!(closing.certificate_status(), QualificationStatus::NotTested);
    assert!(!closing.fake_transport_is_certificate());
    assert_eq!(harness.certificate_status(), QualificationStatus::NotTested);
}
