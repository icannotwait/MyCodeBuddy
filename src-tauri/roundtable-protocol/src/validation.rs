//! Result schema, visible-alias checks, and the pure candidate seal.
//!
//! The tool payload is the only input. Assistant prose is not scanned for a
//! JSON object. Byte caps are canonical UTF-8 bytes, not character counts.
//! `submit_candidate` stages one candidate; it does not accept the turn or
//! decide a normal finish. A pure transition does not prove a database race.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    canonical_bytes, parse_strict_json, v1_1, AgreementLevel, AliasKind, AliasRefV1,
    CandidateReceipt, CandidateState, ClaimId, ConclusionV1, ConsensusItemV1, Hash256,
    LimitsOrigin, MemberKind, MemberResultV1, MessageId, ModeratorKind, ModeratorResultV1,
    ParseLimits, PhaseKind, PublicationStatus, PublishedHistory, PublishedMember, ResponseId,
    RtError, SpeakerId, Stance, SubmissionId,
};

/// Semantic invalid submissions that remain repairable. The next one closes
/// the attempt. Schema-shape mistakes use [`MAX_REPAIRABLE_SHAPE_SUBMISSIONS`].
pub const MAX_REPAIRABLE_INVALID_SUBMISSIONS: u32 = 3;

/// Schema-shape mistakes (missing or unknown fields, invalid JSON, wrong
/// kind, empty text, count limits) stay repairable up to this bound. The next
/// one closes the attempt. Alias and consensus failures still use
/// [`MAX_REPAIRABLE_INVALID_SUBMISSIONS`].
pub const MAX_REPAIRABLE_SHAPE_SUBMISSIONS: u32 = 8;

const IDENTITY_KEYS: &[&str] = &[
    "speaker_id",
    "speaker",
    "room_id",
    "room",
    "attempt_id",
    "attempt",
    "participant_id",
    "binding_id",
    "principal_id",
    "session_id",
    "token",
    "incarnation",
    "phase_id",
];

const MEMBER_FIELDS: &[&str] = &[
    "kind",
    "summary",
    "claims",
    "responses",
    "open_questions",
    "position_changes",
    "reason",
];

const MODERATOR_FIELDS: &[&str] = &[
    "kind",
    "recommendation",
    "alternatives",
    "consensus_items",
    "disagreements",
    "risks",
    "decision_requests",
];

/// Complete model-facing JSON Schema shared by all delivery transports.
/// Enum serialization and quotas use the validator's types/constants; property
/// and required-field parity is maintained by validator/corpus tests. Dynamic
/// alias/ownership rules are documented and remain enforced by the validator.
/// Identity and coverage stay host-owned and absent from the model input.
pub fn result_schema(phase: Option<PhaseKind>) -> Value {
    use serde_json::json;
    fn object(required: &[&str], properties: Value) -> Value {
        json!({"type":"object","additionalProperties":false,"required":required,"properties":properties})
    }
    fn array(items: Value) -> Value { json!({"type":"array","items":items}) }
    fn enumeration(values: impl Serialize) -> Value { json!({"type":"string","enum":values}) }
    let text = json!({"type":"string","minLength":1,"pattern":"\\S"});
    let aliases = array(json!({"type":"string","minLength":1}));
    let alias = object(&["kind","alias"], json!({
        "kind":enumeration([AliasKind::Message, AliasKind::Claim, AliasKind::Evidence]),
        "alias":{"type":"string","minLength":1}
    }));
    let conclusion = object(&["text","aliases","inference"], json!({"text":text,"aliases":array(alias.clone()),"inference":{"type":"boolean"}}));
    let mut member = object(&["kind","summary","claims"], json!({
        "kind": enumeration(match phase { Some(PhaseKind::Proposal) => vec![MemberKind::Proposal,MemberKind::Abstain], Some(PhaseKind::Critique) => vec![MemberKind::Critique,MemberKind::Abstain], _ => vec![MemberKind::Proposal,MemberKind::Critique,MemberKind::Abstain] }),
        "summary":text,
        "claims":{"type":"array","maxItems":v1_1::MAX_CLAIMS,"items":object(&["local_key","text","evidence_aliases","confidence"],json!({
            "local_key":{"type":"string","minLength":1},"text":text,"evidence_aliases":aliases,
            "confidence":enumeration([crate::Confidence::Low,crate::Confidence::Medium,crate::Confidence::High])
        }))},
        "responses":{"type":"array","maxItems":v1_1::MAX_RESPONSES,"items":{
            "type":"object","additionalProperties":false,"required":["stance","priority","text","evidence_aliases"],
            "oneOf":[{"required":["target_claim_alias"],"not":{"required":["target_response_alias"]}},{"required":["target_response_alias"],"not":{"required":["target_claim_alias"]}}],
            "properties":{"target_claim_alias":{"type":"string","minLength":1},"target_response_alias":{"type":"string","minLength":1},"stance":enumeration([Stance::Support,Stance::Challenge,Stance::Clarify,Stance::Revise]),"priority":enumeration([crate::Priority::Normal,crate::Priority::Critical]),"text":text,"evidence_aliases":aliases}
        }},
        "open_questions":{"type":"array","maxItems":v1_1::MAX_OPEN_QUESTIONS,"items":text},
        "position_changes":{"type":"array","maxItems":v1_1::MAX_POSITION_CHANGES,"items":object(&["own_prior_claim_alias","new_local_claim_key","reason","trigger_response_aliases"],json!({"own_prior_claim_alias":{"type":"string"},"new_local_claim_key":{"type":"string"},"reason":text,"trigger_response_aliases":aliases}))},
        "reason":{"type":"string"}
    }));
    member["allOf"] = json!([{"if":{"properties":{"kind":{"const":"abstain"}}},"then":{"required":["reason"],"properties":{"reason":text,"claims":{"maxItems":0},"responses":{"maxItems":0},"position_changes":{"maxItems":0}}},"else":{"properties":{"claims":{"minItems":1}}}}]);
    member["examples"] = json!([{"kind":if phase==Some(PhaseKind::Critique){"critique"}else{"proposal"},"summary":"A reasoned position","claims":[{"local_key":"a","text":"An explicitly stated claim","evidence_aliases":[],"confidence":"low"}],"responses":[],"open_questions":[],"position_changes":[]}]);
    let mut moderator = object(MODERATOR_FIELDS, json!({
        "kind":enumeration([ModeratorKind::Synthesis]),"recommendation":conclusion,
        "alternatives":array(conclusion.clone()),"disagreements":array(conclusion.clone()),"risks":array(conclusion.clone()),"decision_requests":array(conclusion),
        "consensus_items":array(object(&["text","agreement_level","aliases","supporter_aliases","support_response_aliases","inference"],json!({"text":text,"agreement_level":enumeration([AgreementLevel::ExplicitAgreement,AgreementLevel::CompatiblePositions,AgreementLevel::Unresolved]),"aliases":array(alias),"supporter_aliases":aliases,"support_response_aliases":aliases,"inference":{"type":"boolean"}})))
    }));
    moderator["examples"] = json!([{"kind":"synthesis","recommendation":{"text":"This recommendation is an inference","aliases":[],"inference":true},"alternatives":[],"consensus_items":[],"disagreements":[],"risks":[],"decision_requests":[]}]);
    let mut schema = match phase { Some(PhaseKind::Synthesis) => moderator, Some(_) => member, None => json!({"oneOf":[member,moderator]}) };
    schema["$schema"] = json!("http://json-schema.org/draft-07/schema#");
    schema["$id"] = json!("roundtable_result_v1");
    schema["description"] = json!("Submit only this result object through submit_result with a fresh UUID submission_id. Do not submit identity fields or coverage. Optional fields must be omitted, never null. Use only visible aliases supplied in context.aliases and context.alias_catalog; IDs are not aliases. Claim local_key values must be unique within your result. Cite evidence aliases only after reading the frozen evidence. A non-abstaining member must answer every metadata.mandatory_targets entry with exactly one target_claim_alias or target_response_alias per response. Support cannot target your own claims. Position changes must refer to your published prior claim, a local claim key in this result and published response aliases. Moderator conclusions require visible message/claim/evidence aliases or inference=true. Explicit agreement requires at least two published support responses from distinct other speakers on the same claim, with matching supporter aliases. Stay within metadata.phase.output_byte_limit canonical UTF-8 bytes. An abstention requires a non-empty reason and no claims, responses or position changes. A staged receipt is not turn acceptance; finish the ACP turn normally after receiving the receipt.");
    schema
}

/// Host-owned scope. The payload cannot choose the speaker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultScope {
    pub phase_kind: PhaseKind,
    pub speaker_id: SpeakerId,
    pub aliases: VisibleAliases,
    pub mandatory_targets: Vec<RequiredTarget>,
    pub published: PublishedHistory,
    pub quota_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VisibleAliases {
    pub claims: BTreeMap<String, ClaimRef>,
    pub responses: BTreeMap<String, ResponseRef>,
    pub messages: BTreeMap<String, MessageId>,
    pub evidence: BTreeMap<String, EvidenceRef>,
    pub speakers: BTreeMap<String, SpeakerId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRef {
    pub claim_id: ClaimId,
    pub speaker_id: SpeakerId,
    pub published: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseRef {
    pub response_id: ResponseId,
    pub speaker_id: SpeakerId,
    pub published: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AliasVisibility {
    Published,
    OwnAttempt,
    PeerStaged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub evidence_id: crate::EvidenceId,
    pub visibility: AliasVisibility,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequiredTarget {
    pub claim_alias: String,
    pub claim_id: ClaimId,
    pub response_alias: Option<String>,
    pub response_id: Option<ResponseId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedResult {
    pub byte_len: usize,
    pub canonical_hash: Hash256,
    pub counts_toward_quorum: bool,
    pub abstained: bool,
    pub member: Option<MemberResultV1>,
    pub moderator: Option<ModeratorResultV1>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionState {
    pub invalid_count: u32,
    pub shape_invalid_count: u32,
    pub closed: bool,
    pub sealed: Option<CandidateReceipt>,
    seen: BTreeMap<SubmissionId, SeenSubmission>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SeenSubmission {
    hash: Hash256,
    errors: Vec<FieldError>,
}

impl SubmissionState {
    pub fn open() -> Self {
        Self {
            invalid_count: 0,
            shape_invalid_count: 0,
            closed: false,
            sealed: None,
            seen: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionDecision {
    pub outcome: DecisionKind,
    pub next_state: SubmissionState,
    pub closes_attempt: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionKind {
    Staged(CandidateReceipt),
    FieldErrors(Vec<FieldError>),
    SubmissionConflict,
    ResultAlreadySealed,
    AttemptClosed,
}

/// Closed field diagnostic. `message` is static and never copies a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldCode {
    InvalidJson,
    ByteLimit,
    DuplicateKey,
    UnknownField,
    MissingField,
    MaxDepth,
    TrailingData,
    IdentityNotSelectable,
    WrongPhaseKind,
    EmptySummary,
    ClaimCount,
    ResponseCount,
    OpenQuestionCount,
    PositionChangeCount,
    DuplicateLocalKey,
    EmptyText,
    UnknownAlias,
    PeerStagedEvidence,
    SelfSupport,
    AmbiguousTarget,
    MissingTarget,
    ClaimOwner,
    UnpublishedClaim,
    UnpublishedResponse,
    MissingLocalClaim,
    MandatoryTarget,
    AbstainReason,
    AbstainClaims,
    AbstainShape,
    CoverageRejected,
    InferenceRequired,
    ConsensusSupport,
}

impl FieldCode {
    pub const ALL: &'static [Self] = &[
        Self::InvalidJson,
        Self::ByteLimit,
        Self::DuplicateKey,
        Self::UnknownField,
        Self::MissingField,
        Self::MaxDepth,
        Self::TrailingData,
        Self::IdentityNotSelectable,
        Self::WrongPhaseKind,
        Self::EmptySummary,
        Self::ClaimCount,
        Self::ResponseCount,
        Self::OpenQuestionCount,
        Self::PositionChangeCount,
        Self::DuplicateLocalKey,
        Self::EmptyText,
        Self::UnknownAlias,
        Self::PeerStagedEvidence,
        Self::SelfSupport,
        Self::AmbiguousTarget,
        Self::MissingTarget,
        Self::ClaimOwner,
        Self::UnpublishedClaim,
        Self::UnpublishedResponse,
        Self::MissingLocalClaim,
        Self::MandatoryTarget,
        Self::AbstainReason,
        Self::AbstainClaims,
        Self::AbstainShape,
        Self::CoverageRejected,
        Self::InferenceRequired,
        Self::ConsensusSupport,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::ByteLimit => "byte_limit",
            Self::DuplicateKey => "duplicate_key",
            Self::UnknownField => "unknown_field",
            Self::MissingField => "missing_field",
            Self::MaxDepth => "max_depth",
            Self::TrailingData => "trailing_data",
            Self::IdentityNotSelectable => "identity_not_selectable",
            Self::WrongPhaseKind => "wrong_phase_kind",
            Self::EmptySummary => "empty_summary",
            Self::ClaimCount => "claim_count",
            Self::ResponseCount => "response_count",
            Self::OpenQuestionCount => "open_question_count",
            Self::PositionChangeCount => "position_change_count",
            Self::DuplicateLocalKey => "duplicate_local_key",
            Self::EmptyText => "empty_text",
            Self::UnknownAlias => "unknown_alias",
            Self::PeerStagedEvidence => "peer_staged_evidence",
            Self::SelfSupport => "self_support",
            Self::AmbiguousTarget => "ambiguous_target",
            Self::MissingTarget => "missing_target",
            Self::ClaimOwner => "claim_owner",
            Self::UnpublishedClaim => "unpublished_claim",
            Self::UnpublishedResponse => "unpublished_response",
            Self::MissingLocalClaim => "missing_local_claim",
            Self::MandatoryTarget => "mandatory_target",
            Self::AbstainReason => "abstain_reason",
            Self::AbstainClaims => "abstain_claims",
            Self::AbstainShape => "abstain_shape",
            Self::CoverageRejected => "coverage_rejected",
            Self::InferenceRequired => "inference_required",
            Self::ConsensusSupport => "consensus_support",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::InvalidJson => "The result is not strict JSON.",
            Self::ByteLimit => "The canonical result exceeds the byte quota.",
            Self::DuplicateKey => "A JSON object repeats a key.",
            Self::UnknownField => "The result contains a field outside the schema.",
            Self::MissingField => "A required field is missing.",
            Self::MaxDepth => "The JSON exceeds the depth limit.",
            Self::TrailingData => "The JSON has trailing data.",
            Self::IdentityNotSelectable => "The tool payload cannot select an identity.",
            Self::WrongPhaseKind => "The result kind does not match this phase.",
            Self::EmptySummary => "The summary must be non-empty text.",
            Self::ClaimCount => "Claims must number from 1 to 20, or 0 when abstaining.",
            Self::ResponseCount => "Responses must number at most 30.",
            Self::OpenQuestionCount => "Open questions must number at most 20.",
            Self::PositionChangeCount => "Position changes must number at most 20.",
            Self::DuplicateLocalKey => "Claim local keys must be unique.",
            Self::EmptyText => "Text must be non-empty.",
            Self::UnknownAlias => "The alias is not in the visible map for this attempt.",
            Self::PeerStagedEvidence => "Peer staged evidence is not visible.",
            Self::SelfSupport => "Self support cannot stand in for cross-review.",
            Self::AmbiguousTarget => "A response must name a claim or a response, not both.",
            Self::MissingTarget => "A response must name one claim or one response.",
            Self::ClaimOwner => "The prior claim must belong to this speaker.",
            Self::UnpublishedClaim => "The claim must already be published.",
            Self::UnpublishedResponse => "The response must already be published.",
            Self::MissingLocalClaim => "The new local claim key is not in this result.",
            Self::MandatoryTarget => "Each mandatory target needs a covering response.",
            Self::AbstainReason => "Abstain requires a reason.",
            Self::AbstainClaims => "Abstain cannot include claims.",
            Self::AbstainShape => "Abstain cannot include responses or position changes.",
            Self::CoverageRejected => "Coverage is produced by the service and cannot be submitted.",
            Self::InferenceRequired => "An inference without a valid reference must be labeled inference.",
            Self::ConsensusSupport => "Explicit agreement needs published support from distinct speakers on the same target.",
        }
    }

    /// Shape mistakes are discoverable from the published JSON Schema.
    /// Alias, consensus, and quota failures are not.
    const fn is_schema_shape(self) -> bool {
        matches!(
            self,
            Self::InvalidJson
                | Self::DuplicateKey
                | Self::UnknownField
                | Self::MissingField
                | Self::MaxDepth
                | Self::TrailingData
                | Self::IdentityNotSelectable
                | Self::WrongPhaseKind
                | Self::EmptySummary
                | Self::ClaimCount
                | Self::ResponseCount
                | Self::OpenQuestionCount
                | Self::PositionChangeCount
                | Self::DuplicateLocalKey
                | Self::EmptyText
                | Self::AbstainReason
                | Self::AbstainClaims
                | Self::AbstainShape
                | Self::CoverageRejected
        )
    }

    fn from_reason(reason: &str) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|code| code.as_str() == reason)
            .unwrap_or(Self::InvalidJson)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub path: String,
    pub code: FieldCode,
    pub message: String,
}

pub fn validate_result(
    raw: &[u8],
    scope: &ResultScope,
) -> Result<ValidatedResult, Vec<FieldError>> {
    let document = document(raw)?;
    if document.canonical.len() > quota_bytes(scope) {
        return Err(vec![field("$", FieldCode::ByteLimit)]);
    }
    let mut forbidden = Vec::new();
    reject_forbidden(&document.value, "$", &mut forbidden);
    if !forbidden.is_empty() {
        return Err(forbidden);
    }
    let Some(kind) = document.value.get("kind").and_then(Value::as_str) else {
        return Err(vec![field("$.kind", FieldCode::MissingField)]);
    };
    match scope.phase_kind {
        PhaseKind::Proposal if kind == "proposal" || kind == "abstain" => {
            validate_member(&document, scope)
        }
        PhaseKind::Critique if kind == "critique" || kind == "abstain" => {
            validate_member(&document, scope)
        }
        PhaseKind::Synthesis if kind == "synthesis" => validate_moderator(&document, scope),
        _ => Err(vec![field("$.kind", FieldCode::WrongPhaseKind)]),
    }
}

pub fn submit_candidate(
    state: &SubmissionState,
    submission_id: &SubmissionId,
    raw: &[u8],
    scope: &ResultScope,
) -> SubmissionDecision {
    let hash = payload_hash(raw);
    if let Some(receipt) = &state.sealed {
        if &receipt.submission_id == submission_id && receipt.payload_hash == hash {
            return decision(state.clone(), DecisionKind::Staged(receipt.clone()), false);
        }
        return decision(state.clone(), DecisionKind::ResultAlreadySealed, false);
    }
    if state.closed {
        return decision(state.clone(), DecisionKind::AttemptClosed, false);
    }
    if let Some(prior) = state.seen.get(submission_id) {
        if prior.hash == hash {
            return decision(
                state.clone(),
                DecisionKind::FieldErrors(prior.errors.clone()),
                false,
            );
        }
        return decision(state.clone(), DecisionKind::SubmissionConflict, false);
    }
    match validate_result(raw, scope) {
        Ok(validated) => {
            let receipt = CandidateReceipt {
                submission_id: submission_id.clone(),
                payload_hash: validated.canonical_hash,
                candidate_id: validated.canonical_hash.to_hex(),
                state: CandidateState::Staged,
            };
            let mut next = state.clone();
            next.sealed = Some(receipt.clone());
            next.seen.insert(
                submission_id.clone(),
                SeenSubmission {
                    hash: receipt.payload_hash,
                    errors: Vec::new(),
                },
            );
            decision(next, DecisionKind::Staged(receipt), false)
        }
        Err(errors) => {
            let mut next = state.clone();
            let closes = if shape_only(&errors) {
                next.shape_invalid_count = next.shape_invalid_count.saturating_add(1);
                next.shape_invalid_count > MAX_REPAIRABLE_SHAPE_SUBMISSIONS
            } else {
                next.invalid_count = next.invalid_count.saturating_add(1);
                next.invalid_count > MAX_REPAIRABLE_INVALID_SUBMISSIONS
            };
            next.closed = closes;
            next.seen.insert(
                submission_id.clone(),
                SeenSubmission {
                    hash,
                    errors: errors.clone(),
                },
            );
            decision(next, DecisionKind::FieldErrors(errors), closes)
        }
    }
}

pub fn validate_consensus(
    item: &ConsensusItemV1,
    published: &PublishedHistory,
) -> Result<(), Vec<FieldError>> {
    if item.agreement_level != AgreementLevel::ExplicitAgreement {
        return Ok(());
    }
    let edges = support_edges(published);
    let mut matched = Vec::new();
    for alias in &item.support_response_aliases {
        let Ok(response_id) = alias.parse::<ResponseId>() else {
            return Err(consensus_error());
        };
        let Some(edge) = edges.iter().find(|edge| edge.response_id == response_id) else {
            return Err(consensus_error());
        };
        matched.push(edge);
    }
    if matched.len() < 2 {
        return Err(consensus_error());
    }
    let claim_id = matched[0].claim_id;
    if matched.iter().any(|edge| edge.claim_id != claim_id) {
        return Err(consensus_error());
    }
    let speakers: BTreeSet<SpeakerId> = matched.iter().map(|edge| edge.speaker_id).collect();
    if speakers.len() != matched.len() {
        return Err(consensus_error());
    }
    let mut listed = BTreeSet::new();
    for alias in &item.supporter_aliases {
        let Ok(speaker_id) = alias.parse::<SpeakerId>() else {
            return Err(consensus_error());
        };
        listed.insert(speaker_id);
    }
    if listed != speakers {
        return Err(consensus_error());
    }
    Ok(())
}

struct Document {
    value: Value,
    canonical: Vec<u8>,
    hash: Hash256,
}

struct ResolvedTarget {
    speaker_id: SpeakerId,
    claim_id: Option<ClaimId>,
    response_id: Option<ResponseId>,
}

struct SupportEdge {
    speaker_id: SpeakerId,
    response_id: ResponseId,
    claim_id: ClaimId,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModeratorInput {
    kind: ModeratorKind,
    recommendation: ConclusionV1,
    alternatives: Vec<ConclusionV1>,
    consensus_items: Vec<ConsensusItemV1>,
    disagreements: Vec<ConclusionV1>,
    risks: Vec<ConclusionV1>,
    decision_requests: Vec<ConclusionV1>,
}

fn document(raw: &[u8]) -> Result<Document, Vec<FieldError>> {
    if raw.len() > v1_1::MAX_RESULT_BYTES as usize {
        return Err(vec![field("$", FieldCode::ByteLimit)]);
    }
    let limits = ParseLimits {
        origin: LimitsOrigin::Custom,
        max_bytes: v1_1::MAX_RESULT_BYTES as usize,
        max_depth: crate::profile_suggestions::MAX_JSON_DEPTH,
    };
    let value = parse_strict_json(raw, &limits).map_err(errors_from_parse)?;
    let canonical = canonical_bytes(&value).map_err(errors_from_parse)?;
    let hash = Hash256::sha256(&canonical);
    Ok(Document {
        value,
        canonical,
        hash,
    })
}

fn payload_hash(raw: &[u8]) -> Hash256 {
    match document(raw) {
        Ok(document) => document.hash,
        Err(_) => Hash256::sha256(raw),
    }
}

fn quota_bytes(scope: &ResultScope) -> usize {
    scope.quota_bytes.min(v1_1::MAX_RESULT_BYTES) as usize
}

fn validate_member(
    document: &Document,
    scope: &ResultScope,
) -> Result<ValidatedResult, Vec<FieldError>> {
    reject_unknown(&document.value, MEMBER_FIELDS)?;
    let shape = member_shape_errors(&document.value);
    if !shape.is_empty() {
        return Err(shape);
    }
    let member: MemberResultV1 =
        serde_json::from_value(document.value.clone()).map_err(errors_from_serde)?;
    let mut errors = Vec::new();
    if member.kind == MemberKind::Abstain {
        check_abstain(&member, &mut errors);
    } else {
        check_active_member(&member, scope, &mut errors);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ValidatedResult {
        byte_len: document.canonical.len(),
        canonical_hash: document.hash,
        counts_toward_quorum: member.kind != MemberKind::Abstain,
        abstained: member.kind == MemberKind::Abstain,
        member: Some(member),
        moderator: None,
    })
}

fn validate_moderator(
    document: &Document,
    scope: &ResultScope,
) -> Result<ValidatedResult, Vec<FieldError>> {
    reject_unknown(&document.value, MODERATOR_FIELDS)?;
    let shape = moderator_shape_errors(&document.value);
    if !shape.is_empty() {
        return Err(shape);
    }
    let input: ModeratorInput =
        serde_json::from_value(document.value.clone()).map_err(errors_from_serde)?;
    let mut errors = Vec::new();
    check_conclusion(
        "$.recommendation",
        &input.recommendation,
        scope,
        &mut errors,
    );
    check_each("$.alternatives", &input.alternatives, scope, &mut errors);
    check_each("$.disagreements", &input.disagreements, scope, &mut errors);
    check_each("$.risks", &input.risks, scope, &mut errors);
    check_each(
        "$.decision_requests",
        &input.decision_requests,
        scope,
        &mut errors,
    );
    for (index, item) in input.consensus_items.iter().enumerate() {
        let path = format!("$.consensus_items[{index}]");
        check_alias_list(
            &path,
            &item.text,
            &item.aliases,
            item.inference,
            scope,
            &mut errors,
        );
        if item.agreement_level == AgreementLevel::ExplicitAgreement {
            if let Some(rewritten) = rewrite_support(item, scope, &path, &mut errors) {
                if let Err(more) = validate_consensus(&rewritten, &scope.published) {
                    errors.extend(more.into_iter().map(|mut error| {
                        if error.path == "$" {
                            error.path = path.clone();
                        }
                        error
                    }));
                }
            }
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ValidatedResult {
        byte_len: document.canonical.len(),
        canonical_hash: document.hash,
        counts_toward_quorum: false,
        abstained: false,
        member: None,
        moderator: Some(ModeratorResultV1 {
            kind: input.kind,
            speaker_id: scope.speaker_id,
            recommendation: input.recommendation,
            alternatives: input.alternatives,
            consensus_items: input.consensus_items,
            disagreements: input.disagreements,
            risks: input.risks,
            decision_requests: input.decision_requests,
            coverage: None,
        }),
    })
}

fn check_abstain(member: &MemberResultV1, errors: &mut Vec<FieldError>) {
    if member.summary.trim().is_empty() {
        errors.push(field("$.summary", FieldCode::EmptySummary));
    }
    if member
        .reason
        .as_deref()
        .is_none_or(|reason| reason.trim().is_empty())
    {
        errors.push(field("$.reason", FieldCode::AbstainReason));
    }
    if !member.claims.is_empty() {
        errors.push(field("$.claims", FieldCode::AbstainClaims));
    }
    if !member.responses.is_empty() || !member.position_changes.is_empty() {
        errors.push(field("$", FieldCode::AbstainShape));
    }
    check_open_questions(&member.open_questions, errors);
}

fn check_active_member(member: &MemberResultV1, scope: &ResultScope, errors: &mut Vec<FieldError>) {
    if member.summary.trim().is_empty() {
        errors.push(field("$.summary", FieldCode::EmptySummary));
    }
    if member.claims.is_empty() || member.claims.len() > v1_1::MAX_CLAIMS {
        errors.push(field("$.claims", FieldCode::ClaimCount));
    }
    let mut local_keys = BTreeSet::new();
    for (index, claim) in member.claims.iter().enumerate() {
        if claim.local_key.is_empty() {
            errors.push(field(
                format!("$.claims[{index}].local_key"),
                FieldCode::EmptyText,
            ));
        } else if !local_keys.insert(claim.local_key.as_str()) {
            errors.push(field(
                format!("$.claims[{index}].local_key"),
                FieldCode::DuplicateLocalKey,
            ));
        }
        if claim.text.trim().is_empty() {
            errors.push(field(
                format!("$.claims[{index}].text"),
                FieldCode::EmptyText,
            ));
        }
        check_evidence(
            &format!("$.claims[{index}]"),
            &claim.evidence_aliases,
            scope,
            errors,
        );
    }
    if member.responses.len() > v1_1::MAX_RESPONSES {
        errors.push(field("$.responses", FieldCode::ResponseCount));
    }
    let mut covered = vec![false; scope.mandatory_targets.len()];
    for (index, response) in member.responses.iter().enumerate() {
        let path = format!("$.responses[{index}]");
        if response.text.trim().is_empty() {
            errors.push(field(format!("{path}.text"), FieldCode::EmptyText));
        }
        check_evidence(&path, &response.evidence_aliases, scope, errors);
        if let Some(target) = resolve_response(&path, response, scope, errors) {
            if response.stance == Stance::Support && target.speaker_id == scope.speaker_id {
                errors.push(field(&path, FieldCode::SelfSupport));
            } else {
                mark_covered(&target, &scope.mandatory_targets, &mut covered);
            }
        }
    }
    if covered.iter().any(|hit| !hit) {
        errors.push(field("$.responses", FieldCode::MandatoryTarget));
    }
    check_open_questions(&member.open_questions, errors);
    if member.position_changes.len() > v1_1::MAX_POSITION_CHANGES {
        errors.push(field("$.position_changes", FieldCode::PositionChangeCount));
    }
    for (index, change) in member.position_changes.iter().enumerate() {
        let path = format!("$.position_changes[{index}]");
        if change.reason.trim().is_empty() {
            errors.push(field(format!("{path}.reason"), FieldCode::EmptyText));
        }
        match scope.aliases.claims.get(&change.own_prior_claim_alias) {
            None => errors.push(field(
                format!("{path}.own_prior_claim_alias"),
                FieldCode::UnknownAlias,
            )),
            Some(claim) if claim.speaker_id != scope.speaker_id => errors.push(field(
                format!("{path}.own_prior_claim_alias"),
                FieldCode::ClaimOwner,
            )),
            Some(claim) if !claim.published => errors.push(field(
                format!("{path}.own_prior_claim_alias"),
                FieldCode::UnpublishedClaim,
            )),
            Some(_) => {}
        }
        if !local_keys.contains(change.new_local_claim_key.as_str()) {
            errors.push(field(
                format!("{path}.new_local_claim_key"),
                FieldCode::MissingLocalClaim,
            ));
        }
        for (trigger_index, alias) in change.trigger_response_aliases.iter().enumerate() {
            match scope.aliases.responses.get(alias) {
                Some(response) if response.published => {}
                Some(_) => errors.push(field(
                    format!("{path}.trigger_response_aliases[{trigger_index}]"),
                    FieldCode::UnpublishedResponse,
                )),
                None => errors.push(field(
                    format!("{path}.trigger_response_aliases[{trigger_index}]"),
                    FieldCode::UnknownAlias,
                )),
            }
        }
    }
}

fn check_open_questions(questions: &[String], errors: &mut Vec<FieldError>) {
    if questions.len() > v1_1::MAX_OPEN_QUESTIONS {
        errors.push(field("$.open_questions", FieldCode::OpenQuestionCount));
    }
    for (index, question) in questions.iter().enumerate() {
        if question.trim().is_empty() {
            errors.push(field(
                format!("$.open_questions[{index}]"),
                FieldCode::EmptyText,
            ));
        }
    }
}

fn check_evidence(
    prefix: &str,
    aliases: &[String],
    scope: &ResultScope,
    errors: &mut Vec<FieldError>,
) {
    for (index, alias) in aliases.iter().enumerate() {
        let path = format!("{prefix}.evidence_aliases[{index}]");
        match scope.aliases.evidence.get(alias) {
            None => errors.push(field(path, FieldCode::UnknownAlias)),
            Some(evidence) if evidence.visibility == AliasVisibility::PeerStaged => {
                errors.push(field(path, FieldCode::PeerStagedEvidence));
            }
            Some(_) => {}
        }
    }
}

fn resolve_response(
    path: &str,
    response: &crate::ResponseV1,
    scope: &ResultScope,
    errors: &mut Vec<FieldError>,
) -> Option<ResolvedTarget> {
    match (
        response.target_claim_alias.as_deref(),
        response.target_response_alias.as_deref(),
    ) {
        (Some(_), Some(_)) => {
            errors.push(field(path, FieldCode::AmbiguousTarget));
            None
        }
        (None, None) => {
            errors.push(field(path, FieldCode::MissingTarget));
            None
        }
        (Some(alias), None) => match scope.aliases.claims.get(alias) {
            None => {
                errors.push(field(
                    format!("{path}.target_claim_alias"),
                    FieldCode::UnknownAlias,
                ));
                None
            }
            Some(claim) if !claim.published => {
                errors.push(field(
                    format!("{path}.target_claim_alias"),
                    FieldCode::UnpublishedClaim,
                ));
                None
            }
            Some(claim) => Some(ResolvedTarget {
                speaker_id: claim.speaker_id,
                claim_id: Some(claim.claim_id),
                response_id: None,
            }),
        },
        (None, Some(alias)) => match scope.aliases.responses.get(alias) {
            None => {
                errors.push(field(
                    format!("{path}.target_response_alias"),
                    FieldCode::UnknownAlias,
                ));
                None
            }
            Some(target) if !target.published => {
                errors.push(field(
                    format!("{path}.target_response_alias"),
                    FieldCode::UnpublishedResponse,
                ));
                None
            }
            Some(target) => Some(ResolvedTarget {
                speaker_id: target.speaker_id,
                claim_id: None,
                response_id: Some(target.response_id),
            }),
        },
    }
}

fn mark_covered(target: &ResolvedTarget, required: &[RequiredTarget], covered: &mut [bool]) {
    for (index, item) in required.iter().enumerate() {
        let hit = if let Some(response_id) = item.response_id {
            target.response_id == Some(response_id)
        } else {
            target.claim_id == Some(item.claim_id) && target.response_id.is_none()
        };
        if hit {
            covered[index] = true;
        }
    }
}

fn check_each(
    prefix: &str,
    items: &[ConclusionV1],
    scope: &ResultScope,
    errors: &mut Vec<FieldError>,
) {
    for (index, item) in items.iter().enumerate() {
        check_conclusion(&format!("{prefix}[{index}]"), item, scope, errors);
    }
}

fn check_conclusion(
    path: &str,
    item: &ConclusionV1,
    scope: &ResultScope,
    errors: &mut Vec<FieldError>,
) {
    check_alias_list(
        path,
        &item.text,
        &item.aliases,
        item.inference,
        scope,
        errors,
    );
}

fn check_alias_list(
    path: &str,
    text: &str,
    aliases: &[AliasRefV1],
    inference: bool,
    scope: &ResultScope,
    errors: &mut Vec<FieldError>,
) {
    if text.trim().is_empty() {
        errors.push(field(format!("{path}.text"), FieldCode::EmptyText));
    }
    let mut valid = 0usize;
    for (index, alias) in aliases.iter().enumerate() {
        let alias_path = format!("{path}.aliases[{index}]");
        match classify_alias(alias, scope) {
            AliasHit::Valid => valid += 1,
            AliasHit::Missing => errors.push(field(alias_path, FieldCode::UnknownAlias)),
            AliasHit::PeerStaged => {
                errors.push(field(alias_path, FieldCode::PeerStagedEvidence));
            }
            AliasHit::Unpublished => {
                errors.push(field(alias_path, FieldCode::UnpublishedClaim));
            }
        }
    }
    if valid == 0 && !inference {
        errors.push(field(path, FieldCode::InferenceRequired));
    }
}

enum AliasHit {
    Valid,
    Missing,
    PeerStaged,
    Unpublished,
}

fn classify_alias(alias: &AliasRefV1, scope: &ResultScope) -> AliasHit {
    if alias.alias.is_empty() {
        return AliasHit::Missing;
    }
    match alias.kind {
        AliasKind::Message => {
            if scope.aliases.messages.contains_key(&alias.alias) {
                AliasHit::Valid
            } else {
                AliasHit::Missing
            }
        }
        AliasKind::Claim => match scope.aliases.claims.get(&alias.alias) {
            None => AliasHit::Missing,
            Some(claim) if claim.published => AliasHit::Valid,
            Some(_) => AliasHit::Unpublished,
        },
        AliasKind::Evidence => match scope.aliases.evidence.get(&alias.alias) {
            None => AliasHit::Missing,
            Some(evidence) if evidence.visibility == AliasVisibility::PeerStaged => {
                AliasHit::PeerStaged
            }
            Some(_) => AliasHit::Valid,
        },
    }
}

fn rewrite_support(
    item: &ConsensusItemV1,
    scope: &ResultScope,
    path: &str,
    errors: &mut Vec<FieldError>,
) -> Option<ConsensusItemV1> {
    let mut ok = true;
    let mut supporters = Vec::new();
    for (index, alias) in item.supporter_aliases.iter().enumerate() {
        match scope.aliases.speakers.get(alias) {
            Some(speaker_id) => supporters.push(speaker_id.to_string()),
            None => {
                ok = false;
                errors.push(field(
                    format!("{path}.supporter_aliases[{index}]"),
                    FieldCode::UnknownAlias,
                ));
            }
        }
    }
    let mut responses = Vec::new();
    for (index, alias) in item.support_response_aliases.iter().enumerate() {
        match scope.aliases.responses.get(alias) {
            Some(response) => responses.push(response.response_id.to_string()),
            None => {
                ok = false;
                errors.push(field(
                    format!("{path}.support_response_aliases[{index}]"),
                    FieldCode::UnknownAlias,
                ));
            }
        }
    }
    if !ok {
        return None;
    }
    Some(ConsensusItemV1 {
        text: item.text.clone(),
        agreement_level: item.agreement_level,
        aliases: item.aliases.clone(),
        supporter_aliases: supporters,
        support_response_aliases: responses,
        inference: item.inference,
    })
}

fn support_edges(published: &PublishedHistory) -> Vec<SupportEdge> {
    let mut claims = BTreeSet::new();
    for phase in &published.phases {
        for member in &phase.members {
            if !published_member(member) {
                continue;
            }
            for claim in &member.claims {
                if !claim.text.is_empty() {
                    claims.insert(claim.claim_id);
                }
            }
        }
    }
    let mut edges = Vec::new();
    for phase in &published.phases {
        for member in &phase.members {
            if !published_member(member) {
                continue;
            }
            for response in &member.responses {
                if response.stance != Stance::Support {
                    continue;
                }
                let Some(claim_id) = response.target_claim_id else {
                    continue;
                };
                if !claims.contains(&claim_id) {
                    continue;
                }
                edges.push(SupportEdge {
                    speaker_id: member.speaker.speaker_id,
                    response_id: response.response_id,
                    claim_id,
                });
            }
        }
    }
    edges
}

/// Local published-vote predicate. Seal checks do not call the phase strategy.
fn published_member(member: &PublishedMember) -> bool {
    member.status == PublicationStatus::Accepted
        && member.kind != MemberKind::Abstain
        && member
            .claims
            .first()
            .is_some_and(|claim| !claim.text.is_empty())
}

fn reject_forbidden(value: &Value, path: &str, errors: &mut Vec<FieldError>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                if IDENTITY_KEYS.contains(&key.as_str()) {
                    errors.push(field(child_path, FieldCode::IdentityNotSelectable));
                } else if key == "coverage" {
                    errors.push(field(child_path, FieldCode::CoverageRejected));
                } else {
                    reject_forbidden(child, &child_path, errors);
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                reject_forbidden(item, &format!("{path}[{index}]"), errors);
            }
        }
        _ => {}
    }
}

fn reject_unknown(value: &Value, allowed: &[&str]) -> Result<(), Vec<FieldError>> {
    let Some(map) = value.as_object() else {
        return Err(vec![field("$", FieldCode::InvalidJson)]);
    };
    let mut errors = Vec::new();
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.push(field(format!("$.{key}"), FieldCode::UnknownField));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn errors_from_parse(err: RtError) -> Vec<FieldError> {
    if !err.details.field_errors.is_empty() {
        return err
            .details
            .field_errors
            .into_iter()
            .map(|item| {
                let code = FieldCode::from_reason(&item.reason);
                FieldError {
                    path: item.path,
                    message: code.message().to_string(),
                    code,
                }
            })
            .collect();
    }
    let code = FieldCode::from_reason(err.details.reason.as_deref().unwrap_or("invalid_json"));
    vec![field("$", code)]
}

fn errors_from_serde(err: serde_json::Error) -> Vec<FieldError> {
    let text = err.to_string();
    if let Some(name) = between(&text, "missing field `", "`") {
        return vec![field(format!("$.{name}"), FieldCode::MissingField)];
    }
    if let Some(name) = between(&text, "unknown field `", "`") {
        return vec![field(format!("$.{name}"), FieldCode::UnknownField)];
    }
    vec![field("$", FieldCode::InvalidJson)]
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = text.split_once(start)?.1;
    Some(rest.split_once(end)?.0)
}

fn consensus_error() -> Vec<FieldError> {
    vec![field("$", FieldCode::ConsensusSupport)]
}

fn field(path: impl Into<String>, code: FieldCode) -> FieldError {
    FieldError {
        path: path.into(),
        code,
        message: code.message().to_string(),
    }
}

fn decision(
    next_state: SubmissionState,
    outcome: DecisionKind,
    closes_attempt: bool,
) -> SubmissionDecision {
    SubmissionDecision {
        outcome,
        next_state,
        closes_attempt,
    }
}

#[cfg(test)]
mod schema_parity_tests {
    use super::*;

    #[test]
    fn model_schema_property_names_match_validator_allowlists() {
        for (kind, allowed) in [(PhaseKind::Proposal, MEMBER_FIELDS), (PhaseKind::Synthesis, MODERATOR_FIELDS)] {
            let schema = result_schema(Some(kind));
            let actual: BTreeSet<_> = schema["properties"].as_object().unwrap().keys().map(String::as_str).collect();
            let expected: BTreeSet<_> = allowed.iter().copied().collect();
            assert_eq!(actual,expected);
        }
    }
}

fn shape_only(errors: &[FieldError]) -> bool {
    !errors.is_empty() && errors.iter().all(|error| error.code.is_schema_shape())
}

fn member_shape_errors(value: &Value) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let Some(map) = value.as_object() else {
        return vec![field("$", FieldCode::InvalidJson)];
    };
    expect_string(map, "$", "kind", &mut errors);
    expect_string(map, "$", "summary", &mut errors);
    if let Some(claims) = expect_array(map, "$", "claims", &mut errors) {
        for (index, claim) in claims.iter().enumerate() {
            claim_shape(claim, &format!("$.claims[{index}]"), &mut errors);
        }
    }
    if let Some(responses) = optional_array(map, "$", "responses", &mut errors) {
        for (index, response) in responses.iter().enumerate() {
            response_shape(response, &format!("$.responses[{index}]"), &mut errors);
        }
    }
    if let Some(questions) = optional_array(map, "$", "open_questions", &mut errors) {
        for (index, question) in questions.iter().enumerate() {
            if !question.is_string() {
                errors.push(field(
                    format!("$.open_questions[{index}]"),
                    FieldCode::InvalidJson,
                ));
            }
        }
    }
    if let Some(changes) = optional_array(map, "$", "position_changes", &mut errors) {
        for (index, change) in changes.iter().enumerate() {
            position_shape(change, &format!("$.position_changes[{index}]"), &mut errors);
        }
    }
    if map.get("reason").is_some_and(|reason| !reason.is_string()) {
        errors.push(field("$.reason", FieldCode::InvalidJson));
    }
    errors
}

fn moderator_shape_errors(value: &Value) -> Vec<FieldError> {
    let mut errors = Vec::new();
    let Some(map) = value.as_object() else {
        return vec![field("$", FieldCode::InvalidJson)];
    };
    expect_string(map, "$", "kind", &mut errors);
    match map.get("recommendation") {
        None => errors.push(field("$.recommendation", FieldCode::MissingField)),
        Some(item) => conclusion_shape(item, "$.recommendation", &mut errors),
    }
    for name in [
        "alternatives",
        "disagreements",
        "risks",
        "decision_requests",
    ] {
        if let Some(items) = expect_array(map, "$", name, &mut errors) {
            for (index, item) in items.iter().enumerate() {
                conclusion_shape(item, &format!("$.{name}[{index}]"), &mut errors);
            }
        }
    }
    if let Some(items) = expect_array(map, "$", "consensus_items", &mut errors) {
        for (index, item) in items.iter().enumerate() {
            consensus_shape(item, &format!("$.consensus_items[{index}]"), &mut errors);
        }
    }
    errors
}

fn claim_shape(value: &Value, path: &str, errors: &mut Vec<FieldError>) {
    let Some(map) = object_fields(
        value,
        path,
        &["local_key", "text", "evidence_aliases", "confidence"],
        errors,
    ) else {
        return;
    };
    expect_string(map, path, "local_key", errors);
    expect_string(map, path, "text", errors);
    expect_string_array(map, path, "evidence_aliases", errors);
    expect_enum(map, path, "confidence", &["low", "medium", "high"], errors);
}

fn response_shape(value: &Value, path: &str, errors: &mut Vec<FieldError>) {
    let Some(map) = object_fields(
        value,
        path,
        &[
            "target_claim_alias",
            "target_response_alias",
            "stance",
            "priority",
            "text",
            "evidence_aliases",
        ],
        errors,
    ) else {
        return;
    };
    expect_optional_string(map, path, "target_claim_alias", errors);
    expect_optional_string(map, path, "target_response_alias", errors);
    expect_enum(
        map,
        path,
        "stance",
        &["support", "challenge", "clarify", "revise"],
        errors,
    );
    expect_enum(map, path, "priority", &["normal", "critical"], errors);
    expect_string(map, path, "text", errors);
    expect_string_array(map, path, "evidence_aliases", errors);
}

fn position_shape(value: &Value, path: &str, errors: &mut Vec<FieldError>) {
    let Some(map) = object_fields(
        value,
        path,
        &[
            "own_prior_claim_alias",
            "new_local_claim_key",
            "reason",
            "trigger_response_aliases",
        ],
        errors,
    ) else {
        return;
    };
    expect_string(map, path, "own_prior_claim_alias", errors);
    expect_string(map, path, "new_local_claim_key", errors);
    expect_string(map, path, "reason", errors);
    expect_string_array(map, path, "trigger_response_aliases", errors);
}

fn conclusion_shape(value: &Value, path: &str, errors: &mut Vec<FieldError>) {
    let Some(map) = object_fields(value, path, &["text", "aliases", "inference"], errors) else {
        return;
    };
    expect_string(map, path, "text", errors);
    if let Some(aliases) = expect_array(map, path, "aliases", errors) {
        for (index, alias) in aliases.iter().enumerate() {
            alias_shape(alias, &format!("{path}.aliases[{index}]"), errors);
        }
    }
    expect_bool(map, path, "inference", errors);
}

fn consensus_shape(value: &Value, path: &str, errors: &mut Vec<FieldError>) {
    let Some(map) = object_fields(
        value,
        path,
        &[
            "text",
            "agreement_level",
            "aliases",
            "supporter_aliases",
            "support_response_aliases",
            "inference",
        ],
        errors,
    ) else {
        return;
    };
    expect_string(map, path, "text", errors);
    expect_enum(
        map,
        path,
        "agreement_level",
        &["explicit_agreement", "compatible_positions", "unresolved"],
        errors,
    );
    if let Some(aliases) = expect_array(map, path, "aliases", errors) {
        for (index, alias) in aliases.iter().enumerate() {
            alias_shape(alias, &format!("{path}.aliases[{index}]"), errors);
        }
    }
    expect_string_array(map, path, "supporter_aliases", errors);
    expect_string_array(map, path, "support_response_aliases", errors);
    expect_bool(map, path, "inference", errors);
}

fn alias_shape(value: &Value, path: &str, errors: &mut Vec<FieldError>) {
    let Some(map) = object_fields(value, path, &["kind", "alias"], errors) else {
        return;
    };
    expect_enum(map, path, "kind", &["message", "claim", "evidence"], errors);
    expect_string(map, path, "alias", errors);
}

fn object_fields<'a>(
    value: &'a Value,
    path: &str,
    allowed: &[&str],
    errors: &mut Vec<FieldError>,
) -> Option<&'a serde_json::Map<String, Value>> {
    let Some(map) = value.as_object() else {
        errors.push(field(path, FieldCode::InvalidJson));
        return None;
    };
    for key in map.keys() {
        if !allowed.contains(&key.as_str()) {
            errors.push(field(format!("{path}.{key}"), FieldCode::UnknownField));
        }
    }
    Some(map)
}

fn expect_string(
    map: &serde_json::Map<String, Value>,
    path: &str,
    key: &str,
    errors: &mut Vec<FieldError>,
) {
    match map.get(key) {
        None => errors.push(field(format!("{path}.{key}"), FieldCode::MissingField)),
        Some(Value::String(_)) => {}
        Some(_) => errors.push(field(format!("{path}.{key}"), FieldCode::InvalidJson)),
    }
}

fn expect_optional_string(
    map: &serde_json::Map<String, Value>,
    path: &str,
    key: &str,
    errors: &mut Vec<FieldError>,
) {
    if map.get(key).is_some_and(|value| !value.is_string()) {
        errors.push(field(format!("{path}.{key}"), FieldCode::InvalidJson));
    }
}

fn expect_bool(
    map: &serde_json::Map<String, Value>,
    path: &str,
    key: &str,
    errors: &mut Vec<FieldError>,
) {
    match map.get(key) {
        None => errors.push(field(format!("{path}.{key}"), FieldCode::MissingField)),
        Some(Value::Bool(_)) => {}
        Some(_) => errors.push(field(format!("{path}.{key}"), FieldCode::InvalidJson)),
    }
}

fn expect_enum(
    map: &serde_json::Map<String, Value>,
    path: &str,
    key: &str,
    allowed: &[&str],
    errors: &mut Vec<FieldError>,
) {
    match map.get(key).and_then(Value::as_str) {
        Some(value) if allowed.contains(&value) => {}
        None if map.get(key).is_none() => {
            errors.push(field(format!("{path}.{key}"), FieldCode::MissingField));
        }
        _ => errors.push(field(format!("{path}.{key}"), FieldCode::InvalidJson)),
    }
}

fn expect_array<'a>(
    map: &'a serde_json::Map<String, Value>,
    path: &str,
    key: &str,
    errors: &mut Vec<FieldError>,
) -> Option<&'a Vec<Value>> {
    match map.get(key) {
        Some(Value::Array(items)) => Some(items),
        None => {
            errors.push(field(format!("{path}.{key}"), FieldCode::MissingField));
            None
        }
        Some(_) => {
            errors.push(field(format!("{path}.{key}"), FieldCode::InvalidJson));
            None
        }
    }
}

fn optional_array<'a>(
    map: &'a serde_json::Map<String, Value>,
    path: &str,
    key: &str,
    errors: &mut Vec<FieldError>,
) -> Option<&'a Vec<Value>> {
    match map.get(key) {
        None => None,
        Some(Value::Array(items)) => Some(items),
        Some(_) => {
            errors.push(field(format!("{path}.{key}"), FieldCode::InvalidJson));
            None
        }
    }
}

fn expect_string_array(
    map: &serde_json::Map<String, Value>,
    path: &str,
    key: &str,
    errors: &mut Vec<FieldError>,
) {
    let Some(items) = expect_array(map, path, key, errors) else {
        return;
    };
    for (index, item) in items.iter().enumerate() {
        if !item.is_string() {
            errors.push(field(
                format!("{path}.{key}[{index}]"),
                FieldCode::InvalidJson,
            ));
        }
    }
}

/// JSON Schema for `submit_result` arguments. `result` matches the phase.
/// Identity fields (`speaker_id`, `coverage`) are omitted.
pub fn submit_result_input_schema(phase: PhaseKind) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["submission_id", "result"],
        "properties": {
            "submission_id": {
                "type": "string",
                "minLength": 1,
                "description": "New id for this body. Reuse an id only to retry the identical body."
            },
            "result": result_schema(Some(phase))
        }
    })
}

/// Compact example from the exact same contract as the prompt and MCP schema.
/// Dynamic mandatory targets still require the corresponding covering responses.
pub fn seat_schema_example(phase: PhaseKind) -> &'static str {
    static EXAMPLES: std::sync::OnceLock<[String;3]> = std::sync::OnceLock::new();
    let examples=EXAMPLES.get_or_init(|| [PhaseKind::Proposal,PhaseKind::Critique,PhaseKind::Synthesis]
        .map(|phase|format!("Use a new submission_id and address every mandatory target. Omit identity and coverage. Example result: {}",result_schema(Some(phase))["examples"][0])));
    &examples[match phase { PhaseKind::Proposal=>0,PhaseKind::Critique=>1,PhaseKind::Synthesis=>2 }]
}
