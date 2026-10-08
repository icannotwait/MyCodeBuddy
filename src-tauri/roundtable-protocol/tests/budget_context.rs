//! Attempt, time, and context-capacity rules.
//!
//! Expected budgets are the static appendix table. Nothing here calls
//! `budget_plan` to fill that table.

#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

use roundtable_protocol::{
    apply_time_checkpoint, bound_request, budget_plan, canonical_bytes, charge_interjection,
    check_business_projection_caps, checkpoint_business_projections, deadline_still_open,
    event_reserve, json_string_worst_bytes, member_result_body_bytes, preflight_context,
    preflight_exact_prompt, repair_coverage, reserve_attempts, room_active_ms, storage_reserve,
    AttemptSlot, BindingId, BudgetState, ContextBound, DeliveryEncoder, DispatchObservation,
    DurationMs, EncodedInputBounds, ErrorCode, Hash256, MonoMs, PhaseKind, PhaseSnapshotV1,
    QualifiedContextProfile, RequestTranscriptBound, ReservationKind, ReservationRequest, Revision,
    RoleSnapshot, SafeInt, Seq, TimeLedger, Timeouts, TokenBound, ToolExchange, ToolQuotaV1,
};

fn expect_static(n: u32, c: u32, r: u32) -> (u64, u64, u64, u64) {
    let (discussion_ms, rooms) = match (n, c) {
        (2, 1) => (900_000, [1_350_000, 2_250_000, 3_150_000, 5_850_000]),
        (2, 2) => (450_000, [900_000, 1_350_000, 1_800_000, 3_150_000]),
        (3, 1) => (1_350_000, [1_800_000, 3_150_000, 4_500_000, 8_550_000]),
        (3, 2) => (900_000, [1_350_000, 2_250_000, 3_150_000, 5_850_000]),
        (3, 3) => (450_000, [900_000, 1_350_000, 1_800_000, 3_150_000]),
        (7, 1) => (3_150_000, [3_600_000, 6_750_000, 9_900_000, 19_350_000]),
        (7, 2) => (1_800_000, [2_250_000, 4_050_000, 5_850_000, 11_250_000]),
        (7, 7) => (450_000, [900_000, 1_350_000, 1_800_000, 3_150_000]),
        _ => panic!("static table has no row for N={n} C={c}"),
    };
    let room_ms = match r {
        0 => rooms[0],
        1 => rooms[1],
        2 => rooms[2],
        5 => rooms[3],
        _ => panic!("static table has no R={r}"),
    };
    let (base_attempts, max_attempts) = match (n, r) {
        (2, 0) => (3, 4),
        (2, 1) => (5, 7),
        (2, 2) => (7, 9),
        (2, 5) => (13, 18),
        (3, 0) => (4, 5),
        (3, 1) => (7, 9),
        (3, 2) => (10, 14),
        (3, 5) => (19, 26),
        (7, 0) => (8, 11),
        (7, 1) => (15, 21),
        (7, 2) => (22, 30),
        (7, 5) => (43, 60),
        _ => panic!("static table has no attempts for N={n} R={r}"),
    };
    (discussion_ms, room_ms, base_attempts, max_attempts)
}

struct FakeTokens {
    capacity: Option<u64>,
}

#[test]
fn delivery_prompt_contains_frozen_phase_context() {
    let phase = phase_snapshot();
    let encoded = DeliveryEncoder::prompt_utf8(
        &phase,
        &role_snapshot(),
        &support::id::<BindingId>("binding"),
    )
    .unwrap();
    let prompt: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(prompt["phase"], serde_json::to_value(&phase).unwrap());
}

#[test]
fn delivery_context_is_hashed_and_capacity_checked_as_actual_bytes() {
    let phase = phase_snapshot();
    let role = role_snapshot();
    let binding = support::id::<BindingId>("binding");
    let context =
        serde_json::json!({"topic":"圆桌", "sources":["frozen bytes"], "history":["published"]});
    let capacity = 10_000_000;
    let tokens = FakeTokens {
        capacity: Some(capacity),
    };
    let profile = profile(capacity);
    let bytes = DeliveryEncoder::prompt_with_context(&phase, &role, &binding, &context).unwrap();
    let manifest =
        DeliveryEncoder::encode_with_context(&phase, &role, &binding, &context, &tokens, &profile)
            .unwrap();
    assert_eq!(manifest.prompt_hash, Hash256::sha256(&bytes));
    assert_eq!(manifest.prompt_bytes.0, bytes.len() as u64);
    let decoded: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded["context"], context);
    assert_eq!(decoded["metadata"]["role"], role.role);
    let changed = serde_json::json!({"topic":"different"});
    assert_ne!(
        manifest.prompt_hash,
        DeliveryEncoder::encode_with_context(&phase, &role, &binding, &changed, &tokens, &profile)
            .unwrap()
            .prompt_hash
    );
    let huge = serde_json::json!({"topic":"x".repeat(capacity as usize)});
    assert_eq!(
        DeliveryEncoder::encode_with_context(&phase, &role, &binding, &huge, &tokens, &profile)
            .unwrap_err()
            .code,
        ErrorCode::ContextTooLarge
    );
}

impl TokenBound for FakeTokens {
    fn upper_bound(&self, utf8: &[u8]) -> roundtable_protocol::RtResult<u64> {
        u64::try_from(utf8.len()).map_err(|_| roundtable_protocol::RtError {
            code: ErrorCode::InvalidArgument,
            message: "The request is invalid.".to_string(),
            retryable: false,
            current_revision: None,
            details: roundtable_protocol::ErrorDetails {
                reason: Some("overflow".to_string()),
                field_errors: Vec::new(),
            },
        })
    }

    fn capacity_tokens(&self) -> Option<u64> {
        self.capacity
    }
}

fn profile(capacity: u64) -> QualifiedContextProfile {
    QualifiedContextProfile::proposed(
        "test-tokenizer",
        Hash256::from_bytes([0x44; 32]),
        capacity,
        0,
        "proof-test",
    )
}

fn input(embedded: &str) -> EncodedInputBounds {
    EncodedInputBounds {
        question: "问题".to_string(),
        role: "角色".to_string(),
        interjection: "插话".to_string(),
        evidence: "证据".to_string(),
        schema: "schema".to_string(),
        tools: "tools".to_string(),
        embedded: embedded.to_string(),
    }
}

fn request(
    kind: ReservationKind,
    revision: u64,
    slot: AttemptSlot,
    observation: DispatchObservation,
    now: u64,
    deadline: u64,
) -> ReservationRequest {
    ReservationRequest {
        kind,
        revision,
        slot,
        observation,
        now: MonoMs(now),
        deadline: MonoMs(deadline),
    }
}

#[test]
fn parameterized_budget_and_repair_slots() {
    let direct = budget_plan(3, 2, 3, Timeouts::default()).unwrap();
    assert_eq!((direct.base_attempts, direct.max_attempts), (10, 14));
    assert_eq!(
        (direct.slot_ms, direct.discussion_ms, direct.room_ms),
        (225_000, 450_000, 1_800_000)
    );
    let wide = budget_plan(7, 5, 2, Timeouts::default()).unwrap();
    assert_eq!(
        (wide.base_attempts, wide.max_attempts, wide.room_ms),
        (43, 60, 11_250_000)
    );

    for n in [2, 3, 7] {
        let mut concurrences = vec![1, 2, n];
        concurrences.sort_unstable();
        concurrences.dedup();
        for r in [0, 1, 2, 5] {
            for c in concurrences.iter().copied().filter(|c| *c <= n) {
                let (discussion_ms, room_ms, base_attempts, max_attempts) = expect_static(n, c, r);
                let plan = budget_plan(n, r, c, Timeouts::default()).unwrap();
                assert_eq!(plan.slot_ms, 225_000, "N={n} R={r} C={c}");
                assert_eq!(plan.discussion_ms, discussion_ms, "N={n} R={r} C={c}");
                assert_eq!(plan.room_ms, room_ms, "N={n} R={r} C={c}");
                assert_eq!(plan.base_attempts, base_attempts, "N={n} R={r} C={c}");
                assert_eq!(plan.max_attempts, max_attempts, "N={n} R={r} C={c}");
                let summed_member_slots = u64::from(n) * plan.slot_ms * 2;
                if c > 1 {
                    assert!(
                        plan.discussion_ms < summed_member_slots,
                        "concurrent slots must not be added into the discussion wall clock"
                    );
                }

                let mut config = support::config(n, r, c);
                config.quotas.output_byte_limit = SafeInt(1024);
                let capacity = 50_000_000;
                let bound = preflight_context(
                    &config,
                    &input("轨迹"),
                    &FakeTokens {
                        capacity: Some(capacity),
                    },
                    &profile(capacity),
                )
                .unwrap();
                assert!(
                    bound.verified_token_upper_bound + bound.generation_reserve_tokens <= capacity
                );
                assert_eq!(
                    bound.full_plan_reading_bytes,
                    roundtable_protocol::cumulative_result_reading_bytes(n, r, 1024).unwrap()
                );

                let mut state = BudgetState::fresh(n, r, c, plan.clone());
                let early = reserve_attempts(
                    &state,
                    request(
                        ReservationKind::OptionalRetry,
                        1,
                        AttemptSlot::Member {
                            phase_index: 0,
                            ordinal: 0,
                        },
                        DispatchObservation::Admitted,
                        0,
                        1,
                    ),
                )
                .unwrap_err();
                assert_eq!(early.code, ErrorCode::InvalidState);
                assert_eq!(state.consumed_prompts, 0);

                let revision = 1;
                for phase in 0..=r {
                    state.current_phase_index = phase;
                    for ordinal in 0..n {
                        let next = reserve_attempts(
                            &state,
                            request(
                                ReservationKind::FirstLaunch,
                                revision,
                                AttemptSlot::Member {
                                    phase_index: phase,
                                    ordinal,
                                },
                                DispatchObservation::Admitted,
                                0,
                                1,
                            ),
                        )
                        .unwrap();
                        assert!(next.consumed_prompt);
                        state = next.state;
                    }
                }
                state.current_phase_index = r + 1;
                let moderator = reserve_attempts(
                    &state,
                    request(
                        ReservationKind::FirstLaunch,
                        revision,
                        AttemptSlot::Moderator,
                        DispatchObservation::Admitted,
                        0,
                        1,
                    ),
                )
                .unwrap();
                state = moderator.state;
                assert_eq!(state.consumed_prompts, plan.base_attempts);
                let spare = plan.max_attempts - plan.base_attempts;
                for index in 0..spare {
                    let next = reserve_attempts(
                        &state,
                        request(
                            ReservationKind::OptionalRetry,
                            revision,
                            AttemptSlot::Member {
                                phase_index: (index / u64::from(n)) as u32,
                                ordinal: (index % u64::from(n)) as u32,
                            },
                            DispatchObservation::Admitted,
                            0,
                            1,
                        ),
                    )
                    .unwrap();
                    state = next.state;
                }
                assert_eq!(state.consumed_prompts, plan.max_attempts);
                let exhausted = reserve_attempts(
                    &state,
                    request(
                        ReservationKind::OptionalRetry,
                        revision,
                        AttemptSlot::Member {
                            phase_index: (spare / u64::from(n)) as u32,
                            ordinal: (spare % u64::from(n)) as u32,
                        },
                        DispatchObservation::Admitted,
                        0,
                        1,
                    ),
                )
                .unwrap_err();
                assert_eq!(exhausted.code, ErrorCode::InsufficientBudget);
            }
        }
    }

    assert!(budget_plan(2, 0, 3, Timeouts::default()).is_err());
    assert!(budget_plan(1, 0, 1, Timeouts::default()).is_err());

    let plan = budget_plan(3, 0, 1, Timeouts::default()).unwrap();
    assert_eq!((plan.base_attempts, plan.max_attempts), (4, 5));
    let mut state = BudgetState::fresh(3, 0, 1, plan.clone());
    state = reserve_attempts(
        &state,
        request(
            ReservationKind::FirstLaunch,
            1,
            AttemptSlot::Member {
                phase_index: 0,
                ordinal: 0,
            },
            DispatchObservation::Admitted,
            0,
            1,
        ),
    )
    .unwrap()
    .state;
    state = reserve_attempts(
        &state,
        request(
            ReservationKind::OptionalRetry,
            1,
            AttemptSlot::Member {
                phase_index: 0,
                ordinal: 0,
            },
            DispatchObservation::Admitted,
            0,
            1,
        ),
    )
    .unwrap()
    .state;
    assert_eq!(state.consumed_prompts, 2);
    assert_eq!(state.reserved_first_launches(), 3);
    let stolen = reserve_attempts(
        &state,
        request(
            ReservationKind::OptionalRetry,
            1,
            AttemptSlot::Member {
                phase_index: 0,
                ordinal: 0,
            },
            DispatchObservation::Admitted,
            0,
            1,
        ),
    )
    .unwrap_err();
    assert_eq!(stolen.code, ErrorCode::InvalidState);
    let still_first = reserve_attempts(
        &state,
        request(
            ReservationKind::FirstLaunch,
            1,
            AttemptSlot::Member {
                phase_index: 0,
                ordinal: 1,
            },
            DispatchObservation::Admitted,
            0,
            1,
        ),
    )
    .unwrap();
    assert!(still_first.consumed_prompt);
    assert_eq!(still_first.state.consumed_prompts, 3);

    let future = budget_plan(3, 1, 3, Timeouts::default()).unwrap();
    let mut future_state = BudgetState::fresh(3, 1, 3, future);
    for ordinal in 0..3 {
        future_state = reserve_attempts(
            &future_state,
            request(
                ReservationKind::FirstLaunch,
                1,
                AttemptSlot::Member {
                    phase_index: 0,
                    ordinal,
                },
                DispatchObservation::Admitted,
                0,
                1,
            ),
        )
        .unwrap()
        .state;
    }
    assert_eq!(future_state.reserved_first_launches(), 4);
    let future_retry = reserve_attempts(
        &future_state,
        request(
            ReservationKind::OptionalRetry,
            9,
            AttemptSlot::Member {
                phase_index: 1,
                ordinal: 0,
            },
            DispatchObservation::Admitted,
            0,
            1,
        ),
    )
    .unwrap_err();
    assert_eq!(future_retry.code, ErrorCode::InvalidState);

    let mut launches = BudgetState::fresh(3, 0, 1, plan.clone());
    for _ in 0..2 {
        let next = reserve_attempts(
            &launches,
            request(
                ReservationKind::FirstLaunch,
                1,
                AttemptSlot::Member {
                    phase_index: 0,
                    ordinal: 0,
                },
                DispatchObservation::ReservedNotEnqueued,
                0,
                1,
            ),
        )
        .unwrap();
        assert!(!next.consumed_prompt);
        assert!(!next.admission_counted);
        assert!(next.launch_counted);
        launches = next.state;
    }
    assert_eq!(launches.consumed_prompts, 0);
    assert_eq!(launches.launch_starts(1), 2);
    assert_eq!(launches.admissions(1), 0);
    assert_eq!(
        reserve_attempts(
            &launches,
            request(
                ReservationKind::FirstLaunch,
                1,
                AttemptSlot::Member {
                    phase_index: 0,
                    ordinal: 0,
                },
                DispatchObservation::ReservedNotEnqueued,
                0,
                1,
            ),
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidState
    );

    let mut admissions = BudgetState::fresh(3, 0, 1, plan.clone());
    for (ordinal, observation) in [
        (0, DispatchObservation::Admitted),
        (1, DispatchObservation::Cancelled { enqueued: true }),
    ] {
        admissions = reserve_attempts(
            &admissions,
            request(
                if ordinal == 0 {
                    ReservationKind::FirstLaunch
                } else {
                    ReservationKind::OptionalRetry
                },
                7,
                AttemptSlot::Member {
                    phase_index: 0,
                    ordinal: 0,
                },
                observation,
                0,
                1,
            ),
        )
        .unwrap()
        .state;
    }
    assert_eq!(admissions.admissions(7), 2);
    assert_eq!(admissions.consumed_prompts, 2);
    assert_eq!(
        reserve_attempts(
            &admissions,
            request(
                ReservationKind::OptionalRetry,
                7,
                AttemptSlot::Member {
                    phase_index: 0,
                    ordinal: 0,
                },
                DispatchObservation::Admitted,
                0,
                1,
            ),
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidState
    );

    let mut uncertain =
        BudgetState::fresh(2, 0, 1, budget_plan(2, 0, 1, Timeouts::default()).unwrap());
    let crashed = reserve_attempts(
        &uncertain,
        request(
            ReservationKind::FirstLaunch,
            1,
            AttemptSlot::Member {
                phase_index: 0,
                ordinal: 0,
            },
            DispatchObservation::Uncertain,
            0,
            1,
        ),
    )
    .unwrap();
    assert!(crashed.consumed_prompt);
    assert!(crashed.admission_counted);
    uncertain = crashed.state;
    assert_eq!(
        reserve_attempts(
            &uncertain,
            request(
                ReservationKind::FirstLaunch,
                1,
                AttemptSlot::Member {
                    phase_index: 0,
                    ordinal: 0,
                },
                DispatchObservation::Admitted,
                0,
                1,
            ),
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidState
    );
    let retry_after_uncertain = reserve_attempts(
        &uncertain,
        request(
            ReservationKind::OptionalRetry,
            1,
            AttemptSlot::Member {
                phase_index: 0,
                ordinal: 0,
            },
            DispatchObservation::Admitted,
            0,
            1,
        ),
    )
    .unwrap();
    assert!(retry_after_uncertain.consumed_prompt);
    assert_eq!(retry_after_uncertain.state.consumed_prompts, 2);

    let cancelled = BudgetState::fresh(2, 0, 2, budget_plan(2, 0, 2, Timeouts::default()).unwrap());
    let not_queued = reserve_attempts(
        &cancelled,
        request(
            ReservationKind::FirstLaunch,
            1,
            AttemptSlot::Member {
                phase_index: 0,
                ordinal: 1,
            },
            DispatchObservation::Cancelled { enqueued: false },
            0,
            1,
        ),
    )
    .unwrap();
    assert!(!not_queued.consumed_prompt);
    assert_eq!(not_queued.state.consumed_prompts, 0);
    assert_eq!(not_queued.state.launch_starts(1), 1);

    assert!(!deadline_still_open(MonoMs(5), MonoMs(5)));
    assert!(deadline_still_open(MonoMs(4), MonoMs(5)));
    let timed_out = reserve_attempts(
        &BudgetState::fresh(2, 0, 1, budget_plan(2, 0, 1, Timeouts::default()).unwrap()),
        request(
            ReservationKind::FirstLaunch,
            1,
            AttemptSlot::Moderator,
            DispatchObservation::Admitted,
            5,
            5,
        ),
    )
    .unwrap_err();
    assert_eq!(timed_out.code, ErrorCode::InvalidState);
    assert_eq!(
        timed_out.details.reason.as_deref(),
        Some("deadline_reached")
    );

    let parallel = budget_plan(3, 2, 3, Timeouts::default()).unwrap();
    let serial = budget_plan(3, 2, 1, Timeouts::default()).unwrap();
    assert!(serial.room_ms > parallel.room_ms);
    let lowered = repair_coverage(3, 2, 1, &serial, parallel.room_ms).unwrap_err();
    assert_eq!(lowered.code, ErrorCode::InsufficientBudget);
    let minimum = repair_coverage(3, 2, 3, &parallel, 900_000).unwrap();
    assert_eq!(minimum.configured_room_ms, 900_000);
    assert_eq!(minimum.funded_repair_slots, 0);
    assert_eq!(minimum.unfunded_repair_slots, parallel.base_attempts);
    let funded = repair_coverage(3, 2, 3, &parallel, parallel.room_ms).unwrap();
    assert_eq!(funded.configured_room_ms, parallel.room_ms);
    assert_eq!(funded.funded_repair_slots, parallel.base_attempts);
    assert_eq!(funded.unfunded_repair_slots, 0);

    assert_eq!(
        room_active_ms(&[(0, 225_000), (0, 225_000)]).unwrap(),
        225_000
    );
    assert_eq!(room_active_ms(&[(0, 100), (50, 150)]).unwrap(), 150);
    assert_eq!(room_active_ms(&[(0, 100), (200, 250)]).unwrap(), 150);

    let kept = launches.consumed_prompts;
    let mut reopened = launches.clone();
    reopened.current_phase_index = 0;
    let again = reserve_attempts(
        &reopened,
        request(
            ReservationKind::FirstLaunch,
            2,
            AttemptSlot::Member {
                phase_index: 0,
                ordinal: 2,
            },
            DispatchObservation::Admitted,
            0,
            1,
        ),
    )
    .unwrap();
    assert_eq!(again.state.consumed_prompts, kept + 1);
}

#[test]
fn capacity_includes_every_byte() {
    let mut config = support::config(3, 2, 3);
    config.quotas.output_byte_limit = SafeInt(1024);
    config.quotas.interjection_byte_limit = SafeInt(16 * 1024);
    let capacity = 50_000_000;
    let tokens = FakeTokens {
        capacity: Some(capacity),
    };
    let base_profile = profile(capacity);
    let base_input = input("base");
    let base = preflight_context(&config, &base_input, &tokens, &base_profile).unwrap();

    let mut question = base_input.clone();
    question.question.push('题');
    assert!(bound_tokens(&config, &question, &tokens, &base_profile) > admission(&base));

    let mut role = base_input.clone();
    role.role.push('角');
    assert!(bound_tokens(&config, &role, &tokens, &base_profile) > admission(&base));

    let mut interjection = base_input.clone();
    interjection.interjection.push('话');
    assert!(bound_tokens(&config, &interjection, &tokens, &base_profile) > admission(&base));

    let mut larger_result = config.clone();
    larger_result.quotas.output_byte_limit = SafeInt(2048);
    let grew_result =
        preflight_context(&larger_result, &base_input, &tokens, &base_profile).unwrap();
    assert!(grew_result.member_result_body_bytes > base.member_result_body_bytes);
    assert!(grew_result.future_bytes > base.future_bytes);
    assert!(admission(&grew_result) > admission(&base));

    let mut evidence = base_input.clone();
    evidence.evidence.push('摘');
    assert!(bound_tokens(&config, &evidence, &tokens, &base_profile) > admission(&base));

    let mut schema = base_input.clone();
    schema.schema.push('s');
    assert!(bound_tokens(&config, &schema, &tokens, &base_profile) > admission(&base));

    let mut tools = base_input.clone();
    tools.tools.push('t');
    assert!(bound_tokens(&config, &tools, &tokens, &base_profile) > admission(&base));

    let plain = input("a");
    let escaped = input("a\\");
    let plain_bound = preflight_context(&config, &plain, &tokens, &base_profile).unwrap();
    let escaped_bound = preflight_context(&config, &escaped, &tokens, &base_profile).unwrap();
    assert_eq!(escaped_bound.exact_bytes, plain_bound.exact_bytes + 2);
    assert!(admission(&escaped_bound) > admission(&plain_bound));

    let mut reserve = base_profile.clone();
    reserve.generation_reserve_tokens += 1;
    let grew_reserve = preflight_context(&config, &base_input, &tokens, &reserve).unwrap();
    assert!(admission(&grew_reserve) > admission(&base));
    assert_eq!(
        grew_reserve.generation_reserve_tokens,
        reserve.generation_reserve_tokens
    );

    let mut full = support::config(3, 2, 3);
    full.quotas.output_byte_limit = SafeInt(65_536);
    let full_bound = preflight_context(&full, &base_input, &tokens, &base_profile).unwrap();
    assert_eq!(full_bound.member_result_body_bytes, 589_824);
    assert_eq!(member_result_body_bytes(3, 2, 65_536).unwrap(), 589_824);
    assert!(full_bound.exact_bytes + full_bound.future_bytes > 589_824);
    assert_ne!(full_bound.future_bytes, 589_824);
    assert!(full_bound.future_bytes >= 589_824 * 6);
    assert_eq!(full_bound.full_plan_reading_bytes, 1_769_472);

    let fixture = "圆桌😀\\";
    let fixture_input = input(fixture);
    let fixture_bound = preflight_context(&config, &fixture_input, &tokens, &base_profile).unwrap();
    let prompt = preflight_exact_prompt(&config, &fixture_input).unwrap();
    assert_eq!(fixture_bound.exact_bytes, prompt.len() as u64);
    assert_ne!(fixture.chars().count() as u64, fixture.len() as u64);
    assert_ne!(fixture_bound.exact_bytes, fixture.chars().count() as u64);
    assert!(prompt
        .windows(4)
        .any(|window| window == [0xF0, 0x9F, 0x98, 0x80]));
    assert!(prompt.windows(2).any(|window| window == *br"\\"));

    let phase = phase_snapshot();
    let mut role_snapshot = role_snapshot();
    role_snapshot.role = fixture.to_string();
    let binding = support::id::<BindingId>("binding");
    let manifest =
        DeliveryEncoder::encode(&phase, &role_snapshot, &binding, &tokens, &base_profile).unwrap();
    let encoded = DeliveryEncoder::prompt_utf8(&phase, &role_snapshot, &binding).unwrap();
    assert_eq!(manifest.prompt_bytes, SafeInt(encoded.len() as u64));
    assert_eq!(manifest.binding_id, binding);
    assert!(encoded.windows(2).any(|window| window == *br"\\"));
    assert!(manifest.prior_cursor.is_none());

    let unknown = preflight_context(
        &config,
        &base_input,
        &FakeTokens { capacity: None },
        &base_profile,
    )
    .unwrap_err();
    assert_eq!(unknown.code, ErrorCode::CapacityUnknown);

    let mismatched = preflight_context(
        &config,
        &base_input,
        &FakeTokens {
            capacity: Some(capacity + 1),
        },
        &base_profile,
    )
    .unwrap_err();
    assert_eq!(mismatched.code, ErrorCode::CapacityUnknown);

    let tiny = profile(8);
    let too_large = preflight_context(
        &config,
        &base_input,
        &FakeTokens { capacity: Some(8) },
        &tiny,
    )
    .unwrap_err();
    assert_eq!(too_large.code, ErrorCode::ContextTooLarge);

    assert_eq!(
        charge_interjection(10, 8, 5).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        json_string_worst_bytes(u64::MAX).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        member_result_body_bytes(u32::MAX, u32::MAX, u64::MAX)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );

    let standard = QualifiedContextProfile::proposed(
        "test-tokenizer",
        Hash256::from_bytes([0x44; 32]),
        capacity,
        0,
        "proof-test",
    );
    let mut transcript = RequestTranscriptBound::empty();
    let invalid = ToolExchange::submit_field_errors("summary", "missing");
    for _ in 0..3 {
        transcript.record_exchange(&invalid, &standard).unwrap();
        let input_bytes = bound_request(&transcript, &standard).unwrap();
        assert!(input_bytes + standard.generation_reserve_tokens <= capacity);
    }
    let receipt = ToolExchange::receipt("sub-1");
    transcript.record_exchange(&receipt, &standard).unwrap();
    let after_valid = bound_request(&transcript, &standard).unwrap();
    assert!(after_valid >= invalid.reply.len() as u64 * 3 + receipt.reply.len() as u64);
    assert!(after_valid + standard.generation_reserve_tokens <= capacity);
    assert_eq!(transcript.tool_calls, 4);
    assert_eq!(transcript.model_requests, 4);

    let mut duplicate = RequestTranscriptBound::empty();
    duplicate.record_exchange(&receipt, &standard).unwrap();
    duplicate.record_exchange(&receipt, &standard).unwrap();
    assert_eq!(duplicate.tool_calls, 2);
    assert_eq!(duplicate.tool_reply_bytes, receipt.reply.len() as u64 * 2);
    let duplicate_bound = bound_request(&duplicate, &standard).unwrap();
    assert_eq!(duplicate_bound, receipt.reply.len() as u64 * 2);
    let mut one_reply = standard.clone();
    one_reply.max_tool_reply_bytes = receipt.reply.len() as u64;
    assert_eq!(
        bound_request(&duplicate, &one_reply).unwrap_err().code,
        ErrorCode::ContextTooLarge
    );

    let empty = ToolExchange::search_empty();
    let failed = ToolExchange::search_error();
    assert!(!empty.reply.is_empty());
    assert!(!failed.reply.is_empty());
    let mut searches = RequestTranscriptBound::empty();
    searches.record_exchange(&empty, &standard).unwrap();
    let with_empty = bound_request(&searches, &standard).unwrap();
    searches.record_exchange(&failed, &standard).unwrap();
    assert!(bound_request(&searches, &standard).unwrap() > with_empty);
    assert_eq!(searches.tool_calls, 2);

    let mut maxed = RequestTranscriptBound::empty();
    maxed.model_requests = 64;
    maxed.tool_calls = 128;
    maxed.generated_utf8_bytes = 262_144;
    maxed.tool_reply_bytes = 131_072;
    maxed.evidence_attempt_bytes = 32_768;
    maxed.adapter_bytes = 1_048_576 - 131_072;
    assert_eq!(bound_request(&maxed, &standard).unwrap(), 1_048_576);
    maxed.adapter_bytes += 1;
    assert_eq!(
        bound_request(&maxed, &standard).unwrap_err().code,
        ErrorCode::ContextTooLarge
    );
    maxed.adapter_bytes -= 1;
    maxed.model_requests = 65;
    assert!(bound_request(&maxed, &standard).is_err());
    maxed.model_requests = 64;
    maxed.tool_calls = 129;
    assert!(bound_request(&maxed, &standard).is_err());
    maxed.tool_calls = 128;
    maxed.generated_utf8_bytes = 262_145;
    assert!(bound_request(&maxed, &standard).is_err());
    maxed.generated_utf8_bytes = 262_144;
    maxed.tool_reply_bytes = 131_073;
    assert!(bound_request(&maxed, &standard).is_err());

    let mut evidence_call = RequestTranscriptBound::empty();
    let legal_evidence = ToolExchange::evidence(8_192);
    evidence_call
        .record_exchange(&legal_evidence, &standard)
        .unwrap();
    assert!(evidence_over_call(&standard));
    assert_eq!(
        record_evidence_total(&standard, 32_769).unwrap_err().code,
        ErrorCode::ContextTooLarge
    );
    assert!(record_evidence_total(&standard, 32_768).is_ok());

    let mut overflow = RequestTranscriptBound::empty();
    overflow.adapter_bytes = u64::MAX;
    overflow.prior_output_bytes = 1;
    assert_eq!(
        bound_request(&overflow, &standard).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

fn admission(bound: &ContextBound) -> u64 {
    bound
        .verified_token_upper_bound
        .checked_add(bound.generation_reserve_tokens)
        .unwrap()
}

fn bound_tokens(
    config: &roundtable_protocol::RoundtableConfigV1,
    input: &EncodedInputBounds,
    tokens: &FakeTokens,
    profile: &QualifiedContextProfile,
) -> u64 {
    admission(&preflight_context(config, input, tokens, profile).unwrap())
}

fn evidence_over_call(profile: &QualifiedContextProfile) -> bool {
    let mut transcript = RequestTranscriptBound::empty();
    transcript
        .record_exchange(&ToolExchange::evidence(8_193), profile)
        .is_err()
}

fn record_evidence_total(
    profile: &QualifiedContextProfile,
    total: u64,
) -> roundtable_protocol::RtResult<()> {
    let mut transcript = RequestTranscriptBound::empty();
    let mut remaining = total;
    while remaining > 0 {
        let chunk = remaining.min(8_192);
        transcript.record_exchange(&ToolExchange::evidence(chunk), profile)?;
        remaining -= chunk;
    }
    Ok(())
}

fn phase_snapshot() -> PhaseSnapshotV1 {
    PhaseSnapshotV1 {
        schema_version: 1,
        phase_id: support::id("phase"),
        phase_index: 0,
        revision: Revision(1),
        kind: PhaseKind::Proposal,
        critique_round: None,
        config_version: Revision(1),
        question_version: Revision(1),
        interjection_version: Revision(1),
        source_manifest_id: support::id("manifest"),
        source_manifest_hash: Hash256::from_bytes([0x11; 32]),
        published_messages: Vec::new(),
        members: Vec::new(),
        mandatory_targets: Vec::new(),
        policy_hash: Hash256::from_bytes([0x22; 32]),
        output_byte_limit: SafeInt(1024),
        tool_quota: ToolQuotaV1 {
            per_call_bytes: SafeInt(8192),
            per_attempt_bytes: SafeInt(32768),
        },
    }
}

fn role_snapshot() -> RoleSnapshot {
    RoleSnapshot {
        role: "角色".to_string(),
        model: "model-a".to_string(),
        effort: "low".to_string(),
        provider_ref: "provider:test".to_string(),
        prompt_version: "prompt-1".to_string(),
        template_version: "template-1".to_string(),
        schema_id: "schema-1".to_string(),
        schema_text: "schema".to_string(),
        tool_version: "tools-1".to_string(),
        tool_text: "tools".to_string(),
    }
}

#[test]
fn checkpoint_budget_fits_event_and_storage_limits() {
    let plan = budget_plan(7, 5, 2, Timeouts::default()).unwrap();
    assert_eq!(plan.room_ms, 11_250_000);
    let events = event_reserve(plan.max_attempts, 5).unwrap();
    assert_eq!(
        events.ordinary_events,
        12 + 8 * 60 + 6 * (5 + 2) + 8 * 32 + 64
    );
    assert_eq!(events.terminal_events, 128);
    assert_eq!(events.total_events, events.ordinary_events + 128);
    assert!(events.total_events <= 10_000);
    assert_ne!(events.total_events, 11_250);
    let storage = storage_reserve(&events, 2).unwrap();
    assert!(storage.projection_bytes >= 128 * 64 * 1024);
    assert_eq!(storage.diagnostic_bytes, 2 * 64 * 1024);
    assert!(storage.metadata_bytes > 0);
    assert_eq!(
        storage.total_bytes,
        storage
            .projection_bytes
            .checked_add(storage.diagnostic_bytes)
            .unwrap()
            .checked_add(storage.metadata_bytes)
            .unwrap()
    );

    let mut ledger = TimeLedger {
        ledger_seq: Seq(0),
        remaining_room_ms: DurationMs(plan.room_ms),
        remaining_phase_ms: DurationMs(plan.room_ms),
        prepaid_until: MonoMs(0),
        last_sample_mono: MonoMs(0),
    };
    let mut projections = 0;
    let mut now = 0;
    while ledger.remaining_room_ms.0 > 0 {
        ledger = apply_time_checkpoint(&ledger, MonoMs(now), 1_000).unwrap();
        projections += checkpoint_business_projections(&ledger);
        now += 1_000;
    }
    assert_eq!(ledger.remaining_room_ms.0, 0);
    assert_eq!(ledger.remaining_phase_ms.0, 0);
    assert_eq!(projections, 0);
    assert_ne!(projections, 11_250);
    assert_eq!(ledger.ledger_seq, Seq(11_250));
    assert_eq!(ledger.prepaid_until, MonoMs(11_250_000));
    let wire = canonical_bytes(&ledger).unwrap();
    let wire = String::from_utf8(wire).unwrap();
    assert!(!wire.contains("prepaid_until"));
    assert!(!wire.contains("last_sample_mono"));
    let still = storage_reserve(&events, 2).unwrap();
    assert!(still.total_bytes >= 128 * 64 * 1024);

    let overflow_add = budget_plan(
        2,
        0,
        1,
        Timeouts {
            launch_ms: u64::MAX - 10,
            prompt_ms: 20,
            validation_ms: 0,
            cleanup_ms: 0,
        },
    )
    .unwrap_err();
    assert_eq!(overflow_add.code, ErrorCode::InvalidArgument);
    let overflow_mul = budget_plan(u32::MAX, u32::MAX, 1, Timeouts::default()).unwrap_err();
    assert_eq!(overflow_mul.code, ErrorCode::InvalidArgument);
    let div_zero = budget_plan(2, 0, 0, Timeouts::default()).unwrap_err();
    assert_eq!(div_zero.code, ErrorCode::InvalidArgument);
    let underflow = apply_time_checkpoint(
        &TimeLedger {
            ledger_seq: Seq(1),
            remaining_room_ms: DurationMs(999),
            remaining_phase_ms: DurationMs(999),
            prepaid_until: MonoMs(0),
            last_sample_mono: MonoMs(0),
        },
        MonoMs(0),
        1_000,
    )
    .unwrap_err();
    assert_eq!(underflow.code, ErrorCode::InvalidArgument);
    assert_eq!(
        apply_time_checkpoint(&ledger, MonoMs(0), 1_001)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        event_reserve(u64::MAX, 0).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        check_business_projection_caps(9, 0, 0).unwrap_err().code,
        ErrorCode::CapabilityUnqualified
    );
    assert_eq!(
        check_business_projection_caps(9, 0, 0)
            .unwrap_err()
            .details
            .reason
            .as_deref(),
        Some("context_contract_violation")
    );
    assert!(check_business_projection_caps(8, 6, 8).is_ok());

    let refunded = roundtable_protocol::refund_known_unused(
        &TimeLedger {
            ledger_seq: Seq(2),
            remaining_room_ms: DurationMs(100),
            remaining_phase_ms: DurationMs(40),
            prepaid_until: MonoMs(1_000),
            last_sample_mono: MonoMs(0),
        },
        600,
    )
    .unwrap();
    assert_eq!(refunded.remaining_room_ms, DurationMs(700));
    assert_eq!(checkpoint_business_projections(&refunded), 0);
}

#[test]
fn scoped_delivery_identifies_same_role_seat_and_only_its_required_aliases() {
    use roundtable_protocol::{
        MandatoryTargetV1, PublishedHistory, RequiredTarget, ResultScope, SpeakerOrdinal,
        VisibleAliases,
    };
    let mut phase = phase_snapshot();
    phase.kind = PhaseKind::Critique;
    let first = SpeakerOrdinal {
        speaker_id: support::id("first"),
        ordinal: 0,
    };
    let second = SpeakerOrdinal {
        speaker_id: support::id("second"),
        ordinal: 1,
    };
    let claim = support::id("target-claim");
    phase.mandatory_targets = vec![
        MandatoryTargetV1 {
            speaker_id: first.speaker_id,
            claim_id: claim,
            response_id: None,
        },
        MandatoryTargetV1 {
            speaker_id: second.speaker_id,
            claim_id: support::id("other-claim"),
            response_id: None,
        },
    ];
    let scope = ResultScope {
        phase_kind: PhaseKind::Critique,
        speaker_id: first.speaker_id,
        aliases: VisibleAliases::default(),
        mandatory_targets: vec![RequiredTarget {
            claim_alias: "c0".into(),
            claim_id: claim,
            response_alias: None,
            response_id: None,
        }],
        published: PublishedHistory::default(),
        quota_bytes: 8192,
    };
    let mut role = role_snapshot();
    role.schema_text = roundtable_protocol::result_schema(Some(PhaseKind::Critique)).to_string();
    let bytes = DeliveryEncoder::prompt_for_speaker(
        &phase,
        &role,
        &support::id("binding"),
        &first,
        &scope,
        &serde_json::json!({"speaker_id":second.speaker_id}),
    )
    .unwrap();
    let prompt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        prompt["metadata"]["speaker_id"],
        first.speaker_id.to_string()
    );
    assert_eq!(prompt["metadata"]["speaker_ordinal"], 0);
    assert_eq!(
        prompt["metadata"]["mandatory_targets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        prompt["metadata"]["mandatory_targets"][0]["claim_alias"],
        "c0"
    );
    let manifest = DeliveryEncoder::encode_prompt(
        &phase,
        &role,
        &support::id("binding"),
        &FakeTokens {
            capacity: Some(10_000_000),
        },
        &profile(10_000_000),
        &bytes,
    )
    .unwrap();
    assert_eq!(manifest.prompt_hash, Hash256::sha256(&bytes));
    assert!(DeliveryEncoder::prompt_for_speaker(
        &phase,
        &role,
        &support::id("binding"),
        &second,
        &scope,
        &serde_json::json!({})
    )
    .is_err());
}

#[test]
fn untrusted_plan_quotas_cannot_request_unbounded_token_witness_allocation() {
    let tokens = FakeTokens {
        capacity: Some(u64::MAX),
    };
    assert_eq!(
        tokens.upper_bound_for_length(1 << 40).unwrap_err().code,
        ErrorCode::CapacityUnknown
    );
}

#[test]
fn interjection_encoding_has_a_separate_shared_ceiling_without_truncation() {
    use roundtable_protocol::{interjection_context_limit, validate_interjection_context};
    let id = "00000000-0000-4000-8000-000000000001";
    let quota = 16_384;
    // One maximum raw-byte input still fits, including worst JSON escaping.
    for text in ["x".repeat(quota as usize), "\0".repeat(quota as usize)] {
        let input = serde_json::json!([{"input_id":id,"text":text}]);
        let before = input.clone();
        assert!(validate_interjection_context(&input, quota).is_ok());
        assert_eq!(input, before);
    }
    let tiny = serde_json::json!({"input_id":id,"text":"x"});
    let entry_bytes = canonical_bytes(&tiny).unwrap().len() as u64 + 1;
    let count = (interjection_context_limit(quota).unwrap() - 1) / entry_bytes;
    let boundary = serde_json::Value::Array(vec![tiny.clone(); count as usize]);
    assert!(validate_interjection_context(&boundary, quota).is_ok());
    let too_many = serde_json::Value::Array(vec![tiny; count as usize + 1]);
    assert!(
        (count + 1) < quota,
        "raw text quota alone still permits this input"
    );
    assert_eq!(
        validate_interjection_context(&too_many, quota)
            .unwrap_err()
            .code,
        ErrorCode::ContextTooLarge
    );
    assert_eq!(too_many.as_array().unwrap().len(), count as usize + 1);
}

#[test]
fn shared_delivery_admission_rejects_small_profile_before_encoding_and_keeps_defaults() {
    let mut phase = phase_snapshot();
    phase.output_byte_limit = SafeInt(8_192);
    phase.tool_quota.per_attempt_bytes = SafeInt(32_768);
    let role = role_snapshot();
    let binding = support::id("reserve-binding");
    let prompt = DeliveryEncoder::prompt_utf8(&phase, &role, &binding).unwrap();
    let mut small = profile(204_800);
    small.max_request_body_bytes = 196_608;
    let tokens = FakeTokens {
        capacity: Some(204_800),
    };
    assert_eq!(
        6 * (8_192 + 32_768) + small.generation_reserve_tokens,
        253_952
    );
    assert_eq!(
        roundtable_protocol::admit_qualified_delivery(
            tokens.upper_bound(&prompt).unwrap(),
            8_192,
            32_768,
            &tokens,
            &small
        )
        .unwrap_err()
        .code,
        ErrorCode::ContextTooLarge
    );
    assert_eq!(
        DeliveryEncoder::encode_prompt(&phase, &role, &binding, &tokens, &small, &prompt)
            .unwrap_err()
            .code,
        ErrorCode::ContextTooLarge
    );
    let normal = profile(2_000_000);
    let normal_tokens = FakeTokens {
        capacity: Some(2_000_000),
    };
    assert_eq!(
        roundtable_protocol::admit_qualified_delivery(
            normal_tokens.upper_bound(&prompt).unwrap(),
            8_192,
            32_768,
            &normal_tokens,
            &normal
        )
        .unwrap(),
        1_056_768
    );
    DeliveryEncoder::encode_prompt(&phase, &role, &binding, &normal_tokens, &normal, &prompt)
        .unwrap();
}
