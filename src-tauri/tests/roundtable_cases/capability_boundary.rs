//! P07e sealed launch capabilities and completion failure fixtures.
//!
//! Service mode is built from an empty set. Ordinary flags, forged companion
//! tokens, legacy feature flags, extra tools, extra MCP servers, global
//! config, and native shell egress cannot widen it. `end_turn` is not
//! success. A fake pass is not a certificate.

use std::str::FromStr;
use std::sync::Arc;

use codeg_lib::roundtable::{
    bind_private_ingress, classify_service_failure, dispatch_tool, drain_recorded_failures,
    host_broker_call, host_tools_agent_manifest, policy_hash_for, qualify_service,
    record_service_response, record_service_update, sealed_service_manifest,
    service_client_capabilities_value, service_manifest_for_session, verify_service_manifest,
    AdvertisedToolsNotUsed, BarrierFact, CompletionCoordinator, CompletionMarker,
    ConnectionOwner, FailureObservation, FailureSource, InMemoryToolStore, ManifestExtras,
    OrdinarySessionFlags, PrivateRuntimeSink, QualificationReport, RoundtableToolCall,
    ServiceTurnDecision, ATTEMPT_TOKEN_ENV, SERVICE_TOOL_VERSION,
};
use roundtable_protocol::{
    canonical_bytes, submit_candidate, AcpUpdate, BindingId, CandidateState, DecisionKind, Epoch,
    ErrorCode, Fence, FinishKind, HandlerId, Hash256, PhaseId, PhaseKind, QualificationStatus,
    ResultScope, Revision, SpeakerId, VisibleAliases,
};
use serde_json::{json, Value};

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

fn fence_for(attempt: u8) -> Fence {
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

fn three_tools() -> Vec<String> {
    vec![
        "read_evidence".to_string(),
        "search_evidence".to_string(),
        "submit_result".to_string(),
    ]
}

fn all_ordinary_flags() -> OrdinarySessionFlags {
    OrdinarySessionFlags::from_names(&[
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
    ])
}

fn air(failure: Value) -> serde_json::Map<String, Value> {
    json!({
        "jetbrains": {
            "air": {
                "version": 1,
                "sessionFailure": failure
            }
        }
    })
    .as_object()
    .expect("object")
    .clone()
}

fn error_record(id: &str) -> Value {
    json!({
        "id": id,
        "revision": 1,
        "category": "request",
        "severity": "error",
        "title": "request failed"
    })
}

fn observation(
    fence: &Fence,
    turn: u64,
    meta: Option<&serde_json::Map<String, Value>>,
    source: FailureSource,
    stop_reason: Option<&str>,
    http_status: Option<u16>,
) -> FailureObservation {
    FailureObservation {
        fence: fence.clone(),
        turn_generation: turn,
        classification: classify_service_failure(meta, source, stop_reason, http_status),
    }
}

fn sealed_candidate() -> roundtable_protocol::CandidateReceipt {
    let scope = ResultScope {
        phase_kind: PhaseKind::Proposal,
        speaker_id: id::<SpeakerId>(18),
        aliases: VisibleAliases::default(),
        mandatory_targets: Vec::new(),
        published: roundtable_protocol::PublishedHistory::default(),
        quota_bytes: 8 * 1024,
    };
    let raw = canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "成员摘要",
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": [],
            "confidence": "low"
        }]
    }))
    .unwrap();
    let decision = submit_candidate(
        &roundtable_protocol::SubmissionState::open(),
        &roundtable_protocol::SubmissionId::from_str("seal-1").unwrap(),
        &raw,
        &scope,
    );
    match decision.outcome {
        DecisionKind::Staged(receipt) => receipt,
        other => panic!("expected one staged candidate, got {other:?}"),
    }
}

fn open_turn(fence: Fence, turn: u64) -> (CompletionCoordinator, roundtable_protocol::CompletionBarrier) {
    let mut coordinator = CompletionCoordinator::open(fence.clone());
    let handler = HandlerId::new("service-tool");
    coordinator.admit_handler(handler.clone()).expect("admit");
    coordinator
        .stage_candidate(sealed_candidate())
        .expect("stage");
    let barrier = coordinator.begin(CompletionMarker {
        fence: fence.clone(),
        turn_generation: turn,
        ingress_watermark: 1,
        prompt_response_seq: 1,
        finish: FinishKind::Normal,
    });
    coordinator.finish_handler(&handler);
    coordinator.apply_update(AcpUpdate {
        seq: 1,
        attempt_id: fence.attempt_id,
        binding_id: fence.binding_id,
        incarnation: fence.incarnation,
        cancelled: false,
    });
    (coordinator, barrier)
}

fn adjudicate(
    coordinator: &mut CompletionCoordinator,
    fence: &Fence,
    barrier: &roundtable_protocol::CompletionBarrier,
) -> ServiceTurnDecision {
    coordinator
        .adjudicate_service_turn(BarrierFact {
            fence: fence.clone(),
            admitted_handlers: barrier.pending_tools.clone(),
            acp_watermark: barrier.ingress_watermark,
        })
        .expect("barrier drained")
}

struct Sink;

impl PrivateRuntimeSink for Sink {
    fn attach(&self, _connection_id: &str, _owner: &ConnectionOwner) {}
}

#[test]
fn service_mode_stays_three_tools_when_ordinary_flags_are_on() {
    let flags = all_ordinary_flags();
    assert!(flags.any_enabled());
    let manifest = service_manifest_for_session(&flags);
    let sealed = sealed_service_manifest();
    assert_eq!(manifest, sealed);
    assert_eq!(manifest.tool_names, three_tools());
    verify_service_manifest(&manifest, &ManifestExtras::default()).expect("sealed");
    assert!(host_broker_call(&manifest, &ManifestExtras::default(), "read_evidence").is_ok());
    assert!(host_broker_call(&manifest, &ManifestExtras::default(), "search_evidence").is_ok());
    assert!(host_broker_call(&manifest, &ManifestExtras::default(), "submit_result").is_ok());
    let caps = service_client_capabilities_value();
    assert_eq!(
        caps["_meta"]["jetbrains"]["air"]["capabilities"],
        json!(["sessionFailure"])
    );
    assert_ne!(caps.get("terminal"), Some(&Value::Bool(true)));
    assert_ne!(
        caps.get("fs").and_then(|fs| fs.get("readTextFile")),
        Some(&Value::Bool(true))
    );
    assert!(caps.get("elicitation").is_none());
}

#[test]
fn widen_attempts_cannot_expand_the_service_set() {
    let sealed = sealed_service_manifest();
    let forged = ManifestExtras {
        ordinary_companion_token: true,
        ..ManifestExtras::default()
    };
    let err = verify_service_manifest(&sealed, &forged).expect_err("forged token");
    assert_eq!(reason(&err), "ordinary_companion_token");
    let err = host_broker_call(&sealed, &forged, "read_evidence").expect_err("broker");
    assert_eq!(reason(&err), "ordinary_companion_token");

    let legacy = ManifestExtras {
        legacy_feature_flags: vec!["browser,delegation".to_string()],
        ..ManifestExtras::default()
    };
    let err = verify_service_manifest(&sealed, &legacy).expect_err("legacy flag");
    assert_eq!(reason(&err), "legacy_feature_flag");

    let mut extra_tool = sealed.clone();
    extra_tool.tool_names.push("browser_eval".to_string());
    let err = verify_service_manifest(&extra_tool, &ManifestExtras::default()).expect_err("tool");
    assert_eq!(reason(&err), "unknown_capability");
    assert!(host_broker_call(&extra_tool, &ManifestExtras::default(), "browser_eval").is_err());

    let mut extra_mcp = sealed.clone();
    extra_mcp.mcp_servers.push("filesystem".to_string());
    let err = verify_service_manifest(&extra_mcp, &ManifestExtras::default()).expect_err("mcp");
    assert_eq!(reason(&err), "extra_mcp");

    let global = ManifestExtras {
        global_config: true,
        ..ManifestExtras::default()
    };
    let err = verify_service_manifest(&sealed, &global).expect_err("global");
    assert_eq!(reason(&err), "global_config");
    let mut config = sealed.clone();
    config.config = "~/.codex/config.toml".to_string();
    let err = verify_service_manifest(&config, &ManifestExtras::default()).expect_err("path");
    assert_eq!(reason(&err), "global_config");

    let mut shell = sealed.clone();
    shell.native_shell = true;
    shell.network_destinations.push("https://example.test".to_string());
    let err = verify_service_manifest(&shell, &ManifestExtras::default()).expect_err("shell");
    assert_eq!(reason(&err), "native_shell_egress");

    let hidden = ManifestExtras {
        ui_hidden_tools: vec!["submit_result".to_string()],
        ..ManifestExtras::default()
    };
    let err = verify_service_manifest(&sealed, &hidden).expect_err("ui");
    assert_eq!(reason(&err), "ui_switch_cannot_hide");
    assert_eq!(sealed.tool_names, three_tools());
}

#[test]
fn host_tools_agent_alone_cannot_qualify() {
    let manifest = host_tools_agent_manifest();
    assert!(manifest.native_shell);
    assert_ne!(manifest.tool_names, three_tools());
    assert!(verify_service_manifest(&manifest, &ManifestExtras::default()).is_err());
    let joined = policy_hash_for(&sealed_service_manifest()).expect("hash");
    let key = key_with(joined);
    let report = QualificationReport {
        key: key.clone(),
        status: QualificationStatus::Passed,
        evidence_ref: "fake-agent".to_string(),
    };
    assert_eq!(
        qualify_service(&key, &manifest, Some(&report)),
        QualificationStatus::Failed
    );
}

#[test]
fn manifest_hash_joins_policy_and_fake_pass_is_not_a_certificate() {
    let manifest = sealed_service_manifest();
    let text = String::from_utf8(canonical_bytes(&manifest).unwrap()).unwrap();
    assert!(!text.contains("sk-billing"));
    assert!(!text.contains("super-secret-token"));
    assert_eq!(
        manifest.environment_keys,
        vec![ATTEMPT_TOKEN_ENV.to_string()]
    );
    assert!(manifest.inherited_fds.is_empty());
    let manifest_hash = manifest.canonical_hash().unwrap();
    let joined = policy_hash_for(&manifest).unwrap();
    assert_ne!(joined, manifest_hash);
    let key = key_with(joined);
    assert_eq!(
        qualify_service(&key, &manifest, None),
        QualificationStatus::NotTested
    );
    let report = QualificationReport {
        key: key.clone(),
        status: QualificationStatus::Passed,
        evidence_ref: "fake-success".to_string(),
    };
    assert_eq!(
        qualify_service(&key, &manifest, Some(&report)),
        QualificationStatus::NotTested
    );
    assert_ne!(
        qualify_service(&key, &manifest, Some(&report)),
        QualificationStatus::Passed
    );
    let mut drifted = key.clone();
    drifted.policy_hash = manifest_hash;
    assert_eq!(
        qualify_service(&drifted, &manifest, Some(&report)),
        QualificationStatus::Expired
    );
    let broker = codeg_lib::roundtable::FakeBrokerTransport::open("socket", "incarnation");
    assert!(!broker.is_certificate());
    assert_eq!(
        broker.certificate_status(),
        QualificationStatus::NotTested
    );
}

#[tokio::test]
async fn dispatch_rejects_tools_outside_the_sealed_manifest() {
    let manifest = sealed_service_manifest();
    verify_service_manifest(&manifest, &ManifestExtras::default()).unwrap();
    let registry = codeg_lib::roundtable::TokenRegistry::new();
    let fence = fence_for(2);
    let speaker = id::<SpeakerId>(8);
    let profile = roundtable_protocol::QualifiedContextProfile::proposed(
        "test-tokenizer",
        Hash256::from_bytes([0x44; 32]),
        2_000_000,
        0,
        "proof-p07e",
    );
    let binding = codeg_lib::roundtable::TokenBinding {
        attempt_id: fence.attempt_id,
        room_id: id(5),
        fence: fence.clone(),
        speaker_id: speaker,
        tool_version: SERVICE_TOOL_VERSION.to_string(),
        aliases: VisibleAliases::default(),
        result_scope: ResultScope {
            phase_kind: PhaseKind::Proposal,
            speaker_id: speaker,
            aliases: VisibleAliases::default(),
            mandatory_targets: Vec::new(),
            published: roundtable_protocol::PublishedHistory::default(),
            quota_bytes: 8_192,
        },
        evidence: std::collections::BTreeMap::new(),
        profile,
    };
    let token = registry.issue(binding.clone());
    let scope = registry
        .admit(token.reveal_for_same_sandbox(), &binding, "read_evidence")
        .expect("admit");
    let err = dispatch_tool(
        &scope,
        RoundtableToolCall {
            name: "browser_eval".to_string(),
            arguments: json!({}),
        },
        &InMemoryToolStore::new(),
    )
    .await
    .expect_err("browser");
    assert_eq!(err.code, ErrorCode::Forbidden);
    assert_eq!(reason(&err), "tool_not_admitted");
}

#[test]
fn recorded_session_failure_uses_the_typed_parser() {
    let fence = fence_for(2);
    bind_private_ingress(
        "p07e-record",
        Arc::new(Sink),
        fence.clone(),
        4,
    );
    let meta = air(error_record("turn-4"));
    record_service_update("p07e-record", Some(4), Some(&meta));
    record_service_response("p07e-record", Some(4), Some(&meta), "end_turn");
    let notes = drain_recorded_failures("p07e-record");
    assert_eq!(notes.len(), 2);
    assert!(drain_recorded_failures("p07e-record").is_empty());
    assert_eq!(notes[0].classification.records[0].severity, "error");
    assert_eq!(notes[0].classification.records[0].source, FailureSource::Update);
    assert_eq!(notes[1].classification.records[0].source, FailureSource::Response);
    assert!(!notes[0].classification.incompatible);
}

#[test]
fn http_400_and_end_turn_blocks_accepted() {
    let fence = fence_for(2);
    let note = observation(&fence, 4, None, FailureSource::Response, Some("end_turn"), Some(400));
    let (mut coordinator, barrier) = open_turn(fence.clone(), 4);
    coordinator.observe_service_failure(note);
    let decision = adjudicate(&mut coordinator, &fence, &barrier);
    assert!(decision.blocked);
    assert!(!decision.accepted);
    assert!(!decision.certificate);
    assert_eq!(decision.reason.as_deref(), Some("http_status"));
    assert_eq!(decision.stop_reason.as_deref(), Some("end_turn"));
    assert_ne!(decision.finish_reason, "accepted");
}

#[test]
fn candidate_then_response_error_blocks_accepted() {
    let fence = fence_for(2);
    let meta = air(error_record("turn-4"));
    let note = observation(
        &fence,
        4,
        Some(&meta),
        FailureSource::Response,
        Some("end_turn"),
        None,
    );
    let (mut coordinator, barrier) = open_turn(fence.clone(), 4);
    coordinator.observe_service_failure(note);
    let decision = adjudicate(&mut coordinator, &fence, &barrier);
    assert!(decision.blocked);
    assert!(!decision.accepted);
    assert!(!decision.certificate);
    assert_eq!(decision.reason.as_deref(), Some("session_failure"));
    assert!(decision.candidate_id.is_some());
    assert_eq!(
        coordinator.staged_candidate().map(|receipt| receipt.state),
        Some(CandidateState::Staged)
    );
}

#[test]
fn late_error_inside_barrier_blocks_accepted() {
    let fence = fence_for(2);
    let mut coordinator = CompletionCoordinator::open(fence.clone());
    let handler = HandlerId::new("late-tool");
    coordinator.admit_handler(handler.clone()).expect("admit");
    coordinator
        .stage_candidate(sealed_candidate())
        .expect("stage");
    let barrier = coordinator.begin(CompletionMarker {
        fence: fence.clone(),
        turn_generation: 4,
        ingress_watermark: 1,
        prompt_response_seq: 1,
        finish: FinishKind::Normal,
    });
    let early = coordinator
        .adjudicate_service_turn(BarrierFact {
            fence: fence.clone(),
            admitted_handlers: barrier.pending_tools.clone(),
            acp_watermark: barrier.ingress_watermark,
        })
        .expect_err("still inside the barrier");
    assert_eq!(early.code, ErrorCode::InvalidState);
    assert_eq!(reason(&early), "barrier_pending");
    let meta = air(error_record("late"));
    coordinator.observe_service_failure(observation(
        &fence,
        4,
        Some(&meta),
        FailureSource::Update,
        None,
        None,
    ));
    coordinator.finish_handler(&handler);
    coordinator.apply_update(AcpUpdate {
        seq: 1,
        attempt_id: fence.attempt_id,
        binding_id: fence.binding_id,
        incarnation: fence.incarnation,
        cancelled: false,
    });
    let decision = adjudicate(&mut coordinator, &fence, &barrier);
    assert!(decision.blocked);
    assert!(!decision.accepted);
    assert_eq!(decision.blocking_ids, vec!["late".to_string()]);
}

#[test]
fn old_turn_error_does_not_block_or_poison_the_next_attempt() {
    let fence = fence_for(2);
    let meta = air(error_record("old"));
    let (mut current, barrier) = open_turn(fence.clone(), 4);
    current.observe_service_failure(observation(
        &fence,
        3,
        Some(&meta),
        FailureSource::Update,
        None,
        None,
    ));
    let decision = adjudicate(&mut current, &fence, &barrier);
    assert!(!decision.blocked);
    assert!(!decision.accepted);
    assert!(!decision.certificate);
    assert_eq!(decision.finish_reason, "normal");

    let next_fence = fence_for(9);
    let (mut next, next_barrier) = open_turn(next_fence.clone(), 4);
    let next_decision = adjudicate(&mut next, &next_fence, &next_barrier);
    assert!(!next_decision.blocked);
    assert!(!next_decision.accepted);
    assert_ne!(next_decision.finish_reason, "accepted");
}

#[test]
fn warning_does_not_fail_the_turn() {
    let fence = fence_for(2);
    let meta = air(json!({
        "id": "warn-1",
        "revision": 1,
        "severity": "warning",
        "title": "retrying"
    }));
    let (mut coordinator, barrier) = open_turn(fence.clone(), 4);
    coordinator.observe_service_failure(observation(
        &fence,
        4,
        Some(&meta),
        FailureSource::Update,
        Some("end_turn"),
        None,
    ));
    let decision = adjudicate(&mut coordinator, &fence, &barrier);
    assert!(!decision.blocked);
    assert!(!decision.accepted);
    assert!(!decision.certificate);
    assert_eq!(decision.warning_ids, vec!["warn-1".to_string()]);
    assert!(decision.blocking_ids.is_empty());
    assert_eq!(decision.finish_reason, "normal");
    assert_ne!(decision.finish_reason, "accepted");
}

#[test]
fn duplicate_error_blocks_once() {
    let fence = fence_for(2);
    let meta = air(error_record("dup"));
    let (mut coordinator, barrier) = open_turn(fence.clone(), 4);
    coordinator.observe_service_failure(observation(
        &fence,
        4,
        Some(&meta),
        FailureSource::Update,
        None,
        None,
    ));
    coordinator.observe_service_failure(observation(
        &fence,
        4,
        Some(&meta),
        FailureSource::Response,
        Some("end_turn"),
        None,
    ));
    let decision = adjudicate(&mut coordinator, &fence, &barrier);
    assert!(decision.blocked);
    assert!(!decision.accepted);
    assert_eq!(decision.blocking_ids, vec!["dup".to_string()]);
    assert_eq!(
        decision.sources,
        vec![FailureSource::Update, FailureSource::Response]
    );
}

#[test]
fn end_turn_is_not_success() {
    let fence = fence_for(2);
    let (mut coordinator, barrier) = open_turn(fence.clone(), 4);
    coordinator.observe_service_failure(observation(
        &fence,
        4,
        None,
        FailureSource::Response,
        Some("end_turn"),
        None,
    ));
    let decision = adjudicate(&mut coordinator, &fence, &barrier);
    assert!(!decision.blocked);
    assert!(!decision.accepted);
    assert!(!decision.certificate);
    assert_eq!(decision.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(decision.finish_reason, "normal");
    assert_ne!(decision.finish_reason, "accepted");
}

#[test]
fn unrecognized_failure_shape_is_adapter_incompatible() {
    let fence = fence_for(2);
    let meta = air(json!({"title": "no id", "version": 2}));
    let (mut coordinator, barrier) = open_turn(fence.clone(), 4);
    coordinator.observe_service_failure(observation(
        &fence,
        4,
        Some(&meta),
        FailureSource::Response,
        Some("end_turn"),
        None,
    ));
    let decision = adjudicate(&mut coordinator, &fence, &barrier);
    assert!(decision.blocked);
    assert!(decision.adapter_incompatible);
    assert!(!decision.accepted);
    assert!(!decision.certificate);
    assert_eq!(decision.reason.as_deref(), Some("adapter_incompatible"));
    assert_eq!(decision.public_code(), Some(ErrorCode::CapabilityUnqualified));
    assert!(ErrorCode::parse("adapter_incompatible").is_none());
}

fn key_with(policy: Hash256) -> codeg_lib::roundtable::QualificationKey {
    codeg_lib::roundtable::QualificationKey {
        os: codeg_lib::roundtable::OsIdentity {
            name: "linux".to_string(),
            version: "debian-12".to_string(),
        },
        binaries: vec![codeg_lib::roundtable::CertifiedBinary {
            role: "cli".to_string(),
            absolute_path: if cfg!(windows) {
                r"C:\crun\pinned\codex.exe".to_string()
            } else {
                "/usr/local/libexec/codeg/codex-pinned".to_string()
            },
            version: "codex-acp-2.1.1".to_string(),
            sha256: Hash256::from_bytes([2; 32]),
        }],
        image_digest: format!("sha256:{}", Hash256::from_bytes([3; 32]).to_hex()),
        policy_hash: policy,
        tool_contract_hash: Hash256::from_bytes([5; 32]),
        core_hash: Hash256::from_bytes([6; 32]),
        adapter_version: "codex-acp@2.1.1".to_string(),
        isolator_version: "linux-oci-1".to_string(),
        plan_hash: Hash256::from_bytes([7; 32]),
    }
}
