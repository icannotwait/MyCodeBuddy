//! Result schema, alias checks, and the pure seal transition.
//!
//! These tests do not prove a database race. A later persistence test has to
//! show that one unique constraint admits only one sealed candidate.

#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

use std::path::PathBuf;
use std::str::FromStr;

use serde::Deserialize;
use serde_json::{json, Value};

use roundtable_protocol::validation::FieldError;
use roundtable_protocol::{
    canonical_bytes, submit_candidate, validate_consensus, validate_result, AgreementLevel,
    AliasKind, AliasRefV1, AliasVisibility, CandidateState, ClaimId, ClaimRef, ConsensusItemV1,
    DecisionKind, EvidenceId, EvidenceRef, FieldCode, MemberKind, PhaseId, PhaseKind, Priority,
    PublicationStatus, PublishedClaim, PublishedHistory, PublishedMember, PublishedPhase,
    PublishedResponse, RequiredTarget, ResponseId, ResponseRef, ResultScope, Seq, SpeakerId,
    SpeakerOrdinal, Stance, SubmissionId, ValidatedResult, VisibleAliases,
    MAX_REPAIRABLE_INVALID_SUBMISSIONS, MAX_REPAIRABLE_SHAPE_SUBMISSIONS,
};

#[test]
fn submission_seals_once() {
    let fixture = fixture();
    assert_eq!(
        fixture.limits.max_repairable_invalid_submissions,
        MAX_REPAIRABLE_INVALID_SUBMISSIONS
    );
    let scope = result_scope(PhaseKind::Proposal, fixture.limits.default_quota_bytes);
    let bad = canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "empty",
        "claims": []
    }))
    .unwrap();
    assert_eq!(
        MAX_REPAIRABLE_SHAPE_SUBMISSIONS, 8,
        "shape mistakes stay bounded"
    );
    let mut state = roundtable_protocol::SubmissionState::open();
    let mut eighth_invalid = None;
    for index in 1..=8 {
        let decision = submit(&state, &format!("bad-{index}"), &bad, &scope);
        assert!(
            matches!(decision.outcome, DecisionKind::FieldErrors(ref errors) if !errors.is_empty()),
            "invalid {index} returns field errors"
        );
        assert!(!decision.closes_attempt, "shape {index} stays open");
        assert!(!decision.next_state.closed);
        if index == 8 {
            eighth_invalid = Some(decision);
        } else {
            state = decision.next_state;
        }
    }
    let eighth_invalid = eighth_invalid.expect("eighth invalid");
    let ninth_invalid = submit(&eighth_invalid.next_state, "bad-9", &bad, &scope);
    assert_eq!(eighth_invalid.next_state.shape_invalid_count, 8);
    assert_eq!(eighth_invalid.next_state.invalid_count, 0);
    assert!(!eighth_invalid.closes_attempt);
    assert!(ninth_invalid.closes_attempt);
    assert!(ninth_invalid.next_state.closed);
    assert_eq!(ninth_invalid.next_state.shape_invalid_count, 9);
    assert!(matches!(
        ninth_invalid.outcome,
        DecisionKind::FieldErrors(_)
    ));
    let replay = submit(&eighth_invalid.next_state, "bad-1", &bad, &scope);
    assert_eq!(replay.next_state.shape_invalid_count, 8);
    assert!(!replay.closes_attempt);
    let conflict = submit(
        &eighth_invalid.next_state,
        "bad-1",
        &canonical_bytes(&json!({
            "kind": "proposal",
            "summary": "different",
            "claims": []
        }))
        .unwrap(),
        &scope,
    );
    assert!(matches!(conflict.outcome, DecisionKind::SubmissionConflict));
    assert_eq!(conflict.next_state.shape_invalid_count, 8);
    assert_eq!(conflict.next_state.invalid_count, 0);
    assert!(!conflict.closes_attempt);

    let mut repair = roundtable_protocol::SubmissionState::open();
    for index in 1..=3 {
        repair = submit(&repair, &format!("repair-{index}"), &bad, &scope).next_state;
    }
    let good = minimal_proposal();
    let sealed = submit(&repair, "seal-after-3", &good, &scope);
    let receipt = staged(&sealed);
    assert_eq!(receipt.state, CandidateState::Staged);
    assert!(!sealed.closes_attempt);
    assert!(!sealed.next_state.closed);
    assert_eq!(sealed.next_state.shape_invalid_count, 3);
    assert_eq!(sealed.next_state.invalid_count, 0);
    assert!(sealed.next_state.sealed.is_some());
    let again = submit(&sealed.next_state, "seal-after-3", &good, &scope);
    assert_eq!(staged(&again), receipt);
    assert!(!again.closes_attempt);

    let mut fresh = roundtable_protocol::SubmissionState::open();
    let sealed = submit(&fresh, "seal-1", &good, &scope);
    let receipt = staged(&sealed);
    fresh = sealed.next_state.clone();
    assert_eq!(receipt.state, CandidateState::Staged);
    assert_eq!(receipt.submission_id, sid("seal-1"));
    let spaced = spaced_copy(&good);
    assert_ne!(spaced, good);
    let idempotent = submit(&fresh, "seal-1", &spaced, &scope);
    assert_eq!(staged(&idempotent), receipt);
    let changed = submit(
        &fresh,
        "seal-1",
        &canonical_bytes(&json!({
            "kind": "proposal",
            "summary": "changed",
            "claims": [{
                "local_key": "c0",
                "text": "claim",
                "evidence_aliases": [],
                "confidence": "low"
            }]
        }))
        .unwrap(),
        &scope,
    );
    assert!(matches!(changed.outcome, DecisionKind::ResultAlreadySealed));
    let other = submit(&fresh, "seal-2", &good, &scope);
    assert!(matches!(other.outcome, DecisionKind::ResultAlreadySealed));
    assert_eq!(other.next_state.sealed.as_ref().unwrap(), &receipt);
    let rendered = format!("{sealed:?}{receipt:?}");
    assert!(!rendered.contains("Accepted"));
    assert!(!rendered.contains("normal_finish"));
    let semantic = canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "alias",
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": ["missing-alias"],
            "confidence": "low"
        }]
    }))
    .unwrap();
    let mut semantic_state = roundtable_protocol::SubmissionState::open();
    for index in 1..=3 {
        let decision = submit(&semantic_state, &format!("sem-{index}"), &semantic, &scope);
        assert!(!decision.closes_attempt, "semantic {index} stays open");
        semantic_state = decision.next_state;
    }
    assert_eq!(semantic_state.invalid_count, 3);
    assert_eq!(semantic_state.shape_invalid_count, 0);
    let fourth_semantic = submit(&semantic_state, "sem-4", &semantic, &scope);
    assert!(fourth_semantic.closes_attempt);
    assert_eq!(fourth_semantic.next_state.invalid_count, 4);
    assert!(matches!(
        fourth_semantic.outcome,
        DecisionKind::FieldErrors(_)
    ));
    let closed = submit(&fourth_semantic.next_state, "late", &good, &scope);
    assert!(matches!(closed.outcome, DecisionKind::AttemptClosed));
    assert!(!closed.closes_attempt);
    assert_eq!(closed.next_state.invalid_count, 4);
    assert!(closed.next_state.sealed.is_none());
}

#[test]
fn nested_missing_claim_field_names_its_path() {
    let scope = result_scope(PhaseKind::Proposal, 8 * 1024);
    let raw = canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "partial",
        "claims": [{}]
    }))
    .unwrap();
    let errors = validate_result(&raw, &scope).unwrap_err();
    for path in [
        "$.claims[0].local_key",
        "$.claims[0].text",
        "$.claims[0].evidence_aliases",
        "$.claims[0].confidence",
    ] {
        assert!(
            errors
                .iter()
                .any(|error| error.path == path && error.code == FieldCode::MissingField),
            "missing {path} in {errors:?}"
        );
    }
    let bad_confidence = canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "partial",
        "claims": [{
            "local_key": "c1",
            "text": "claim",
            "evidence_aliases": [],
            "confidence": "huge"
        }]
    }))
    .unwrap();
    assert_code(
        &bad_confidence,
        &scope,
        "$.claims[0].confidence",
        FieldCode::InvalidJson,
    );
    let schema = roundtable_protocol::submit_result_input_schema(PhaseKind::Proposal);
    let required = schema["properties"]["result"]["oneOf"][0]["required"]
        .as_array()
        .expect("proposal required");
    assert!(required.iter().any(|field| field == "kind"));
    assert!(required.iter().any(|field| field == "summary"));
    assert!(required.iter().any(|field| field == "claims"));
    let claim_required = schema["properties"]["result"]["oneOf"][0]["properties"]["claims"]
        ["items"]["required"]
        .as_array()
        .expect("claim required");
    assert!(claim_required.iter().any(|field| field == "local_key"));
    let synthesis = roundtable_protocol::submit_result_input_schema(PhaseKind::Synthesis);
    let properties = synthesis["properties"]["result"]["properties"]
        .as_object()
        .expect("synthesis properties");
    assert!(properties.contains_key("consensus_items"));
    assert!(!properties.contains_key("speaker_id"));
    assert!(!properties.contains_key("coverage"));
    let example = roundtable_protocol::seat_schema_example(PhaseKind::Proposal);
    assert!(example.contains("\"kind\":\"proposal\""));
    assert!(example.contains("local_key"));
}

#[test]
fn references_and_identity() {
    let fixture = fixture();
    assert_eq!(fixture.contract_profile, "roundtable_plan_1_2");
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(
        fixture.limits.default_quota_bytes,
        roundtable_protocol::v1_1::DEFAULT_RESULT_BYTES
    );
    assert_eq!(
        fixture.limits.hard_cap_bytes,
        roundtable_protocol::v1_1::MAX_RESULT_BYTES
    );
    assert_eq!(
        fixture.limits.max_claims,
        roundtable_protocol::v1_1::MAX_CLAIMS
    );
    assert_eq!(
        fixture.limits.max_responses,
        roundtable_protocol::v1_1::MAX_RESPONSES
    );
    assert_eq!(
        fixture.limits.max_open_questions,
        roundtable_protocol::v1_1::MAX_OPEN_QUESTIONS
    );
    assert_eq!(
        fixture.limits.max_position_changes,
        roundtable_protocol::v1_1::MAX_POSITION_CHANGES
    );
    assert_eq!(fixture.limits.min_claims, 1);
    assert_eq!(fixture.limits.abstain_claims, 0);

    for case in &fixture.cases {
        let phase = phase_kind(&case.phase_kind);
        let quota = case
            .quota_bytes
            .unwrap_or(fixture.limits.default_quota_bytes);
        let scope = result_scope(phase, quota);
        let raw = case.raw_bytes();
        let result = validate_result(&raw, &scope);
        if case.ok.unwrap_or(false) {
            let validated =
                result.unwrap_or_else(|errors| panic!("{} accepted: {errors:?}", case.name));
            assert_no_secret(&case.name, &validated);
            if case.name == "labeled_inference" {
                assert!(!validated.counts_toward_quorum);
                assert!(validated.moderator.is_some());
                assert_eq!(
                    validated.moderator.as_ref().unwrap().speaker_id,
                    scope.speaker_id
                );
                assert!(validated.moderator.as_ref().unwrap().coverage.is_none());
            }
            continue;
        }
        let errors = result.expect_err(&case.name);
        assert_readable(&errors);
        assert!(
            errors.iter().any(|error| {
                error.path == case.path.as_deref().unwrap()
                    && error.code.as_str() == case.code.as_deref().unwrap()
            }),
            "{} missing {} {}: {errors:?}",
            case.name,
            case.path.as_deref().unwrap(),
            case.code.as_deref().unwrap()
        );
    }

    let scope = result_scope(PhaseKind::Proposal, 8 * 1024);
    let empty_proposal = canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "",
        "claims": []
    }))
    .unwrap();
    assert!(validate_result(&empty_proposal, &scope).is_err());

    let exact = padded_proposal(8 * 1024, false);
    assert_eq!(exact.len(), 8 * 1024);
    let validated = validate_result(&exact, &scope).expect("8 KiB canonical fits");
    assert_eq!(validated.byte_len, 8 * 1024);
    let over = padded_proposal(8 * 1024 + 1, false);
    assert_eq!(over.len(), 8 * 1024 + 1);
    assert_code(&over, &scope, "$", FieldCode::ByteLimit);
    let mut spaced = exact.clone();
    spaced.insert(1, b' ');
    assert_eq!(spaced.len(), 8 * 1024 + 1);
    let spaced_ok = validate_result(&spaced, &scope).expect("whitespace is not canonical size");
    assert_eq!(spaced_ok.byte_len, 8 * 1024);

    let han_exact = padded_proposal(8 * 1024, true);
    assert_eq!(han_exact.len(), 8 * 1024);
    assert!(std::str::from_utf8(&han_exact).unwrap().contains('中'));
    assert!(validate_result(&han_exact, &scope).is_ok());
    let han_over = han_summary(3000);
    let han_text = String::from_utf8(han_over.clone()).unwrap();
    assert!(han_over.len() > 8 * 1024);
    assert!(han_text.chars().count() < 8 * 1024);
    assert_code(&han_over, &scope, "$", FieldCode::ByteLimit);

    let raised = result_scope(PhaseKind::Proposal, 64 * 1024);
    let cap = padded_proposal(64 * 1024, true);
    assert_eq!(cap.len(), 64 * 1024);
    assert_eq!(
        validate_result(&cap, &raised)
            .expect("64 KiB hard cap")
            .byte_len,
        64 * 1024
    );
    let past_cap = padded_proposal(64 * 1024 + 1, false);
    assert_eq!(past_cap.len(), 64 * 1024 + 1);
    let too_high = result_scope(PhaseKind::Proposal, 64 * 1024 + 1);
    assert_code(&past_cap, &too_high, "$", FieldCode::ByteLimit);
    assert_code(&past_cap, &raised, "$", FieldCode::ByteLimit);

    let wide = scope_with_alias(PhaseKind::Proposal, 64 * 1024);
    let claims_20 = counted(20, 0, 0);
    assert!(validate_result(&claims_20, &wide).is_ok());
    assert_code(&counted(21, 0, 0), &wide, "$.claims", FieldCode::ClaimCount);
    assert!(validate_result(&counted(1, 30, 0), &wide).is_ok());
    assert_code(
        &counted(1, 31, 0),
        &wide,
        "$.responses",
        FieldCode::ResponseCount,
    );
    assert!(validate_result(&counted(1, 0, 20), &wide).is_ok());
    assert_code(
        &counted(1, 0, 21),
        &wide,
        "$.open_questions",
        FieldCode::OpenQuestionCount,
    );

    let abstain = validate_result(
        &canonical_bytes(&json!({
            "kind": "abstain",
            "summary": "弃权",
            "reason": "没有依据",
            "claims": []
        }))
        .unwrap(),
        &scope,
    )
    .unwrap();
    assert!(abstain.abstained);
    assert!(!abstain.counts_toward_quorum);
    let proposal = validate_result(&minimal_proposal(), &scope).unwrap();
    assert!(!proposal.abstained);
    assert!(proposal.counts_toward_quorum);

    let mut linked = scope_with_alias(PhaseKind::Critique, 8 * 1024);
    linked.mandatory_targets = vec![RequiredTarget {
        claim_alias: "c-pub".to_string(),
        claim_id: support::id("claim-pub"),
        response_alias: None,
        response_id: None,
    }];
    assert_code(
        &critique_response(None, None, "challenge"),
        &linked,
        "$.responses",
        FieldCode::MandatoryTarget,
    );
    let covered = critique_response(Some("c-pub"), None, "challenge");
    assert!(validate_result(&covered, &linked).is_ok());
    assert_code(
        &critique_response(Some("own-claim"), None, "support"),
        &own_scope(),
        "$.responses[0]",
        FieldCode::SelfSupport,
    );
    assert_code(
        &json_bytes(&json!({
            "kind": "proposal",
            "summary": "peer",
            "claims": [{
                "local_key": "c0",
                "text": "claim",
                "evidence_aliases": ["e-peer"],
                "confidence": "medium"
            }]
        })),
        &peer_scope(),
        "$.claims[0].evidence_aliases[0]",
        FieldCode::PeerStagedEvidence,
    );

    let mut history_scope = own_scope();
    history_scope.aliases.claims.insert(
        "other-claim".to_string(),
        ClaimRef {
            claim_id: support::id("claim-other"),
            speaker_id: support::id("speaker-other"),
            published: true,
        },
    );
    history_scope.aliases.claims.insert(
        "staged-own".to_string(),
        ClaimRef {
            claim_id: support::id("claim-staged"),
            speaker_id: history_scope.speaker_id,
            published: false,
        },
    );
    assert_code(
        &position_change("other-claim", "c0"),
        &history_scope,
        "$.position_changes[0].own_prior_claim_alias",
        FieldCode::ClaimOwner,
    );
    assert_code(
        &position_change("staged-own", "c0"),
        &history_scope,
        "$.position_changes[0].own_prior_claim_alias",
        FieldCode::UnpublishedClaim,
    );
    assert_code(
        &position_change("own-claim", "missing"),
        &history_scope,
        "$.position_changes[0].new_local_claim_key",
        FieldCode::MissingLocalClaim,
    );
    assert!(validate_result(&position_change("own-claim", "c0"), &history_scope).is_ok());
    assert_code(
        &counted_position_changes(21),
        &history_scope,
        "$.position_changes",
        FieldCode::PositionChangeCount,
    );
    assert!(validate_result(&counted_position_changes(20), &history_scope).is_ok());

    let token = "rt-7f3a9c-secret";
    let leaked = json_bytes(&json!({
        "kind": "proposal",
        "summary": token,
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": [token],
            "confidence": "low"
        }]
    }));
    let errors = validate_result(&leaked, &scope).unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.code == FieldCode::UnknownAlias));
    for error in &errors {
        assert!(!error.path.contains(token));
        assert!(!error.message.contains(token));
        assert!(!error.code.as_str().contains(token));
    }
}

#[test]
fn consensus_needs_distinct_support_edges() {
    let published = support_history();
    let unsupported = consensus(AgreementLevel::ExplicitAgreement, &[], &[]);
    assert!(validate_consensus(&unsupported, &published).is_err());
    let supported = consensus(
        AgreementLevel::ExplicitAgreement,
        &["speaker-1", "speaker-2"],
        &["response-1", "response-2"],
    );
    assert!(validate_consensus(&supported, &published).is_ok());
    assert!(validate_consensus(
        &consensus(
            AgreementLevel::ExplicitAgreement,
            &["speaker-1", "speaker-1"],
            &["response-1", "response-1b"]
        ),
        &published
    )
    .is_err());
    assert!(validate_consensus(
        &consensus(
            AgreementLevel::ExplicitAgreement,
            &["speaker-1", "speaker-3"],
            &["response-1", "response-3"]
        ),
        &published
    )
    .is_err());
    assert!(validate_consensus(
        &consensus(
            AgreementLevel::ExplicitAgreement,
            &["speaker-9"],
            &["response-missing"]
        ),
        &published
    )
    .is_err());
    assert!(validate_consensus(
        &consensus(
            AgreementLevel::ExplicitAgreement,
            &["speaker-4", "speaker-5"],
            &["response-4", "response-5"]
        ),
        &published
    )
    .is_err());
    assert!(validate_consensus(
        &consensus(
            AgreementLevel::ExplicitAgreement,
            &["speaker-6"],
            &["response-6"]
        ),
        &published
    )
    .is_err());
    assert!(validate_consensus(
        &consensus(
            AgreementLevel::ExplicitAgreement,
            &["speaker-abstain"],
            &["response-abstain"]
        ),
        &published
    )
    .is_err());
    let compatible = consensus(AgreementLevel::CompatiblePositions, &[], &[]);
    assert!(validate_consensus(&compatible, &published).is_ok());
    let unresolved = consensus(AgreementLevel::Unresolved, &[], &[]);
    assert!(validate_consensus(&unresolved, &published).is_ok());

    let mut scope = result_scope(PhaseKind::Synthesis, 8 * 1024);
    scope
        .aliases
        .speakers
        .insert("s1".to_string(), support::id::<SpeakerId>("speaker-1"));
    scope.aliases.responses.insert(
        "r1".to_string(),
        ResponseRef {
            response_id: support::id("response-1"),
            speaker_id: support::id("speaker-1"),
            published: true,
        },
    );
    let raw = json_bytes(&json!({
        "kind": "synthesis",
        "recommendation": {
            "text": "有引用",
            "aliases": [{"kind": "claim", "alias": "c-target"}],
            "inference": false
        },
        "alternatives": [],
        "consensus_items": [{
            "text": "显式同意",
            "agreement_level": "explicit_agreement",
            "aliases": [],
            "supporter_aliases": ["s1"],
            "support_response_aliases": ["r1"],
            "inference": true
        }],
        "disagreements": [],
        "risks": [],
        "decision_requests": []
    }));
    scope.aliases.claims.insert(
        "c-target".to_string(),
        ClaimRef {
            claim_id: support::id("claim-target"),
            speaker_id: support::id("speaker-0"),
            published: true,
        },
    );
    let errors = validate_result(&raw, &scope).unwrap_err();
    assert!(errors
        .iter()
        .any(|error| error.code == FieldCode::ConsensusSupport));
    assert_readable(&errors);
}

fn staged(
    decision: &roundtable_protocol::SubmissionDecision,
) -> roundtable_protocol::CandidateReceipt {
    match &decision.outcome {
        DecisionKind::Staged(receipt) => receipt.clone(),
        other => panic!("expected staged candidate, got {other:?}"),
    }
}

fn submit(
    state: &roundtable_protocol::SubmissionState,
    id: &str,
    raw: &[u8],
    scope: &ResultScope,
) -> roundtable_protocol::SubmissionDecision {
    submit_candidate(state, &sid(id), raw, scope)
}

fn sid(text: &str) -> SubmissionId {
    SubmissionId::from_str(text).expect(text)
}

fn minimal_proposal() -> Vec<u8> {
    canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "成员摘要",
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": [],
            "confidence": "low"
        }]
    }))
    .unwrap()
}

fn spaced_copy(canonical: &[u8]) -> Vec<u8> {
    let mut copy = canonical.to_vec();
    copy.insert(1, b' ');
    copy
}

fn result_scope(kind: PhaseKind, quota_bytes: u32) -> ResultScope {
    ResultScope {
        phase_kind: kind,
        speaker_id: support::id("speaker-self"),
        aliases: VisibleAliases::default(),
        mandatory_targets: Vec::new(),
        published: PublishedHistory::default(),
        quota_bytes,
    }
}

fn scope_with_alias(kind: PhaseKind, quota_bytes: u32) -> ResultScope {
    let mut scope = result_scope(kind, quota_bytes);
    scope.aliases.claims.insert(
        "c-pub".to_string(),
        ClaimRef {
            claim_id: support::id("claim-pub"),
            speaker_id: support::id("speaker-other"),
            published: true,
        },
    );
    scope
}

fn own_scope() -> ResultScope {
    let mut scope = scope_with_alias(PhaseKind::Critique, 64 * 1024);
    scope.aliases.claims.insert(
        "own-claim".to_string(),
        ClaimRef {
            claim_id: support::id("claim-own"),
            speaker_id: scope.speaker_id,
            published: true,
        },
    );
    scope
}

fn peer_scope() -> ResultScope {
    let mut scope = result_scope(PhaseKind::Proposal, 8 * 1024);
    scope.aliases.evidence.insert(
        "e-peer".to_string(),
        EvidenceRef {
            evidence_id: support::id::<EvidenceId>("evidence-peer"),
            visibility: AliasVisibility::PeerStaged,
        },
    );
    scope
}

fn padded_proposal(target: usize, han: bool) -> Vec<u8> {
    let seed = if han { "中" } else { "s" };
    let base = proposal_summary(seed);
    assert!(base.len() <= target, "seed {} exceeds {target}", base.len());
    proposal_summary(&format!("{seed}{}", "a".repeat(target - base.len())))
}

fn proposal_summary(summary: &str) -> Vec<u8> {
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
    .unwrap()
}

fn han_summary(count: usize) -> Vec<u8> {
    proposal_summary(&"中".repeat(count))
}

fn counted(claims: usize, responses: usize, questions: usize) -> Vec<u8> {
    let claims: Vec<_> = (0..claims)
        .map(|index| {
            json!({
                "local_key": format!("c{index}"),
                "text": "t",
                "evidence_aliases": [],
                "confidence": "low"
            })
        })
        .collect();
    let responses: Vec<_> = (0..responses)
        .map(|_| {
            json!({
                "target_claim_alias": "c-pub",
                "stance": "challenge",
                "priority": "normal",
                "text": "r",
                "evidence_aliases": []
            })
        })
        .collect();
    let open_questions: Vec<_> = (0..questions).map(|index| format!("q{index}")).collect();
    canonical_bytes(&json!({
        "kind": "proposal",
        "summary": "s",
        "claims": claims,
        "responses": responses,
        "open_questions": open_questions
    }))
    .unwrap()
}

fn critique_response(
    claim_alias: Option<&str>,
    response_alias: Option<&str>,
    stance: &str,
) -> Vec<u8> {
    let mut response = json!({
        "stance": stance,
        "priority": "normal",
        "text": "response",
        "evidence_aliases": []
    });
    if let Some(alias) = claim_alias {
        response["target_claim_alias"] = json!(alias);
    }
    if let Some(alias) = response_alias {
        response["target_response_alias"] = json!(alias);
    }
    canonical_bytes(&json!({
        "kind": "critique",
        "summary": "评议",
        "claims": [{
            "local_key": "c0",
            "text": "claim",
            "evidence_aliases": [],
            "confidence": "high"
        }],
        "responses": [response]
    }))
    .unwrap()
}

fn position_change(prior: &str, new_key: &str) -> Vec<u8> {
    canonical_bytes(&json!({
        "kind": "critique",
        "summary": "改变",
        "claims": [{
            "local_key": "c0",
            "text": "replacement",
            "evidence_aliases": [],
            "confidence": "medium"
        }],
        "responses": [{
            "target_claim_alias": "c-pub",
            "stance": "revise",
            "priority": "critical",
            "text": "cover",
            "evidence_aliases": []
        }],
        "position_changes": [{
            "own_prior_claim_alias": prior,
            "new_local_claim_key": new_key,
            "reason": "因为评议",
            "trigger_response_aliases": []
        }]
    }))
    .unwrap()
}

fn counted_position_changes(count: usize) -> Vec<u8> {
    let changes: Vec<_> = (0..count)
        .map(|_| {
            json!({
                "own_prior_claim_alias": "own-claim",
                "new_local_claim_key": "c0",
                "reason": "更新",
                "trigger_response_aliases": []
            })
        })
        .collect();
    canonical_bytes(&json!({
        "kind": "critique",
        "summary": "多次改变",
        "claims": [{
            "local_key": "c0",
            "text": "replacement",
            "evidence_aliases": [],
            "confidence": "low"
        }],
        "responses": [{
            "target_claim_alias": "c-pub",
            "stance": "clarify",
            "priority": "normal",
            "text": "cover",
            "evidence_aliases": []
        }],
        "position_changes": changes
    }))
    .unwrap()
}

fn support_history() -> PublishedHistory {
    PublishedHistory {
        phases: vec![PublishedPhase {
            phase_id: support::id::<PhaseId>("phase-published"),
            phase_index: 0,
            kind: PhaseKind::Proposal,
            publication_seq: Seq(1),
            members: vec![
                published_member(
                    "speaker-0",
                    MemberKind::Proposal,
                    PublicationStatus::Accepted,
                    0,
                    vec![],
                ),
                published_member(
                    "speaker-1",
                    MemberKind::Critique,
                    PublicationStatus::Accepted,
                    1,
                    vec![
                        published_response(
                            "response-1",
                            "speaker-1",
                            Stance::Support,
                            Some("claim-target"),
                        ),
                        published_response(
                            "response-1b",
                            "speaker-1",
                            Stance::Support,
                            Some("claim-target"),
                        ),
                    ],
                ),
                published_member(
                    "speaker-2",
                    MemberKind::Critique,
                    PublicationStatus::Accepted,
                    2,
                    vec![published_response(
                        "response-2",
                        "speaker-2",
                        Stance::Support,
                        Some("claim-target"),
                    )],
                ),
                published_member(
                    "speaker-3",
                    MemberKind::Critique,
                    PublicationStatus::Accepted,
                    4,
                    vec![published_response(
                        "response-3",
                        "speaker-3",
                        Stance::Support,
                        Some("claim-other"),
                    )],
                ),
                published_member(
                    "speaker-4",
                    MemberKind::Critique,
                    PublicationStatus::Accepted,
                    5,
                    vec![published_response(
                        "response-4",
                        "speaker-4",
                        Stance::Challenge,
                        Some("claim-target"),
                    )],
                ),
                published_member(
                    "speaker-5",
                    MemberKind::Critique,
                    PublicationStatus::Accepted,
                    6,
                    vec![published_response(
                        "response-5",
                        "speaker-5",
                        Stance::Challenge,
                        Some("claim-target"),
                    )],
                ),
                published_member(
                    "speaker-6",
                    MemberKind::Critique,
                    PublicationStatus::Accepted,
                    7,
                    vec![published_response(
                        "response-6",
                        "speaker-6",
                        Stance::Support,
                        Some("claim-target"),
                    )],
                ),
                published_member(
                    "speaker-abstain",
                    MemberKind::Abstain,
                    PublicationStatus::Abstained,
                    8,
                    vec![published_response(
                        "response-abstain",
                        "speaker-abstain",
                        Stance::Support,
                        Some("claim-target"),
                    )],
                ),
            ],
        }],
    }
}

fn published_member(
    speaker: &str,
    kind: MemberKind,
    status: PublicationStatus,
    ordinal: u32,
    responses: Vec<PublishedResponse>,
) -> PublishedMember {
    PublishedMember {
        speaker: SpeakerOrdinal {
            ordinal,
            speaker_id: support::id(speaker),
        },
        kind,
        summary: "published".to_string(),
        status,
        claims: vec![PublishedClaim {
            claim_id: if speaker == "speaker-0" {
                support::id("claim-target")
            } else {
                support::id(&format!("claim-{speaker}"))
            },
            text: "claim".to_string(),
        }],
        responses,
    }
}

fn published_response(
    label: &str,
    _speaker: &str,
    stance: Stance,
    claim: Option<&str>,
) -> PublishedResponse {
    PublishedResponse {
        response_id: support::id::<ResponseId>(label),
        publication_seq: Seq(2),
        stance,
        priority: Priority::Normal,
        target_claim_id: claim.map(support::id::<ClaimId>),
        target_response_id: None,
    }
}

fn consensus(level: AgreementLevel, supporters: &[&str], responses: &[&str]) -> ConsensusItemV1 {
    ConsensusItemV1 {
        text: "agreement".to_string(),
        agreement_level: level,
        aliases: vec![AliasRefV1 {
            kind: AliasKind::Claim,
            alias: "ignored-by-direct-check".to_string(),
        }],
        supporter_aliases: supporters
            .iter()
            .map(|label| support::id::<SpeakerId>(label).to_string())
            .collect(),
        support_response_aliases: responses
            .iter()
            .map(|label| support::id::<ResponseId>(label).to_string())
            .collect(),
        inference: false,
    }
}

fn assert_code(raw: &[u8], scope: &ResultScope, path: &str, code: FieldCode) {
    let errors = validate_result(raw, scope).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.path == path && error.code == code),
        "missing {path} {code:?} in {errors:?}"
    );
    assert_readable(&errors);
}

fn assert_readable(errors: &[FieldError]) {
    assert!(!errors.is_empty());
    for error in errors {
        assert!(FieldCode::ALL.contains(&error.code));
        assert!(!error.code.as_str().contains(' '));
        assert!(!error.message.is_empty());
        assert!(error.message.chars().any(|ch| ch.is_ascii_alphabetic()));
        assert!(!error.path.contains("rt-7f3a9c-secret"));
        assert!(!error.message.contains("rt-7f3a9c-secret"));
    }
}

fn assert_no_secret(name: &str, validated: &ValidatedResult) {
    let rendered = format!("{validated:?}");
    assert!(!rendered.contains("rt-7f3a9c-secret"), "{name}");
}

fn json_bytes(value: &Value) -> Vec<u8> {
    canonical_bytes(value).unwrap()
}

fn phase_kind(text: &str) -> PhaseKind {
    match text {
        "proposal" => PhaseKind::Proposal,
        "critique" => PhaseKind::Critique,
        "synthesis" => PhaseKind::Synthesis,
        other => panic!("unknown phase {other}"),
    }
}

fn fixture() -> FixtureFile {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/roundtable/fixtures/result-counterexamples.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).expect("result counterexamples")
}

#[derive(Debug, Deserialize)]
struct FixtureFile {
    contract_profile: String,
    schema_version: u32,
    limits: FixtureLimits,
    cases: Vec<FixtureCase>,
}

#[derive(Debug, Deserialize)]
struct FixtureLimits {
    default_quota_bytes: u32,
    hard_cap_bytes: u32,
    min_claims: usize,
    max_claims: usize,
    abstain_claims: usize,
    max_responses: usize,
    max_open_questions: usize,
    max_position_changes: usize,
    max_repairable_invalid_submissions: u32,
}

#[derive(Debug, Deserialize)]
struct FixtureCase {
    name: String,
    phase_kind: String,
    #[serde(default)]
    quota_bytes: Option<u32>,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    raw: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    ok: Option<bool>,
}

impl FixtureCase {
    fn raw_bytes(&self) -> Vec<u8> {
        if let Some(raw) = &self.raw {
            return raw.as_bytes().to_vec();
        }
        canonical_bytes(self.payload.as_ref().expect("payload")).unwrap()
    }
}
