//! Per-attempt request ledger. Counts and bytes go through [`bound_request`].
//! A failed or duplicate exchange still consumes its budget. Idempotent
//! candidate reuse does not waive the transmission. The generation reserve is
//! checked against model capacity before a request is committed.

use std::collections::BTreeMap;

use roundtable_protocol::{
    bound_request, QualifiedContextProfile, RequestTranscriptBound, RtResult, ToolExchange,
};

use super::rt_error;
use roundtable_protocol::ErrorCode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedModelRequest {
    pub model: String,
    pub method: String,
    pub path: String,
    pub prior_output: Vec<u8>,
    pub adapter_bytes: u64,
    pub history_ref: Option<String>,
    pub target_url: Option<String>,
    pub redirect_to: Option<String>,
    pub declared_tools: Vec<String>,
    pub tool_exchanges: Vec<ToolExchange>,
    pub caller_authorization: Option<String>,
    pub submission_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestPermit {
    body_bytes: u64,
    model_requests: u32,
    tool_calls: u32,
    prior_output_bytes: u64,
    tool_reply_bytes: u64,
    generated_utf8_bytes: u64,
    candidate_id: Option<String>,
}

impl RequestPermit {
    pub fn body_bytes(&self) -> u64 {
        self.body_bytes
    }

    pub fn model_requests(&self) -> u32 {
        self.model_requests
    }

    pub fn tool_calls(&self) -> u32 {
        self.tool_calls
    }

    pub fn prior_output_bytes(&self) -> u64 {
        self.prior_output_bytes
    }

    pub fn tool_reply_bytes(&self) -> u64 {
        self.tool_reply_bytes
    }

    pub fn generated_utf8_bytes(&self) -> u64 {
        self.generated_utf8_bytes
    }

    pub(crate) fn candidate_id(&self) -> Option<&str> {
        self.candidate_id.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountingSnapshot {
    pub model_requests: u32,
    pub tool_calls: u32,
    pub tool_reply_bytes: u64,
    pub generated_utf8_bytes: u64,
    pub prior_output_bytes: u64,
    pub evidence_attempt_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct RequestAccounting {
    transcript: RequestTranscriptBound,
    required_prior: Vec<u8>,
    candidates: BTreeMap<String, String>,
    next_candidate: u64,
}

impl RequestAccounting {
    pub(crate) fn new() -> Self {
        Self {
            transcript: RequestTranscriptBound::empty(),
            required_prior: Vec::new(),
            candidates: BTreeMap::new(),
            next_candidate: 0,
        }
    }

    pub fn authorize(
        &mut self,
        request: &EncodedModelRequest,
        profile: &QualifiedContextProfile,
    ) -> RtResult<RequestPermit> {
        enforce_profile_caps(profile)?;
        if profile.model_capacity_tokens == 0 {
            return Err(rt_error(ErrorCode::CapacityUnknown, "capacity_unknown"));
        }
        if request.prior_output != self.required_prior {
            return Err(rt_error(ErrorCode::InvalidArgument, "prior_output_missing"));
        }
        let mut projected = self.transcript.clone();
        for exchange in &request.tool_exchanges {
            let mut exchange = exchange.clone();
            // The HTTP request is one model request. Each envelope still counts
            // as a tool call even when the caller declares zero.
            exchange.model_requests = 0;
            exchange.tool_calls = exchange.tool_calls.max(1);
            projected.record_exchange(&exchange, profile)?;
        }
        projected.model_requests = projected
            .model_requests
            .checked_add(1)
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        projected.adapter_bytes = request.adapter_bytes;
        projected.prior_output_bytes = u64::try_from(request.prior_output.len())
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
        let body_bytes = bound_request(&projected, profile)?;
        admit_generation_reserve(body_bytes, profile)?;
        let candidate_id = assign_candidate(
            &mut self.candidates,
            &mut self.next_candidate,
            request.submission_id.as_deref(),
        );
        self.transcript = projected;
        Ok(RequestPermit {
            body_bytes,
            model_requests: self.transcript.model_requests,
            tool_calls: self.transcript.tool_calls,
            prior_output_bytes: self.transcript.prior_output_bytes,
            tool_reply_bytes: self.transcript.tool_reply_bytes,
            generated_utf8_bytes: self.transcript.generated_utf8_bytes,
            candidate_id,
        })
    }

    pub(crate) fn note_generated(
        &mut self,
        utf8: &[u8],
        profile: &QualifiedContextProfile,
    ) -> RtResult<()> {
        self.consume_generated(utf8.len() as u64, profile)?;
        self.commit_generated_prior(utf8);
        Ok(())
    }

    /// Consumption is irreversible even when the response later fails parsing.
    pub(crate) fn consume_generated(
        &mut self,
        added: u64,
        profile: &QualifiedContextProfile,
    ) -> RtResult<()> {
        self.transcript.generated_utf8_bytes =
            self.transcript.generated_utf8_bytes.saturating_add(added);
        if self.transcript.generated_utf8_bytes > profile.max_attempt_generated_utf8_bytes {
            return Err(rt_error(ErrorCode::ContextTooLarge, "generated_utf8_limit"));
        }
        Ok(())
    }
    pub(crate) fn commit_generated_prior(&mut self, utf8: &[u8]) {
        self.required_prior.extend_from_slice(utf8);
    }

    pub(crate) fn snapshot(&self) -> AccountingSnapshot {
        AccountingSnapshot {
            model_requests: self.transcript.model_requests,
            tool_calls: self.transcript.tool_calls,
            tool_reply_bytes: self.transcript.tool_reply_bytes,
            generated_utf8_bytes: self.transcript.generated_utf8_bytes,
            prior_output_bytes: self.transcript.prior_output_bytes,
            evidence_attempt_bytes: self.transcript.evidence_attempt_bytes,
        }
    }
}

impl Default for RequestAccounting {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn enforce_profile_caps(profile: &QualifiedContextProfile) -> RtResult<()> {
    if profile.max_model_requests > QualifiedContextProfile::MAX_MODEL_REQUESTS
        || profile.max_tool_calls > QualifiedContextProfile::MAX_TOOL_CALLS
        || profile.max_tool_reply_bytes > QualifiedContextProfile::MAX_TOOL_REPLY_BYTES
        || profile.max_attempt_generated_utf8_bytes
            > QualifiedContextProfile::MAX_ATTEMPT_GENERATED_UTF8_BYTES
        || profile.max_request_body_bytes > QualifiedContextProfile::MAX_REQUEST_BODY_BYTES
    {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "profile_over_cap",
        ));
    }
    if profile.generation_reserve_tokens < QualifiedContextProfile::GENERATION_RESERVE_TOKENS {
        return Err(rt_error(
            ErrorCode::CapabilityUnqualified,
            "reserve_too_small",
        ));
    }
    Ok(())
}

/// `adapter_hidden + verified input bytes + generation reserve` must fit.
/// A missing capacity is not treated as zero room.
fn admit_generation_reserve(body_bytes: u64, profile: &QualifiedContextProfile) -> RtResult<()> {
    if profile.model_capacity_tokens == 0 {
        return Err(rt_error(ErrorCode::CapacityUnknown, "capacity_unknown"));
    }
    let admission = profile
        .adapter_hidden_bound_tokens
        .checked_add(body_bytes)
        .and_then(|value| value.checked_add(profile.generation_reserve_tokens))
        .ok_or_else(|| rt_error(ErrorCode::CapacityUnknown, "capacity_unknown"))?;
    if admission > profile.model_capacity_tokens {
        return Err(rt_error(ErrorCode::ContextTooLarge, "context_too_large"));
    }
    Ok(())
}

fn assign_candidate(
    candidates: &mut BTreeMap<String, String>,
    next_candidate: &mut u64,
    submission_id: Option<&str>,
) -> Option<String> {
    let id = submission_id?;
    if let Some(existing) = candidates.get(id) {
        return Some(existing.clone());
    }
    *next_candidate = next_candidate.saturating_add(1);
    let assigned = format!("cand-{next_candidate}");
    candidates.insert(id.to_string(), assigned.clone());
    Some(assigned)
}
