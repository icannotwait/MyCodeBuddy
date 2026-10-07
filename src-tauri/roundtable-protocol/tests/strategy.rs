//! Bounded phase strategy, frozen response targets, and host coverage.
//!
//! Expected schedules are fixed fixtures. Nothing here reads a preview,
//! a staged candidate, or the wall clock.

#[path = "support/mod.rs"]
#[allow(dead_code)]
mod support;

use roundtable_protocol::{
    assigned_targets, budget_plan, classify_member, coverage, next_intents, phase_quorum,
    AssignedTarget, BudgetState, ClaimId, CoverageV1, ErrorCode, MemberKind, MonoMs, PhaseKind,
    Priority, PublicationStatus, PublishedClaim, PublishedHistory, PublishedMember, PublishedPhase,
    PublishedResponse, ResponseId, ScheduleMark, Seq, SlotOutcome, SlotSnapshot, SpeakerOrdinal,
    SpeakerTargets, Stance, StrategyBudget, StrategyView, Timeouts,
};

fn speaker(ordinal: u32) -> SpeakerOrdinal {
    SpeakerOrdinal {
        ordinal,
        speaker_id: support::id(&format!("speaker-{ordinal}")),
    }
}

fn claim(ordinal: u32, index: usize) -> PublishedClaim {
    PublishedClaim {
        claim_id: support::id(&format!("claim-{ordinal}-{index}")),
        text: format!("claim-{ordinal}-{index}"),
    }
}

fn claims(ordinal: u32, count: usize) -> Vec<PublishedClaim> {
    labeled_claims("claim", ordinal, count)
}

fn labeled_claims(prefix: &str, ordinal: u32, count: usize) -> Vec<PublishedClaim> {
    (0..count)
        .map(|index| PublishedClaim {
            claim_id: support::id(&format!("{prefix}-{ordinal}-{index}")),
            text: format!("{prefix}-{ordinal}-{index}"),
        })
        .collect()
}

fn response(
    label: &str,
    seq: u64,
    stance: Stance,
    priority: Priority,
    claim_label: Option<&str>,
    response_label: Option<&str>,
) -> PublishedResponse {
    PublishedResponse {
        response_id: support::id(label),
        publication_seq: Seq(seq),
        stance,
        priority,
        target_claim_id: claim_label.map(support::id),
        target_response_id: response_label.map(support::id),
    }
}

fn filler(index: usize) -> PublishedResponse {
    response(
        &format!("filler-{index}"),
        1,
        Stance::Clarify,
        Priority::Normal,
        None,
        None,
    )
}

fn member(
    ordinal: u32,
    kind: MemberKind,
    status: PublicationStatus,
    summary: &str,
    claims: Vec<PublishedClaim>,
    responses: Vec<PublishedResponse>,
) -> PublishedMember {
    PublishedMember {
        speaker: speaker(ordinal),
        kind,
        summary: summary.to_string(),
        status,
        claims,
        responses,
    }
}

fn accepted(
    ordinal: u32,
    kind: MemberKind,
    claim_count: usize,
    responses: Vec<PublishedResponse>,
) -> PublishedMember {
    member(
        ordinal,
        kind,
        PublicationStatus::Accepted,
        "summary",
        claims(ordinal, claim_count),
        responses,
    )
}

fn accepted_claims(
    ordinal: u32,
    kind: MemberKind,
    claims: Vec<PublishedClaim>,
    responses: Vec<PublishedResponse>,
) -> PublishedMember {
    member(
        ordinal,
        kind,
        PublicationStatus::Accepted,
        "summary",
        claims,
        responses,
    )
}

fn phase(
    label: &str,
    index: u32,
    seq: u64,
    kind: PhaseKind,
    members: Vec<PublishedMember>,
) -> PublishedPhase {
    PublishedPhase {
        phase_id: support::id(label),
        phase_index: index,
        kind,
        publication_seq: Seq(seq),
        members,
    }
}

fn history(phases: Vec<PublishedPhase>) -> PublishedHistory {
    PublishedHistory { phases }
}

fn budget(n: u32, r: u32, phase_index: u32, now: u64, deadline: u64) -> StrategyBudget {
    let plan = budget_plan(n, r, 1, Timeouts::default()).expect("budget");
    let mut state = BudgetState::fresh(n, r, 1, plan);
    state.current_phase_index = phase_index;
    StrategyBudget {
        state,
        now: MonoMs(now),
        phase_deadline: MonoMs(deadline),
    }
}

fn slot(
    ordinal: u32,
    kind: PhaseKind,
    outcome: SlotOutcome,
    first_launch_spent: bool,
    retry_requested: bool,
) -> SlotSnapshot {
    SlotSnapshot {
        phase_id: support::id("phase-current"),
        kind,
        speaker: speaker(ordinal),
        outcome,
        first_launch_spent,
        retry_requested,
    }
}

fn view_with(
    n: u32,
    r: u32,
    phase_index: u32,
    slots: Vec<SlotSnapshot>,
    now: u64,
    deadline: u64,
) -> StrategyView {
    StrategyView {
        published: history(Vec::new()),
        slots,
        budget: budget(n, r, phase_index, now, deadline),
        moderator: SpeakerOrdinal {
            ordinal: 100,
            speaker_id: support::id("moderator"),
        },
        next_phase_id: support::id("phase-next"),
    }
}

fn by_ordinal(targets: &[SpeakerTargets], ordinal: u32) -> &SpeakerTargets {
    targets
        .iter()
        .find(|target| target.speaker.ordinal == ordinal)
        .expect("speaker")
}

fn base_claim(target: &SpeakerTargets) -> &AssignedTarget {
    let base = target
        .required
        .iter()
        .find(|item| item.response_id.is_none())
        .expect("base claim");
    assert_eq!(
        target.required.first().map(|item| item.response_id),
        Some(None)
    );
    base
}

#[test]
fn ring_targets_and_incoming_challenges() {
    let proposal = phase(
        "proposal",
        0,
        1,
        PhaseKind::Proposal,
        vec![
            accepted(2, MemberKind::Proposal, 20, Vec::new()),
            accepted(0, MemberKind::Proposal, 20, Vec::new()),
            accepted(1, MemberKind::Proposal, 20, Vec::new()),
        ],
    );
    assert!(proposal
        .members
        .iter()
        .all(|member| member.claims.len() == 20));
    let expected = [speaker(0), speaker(1), speaker(2)];
    let published = history(vec![proposal.clone()]);
    let targets = assigned_targets(&proposal, &expected, &published).unwrap();
    assert_eq!(targets.len(), 3);
    assert!(targets.iter().all(|target| target.required.len() <= 4));
    assert_eq!(
        base_claim(by_ordinal(&targets, 0)).claim_id,
        claim(2, 0).claim_id
    );
    assert_eq!(
        base_claim(by_ordinal(&targets, 1)).claim_id,
        claim(0, 0).claim_id
    );
    assert_eq!(
        base_claim(by_ordinal(&targets, 2)).claim_id,
        claim(1, 0).claim_id
    );
    for target in &targets {
        assert!(target
            .required
            .iter()
            .all(|item| item.response_id.is_none()));
        assert!(target.required.iter().all(|item| {
            (0..3).any(|ordinal| item.claim_id == claim(ordinal, 0).claim_id)
                && (1..20).all(|index| item.claim_id != claim(item.source_ordinal, index).claim_id)
        }));
    }
    let frozen = assigned_targets(&proposal, &expected, &published).unwrap();
    assert_eq!(frozen, targets);

    let mut absent_middle = proposal.clone();
    for published in &mut absent_middle.members {
        if published.speaker.ordinal == 1 {
            *published = member(
                1,
                MemberKind::Proposal,
                PublicationStatus::Absent,
                "summary",
                Vec::new(),
                Vec::new(),
            );
        }
    }
    let mutual = assigned_targets(
        &absent_middle,
        &expected,
        &history(vec![absent_middle.clone()]),
    )
    .unwrap();
    assert_eq!(mutual.len(), expected.len());
    assert_eq!(
        base_claim(by_ordinal(&mutual, 0)).claim_id,
        claim(2, 0).claim_id
    );
    assert_eq!(
        base_claim(by_ordinal(&mutual, 2)).claim_id,
        claim(0, 0).claim_id
    );
    assert_eq!(
        base_claim(by_ordinal(&mutual, 1)).claim_id,
        claim(0, 0).claim_id
    );
    assert_eq!(
        by_ordinal(&mutual, 0).required,
        by_ordinal(&frozen, 0).required
    );
    assert_ne!(
        by_ordinal(&mutual, 2).required,
        by_ordinal(&frozen, 2).required
    );

    let mut speaker_one: Vec<_> = (0..10).map(filler).collect();
    speaker_one[0] = response(
        "resp-e",
        1,
        Stance::Challenge,
        Priority::Normal,
        Some("claim-0-4"),
        None,
    );
    speaker_one[1] = response(
        "resp-f",
        10,
        Stance::Challenge,
        Priority::Critical,
        Some("claim-0-5"),
        None,
    );
    speaker_one[3] = response(
        "resp-support-ref",
        10,
        Stance::Support,
        Priority::Critical,
        Some("claim-0-6"),
        None,
    );
    speaker_one[9] = response(
        "resp-b",
        10,
        Stance::Challenge,
        Priority::Critical,
        Some("claim-0-2"),
        None,
    );
    let mut speaker_two: Vec<_> = (100..106).map(filler).collect();
    speaker_two[0] = response(
        "resp-d",
        9,
        Stance::Challenge,
        Priority::Critical,
        Some("claim-0-3"),
        None,
    );
    speaker_two[1] = response(
        "resp-c",
        11,
        Stance::Challenge,
        Priority::Critical,
        Some("claim-0-7"),
        None,
    );
    speaker_two[5] = response(
        "resp-a",
        10,
        Stance::Challenge,
        Priority::Critical,
        Some("claim-0-1"),
        None,
    );
    speaker_two.push(response(
        "resp-not-claim",
        1,
        Stance::Challenge,
        Priority::Critical,
        None,
        Some("resp-e"),
    ));
    speaker_two.push(response(
        "resp-other",
        1,
        Stance::Challenge,
        Priority::Critical,
        Some("claim-1-0"),
        None,
    ));
    let previous = phase(
        "critique-prev",
        1,
        20,
        PhaseKind::Critique,
        vec![
            accepted_claims(
                2,
                MemberKind::Critique,
                labeled_claims("prev", 2, 20),
                speaker_two,
            ),
            accepted_claims(
                0,
                MemberKind::Critique,
                labeled_claims("prev", 0, 20),
                Vec::new(),
            ),
            accepted_claims(
                1,
                MemberKind::Critique,
                labeled_claims("prev", 1, 20),
                speaker_one,
            ),
        ],
    );
    assert!(previous
        .members
        .iter()
        .all(|member| member.claims.len() == 20));
    let challenged = history(vec![proposal, previous.clone()]);
    let targets = assigned_targets(&previous, &expected, &challenged).unwrap();
    assert!(targets.iter().all(|target| target.required.len() <= 4));
    let incoming = &by_ordinal(&targets, 0).required;
    assert_eq!(incoming.len(), 4);
    assert_eq!(incoming[0].claim_id, support::id::<ClaimId>("prev-2-0"));
    assert_eq!(incoming[0].response_id, None);
    assert_eq!(
        incoming[1].response_id,
        Some(support::id::<ResponseId>("resp-d"))
    );
    assert_eq!(
        incoming[2].response_id,
        Some(support::id::<ResponseId>("resp-f"))
    );
    assert_eq!(
        incoming[3].response_id,
        Some(support::id::<ResponseId>("resp-b"))
    );
    let selected: Vec<_> = incoming
        .iter()
        .filter_map(|item| item.response_id)
        .collect();
    for label in [
        "resp-a",
        "resp-c",
        "resp-e",
        "resp-support-ref",
        "resp-other",
        "resp-not-claim",
    ] {
        assert!(
            !selected.contains(&support::id::<ResponseId>(label)),
            "{label}"
        );
    }
    assert_eq!(by_ordinal(&targets, 1).required.len(), 2);
    assert_eq!(
        by_ordinal(&targets, 1).required[1].response_id,
        Some(support::id::<ResponseId>("resp-other"))
    );
    assert_eq!(by_ordinal(&targets, 2).required.len(), 1);
    assert_eq!(
        assigned_targets(&previous, &expected, &challenged).unwrap(),
        targets
    );

    let early = round_five();
    let latest = early.phases.last().unwrap().clone();
    let scheduled = assigned_targets(&latest, &expected, &early).unwrap();
    assert!(scheduled.iter().all(|target| target.required.len() <= 4));
    let required_ids: Vec<_> = scheduled
        .iter()
        .flat_map(|target| target.required.iter().filter_map(|item| item.response_id))
        .collect();
    assert!(required_ids.contains(&support::id::<ResponseId>("r5-new")));
    assert!(!required_ids.contains(&support::id::<ResponseId>("r5-old-unanswered")));
    assert!(!required_ids.contains(&support::id::<ResponseId>("r5-old-answered")));
    let report = coverage(&early, &scheduled);
    assert!(report
        .unanswered_history
        .iter()
        .any(|gap| { gap.response_id == support::id::<ResponseId>("r5-old-unanswered") }));
    assert!(report
        .unanswered_history
        .iter()
        .all(|gap| { gap.response_id != support::id::<ResponseId>("r5-old-answered") }));
    assert!(report
        .support_edges
        .iter()
        .all(|edge| !edge.semantic_support()));
    assert!(report
        .support_edges
        .iter()
        .all(|edge| { edge.response_id != support::id::<ResponseId>("r5-claim-ref") }));

    let (partial, targets) = partial_coverage();
    assert_eq!(coverage(&partial, &targets).unanswered_required, 1);
    let report = coverage(&partial, &targets);
    assert_eq!(
        (report.assigned, report.answered, report.unanswered_required),
        (2, 1, 1)
    );
    assert_eq!((report.valid, report.absent, report.abstained), (1, 1, 1));
    assert_eq!(report.support_edges.len(), 1);
    assert!(!report.support_edges[0].semantic_support());
    assert_eq!(
        report.support_edges[0].response_id,
        support::id::<ResponseId>("partial-support")
    );
    assert_ne!(
        report.support_edges[0].source_speaker_id,
        speaker(1).speaker_id
    );
    let _typed: CoverageV1 = report;
}

fn round_five() -> PublishedHistory {
    let proposal = phase(
        "r5-proposal",
        0,
        1,
        PhaseKind::Proposal,
        vec![
            accepted(0, MemberKind::Proposal, 1, Vec::new()),
            accepted(1, MemberKind::Proposal, 1, Vec::new()),
            accepted(2, MemberKind::Proposal, 1, Vec::new()),
        ],
    );
    let old_open = phase(
        "r5-c1",
        1,
        2,
        PhaseKind::Critique,
        vec![
            accepted(0, MemberKind::Critique, 1, Vec::new()),
            accepted(
                1,
                MemberKind::Critique,
                1,
                vec![response(
                    "r5-old-unanswered",
                    50,
                    Stance::Challenge,
                    Priority::Critical,
                    Some("claim-0-0"),
                    None,
                )],
            ),
            accepted(2, MemberKind::Critique, 1, Vec::new()),
        ],
    );
    let old_closed = phase(
        "r5-c2",
        2,
        3,
        PhaseKind::Critique,
        vec![
            accepted(0, MemberKind::Critique, 1, Vec::new()),
            accepted(1, MemberKind::Critique, 1, Vec::new()),
            accepted(
                2,
                MemberKind::Critique,
                1,
                vec![response(
                    "r5-old-answered",
                    80,
                    Stance::Challenge,
                    Priority::Critical,
                    Some("claim-0-0"),
                    None,
                )],
            ),
        ],
    );
    let answered = phase(
        "r5-c3",
        3,
        4,
        PhaseKind::Critique,
        vec![
            accepted(
                0,
                MemberKind::Critique,
                1,
                vec![response(
                    "r5-answer",
                    1,
                    Stance::Clarify,
                    Priority::Normal,
                    Some("claim-0-0"),
                    Some("r5-old-answered"),
                )],
            ),
            accepted(1, MemberKind::Critique, 1, Vec::new()),
            accepted(2, MemberKind::Critique, 1, Vec::new()),
        ],
    );
    let claim_ref = phase(
        "r5-c4",
        4,
        5,
        PhaseKind::Critique,
        vec![
            accepted(
                0,
                MemberKind::Critique,
                1,
                vec![response(
                    "r5-claim-ref",
                    90,
                    Stance::Clarify,
                    Priority::Normal,
                    Some("claim-0-0"),
                    None,
                )],
            ),
            accepted(1, MemberKind::Critique, 1, Vec::new()),
            accepted(2, MemberKind::Critique, 1, Vec::new()),
        ],
    );
    let previous = phase(
        "r5-c5",
        5,
        6,
        PhaseKind::Critique,
        vec![
            accepted(0, MemberKind::Critique, 1, Vec::new()),
            accepted(
                1,
                MemberKind::Critique,
                1,
                vec![response(
                    "r5-new",
                    7,
                    Stance::Challenge,
                    Priority::Critical,
                    Some("claim-0-0"),
                    None,
                )],
            ),
            accepted(2, MemberKind::Critique, 1, Vec::new()),
        ],
    );
    history(vec![
        proposal, old_open, old_closed, answered, claim_ref, previous,
    ])
}

fn partial_coverage() -> (PublishedHistory, Vec<SpeakerTargets>) {
    let answered = response(
        "partial-answer",
        3,
        Stance::Clarify,
        Priority::Normal,
        Some("partial-claim"),
        None,
    );
    let support = response(
        "partial-support",
        4,
        Stance::Support,
        Priority::Normal,
        Some("partial-other"),
        None,
    );
    let referenced = response(
        "partial-challenge-ref",
        5,
        Stance::Challenge,
        Priority::Critical,
        Some("partial-other"),
        None,
    );
    let absent_support = response(
        "partial-absent-support",
        6,
        Stance::Support,
        Priority::Critical,
        Some("partial-claim"),
        None,
    );
    let phase = phase(
        "partial-phase",
        1,
        4,
        PhaseKind::Critique,
        vec![
            member(
                0,
                MemberKind::Critique,
                PublicationStatus::Accepted,
                "summary",
                vec![
                    PublishedClaim {
                        claim_id: support::id("partial-claim"),
                        text: "claim".to_string(),
                    },
                    PublishedClaim {
                        claim_id: support::id("partial-other"),
                        text: "other".to_string(),
                    },
                ],
                vec![answered, support, referenced],
            ),
            member(
                1,
                MemberKind::Critique,
                PublicationStatus::Absent,
                "summary",
                Vec::new(),
                vec![absent_support],
            ),
            member(
                2,
                MemberKind::Abstain,
                PublicationStatus::Abstained,
                "summary",
                Vec::new(),
                vec![response(
                    "partial-abstain-support",
                    8,
                    Stance::Support,
                    Priority::Normal,
                    Some("partial-claim"),
                    None,
                )],
            ),
        ],
    );
    let targets = vec![SpeakerTargets {
        speaker: speaker(0),
        required: vec![
            AssignedTarget {
                claim_id: support::id("partial-claim"),
                response_id: None,
                source_ordinal: 2,
                publication_seq: Seq(4),
                priority: Priority::Normal,
            },
            AssignedTarget {
                claim_id: support::id("partial-other"),
                response_id: Some(support::id("partial-missing")),
                source_ordinal: 1,
                publication_seq: Seq(4),
                priority: Priority::Critical,
            },
        ],
    }];
    (history(vec![phase]), targets)
}

#[test]
fn empty_claims_cannot_seed_next_phase() {
    let proposal = member(
        0,
        MemberKind::Proposal,
        PublicationStatus::Accepted,
        "non-empty summary",
        Vec::new(),
        vec![response(
            "empty-support",
            1,
            Stance::Support,
            Priority::Critical,
            Some("missing-claim"),
            None,
        )],
    );
    let critique = member(
        1,
        MemberKind::Critique,
        PublicationStatus::Accepted,
        "non-empty summary",
        Vec::new(),
        Vec::new(),
    );
    assert!(!proposal.summary.is_empty());
    assert!(!critique.summary.is_empty());
    assert!(proposal.claims.is_empty() && critique.claims.is_empty());
    assert!(!proposal.counts_as_valid());
    assert!(!critique.counts_as_valid());
    assert_eq!(classify_member(&proposal), SlotOutcome::Invalid);
    assert_eq!(classify_member(&critique), SlotOutcome::Invalid);
    let abstain = member(
        2,
        MemberKind::Abstain,
        PublicationStatus::Abstained,
        "non-empty summary",
        Vec::new(),
        Vec::new(),
    );
    assert!(!abstain.counts_as_valid());
    assert_eq!(classify_member(&abstain), SlotOutcome::Abstained);

    let previous = phase(
        "empty-phase",
        0,
        1,
        PhaseKind::Proposal,
        vec![proposal.clone(), critique.clone()],
    );
    let err = assigned_targets(
        &previous,
        &[speaker(0), speaker(1)],
        &history(vec![previous.clone()]),
    )
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::NoNextPhase);
    let report = coverage(&history(vec![previous]), &[]);
    assert_eq!((report.valid, report.abstained, report.invalid), (0, 0, 2));
    assert!(report.support_edges.is_empty());
    assert!(report
        .support_edges
        .iter()
        .all(|edge| !edge.semantic_support()));

    let waiting = view_with(
        3,
        1,
        0,
        vec![
            slot(0, PhaseKind::Proposal, SlotOutcome::Valid, true, false),
            slot(1, PhaseKind::Proposal, SlotOutcome::Abstained, true, false),
            slot(2, PhaseKind::Proposal, SlotOutcome::Abstained, true, false),
        ],
        0,
        10,
    );
    assert_eq!(phase_quorum(3, PhaseKind::Proposal), 2);
    assert_eq!(
        next_intents(&waiting).unwrap_err().code,
        ErrorCode::CannotReachQuorum
    );
}

#[test]
fn quorum_waits_for_terminal_slots() {
    assert_eq!(phase_quorum(2, PhaseKind::Proposal), 2);
    assert_eq!(phase_quorum(7, PhaseKind::Critique), 4);
    assert_eq!(phase_quorum(7, PhaseKind::Synthesis), 1);

    let running = view_with(
        3,
        1,
        0,
        vec![
            slot(2, PhaseKind::Proposal, SlotOutcome::Valid, true, false),
            slot(0, PhaseKind::Proposal, SlotOutcome::Valid, true, false),
            slot(1, PhaseKind::Proposal, SlotOutcome::Open, true, false),
        ],
        0,
        10,
    );
    let intents = next_intents(&running).unwrap();
    assert!(intents.is_empty());
    assert!(intents
        .iter()
        .all(|intent| intent.phase_id == support::id("phase-current")));

    let mut impossible = view_with(
        2,
        0,
        0,
        vec![
            slot(0, PhaseKind::Proposal, SlotOutcome::Failed, true, false),
            slot(1, PhaseKind::Proposal, SlotOutcome::Open, true, false),
        ],
        0,
        10,
    );
    assert!(impossible.parallel_consultation());
    assert_eq!(
        impossible.schedule_mark(),
        ScheduleMark::ParallelConsultation
    );
    let waiting = next_intents(&impossible).unwrap();
    assert!(waiting
        .iter()
        .all(|intent| intent.phase_id == support::id("phase-current")));
    assert!(waiting
        .iter()
        .all(|intent| intent.speaker_id != support::id("moderator")));
    impossible.slots[1].outcome = SlotOutcome::Valid;
    assert_eq!(
        next_intents(&impossible).unwrap_err().code,
        ErrorCode::CannotReachQuorum
    );

    let mut timed_out = running.clone();
    timed_out.budget.now = MonoMs(10);
    let advanced = next_intents(&timed_out).unwrap();
    assert_eq!(
        advanced
            .iter()
            .map(|intent| intent.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(advanced
        .iter()
        .all(|intent| { !intent.is_retry && intent.phase_id == support::id("phase-next") }));

    let mut closed = running.clone();
    closed.slots[2].outcome = SlotOutcome::Failed;
    let next = next_intents(&closed).unwrap();
    assert_eq!(
        next.iter().map(|intent| intent.ordinal).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(next[1].speaker_id, speaker(1).speaker_id);

    let mut consultation = running.clone();
    consultation.budget.state.r = 0;
    consultation.budget.state.plan = budget_plan(3, 0, 1, Timeouts::default()).unwrap();
    for slot in &mut consultation.slots {
        slot.outcome = SlotOutcome::Valid;
    }
    assert!(consultation.parallel_consultation());
    assert_eq!(
        consultation.schedule_mark(),
        ScheduleMark::ParallelConsultation
    );
    let moderator = next_intents(&consultation).unwrap();
    assert_eq!(moderator.len(), 1);
    assert_eq!(moderator[0].speaker_id, support::id("moderator"));
    assert_eq!(moderator[0].ordinal, 100);
    assert!(!moderator[0].is_retry);
    assert_eq!(moderator[0].phase_id, support::id("phase-next"));
    assert!(moderator
        .iter()
        .all(|intent| intent.speaker_id != speaker(0).speaker_id));

    let mut phased = consultation.clone();
    phased.budget.state.r = 5;
    phased.budget.state.plan = budget_plan(3, 5, 1, Timeouts::default()).unwrap();
    assert!(!phased.parallel_consultation());
    assert_eq!(phased.schedule_mark(), ScheduleMark::PhasedRounds);
    let critiques = next_intents(&phased).unwrap();
    assert_eq!(critiques.len(), 3);
    assert!(critiques
        .iter()
        .all(|intent| intent.speaker_id != support::id("moderator")));

    let mut queue = view_with(
        3,
        1,
        0,
        vec![
            slot(2, PhaseKind::Proposal, SlotOutcome::Open, false, false),
            slot(0, PhaseKind::Proposal, SlotOutcome::Open, false, false),
            slot(1, PhaseKind::Proposal, SlotOutcome::Open, false, false),
        ],
        0,
        10,
    );
    let first = next_intents(&queue).unwrap();
    assert_eq!(
        first
            .iter()
            .map(|intent| intent.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(first.iter().all(|intent| !intent.is_retry));
    queue.slots[0].first_launch_spent = true;
    queue.slots[0].retry_requested = true;
    queue.slots[1].first_launch_spent = true;
    let only_first = next_intents(&queue).unwrap();
    assert_eq!(only_first.len(), 1);
    assert_eq!(only_first[0].ordinal, 1);
    assert!(!only_first[0].is_retry);
    queue.slots[2].first_launch_spent = true;
    queue.slots[1].retry_requested = true;
    let retries = next_intents(&queue).unwrap();
    assert_eq!(
        retries
            .iter()
            .map(|intent| intent.ordinal)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert!(retries.iter().all(|intent| intent.is_retry));
}

#[test]
fn final_coverage_preserves_latest_discussion_and_failed_seat_status() {
    let failed = member(
        1,
        MemberKind::Abstain,
        PublicationStatus::Failed,
        "",
        vec![],
        vec![],
    );
    let absent = member(
        2,
        MemberKind::Abstain,
        PublicationStatus::Absent,
        "",
        vec![],
        vec![],
    );
    assert_eq!(classify_member(&failed), SlotOutcome::Failed);
    assert_eq!(classify_member(&absent), SlotOutcome::Absent);
    let mut published = history(vec![phase(
        "discussion",
        1,
        5,
        PhaseKind::Critique,
        vec![
            accepted(0, MemberKind::Critique, 1, vec![]),
            failed,
            absent,
            member(
                3,
                MemberKind::Abstain,
                PublicationStatus::Abstained,
                "No position",
                vec![],
                vec![],
            ),
        ],
    )]);
    let before = coverage(&published, &[]);
    assert_eq!(
        (
            before.valid,
            before.absent,
            before.abstained,
            before.invalid
        ),
        (1, 2, 1, 0)
    );
    published
        .phases
        .push(phase("synthesis", 2, 9, PhaseKind::Synthesis, vec![]));
    let after = coverage(&published, &[]);
    assert_eq!(
        (after.valid, after.absent, after.abstained, after.invalid),
        (1, 2, 1, 0)
    );
}
