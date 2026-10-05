//! P11 durable evidence reads and candidate seals.
//!
//! The shared dispatcher, validator, encoder, and reply quotas stay in
//! `tool_core` / `roundtable_protocol`. This file only drives the token
//! authority and the SQLite store. A staged receipt is not an accepted turn.
//! The product execution gate stays off and `synchronous` stays `NORMAL`.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use codeg_lib::roundtable::{
    migrate_roundtable, note_unverified_reference, open_roundtable_store, persist_candidate,
    read_evidence, register_input_evidence, search_evidence, service_channel_owner,
    verify_connection_profile, AdmissionFacts, ConnectionOwner, DurableToolStore, EvidenceUsage,
    ExecutionGate, ExecutionScope, GateToolAuthority, InputEvidence, NewAttempt, NewBinding,
    NewManifest, NewPhase, NewRoom, NewSpeaker, NewTurn, ObjectStore, OsIdentity,
    QualificationKey, ReadEvidenceArgs, ReservationLedger, RoundtableToolCall, SearchEvidenceArgs,
    TokenBinding, ToolAuthority, ToolSession, SERVICE_TOOL_VERSION,
};
use roundtable_protocol::{
    canonical_bytes, AliasVisibility, AttemptId, BindingId, CandidateReceipt, CandidateState,
    DecisionKind, Epoch, ErrorCode, EvidenceId, EvidenceRef, Fence, FinishKind, Hash256, MonoMs,
    ObjectKind, ObjectRefV1, PhaseId, PhaseKind, PrincipalId, QualificationStatus,
    QualifiedContextProfile, ResultScope, Revision, RoomId, SafeInt, ServiceOwner, SpeakerId,
    SubmissionId, VisibleAliases,
};
use serde_json::{json, Value};

use crate::roundtable_support::open_pool;

struct Harness {
    _dir: tempfile::TempDir,
    conn: sea_orm::DatabaseConnection,
    store: DurableToolStore,
    authority: GateToolAuthority,
    token: codeg_lib::roundtable::AttemptToken,
    scope: codeg_lib::roundtable::AdmittedToolScope,
    room_id: RoomId,
    attempt_id: AttemptId,
    speaker_id: SpeakerId,
    phase_id: String,
}

async fn open_harness(slot: u8) -> Harness {
    let (dir, conn) = open_pool(4).await;
    migrate_roundtable(&conn).await.expect("migrate");
    let db = open_roundtable_store(conn.clone()).await.expect("store");
    let room_id = id::<RoomId>(slot.saturating_mul(10).saturating_add(1));
    let speaker_id = id::<SpeakerId>(slot.saturating_mul(10).saturating_add(2));
    let phase = id::<PhaseId>(slot.saturating_mul(10).saturating_add(3));
    let binding_id = id::<BindingId>(slot.saturating_mul(10).saturating_add(4));
    let turn = id_text(slot.saturating_mul(10).saturating_add(5));
    let attempt_id = id::<AttemptId>(slot.saturating_mul(10).saturating_add(6));
    let manifest = id_text(slot.saturating_mul(10).saturating_add(7));
    let manifest_hash = "ab".repeat(32);
    seed_graph(
        &db,
        &room_id,
        &speaker_id,
        &phase,
        &binding_id,
        &turn,
        &attempt_id,
        &manifest,
        &manifest_hash,
    )
    .await;

    let small = b"alpha\naxb\na.b\n.*";
    let big = largest_line();
    let over = "a".repeat(big.len() + 1);
    let objects = ObjectStore::open(
        dir.path().join("objects"),
        Arc::new(ReservationLedger::new(8_000_000)),
        id::<PrincipalId>(1),
    )
    .expect("objects");
    let small_ref = put_object(&objects, small).await;
    let big_ref = put_object(&objects, big.as_bytes()).await;
    let over_ref = put_object(&objects, over.as_bytes()).await;
    let scope = result_scope(speaker_id);
    let binding = token_binding(
        attempt_id,
        room_id,
        speaker_id,
        phase,
        scope.clone(),
        &[
            ("e0", &small_ref, AliasVisibility::Published),
            ("big", &big_ref, AliasVisibility::Published),
            ("over", &over_ref, AliasVisibility::Published),
            ("peer", &small_ref, AliasVisibility::PeerStaged),
        ],
    );
    let authority = GateToolAuthority::open(
        dir.path(),
        ExecutionScope::Fake,
        facts(),
        MonoMs(0),
        MonoMs(1_000_000),
        binding,
    );
    let token = authority.issue();
    let admitted = authority.admit_tool(&token).await.expect("admit");
    let session = ToolSession {
        room_id: room_id.to_string(),
        attempt_id: attempt_id.to_string(),
        speaker_id: speaker_id.to_string(),
        phase_id: phase.to_string(),
        phase_revision: 2,
        manifest_id: manifest,
        workspace_snapshot_id: id_text(40),
        manifest_hash,
    };
    let store = DurableToolStore::open(db, objects, session, scope);
    Harness {
        _dir: dir,
        conn,
        store,
        authority,
        token,
        scope: admitted,
        room_id,
        attempt_id,
        speaker_id,
        phase_id: phase.to_string(),
    }
}

async fn seed_graph(
    db: &codeg_lib::roundtable::RoundtableStore,
    room_id: &RoomId,
    speaker_id: &SpeakerId,
    phase: &PhaseId,
    binding_id: &BindingId,
    turn: &str,
    attempt_id: &AttemptId,
    manifest: &str,
    manifest_hash: &str,
) {
    let room = room_id.to_string();
    let speaker = speaker_id.to_string();
    db.insert_room(&NewRoom {
        room_id: room.clone(),
        principal_id: id_text(30),
        status: "running".to_string(),
        config_ref: "config".to_string(),
        revision: 1,
        run_epoch: 7,
        boot_epoch: 7,
        last_seq: 0,
        remaining_active_ms: 1_800_000,
        blocked_reason: None,
        result_quality: None,
    })
    .await
    .expect("room");
    db.insert_speaker(&NewSpeaker {
        room_id: room.clone(),
        speaker_id: speaker.clone(),
        ordinal: 0,
        role: "member".to_string(),
        provider_ref: "provider".to_string(),
        model_id: "model".to_string(),
        model_snapshot_json: "{}".to_string(),
    })
    .await
    .expect("speaker");
    db.insert_manifest(&NewManifest {
        room_id: room.clone(),
        manifest_id: manifest.to_string(),
        version: 1,
        manifest_hash: manifest_hash.to_string(),
        body_json: "{\"files\":[]}".to_string(),
    })
    .await
    .expect("manifest");
    db.insert_phase(&NewPhase {
        room_id: room.clone(),
        phase_id: phase.to_string(),
        phase_index: 0,
        revision: 2,
        status: "running".to_string(),
        snapshot_ref: "snap".to_string(),
        snapshot_hash: manifest_hash.to_string(),
        expected: 1,
        quorum: 1,
        remaining_ms: 450_000,
        manifest_id: Some(manifest.to_string()),
    })
    .await
    .expect("phase");
    db.insert_binding(&NewBinding {
        room_id: room.clone(),
        binding_id: binding_id.to_string(),
        speaker_id: speaker,
        generation: 1,
        incarnation: id_text(31),
        external_session_id: "ext".to_string(),
        policy_ref: "policy".to_string(),
        certificate_ref: "cert".to_string(),
        context_state: "fresh".to_string(),
        state: "active".to_string(),
    })
    .await
    .expect("binding");
    db.insert_turn(&NewTurn {
        room_id: room.clone(),
        turn_id: turn.to_string(),
        phase_id: phase.to_string(),
        speaker_id: speaker_id.to_string(),
        status: "open".to_string(),
        admitted_attempt_count: 0,
    })
    .await
    .expect("turn");
    db.insert_attempt(&NewAttempt {
        room_id: room,
        attempt_id: attempt_id.to_string(),
        turn_id: turn.to_string(),
        attempt_no: 1,
        binding_id: binding_id.to_string(),
        fence: 1,
        dispatch_state: "sent".to_string(),
        state: "active".to_string(),
        prompt_hash: manifest_hash.to_string(),
        delivery_hash: manifest_hash.to_string(),
        cleanup_state: "pending".to_string(),
        residual_remote_work: 0,
    })
    .await
    .expect("attempt");
}

fn result_scope(speaker_id: SpeakerId) -> ResultScope {
    ResultScope {
        phase_kind: PhaseKind::Proposal,
        speaker_id,
        aliases: VisibleAliases::default(),
        mandatory_targets: Vec::new(),
        published: roundtable_protocol::PublishedHistory::default(),
        quota_bytes: 8_192,
    }
}

fn token_binding(
    attempt_id: AttemptId,
    room_id: RoomId,
    speaker_id: SpeakerId,
    phase: PhaseId,
    scope: ResultScope,
    evidence: &[(&str, &ObjectRefV1, AliasVisibility)],
) -> TokenBinding {
    let mut aliases = VisibleAliases::default();
    let mut objects = BTreeMap::new();
    for (name, object, visibility) in evidence {
        aliases.evidence.insert(
            (*name).to_string(),
            EvidenceRef {
                evidence_id: id::<EvidenceId>(9),
                visibility: *visibility,
            },
        );
        objects.insert((*name).to_string(), (*object).clone());
    }
    let mut result = scope;
    result.aliases = aliases.clone();
    TokenBinding {
        attempt_id,
        room_id,
        fence: Fence {
            boot_epoch: Epoch(7),
            run_epoch: Epoch(7),
            phase_id: phase,
            phase_revision: Revision(2),
            attempt_id,
            binding_id: id::<BindingId>(4),
            incarnation: id(8),
            context_hash: Hash256::from_bytes([0x11; 32]),
            policy_hash: Hash256::from_bytes([0x22; 32]),
        },
        speaker_id,
        tool_version: SERVICE_TOOL_VERSION.to_string(),
        aliases,
        result_scope: result,
        evidence: objects,
        profile: QualifiedContextProfile::proposed(
            "test-tokenizer",
            Hash256::from_bytes([0x44; 32]),
            2_000_000,
            0,
            "proof-p11",
        ),
    }
}

fn facts() -> AdmissionFacts {
    AdmissionFacts {
        certificate: QualificationStatus::NotTested,
        presented_key: QualificationKey {
            os: OsIdentity {
                name: "test".to_string(),
                version: "0".to_string(),
            },
            binaries: Vec::new(),
            image_digest: String::new(),
            policy_hash: Hash256::from_bytes([0; 32]),
            tool_contract_hash: Hash256::from_bytes([0; 32]),
            core_hash: Hash256::from_bytes([0; 32]),
            adapter_version: String::new(),
            isolator_version: String::new(),
            plan_hash: Hash256::from_bytes([0; 32]),
        },
        qualification_attempts_used: 0,
        qualification_spend_used: 0,
        fixture_hash: Hash256::from_bytes([0; 32]),
        recipient: String::new(),
    }
}

async fn put_object(objects: &ObjectStore, bytes: &[u8]) -> ObjectRefV1 {
    let stored = objects.put(bytes).await.expect("put");
    ObjectRefV1 {
        object_id: stored.object_id,
        kind: ObjectKind::SourceExcerpt,
        content_hash: stored.content_hash,
        total_bytes: SafeInt(stored.total_bytes),
    }
}

fn largest_line() -> String {
    let mut best = 1usize;
    let mut size = 1usize;
    while size <= 8_192 {
        let excerpt = "a".repeat(size);
        if exchange_len(&excerpt) <= 8_192 {
            best = size;
            size += 1;
        } else {
            break;
        }
    }
    "a".repeat(best)
}

fn exchange_len(excerpt: &str) -> usize {
    let body = canonical_bytes(&json!({
        "excerpt": excerpt,
        "file_alias": "big",
        "has_more": false,
        "total_lines": 1
    }))
    .expect("body");
    let args = canonical_bytes(&json!({
        "end_line": 1,
        "file_alias": "big",
        "start_line": 1
    }))
    .expect("args");
    body.len() + args.len()
}

fn valid_raw(summary: &str) -> Vec<u8> {
    canonical_bytes(&json!({
        "kind": "proposal",
        "summary": summary,
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": [],
            "confidence": "low"
        }]
    }))
    .expect("valid")
}

fn bad_raw(summary: &str) -> Vec<u8> {
    canonical_bytes(&json!({
        "kind": "proposal",
        "summary": summary,
        "claims": []
    }))
    .expect("bad")
}

fn sid(text: &str) -> SubmissionId {
    SubmissionId::from_str(text).expect(text)
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

async fn invoke<'a>(
    harness: &'a Harness,
    token: &'a codeg_lib::roundtable::AttemptToken,
    name: &str,
    arguments: Value,
) -> roundtable_protocol::RtResult<codeg_lib::roundtable::RoundtableToolResponse> {
    codeg_lib::roundtable::invoke_scoped_tool(
        token,
        RoundtableToolCall {
            name: name.to_string(),
            arguments,
        },
        &harness.authority,
        &harness.store,
    )
    .await
}

#[tokio::test]
async fn token_scope_and_tool_budget() {
    let harness = open_harness(1).await;
    let binding = harness.authority.binding_of(&harness.token).expect("binding");
    assert_eq!(binding.attempt_id, harness.attempt_id);
    assert_eq!(binding.room_id, harness.room_id);
    assert_eq!(binding.speaker_id, harness.speaker_id);
    assert_eq!(binding.tool_version, SERVICE_TOOL_VERSION);
    assert_eq!(binding.fence.phase_revision, Revision(2));
    assert!(binding.aliases.evidence.contains_key("e0"));
    assert!(!harness.authority.gate_enabled());
    assert!(!ExecutionGate::open(harness._dir.path()).enabled());
    let profile = verify_connection_profile(&harness.conn)
        .await
        .expect("profile");
    assert!(profile
        .connections
        .iter()
        .all(|item| item.synchronous == "NORMAL"));

    let mcp = include_str!("../../src/roundtable/mcp.rs");
    assert!(!mcp.contains("pub async fn dispatch_tool"));
    assert!(!mcp.contains("CompanionLeaseRegistry"));
    assert!(!mcp.contains("sqlx"));
    let owner = service_channel_owner(&ConnectionOwner::Service {
        room_id: harness.room_id,
        attempt_id: harness.attempt_id,
        boot_epoch: Epoch(7),
    })
    .expect("service owner");
    assert_eq!(
        owner,
        ServiceOwner {
            room_id: harness.room_id,
            attempt_id: harness.attempt_id,
            boot_epoch: Epoch(7),
        }
    );
    let window = service_channel_owner(&ConnectionOwner::Window {
        label: "observer".to_string(),
        operation_id: None,
    })
    .expect_err("window");
    assert_eq!(reason(&window), "service_owner_required");

    assert_eq!(
        u64::from(roundtable_protocol::v1_1::DEFAULT_INTERJECTION_BYTES),
        16_384
    );
    assert_eq!(harness.authority.note_interjection(16_384).expect("quota"), 0);
    assert!(harness.authority.note_interjection(1).is_err());
    let fresh = GateToolAuthority::open(
        harness._dir.path(),
        ExecutionScope::Fake,
        facts(),
        MonoMs(0),
        MonoMs(10),
        harness.authority.binding_of(&harness.token).expect("pinned"),
    );
    assert!(fresh.note_interjection(16_385).is_err());

    let input = register_input_evidence(
        &harness.store,
        InputEvidence {
            path: "src/lib.rs".to_string(),
            content_hash: "cd".repeat(32),
            published_seq: 4,
            staged_phase_id: harness.phase_id.clone(),
            phase_revision: 2,
        },
    )
    .await
    .expect("input");
    assert_eq!(input.file_alias, "E1");
    assert_eq!(input.owner_speaker_id, harness.speaker_id.to_string());
    assert_eq!(input.owner_attempt_id, None);
    assert_eq!(input.staged_phase_id, harness.phase_id);
    assert_eq!(input.phase_revision, 2);
    assert_eq!(input.published_seq, Some(4));
    assert!(input.verified);

    let url = note_unverified_reference(&harness.store, "url", "https://example.test/a")
        .await
        .expect("url");
    let cli = note_unverified_reference(&harness.store, "cli_native_read", "src/lib.rs")
        .await
        .expect("cli");
    assert_eq!(url.file_alias, "E2");
    assert_eq!(cli.file_alias, "E3");
    assert!(!url.verified && !cli.verified);
    assert_eq!(url.attribution, "unverified_reference");
    assert_eq!(cli.origin, "cli_native_read");
    assert!(note_unverified_reference(&harness.store, "file", "src/lib.rs")
        .await
        .is_err());

    let slice = read_evidence(
        &harness.scope,
        ReadEvidenceArgs {
            file_alias: "e0".to_string(),
            start_line: 1,
            end_line: 1,
        },
        &harness.store,
    )
    .await
    .expect("read");
    assert!(slice.encoded_bytes <= 8192);
    assert_eq!(slice.excerpt, "alpha");
    assert_eq!(slice.total_lines, 4);
    assert!(slice.has_more);
    assert_eq!(slice.byte_start, 0);
    assert_eq!(slice.byte_end, 6);
    assert_eq!(slice.evidence_alias, "E4");
    assert_eq!(slice.file_alias, "e0");
    let rows = harness.store.list_evidence().await.expect("rows");
    let read_row = rows.iter().find(|row| row.file_alias == "E4").expect("E4");
    assert_eq!(read_row.owner_speaker_id, harness.speaker_id.to_string());
    assert_eq!(
        read_row.owner_attempt_id.as_deref(),
        Some(harness.attempt_id.to_string().as_str())
    );
    assert_eq!(read_row.staged_phase_id, harness.phase_id);
    assert_eq!(read_row.phase_revision, 2);
    assert_eq!(read_row.published_seq, None);
    assert!(read_row.verified);
    assert_eq!(read_row.line_start, Some(1));
    assert_eq!(read_row.line_end, Some(1));

    let page = search_evidence(
        &harness.scope,
        SearchEvidenceArgs {
            file_alias: "e0".to_string(),
            query: "a.b".to_string(),
            limit: 10,
        },
        &harness.store,
    )
    .await
    .expect("literal search");
    assert!(page.literal);
    assert_eq!(page.hits.len(), 1);
    assert_eq!(page.hits[0].line, 3);
    assert_eq!(page.hits[0].text, "a.b");
    assert!(page.encoded_bytes <= 8192);
    let dotted = search_evidence(
        &harness.scope,
        SearchEvidenceArgs {
            file_alias: "e0".to_string(),
            query: ".".to_string(),
            limit: 20,
        },
        &harness.store,
    )
    .await
    .expect("dot is text");
    assert_eq!(
        dotted
            .hits
            .iter()
            .map(|hit| hit.text.as_str())
            .collect::<Vec<_>>(),
        vec!["a.b", ".*"]
    );
    let star = search_evidence(
        &harness.scope,
        SearchEvidenceArgs {
            file_alias: "e0".to_string(),
            query: ".*".to_string(),
            limit: 20,
        },
        &harness.store,
    )
    .await
    .expect("star is text");
    assert_eq!(star.hits.len(), 1);
    assert_eq!(star.hits[0].text, ".*");

    let peer = search_evidence(
        &harness.scope,
        SearchEvidenceArgs {
            file_alias: "peer".to_string(),
            query: "alpha".to_string(),
            limit: 1,
        },
        &harness.store,
    )
    .await
    .expect_err("peer staged");
    assert_eq!(peer.code, ErrorCode::Forbidden);
    assert_eq!(reason(&peer), "peer_staged_evidence");

    let extra = read_evidence(
        &harness.scope,
        ReadEvidenceArgs {
            file_alias: "e0".to_string(),
            start_line: 1,
            end_line: 1,
        },
        &harness.store,
    )
    .await
    .expect("range still reads");
    assert_eq!(extra.excerpt, "alpha");
    let unknown = invoke(
        &harness,
        &harness.token,
        "read_evidence",
        json!({"file_alias": "e0", "start_line": 1, "end_line": 1, "path": "../x"}),
    )
    .await
    .expect_err("unknown field");
    assert_eq!(reason(&unknown), "unknown_field");
    let bad_range = invoke(
        &harness,
        &harness.token,
        "read_evidence",
        json!({"file_alias": "e0", "start_line": 0, "end_line": 1}),
    )
    .await
    .expect_err("line");
    assert_eq!(reason(&bad_range), "line_range");
    let wide_query = invoke(
        &harness,
        &harness.token,
        "search_evidence",
        json!({"file_alias": "e0", "query": "a".repeat(257), "limit": 1}),
    )
    .await
    .expect_err("query");
    assert_eq!(reason(&wide_query), "query_bounds");
    let wide_limit = invoke(
        &harness,
        &harness.token,
        "search_evidence",
        json!({"file_alias": "e0", "query": "a", "limit": 21}),
    )
    .await
    .expect_err("limit");
    assert_eq!(reason(&wide_limit), "query_bounds");

    let forged = invoke(
        &harness,
        &harness.token,
        "read_evidence",
        json!({
            "file_alias": "e0",
            "start_line": 1,
            "end_line": 1,
            "room_id": id_text(9),
            "attempt_id": id_text(9)
        }),
    )
    .await
    .expect_err("model identity");
    assert_eq!(forged.code, ErrorCode::Forbidden);
    assert_eq!(reason(&forged), "identity_not_selectable");
    assert_eq!(
        harness.authority.binding_of(&harness.token).expect("still").room_id,
        harness.room_id
    );
    assert_eq!(
        harness
            .authority
            .binding_of(&harness.token)
            .expect("still")
            .attempt_id,
        harness.attempt_id
    );

    let over = invoke(
        &harness,
        &harness.token,
        "read_evidence",
        json!({"file_alias": "over", "start_line": 1, "end_line": 1}),
    )
    .await
    .expect_err("per call");
    assert_eq!(over.code, ErrorCode::ContextTooLarge);

    let already = harness.scope.evidence_attempt_bytes();
    let unit = exchange_len(&largest_line()) as u64;
    assert!(unit <= 8_192);
    let fits = (32_768 - already - 2_048) / unit;
    assert!(fits >= 1);
    for _ in 0..fits {
        invoke(
            &harness,
            &harness.token,
            "read_evidence",
            json!({"file_alias": "big", "start_line": 1, "end_line": 1}),
        )
        .await
        .expect("within attempt budget");
    }
    let overflow = invoke(
        &harness,
        &harness.token,
        "read_evidence",
        json!({"file_alias": "big", "start_line": 1, "end_line": 1}),
    )
    .await
    .expect_err("attempt budget");
    assert_eq!(overflow.code, ErrorCode::ContextTooLarge);
    let usage: EvidenceUsage = harness.authority.usage();
    assert!(usage.returned_bytes <= 32768);

    let short = harness.authority.issue_until(MonoMs(5));
    harness.authority.advance_to(MonoMs(5));
    let expired = invoke(
        &harness,
        &short,
        "read_evidence",
        json!({"file_alias": "e0", "start_line": 1, "end_line": 1}),
    )
    .await
    .expect_err("expired");
    assert_eq!(reason(&expired), "token_expired");
    harness.authority.set_live_attempt(id(9));
    let crossed = invoke(
        &harness,
        &harness.token,
        "read_evidence",
        json!({"file_alias": "e0", "start_line": 1, "end_line": 1}),
    )
    .await
    .expect_err("cross attempt");
    assert_eq!(reason(&crossed), "attempt_mismatch");
    harness.authority.reset_control();
    let other = harness.authority.issue();
    harness.authority.revoke(&other).expect("revoke");
    let revoked = invoke(
        &harness,
        &other,
        "read_evidence",
        json!({"file_alias": "e0", "start_line": 1, "end_line": 1}),
    )
    .await
    .expect_err("revoked");
    assert_eq!(reason(&revoked), "token_revoked");

    harness.authority.complete_inflight();
    let inflight = harness.authority.admit_tool(&harness.token).await.expect("inflight");
    let barrier = harness.authority.stop();
    assert_eq!(barrier.pending_tools.len(), 1);
    assert!(!harness.authority.gate_held());
    let late_read = read_evidence(
        &inflight,
        ReadEvidenceArgs {
            file_alias: "e0".to_string(),
            start_line: 2,
            end_line: 2,
        },
        &harness.store,
    )
    .await
    .expect("admitted read finishes");
    assert_eq!(late_read.excerpt, "axb");
    assert!(late_read.encoded_bytes <= 8192);
    let denied = invoke(
        &harness,
        &harness.token,
        "read_evidence",
        json!({"file_alias": "e0", "start_line": 1, "end_line": 1}),
    )
    .await
    .expect_err("stopped");
    assert_eq!(denied.code, ErrorCode::InvalidState);
    assert_eq!(reason(&denied), "attempt_closed");
    harness.authority.complete_inflight();
    assert_eq!(harness.authority.pending_handlers(), 0);

    let staged = persist_candidate(
        &inflight,
        sid("after-stop"),
        valid_raw("after stop"),
        &harness.store,
    )
    .await
    .expect("audit seal");
    assert_eq!(staged.state, CandidateState::Staged);
    assert!(harness.store.promote_staged_candidate().await.is_err());
    let audit = harness.store.audit().await.expect("audit");
    assert_eq!(audit.accepted_count, 0);
    assert_ne!(audit.attempt_state, "accepted");
    assert_eq!(audit.sealed_count, 1);
}

#[tokio::test]
async fn candidate_receipt_survives_lost_reply() {
    let harness = open_harness(2).await;
    let bad = invoke(
        &harness,
        &harness.token,
        "submit_result",
        json!({"submission_id": "bad-1", "result": json!({"kind": "proposal", "summary": "empty", "claims": []})}),
    )
    .await
    .expect("field errors are a tool result");
    assert!(bad.receipt.is_none());
    let body: Value = serde_json::from_slice(&bad.body).expect("body");
    assert_eq!(body["kind"], json!("field_errors"));
    assert!(body["candidate_id"].is_null());
    assert!(!String::from_utf8_lossy(&bad.body).contains("accepted"));
    match bad.decision.expect("decision").outcome {
        DecisionKind::FieldErrors(errors) => assert!(!errors.is_empty()),
        other => panic!("field errors reported as {other:?}"),
    }
    let after_error = harness.store.audit().await.expect("audit");
    assert_eq!(after_error.accepted_count, 0);
    assert_eq!(after_error.sealed_count, 0);
    assert_ne!(after_error.attempt_state, "accepted");
    assert!(!after_error.closed);
    let calls_after_error = harness.scope.tool_calls();
    let changed_bad = invoke(
        &harness,
        &harness.token,
        "submit_result",
        json!({"submission_id": "bad-1", "result": json!({"kind": "proposal", "summary": "other empty", "claims": []})}),
    )
    .await
    .expect("same id different payload conflicts");
    assert!(changed_bad.receipt.is_none());
    assert!(matches!(
        changed_bad.decision.expect("decision").outcome,
        DecisionKind::SubmissionConflict
    ));
    assert!(!String::from_utf8_lossy(&changed_bad.body).contains("accepted"));
    assert!(harness.scope.tool_calls() > calls_after_error);

    let raw = valid_raw("成员摘要");
    let first_receipt = persist_candidate(&harness.scope, sid("seal-1"), raw.clone(), &harness.store)
        .await
        .expect("commit");
    assert_eq!(first_receipt.state, CandidateState::Staged);
    let _lost_reply = first_receipt.clone();
    drop(_lost_reply);
    let retry_receipt = persist_candidate(&harness.scope, sid("seal-1"), raw.clone(), &harness.store)
        .await
        .expect("retry");
    assert_eq!(retry_receipt,first_receipt);
    assert_eq!(harness.store.audit().await.expect("audit").sealed_count, 1);

    let conflict = persist_candidate(
        &harness.scope,
        sid("seal-1"),
        valid_raw("different payload"),
        &harness.store,
    )
    .await
    .expect_err("conflict");
    assert_eq!(conflict.code, ErrorCode::IdempotencyConflict);
    assert_eq!(reason(&conflict), "submission_conflict");
    assert_eq!(harness.store.audit().await.expect("audit").sealed_count, 1);

    let replay = GateToolAuthority::open(
        harness._dir.path(),
        ExecutionScope::Fake,
        facts(),
        MonoMs(0),
        MonoMs(1_000_000),
        harness.authority.binding_of(&harness.token).expect("binding"),
    );
    let replay_token = replay.issue();
    let first_call = codeg_lib::roundtable::invoke_scoped_tool(
        &replay_token,
        RoundtableToolCall {
            name: "submit_result".to_string(),
            arguments: json!({"submission_id": "seal-1", "result": json!({"kind": "proposal", "summary": "成员摘要", "claims": [{"local_key": "c0", "text": "claim", "evidence_aliases": [], "confidence": "low"}]})}),
        },
        &replay,
        &harness.store,
    )
    .await
    .expect("durable retry");
    assert_eq!(first_call.receipt.clone().expect("receipt"), first_receipt);
    let second_call = codeg_lib::roundtable::invoke_scoped_tool(
        &replay_token,
        RoundtableToolCall {
            name: "submit_result".to_string(),
            arguments: json!({"submission_id": "seal-1", "result": json!({"kind": "proposal", "summary": "成员摘要", "claims": [{"local_key": "c0", "text": "claim", "evidence_aliases": [], "confidence": "low"}]})}),
        },
        &replay,
        &harness.store,
    )
    .await
    .expect("retry still charged");
    assert_eq!(second_call.receipt.expect("receipt"), first_receipt);
    let changed = codeg_lib::roundtable::invoke_scoped_tool(
        &replay_token,
        RoundtableToolCall {
            name: "submit_result".to_string(),
            arguments: json!({"submission_id": "seal-1", "result": json!({"kind": "proposal", "summary": "different payload", "claims": [{"local_key": "c0", "text": "claim", "evidence_aliases": [], "confidence": "low"}]})}),
        },
        &replay,
        &harness.store,
    )
    .await
    .expect("conflict response");
    assert!(changed.receipt.is_none());
    // A sealed id with a different payload is `result_already_sealed` from
    // `submit_candidate`. The durable row API reports `submission_conflict`.
    // Either rejection leaves the staged receipt in place and still charges.
    assert!(matches!(
        changed.decision.expect("decision").outcome,
        DecisionKind::ResultAlreadySealed
    ));
    assert!(!String::from_utf8_lossy(&changed.body).contains("accepted"));
    let view = replay.admit_tool(&replay_token).await.expect("view");
    assert!(view.tool_calls() >= 3);

    harness
        .store
        .note_abnormal_finish(FinishKind::Cancelled)
        .await
        .expect("cancel");
    let accepted_count_after_cancel = harness.store.audit().await.expect("audit").accepted_count;
    assert_eq!(accepted_count_after_cancel,0);
    let late = persist_candidate(
        &harness.scope,
        sid("seal-2"),
        valid_raw("too late"),
        &harness.store,
    )
    .await
    .expect_err("cancel blocks a new candidate");
    assert_eq!(reason(&late), "not_accepted");
    harness
        .store
        .note_abnormal_finish(FinishKind::Failed)
        .await
        .expect("failed");
    let after_fail = harness.store.audit().await.expect("audit");
    assert_eq!(after_fail.accepted_count, 0);
    assert_ne!(after_fail.attempt_state, "accepted");
    assert!(harness.store.promote_staged_candidate().await.is_err());
    let idempotent = persist_candidate(&harness.scope, sid("seal-1"), raw, &harness.store)
        .await
        .expect("original receipt remains");
    assert_eq!(idempotent, first_receipt);
    assert_eq!(
        harness.store.audit().await.expect("audit").accepted_count,
        0
    );
}

#[tokio::test]
async fn concurrent_valid_submissions_seal_once() {
    let harness = open_harness(3).await;
    let raw_a = valid_raw("left");
    let raw_b = valid_raw("right");
    let id_a = sid("seal-a");
    let id_b = sid("seal-b");
    let (left, right) = tokio::join!(
        persist_candidate(&harness.scope, id_a.clone(), raw_a.clone(), &harness.store),
        persist_candidate(&harness.scope, id_b.clone(), raw_b.clone(), &harness.store),
    );
    let mut sealed: Option<CandidateReceipt> = None;
    let mut already = false;
    for result in [left, right] {
        match result {
            Ok(receipt) => {
                assert_eq!(receipt.state, CandidateState::Staged);
                assert!(sealed.is_none(), "two candidates");
                sealed = Some(receipt);
            }
            Err(err) => {
                assert!(!already, "two failures");
                assert_eq!(err.code, ErrorCode::InvalidState);
                assert_eq!(reason(&err), "result_already_sealed");
                already = true;
            }
        }
    }
    assert!(already);
    let winner = sealed.expect("one receipt");
    let winner_raw = if winner.submission_id == id_a {
        raw_a
    } else {
        raw_b
    };
    let again = persist_candidate(
        &harness.scope,
        winner.submission_id.clone(),
        winner_raw,
        &harness.store,
    )
    .await
    .expect("winner retry");
    assert_eq!(again, winner);
    let audit = harness.store.audit().await.expect("audit");
    assert_eq!(audit.sealed_count, 1);
    assert_eq!(audit.accepted_count, 0);
    assert_ne!(audit.attempt_state, "accepted");
}

#[tokio::test]
async fn concurrent_invalid_third_fourth_close_once() {
    let harness = open_harness(4).await;
    for (index, name) in [("bad-1", "one"), ("bad-2", "two")] {
        let err = persist_candidate(&harness.scope, sid(index), bad_raw(name), &harness.store)
            .await
            .expect_err("repairable");
        assert_eq!(reason(&err), "field_errors");
        assert!(!err.details.field_errors.is_empty());
    }
    let open = harness.store.audit().await.expect("open");
    assert_eq!(open.invalid_count, 2);
    assert!(!open.closed);
    assert_eq!(open.sealed_count, 0);
    assert_eq!(open.accepted_count, 0);
    assert_ne!(open.attempt_state, "accepted");

    let (third, fourth) = tokio::join!(
        persist_candidate(&harness.scope, sid("bad-3"), bad_raw("three"), &harness.store),
        persist_candidate(&harness.scope, sid("bad-4"), bad_raw("four"), &harness.store),
    );
    for result in [third, fourth] {
        let err = result.expect_err("invalid");
        assert_eq!(reason(&err), "field_errors");
        assert!(!err.details.field_errors.is_empty());
        assert!(!format!("{err:?}").contains("accepted"));
    }
    let closed = harness.store.audit().await.expect("closed");
    assert_eq!(closed.invalid_count, 4);
    assert!(closed.closed);
    assert_eq!(closed.sealed_count, 0);
    assert_eq!(closed.accepted_count, 0);
    assert_eq!(closed.attempt_state, "invalid");

    let late = persist_candidate(
        &harness.scope,
        sid("seal-late"),
        valid_raw("after close"),
        &harness.store,
    )
    .await
    .expect_err("closed");
    assert_eq!(reason(&late), "attempt_closed");
    let after = harness.store.audit().await.expect("after");
    assert_eq!(after.invalid_count, 4);
    assert!(after.closed);
    assert_eq!(after.sealed_count, 0);
    assert_eq!(after.accepted_count, 0);
    assert_ne!(after.attempt_state, "accepted");
}
