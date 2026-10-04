//! Golden protocol contracts. Assertions name the closed sets directly.

#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::path::PathBuf;

use roundtable_protocol::{
    canonical_hash, decode_config, decode_json, mutation_methods, read_methods, validate_config,
    AttemptState, CommandNameV1, ControlKind, ControlStep, ErrorCode, InternalReason, LimitsOrigin,
    MemberKind, ParseLimits, PauseRequest, PhaseKind, PhaseState, ResourceLimits, RoomState,
    RtError, RuntimeMark, CONTRACT_PROFILE, INACCESSIBLE_ROOM_HTTP_STATUS, SCHEMA_VERSION,
};

fn limits() -> ResourceLimits {
    ResourceLimits::suggested_profile()
}

fn parse() -> ParseLimits {
    ParseLimits::suggested_profile()
}

#[test]
fn config_ranges_and_closed_commands() {
    let limits = limits();
    let parse = parse();
    assert_eq!(CONTRACT_PROFILE, "roundtable_plan_1_2");
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(limits.origin, LimitsOrigin::SuggestionNotV11HardLimit);
    assert_eq!(limits.max_participants, 16);
    assert_eq!(limits.max_critique_rounds, 8);
    assert_ne!(limits.max_participants, 3);
    assert_ne!(limits.max_critique_rounds, 2);
    assert_ne!(limits.max_participants, 2);

    assert!(validate_config(&support::config(7, 5, 2), &limits).is_ok());
    assert_eq!(
        validate_config(&support::config(1, 0, 1), &limits)
            .unwrap_err()
            .details
            .reason
            .as_deref(),
        Some("participant_count")
    );
    assert_eq!(
        validate_config(&support::config(2, 0, 0), &limits)
            .unwrap_err()
            .details
            .reason
            .as_deref(),
        Some("concurrency")
    );
    assert_eq!(
        validate_config(&support::config(2, 0, 3), &limits)
            .unwrap_err()
            .details
            .reason
            .as_deref(),
        Some("concurrency")
    );
    let mut tight = limits.clone();
    tight.origin = LimitsOrigin::Custom;
    tight.max_participants = 4;
    assert!(validate_config(&support::config(7, 5, 2), &tight).is_err());

    let duplicate = decode_config(br#"{"n":2,"n":3}"#, &parse).unwrap_err();
    assert_eq!(duplicate.code, ErrorCode::InvalidArgument);
    assert_eq!(duplicate.details.reason.as_deref(), Some("duplicate_key"));
    assert_ne!(duplicate.details.reason.as_deref(), Some("missing_field"));

    let deep = decode_config(&nested_object(33), &parse).unwrap_err();
    assert_eq!(deep.code, ErrorCode::InvalidArgument);
    assert_eq!(deep.details.reason.as_deref(), Some("max_depth"));

    let mut deeper = parse.clone();
    deeper.max_depth = 40;
    assert!(
        roundtable_protocol::parse_strict_json(&nested_object(33), &deeper).is_ok(),
        "depth 32 is a profile suggestion, not a hardcoded parser maximum"
    );

    let nan = decode_config(br#"{"topic":NaN}"#, &parse).unwrap_err();
    assert_eq!(nan.details.reason.as_deref(), Some("non_finite_number"));
    let inf = decode_config(br#"{"topic":Infinity}"#, &parse).unwrap_err();
    assert_eq!(inf.details.reason.as_deref(), Some("non_finite_number"));
    let neg_zero = decode_config(br#"{"topic":-0}"#, &parse).unwrap_err();
    assert_eq!(neg_zero.details.reason.as_deref(), Some("negative_zero"));
    let float = decode_config(br#"{"concurrency":1.5}"#, &parse).unwrap_err();
    assert_eq!(float.details.reason.as_deref(), Some("float_rejected"));

    let unknown = decode_config(&with_unknown_field(), &parse).unwrap_err();
    assert_eq!(unknown.code, ErrorCode::InvalidArgument);
    assert_eq!(unknown.details.reason.as_deref(), Some("unknown_field"));

    let strategy = decode_config(&with_strategy("free_talk"), &parse).unwrap_err();
    assert_eq!(strategy.details.reason.as_deref(), Some("unknown_strategy"));

    assert_commands_partition();
    assert!(!mutation_methods().contains(&"roundtable_delete"));
    assert!(!mutation_methods().contains(&"roundtable_export"));
    assert!(!read_methods().contains(&"roundtable_delete"));
    assert!(!CommandNameV1::ALL.contains(&"roundtable_delete"));
    assert!(!CommandNameV1::ALL.contains(&"roundtable_export"));

    assert_state_closures();
    assert_error_mappings();
    assert_moderator_speaker_non_empty();
    assert_quality_holdout();
    let fence = support::fence(3);
    assert_eq!(fence.boot_epoch, roundtable_protocol::Epoch(3));
    assert_eq!(fence.run_epoch.0, 3);
    assert_eq!(fence.phase_id, support::id("phase"));
}

#[test]
fn strict_json_and_canonical_hash() {
    let parse = parse();
    let omitted = decode_config(&config_json(false, false), &parse).unwrap();
    let explicit = decode_config(&config_json(true, false), &parse).unwrap();
    assert_eq!(omitted, explicit);
    assert_eq!(
        canonical_hash(&omitted).unwrap(),
        canonical_hash(&explicit).unwrap()
    );

    let reordered = decode_config(&config_json(true, true), &parse).unwrap();
    assert_eq!(
        canonical_hash(&explicit).unwrap(),
        canonical_hash(&reordered).unwrap()
    );
    let mut reversed = support::member(MemberKind::Proposal, 2);
    reversed.open_questions = vec!["q1".to_string(), "q2".to_string()];
    let mut swapped = reversed.clone();
    swapped.open_questions = vec!["q2".to_string(), "q1".to_string()];
    assert_ne!(
        canonical_hash(&reversed).unwrap(),
        canonical_hash(&swapped).unwrap()
    );

    let chinese = decode_config(&config_json_topic("中文😀"), &parse).unwrap();
    let canonical = roundtable_protocol::canonical_bytes(&chinese).unwrap();
    let text = String::from_utf8(canonical.clone()).unwrap();
    assert!(text.contains("中文"));
    assert!(text.contains("😀"));
    assert!(!text.contains("\\u4e2d"));
    assert!(!canonical.contains(&b'\n'));
    assert!(!canonical.contains(&b' '));

    let control = decode_config(&config_json_topic("a\u{0001}b"), &parse).unwrap();
    let control_text =
        String::from_utf8(roundtable_protocol::canonical_bytes(&control).unwrap()).unwrap();
    assert!(control_text.contains("\\u0001"));
    assert!(!control_text.as_bytes().contains(&0x01));

    let safe = decode_config(&config_json_input("9007199254740991"), &parse);
    assert!(safe.is_ok(), "{safe:?}");
    let unsafe_int = decode_config(&config_json_input("9007199254740992"), &parse).unwrap_err();
    assert_eq!(unsafe_int.details.reason.as_deref(), Some("safe_integer"));

    let max_u64 = decode_config(&config_json_budget("18446744073709551615"), &parse);
    assert!(max_u64.is_ok(), "{max_u64:?}");
    let overflow = decode_config(&config_json_budget("18446744073709551616"), &parse).unwrap_err();
    assert_eq!(overflow.code, ErrorCode::InvalidArgument);
    let leading = decode_config(&config_json_budget("01"), &parse).unwrap_err();
    assert_eq!(leading.details.reason.as_deref(), Some("wire_u64"));

    let absent_name = decode_config(&config_json(false, false), &parse).unwrap();
    assert!(absent_name.display_name.is_none());
    let null_name = decode_config(&config_with_display_null(), &parse).unwrap_err();
    assert_eq!(
        null_name.details.reason.as_deref(),
        Some("null_not_allowed")
    );
    let missing_commit = decode_config(&config_missing_base_commit(), &parse).unwrap_err();
    assert_eq!(
        missing_commit.details.reason.as_deref(),
        Some("missing_field")
    );

    let bytes = roundtable_protocol::canonical_bytes(&explicit).unwrap();
    assert_eq!(canonical_hash(&explicit).unwrap().to_hex().len(), 64);
    assert_eq!(
        roundtable_protocol::Hash256::sha256(&bytes),
        canonical_hash(&explicit).unwrap()
    );

    assert_golden_fixtures();
    assert_get_read_is_exclusive();
    assert_command_body_has_no_principal();
}

fn assert_commands_partition() {
    const EXPECTED: [&str; 18] = [
        "roundtable_preflight",
        "roundtable_create",
        "roundtable_update_draft",
        "roundtable_start",
        "roundtable_get",
        "roundtable_list",
        "roundtable_pause",
        "roundtable_resume",
        "roundtable_stop",
        "roundtable_interject",
        "roundtable_retry_synthesis",
        "roundtable_events",
        "roundtable_messages",
        "roundtable_evidence",
        "roundtable_operation",
        "roundtable_clone",
        "roundtable_attach",
        "roundtable_detach",
    ];
    assert_eq!(CommandNameV1::ALL, EXPECTED);
    let mut unique = CommandNameV1::ALL.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 18);

    let mut partition = mutation_methods().to_vec();
    partition.extend_from_slice(read_methods());
    let mut sorted_partition = partition.clone();
    sorted_partition.sort_unstable();
    let mut sorted_all = CommandNameV1::ALL.to_vec();
    sorted_all.sort_unstable();
    assert_eq!(sorted_partition, sorted_all);
    assert_eq!(
        mutation_methods().len() + read_methods().len(),
        CommandNameV1::ALL.len()
    );
    for name in mutation_methods() {
        assert!(
            !read_methods().contains(name),
            "{name} is in both partitions"
        );
    }
}

fn assert_state_closures() {
    assert_eq!(
        names(RoomState::ALL, RoomState::as_str),
        [
            "draft",
            "ready",
            "running",
            "pausing",
            "paused",
            "recovering",
            "stopping",
            "stopped",
            "failed",
            "completed",
        ]
    );
    assert_eq!(
        names(PhaseState::ALL, PhaseState::as_str),
        [
            "ready",
            "running",
            "closing",
            "published",
            "failed",
            "superseded"
        ]
    );
    assert_eq!(
        names(AttemptState::ALL, AttemptState::as_str),
        [
            "reserved",
            "launching",
            "admitting",
            "admitted",
            "streaming",
            "validating",
            "accepted",
            "invalid",
            "failed",
            "timed_out",
            "interrupted",
            "uncertain",
        ]
    );
    assert_eq!(
        names(RuntimeMark::ALL, RuntimeMark::as_str),
        ["cancelling", "cleanup"]
    );
    assert!(!names(AttemptState::ALL, AttemptState::as_str).contains(&"cancelling"));
    assert!(!names(AttemptState::ALL, AttemptState::as_str).contains(&"not_dispatched"));
    assert!(!names(RoomState::ALL, RoomState::as_str).contains(&"synthesis_failed"));
    assert_eq!(
        names(PhaseKind::ALL, PhaseKind::as_str),
        ["proposal", "critique", "synthesis"]
    );
    assert_eq!(
        names(ControlKind::ALL, ControlKind::as_str),
        [
            "pause",
            "stop",
            "restart_current",
            "retry_synthesis",
            "recover"
        ]
    );
    assert_eq!(
        names(ControlStep::ALL, ControlStep::as_str),
        [
            "requested",
            "revoking",
            "cleaning",
            "applying",
            "done",
            "blocked"
        ]
    );
}

fn assert_error_mappings() {
    assert_eq!(
        names(ErrorCode::ALL, ErrorCode::as_str),
        [
            "invalid_argument",
            "unauthenticated",
            "forbidden",
            "revision_conflict",
            "idempotency_conflict",
            "command_in_progress",
            "control_in_progress",
            "invalid_state",
            "policy_unenforceable",
            "capability_unqualified",
            "context_too_large",
            "capacity_unknown",
            "insufficient_budget",
            "cannot_reach_quorum",
            "no_next_phase",
            "capacity_limited",
            "storage_unavailable",
            "runtime_unavailable",
        ]
    );
    assert!(ErrorCode::parse("not_found").is_none());
    assert!(ErrorCode::parse("attempt_closed").is_none());
    assert!(ErrorCode::parse("duplicate_key").is_none());
    assert_eq!(INACCESSIBLE_ROOM_HTTP_STATUS, 404);
    assert!(ErrorCode::ALL.iter().all(|code| code.http_status() != 404));

    let mapped = [
        (InternalReason::AttemptClosed, ErrorCode::InvalidState),
        (InternalReason::StaleFence, ErrorCode::InvalidState),
        (InternalReason::QueueRejected, ErrorCode::InvalidState),
        (InternalReason::SnapshotUnstable, ErrorCode::InvalidArgument),
        (
            InternalReason::ContextContractViolation,
            ErrorCode::CapabilityUnqualified,
        ),
    ];
    for (reason, code) in mapped {
        let err = RtError::from_reason(reason);
        assert_eq!(err.code, code);
        assert_eq!(err.details.reason.as_deref(), Some(reason.as_str()));
        assert!(!err.message.contains('\\'));
        assert!(!err.message.to_ascii_lowercase().contains("token"));
        assert!(!err.message.to_ascii_lowercase().contains("secret"));
    }
}

fn assert_moderator_speaker_non_empty() {
    let fixture = read_fixture("moderator.json");
    let case = fixture["cases"][0].clone();
    let original = decode_hex(case["original_hex"].as_str().unwrap());
    let decoded: roundtable_protocol::ModeratorResultV1 = decode_json(&original, &parse()).unwrap();
    assert!(!decoded.speaker_id.to_string().is_empty());
    assert!(decoded.coverage.is_none());
    let canonical = roundtable_protocol::canonical_bytes(&decoded).unwrap();
    assert_eq!(
        hex(&canonical),
        case["canonical_utf8_hex"].as_str().unwrap()
    );
    assert_eq!(
        roundtable_protocol::Hash256::sha256(&canonical).to_hex(),
        case["sha256"].as_str().unwrap()
    );
    assert!(String::from_utf8(canonical)
        .unwrap()
        .contains("\"coverage\":null"));
}

fn assert_golden_fixtures() {
    for name in [
        "config.json",
        "member.json",
        "moderator.json",
        "projection.json",
        "errors.json",
        "commands.json",
    ] {
        let fixture = read_fixture(name);
        assert_eq!(fixture["contract_profile"], "roundtable_plan_1_2");
        assert_eq!(fixture["schema_version"], 1);
        let cases = fixture["cases"].as_array().expect(name);
        assert!(!cases.is_empty(), "{name} has no golden cases");
        for case in cases {
            for key in [
                "original_hex",
                "normalized_utf8_hex",
                "canonical_utf8_hex",
                "sha256",
            ] {
                let value = case[key].as_str().unwrap_or("");
                assert!(!value.is_empty(), "{name} case missing {key}");
            }
            let canonical = decode_hex(case["canonical_utf8_hex"].as_str().unwrap());
            assert_eq!(
                roundtable_protocol::Hash256::sha256(&canonical).to_hex(),
                case["sha256"].as_str().unwrap(),
                "{name} sha256"
            );
            assert_eq!(
                hex(&canonical),
                case["canonical_utf8_hex"].as_str().unwrap()
            );
        }
    }

    let config = read_fixture("config.json");
    let case = &config["cases"][0];
    let original = decode_hex(case["original_hex"].as_str().unwrap());
    let decoded = decode_config(&original, &parse()).unwrap();
    let canonical = roundtable_protocol::canonical_bytes(&decoded).unwrap();
    assert_eq!(
        hex(&canonical),
        case["normalized_utf8_hex"].as_str().unwrap()
    );
    assert_eq!(
        hex(&canonical),
        case["canonical_utf8_hex"].as_str().unwrap()
    );
    assert!(decoded.topic.contains('中'));

    let commands = read_fixture("commands.json");
    let listed: Vec<&str> = commands["json_schema"]["properties"]["commands"]["items"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(listed, CommandNameV1::ALL);

    let projection = read_fixture("projection.json");
    let body_hex = projection["cases"][0]["canonical_utf8_hex"]
        .as_str()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&decode_hex(body_hex)).unwrap();
    assert!(body.get("hash").is_none());
    assert!(body.get("moderator_speaker_id").is_some());
}

fn assert_get_read_is_exclusive() {
    let room = "11111111-1111-4111-8111-111111111111";
    let ok = format!(r#"{{"room_id":"{room}","read":{{"projection":{{}}}}}}"#);
    assert!(decode_json::<roundtable_protocol::GetRequest>(ok.as_bytes(), &parse()).is_ok());
    let both = format!(r#"{{"room_id":"{room}","read":{{"projection":{{}},"metrics":{{}}}}}}"#);
    let err =
        decode_json::<roundtable_protocol::GetRequest>(both.as_bytes(), &parse()).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

fn assert_command_body_has_no_principal() {
    let raw = br#"{"room_id":"11111111-1111-4111-8111-111111111111","request_id":"22222222-2222-4222-8222-222222222222","expected_revision":"3","reason":"pause","principal_id":"33333333-3333-4333-8333-333333333333"}"#;
    let err = decode_json::<PauseRequest>(raw, &parse()).unwrap_err();
    assert_eq!(err.details.reason.as_deref(), Some("unknown_field"));
}

fn assert_quality_holdout() {
    let path = repo_root().join("docs/roundtable/fixtures/quality-holdout.json");
    let bytes = fs::read(&path).expect("quality holdout fixture");
    let hash = roundtable_protocol::Hash256::sha256(&bytes).to_hex();
    let doc = fs::read_to_string(repo_root().join("docs/roundtable/quality-protocol.md"))
        .expect("quality protocol");
    assert!(doc.contains(&hash), "doc missing holdout hash {hash}");
    assert!(!doc.to_ascii_lowercase().contains("placeholder"));
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let tasks = value["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 12);
    let mut families = std::collections::BTreeMap::<&str, usize>::new();
    for task in tasks {
        *families
            .entry(task["family"].as_str().unwrap())
            .or_default() += 1;
        let checks = task["checks"].as_array().unwrap();
        assert!(!checks.is_empty());
        for check in checks {
            assert!(matches!(
                check["check_type"].as_str().unwrap(),
                "seeded_error" | "substantive_response" | "minority_risk"
            ));
        }
    }
    assert_eq!(families.get("fact_check"), Some(&4));
    assert_eq!(families.get("plan_tradeoff"), Some(&4));
    assert_eq!(families.get("counterexample_tracking"), Some(&4));
}

fn names<T: Copy>(items: &[T], as_str: fn(T) -> &'static str) -> Vec<&'static str> {
    items.iter().copied().map(as_str).collect()
}

fn nested_object(depth: usize) -> Vec<u8> {
    let mut text = String::new();
    for _ in 0..depth {
        text.push_str(r#"{"a":"#);
    }
    text.push('1');
    for _ in 0..depth {
        text.push('}');
    }
    text.into_bytes()
}

fn config_json(explicit_defaults: bool, reorder: bool) -> Vec<u8> {
    let strategy_version = if explicit_defaults {
        r#""version":1,"#
    } else {
        ""
    };
    let quota_defaults = if explicit_defaults {
        r#""output_byte_limit":8192,"interjection_byte_limit":16384,"#
    } else {
        ""
    };
    let snapshot = "11111111-1111-4111-8111-111111111111";
    if reorder {
        return format!(
            r#"{{"quotas":{{{quota_defaults}"input_byte_limit":4096}},"timeouts":{{"attempt_timeout":"225000"}},"budgets":{{"phase_budget":"450000","room_budget":"900000"}},"concurrency":2,"strategy":{{"critique_rounds":5,{strategy_version}"type":"phased_rounds"}},"moderator_ordinal":0,"participants":[{{"provider_ref":"provider:test","role":"role-0","ordinal":0}},{{"provider_ref":"provider:test","role":"role-1","ordinal":1}},{{"ordinal":2,"provider_ref":"provider:test","role":"role-2"}},{{"ordinal":3,"provider_ref":"provider:test","role":"role-3"}},{{"ordinal":4,"provider_ref":"provider:test","role":"role-4"}},{{"ordinal":5,"provider_ref":"provider:test","role":"role-5"}},{{"ordinal":6,"provider_ref":"provider:test","role":"role-6"}}],"source_refs":[{{"base_commit":null,"snapshot_id":"{snapshot}"}}],"workspace_id":"workspace-test","topic":"圆桌契约","schema_version":1,"strict_snapshot_v1":true}}"#
        )
        .into_bytes();
    }
    format!(
        r#"{{"schema_version":1,"topic":"圆桌契约","workspace_id":"workspace-test","source_refs":[{{"snapshot_id":"{snapshot}","base_commit":null}}],"participants":[{{"ordinal":0,"role":"role-0","provider_ref":"provider:test"}},{{"ordinal":1,"role":"role-1","provider_ref":"provider:test"}},{{"ordinal":2,"role":"role-2","provider_ref":"provider:test"}},{{"ordinal":3,"role":"role-3","provider_ref":"provider:test"}},{{"ordinal":4,"role":"role-4","provider_ref":"provider:test"}},{{"ordinal":5,"role":"role-5","provider_ref":"provider:test"}},{{"ordinal":6,"role":"role-6","provider_ref":"provider:test"}}],"moderator_ordinal":0,"strategy":{{"type":"phased_rounds",{strategy_version}"critique_rounds":5}},"concurrency":2,{strict}"budgets":{{"room_budget":"900000","phase_budget":"450000"}},"timeouts":{{"attempt_timeout":"225000"}},"quotas":{{{quota_defaults}"input_byte_limit":4096}}}}"#,
        strict = if explicit_defaults {
            r#""strict_snapshot_v1":true,"#
        } else {
            ""
        }
    )
    .into_bytes()
}

fn config_json_topic(topic: &str) -> Vec<u8> {
    let mut value: serde_json::Value = serde_json::from_slice(&config_json(true, false)).unwrap();
    value["topic"] = serde_json::Value::String(topic.to_string());
    // Re-encode through serde only for this helper's input text. The decoder
    // under test still receives raw bytes and rejects duplicate keys itself.
    serde_json::to_vec(&value).unwrap()
}

fn config_json_input(number: &str) -> Vec<u8> {
    let text = String::from_utf8(config_json(true, false)).unwrap();
    text.replace("4096", number).into_bytes()
}

fn config_json_budget(number: &str) -> Vec<u8> {
    let text = String::from_utf8(config_json(true, false)).unwrap();
    text.replace("900000", number).into_bytes()
}

fn with_unknown_field() -> Vec<u8> {
    let text = String::from_utf8(config_json(true, false)).unwrap();
    let mut text = text;
    text.pop();
    text.push_str(r#","not_a_field":1}"#);
    text.into_bytes()
}

fn with_strategy(name: &str) -> Vec<u8> {
    String::from_utf8(config_json(true, false))
        .unwrap()
        .replace("phased_rounds", name)
        .into_bytes()
}

fn config_with_display_null() -> Vec<u8> {
    let text = String::from_utf8(config_json(true, false)).unwrap();
    text.replace(
        r#""topic":"圆桌契约""#,
        r#""topic":"圆桌契约","display_name":null"#,
    )
    .into_bytes()
}

fn config_missing_base_commit() -> Vec<u8> {
    String::from_utf8(config_json(true, false))
        .unwrap()
        .replace(r#","base_commit":null"#, "")
        .into_bytes()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_fixture(name: &str) -> serde_json::Value {
    let path = repo_root().join("docs/roundtable/fixtures").join(name);
    let bytes = fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).unwrap()
}

fn decode_hex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).unwrap())
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    roundtable_protocol::to_hex(bytes)
}
