//! Shared three-tool dispatch. P11's durable store implements [`ToolStore`]
//! and reuses this path. This module does not own a second validator.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use roundtable_protocol::{
    bound_request, canonical_bytes, submit_candidate, validate_result, AliasVisibility,
    CandidateReceipt, CandidateState, DecisionKind, ErrorCode, Hash256, ObjectRefV1,
    QualifiedContextProfile, RequestTranscriptBound, ResultScope, RtResult, SubmissionDecision,
    SubmissionId, SubmissionState, ToolExchange, ValidatedResult, VisibleAliases,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use super::rt_error;

pub type ObjectRef = ObjectRefV1;

pub const SERVICE_TOOL_NAMES: [&str; 3] = ["read_evidence", "search_evidence", "submit_result"];
pub const SERVICE_TOOL_VERSION: &str = "roundtable_tools_v1";
pub const SERVICE_RESULT_SCHEMA_ID: &str = "roundtable_result_v1";
pub const SERVICE_TOOL_SCHEMA: &str = "read_evidence{file_alias,start_line,end_line};search_evidence{file_alias,query,limit};submit_result{submission_id,result}";
static SERVICE_RESULT_SCHEMA: std::sync::OnceLock<String> = std::sync::OnceLock::new();

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
    "fence",
];

pub fn service_tool_names() -> &'static [&'static str] {
    &SERVICE_TOOL_NAMES
}

pub fn service_tool_schema() -> &'static str {
    SERVICE_TOOL_SCHEMA
}

pub fn service_result_schema() -> &'static str {
    SERVICE_RESULT_SCHEMA.get_or_init(|| roundtable_protocol::result_schema(None).to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenBinding {
    pub attempt_id: roundtable_protocol::AttemptId,
    pub room_id: roundtable_protocol::RoomId,
    pub fence: roundtable_protocol::Fence,
    pub speaker_id: roundtable_protocol::SpeakerId,
    pub tool_version: String,
    pub aliases: VisibleAliases,
    pub result_scope: ResultScope,
    pub evidence: BTreeMap<String, ObjectRef>,
    pub profile: QualifiedContextProfile,
}

pub struct AttemptToken {
    secret: String,
}

impl AttemptToken {
    /// The same-sandbox agent can read this value. It still cannot cross
    /// attempt, room, fence, or tool.
    pub fn reveal_for_same_sandbox(&self) -> &str {
        &self.secret
    }
}

impl std::fmt::Debug for AttemptToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AttemptToken([redacted])")
    }
}

struct AttemptLedger {
    submissions: SubmissionState,
    transcript: RequestTranscriptBound,
}

struct TokenRecord {
    binding: TokenBinding,
    revoked: bool,
    ledger: Arc<Mutex<AttemptLedger>>,
    dispatch_lock: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Default)]
pub struct TokenRegistry {
    records: Mutex<HashMap<String, TokenRecord>>,
}

impl TokenRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn issue(&self, binding: TokenBinding) -> AttemptToken {
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        self.records.lock().expect("tokens").insert(
            secret.clone(),
            TokenRecord {
                binding,
                revoked: false,
                ledger: Arc::new(Mutex::new(AttemptLedger {
                    submissions: SubmissionState::open(),
                    transcript: RequestTranscriptBound::empty(),
                })),
                dispatch_lock: Arc::new(tokio::sync::Mutex::new(())),
            },
        );
        AttemptToken { secret }
    }

    pub fn revoke(&self, secret: &str) -> RtResult<()> {
        let mut records = self.records.lock().expect("tokens");
        let record = records
            .get_mut(secret)
            .ok_or_else(|| rt_error(ErrorCode::Unauthenticated, "token_unknown"))?;
        record.revoked = true;
        Ok(())
    }

    pub fn admit(
        &self,
        secret: &str,
        live: &TokenBinding,
        tool: &str,
    ) -> RtResult<AdmittedToolScope> {
        let records = self.records.lock().expect("tokens");
        let record = records
            .get(secret)
            .ok_or_else(|| rt_error(ErrorCode::Unauthenticated, "token_unknown"))?;
        if record.revoked {
            return Err(rt_error(ErrorCode::Unauthenticated, "token_revoked"));
        }
        if live.attempt_id != record.binding.attempt_id {
            return Err(rt_error(ErrorCode::Forbidden, "attempt_mismatch"));
        }
        if live.room_id != record.binding.room_id {
            return Err(rt_error(ErrorCode::Forbidden, "room_mismatch"));
        }
        if live.fence != record.binding.fence {
            return Err(rt_error(ErrorCode::Forbidden, "fence_mismatch"));
        }
        if live.speaker_id != record.binding.speaker_id {
            return Err(rt_error(ErrorCode::Forbidden, "speaker_mismatch"));
        }
        if live.tool_version != record.binding.tool_version {
            return Err(rt_error(ErrorCode::Forbidden, "tool_version_mismatch"));
        }
        if live.aliases != record.binding.aliases {
            return Err(rt_error(ErrorCode::Forbidden, "alias_mismatch"));
        }
        if live.result_scope != record.binding.result_scope
            || live.evidence != record.binding.evidence
            || live.profile != record.binding.profile
        {
            return Err(rt_error(ErrorCode::Forbidden, "scope_mismatch"));
        }
        if !service_tool_names().contains(&tool) {
            return Err(rt_error(ErrorCode::Forbidden, "tool_not_admitted"));
        }
        Ok(AdmittedToolScope {
            ledger: Arc::clone(&record.ledger),
            dispatch_lock: Arc::clone(&record.dispatch_lock),
            aliases: record.binding.aliases.clone(),
            result_scope: record.binding.result_scope.clone(),
            evidence: record.binding.evidence.clone(),
            profile: record.binding.profile.clone(),
        })
    }
}

pub struct AdmittedToolScope {
    ledger: Arc<Mutex<AttemptLedger>>,
    dispatch_lock: Arc<tokio::sync::Mutex<()>>,
    aliases: VisibleAliases,
    result_scope: ResultScope,
    evidence: BTreeMap<String, ObjectRef>,
    profile: QualifiedContextProfile,
}

impl std::fmt::Debug for AdmittedToolScope {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdmittedToolScope")
            .field("aliases", &self.aliases)
            .field("result_scope", &self.result_scope)
            .finish_non_exhaustive()
    }
}

impl AdmittedToolScope {
    pub fn result_scope(&self) -> &ResultScope {
        &self.result_scope
    }

    pub fn evidence_ref(&self, alias: &str) -> Option<&ObjectRef> {
        self.evidence.get(alias)
    }

    pub fn tool_calls(&self) -> u32 {
        self.ledger.lock().expect("ledger").transcript.tool_calls
    }

    pub fn tool_reply_bytes(&self) -> u64 {
        self.ledger
            .lock()
            .expect("ledger")
            .transcript
            .tool_reply_bytes
    }

    pub fn evidence_attempt_bytes(&self) -> u64 {
        self.ledger
            .lock()
            .expect("ledger")
            .transcript
            .evidence_attempt_bytes
    }

    pub fn sealed_receipt(&self) -> Option<CandidateReceipt> {
        self.ledger
            .lock()
            .expect("ledger")
            .submissions
            .sealed
            .clone()
    }
}

#[derive(Debug, Clone)]
pub struct RoundtableToolCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct RoundtableToolResponse {
    pub tool: String,
    pub body: Vec<u8>,
    pub receipt: Option<CandidateReceipt>,
    pub decision: Option<SubmissionDecision>,
}

#[async_trait]
pub trait ToolStore: Send + Sync {
    async fn get_evidence(&self, object: &ObjectRef) -> RtResult<Vec<u8>>;
    async fn submit(
        &self,
        submission_id: &SubmissionId,
        validated: &ValidatedResult,
    ) -> RtResult<CandidateReceipt>;

    /// Same seal as [`ToolStore::submit`], plus the canonical payload bytes.
    async fn submit_canonical(
        &self,
        submission_id: &SubmissionId,
        validated: &ValidatedResult,
        raw: &[u8],
    ) -> RtResult<CandidateReceipt>;

    /// Record a field-error submission. The shared dispatcher already decided
    /// the outcome. In-memory stores keep that decision in the attempt ledger.
    async fn note_field_errors(
        &self,
        submission_id: &SubmissionId,
        raw: &[u8],
        scope: &ResultScope,
    ) -> RtResult<()>;
}

#[derive(Default)]
pub struct InMemoryToolStore {
    objects: Mutex<BTreeMap<String, Vec<u8>>>,
    sealed: Mutex<BTreeMap<String, CandidateReceipt>>,
}

impl InMemoryToolStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_object(&self, object: &ObjectRef, bytes: &[u8]) -> RtResult<()> {
        if Hash256::sha256(bytes) != object.content_hash {
            return Err(rt_error(ErrorCode::InvalidArgument, "object_hash"));
        }
        if u64::try_from(bytes.len()).ok() != Some(object.total_bytes.0) {
            return Err(rt_error(ErrorCode::InvalidArgument, "object_length"));
        }
        self.objects
            .lock()
            .expect("objects")
            .insert(object_key(object), bytes.to_vec());
        Ok(())
    }
}

#[async_trait]
impl ToolStore for InMemoryToolStore {
    async fn get_evidence(&self, object: &ObjectRef) -> RtResult<Vec<u8>> {
        let objects = self.objects.lock().expect("objects");
        let bytes = objects
            .get(&object_key(object))
            .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "unknown_object"))?;
        if Hash256::sha256(bytes) != object.content_hash
            || u64::try_from(bytes.len()).ok() != Some(object.total_bytes.0)
        {
            return Err(rt_error(ErrorCode::InvalidArgument, "object_hash"));
        }
        Ok(bytes.clone())
    }

    async fn submit(
        &self,
        submission_id: &SubmissionId,
        validated: &ValidatedResult,
    ) -> RtResult<CandidateReceipt> {
        let receipt = CandidateReceipt {
            submission_id: submission_id.clone(),
            payload_hash: validated.canonical_hash,
            candidate_id: validated.canonical_hash.to_hex(),
            state: CandidateState::Staged,
        };
        let mut sealed = self.sealed.lock().expect("sealed");
        let key = submission_id.to_string();
        if let Some(existing) = sealed.get(&key) {
            if existing.payload_hash == receipt.payload_hash {
                return Ok(existing.clone());
            }
            return Err(rt_error(
                ErrorCode::IdempotencyConflict,
                "submission_conflict",
            ));
        }
        sealed.insert(key, receipt.clone());
        Ok(receipt)
    }

    async fn submit_canonical(
        &self,
        submission_id: &SubmissionId,
        validated: &ValidatedResult,
        raw: &[u8],
    ) -> RtResult<CandidateReceipt> {
        let _ = raw;
        self.submit(submission_id, validated).await
    }

    async fn note_field_errors(
        &self,
        submission_id: &SubmissionId,
        raw: &[u8],
        scope: &ResultScope,
    ) -> RtResult<()> {
        let _ = (submission_id, raw, scope);
        Ok(())
    }
}

pub async fn dispatch_tool(
    scope: &AdmittedToolScope,
    call: RoundtableToolCall,
    store: &dyn ToolStore,
) -> RtResult<RoundtableToolResponse> {
    let _dispatch = scope.dispatch_lock.lock().await;
    let arguments = canonical_bytes(&call.arguments)?;
    // Reserve each admitted call before validation or I/O. Errors consume the
    // same call/argument allowance as successful requests.
    {
        let mut ledger = scope.ledger.lock().expect("ledger");
        let mut projected = ledger.transcript.clone();
        projected
            .record_exchange(
                &ToolExchange {
                    arguments,
                    reply: Vec::new(),
                    generated_utf8_bytes: 0,
                    evidence_bytes: 0,
                    model_requests: 0,
                    tool_calls: 1,
                },
                &scope.profile,
            )
            .map_err(|err| rt_error(err.code, "tool_budget"))?;
        bound_request(&projected, &scope.profile)
            .map_err(|err| rt_error(err.code, "tool_budget"))?;
        ledger.transcript = projected;
    }
    let result = dispatch_admitted(scope, call, store).await;
    if let Err(error) = &result {
        let body = canonical_bytes(error)?;
        charge_exchange(scope, &[], &body, false)?;
    }
    result
}

async fn dispatch_admitted(
    scope: &AdmittedToolScope,
    call: RoundtableToolCall,
    store: &dyn ToolStore,
) -> RtResult<RoundtableToolResponse> {
    let tool_name = call.name.as_str();
    if !service_tool_names().contains(&tool_name) {
        return Err(rt_error(ErrorCode::Forbidden, "tool_not_admitted"));
    }
    let arguments = call
        .arguments
        .as_object()
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "invalid_argument"))?;
    if arguments
        .keys()
        .any(|key| IDENTITY_KEYS.contains(&key.as_str()))
    {
        return Err(rt_error(ErrorCode::Forbidden, "identity_not_selectable"));
    }
    match call.name.as_str() {
        "read_evidence" => read_evidence(scope, &call.arguments, store).await,
        "search_evidence" => search_evidence(scope, &call.arguments, store).await,
        "submit_result" => submit_result(scope, &call.arguments, store).await,
        _ => Err(rt_error(ErrorCode::Forbidden, "tool_not_admitted")),
    }
}

async fn read_evidence(
    scope: &AdmittedToolScope,
    arguments: &Value,
    store: &dyn ToolStore,
) -> RtResult<RoundtableToolResponse> {
    if scope.aliases.evidence.is_empty() {
        return empty_corpus_response("read_evidence", scope, arguments);
    }
    require_keys(arguments, &["file_alias", "start_line", "end_line"])?;
    let alias = required_str(arguments, "file_alias")?;
    let start = required_u64(arguments, "start_line")?;
    let end = required_u64(arguments, "end_line")?;
    let bytes = load_alias(scope, alias, store).await?;
    let text = String::from_utf8(bytes)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "evidence_encoding"))?;
    let (excerpt, total_lines, has_more) = slice_lines(&text, start, end)?;
    let body = canonical_bytes(&ReadBody {
        excerpt: &excerpt,
        file_alias: alias,
        has_more,
        total_lines,
    })?;
    let argument_bytes = canonical_bytes(arguments)?;
    charge_exchange(scope, &argument_bytes, &body, true)?;
    Ok(RoundtableToolResponse {
        tool: "read_evidence".to_string(),
        body,
        receipt: None,
        decision: None,
    })
}

async fn search_evidence(
    scope: &AdmittedToolScope,
    arguments: &Value,
    store: &dyn ToolStore,
) -> RtResult<RoundtableToolResponse> {
    if scope.aliases.evidence.is_empty() {
        return empty_corpus_response("search_evidence", scope, arguments);
    }
    require_keys(arguments, &["file_alias", "query", "limit"])?;
    let alias = required_str(arguments, "file_alias")?;
    let query = required_str(arguments, "query")?;
    if query.is_empty() || query.len() > 256 {
        return Err(rt_error(ErrorCode::InvalidArgument, "query_bounds"));
    }
    let limit = required_u64(arguments, "limit")?;
    if !(1..=20).contains(&limit) {
        return Err(rt_error(ErrorCode::InvalidArgument, "query_bounds"));
    }
    let bytes = load_alias(scope, alias, store).await?;
    let text = String::from_utf8(bytes)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "evidence_encoding"))?;
    let mut hits = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if line.contains(query) {
            hits.push(SearchHit {
                line: index as u64 + 1,
                text: line.to_string(),
            });
            if hits.len() == limit as usize {
                break;
            }
        }
    }
    let body = canonical_bytes(&SearchBody {
        file_alias: alias,
        hits: &hits,
        literal: true,
    })?;
    let argument_bytes = canonical_bytes(arguments)?;
    charge_exchange(scope, &argument_bytes, &body, true)?;
    Ok(RoundtableToolResponse {
        tool: "search_evidence".to_string(),
        body,
        receipt: None,
        decision: None,
    })
}

async fn submit_result(
    scope: &AdmittedToolScope,
    arguments: &Value,
    store: &dyn ToolStore,
) -> RtResult<RoundtableToolResponse> {
    require_keys(arguments, &["submission_id", "result"])?;
    let submission_text = required_str(arguments, "submission_id")?;
    let submission_id = SubmissionId::from_str(submission_text)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "invalid_submission_id"))?;
    let result = arguments
        .get("result")
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "invalid_argument"))?;
    let raw = canonical_bytes(result)?;
    let decision = {
        let ledger = scope.ledger.lock().expect("ledger");
        submit_candidate(
            &ledger.submissions,
            &submission_id,
            &raw,
            &scope.result_scope,
        )
    };
    let (receipt, kind) = match &decision.outcome {
        DecisionKind::Staged(receipt) => (Some(receipt.clone()), "staged"),
        DecisionKind::FieldErrors(_) => (None, "field_errors"),
        DecisionKind::SubmissionConflict => (None, "submission_conflict"),
        DecisionKind::ResultAlreadySealed => (None, "result_already_sealed"),
        DecisionKind::AttemptClosed => (None, "attempt_closed"),
    };
    let body = canonical_bytes(&SubmitBody {
        candidate_id: receipt.as_ref().map(|item| item.candidate_id.as_str()),
        kind,
        submission_id: submission_text,
        field_errors: match &decision.outcome {
            DecisionKind::FieldErrors(errors) => errors
                .iter()
                .map(|error| serde_json::json!({"path": error.path, "reason": error.code.as_str()}))
                .collect(),
            _ => Vec::new(),
        },
    })?;
    let argument_bytes = canonical_bytes(arguments)?;
    // A refused reply must never leave a sealed candidate in the durable store.
    charge_exchange(scope, &argument_bytes, &body, false)?;
    let prior_sealed = scope.sealed_receipt();
    if prior_sealed.is_none() {
        if let Some(receipt) = &receipt {
            let validated = validate_result(&raw, &scope.result_scope)
                .map_err(|_| rt_error(ErrorCode::InvalidState, "validator_disagreement"))?;
            let stored = store
                .submit_canonical(&submission_id, &validated, &raw)
                .await?;
            if &stored != receipt {
                return Err(rt_error(ErrorCode::InvalidState, "receipt_mismatch"));
            }
        } else if matches!(decision.outcome, DecisionKind::FieldErrors(_)) {
            store
                .note_field_errors(&submission_id, &raw, &scope.result_scope)
                .await?;
        }
    }
    {
        let mut ledger = scope.ledger.lock().expect("ledger");
        ledger.submissions = decision.next_state.clone();
    }
    Ok(RoundtableToolResponse {
        tool: "submit_result".to_string(),
        body,
        receipt,
        decision: Some(decision),
    })
}

const EMPTY_CORPUS_HINT: &str = "no frozen evidence; submit without citations";

/// A frozen corpus with zero evidence aliases has nothing to search. Return
/// one structured empty page instead of `unknown_alias` for every guessed name.
fn empty_corpus_response(
    tool: &str,
    scope: &AdmittedToolScope,
    arguments: &Value,
) -> RtResult<RoundtableToolResponse> {
    let body = canonical_bytes(&EmptyCorpusBody {
        aliases: &[],
        empty: true,
        hint: EMPTY_CORPUS_HINT,
    })?;
    let argument_bytes = canonical_bytes(arguments)?;
    charge_exchange(scope, &argument_bytes, &body, false)?;
    Ok(RoundtableToolResponse {
        tool: tool.to_string(),
        body,
        receipt: None,
        decision: None,
    })
}

fn charge_exchange(
    scope: &AdmittedToolScope,
    arguments: &[u8],
    reply: &[u8],
    evidence: bool,
) -> RtResult<()> {
    let mut ledger = scope.ledger.lock().expect("ledger");
    charge_ledger(&mut ledger, arguments, reply, evidence, &scope.profile)
}

fn charge_ledger(
    ledger: &mut AttemptLedger,
    arguments: &[u8],
    reply: &[u8],
    evidence: bool,
    profile: &QualifiedContextProfile,
) -> RtResult<()> {
    let admitted = arguments
        .len()
        .checked_add(reply.len())
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "overflow"))?;
    let evidence_bytes = if evidence {
        u64::try_from(admitted).map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?
    } else {
        0
    };
    let exchange = ToolExchange {
        // Arguments and the call itself were reserved by dispatch_tool.
        arguments: Vec::new(),
        reply: reply.to_vec(),
        generated_utf8_bytes: 0,
        evidence_bytes,
        model_requests: 0,
        tool_calls: 0,
    };
    let mut projected = ledger.transcript.clone();
    projected
        .record_exchange(&exchange, profile)
        .map_err(|err| rt_error(err.code, "tool_budget"))?;
    bound_request(&projected, profile).map_err(|err| rt_error(err.code, "tool_budget"))?;
    ledger.transcript = projected;
    Ok(())
}

async fn load_alias(
    scope: &AdmittedToolScope,
    alias: &str,
    store: &dyn ToolStore,
) -> RtResult<Vec<u8>> {
    let evidence = scope
        .aliases
        .evidence
        .get(alias)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "unknown_alias"))?;
    if evidence.visibility == AliasVisibility::PeerStaged {
        return Err(rt_error(ErrorCode::Forbidden, "peer_staged_evidence"));
    }
    let object = scope
        .evidence
        .get(alias)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "unknown_object"))?;
    store.get_evidence(object).await
}

fn slice_lines(text: &str, start: u64, end: u64) -> RtResult<(String, u64, bool)> {
    if start == 0 || end < start {
        return Err(rt_error(ErrorCode::InvalidArgument, "line_range"));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let total = u64::try_from(lines.len()).unwrap_or(u64::MAX);
    if start > total {
        return Err(rt_error(ErrorCode::InvalidArgument, "line_range"));
    }
    let end = end.min(total);
    let excerpt = lines[(start as usize - 1)..(end as usize)].join("\n");
    if excerpt.len() > 8_192 {
        return Err(rt_error(ErrorCode::ContextTooLarge, "evidence_reply_limit"));
    }
    Ok((excerpt, total, end < total))
}

fn require_keys(arguments: &Value, allowed: &[&str]) -> RtResult<()> {
    let object = arguments
        .as_object()
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "invalid_argument"))?;
    if object
        .keys()
        .any(|key| !allowed.contains(&key.as_str()) && !IDENTITY_KEYS.contains(&key.as_str()))
    {
        return Err(rt_error(ErrorCode::InvalidArgument, "unknown_field"));
    }
    Ok(())
}

fn required_str<'a>(arguments: &'a Value, key: &str) -> RtResult<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "invalid_argument"))
}

fn required_u64(arguments: &Value, key: &str) -> RtResult<u64> {
    arguments
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "invalid_argument"))
}

fn object_key(object: &ObjectRef) -> String {
    format!("{}:{}", object.object_id, object.content_hash.to_hex())
}

#[derive(Serialize)]
struct EmptyCorpusBody<'a> {
    aliases: &'a [&'a str],
    empty: bool,
    hint: &'a str,
}

#[derive(Serialize)]
struct ReadBody<'a> {
    excerpt: &'a str,
    file_alias: &'a str,
    has_more: bool,
    total_lines: u64,
}

#[derive(Serialize)]
struct SearchHit {
    line: u64,
    text: String,
}

#[derive(Serialize)]
struct SearchBody<'a> {
    file_alias: &'a str,
    hits: &'a [SearchHit],
    literal: bool,
}

#[derive(Serialize)]
struct SubmitBody<'a> {
    candidate_id: Option<&'a str>,
    kind: &'a str,
    submission_id: &'a str,
    field_errors: Vec<Value>,
}
