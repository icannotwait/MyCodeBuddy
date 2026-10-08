//! Review regressions use the protocol crate without linking the application.

#[path = "../../src/roundtable/paging.rs"]
mod paging;
#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

use roundtable_protocol::*;

fn request(revision: u64, ordinal: u32, kind: ReservationKind) -> ReservationRequest {
    ReservationRequest {
        revision,
        slot: AttemptSlot::Member {
            phase_index: 0,
            ordinal,
        },
        kind,
        observation: DispatchObservation::Admitted,
        now: MonoMs(0),
        deadline: MonoMs(1),
    }
}

#[test]
fn launch_caps_belong_to_each_turn_and_reopen_keeps_room_cost() {
    let mut state = BudgetState::fresh(3, 0, 3, budget_plan(3, 0, 3, Timeouts::default()).unwrap());
    for ordinal in 0..3 {
        state = reserve_attempts(&state, request(1, ordinal, ReservationKind::FirstLaunch))
            .expect("every member can launch in one phase revision")
            .state;
    }
    state = reserve_attempts(&state, request(2, 0, ReservationKind::FirstLaunch))
        .expect("reopening gives the turn a new first launch")
        .state;
    assert_eq!(state.consumed_prompts, 4);
    assert!(reserve_attempts(&state, request(2, 1, ReservationKind::FirstLaunch)).is_ok());
}

#[test]
fn old_revision_cannot_restore_first_launch_reservations() {
    let state = BudgetState::fresh(3, 0, 3, budget_plan(3, 0, 3, Timeouts::default()).unwrap());
    let state = reserve_attempts(&state, request(2, 0, ReservationKind::FirstLaunch))
        .unwrap()
        .state;
    assert!(reserve_attempts(&state, request(1, 1, ReservationKind::FirstLaunch)).is_err());
}

#[test]
fn retries_keep_unstarted_members_future_phases_and_moderator_funded() {
    let mut state = BudgetState::fresh(3, 0, 3, budget_plan(3, 0, 3, Timeouts::default()).unwrap());
    for ordinal in 0..3 {
        state = reserve_attempts(&state, request(1, ordinal, ReservationKind::FirstLaunch))
            .unwrap()
            .state;
    }
    state = reserve_attempts(&state, request(1, 0, ReservationKind::OptionalRetry))
        .unwrap()
        .state;
    assert_eq!(
        reserve_attempts(&state, request(1, 1, ReservationKind::OptionalRetry))
            .unwrap_err()
            .code,
        ErrorCode::InsufficientBudget
    );
    assert_eq!(state.consumed_prompts, 4);
}

fn measurement(epoch: u64, value: u64, dedupe: &str) -> MeasurementV1 {
    MeasurementV1 {
        unit: "token".into(),
        source: "provider-a".into(),
        scope: "request".into(),
        semantics: MeasureSemantics::Cumulative,
        counter_id: "output_tokens".into(),
        epoch,
        seq: None,
        dedupe: dedupe.into(),
        value: Some(value),
        observed_at: "2026-10-05T00:00:00Z".into(),
        attributed: true,
        trusted_reset: false,
        billable: true,
    }
}

#[test]
fn cumulative_usage_adds_epoch_deltas_and_independent_counter_identities() {
    let mut usage = fold_measurement(&UsageState::empty(), measurement(1, 20, "a"));
    usage = fold_measurement(&usage, measurement(2, 5, "b"));
    assert_eq!(usage.confirmed_output_tokens, Some(25));
    usage = fold_measurement(&usage, measurement(1, 25, "late"));
    assert_eq!(usage.confirmed_output_tokens, Some(30));
    let mut independent = measurement(1, 7, "a");
    independent.source = "provider-b".into();
    usage = fold_measurement(&usage, independent);
    assert_eq!(usage.confirmed_output_tokens, Some(37));
    let mut reset = measurement(2, 3, "reset");
    reset.trusted_reset = true;
    usage = fold_measurement(&usage, reset.clone());
    assert_eq!(usage.confirmed_output_tokens, Some(40));
    reset.value = Some(6);
    reset.dedupe = "after-reset".into();
    reset.trusted_reset = false;
    usage = fold_measurement(&usage, reset);
    assert_eq!(usage.confirmed_output_tokens, Some(43));
}

#[test]
fn fixed_manifest_cursor_rejects_malformed_foreign_and_out_of_range_offsets() {
    let entries = vec!["first".to_owned(), "second".to_owned()];
    let hash = Hash256::sha256(b"one manifest");
    let page = paging::read_manifest_page(&entries, hash, None, 1).unwrap();
    assert_eq!(page.limit, 1);
    assert_eq!(page.body_hash, hash);
    assert!(
        paging::read_manifest_page(&entries, Hash256::sha256(b"other"), Some(&page.cursor), 1)
            .is_err()
    );
    for cursor in ["junk", "3", "18446744073709551615"] {
        let result = std::panic::catch_unwind(|| {
            paging::read_manifest_page(&entries, hash, Some(cursor), u32::MAX)
        });
        assert!(result.is_ok(), "cursor must not panic: {cursor}");
        assert!(
            result.unwrap().is_err(),
            "invalid cursor must not restart: {cursor}"
        );
    }
    assert!(paging::read_manifest_page(&entries, hash, None, 0).is_err());
}

#[test]
fn bound_cursor_offsets_are_canonical_and_never_panic() {
    let entries = vec!["first".to_owned(), "second".to_owned()];
    let hash = Hash256::sha256(b"manifest");
    for offset in [
        "+1",
        "01",
        "3",
        "18446744073709551615",
        "18446744073709551616",
    ] {
        let cursor = format!("v1:{}:{offset}", hash.to_hex());
        let result = std::panic::catch_unwind(|| {
            paging::read_manifest_page(&entries, hash, Some(&cursor), 1)
        });
        assert!(result.is_ok());
        assert!(result.unwrap().is_err(), "invalid offset: {offset}");
    }
}

#[test]
fn opaque_cursors_bind_all_authorized_scope_and_expire() {
    let scope = paging::CursorScope {
        principal_id: support::id("operator"),
        room_id: support::id("room"),
        projection_id: support::id("projection"),
        manifest_id: support::id("manifest"),
    };
    let mut pool = paging::ScopedCursors::default();
    let token = pool.issue(scope.clone(), "v1:fixed:1".into(), 100).unwrap();
    assert_eq!(pool.resolve(&token, &scope, 101).unwrap(), "v1:fixed:1");
    let mut other = scope.clone();
    other.principal_id = support::id("other-operator");
    assert!(pool.resolve(&token, &other, 101).is_err());
    other = scope.clone();
    other.room_id = support::id("other-room");
    assert!(pool.resolve(&token, &other, 101).is_err());
    other = scope.clone();
    other.projection_id = support::id("other-projection");
    assert!(pool.resolve(&token, &other, 101).is_err());
    other = scope.clone();
    other.manifest_id = support::id("other-manifest");
    assert!(pool.resolve(&token, &other, 101).is_err());
    assert!(pool.resolve("forged", &scope, 101).is_err());
    assert!(pool.resolve(&token, &scope, 100 + 15 * 60 * 1000).is_err());
}

#[test]
fn gauge_is_not_billing_and_cumulative_seq_cannot_be_reused() {
    let mut first = measurement(1, 10, "first");
    first.seq = Some(1);
    let state = fold_measurement(&UsageState::empty(), first);
    let mut repeated = measurement(1, 20, "same-seq");
    repeated.seq = Some(1);
    assert_eq!(
        fold_measurement(&state, repeated).confirmed_output_tokens,
        Some(10)
    );
    let mut gauge = measurement(1, 100, "gauge");
    gauge.semantics = MeasureSemantics::Gauge;
    assert_eq!(
        fold_measurement(&state, gauge).confirmed_output_tokens,
        Some(10)
    );
}

#[test]
fn incremental_usage_is_deduplicated_and_non_token_units_do_not_bill_tokens() {
    let mut increment = measurement(1, 6, "increment");
    increment.semantics = MeasureSemantics::Incremental;
    let state = fold_measurement(&UsageState::empty(), increment.clone());
    assert_eq!(
        fold_measurement(&state, increment).confirmed_output_tokens,
        Some(6)
    );
    let mut bytes = measurement(1, 100, "bytes");
    bytes.unit = "byte".into();
    assert_eq!(
        fold_measurement(&state, bytes).confirmed_output_tokens,
        Some(6)
    );
    let mut occupancy = measurement(1, 0, "unknown-gauge");
    occupancy.semantics = MeasureSemantics::Gauge;
    occupancy.value = None;
    assert!(!fold_measurement(&state, occupancy).unknown_total);
}

fn aggregate() -> RoomAggregate {
    RoomAggregate {
        projection_id: support::id("projection"),
        body: {
            let mut body: serde_json::Value = serde_json::from_str(r#"{
            "schema_version":1,"room_id":"11111111-1111-4111-8111-111111111111",
            "revision":"1","run_epoch":"1","last_seq":"1","status":"running",
            "blocked_reason":null,"config_hash":"1111111111111111111111111111111111111111111111111111111111111111",
            "moderator_speaker_id":null,"phase_refs":[],"ledger_seq":"1",
            "sampled_active_ms":"0","sampled_at_utc":"2026-10-05T00:00:00Z"
        }"#).unwrap();
            body["messages"] = serde_json::json!([]);
            body["evidence_manifests"] = serde_json::json!([]);
            body["replay"] = serde_json::to_value(ProjectionReplayV1::default()).unwrap();
            serde_json::from_value(body).unwrap()
        },
        messages: vec![PublishedMessageRef {
            message_id: support::id("message"),
            hash: Hash256::sha256(b"body"),
        }],
        evidence_manifests: vec![support::id("manifest")],
        required_message_ids: vec![support::id("message")],
        required_evidence_manifests: vec![support::id("manifest")],
    }
}

#[test]
fn projection_hash_and_body_include_fixed_message_and_evidence_membership() {
    let first = aggregate();
    let projected = project(&first).unwrap();
    let body = serde_json::to_value(&projected.body).unwrap();
    assert_eq!(
        body["messages"][0]["message_id"],
        first.messages[0].message_id.to_string()
    );
    assert_eq!(
        body["evidence_manifests"][0],
        first.evidence_manifests[0].to_string()
    );
    let mut changed = first;
    changed.messages[0].hash = Hash256::sha256(b"other body");
    assert_ne!(
        projected.projection_ref.hash,
        project(&changed).unwrap().projection_ref.hash
    );
}

#[test]
fn fixed_manifest_pages_obey_the_byte_cap_and_advance_by_returned_entries() {
    let entries = vec!["first".repeat(120_000), "second".repeat(100_000)];
    let hash = Hash256::sha256(b"fixed-large-manifest");
    let first = paging::read_manifest_page(&entries, hash, None, 100).unwrap();
    assert_eq!(first.entries.len(), 1);
    assert!(first.entries == entries[..1]);
    assert!(first.cursor.ends_with(":1"));
    assert!(
        first.entries.iter().map(String::len).sum::<usize>() + 4096
            <= v1_1::MAX_PAGE_BYTES as usize
    );
    let second = paging::read_manifest_page(&entries, hash, Some(&first.cursor), 100).unwrap();
    assert_eq!(second.entries.len(), 1);
    assert!(second.entries == entries[1..]);
    assert!(second.cursor.ends_with(":2"));
    assert_eq!(second.body_hash, hash);
}

#[test]
fn oversized_manifest_entry_requires_object_paging_instead_of_truncation() {
    let entries = vec!["x".repeat(v1_1::MAX_PAGE_BYTES as usize)];
    let result = paging::read_manifest_page(&entries, Hash256::sha256(b"oversized"), None, 100);
    assert!(
        result.is_err(),
        "single oversized entry must not be emitted inline"
    );
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        error.details.reason.as_deref(),
        Some("object_page_required")
    );
}

#[test]
fn usage_view_roundtrips_confirmed_tokens_and_uncertainty() {
    let wire = serde_json::json!({
        "usage_version": "7",
        "measurements": [{ "key": "attempts", "value": 2 }],
        "totals": [{ "key": "active_ms", "value": 123 }],
        "unknown_count": 1,
        "confirmed_output_tokens": 42,
        "unknown_total": true,
        "uncertain": true,
    });
    let usage: UsageViewV1 = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(usage).unwrap(), wire);
}

#[test]
fn usage_view_defaults_missing_token_details_and_rejects_invalid_counts() {
    let wire = serde_json::json!({
        "usage_version": "0", "measurements": [], "totals": [], "unknown_count": 0
    });
    let usage: UsageViewV1 = serde_json::from_value(wire).unwrap();
    let mut normalized = serde_json::to_value(usage).unwrap();
    assert_eq!(
        normalized["confirmed_output_tokens"],
        serde_json::Value::Null
    );
    assert_eq!(normalized["unknown_total"], false);
    assert_eq!(normalized["uncertain"], false);
    for invalid in [
        serde_json::json!(-1),
        serde_json::json!(9_007_199_254_740_992u64),
    ] {
        normalized["confirmed_output_tokens"] = invalid;
        assert!(serde_json::from_value::<UsageViewV1>(normalized.clone()).is_err());
    }
}
