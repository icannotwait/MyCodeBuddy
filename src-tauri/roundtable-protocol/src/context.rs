//! Pure context bounds. Token algorithms stay behind [`TokenBound`].
//! A fake bound is not provided here and is not a capacity proof.

use serde::Serialize;

use crate::budget::{invalid, too_large, unknown};
use crate::canonical::canonical_bytes;
use crate::model::{
    canonical_hash, BindingId, DeliveryManifestV1, Hash256, PhaseSnapshotV1, RoundtableConfigV1,
    RtResult, SafeInt, SCHEMA_VERSION,
};

pub const JSON_ESCAPE_BYTES_PER_INPUT_BYTE: u64 = 6;
pub const EVIDENCE_REPLY_BYTES: u64 = 8_192;
pub const EVIDENCE_ATTEMPT_BYTES: u64 = 32_768;

/// Worst case for this crate's canonical JSON strings: a control byte becomes
/// `\u00XX` (6 bytes). Quote and backslash expand to 2. Raw `S` is not a bound.
pub fn json_string_worst_bytes(raw_bytes: u64) -> RtResult<u64> {
    raw_bytes
        .checked_mul(JSON_ESCAPE_BYTES_PER_INPUT_BYTE)
        .ok_or_else(|| invalid("overflow"))
}

/// `P × S` with `P = N × (1 + R)`. Member result bodies only.
pub fn member_result_body_bytes(n: u32, r: u32, result_bytes: u64) -> RtResult<u64> {
    let results = u64::from(n)
        .checked_mul(
            u64::from(r)
                .checked_add(1)
                .ok_or_else(|| invalid("overflow"))?,
        )
        .ok_or_else(|| invalid("overflow"))?;
    results
        .checked_mul(result_bytes)
        .ok_or_else(|| invalid("overflow"))
}

/// Cumulative result reading `N² × S × R × (R + 1) / 2`.
/// This is the whole-plan growth, not the single largest prompt window.
pub fn cumulative_result_reading_bytes(n: u32, r: u32, result_bytes: u64) -> RtResult<u64> {
    let participants = u64::from(n);
    let rounds = u64::from(r);
    let squared = participants
        .checked_mul(participants)
        .ok_or_else(|| invalid("overflow"))?;
    let round_term = rounds
        .checked_add(1)
        .and_then(|value| value.checked_mul(rounds))
        .ok_or_else(|| invalid("overflow"))?;
    squared
        .checked_mul(result_bytes)
        .and_then(|value| value.checked_mul(round_term))
        .and_then(|value| value.checked_div(2))
        .ok_or_else(|| invalid("overflow"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualifiedContextProfile {
    pub tokenizer_id: String,
    pub tokenizer_hash: Hash256,
    pub model_capacity_tokens: u64,
    pub max_model_requests: u32,
    pub max_tool_calls: u32,
    pub max_tool_reply_bytes: u64,
    pub max_attempt_generated_utf8_bytes: u64,
    pub max_request_body_bytes: u64,
    pub generation_reserve_tokens: u64,
    pub adapter_hidden_bound_tokens: u64,
    pub proof_ref: String,
}

impl QualifiedContextProfile {
    pub const MAX_MODEL_REQUESTS: u32 = 64;
    pub const MAX_TOOL_CALLS: u32 = 128;
    pub const MAX_TOOL_REPLY_BYTES: u64 = 131_072;
    pub const MAX_ATTEMPT_GENERATED_UTF8_BYTES: u64 = 262_144;
    pub const MAX_REQUEST_BODY_BYTES: u64 = 1_048_576;
    pub const GENERATION_RESERVE_TOKENS: u64 = 8_192;

    pub fn proposed(
        tokenizer_id: impl Into<String>,
        tokenizer_hash: Hash256,
        model_capacity_tokens: u64,
        adapter_hidden_bound_tokens: u64,
        proof_ref: impl Into<String>,
    ) -> Self {
        Self {
            tokenizer_id: tokenizer_id.into(),
            tokenizer_hash,
            model_capacity_tokens,
            max_model_requests: Self::MAX_MODEL_REQUESTS,
            max_tool_calls: Self::MAX_TOOL_CALLS,
            max_tool_reply_bytes: Self::MAX_TOOL_REPLY_BYTES,
            max_attempt_generated_utf8_bytes: Self::MAX_ATTEMPT_GENERATED_UTF8_BYTES,
            max_request_body_bytes: Self::MAX_REQUEST_BODY_BYTES,
            generation_reserve_tokens: Self::GENERATION_RESERVE_TOKENS,
            adapter_hidden_bound_tokens,
            proof_ref: proof_ref.into(),
        }
    }
}

pub trait TokenBound {
    fn upper_bound(&self, utf8: &[u8]) -> RtResult<u64>;
    fn capacity_tokens(&self) -> Option<u64>;

    /// Qualified length-only bounds can override this without allocating.
    /// The conservative default refuses unbounded user-controlled witnesses.
    fn upper_bound_for_length(&self, len: u64) -> RtResult<u64> {
        if len > 16 * 1024 * 1024 { return Err(unknown("capacity_unknown")); }
        let size = usize::try_from(len).map_err(|_| invalid("overflow"))?;
        let mut witness = Vec::new();
        witness.try_reserve_exact(size).map_err(|_| unknown("capacity_unknown"))?;
        witness.resize(size, 0);
        self.upper_bound(&witness)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedInputBounds {
    pub question: String,
    pub role: String,
    pub interjection: String,
    pub evidence: String,
    pub schema: String,
    pub tools: String,
    pub embedded: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextBound {
    pub exact_bytes: u64,
    pub future_bytes: u64,
    pub verified_token_upper_bound: u64,
    pub generation_reserve_tokens: u64,
    pub profile_hash: Hash256,
    pub member_result_body_bytes: u64,
    pub full_plan_reading_bytes: u64,
}

pub fn preflight_exact_prompt(
    config: &RoundtableConfigV1,
    input: &EncodedInputBounds,
) -> RtResult<Vec<u8>> {
    let roles: Vec<&str> = config
        .participants
        .iter()
        .map(|participant| participant.role.as_str())
        .collect();
    canonical_bytes(&KnownPrompt {
        embedded: &input.embedded,
        evidence: &input.evidence,
        interjection: &input.interjection,
        output_byte_limit: config.quotas.output_byte_limit.0,
        question: &input.question,
        role: &input.role,
        roles,
        schema: &input.schema,
        tools: &input.tools,
        topic: &config.topic,
    })
}

pub fn preflight_context(
    config: &RoundtableConfigV1,
    input: &EncodedInputBounds,
    tokens: &dyn TokenBound,
    profile: &QualifiedContextProfile,
) -> RtResult<ContextBound> {
    let participants = u32::try_from(config.participants.len()).map_err(|_| invalid("overflow"))?;
    let results = member_result_body_bytes(participants, config.strategy.critique_rounds, config.quotas.output_byte_limit.0)?;
    let future_bytes = json_string_worst_bytes(results)?
        .checked_add(json_string_worst_bytes(config.quotas.interjection_byte_limit.0)?)
        .and_then(|value| value.checked_add(json_string_worst_bytes(EVIDENCE_ATTEMPT_BYTES).ok()?))
        .and_then(|value| value.checked_add(json_string_worst_bytes(profile.max_tool_reply_bytes).ok()?))
        .ok_or_else(|| invalid("overflow"))?;
    preflight_encoded_context(config, input, tokens, profile, future_bytes)
}

/// Bound an already encoded future representation. Canonical result objects
/// must not be charged as if their strings were escaped a second time.
pub fn preflight_encoded_context(
    config: &RoundtableConfigV1,
    input: &EncodedInputBounds,
    tokens: &dyn TokenBound,
    profile: &QualifiedContextProfile,
    future_bytes: u64,
) -> RtResult<ContextBound> {
    let participants = u32::try_from(config.participants.len()).map_err(|_| invalid("overflow"))?;
    let rounds = config.strategy.critique_rounds;
    let result_bytes = config.quotas.output_byte_limit.0;
    let member_result_body_bytes = member_result_body_bytes(participants, rounds, result_bytes)?;
    let full_plan_reading_bytes =
        cumulative_result_reading_bytes(participants, rounds, result_bytes)?;
    let prompt = preflight_exact_prompt(config, input)?;
    let exact_bytes = u64::try_from(prompt.len()).map_err(|_| invalid("overflow"))?;
    let Some(capacity) = tokens.capacity_tokens() else {
        return Err(unknown("capacity_unknown"));
    };
    if capacity != profile.model_capacity_tokens {
        return Err(unknown("capacity_unknown"));
    }
    let verified_token_upper_bound = tokens
        .upper_bound(&prompt)?
        .checked_add(bound_length(tokens, future_bytes)?)
        .and_then(|value| value.checked_add(profile.adapter_hidden_bound_tokens))
        .ok_or_else(|| invalid("overflow"))?;
    let admission = verified_token_upper_bound
        .checked_add(profile.generation_reserve_tokens)
        .ok_or_else(|| invalid("overflow"))?;
    if admission > capacity {
        return Err(too_large("context_too_large"));
    }
    Ok(ContextBound {
        exact_bytes,
        future_bytes,
        verified_token_upper_bound,
        generation_reserve_tokens: profile.generation_reserve_tokens,
        profile_hash: canonical_hash(profile)?,
        member_result_body_bytes,
        full_plan_reading_bytes,
    })
}

/// Raw text quota and encoded context space are separate ceilings. The extra
/// 64 bytes admit one maximum-escaped input with its host-owned UUID wrapper.
pub fn interjection_context_limit(raw_quota: u64) -> RtResult<u64> {
    json_string_worst_bytes(raw_quota)?.checked_add(64).ok_or_else(|| invalid("overflow"))
}

/// Called both before an optional input is accepted and before frozen delivery.
/// Rejection here cannot spend a later model turn on an oversized input list.
pub fn validate_interjection_context(inputs: &serde_json::Value, raw_quota: u64) -> RtResult<u64> {
    if !inputs.is_array() { return Err(invalid("interjection_context")); }
    let bytes = canonical_bytes(inputs)?.len() as u64;
    if bytes > interjection_context_limit(raw_quota)? {
        return Err(too_large("interjection_context_bytes"));
    }
    Ok(bytes)
}

/// `reserved` is the creation-time interjection quota. Overspend is rejected.
pub fn charge_interjection(reserved: u64, already: u64, incoming: u64) -> RtResult<u64> {
    let used = already
        .checked_add(incoming)
        .ok_or_else(|| invalid("overflow"))?;
    reserved
        .checked_sub(used)
        .ok_or_else(|| invalid("underflow"))
}

fn bound_length(tokens: &dyn TokenBound, len: u64) -> RtResult<u64> {
    tokens.upper_bound_for_length(len)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSnapshot {
    pub role: String,
    pub model: String,
    pub effort: String,
    pub provider_ref: String,
    pub prompt_version: String,
    pub template_version: String,
    pub schema_id: String,
    pub schema_text: String,
    pub tool_version: String,
    pub tool_text: String,
}

/// One admission contract for creation-time worst-case delivery and the exact
/// prompt delivered later. Retain the result/evidence reserves and cover every
/// model request admitted by the qualified request-body cap. Token counts are
/// obtained only from the qualified bound; hidden and generation are charged
/// once after taking the larger of those two obligations.
pub fn admit_qualified_delivery(
    prompt_token_upper_bound: u64,
    output_byte_limit: u64,
    evidence_attempt_bytes: u64,
    tokens: &dyn TokenBound,
    profile: &QualifiedContextProfile,
) -> RtResult<u64> {
    let capacity = tokens.capacity_tokens().ok_or_else(|| unknown("capacity_unknown"))?;
    if capacity != profile.model_capacity_tokens { return Err(unknown("capacity_unknown")); }
    let future = json_string_worst_bytes(output_byte_limit)?
        .checked_add(json_string_worst_bytes(evidence_attempt_bytes)?)
        .ok_or_else(|| invalid("overflow"))?;
    let delivery = prompt_token_upper_bound.checked_add(bound_length(tokens, future)?)
        .ok_or_else(|| invalid("overflow"))?;
    let request = bound_length(tokens, profile.max_request_body_bytes)?;
    let admission = delivery.max(request)
        .checked_add(profile.adapter_hidden_bound_tokens)
        .and_then(|value| value.checked_add(profile.generation_reserve_tokens))
        .ok_or_else(|| invalid("overflow"))?;
    if admission > capacity { return Err(too_large("context_too_large")); }
    Ok(admission)
}

pub struct DeliveryEncoder;

impl DeliveryEncoder {
    pub fn prompt_utf8(
        phase: &PhaseSnapshotV1,
        role: &RoleSnapshot,
        binding: &BindingId,
    ) -> RtResult<Vec<u8>> {
        canonical_bytes(&DeliveryPrompt {
            binding_id: binding.to_string(),
            effort: &role.effort,
            model: &role.model,
            phase_hash: canonical_hash(phase)?.to_hex(),
            phase,
            provider_ref: &role.provider_ref,
            prompt_version: &role.prompt_version,
            role: &role.role,
            schema: &role.schema_text,
            schema_id: &role.schema_id,
            template_version: &role.template_version,
            tool_version: &role.tool_version,
            tools: &role.tool_text,
        })
    }

    pub fn encode(
        phase: &PhaseSnapshotV1,
        role: &RoleSnapshot,
        binding: &BindingId,
        tokens: &dyn TokenBound,
        profile: &QualifiedContextProfile,
    ) -> RtResult<DeliveryManifestV1> {
        let prompt = Self::prompt_utf8(phase, role, binding)?;
        Self::encode_prompt(phase, role, binding, tokens, profile, &prompt)
    }

    /// Encode the actual immutable content delivered to the participant. User
    /// content has its own field and cannot rewrite role or schema metadata.
    pub fn prompt_with_context(
        phase: &PhaseSnapshotV1,
        role: &RoleSnapshot,
        binding: &BindingId,
        context: &serde_json::Value,
    ) -> RtResult<Vec<u8>> {
        let metadata: serde_json::Value =
            serde_json::from_slice(&Self::prompt_utf8(phase, role, binding)?)
                .map_err(|_| invalid("delivery_encoding"))?;
        canonical_bytes(&serde_json::json!({"metadata": metadata, "context": context}))
    }

    pub fn encode_with_context(
        phase: &PhaseSnapshotV1,
        role: &RoleSnapshot,
        binding: &BindingId,
        context: &serde_json::Value,
        tokens: &dyn TokenBound,
        profile: &QualifiedContextProfile,
    ) -> RtResult<DeliveryManifestV1> {
        let prompt = Self::prompt_with_context(phase, role, binding, context)?;
        Self::encode_prompt(phase, role, binding, tokens, profile, &prompt)
    }

    /// Bind the service-owned seat and its alias-based targets into the exact
    /// bytes delivered to a fresh session. The shared phase hash stays intact.
    pub fn prompt_for_speaker(
        phase: &PhaseSnapshotV1,
        role: &RoleSnapshot,
        binding: &BindingId,
        speaker: &crate::SpeakerOrdinal,
        scope: &crate::ResultScope,
        context: &serde_json::Value,
    ) -> RtResult<Vec<u8>> {
        if scope.speaker_id != speaker.speaker_id || scope.phase_kind != phase.kind {
            return Err(invalid("delivery_identity"));
        }
        let mut metadata: serde_json::Value = serde_json::from_slice(&Self::prompt_utf8(phase, role, binding)?)
            .map_err(|_| invalid("delivery_encoding"))?;
        metadata["speaker_id"] = serde_json::json!(speaker.speaker_id);
        metadata["speaker_ordinal"] = serde_json::json!(speaker.ordinal);
        metadata["mandatory_targets"] = serde_json::json!(scope.mandatory_targets);
        canonical_bytes(&serde_json::json!({"metadata":metadata,"context":context}))
    }

    /// Hash and capacity-check the exact prompt sent by the runtime.
    pub fn encode_prompt(
        phase: &PhaseSnapshotV1,
        role: &RoleSnapshot,
        binding: &BindingId,
        tokens: &dyn TokenBound,
        profile: &QualifiedContextProfile,
        prompt: &[u8],
    ) -> RtResult<DeliveryManifestV1> {
        let exact = u64::try_from(prompt.len()).map_err(|_| invalid("overflow"))?;
        if exact > crate::MAX_SAFE_INTEGER {
            return Err(invalid("overflow"));
        }
        admit_qualified_delivery(
            tokens.upper_bound(prompt)?, phase.output_byte_limit.0,
            phase.tool_quota.per_attempt_bytes.0, tokens, profile,
        )?;
        Ok(DeliveryManifestV1 {
            schema_version: SCHEMA_VERSION,
            public_view_hash: canonical_hash(phase)?,
            role_hash: canonical_hash(role)?,
            prompt_version: role.prompt_version.clone(),
            template_version: role.template_version.clone(),
            schema_id: role.schema_id.clone(),
            tool_version: role.tool_version.clone(),
            output_byte_limit: phase.output_byte_limit,
            model: role.model.clone(),
            effort: role.effort.clone(),
            provider_ref: role.provider_ref.clone(),
            binding_id: *binding,
            prior_cursor: None,
            prompt_hash: Hash256::sha256(prompt),
            prompt_bytes: SafeInt(exact),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestTranscriptBound {
    pub adapter_bytes: u64,
    pub prior_output_bytes: u64,
    pub tool_argument_bytes: u64,
    pub tool_reply_bytes: u64,
    pub model_requests: u32,
    pub tool_calls: u32,
    pub generated_utf8_bytes: u64,
    pub evidence_attempt_bytes: u64,
}

impl RequestTranscriptBound {
    pub fn empty() -> Self {
        Self {
            adapter_bytes: 0,
            prior_output_bytes: 0,
            tool_argument_bytes: 0,
            tool_reply_bytes: 0,
            model_requests: 0,
            tool_calls: 0,
            generated_utf8_bytes: 0,
            evidence_attempt_bytes: 0,
        }
    }

    pub fn record_exchange(
        &mut self,
        exchange: &ToolExchange,
        _profile: &QualifiedContextProfile,
    ) -> RtResult<()> {
        let arguments = u64::try_from(exchange.arguments.len()).map_err(|_| invalid("overflow"))?;
        let reply = u64::try_from(exchange.reply.len()).map_err(|_| invalid("overflow"))?;
        let admitted = arguments
            .checked_add(reply)
            .ok_or_else(|| invalid("overflow"))?;
        // A declared evidence total must cover the bytes this exchange admits.
        // Zero is a non-evidence tool reply and stays on the tool-reply cap.
        if exchange.evidence_bytes > 0 && exchange.evidence_bytes < admitted {
            return Err(invalid("evidence_bytes"));
        }
        if exchange.evidence_bytes > EVIDENCE_REPLY_BYTES {
            return Err(too_large("evidence_reply_limit"));
        }
        let evidence = self
            .evidence_attempt_bytes
            .checked_add(exchange.evidence_bytes)
            .ok_or_else(|| invalid("overflow"))?;
        if evidence > EVIDENCE_ATTEMPT_BYTES {
            return Err(too_large("evidence_attempt_limit"));
        }
        self.tool_argument_bytes = self
            .tool_argument_bytes
            .checked_add(arguments)
            .ok_or_else(|| invalid("overflow"))?;
        self.tool_reply_bytes = self
            .tool_reply_bytes
            .checked_add(reply)
            .ok_or_else(|| invalid("overflow"))?;
        self.model_requests = self
            .model_requests
            .checked_add(exchange.model_requests)
            .ok_or_else(|| invalid("overflow"))?;
        self.tool_calls = self
            .tool_calls
            .checked_add(exchange.tool_calls)
            .ok_or_else(|| invalid("overflow"))?;
        self.generated_utf8_bytes = self
            .generated_utf8_bytes
            .checked_add(exchange.generated_utf8_bytes)
            .ok_or_else(|| invalid("overflow"))?;
        self.evidence_attempt_bytes = evidence;
        Ok(())
    }
}

pub fn bound_request(
    transcript: &RequestTranscriptBound,
    profile: &QualifiedContextProfile,
) -> RtResult<u64> {
    if transcript.model_requests > profile.max_model_requests
        || transcript.tool_calls > profile.max_tool_calls
        || transcript.tool_reply_bytes > profile.max_tool_reply_bytes
        || transcript.generated_utf8_bytes > profile.max_attempt_generated_utf8_bytes
        || transcript.evidence_attempt_bytes > EVIDENCE_ATTEMPT_BYTES
    {
        return Err(too_large("context_too_large"));
    }
    let body = transcript
        .adapter_bytes
        .checked_add(transcript.prior_output_bytes)
        .and_then(|value| value.checked_add(transcript.tool_argument_bytes))
        .and_then(|value| value.checked_add(transcript.tool_reply_bytes))
        .ok_or_else(|| invalid("overflow"))?;
    if body > profile.max_request_body_bytes {
        return Err(too_large("context_too_large"));
    }
    Ok(body)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExchange {
    pub arguments: Vec<u8>,
    pub reply: Vec<u8>,
    pub generated_utf8_bytes: u64,
    pub evidence_bytes: u64,
    pub model_requests: u32,
    pub tool_calls: u32,
}

impl ToolExchange {
    pub fn submit_field_errors(path: &str, reason: &str) -> Self {
        Self::counted(envelope("field_errors", &format!("{path}:{reason}")), 0)
    }

    pub fn receipt(submission_id: &str) -> Self {
        Self::counted(envelope("receipt", submission_id), 0)
    }

    pub fn search_empty() -> Self {
        Self::counted(envelope("search", "empty"), 0)
    }

    pub fn search_error() -> Self {
        Self::counted(envelope("search", "error"), 0)
    }

    pub fn evidence(bytes: u64) -> Self {
        let size = usize::try_from(bytes).unwrap_or(0);
        Self::counted(vec![b'e'; size], bytes)
    }

    fn counted(reply: Vec<u8>, evidence_bytes: u64) -> Self {
        Self {
            arguments: Vec::new(),
            reply,
            generated_utf8_bytes: 0,
            evidence_bytes,
            model_requests: 1,
            tool_calls: 1,
        }
    }
}

fn envelope(kind: &str, detail: &str) -> Vec<u8> {
    canonical_bytes(&ToolEnvelope { detail, kind }).expect("tool envelope is finite text")
}

#[derive(Serialize)]
struct ToolEnvelope<'a> {
    detail: &'a str,
    kind: &'a str,
}

#[derive(Serialize)]
struct KnownPrompt<'a> {
    embedded: &'a str,
    evidence: &'a str,
    interjection: &'a str,
    output_byte_limit: u64,
    question: &'a str,
    role: &'a str,
    roles: Vec<&'a str>,
    schema: &'a str,
    tools: &'a str,
    topic: &'a str,
}

#[derive(Serialize)]
struct DeliveryPrompt<'a> {
    binding_id: String,
    effort: &'a str,
    model: &'a str,
    phase_hash: String,
    phase: &'a PhaseSnapshotV1,
    provider_ref: &'a str,
    prompt_version: &'a str,
    role: &'a str,
    schema: &'a str,
    schema_id: &'a str,
    template_version: &'a str,
    tool_version: &'a str,
    tools: &'a str,
}
