//! Durable evidence reads and candidate seals.
//!
//! Parse, alias checks, schema checks, reply encoding, and reply quotas stay
//! in [`super::tool_core`] and `roundtable_protocol`. This module admits a
//! token, then calls [`dispatch_tool`]. A staged receipt is not an accepted
//! turn. Nothing here enables the product execution gate or changes SQLite
//! `synchronous`.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use roundtable_protocol::validation::FieldError as SchemaFieldError;
use roundtable_protocol::{
    canonical_bytes, charge_interjection, parse_strict_json, profile_suggestions, submit_candidate,
    v1_1, validate_result, AttemptId, CandidateReceipt, DecisionKind, ErrorCode, ErrorDetails,
    Fence, FieldError as WireFieldError, FinishKind, HandlerId, Hash256, InternalReason,
    LimitsOrigin, MonoMs, ParseLimits, PhaseKind, ResultScope, RoomId, RtError, RtResult,
    SubmissionId, SubmissionState,
};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseTransaction, DbErr, QueryResult, Statement,
    TransactionTrait, TryGetable, Value as DbValue,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::feature_gate::{AdmissionFacts, ExecutionGate, ExecutionScope};
use super::objects::{self, ObjectStore};
use super::rt_error;
use super::store::RoundtableStore;
use super::tool_core::{
    dispatch_tool, service_tool_names, AdmittedToolScope, AttemptToken, RoundtableToolCall,
    RoundtableToolResponse, TokenBinding, TokenRegistry, ToolStore,
};

/// Host-owned attempt the durable store writes under.
#[derive(Debug, Clone)]
pub struct ToolSession {
    pub room_id: String,
    pub attempt_id: String,
    pub speaker_id: String,
    pub phase_id: String,
    pub phase_revision: i64,
    pub manifest_id: String,
    pub workspace_snapshot_id: String,
    pub manifest_hash: String,
}

#[derive(Debug, Clone)]
pub struct ReadEvidenceArgs {
    pub file_alias: String,
    pub start_line: u64,
    pub end_line: u64,
}

#[derive(Debug, Clone)]
pub struct SearchEvidenceArgs {
    pub file_alias: String,
    pub query: String,
    pub limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSlice {
    pub excerpt: String,
    pub file_alias: String,
    pub total_lines: u64,
    pub has_more: bool,
    pub encoded_bytes: u64,
    pub byte_start: u64,
    pub byte_end: u64,
    pub evidence_alias: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSearchHit {
    pub line: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSearchPage {
    pub file_alias: String,
    pub hits: Vec<EvidenceSearchHit>,
    pub literal: bool,
    pub encoded_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceUsage {
    pub returned_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputEvidence {
    pub path: String,
    pub content_hash: String,
    pub published_seq: i64,
    pub staged_phase_id: String,
    pub phase_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedEvidence {
    pub evidence_id: String,
    pub file_alias: String,
    pub owner_speaker_id: String,
    pub owner_attempt_id: Option<String>,
    pub staged_phase_id: String,
    pub phase_revision: i64,
    pub published_seq: Option<i64>,
    pub content_hash: String,
    pub attribution: String,
    pub verified: bool,
    pub origin: String,
    pub path: String,
    pub excerpt: String,
    pub line_start: Option<u64>,
    pub line_end: Option<u64>,
    pub byte_start: Option<u64>,
    pub byte_end: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionAudit {
    pub invalid_count: u32,
    pub closed: bool,
    pub sealed_count: u64,
    pub accepted_count: u64,
    pub attempt_state: String,
}

#[async_trait]
pub trait ToolAuthority: Send + Sync {
    async fn admit_tool(&self, token: &AttemptToken) -> RtResult<AdmittedToolScope>;
}

#[derive(Clone)]
struct IssuedToken {
    binding: TokenBinding,
    expires_at: MonoMs,
}

struct Linear {
    gate: roundtable_protocol::CompletionGate,
    state: roundtable_protocol::CompletionState,
    wait: Option<roundtable_protocol::CompletionWait>,
    next_handler: u64,
    open_handlers: Vec<HandlerId>,
    returned_bytes: u64,
}

/// Test authority. It checks the P05 gate, then admits the token.
/// P13's actor replaces this type; the trait stays.
pub struct GateToolAuthority {
    registry: TokenRegistry,
    data_dir: std::path::PathBuf,
    execution: ExecutionScope,
    facts: AdmissionFacts,
    pinned: TokenBinding,
    expires_at: MonoMs,
    execution_lease: Option<Arc<super::resources::ExecutionLease>>,
    now: Mutex<MonoMs>,
    clock: Option<Arc<dyn super::clock::MonoClock>>,
    control: Mutex<TokenBinding>,
    issued: Mutex<HashMap<String, IssuedToken>>,
    interjection_used: Mutex<u64>,
    linear: Mutex<Linear>,
    attempt_io: tokio::sync::Mutex<()>,
    /// Diagnostics-only per-attempt trace (observability; no behaviour).
    trace: std::sync::OnceLock<Arc<super::attempt_trace::AttemptTrace>>,
}

impl GateToolAuthority {
    pub fn open(
        data_dir: &std::path::Path,
        execution: ExecutionScope,
        facts: AdmissionFacts,
        now: MonoMs,
        expires_at: MonoMs,
        pinned: TokenBinding,
    ) -> Self {
        let state = roundtable_protocol::CompletionState::open(pinned.fence.clone());
        Self {
            registry: TokenRegistry::new(),
            data_dir: data_dir.to_path_buf(),
            execution,
            facts,
            pinned: pinned.clone(),
            expires_at,
            execution_lease: None,
            now: Mutex::new(now),
            clock: None,
            control: Mutex::new(pinned),
            issued: Mutex::new(HashMap::new()),
            interjection_used: Mutex::new(0),
            linear: Mutex::new(Linear {
                gate: roundtable_protocol::CompletionGate::new(),
                state,
                wait: None,
                next_handler: 0,
                open_handlers: Vec::new(),
                returned_bytes: 0,
            }),
            attempt_io: tokio::sync::Mutex::new(()),
            trace: std::sync::OnceLock::new(),
        }
    }

    /// Attaches the diagnostics-only attempt trace. Observability only.
    pub(crate) fn set_trace(&self, trace: Arc<super::attempt_trace::AttemptTrace>) {
        let _ = self.trace.set(trace);
    }

    /// Records a tool-broker event on the attempt trace, if any.
    pub(crate) fn trace_tool_event(&self, value: serde_json::Value) {
        if let Some(trace) = self.trace.get() {
            trace.record("tools", value);
        }
    }

    pub fn gate_enabled(&self) -> bool {
        ExecutionGate::open(&self.data_dir).enabled()
    }

    /// Phase pinned when this authority was opened. `tools/list` publishes
    /// that phase's result schema.
    pub fn pinned_phase_kind(&self) -> PhaseKind {
        self.pinned.result_scope.phase_kind
    }

    /// Production authorities sample the host clock on every admission.
    pub fn with_clock(mut self, clock: Arc<dyn super::clock::MonoClock>) -> Self {
        self.clock = Some(clock);
        self
    }

    pub fn with_execution_lease(
        mut self,
        lease: Option<Arc<super::resources::ExecutionLease>>,
    ) -> Self {
        self.execution_lease = lease;
        self
    }

    pub fn gate_held(&self) -> bool {
        lock(&self.linear).gate.held()
    }

    pub fn issue(&self) -> AttemptToken {
        self.issue_until(self.expires_at)
    }

    pub fn issue_until(&self, expires_at: MonoMs) -> AttemptToken {
        let binding = self.pinned.clone();
        let token = self.registry.issue(binding.clone());
        lock(&self.issued).insert(
            token.reveal_for_same_sandbox().to_string(),
            IssuedToken {
                binding,
                expires_at,
            },
        );
        token
    }

    pub fn binding_of(&self, token: &AttemptToken) -> RtResult<TokenBinding> {
        lock(&self.issued)
            .get(token.reveal_for_same_sandbox())
            .map(|issued| issued.binding.clone())
            .ok_or_else(|| rt_error(ErrorCode::Unauthenticated, "token_unknown"))
    }

    pub fn revoke(&self, token: &AttemptToken) -> RtResult<()> {
        self.registry.revoke(token.reveal_for_same_sandbox())
    }

    pub fn advance_to(&self, now: MonoMs) {
        *lock(&self.now) = now;
    }

    pub fn set_live_attempt(&self, attempt: AttemptId) {
        lock(&self.control).attempt_id = attempt;
    }

    pub fn set_live_room(&self, room: RoomId) {
        lock(&self.control).room_id = room;
    }

    pub fn set_control_fence(&self, fence: Fence) {
        lock(&self.control).fence = fence;
    }

    pub fn set_tool_version(&self, version: impl Into<String>) {
        lock(&self.control).tool_version = version.into();
    }

    pub fn reset_control(&self) {
        *lock(&self.control) = self.pinned.clone();
    }

    /// Creation-time interjection reserve. Overspend is rejected.
    pub fn note_interjection(&self, incoming: u64) -> RtResult<u64> {
        let mut used = lock(&self.interjection_used);
        let remaining =
            charge_interjection(u64::from(v1_1::DEFAULT_INTERJECTION_BYTES), *used, incoming)?;
        *used = used.saturating_add(incoming);
        Ok(remaining)
    }

    pub fn usage(&self) -> EvidenceUsage {
        EvidenceUsage {
            returned_bytes: lock(&self.linear).returned_bytes,
        }
    }

    pub fn pending_handlers(&self) -> usize {
        let linear = lock(&self.linear);
        if let Some(wait) = &linear.wait {
            wait.barrier().pending_tools.len()
        } else {
            linear.open_handlers.len()
        }
    }

    /// Close new admission and capture the handlers already admitted.
    /// The completion gate is released before this returns. Waiting is separate.
    pub fn stop(&self) -> roundtable_protocol::CompletionBarrier {
        let mut linear = lock(&self.linear);
        let wait = {
            let Linear { gate, state, .. } = &mut *linear;
            roundtable_protocol::CompletionWait::start(gate, state, 0)
        };
        let barrier = wait.barrier().clone();
        linear.wait = Some(wait);
        barrier
    }

    pub fn complete_inflight(&self) {
        let mut linear = lock(&self.linear);
        let pending = linear.open_handlers.clone();
        for handler in &pending {
            linear.state.finish_handler(handler);
        }
        linear.open_handlers.clear();
        let state = linear.state.clone();
        if let Some(wait) = linear.wait.as_mut() {
            wait.poll(&state);
        }
    }

    fn begin_admission(&self, token: &AttemptToken) -> RtResult<(AdmittedToolScope, HandlerId)> {
        let now = self
            .clock
            .as_ref()
            .map_or_else(|| *lock(&self.now), |clock| MonoMs(clock.now_ms()));
        let secret = token.reveal_for_same_sandbox();
        let issued = lock(&self.issued)
            .get(secret)
            .cloned()
            .ok_or_else(|| rt_error(ErrorCode::Unauthenticated, "token_unknown"))?;
        if self
            .execution_lease
            .as_ref()
            .is_some_and(|lease| lease.admit_tool(now.0) == 0)
        {
            return Err(rt_error(
                ErrorCode::Unauthenticated,
                "prepaid_lease_expired",
            ));
        }
        if now.0 >= issued.expires_at.0 {
            return Err(rt_error(ErrorCode::Unauthenticated, "token_expired"));
        }
        let control = lock(&self.control).clone();
        if issued.binding.attempt_id != control.attempt_id {
            return Err(rt_error(ErrorCode::Forbidden, "attempt_mismatch"));
        }
        if issued.binding.room_id != control.room_id {
            return Err(rt_error(ErrorCode::Forbidden, "room_mismatch"));
        }
        if issued.binding.fence != control.fence {
            return Err(rt_error(ErrorCode::Forbidden, "fence_mismatch"));
        }
        if issued.binding.speaker_id != control.speaker_id {
            return Err(rt_error(ErrorCode::Forbidden, "speaker_mismatch"));
        }
        if issued.binding.tool_version != control.tool_version {
            return Err(rt_error(ErrorCode::Forbidden, "tool_version_mismatch"));
        }
        if issued.binding.aliases != control.aliases {
            return Err(rt_error(ErrorCode::Forbidden, "alias_mismatch"));
        }
        // The permit is dropped before the caller performs evidence I/O.
        let permit =
            ExecutionGate::open(&self.data_dir).check(&self.execution, &self.facts, now)?;
        let scope = self
            .registry
            .admit(secret, &issued.binding, "read_evidence")?;
        drop(permit);
        let mut linear = lock(&self.linear);
        let handler = HandlerId::new(format!("mcp-{}", linear.next_handler));
        linear.next_handler = linear.next_handler.saturating_add(1);
        linear.state.admit_handler(handler.clone())?;
        linear.open_handlers.push(handler.clone());
        Ok((scope, handler))
    }

    fn finish_call(&self, handler: &HandlerId, evidence_bytes: Option<u64>) {
        let mut linear = lock(&self.linear);
        linear.state.finish_handler(handler);
        linear.open_handlers.retain(|open| open != handler);
        if let Some(bytes) = evidence_bytes {
            linear.returned_bytes = linear.returned_bytes.max(bytes);
        }
        let state = linear.state.clone();
        if let Some(wait) = linear.wait.as_mut() {
            wait.poll(&state);
        }
    }
}

#[async_trait]
impl ToolAuthority for GateToolAuthority {
    async fn admit_tool(&self, token: &AttemptToken) -> RtResult<AdmittedToolScope> {
        let _guard = self.attempt_io.lock().await;
        self.begin_admission(token).map(|(scope, _handler)| scope)
    }
}

/// Admit under the authority, then call the shared dispatcher.
/// Unknown tools are rejected before a handler is registered.
pub async fn invoke_scoped_tool(
    token: &AttemptToken,
    call: RoundtableToolCall,
    authority: &GateToolAuthority,
    store: &dyn ToolStore,
) -> RtResult<RoundtableToolResponse> {
    let traced = authority.trace.get().cloned();
    let call_started = std::time::Instant::now();
    if let Some(trace) = &traced {
        trace.record(
            "tools",
            serde_json::json!({
                "event": "tool_call_received",
                "tool": call.name,
                "arguments_bytes": serde_json::to_string(&call.arguments).map(|text| text.len()).unwrap_or(0),
                "submission_id": call.arguments.get("submission_id"),
            }),
        );
    }
    let _guard = authority.attempt_io.lock().await;
    if let Some(trace) = &traced {
        let waited = call_started.elapsed().as_millis();
        if waited > 50 {
            trace.record(
                "tools",
                serde_json::json!({"event":"tool_call_lock_wait","tool":call.name,"waited_ms":waited}),
            );
        }
    }
    if !service_tool_names().contains(&call.name.as_str()) {
        if let Some(trace) = &traced {
            trace.record(
                "tools",
                serde_json::json!({"event":"tool_call_result","tool":call.name,"ok":false,"error":"tool_not_admitted"}),
            );
        }
        return Err(rt_error(ErrorCode::Forbidden, "tool_not_admitted"));
    }
    let (scope, handler) = authority.begin_admission(token)?;
    let guard = AdmittedHandler {
        authority,
        handler,
        evidence_bytes: None,
    };
    let result = dispatch_tool(&scope, call, store).await;
    let charged = match &result {
        Ok(response) if response.tool == "read_evidence" || response.tool == "search_evidence" => {
            Some(scope.evidence_attempt_bytes())
        }
        _ => None,
    };
    let mut guard = guard;
    guard.evidence_bytes = charged;
    drop(guard);
    if let Some(trace) = &traced {
        trace.record("tools", tool_result_record(&result, call_started));
        if let Ok(response) = &result {
            let staged = response.receipt.is_some()
                && serde_json::from_slice::<serde_json::Value>(&response.body)
                    .ok()
                    .and_then(|body| {
                        body.get("kind")
                            .and_then(|kind| kind.as_str())
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some("staged");
            if response.tool == "submit_result" && staged {
                // Seal timestamp side log: rt_submissions has no timestamp.
                trace.record(
                    "tools",
                    serde_json::json!({
                        "event": "submission_sealed",
                        "candidate_id": response.receipt.as_ref().map(|receipt| receipt.candidate_id.as_str()),
                        "seal_wall": super::attempt_trace::wall_now(),
                    }),
                );
            }
        }
    }
    result
}

/// Diagnostics-only summary of a tool result: tool, outcome, kind,
/// field-error paths/reasons, error reason. No result content.
fn tool_result_record(
    result: &RtResult<RoundtableToolResponse>,
    started: std::time::Instant,
) -> serde_json::Value {
    let elapsed = started.elapsed().as_millis();
    match result {
        Ok(response) => {
            let body: serde_json::Value =
                serde_json::from_slice(&response.body).unwrap_or(serde_json::Value::Null);
            let field_errors: Vec<serde_json::Value> = body
                .get("field_errors")
                .and_then(|value| value.as_array())
                .map(|items| items.iter().take(20).cloned().collect())
                .unwrap_or_default();
            serde_json::json!({
                "event": "tool_call_result",
                "tool": response.tool,
                "ok": true,
                "kind": body.get("kind"),
                "candidate_id": body.get("candidate_id"),
                "field_error_count": body.get("field_errors").and_then(|value| value.as_array()).map(Vec::len),
                "field_errors": field_errors,
                "reply_bytes": response.body.len(),
                "elapsed_ms": elapsed,
            })
        }
        Err(error) => serde_json::json!({
            "event": "tool_call_result",
            "ok": false,
            "error_code": format!("{:?}", error.code),
            "error": error.details.reason,
            "elapsed_ms": elapsed,
        }),
    }
}

struct AdmittedHandler<'a> {
    authority: &'a GateToolAuthority,
    handler: HandlerId,
    evidence_bytes: Option<u64>,
}

impl Drop for AdmittedHandler<'_> {
    fn drop(&mut self) {
        self.authority
            .finish_call(&self.handler, self.evidence_bytes);
    }
}

pub async fn read_evidence(
    scope: &AdmittedToolScope,
    args: ReadEvidenceArgs,
    store: &DurableToolStore,
) -> RtResult<EvidenceSlice> {
    let response = dispatch_tool(
        scope,
        RoundtableToolCall {
            name: "read_evidence".to_string(),
            arguments: json!({
                "file_alias": args.file_alias,
                "start_line": args.start_line,
                "end_line": args.end_line,
            }),
        },
        store,
    )
    .await?;
    let parsed: DecodedRead = serde_json::from_slice(&response.body)
        .map_err(|_| rt_error(ErrorCode::InvalidState, "evidence_encoding"))?;
    let object = scope
        .evidence_ref(&args.file_alias)
        .cloned()
        .ok_or_else(|| rt_error(ErrorCode::InvalidArgument, "unknown_object"))?;
    let text = String::from_utf8(store.get_evidence(&object).await?)
        .map_err(|_| rt_error(ErrorCode::InvalidArgument, "evidence_encoding"))?;
    let end = args.end_line.min(parsed.total_lines);
    let (byte_start, byte_end) = line_byte_range(&text, args.start_line, end);
    let record = store
        .record_evidence(EvidenceDraft {
            origin: "read_evidence".to_string(),
            path: args.file_alias.clone(),
            content_hash: object.content_hash.to_hex(),
            excerpt: parsed.excerpt.clone(),
            owner_attempt_id: Some(store.session.attempt_id.clone()),
            staged_phase_id: store.session.phase_id.clone(),
            phase_revision: store.session.phase_revision,
            published_seq: None,
            verified: true,
            attribution: "verified",
            line_start: Some(args.start_line),
            line_end: Some(end),
            byte_start: Some(byte_start),
            byte_end: Some(byte_end),
        })
        .await?;
    Ok(EvidenceSlice {
        excerpt: parsed.excerpt,
        file_alias: parsed.file_alias,
        total_lines: parsed.total_lines,
        has_more: parsed.has_more,
        encoded_bytes: u64::try_from(response.body.len())
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?,
        byte_start,
        byte_end,
        evidence_alias: record.file_alias,
    })
}

pub async fn search_evidence(
    scope: &AdmittedToolScope,
    args: SearchEvidenceArgs,
    store: &dyn ToolStore,
) -> RtResult<EvidenceSearchPage> {
    let response = dispatch_tool(
        scope,
        RoundtableToolCall {
            name: "search_evidence".to_string(),
            arguments: json!({
                "file_alias": args.file_alias,
                "query": args.query,
                "limit": args.limit,
            }),
        },
        store,
    )
    .await?;
    let parsed: DecodedSearch = serde_json::from_slice(&response.body)
        .map_err(|_| rt_error(ErrorCode::InvalidState, "evidence_encoding"))?;
    Ok(EvidenceSearchPage {
        file_alias: parsed.file_alias,
        hits: parsed
            .hits
            .into_iter()
            .map(|hit| EvidenceSearchHit {
                line: hit.line,
                text: hit.text,
            })
            .collect(),
        literal: parsed.literal,
        encoded_bytes: u64::try_from(response.body.len())
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "overflow"))?,
    })
}

pub async fn persist_candidate(
    scope: &AdmittedToolScope,
    submission_id: SubmissionId,
    raw: Vec<u8>,
    store: &DurableToolStore,
) -> RtResult<CandidateReceipt> {
    store
        .commit_candidate(scope.result_scope(), &submission_id, &raw)
        .await
}

pub async fn register_input_evidence(
    store: &DurableToolStore,
    input: InputEvidence,
) -> RtResult<PersistedEvidence> {
    store
        .record_evidence(EvidenceDraft {
            origin: "snapshot".to_string(),
            path: input.path,
            content_hash: input.content_hash,
            excerpt: String::new(),
            owner_attempt_id: None,
            staged_phase_id: input.staged_phase_id,
            phase_revision: input.phase_revision,
            published_seq: Some(input.published_seq),
            verified: true,
            attribution: "verified",
            line_start: None,
            line_end: None,
            byte_start: None,
            byte_end: None,
        })
        .await
}

pub async fn note_unverified_reference(
    store: &DurableToolStore,
    source: &str,
    locator: &str,
) -> RtResult<PersistedEvidence> {
    if source != "url" && source != "cli_native_read" {
        return Err(rt_error(ErrorCode::InvalidArgument, "unverified_source"));
    }
    store
        .record_evidence(EvidenceDraft {
            origin: source.to_string(),
            path: locator.to_string(),
            content_hash: Hash256::sha256(locator.as_bytes()).to_hex(),
            excerpt: locator.to_string(),
            owner_attempt_id: Some(store.session.attempt_id.clone()),
            staged_phase_id: store.session.phase_id.clone(),
            phase_revision: store.session.phase_revision,
            published_seq: None,
            verified: false,
            attribution: "unverified_reference",
            line_start: None,
            line_end: None,
            byte_start: None,
            byte_end: None,
        })
        .await
}

pub struct DurableToolStore {
    db: RoundtableStore,
    objects: ObjectStore,
    session: ToolSession,
    scope: ResultScope,
    alias_seq: Mutex<u64>,
}

impl DurableToolStore {
    pub fn open(
        db: RoundtableStore,
        objects: ObjectStore,
        session: ToolSession,
        scope: ResultScope,
    ) -> Self {
        Self {
            db,
            objects,
            session,
            scope,
            alias_seq: Mutex::new(0),
        }
    }

    pub async fn audit(&self) -> RtResult<SubmissionAudit> {
        let txn = self.db.connection().begin().await.map_err(storage_err)?;
        let result = async {
            let state = self.attempt_state_in(&txn).await?;
            let rows = load_submissions(&txn, &self.session.room_id, &self.session.attempt_id).await?;
            let replayed = replay(&rows, &self.scope)?;
            let sealed_count = count_i64(
                &txn,
                "SELECT COUNT(*) FROM rt_submissions WHERE room_id = ? AND attempt_id = ? AND sealed = 1",
                vec![text(&self.session.room_id), text(&self.session.attempt_id)],
            )
            .await?;
            let accepted_count = self.accepted_count_in(&txn).await?;
            Ok(SubmissionAudit {
                invalid_count: replayed.invalid_count,
                closed: replayed.closed,
                sealed_count: u64::try_from(sealed_count).unwrap_or(u64::MAX),
                accepted_count: u64::try_from(accepted_count).unwrap_or(u64::MAX),
                attempt_state: state,
            })
        }
        .await;
        let _ = txn.rollback().await;
        result
    }

    pub async fn list_evidence(&self) -> RtResult<Vec<PersistedEvidence>> {
        let rows = self
            .db
            .connection()
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                "SELECT evidence_id, owner_speaker_id, content_hash, attribution, publish_seq, body_json
                 FROM rt_evidence WHERE room_id = ? ORDER BY rowid",
                vec![text(&self.session.room_id)],
            ))
            .await
            .map_err(storage_err)?;
        rows.iter().map(evidence_from_row).collect()
    }

    /// A staged row is audit only. This does not mark the attempt accepted.
    pub async fn promote_staged_candidate(&self) -> RtResult<()> {
        let _ = self.audit().await?;
        Err(rt_error(ErrorCode::InvalidState, "staged_not_accepted"))
    }

    pub async fn note_abnormal_finish(&self, kind: FinishKind) -> RtResult<()> {
        let (state, reason) = match kind {
            FinishKind::Cancelled => ("interrupted", "cancelled"),
            FinishKind::Failed => ("failed", "failed"),
            FinishKind::Normal => {
                return Err(rt_error(ErrorCode::InvalidState, "not_abnormal_finish"))
            }
        };
        let txn = self.db.connection().begin().await.map_err(storage_err)?;
        let result = async {
            self.lock_attempt(&txn).await?;
            exec(
                &txn,
                "UPDATE rt_attempts
                 SET state = ?, finish_reason = ?
                 WHERE room_id = ? AND attempt_id = ? AND state != 'accepted'",
                vec![
                    text(state),
                    text(reason),
                    text(&self.session.room_id),
                    text(&self.session.attempt_id),
                ],
            )
            .await?;
            Ok(())
        }
        .await;
        finish(txn, result).await
    }

    pub(crate) async fn commit_candidate(
        &self,
        scope: &ResultScope,
        submission_id: &SubmissionId,
        raw: &[u8],
    ) -> RtResult<CandidateReceipt> {
        let txn = self.db.connection().begin().await.map_err(storage_err)?;
        // Field errors are a committed audit row. Rolling them back would drop
        // the repair count. Only a failed write rolls back.
        match self.apply_in(&txn, scope, submission_id, raw).await {
            Ok(Applied::Receipt(receipt)) => {
                txn.commit().await.map_err(storage_err)?;
                Ok(receipt)
            }
            Ok(Applied::Rejected(err)) => {
                txn.commit().await.map_err(storage_err)?;
                Err(err)
            }
            Err(err) => {
                let _ = txn.rollback().await;
                Err(err)
            }
        }
    }

    async fn apply_in(
        &self,
        txn: &DatabaseTransaction,
        scope: &ResultScope,
        submission_id: &SubmissionId,
        raw: &[u8],
    ) -> RtResult<Applied> {
        self.lock_attempt(txn).await?;
        let attempt_state = self.attempt_state_in(txn).await?;
        if attempt_state == "accepted" {
            return Err(rt_error(ErrorCode::InvalidState, "staged_not_accepted"));
        }
        let rows = load_submissions(txn, &self.session.room_id, &self.session.attempt_id).await?;
        let prior = replay(&rows, scope)?;
        let decision = submit_candidate(&prior, submission_id, raw, scope);
        if matches!(
            attempt_state.as_str(),
            "interrupted" | "failed" | "timed_out"
        ) {
            if let DecisionKind::Staged(receipt) = &decision.outcome {
                if let Some(stored) = sealed_row(&rows, submission_id) {
                    return original_receipt(stored, receipt).map(Applied::Receipt);
                }
            }
            return Err(rt_error(ErrorCode::InvalidState, "not_accepted"));
        }
        let closes = decision.closes_attempt;
        match decision.outcome {
            DecisionKind::Staged(receipt) => {
                let canonical = canonical_submission(raw)?;
                if Hash256::sha256(&canonical) != receipt.payload_hash {
                    return Err(rt_error(ErrorCode::InvalidState, "validator_disagreement"));
                }
                validate_result(&canonical, scope)
                    .map_err(|_| rt_error(ErrorCode::InvalidState, "validator_disagreement"))?;
                if let Some(stored) = sealed_row(&rows, submission_id) {
                    return original_receipt(stored, &receipt).map(Applied::Receipt);
                }
                exec(
                    txn,
                    "INSERT INTO rt_submission_scopes (room_id,attempt_id,scope_json)
                    VALUES (?,?,?)",
                    vec![
                        text(&self.session.room_id),
                        text(&self.session.attempt_id),
                        text(
                            &serde_json::to_string(scope)
                                .map_err(|_| rt_error(ErrorCode::InvalidState, "scope_encode"))?,
                        ),
                    ],
                )
                .await?;
                self.insert_submission(
                    txn,
                    submission_id,
                    &receipt.payload_hash.to_hex(),
                    &canonical,
                    "[]",
                    Some(
                        &serde_json::to_string(&receipt)
                            .map_err(|_| rt_error(ErrorCode::InvalidState, "receipt_encode"))?,
                    ),
                )
                .await?;
                Ok(Applied::Receipt(receipt))
            }
            DecisionKind::FieldErrors(errors) => {
                if rows
                    .iter()
                    .all(|row| row.submission_id != submission_id.to_string())
                {
                    self.insert_submission(
                        txn,
                        submission_id,
                        &Hash256::sha256(raw).to_hex(),
                        raw,
                        &serde_json::to_string(&stored_errors(&errors))
                            .map_err(|_| rt_error(ErrorCode::InvalidState, "field_error_encode"))?,
                        None,
                    )
                    .await?;
                    if closes {
                        exec(
                            txn,
                            "UPDATE rt_attempts
                             SET state = 'invalid', finish_reason = 'invalid'
                             WHERE room_id = ? AND attempt_id = ? AND state != 'accepted'",
                            vec![text(&self.session.room_id), text(&self.session.attempt_id)],
                        )
                        .await?;
                    }
                }
                Ok(Applied::Rejected(field_errors(&errors)))
            }
            DecisionKind::SubmissionConflict => {
                Err(RtError::from_reason(InternalReason::SubmissionConflict))
            }
            DecisionKind::ResultAlreadySealed => {
                if sealed_row(&rows, submission_id).is_some() {
                    Err(RtError::from_reason(InternalReason::SubmissionConflict))
                } else {
                    Err(RtError::from_reason(InternalReason::ResultAlreadySealed))
                }
            }
            DecisionKind::AttemptClosed => Err(RtError::from_reason(InternalReason::AttemptClosed)),
        }
    }

    async fn insert_submission(
        &self,
        txn: &DatabaseTransaction,
        submission_id: &SubmissionId,
        payload_hash: &str,
        raw: &[u8],
        errors_json: &str,
        receipt_json: Option<&str>,
    ) -> RtResult<()> {
        let sealed = i64::from(receipt_json.is_some());
        let raw = std::str::from_utf8(raw)
            .map_err(|_| rt_error(ErrorCode::InvalidArgument, "submission_encoding"))?;
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "INSERT INTO rt_submissions (
                room_id, attempt_id, submission_id, payload_hash, candidate_ref,
                validation_errors_json, receipt_json, sealed
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                text(&self.session.room_id),
                text(&self.session.attempt_id),
                text(&submission_id.to_string()),
                text(payload_hash),
                text(raw),
                text(errors_json),
                opt_text(receipt_json),
                num(sealed),
            ],
        ))
        .await
        .map_err(|err| unique_or_storage(err, sealed))?;
        Ok(())
    }

    async fn lock_attempt(&self, txn: &DatabaseTransaction) -> RtResult<()> {
        // The write is the attempt lock. A no-op assignment still reserves the
        // SQLite writer. Builds that do not count an unchanged row are accepted
        // when the row is visible inside this transaction.
        let updated = exec(
            txn,
            "UPDATE rt_attempts
             SET residual_remote_work = residual_remote_work
             WHERE room_id = ? AND attempt_id = ?",
            vec![text(&self.session.room_id), text(&self.session.attempt_id)],
        )
        .await?;
        if updated == 1 {
            return Ok(());
        }
        let present = count_i64(
            txn,
            "SELECT COUNT(*) FROM rt_attempts WHERE room_id = ? AND attempt_id = ?",
            vec![text(&self.session.room_id), text(&self.session.attempt_id)],
        )
        .await?;
        if present != 1 {
            return Err(rt_error(ErrorCode::InvalidArgument, "attempt_missing"));
        }
        Ok(())
    }

    async fn attempt_state_in(&self, conn: &impl ConnectionTrait) -> RtResult<String> {
        query_text(
            conn,
            "SELECT state FROM rt_attempts WHERE room_id = ? AND attempt_id = ?",
            vec![text(&self.session.room_id), text(&self.session.attempt_id)],
        )
        .await
    }

    async fn accepted_count_in(&self, conn: &impl ConnectionTrait) -> RtResult<i64> {
        count_i64(
            conn,
            "SELECT
                (SELECT COUNT(*) FROM rt_attempts WHERE room_id = ? AND state = 'accepted')
                +
                (SELECT COUNT(*) FROM rt_turns WHERE room_id = ? AND accepted_attempt_id IS NOT NULL)",
            vec![text(&self.session.room_id), text(&self.session.room_id)],
        )
        .await
    }

    async fn record_evidence(&self, draft: EvidenceDraft) -> RtResult<PersistedEvidence> {
        let alias = {
            let mut seq = lock(&self.alias_seq);
            *seq = seq.saturating_add(1);
            format!("E{seq}")
        };
        let evidence_id = Uuid::new_v4().to_string();
        let body = EvidenceBody {
            file_alias: alias.clone(),
            workspace_snapshot_id: self.session.workspace_snapshot_id.clone(),
            manifest_hash: self.session.manifest_hash.clone(),
            path: draft.path.clone(),
            owner_attempt_id: draft.owner_attempt_id.clone(),
            staged_phase_id: draft.staged_phase_id.clone(),
            phase_revision: draft.phase_revision,
            line_start: draft.line_start,
            line_end: draft.line_end,
            byte_start: draft.byte_start,
            byte_end: draft.byte_end,
            excerpt_hash: Hash256::sha256(draft.excerpt.as_bytes()).to_hex(),
            excerpt: draft.excerpt.clone(),
            verified: draft.verified,
            origin: draft.origin.to_string(),
        };
        let body_json = serde_json::to_string(&body)
            .map_err(|_| rt_error(ErrorCode::InvalidState, "evidence_encode"))?;
        exec(
            self.db.connection(),
            "INSERT INTO rt_evidence (
                room_id, evidence_id, manifest_id, owner_speaker_id, content_hash,
                attribution, publish_seq, body_json
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            vec![
                text(&self.session.room_id),
                text(&evidence_id),
                text(&self.session.manifest_id),
                text(&self.session.speaker_id),
                text(&draft.content_hash),
                text(draft.attribution),
                opt_i64(draft.published_seq),
                text(&body_json),
            ],
        )
        .await?;
        Ok(PersistedEvidence {
            evidence_id,
            file_alias: alias,
            owner_speaker_id: self.session.speaker_id.clone(),
            owner_attempt_id: draft.owner_attempt_id,
            staged_phase_id: draft.staged_phase_id,
            phase_revision: draft.phase_revision,
            published_seq: draft.published_seq,
            content_hash: draft.content_hash,
            attribution: draft.attribution.to_string(),
            verified: draft.verified,
            origin: draft.origin.to_string(),
            path: draft.path,
            excerpt: draft.excerpt,
            line_start: draft.line_start,
            line_end: draft.line_end,
            byte_start: draft.byte_start,
            byte_end: draft.byte_end,
        })
    }
}

#[async_trait]
impl ToolStore for DurableToolStore {
    async fn get_evidence(&self, object: &super::tool_core::ObjectRef) -> RtResult<Vec<u8>> {
        let stored = objects::ObjectRef {
            object_id: object.content_hash.to_hex(),
            content_hash: object.content_hash,
            total_bytes: object.total_bytes.0,
        };
        self.objects.get_verified(&stored).await
    }

    async fn submit(
        &self,
        submission_id: &SubmissionId,
        validated: &roundtable_protocol::ValidatedResult,
    ) -> RtResult<CandidateReceipt> {
        let _ = (submission_id, validated);
        Err(rt_error(ErrorCode::InvalidState, "canonical_required"))
    }

    async fn submit_canonical(
        &self,
        submission_id: &SubmissionId,
        validated: &roundtable_protocol::ValidatedResult,
        raw: &[u8],
    ) -> RtResult<CandidateReceipt> {
        if Hash256::sha256(raw) != validated.canonical_hash {
            return Err(rt_error(ErrorCode::InvalidState, "validator_disagreement"));
        }
        self.commit_candidate(&self.scope, submission_id, raw).await
    }

    async fn note_field_errors(
        &self,
        submission_id: &SubmissionId,
        raw: &[u8],
        scope: &ResultScope,
    ) -> RtResult<()> {
        match self.commit_candidate(scope, submission_id, raw).await {
            Err(err) if err.details.reason.as_deref() == Some("field_errors") => Ok(()),
            Ok(_) => Err(rt_error(ErrorCode::InvalidState, "field_error_sealed")),
            Err(err) => Err(err),
        }
    }
}

struct EvidenceDraft {
    origin: String,
    path: String,
    content_hash: String,
    excerpt: String,
    owner_attempt_id: Option<String>,
    staged_phase_id: String,
    phase_revision: i64,
    published_seq: Option<i64>,
    verified: bool,
    attribution: &'static str,
    line_start: Option<u64>,
    line_end: Option<u64>,
    byte_start: Option<u64>,
    byte_end: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EvidenceBody {
    file_alias: String,
    workspace_snapshot_id: String,
    manifest_hash: String,
    path: String,
    owner_attempt_id: Option<String>,
    staged_phase_id: String,
    phase_revision: i64,
    line_start: Option<u64>,
    line_end: Option<u64>,
    byte_start: Option<u64>,
    byte_end: Option<u64>,
    excerpt_hash: String,
    excerpt: String,
    verified: bool,
    origin: String,
}

enum Applied {
    Receipt(CandidateReceipt),
    Rejected(RtError),
}

#[derive(Debug)]
struct SubmissionRow {
    submission_id: String,
    payload_hash: String,
    receipt_json: Option<String>,
    sealed: i64,
    raw: String,
}

#[derive(Deserialize)]
struct DecodedRead {
    excerpt: String,
    file_alias: String,
    has_more: bool,
    total_lines: u64,
}

#[derive(Deserialize)]
struct DecodedSearch {
    file_alias: String,
    hits: Vec<DecodedHit>,
    literal: bool,
}

#[derive(Deserialize)]
struct DecodedHit {
    line: u64,
    text: String,
}

#[derive(Serialize)]
struct StoredError<'a> {
    path: &'a str,
    code: &'a str,
}

fn stored_errors(errors: &[SchemaFieldError]) -> Vec<StoredError<'_>> {
    errors
        .iter()
        .map(|error| StoredError {
            path: &error.path,
            code: error.code.as_str(),
        })
        .collect()
}

fn field_errors(errors: &[SchemaFieldError]) -> RtError {
    RtError {
        code: ErrorCode::InvalidArgument,
        message: "The request is invalid.".to_string(),
        retryable: ErrorCode::InvalidArgument.retryable(),
        current_revision: None,
        details: ErrorDetails {
            reason: Some("field_errors".to_string()),
            field_errors: errors
                .iter()
                .map(|error| WireFieldError {
                    path: error.path.clone(),
                    reason: error.code.as_str().to_string(),
                })
                .collect(),
        },
    }
}

fn canonical_submission(raw: &[u8]) -> RtResult<Vec<u8>> {
    let limits = ParseLimits {
        origin: LimitsOrigin::Custom,
        max_bytes: v1_1::MAX_RESULT_BYTES as usize,
        max_depth: profile_suggestions::MAX_JSON_DEPTH,
    };
    let value = parse_strict_json(raw, &limits).map_err(|_| {
        tracing::warn!(
            excerpt = %super::diagnostics::redact_untrusted_excerpt(raw),
            "rejected submit_result payload"
        );
        rt_error(ErrorCode::InvalidState, "validator_disagreement")
    })?;
    canonical_bytes(&value).map_err(|_| rt_error(ErrorCode::InvalidState, "validator_disagreement"))
}

fn replay(rows: &[SubmissionRow], scope: &ResultScope) -> RtResult<SubmissionState> {
    let mut state = SubmissionState::open();
    for row in rows {
        if Hash256::sha256(row.raw.as_bytes()).to_hex() != row.payload_hash {
            return Err(rt_error(ErrorCode::StorageUnavailable, "submission_hash"));
        }
        let id = SubmissionId::from_str(&row.submission_id)
            .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "submission_id"))?;
        let decision = submit_candidate(&state, &id, row.raw.as_bytes(), scope);
        state = decision.next_state;
    }
    Ok(state)
}

fn sealed_row<'a>(
    rows: &'a [SubmissionRow],
    submission_id: &SubmissionId,
) -> Option<&'a SubmissionRow> {
    rows.iter()
        .find(|row| row.submission_id == submission_id.to_string() && row.sealed == 1)
}

fn original_receipt(
    stored: &SubmissionRow,
    receipt: &CandidateReceipt,
) -> RtResult<CandidateReceipt> {
    let json = stored
        .receipt_json
        .as_deref()
        .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "receipt_missing"))?;
    let original = serde_json::from_str::<CandidateReceipt>(json)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "receipt_corrupt"))?;
    if original.payload_hash == receipt.payload_hash
        && original.submission_id == receipt.submission_id
    {
        Ok(original)
    } else {
        Err(RtError::from_reason(InternalReason::SubmissionConflict))
    }
}

fn unique_or_storage(err: DbErr, sealed: i64) -> RtError {
    let unique = matches!(
        err.sql_err(),
        Some(sea_orm::SqlErr::UniqueConstraintViolation(_))
    ) || err.to_string().contains("UNIQUE");
    if !unique {
        return storage_err(err);
    }
    if sealed == 1 {
        RtError::from_reason(InternalReason::ResultAlreadySealed)
    } else {
        RtError::from_reason(InternalReason::SubmissionConflict)
    }
}

fn line_byte_range(text: &str, start: u64, end: u64) -> (u64, u64) {
    if start == 0 || end < start {
        return (0, 0);
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let start_idx = (start as usize).saturating_sub(1);
    let end_idx = (end as usize).min(lines.len());
    if start_idx >= lines.len() || end_idx < start_idx {
        return (0, 0);
    }
    let mut offset = 0usize;
    for line in &lines[..start_idx] {
        offset = offset.saturating_add(line.len()).saturating_add(1);
    }
    let byte_start = offset;
    for line in &lines[start_idx..end_idx] {
        offset = offset.saturating_add(line.len()).saturating_add(1);
    }
    if end_idx == lines.len() && !text.ends_with('\n') && end_idx > start_idx {
        offset = offset.saturating_sub(1);
    }
    (
        u64::try_from(byte_start).unwrap_or(u64::MAX),
        u64::try_from(offset).unwrap_or(u64::MAX),
    )
}

fn evidence_from_row(row: &QueryResult) -> RtResult<PersistedEvidence> {
    let evidence_id = column::<String>(row, 0)?;
    let owner_speaker_id = column::<String>(row, 1)?;
    let content_hash = column::<String>(row, 2)?;
    let attribution = column::<String>(row, 3)?;
    let published_seq = column::<Option<i64>>(row, 4)?;
    let body_json = column::<String>(row, 5)?;
    let body: EvidenceBody = serde_json::from_str(&body_json)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "evidence_body"))?;
    Ok(PersistedEvidence {
        evidence_id,
        file_alias: body.file_alias,
        owner_speaker_id,
        owner_attempt_id: body.owner_attempt_id,
        staged_phase_id: body.staged_phase_id,
        phase_revision: body.phase_revision,
        published_seq,
        content_hash,
        attribution,
        verified: body.verified,
        origin: body.origin,
        path: body.path,
        excerpt: body.excerpt,
        line_start: body.line_start,
        line_end: body.line_end,
        byte_start: body.byte_start,
        byte_end: body.byte_end,
    })
}

async fn load_submissions(
    conn: &impl ConnectionTrait,
    room_id: &str,
    attempt_id: &str,
) -> RtResult<Vec<SubmissionRow>> {
    let rows = conn
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT submission_id, payload_hash, receipt_json, sealed, candidate_ref
             FROM rt_submissions
             WHERE room_id = ? AND attempt_id = ?
             ORDER BY rowid",
            vec![text(room_id), text(attempt_id)],
        ))
        .await
        .map_err(storage_err)?;
    rows.iter()
        .map(|row| {
            Ok(SubmissionRow {
                submission_id: column(row, 0)?,
                payload_hash: column(row, 1)?,
                receipt_json: column(row, 2)?,
                sealed: column(row, 3)?,
                raw: column::<Option<String>>(row, 4)?
                    .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "submission_raw"))?,
            })
        })
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

async fn finish(txn: DatabaseTransaction, result: RtResult<()>) -> RtResult<()> {
    match result {
        Ok(()) => txn.commit().await.map_err(storage_err),
        Err(err) => {
            let _ = txn.rollback().await;
            Err(err)
        }
    }
}

async fn exec(conn: &impl ConnectionTrait, sql: &str, values: Vec<DbValue>) -> RtResult<u64> {
    conn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        sql,
        values,
    ))
    .await
    .map(|result| result.rows_affected())
    .map_err(storage_err)
}

async fn count_i64(conn: &impl ConnectionTrait, sql: &str, values: Vec<DbValue>) -> RtResult<i64> {
    let row = one_row(conn, sql, values).await?;
    column(&row, 0)
}

async fn query_text(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<DbValue>,
) -> RtResult<String> {
    let row = one_row(conn, sql, values).await?;
    column(&row, 0)
}

async fn one_row(
    conn: &impl ConnectionTrait,
    sql: &str,
    values: Vec<DbValue>,
) -> RtResult<QueryResult> {
    conn.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        sql,
        values,
    ))
    .await
    .map_err(storage_err)?
    .ok_or_else(|| rt_error(ErrorCode::StorageUnavailable, "roundtable_row"))
}

fn column<T: TryGetable>(row: &QueryResult, index: usize) -> RtResult<T> {
    row.try_get_by_index(index)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "roundtable_column"))
}

fn text(value: &str) -> DbValue {
    value.into()
}

fn opt_text(value: Option<&str>) -> DbValue {
    value.map(ToOwned::to_owned).into()
}

fn opt_i64(value: Option<i64>) -> DbValue {
    value.into()
}

fn num(value: i64) -> DbValue {
    value.into()
}

fn storage_err(err: DbErr) -> RtError {
    let _ = err;
    rt_error(ErrorCode::StorageUnavailable, "roundtable_storage")
}
