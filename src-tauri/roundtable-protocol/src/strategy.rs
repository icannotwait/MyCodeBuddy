//! Bounded phase schedule, frozen response targets, and host coverage.
//!
//! The view is an immutable published prefix, current slot states, and the
//! P02 budget. It does not read the current preview or a staged candidate,
//! and it does not perform I/O. Reaching quorum does not close a phase.
//! A failure is not an input to target assignment, so it cannot reassign one.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    deadline_still_open, ClaimId, ErrorCode, ErrorDetails, MemberKind, PhaseId, PhaseKind,
    Priority, ResponseId, RtError, RtResult, SchedulingIntent, Seq, SpeakerId, Stance,
};

/// `Q = max(2, floor(N/2)+1)` for a member phase. Synthesis stays 1.
pub fn phase_quorum(n: u32, kind: PhaseKind) -> u32 {
    if kind == PhaseKind::Synthesis {
        return 1;
    }
    (n / 2 + 1).max(2)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleMark {
    ParallelConsultation,
    PhasedRounds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeakerOrdinal {
    pub ordinal: u32,
    pub speaker_id: SpeakerId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublicationStatus {
    Accepted,
    Abstained,
    Absent,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedClaim {
    pub claim_id: ClaimId,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedResponse {
    pub response_id: ResponseId,
    pub publication_seq: Seq,
    pub stance: Stance,
    pub priority: Priority,
    pub target_claim_id: Option<ClaimId>,
    pub target_response_id: Option<ResponseId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedMember {
    pub speaker: SpeakerOrdinal,
    pub kind: MemberKind,
    pub summary: String,
    pub status: PublicationStatus,
    pub claims: Vec<PublishedClaim>,
    pub responses: Vec<PublishedResponse>,
}

impl PublishedMember {
    /// A non-empty summary is not a proposal. The first claim must be respondable.
    pub fn counts_as_valid(&self) -> bool {
        self.status == PublicationStatus::Accepted
            && self.kind != MemberKind::Abstain
            && self
                .claims
                .first()
                .is_some_and(|claim| !claim.text.is_empty())
    }
}

/// Slot classification for a published member. Empty claims stay invalid.
pub fn classify_member(member: &PublishedMember) -> SlotOutcome {
    match member.status {
        PublicationStatus::Absent => SlotOutcome::Absent,
        PublicationStatus::Failed => SlotOutcome::Failed,
        PublicationStatus::Abstained => SlotOutcome::Abstained,
        PublicationStatus::Accepted if member.kind == MemberKind::Abstain => SlotOutcome::Abstained,
        PublicationStatus::Accepted if member.counts_as_valid() => SlotOutcome::Valid,
        PublicationStatus::Accepted => SlotOutcome::Invalid,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPhase {
    pub phase_id: PhaseId,
    pub phase_index: u32,
    pub kind: PhaseKind,
    pub publication_seq: Seq,
    pub members: Vec<PublishedMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PublishedHistory {
    pub phases: Vec<PublishedPhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssignedTarget {
    pub claim_id: ClaimId,
    /// Set for an incoming challenge. A base claim target leaves this empty.
    pub response_id: Option<ResponseId>,
    pub source_ordinal: u32,
    pub publication_seq: Seq,
    pub priority: Priority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerTargets {
    pub speaker: SpeakerOrdinal,
    pub required: Vec<AssignedTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotOutcome {
    Open,
    Valid,
    Abstained,
    Failed,
    Absent,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSnapshot {
    pub phase_id: PhaseId,
    pub kind: PhaseKind,
    pub speaker: SpeakerOrdinal,
    pub outcome: SlotOutcome,
    pub first_launch_spent: bool,
    pub retry_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyBudget {
    pub state: crate::BudgetState,
    pub now: crate::MonoMs,
    pub phase_deadline: crate::MonoMs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyView {
    pub published: PublishedHistory,
    pub slots: Vec<SlotSnapshot>,
    pub budget: StrategyBudget,
    pub moderator: SpeakerOrdinal,
    pub next_phase_id: PhaseId,
}

impl StrategyView {
    pub fn schedule_mark(&self) -> ScheduleMark {
        if self.budget.state.r == 0 {
            ScheduleMark::ParallelConsultation
        } else {
            ScheduleMark::PhasedRounds
        }
    }

    /// R=0 is a parallel consultation and still has an independent moderator.
    pub fn parallel_consultation(&self) -> bool {
        self.schedule_mark() == ScheduleMark::ParallelConsultation
    }
}

pub fn next_intents(view: &StrategyView) -> RtResult<Vec<SchedulingIntent>> {
    if view.budget.state.plan.slot_ms == 0 || view.budget.state.plan.room_ms == 0 {
        return Err(invalid("invalid_budget"));
    }
    let kind = phase_kind(view)?;
    if !phase_closed(view) {
        return Ok(open_queue(view));
    }
    let valid = view
        .slots
        .iter()
        .filter(|slot| slot.outcome == SlotOutcome::Valid)
        .count();
    let valid = u32::try_from(valid).map_err(|_| invalid("overflow"))?;
    if valid < phase_quorum(view.budget.state.n, kind) {
        return Err(no_quorum("quorum"));
    }
    if kind == PhaseKind::Synthesis || view.budget.state.current_phase_index >= view.budget.state.r
    {
        if kind == PhaseKind::Synthesis {
            return Ok(Vec::new());
        }
        return Ok(vec![SchedulingIntent {
            phase_id: view.next_phase_id,
            speaker_id: view.moderator.speaker_id,
            ordinal: view.moderator.ordinal,
            is_retry: false,
        }]);
    }
    let mut slots = view.slots.clone();
    slots.sort_by_key(|slot| slot.speaker.ordinal);
    Ok(slots
        .into_iter()
        .map(|slot| SchedulingIntent {
            phase_id: view.next_phase_id,
            speaker_id: slot.speaker.speaker_id,
            ordinal: slot.speaker.ordinal,
            is_retry: false,
        })
        .collect())
}

fn phase_closed(view: &StrategyView) -> bool {
    let timed_out = !deadline_still_open(view.budget.now, view.budget.phase_deadline);
    let all_terminal = view
        .slots
        .iter()
        .all(|slot| slot.outcome != SlotOutcome::Open);
    all_terminal || timed_out
}

fn open_queue(view: &StrategyView) -> Vec<SchedulingIntent> {
    let mut slots = view.slots.clone();
    slots.sort_by_key(|slot| slot.speaker.ordinal);
    let mut first = Vec::new();
    let mut retries = Vec::new();
    for slot in slots {
        if slot.outcome != SlotOutcome::Open {
            continue;
        }
        let intent = SchedulingIntent {
            phase_id: slot.phase_id,
            speaker_id: slot.speaker.speaker_id,
            ordinal: slot.speaker.ordinal,
            is_retry: slot.first_launch_spent,
        };
        if !slot.first_launch_spent {
            first.push(intent);
        } else if slot.retry_requested {
            retries.push(intent);
        }
    }
    if !first.is_empty() {
        first
    } else {
        retries
    }
}

fn phase_kind(view: &StrategyView) -> RtResult<PhaseKind> {
    let Some(first) = view.slots.first() else {
        return Err(invalid("slot"));
    };
    if view
        .slots
        .iter()
        .any(|slot| slot.kind != first.kind || slot.phase_id != first.phase_id)
    {
        return Err(invalid("slot"));
    }
    if first.kind == PhaseKind::Synthesis {
        if view.slots.len() != 1 {
            return Err(invalid("slot"));
        }
        return Ok(first.kind);
    }
    let expected = usize::try_from(view.budget.state.n).map_err(|_| invalid("overflow"))?;
    if view.budget.state.n < 2 || view.slots.len() != expected {
        return Err(invalid("slot"));
    }
    let mut ordinals = BTreeSet::new();
    for slot in &view.slots {
        if slot.speaker.ordinal >= view.budget.state.n || !ordinals.insert(slot.speaker.ordinal) {
            return Err(invalid("slot"));
        }
    }
    Ok(first.kind)
}

pub fn assigned_targets(
    previous: &PublishedPhase,
    expected: &[SpeakerOrdinal],
    history: &PublishedHistory,
) -> RtResult<Vec<SpeakerTargets>> {
    if expected.is_empty() {
        return Err(invalid("expected"));
    }
    let mut expected_ordinals = BTreeSet::new();
    for speaker in expected {
        if !expected_ordinals.insert(speaker.ordinal) {
            return Err(invalid("expected"));
        }
    }
    let mut seen = BTreeSet::new();
    for member in &previous.members {
        if !seen.insert(member.speaker.ordinal) {
            return Err(invalid("speaker"));
        }
        if let Some(expected_speaker) = expected
            .iter()
            .find(|speaker| speaker.ordinal == member.speaker.ordinal)
        {
            if expected_speaker.speaker_id != member.speaker.speaker_id {
                return Err(invalid("speaker"));
            }
        }
    }
    let mut ring: Vec<&PublishedMember> = previous
        .members
        .iter()
        .filter(|member| member.counts_as_valid())
        .collect();
    ring.sort_by_key(|member| member.speaker.ordinal);
    if ring.len() < 2 {
        return Err(no_next("empty_claims"));
    }
    for member in &ring {
        if !expected_ordinals.contains(&member.speaker.ordinal) {
            return Err(invalid("expected"));
        }
    }
    let owners = claim_owners(history, previous)?;
    let mut base_for = BTreeMap::new();
    for (index, member) in ring.iter().enumerate() {
        let claim = member.claims.first().ok_or_else(|| invalid("claim"))?;
        let next = &ring[(index + 1) % ring.len()];
        base_for.insert(
            next.speaker.ordinal,
            AssignedTarget {
                claim_id: claim.claim_id,
                response_id: None,
                source_ordinal: member.speaker.ordinal,
                publication_seq: previous.publication_seq,
                priority: Priority::Normal,
            },
        );
    }
    let mut targets = Vec::with_capacity(expected.len());
    for speaker in expected {
        let base = if let Some(target) = base_for.get(&speaker.ordinal).copied() {
            target
        } else {
            let source = ring
                .iter()
                .find(|member| member.speaker.ordinal != speaker.ordinal)
                .ok_or_else(|| invalid("claim"))?;
            let claim = source.claims.first().ok_or_else(|| invalid("claim"))?;
            AssignedTarget {
                claim_id: claim.claim_id,
                response_id: None,
                source_ordinal: source.speaker.ordinal,
                publication_seq: previous.publication_seq,
                priority: Priority::Normal,
            }
        };
        let mut required = vec![base];
        required.extend(incoming_challenges(previous, *speaker, &owners)?);
        targets.push(SpeakerTargets {
            speaker: *speaker,
            required,
        });
    }
    Ok(targets)
}

fn incoming_challenges(
    previous: &PublishedPhase,
    assignee: SpeakerOrdinal,
    owners: &BTreeMap<ClaimId, SpeakerOrdinal>,
) -> RtResult<Vec<AssignedTarget>> {
    let mut ranked = Vec::new();
    for member in &previous.members {
        if !member.counts_as_valid() || member.speaker.speaker_id == assignee.speaker_id {
            continue;
        }
        for (response_ordinal, response) in member.responses.iter().enumerate() {
            if response.stance != Stance::Challenge {
                continue;
            }
            let Some(claim_id) = response.target_claim_id else {
                continue;
            };
            let Some(owner) = owners.get(&claim_id) else {
                continue;
            };
            if owner.speaker_id != assignee.speaker_id {
                continue;
            }
            ranked.push((
                priority_rank(response.priority),
                response.publication_seq,
                member.speaker.ordinal,
                response_ordinal,
                AssignedTarget {
                    claim_id,
                    response_id: Some(response.response_id),
                    source_ordinal: member.speaker.ordinal,
                    publication_seq: response.publication_seq,
                    priority: response.priority,
                },
            ));
        }
    }
    ranked.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
            .then(left.3.cmp(&right.3))
    });
    Ok(ranked
        .into_iter()
        .take(3)
        .map(|(_, _, _, _, target)| target)
        .collect())
}

fn priority_rank(priority: Priority) -> u8 {
    match priority {
        Priority::Critical => 0,
        Priority::Normal => 1,
    }
}

fn claim_owners(
    history: &PublishedHistory,
    previous: &PublishedPhase,
) -> RtResult<BTreeMap<ClaimId, SpeakerOrdinal>> {
    let mut phases: Vec<&PublishedPhase> = history.phases.iter().collect();
    if !phases
        .iter()
        .any(|phase| phase.phase_id == previous.phase_id)
    {
        phases.push(previous);
    }
    let mut owners: BTreeMap<ClaimId, SpeakerOrdinal> = BTreeMap::new();
    for phase in phases {
        for member in &phase.members {
            for claim in &member.claims {
                if let Some(owner) = owners.get(&claim.claim_id) {
                    if owner.speaker_id != member.speaker.speaker_id {
                        return Err(invalid("claim"));
                    }
                    continue;
                }
                owners.insert(claim.claim_id, member.speaker);
            }
        }
    }
    Ok(owners)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportEdge {
    pub source_ordinal: u32,
    pub source_speaker_id: SpeakerId,
    pub target_claim_id: ClaimId,
    pub response_id: ResponseId,
}

impl SupportEdge {
    /// A resolvable claim reference is not semantic support.
    pub const fn semantic_support(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnansweredChallenge {
    pub response_id: ResponseId,
    pub claim_id: ClaimId,
    pub source_ordinal: u32,
    pub target_ordinal: u32,
    pub publication_seq: Seq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageV1 {
    pub assigned: u32,
    pub answered: u32,
    pub unanswered_required: u32,
    pub valid: u32,
    pub absent: u32,
    pub abstained: u32,
    pub invalid: u32,
    pub unanswered_history: Vec<UnansweredChallenge>,
    pub support_edges: Vec<SupportEdge>,
}

pub fn coverage(history: &PublishedHistory, targets: &[SpeakerTargets]) -> CoverageV1 {
    let owners = claim_owners_lossy(history);
    let assigned = count_u32(targets.iter().map(|target| target.required.len()));
    let answered = count_u32(targets.iter().map(|target| {
        target
            .required
            .iter()
            .filter(|required| target_answered(history, target.speaker.speaker_id, required))
            .count()
    }));
    let unanswered_required = count_u32(targets.iter().map(|target| {
        target
            .required
            .iter()
            .filter(|required| !target_answered(history, target.speaker.speaker_id, required))
            .count()
    }));
    let (valid, absent, abstained, invalid) = latest_counts(history);
    CoverageV1 {
        assigned,
        answered,
        unanswered_required,
        valid,
        absent,
        abstained,
        invalid,
        unanswered_history: unanswered_history(history, &owners),
        support_edges: support_edges(history, &owners),
    }
}

fn target_answered(
    history: &PublishedHistory,
    assignee: SpeakerId,
    target: &AssignedTarget,
) -> bool {
    history.phases.iter().any(|phase| {
        phase.members.iter().any(|member| {
            member.counts_as_valid()
                && member.speaker.speaker_id == assignee
                && member
                    .responses
                    .iter()
                    .any(|response| match target.response_id {
                        Some(response_id) => response.target_response_id == Some(response_id),
                        None => response.target_claim_id == Some(target.claim_id),
                    })
        })
    })
}

fn latest_counts(history: &PublishedHistory) -> (u32, u32, u32, u32) {
    let Some(latest) = history
        .phases
        .iter()
        .filter(|phase| phase.kind != PhaseKind::Synthesis)
        .max_by_key(|phase| (phase.publication_seq, phase.phase_index))
    else {
        return (0, 0, 0, 0);
    };
    let mut valid = 0u32;
    let mut absent = 0u32;
    let mut abstained = 0u32;
    let mut invalid = 0u32;
    for member in &latest.members {
        match classify_member(member) {
            SlotOutcome::Valid => valid = valid.saturating_add(1),
            SlotOutcome::Absent | SlotOutcome::Failed => absent = absent.saturating_add(1),
            SlotOutcome::Abstained => abstained = abstained.saturating_add(1),
            _ => invalid = invalid.saturating_add(1),
        }
    }
    (valid, absent, abstained, invalid)
}

fn unanswered_history(
    history: &PublishedHistory,
    owners: &BTreeMap<ClaimId, SpeakerOrdinal>,
) -> Vec<UnansweredChallenge> {
    let mut gaps = Vec::new();
    for phase in &history.phases {
        for member in &phase.members {
            if !member.counts_as_valid() {
                continue;
            }
            for response in &member.responses {
                if response.stance != Stance::Challenge {
                    continue;
                }
                let Some(claim_id) = response.target_claim_id else {
                    continue;
                };
                let Some(owner) = owners.get(&claim_id) else {
                    continue;
                };
                if challenge_answered(
                    history,
                    owner.speaker_id,
                    response.response_id,
                    phase.publication_seq,
                ) {
                    continue;
                }
                gaps.push(UnansweredChallenge {
                    response_id: response.response_id,
                    claim_id,
                    source_ordinal: member.speaker.ordinal,
                    target_ordinal: owner.ordinal,
                    publication_seq: phase.publication_seq,
                });
            }
        }
    }
    gaps.sort_by(|left, right| {
        left.publication_seq
            .cmp(&right.publication_seq)
            .then(left.source_ordinal.cmp(&right.source_ordinal))
            .then(left.response_id.cmp(&right.response_id))
    });
    gaps
}

fn challenge_answered(
    history: &PublishedHistory,
    owner: SpeakerId,
    response_id: ResponseId,
    phase_seq: Seq,
) -> bool {
    history.phases.iter().any(|phase| {
        phase.publication_seq > phase_seq
            && phase.members.iter().any(|member| {
                member.counts_as_valid()
                    && member.speaker.speaker_id == owner
                    && member
                        .responses
                        .iter()
                        .any(|response| response.target_response_id == Some(response_id))
            })
    })
}

fn support_edges(
    history: &PublishedHistory,
    owners: &BTreeMap<ClaimId, SpeakerOrdinal>,
) -> Vec<SupportEdge> {
    let mut edges = Vec::new();
    for phase in &history.phases {
        for member in &phase.members {
            // Absence, abstain, and an invalid result are not support.
            if !member.counts_as_valid() {
                continue;
            }
            for response in &member.responses {
                if response.stance != Stance::Support {
                    continue;
                }
                let Some(claim_id) = response.target_claim_id else {
                    continue;
                };
                if !owners.contains_key(&claim_id) {
                    continue;
                }
                edges.push(SupportEdge {
                    source_ordinal: member.speaker.ordinal,
                    source_speaker_id: member.speaker.speaker_id,
                    target_claim_id: claim_id,
                    response_id: response.response_id,
                });
            }
        }
    }
    edges.sort_by(|left, right| {
        left.source_ordinal
            .cmp(&right.source_ordinal)
            .then(left.response_id.cmp(&right.response_id))
    });
    edges
}

fn claim_owners_lossy(history: &PublishedHistory) -> BTreeMap<ClaimId, SpeakerOrdinal> {
    let mut owners = BTreeMap::new();
    for phase in &history.phases {
        for member in &phase.members {
            for claim in &member.claims {
                owners.entry(claim.claim_id).or_insert(member.speaker);
            }
        }
    }
    owners
}

fn count_u32(counts: impl Iterator<Item = usize>) -> u32 {
    counts.fold(0u32, |total, count| {
        total.saturating_add(u32::try_from(count).unwrap_or(u32::MAX))
    })
}

fn invalid(reason: &str) -> RtError {
    error(
        ErrorCode::InvalidArgument,
        "The request is invalid.",
        reason,
    )
}

fn no_quorum(reason: &str) -> RtError {
    error(
        ErrorCode::CannotReachQuorum,
        "The phase cannot reach quorum.",
        reason,
    )
}

fn no_next(reason: &str) -> RtError {
    error(ErrorCode::NoNextPhase, "There is no next phase.", reason)
}

fn error(code: ErrorCode, message: &str, reason: &str) -> RtError {
    RtError {
        code,
        message: message.to_string(),
        retryable: code.retryable(),
        current_revision: None,
        details: ErrorDetails {
            reason: Some(reason.to_string()),
            field_errors: Vec::new(),
        },
    }
}
