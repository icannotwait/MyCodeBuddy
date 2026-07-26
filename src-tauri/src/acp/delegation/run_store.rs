//! Authoritative store for `delegation_task_runs`.
//!
//! Conversation columns remain a latest-run **projection** only. Settlement and
//! runtime updates write the run row plus a monotonic
//! `conversation.delegation_run_generation` fence in one transaction.
//!
//! Also owns server-side `task_preview` derivation and `request_fingerprint`
//! canonicalization used by both `delegate_to_agent` and `continue_delegation`.

#[cfg(any(test, feature = "test-utils"))]
use std::collections::VecDeque;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use regex::Regex;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::acp::delegation::launch_snapshot::{snapshot_is_complete, LaunchSnapshot};
use crate::acp::delegation::runtime_stats::{
    decode_persisted_runtime_stats, DelegationRuntimeStats, PersistedRuntimeStatsColumns,
};
use crate::acp::delegation::store::{
    classify_sqlite_transient, is_transient_sqlite, PersistedTask, PromoteRetryPolicy, Settlement,
    SqliteTransientClass, TaskStoreError, TerminalTaskWrite,
};
use crate::acp::delegation::types::TaskStatus;
use crate::db::entities::conversation::{self, ConversationStatus, DelegationTaskStatus};
use crate::db::entities::delegation_lineage_budget::{self, Entity as LineageBudget};
use crate::db::entities::delegation_task_run::{
    self, AdmissionClass, DelegationRunStatus, Entity as DelegationTaskRun,
};
use crate::db::entities::delegation_work_unit_budget::{self, Entity as WorkUnitBudget};
use crate::db::AppDatabase;
use crate::models::AgentType;

/// Maximum Unicode scalars retained in a durable `task_preview` after redaction.
pub const TASK_PREVIEW_SCALAR_CAP: usize = 200;

/// Platform rail: at most this many unexpected-cancel continues per lineage /
/// work-unit (charged only at `reserving` → `running`).
pub const UNEXPECTED_CONTINUE_LIMIT: i64 = 2;

/// Platform rail: at most this many recorded replacements per lineage /
/// work-unit (charged only at `reserving` → `running`).
pub const REPLACEMENT_LIMIT: i64 = 1;

/// Hard ceiling on generation per child thread. Creating generation >
/// [`MAX_GENERATION`] is refused with `budget_exhausted`.
pub const MAX_GENERATION: i64 = 100;

fn is_valid_task_id_prefix(prefix: &str) -> bool {
    prefix.len() == 8 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Minimal structured termination audit for startup host_restarted settlement.
/// Preserves enough provenance for continuability: prior non-terminal status
/// and admission_class (reserving inherits class; running → unexpected_continue).
fn host_restarted_termination_audit(
    prior_status: &DelegationRunStatus,
    admission_class: AdmissionClass,
) -> String {
    let prior = match prior_status {
        DelegationRunStatus::Reserving => "reserving",
        DelegationRunStatus::Running => "running",
        DelegationRunStatus::Completed => "completed",
        DelegationRunStatus::Failed => "failed",
        DelegationRunStatus::Canceled => "canceled",
    };
    let class = match admission_class {
        AdmissionClass::NormalRevision => "normal_revision",
        AdmissionClass::UnexpectedContinue => "unexpected_continue",
        AdmissionClass::Replacement => "replacement",
    };
    serde_json::json!({
        "version": 1,
        "source": "host_restart",
        "reason": "host_restarted",
        "prior_status": prior,
        "admission_class": class,
    })
    .to_string()
}

fn host_restarted_bound_reserving_audit() -> String {
    serde_json::json!({
        "version": 1,
        "source": "host_restart",
        "reason": "admission_unknown",
        "prior_status": "reserving",
        "restart_provenance": "bound_reserving",
        "note": "child_connection_id was bound; prompt may have been accepted before restart"
    })
    .to_string()
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Secret / credential substrings replaced with `[redacted]` before truncating.
/// Patterns are applied to the **full** admitted task text (fail-closed).
fn secret_redaction_regexes() -> &'static [Regex] {
    use std::sync::OnceLock;
    static RE: OnceLock<Vec<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        // Order: longer / more-specific prefixes first where they share stems.
        const PATTERNS: &[&str] = &[
            r"-----BEGIN[^-]*-----[\s\S]*?-----END[^-]*-----",
            r"(?i)Bearer\s+\S+",
            r"github_pat_[A-Za-z0-9_]+",
            r"ghp_[A-Za-z0-9_]+",
            r"glpat-[A-Za-z0-9_\-]+",
            r"sk-[A-Za-z0-9_\-]+",
            r"xox[a-zA-Z0-9\-]+",
            r"AKIA[0-9A-Z]{16}",
        ];
        PATTERNS
            .iter()
            .map(|p| Regex::new(p).expect("static secret redaction regex"))
            .collect()
    })
}

/// Server-derived display preview: redact secrets on full text, then take the
/// first [`TASK_PREVIEW_SCALAR_CAP`] Unicode scalars. Never stores the full prompt.
///
/// Fail-closed: if the input is empty after trimming, returns an empty string.
/// (`&str` is always UTF-8 in Rust; callers with non-UTF-8 bytes must not pass them.)
pub fn derive_task_preview(task: &str) -> String {
    if task.is_empty() {
        return String::new();
    }
    let mut redacted = task.to_string();
    for re in secret_redaction_regexes() {
        redacted = re.replace_all(&redacted, "[redacted]").into_owned();
    }
    redacted.chars().take(TASK_PREVIEW_SCALAR_CAP).collect()
}

/// Non-reversible request hash for exact duplicate detection.
///
/// Canonicalization (fixed field order; absent optionals are **empty strings**,
/// never omitted):
/// 1. tool_name
/// 2. full task text (NFC-normalized)
/// 3. work_unit_key or empty
/// 4. replaces_task_id or empty
/// 5. replacement_reason or empty
/// 6. target task_id or empty
/// 7. route_fingerprint as lowercase hex
///
/// Fields are encoded as a JSON array of strings via deterministic
/// `serde_json` serialization (not raw delimiter join). That framing is
/// immune to in-field U+001E / quote / control characters collapsing
/// distinct tuples into the same byte stream.
///
/// Returns lowercase hex SHA-256 of the canonical bytes.
pub fn request_fingerprint(
    tool_name: &str,
    task_text: &str,
    work_unit_key: Option<&str>,
    replaces_task_id: Option<&str>,
    replacement_reason: Option<&str>,
    target_task_id: Option<&str>,
    route_fingerprint_hex: &str,
) -> String {
    let task_nfc = nfc(task_text);
    let route = route_fingerprint_hex.to_ascii_lowercase();
    let fields = [
        tool_name,
        task_nfc.as_str(),
        work_unit_key.unwrap_or(""),
        replaces_task_id.unwrap_or(""),
        replacement_reason.unwrap_or(""),
        target_task_id.unwrap_or(""),
        route.as_str(),
    ];
    // Compact JSON array form is stable for plain strings (no key order issues).
    let canonical =
        serde_json::to_string(&fields).expect("request_fingerprint fields are plain strings");
    let digest = Sha256::digest(canonical.as_bytes());
    hex_lower(&digest)
}

/// Inputs for inserting a run in `reserving` status (durable claim before spawn/resume).
#[derive(Debug, Clone)]
pub struct ReservingRunInsert {
    pub task_id: String,
    pub root_task_id: String,
    pub previous_task_id: Option<String>,
    pub generation: i64,
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: Option<String>,
    pub child_conversation_id: i32,
    pub agent_type: String,
    pub profile_id: Option<String>,
    pub workspace_path: Option<String>,
    pub route_fingerprint: Option<String>,
    pub launch_snapshot_version: Option<String>,
    pub mode_id: Option<String>,
    pub config_values_json: Option<String>,
    pub task_preview: Option<String>,
    pub request_fingerprint: Option<String>,
    pub admission_class: AdmissionClass,
    pub lineage_root_task_id: String,
    pub work_unit_key: Option<String>,
    pub history_only: bool,
    pub replaced_task_id: Option<String>,
    pub replacement_reason: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
}

/// Projection payload applied to the child conversation on a successful fence write.
///
/// For most fields, outer `None` means leave the column unchanged.
/// For nullable line totals (`additions` / `deletions`), use a nested option:
/// - outer `None` → leave unchanged
/// - `Some(None)` → write SQL NULL (clear stale known totals)
/// - `Some(Some(n))` → write `n`
#[derive(Debug, Clone)]
pub struct ConversationProjection {
    pub generation: i64,
    pub task_status: Option<DelegationTaskStatus>,
    /// Nested Option: outer Some = write; inner Some = value, inner None = clear / NULL.
    pub error_code: Option<Option<String>>,
    /// Nested Option: outer Some = write; inner Some = value, inner None = clear / NULL.
    pub finished_at: Option<Option<DateTime<Utc>>>,
    pub conversation_status: Option<ConversationStatus>,
    pub started_at: Option<DateTime<Utc>>,
    /// Optional runtime rollup fields projected onto conversation columns.
    pub tool_call_count: Option<i64>,
    pub edit_tool_call_count: Option<i64>,
    pub touched_files_json: Option<String>,
    pub touched_files_truncated: Option<bool>,
    pub additions: Option<Option<i64>>,
    pub deletions: Option<Option<i64>>,
    pub line_counts_complete: Option<bool>,
    /// When true, always write NULL to generation-scoped runtime rollup fields
    /// (tool/line counts, touched files, additions, deletions).
    pub reset_generation_rollups: bool,
}

/// Durable run row view for broker / status paths.
#[derive(Debug, Clone)]
pub struct PersistedRun {
    pub task_id: String,
    pub root_task_id: String,
    pub previous_task_id: Option<String>,
    pub generation: i64,
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: Option<String>,
    pub child_conversation_id: i32,
    pub agent_type: AgentType,
    pub status: TaskStatus,
    pub run_status: DelegationRunStatus,
    pub error_code: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub reached_running_at: Option<DateTime<Utc>>,
    pub child_connection_id: Option<String>,
    pub request_fingerprint: Option<String>,
    pub task_preview: Option<String>,
    pub admission_class: AdmissionClass,
    pub lineage_root_task_id: String,
    pub work_unit_key: Option<String>,
    pub history_only: bool,
    pub route_fingerprint: Option<String>,
    pub workspace_path: Option<String>,
    pub launch_snapshot_version: Option<String>,
    pub mode_id: Option<String>,
    pub config_values_json: Option<String>,
    pub profile_id: Option<String>,
    pub runtime_stats: Option<DelegationRuntimeStats>,
}

/// Retry metadata on every promote outcome (success or failure). Counts every
/// transient class observed across attempts (mixed BUSY then LOCKED both count).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromoteAttemptMeta {
    /// Total attempts used (1..=3 for production policy).
    pub attempts: u32,
    pub busy_retries: u32,
    pub locked_retries: u32,
    pub busy_snapshot_retries: u32,
}

/// Why a promote claim / reread produced a state conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromoteConflictClass {
    /// Durable row is missing.
    Missing,
    /// Child connection ownership does not match the promote caller.
    Ownership,
    /// Durable status is incompatible with promotion (still reserving after
    /// zero-row / ambiguous failure, or other non-terminal mismatch).
    Status,
}

/// Transient class that exhausted the promote-local retry budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromoteRetryClass {
    Busy,
    Locked,
    BusySnapshot,
}

impl From<SqliteTransientClass> for PromoteRetryClass {
    fn from(class: SqliteTransientClass) -> Self {
        match class {
            SqliteTransientClass::Busy => Self::Busy,
            SqliteTransientClass::Locked => Self::Locked,
            SqliteTransientClass::BusySnapshot => Self::BusySnapshot,
        }
    }
}

/// Public promote outcome kind. Task 4 matches this enum directly.
#[derive(Debug, Clone)]
pub enum PromoteRunningKind {
    Promoted { run: PersistedRun },
    AlreadyRunning { run: PersistedRun },
    TerminalWinner { run: PersistedRun },
    BudgetExhausted { message: String },
    StateConflict {
        class: PromoteConflictClass,
        message: String,
    },
    RetryExhausted {
        class: PromoteRetryClass,
        message: String,
    },
    Permanent { message: String },
}

/// Every outcome (success or failure) carries attempt meta so mixed transient
/// retries are never dropped before classification.
#[derive(Debug, Clone)]
pub struct PromoteRunningOutcome {
    pub kind: PromoteRunningKind,
    pub meta: PromoteAttemptMeta,
}

/// Test-only fault injection for promote retry / commit-ambiguity paths.
#[cfg(any(test, feature = "test-utils"))]
#[derive(Debug, Clone)]
pub enum PromoteTestFault {
    /// Fail **after claim write** with a synthetic `DbErr` so the outer path
    /// classifies a raw transaction-body error (not a pre-body skip).
    AfterClaimTransient(SqliteTransientClass),
    /// Fail **after budget charge** with a synthetic `DbErr` so claim+charge
    /// must roll back together when the attempt is retried/exhausted.
    AfterBudgetTransient(SqliteTransientClass),
    /// Fail **after status write, at projection** with a synthetic `DbErr` so
    /// projection-side BUSY/LOCKED classify via raw `DbErr` (not Permanent).
    AfterProjectionTransient(SqliteTransientClass),
    /// Fail this attempt as a permanent/ambiguous error without opening a
    /// promote write (commit-ambiguity reread against current durable truth).
    AmbiguousPermanent { message: String },
}

/// Reconstruct the immutable non-secret launch snapshot carried by a durable
/// run. `None` is a legacy/incomplete snapshot and must never be resumed.
pub fn launch_snapshot_from_run(run: &PersistedRun) -> Option<LaunchSnapshot> {
    Some(LaunchSnapshot {
        workspace_path: run.workspace_path.clone()?,
        route_fingerprint: run.route_fingerprint.clone()?,
        launch_snapshot_version: run.launch_snapshot_version.clone()?,
        mode_id: run.mode_id.clone(),
        config_values_json: run.config_values_json.clone()?,
        profile_id: run.profile_id.clone(),
    })
}

/// Outcome of a gen-1 durable reserve attempt.
#[derive(Debug, Clone)]
pub enum Gen1AdmitOutcome {
    /// New reserving row inserted.
    Created(PersistedRun),
    /// Exact `request_fingerprint` match for the same parent tool use —
    /// return the existing run without insert (idempotent success).
    Idempotent(PersistedRun),
}

/// Inputs for pure continuability decision (design decision table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinueEligibility {
    pub history_only: bool,
    pub is_latest: bool,
    pub has_active_run: bool,
    pub child_superseded: bool,
    pub child_ownership_valid: bool,
    pub agent_type_matches: bool,
    pub snapshot_complete: bool,
    pub external_id_present: bool,
    pub run_status: DelegationRunStatus,
    pub error_code: Option<String>,
    pub admission_class: AdmissionClass,
    pub reached_running: bool,
    pub termination_audit_json: Option<String>,
}

/// Continuability decision after ownership is already resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinueDecision {
    Admit(AdmissionClass),
    BusyThread,
    StaleTaskId,
    NotContinuable,
}

/// Inputs for durable continue reserve (post-fingerprint on target load).
#[derive(Debug, Clone)]
pub struct ContinueRunAdmission {
    pub task_id: String,
    pub parent_conversation_id: i32,
    pub parent_tool_use_id: String,
    pub target_task_id: String,
    pub task_preview: String,
    pub request_fingerprint: String,
    pub work_unit_key: Option<String>,
}

/// Outcome of a continue durable reserve attempt.
#[derive(Debug, Clone)]
pub enum ContinueAdmitOutcome {
    Created(PersistedRun),
    Idempotent(PersistedRun),
}

/// Decide continue eligibility with design precedence:
/// busy → stale → not_continuable → admit (with admission_class).
pub fn decide_continue_eligibility(e: &ContinueEligibility) -> ContinueDecision {
    // Lifecycle gates that run after parent-tool idempotency (caller-side).
    if e.has_active_run {
        return ContinueDecision::BusyThread;
    }
    if !e.is_latest {
        return ContinueDecision::StaleTaskId;
    }
    if e.history_only
        || e.child_superseded
        || !e.child_ownership_valid
        || !e.agent_type_matches
        || !e.snapshot_complete
        || !e.external_id_present
    {
        return ContinueDecision::NotContinuable;
    }

    match e.run_status {
        DelegationRunStatus::Completed => ContinueDecision::Admit(AdmissionClass::NormalRevision),
        DelegationRunStatus::Failed => {
            if e.error_code.as_deref() == Some("host_restarted")
                && !e.reached_running
                && is_host_restarted_reserving_audit(e.termination_audit_json.as_deref())
            {
                // Pre-admission host_restarted: inherit class unless replacement.
                if e.admission_class == AdmissionClass::Replacement {
                    return ContinueDecision::NotContinuable;
                }
                return ContinueDecision::Admit(e.admission_class.clone());
            }
            if e.reached_running && is_revision_eligible_failure(e.error_code.as_deref()) {
                ContinueDecision::Admit(AdmissionClass::NormalRevision)
            } else {
                ContinueDecision::NotContinuable
            }
        }
        DelegationRunStatus::Canceled => {
            if e.reached_running
                && is_unexpected_cancellation_audit(e.termination_audit_json.as_deref())
            {
                ContinueDecision::Admit(AdmissionClass::UnexpectedContinue)
            } else {
                ContinueDecision::NotContinuable
            }
        }
        DelegationRunStatus::Reserving | DelegationRunStatus::Running => {
            // Should have been busy when has_active_run; fail closed.
            ContinueDecision::BusyThread
        }
    }
}

fn is_revision_eligible_failure(code: Option<&str>) -> bool {
    match code {
        None => true,
        Some("route_policy_rejected")
        | Some("budget_exhausted")
        | Some("not_supported")
        | Some("unresumable")
        | Some("parent_canceled")
        | Some("parent_turn_failed")
        | Some("join_abandoned")
        | Some("parent_disconnected") => false,
        Some("host_restarted") => false, // handled separately (inherit)
        Some("admission_failed") | Some("admission_unknown") => false,
        Some(_) => true,
    }
}

fn is_host_restarted_reserving_audit(audit: Option<&str>) -> bool {
    let Some(raw) = audit else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    v.get("source").and_then(|s| s.as_str()) == Some("host_restart")
        && v.get("reason").and_then(|s| s.as_str()) == Some("host_restarted")
        && v.get("prior_status").and_then(|s| s.as_str()) == Some("reserving")
}

/// Structured termination audit identifies unexpected cancel/recovery.
fn is_unexpected_cancellation_audit(audit: Option<&str>) -> bool {
    let Some(raw) = audit else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    let source = v.get("source").and_then(|s| s.as_str()).unwrap_or("");
    let reason = v.get("reason").and_then(|s| s.as_str()).unwrap_or("");
    let prior = v.get("prior_status").and_then(|s| s.as_str()).unwrap_or("");
    // Host restart is recoverable only after the prior prompt was durable-running.
    if source == "host_restart" && prior == "running" && reason == "host_restarted" {
        return true;
    }
    matches!(
        source,
        "transport_disconnect" | "process_exit" | "session_loss" | "interrupted"
    ) && matches!(
        reason,
        "transport_disconnect" | "session_loss" | "interrupted" | "process_exit"
    )
}

/// Allowed `replacement_reason` values for `delegate_to_agent` recovery.
pub const REPLACEMENT_REASON_UNRESUMABLE: &str = "unresumable";
pub const REPLACEMENT_REASON_BUDGET_EXHAUSTED_CONTINUE: &str = "budget_exhausted_continue";
pub const REPLACEMENT_REASON_NOT_SUPPORTED: &str = "not_supported";
pub const REPLACEMENT_REASON_ADMISSION_FAILED: &str = "admission_failed";
pub const REPLACEMENT_REASON_ADMISSION_UNKNOWN: &str = "admission_unknown";

fn replacement_reason_matches_source(
    reason: &str,
    source: &PersistedRun,
    agent_supports_reuse: bool,
    unexpected_continue_exhausted: bool,
    missing_external_session: bool,
) -> bool {
    match reason {
        REPLACEMENT_REASON_UNRESUMABLE => {
            source.error_code.as_deref() == Some("unresumable")
                || source
                    .workspace_path
                    .as_deref()
                    .map(|value| value.trim().is_empty())
                    .unwrap_or(true)
                || source
                    .route_fingerprint
                    .as_deref()
                    .map(|value| value.trim().is_empty())
                    .unwrap_or(true)
                || missing_external_session
        }
        REPLACEMENT_REASON_BUDGET_EXHAUSTED_CONTINUE => unexpected_continue_exhausted,
        REPLACEMENT_REASON_NOT_SUPPORTED => {
            !agent_supports_reuse || source.error_code.as_deref() == Some("not_supported")
        }
        REPLACEMENT_REASON_ADMISSION_FAILED => {
            source.error_code.as_deref() == Some("admission_failed")
                && source.reached_running_at.is_none()
        }
        REPLACEMENT_REASON_ADMISSION_UNKNOWN => {
            source.error_code.as_deref() == Some("admission_unknown")
                && source.reached_running_at.is_none()
        }
        _ => false,
    }
}

impl PersistedRun {
    pub fn to_persisted_task(&self) -> PersistedTask {
        PersistedTask {
            task_id: self.task_id.clone(),
            child_conversation_id: self.child_conversation_id,
            parent_id: Some(self.parent_conversation_id),
            agent_type: self.agent_type,
            status: self.status,
            error_code: self.error_code.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            runtime_stats: self.runtime_stats.clone(),
        }
    }
}

fn is_unique_violation(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("unique constraint failed")
        || lower.contains("unique constraint")
        || lower.contains("sqlite_constraint_unique")
}

fn map_db_err(err: sea_orm::DbErr) -> TaskStoreError {
    let msg = err.to_string();
    if is_transient_sqlite(&msg) {
        TaskStoreError::Transient(msg)
    } else {
        TaskStoreError::Permanent(msg)
    }
}

/// Map unique-index collisions from gen-1 insert to typed wire errors.
fn map_gen1_insert_err(err: sea_orm::DbErr) -> TaskStoreError {
    let msg = err.to_string();
    if is_transient_sqlite(&msg) {
        return TaskStoreError::Transient(msg);
    }
    if !is_unique_violation(&msg) {
        return TaskStoreError::Permanent(msg);
    }
    let lower = msg.to_ascii_lowercase();
    if lower.contains("parent_tool_use") || lower.contains("idx_dtr_parent_tool_use") {
        return TaskStoreError::DuplicateParentTool(msg);
    }
    // Partial unique non-terminal fences (work-unit gen-1 or per-child).
    if lower.contains("work_unit")
        || lower.contains("idx_dtr_one_nonterminal_gen1_work_unit")
        || lower.contains("idx_dtr_one_nonterminal_per_child")
        || lower.contains("child_generation")
        || lower.contains("idx_dtr_child_generation")
    {
        return TaskStoreError::BusyThread(msg);
    }
    // Fallback: any other unique collision on insert is treated as busy.
    TaskStoreError::BusyThread(msg)
}

fn parse_agent_type(s: &str) -> AgentType {
    parse_known_agent_type(s).unwrap_or(AgentType::ClaudeCode)
}

fn parse_known_agent_type(s: &str) -> Option<AgentType> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn run_status_to_task_status(status: &DelegationRunStatus) -> Option<TaskStatus> {
    match status {
        DelegationRunStatus::Reserving | DelegationRunStatus::Running => Some(TaskStatus::Running),
        DelegationRunStatus::Completed => Some(TaskStatus::Completed),
        DelegationRunStatus::Failed => Some(TaskStatus::Failed),
        DelegationRunStatus::Canceled => Some(TaskStatus::Canceled),
    }
}

fn task_status_to_run_status(status: TaskStatus) -> Result<DelegationRunStatus, TaskStoreError> {
    match status {
        TaskStatus::Completed => Ok(DelegationRunStatus::Completed),
        TaskStatus::Failed => Ok(DelegationRunStatus::Failed),
        TaskStatus::Canceled => Ok(DelegationRunStatus::Canceled),
        TaskStatus::Running | TaskStatus::Unknown => Err(TaskStoreError::Permanent(
            "terminal write must not use running/unknown status".into(),
        )),
    }
}

fn task_status_to_delegation_task_status(
    status: TaskStatus,
) -> Result<DelegationTaskStatus, TaskStoreError> {
    match status {
        TaskStatus::Completed => Ok(DelegationTaskStatus::Completed),
        TaskStatus::Failed => Ok(DelegationTaskStatus::Failed),
        TaskStatus::Canceled => Ok(DelegationTaskStatus::Canceled),
        TaskStatus::Running | TaskStatus::Unknown => Err(TaskStoreError::Permanent(
            "terminal write must not use running/unknown status".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Platform recovery budget rails
// ---------------------------------------------------------------------------

/// SeaORM maps SQLite `ON CONFLICT DO NOTHING` with zero inserted rows to
/// `DbErr::RecordNotInserted`. That is the desired lazy-create outcome.
fn map_ensure_insert_err(err: sea_orm::DbErr) -> Result<(), TaskStoreError> {
    match err {
        sea_orm::DbErr::RecordNotInserted => Ok(()),
        other => Err(map_db_err(other)),
    }
}

async fn ensure_lineage_budget(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
) -> Result<(), TaskStoreError> {
    let model = delegation_lineage_budget::ActiveModel {
        lineage_root_task_id: Set(lineage_root_task_id.to_string()),
        unexpected_continue_count: Set(0),
        replacement_count: Set(0),
    };
    match LineageBudget::insert(model)
        .on_conflict(
            OnConflict::column(delegation_lineage_budget::Column::LineageRootTaskId)
                .do_nothing()
                .to_owned(),
        )
        .exec(txn)
        .await
    {
        Ok(_) => Ok(()),
        Err(err) => map_ensure_insert_err(err),
    }
}

async fn ensure_work_unit_budget(
    txn: &DatabaseTransaction,
    parent_conversation_id: i32,
    work_unit_key: &str,
) -> Result<(), TaskStoreError> {
    let model = delegation_work_unit_budget::ActiveModel {
        parent_conversation_id: Set(parent_conversation_id),
        work_unit_key: Set(work_unit_key.to_string()),
        unexpected_continue_count: Set(0),
        replacement_count: Set(0),
    };
    match WorkUnitBudget::insert(model)
        .on_conflict(
            OnConflict::columns([
                delegation_work_unit_budget::Column::ParentConversationId,
                delegation_work_unit_budget::Column::WorkUnitKey,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(txn)
        .await
    {
        Ok(_) => Ok(()),
        Err(err) => map_ensure_insert_err(err),
    }
}

async fn ensure_budget_rows(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
    parent_conversation_id: i32,
    work_unit_key: Option<&str>,
) -> Result<(), TaskStoreError> {
    ensure_lineage_budget(txn, lineage_root_task_id).await?;
    if let Some(key) = work_unit_key {
        ensure_work_unit_budget(txn, parent_conversation_id, key).await?;
    }
    Ok(())
}

async fn preflight_unexpected_continue(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
    parent_conversation_id: i32,
    work_unit_key: Option<&str>,
) -> Result<(), TaskStoreError> {
    ensure_budget_rows(
        txn,
        lineage_root_task_id,
        parent_conversation_id,
        work_unit_key,
    )
    .await?;

    let lineage = LineageBudget::find_by_id(lineage_root_task_id)
        .one(txn)
        .await
        .map_err(map_db_err)?
        .ok_or_else(|| {
            TaskStoreError::Permanent(format!(
                "lineage budget missing after ensure for {lineage_root_task_id}"
            ))
        })?;
    if lineage.unexpected_continue_count >= UNEXPECTED_CONTINUE_LIMIT {
        return Err(TaskStoreError::BudgetExhausted(format!(
            "unexpected_continue lineage rail exhausted for {lineage_root_task_id}"
        )));
    }

    if let Some(key) = work_unit_key {
        let wu = WorkUnitBudget::find_by_id((parent_conversation_id, key.to_string()))
            .one(txn)
            .await
            .map_err(map_db_err)?
            .ok_or_else(|| {
                TaskStoreError::Permanent(format!(
                    "work-unit budget missing after ensure for ({parent_conversation_id}, {key})"
                ))
            })?;
        if wu.unexpected_continue_count >= UNEXPECTED_CONTINUE_LIMIT {
            return Err(TaskStoreError::BudgetExhausted(format!(
                "unexpected_continue work-unit rail exhausted for ({parent_conversation_id}, {key})"
            )));
        }
    }
    Ok(())
}

async fn preflight_replacement(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
    parent_conversation_id: i32,
    work_unit_key: Option<&str>,
) -> Result<(), TaskStoreError> {
    ensure_budget_rows(
        txn,
        lineage_root_task_id,
        parent_conversation_id,
        work_unit_key,
    )
    .await?;

    let lineage = LineageBudget::find_by_id(lineage_root_task_id)
        .one(txn)
        .await
        .map_err(map_db_err)?
        .ok_or_else(|| {
            TaskStoreError::Permanent(format!(
                "lineage budget missing after ensure for {lineage_root_task_id}"
            ))
        })?;
    if lineage.replacement_count >= REPLACEMENT_LIMIT {
        return Err(TaskStoreError::BudgetExhausted(format!(
            "replacement lineage rail exhausted for {lineage_root_task_id}"
        )));
    }

    if let Some(key) = work_unit_key {
        let wu = WorkUnitBudget::find_by_id((parent_conversation_id, key.to_string()))
            .one(txn)
            .await
            .map_err(map_db_err)?
            .ok_or_else(|| {
                TaskStoreError::Permanent(format!(
                    "work-unit budget missing after ensure for ({parent_conversation_id}, {key})"
                ))
            })?;
        if wu.replacement_count >= REPLACEMENT_LIMIT {
            return Err(TaskStoreError::BudgetExhausted(format!(
                "replacement work-unit rail exhausted for ({parent_conversation_id}, {key})"
            )));
        }
    }
    Ok(())
}

// Promote charges use `charge_*_promote` (preserve raw DbErr). Insert-time
// paths only preflight rails; they never charge until promote.

fn model_to_persisted_run(row: delegation_task_run::Model) -> Option<PersistedRun> {
    let status = run_status_to_task_status(&row.status)?;
    let runtime_stats = match decode_persisted_runtime_stats(PersistedRuntimeStatsColumns {
        started_at: row.started_at,
        finished_at: row.finished_at,
        tool_call_count: row.tool_call_count,
        edit_tool_call_count: row.edit_tool_call_count,
        touched_files_json: row.touched_files_json.as_deref(),
        touched_files_truncated: row.touched_files_truncated,
        additions: row.additions,
        deletions: row.deletions,
        line_counts_complete: row.line_counts_complete,
    }) {
        Ok(stats) => stats,
        Err(err) => {
            tracing::warn!(
                task_id = %row.task_id,
                error = ?err,
                "[run_store] failed to decode runtime_stats"
            );
            None
        }
    };
    Some(PersistedRun {
        task_id: row.task_id,
        root_task_id: row.root_task_id,
        previous_task_id: row.previous_task_id,
        generation: row.generation,
        parent_conversation_id: row.parent_conversation_id,
        parent_tool_use_id: row.parent_tool_use_id,
        child_conversation_id: row.child_conversation_id,
        agent_type: parse_agent_type(&row.agent_type),
        status,
        run_status: row.status,
        error_code: row.error_code,
        started_at: row.started_at,
        finished_at: row.finished_at,
        reached_running_at: row.reached_running_at,
        child_connection_id: row.child_connection_id,
        request_fingerprint: row.request_fingerprint,
        task_preview: row.task_preview,
        admission_class: row.admission_class,
        lineage_root_task_id: row.lineage_root_task_id,
        work_unit_key: row.work_unit_key,
        history_only: row.history_only,
        route_fingerprint: row.route_fingerprint,
        workspace_path: row.workspace_path,
        launch_snapshot_version: row.launch_snapshot_version,
        mode_id: row.mode_id,
        config_values_json: row.config_values_json,
        profile_id: row.profile_id,
        runtime_stats,
    })
}

/// Insert one `reserving` row using an already-open transaction. Admission
/// callers use this with their ownership/replacement checks in the same
/// transaction; the public wrapper below keeps the simple insertion API for
/// existing lifecycle callers.
async fn insert_reserving_txn(
    txn: &DatabaseTransaction,
    insert: &ReservingRunInsert,
) -> Result<(), TaskStoreError> {
    if insert.generation > MAX_GENERATION {
        return Err(TaskStoreError::BudgetExhausted(format!(
            "generation {} exceeds hard ceiling {}",
            insert.generation, MAX_GENERATION
        )));
    }

    match insert.admission_class {
        AdmissionClass::UnexpectedContinue => {
            preflight_unexpected_continue(
                txn,
                &insert.lineage_root_task_id,
                insert.parent_conversation_id,
                insert.work_unit_key.as_deref(),
            )
            .await?;
        }
        AdmissionClass::Replacement => {
            preflight_replacement(
                txn,
                &insert.lineage_root_task_id,
                insert.parent_conversation_id,
                insert.work_unit_key.as_deref(),
            )
            .await?;
        }
        AdmissionClass::NormalRevision => {}
    }

    let now = Utc::now();
    let model = delegation_task_run::ActiveModel {
        task_id: Set(insert.task_id.clone()),
        root_task_id: Set(insert.root_task_id.clone()),
        previous_task_id: Set(insert.previous_task_id.clone()),
        generation: Set(insert.generation),
        parent_conversation_id: Set(insert.parent_conversation_id),
        parent_tool_use_id: Set(insert.parent_tool_use_id.clone()),
        child_conversation_id: Set(insert.child_conversation_id),
        agent_type: Set(insert.agent_type.clone()),
        profile_id: Set(insert.profile_id.clone()),
        workspace_path: Set(insert.workspace_path.clone()),
        route_fingerprint: Set(insert.route_fingerprint.clone()),
        launch_snapshot_version: Set(insert.launch_snapshot_version.clone()),
        mode_id: Set(insert.mode_id.clone()),
        config_values_json: Set(insert.config_values_json.clone()),
        task_preview: Set(insert.task_preview.clone()),
        request_fingerprint: Set(insert.request_fingerprint.clone()),
        admission_class: Set(insert.admission_class.clone()),
        reached_running_at: Set(None),
        lineage_root_task_id: Set(insert.lineage_root_task_id.clone()),
        work_unit_key: Set(insert.work_unit_key.clone()),
        legacy_parent_tool_use_id: Set(None),
        history_only: Set(insert.history_only),
        status: Set(DelegationRunStatus::Reserving),
        error_code: Set(None),
        termination_audit_json: Set(None),
        started_at: Set(insert.started_at.or(Some(now))),
        finished_at: Set(None),
        tool_call_count: Set(Some(0)),
        edit_tool_call_count: Set(Some(0)),
        touched_files_json: Set(Some("[]".into())),
        touched_files_truncated: Set(Some(false)),
        additions: Set(None),
        deletions: Set(None),
        line_counts_complete: Set(Some(false)),
        card_summary_json: Set(None),
        child_turn_anchor: Set(None),
        child_connection_id: Set(None),
        replaced_task_id: Set(insert.replaced_task_id.clone()),
        replacement_reason: Set(insert.replacement_reason.clone()),
        created_at: Set(now),
        updated_at: Set(now),
    };
    model.insert(txn).await.map_err(map_gen1_insert_err)?;
    Ok(())
}

async fn work_unit_has_reached_running_txn(
    txn: &DatabaseTransaction,
    parent_conversation_id: i32,
    work_unit_key: &str,
) -> Result<bool, TaskStoreError> {
    let hit = DelegationTaskRun::find()
        .filter(delegation_task_run::Column::ParentConversationId.eq(parent_conversation_id))
        .filter(delegation_task_run::Column::WorkUnitKey.eq(work_unit_key))
        .filter(delegation_task_run::Column::ReachedRunningAt.is_not_null())
        .limit(1)
        .all(txn)
        .await
        .map_err(map_db_err)?;
    Ok(!hit.is_empty())
}

/// True when a non-terminal (reserving/running) claim already occupies the
/// work unit. Concurrent gen-1 losers map to `busy_thread` rather than
/// `invalid_replacement` while the winner is still in flight.
async fn work_unit_has_nonterminal_txn(
    txn: &DatabaseTransaction,
    parent_conversation_id: i32,
    work_unit_key: &str,
) -> Result<bool, TaskStoreError> {
    let hit = DelegationTaskRun::find()
        .filter(delegation_task_run::Column::ParentConversationId.eq(parent_conversation_id))
        .filter(delegation_task_run::Column::WorkUnitKey.eq(work_unit_key))
        .filter(
            delegation_task_run::Column::Status
                .is_in([DelegationRunStatus::Reserving, DelegationRunStatus::Running]),
        )
        .limit(1)
        .all(txn)
        .await
        .map_err(map_db_err)?;
    Ok(!hit.is_empty())
}

async fn is_latest_run_on_child_txn(
    txn: &DatabaseTransaction,
    child_conversation_id: i32,
    task_id: &str,
) -> Result<bool, TaskStoreError> {
    use sea_orm::QueryOrder;

    let latest = DelegationTaskRun::find()
        .filter(delegation_task_run::Column::ChildConversationId.eq(child_conversation_id))
        .order_by_desc(delegation_task_run::Column::Generation)
        .one(txn)
        .await
        .map_err(map_db_err)?;
    Ok(latest.map(|row| row.task_id == task_id).unwrap_or(false))
}

/// Acquire SQLite's writer reservation before reading continuation eligibility.
/// The assignment is intentionally a no-op; it serializes a continuation with
/// replacement admission without changing the durable row.
async fn lock_continue_admission_txn(
    txn: &DatabaseTransaction,
    task_id: &str,
) -> Result<(), TaskStoreError> {
    DelegationTaskRun::update_many()
        .col_expr(
            delegation_task_run::Column::UpdatedAt,
            Expr::col(delegation_task_run::Column::UpdatedAt).into(),
        )
        .filter(delegation_task_run::Column::TaskId.eq(task_id))
        .exec(txn)
        .await
        .map_err(map_db_err)?;
    Ok(())
}

async fn build_continue_eligibility_txn(
    txn: &DatabaseTransaction,
    target: &PersistedRun,
) -> Result<ContinueEligibility, TaskStoreError> {
    let has_active_run = !DelegationTaskRun::find()
        .filter(delegation_task_run::Column::ChildConversationId.eq(target.child_conversation_id))
        .filter(
            delegation_task_run::Column::Status
                .is_in([DelegationRunStatus::Reserving, DelegationRunStatus::Running]),
        )
        .limit(1)
        .all(txn)
        .await
        .map_err(map_db_err)?
        .is_empty();
    let is_latest =
        is_latest_run_on_child_txn(txn, target.child_conversation_id, &target.task_id).await?;

    let child_task_ids: Vec<String> = DelegationTaskRun::find()
        .filter(delegation_task_run::Column::ChildConversationId.eq(target.child_conversation_id))
        .all(txn)
        .await
        .map_err(map_db_err)?
        .into_iter()
        .map(|row| row.task_id)
        .collect();
    let child_superseded = if child_task_ids.is_empty() {
        false
    } else {
        !DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ReplacedTaskId.is_in(child_task_ids))
            .limit(1)
            .all(txn)
            .await
            .map_err(map_db_err)?
            .is_empty()
    };

    let child = conversation::Entity::find_by_id(target.child_conversation_id)
        .one(txn)
        .await
        .map_err(map_db_err)?;
    let parent = conversation::Entity::find_by_id(target.parent_conversation_id)
        .one(txn)
        .await
        .map_err(map_db_err)?;
    let target_row = DelegationTaskRun::find_by_id(&target.task_id)
        .one(txn)
        .await
        .map_err(map_db_err)?;
    let (child_ownership_valid, agent_type_matches, external_id_present, termination_audit_json) =
        match (child, parent, target_row) {
            (Some(child), Some(parent), Some(target_row))
                if child.deleted_at.is_none()
                    && parent.deleted_at.is_none()
                    && child.parent_id == Some(target.parent_conversation_id) =>
            {
                let run_agent = parse_known_agent_type(&target_row.agent_type);
                let child_agent = parse_known_agent_type(&child.agent_type);
                let agent_type_matches = run_agent
                    .is_some_and(|run_agent| run_agent == target.agent_type)
                    && child_agent.is_some_and(|child_agent| child_agent == target.agent_type)
                    && target_row.agent_type == child.agent_type;
                let external_id_present = child
                    .external_id
                    .as_deref()
                    .map(|value| !value.trim().is_empty())
                    .unwrap_or(false);
                (
                    true,
                    agent_type_matches,
                    external_id_present,
                    target_row.termination_audit_json,
                )
            }
            _ => (false, false, false, None),
        };
    let snapshot_complete = launch_snapshot_from_run(target)
        .map(|snapshot| snapshot_is_complete(&snapshot))
        .unwrap_or(false);

    Ok(ContinueEligibility {
        history_only: target.history_only,
        is_latest,
        has_active_run,
        child_superseded,
        child_ownership_valid,
        agent_type_matches,
        snapshot_complete,
        external_id_present,
        run_status: target.run_status.clone(),
        error_code: target.error_code.clone(),
        admission_class: target.admission_class.clone(),
        reached_running: target.reached_running_at.is_some(),
        termination_audit_json,
    })
}

async fn unexpected_continue_at_limit_txn(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
) -> Result<bool, TaskStoreError> {
    let row = LineageBudget::find_by_id(lineage_root_task_id)
        .one(txn)
        .await
        .map_err(map_db_err)?;
    Ok(row
        .map(|budget| budget.unexpected_continue_count >= UNEXPECTED_CONTINUE_LIMIT)
        .unwrap_or(false))
}

async fn work_unit_unexpected_continue_at_limit_txn(
    txn: &DatabaseTransaction,
    parent_conversation_id: i32,
    work_unit_key: Option<&str>,
) -> Result<bool, TaskStoreError> {
    let Some(work_unit_key) = work_unit_key else {
        return Ok(false);
    };
    let row = WorkUnitBudget::find_by_id((parent_conversation_id, work_unit_key.to_string()))
        .one(txn)
        .await
        .map_err(map_db_err)?;
    Ok(row
        .map(|budget| budget.unexpected_continue_count >= UNEXPECTED_CONTINUE_LIMIT)
        .unwrap_or(false))
}

/// Replacement checks and the reserving insert share one transaction. The
/// replacement counter remains uncharged until `promote_running`, but these
/// checks cannot race a concurrently admitted replacement into a new lineage.
async fn validate_replacement_insert_txn(
    txn: &DatabaseTransaction,
    insert: &ReservingRunInsert,
) -> Result<(), TaskStoreError> {
    let replaced_id = insert
        .replaced_task_id
        .as_deref()
        .ok_or_else(|| TaskStoreError::InvalidReplacement("missing replaces_task_id".into()))?;
    let reason = insert
        .replacement_reason
        .as_deref()
        .ok_or_else(|| TaskStoreError::InvalidReplacement("missing replacement_reason".into()))?;
    if insert.admission_class != AdmissionClass::Replacement || insert.generation != 1 {
        return Err(TaskStoreError::InvalidReplacement(
            "replacement must create a generation-1 replacement run".into(),
        ));
    }
    if insert.root_task_id != insert.task_id {
        return Err(TaskStoreError::InvalidReplacement(
            "replacement root_task_id must be its new task_id".into(),
        ));
    }

    let source_row = DelegationTaskRun::find_by_id(replaced_id)
        .one(txn)
        .await
        .map_err(map_db_err)?
        .ok_or_else(|| TaskStoreError::NotFound(replaced_id.to_string()))?;

    // 1. Direct-parent ownership.
    if source_row.parent_conversation_id != insert.parent_conversation_id {
        // Preserve the same redacted not-found behavior as an absent source.
        return Err(TaskStoreError::NotFound(replaced_id.to_string()));
    }
    let parent =
        crate::db::entities::conversation::Entity::find_by_id(source_row.parent_conversation_id)
            .one(txn)
            .await
            .map_err(map_db_err)?;
    if !parent.is_some_and(|parent| parent.deleted_at.is_none()) {
        return Err(TaskStoreError::NotFound(replaced_id.to_string()));
    }
    let source_agent_type = parse_known_agent_type(&source_row.agent_type).ok_or_else(|| {
        TaskStoreError::InvalidReplacement("replacement source has unknown agent_type".into())
    })?;
    let source = model_to_persisted_run(source_row).ok_or_else(|| {
        TaskStoreError::InvalidReplacement("replacement source is unreadable".into())
    })?;
    // 2. Same role/profile.
    let insert_agent_type = parse_known_agent_type(&insert.agent_type).ok_or_else(|| {
        TaskStoreError::InvalidReplacement("replacement agent_type is unknown".into())
    })?;
    if source_agent_type != insert_agent_type {
        return Err(TaskStoreError::InvalidReplacement(
            "replacement agent_type mismatch".into(),
        ));
    }
    if (source.profile_id.is_some() || insert.profile_id.is_some())
        && source.profile_id != insert.profile_id
    {
        return Err(TaskStoreError::InvalidReplacement(
            "replacement profile_id mismatch".into(),
        ));
    }
    // 3. Same normalized workspace and orchestration key.
    let source_workspace = source.workspace_path.as_deref().unwrap_or("");
    let insert_workspace = insert.workspace_path.as_deref().unwrap_or("");
    if !crate::parsers::path_eq_for_matching(source_workspace, insert_workspace) {
        return Err(TaskStoreError::InvalidReplacement(
            "replacement workspace mismatch".into(),
        ));
    }
    if source.work_unit_key != insert.work_unit_key {
        return Err(TaskStoreError::InvalidReplacement(
            "replacement work_unit_key mismatch".into(),
        ));
    }
    // 4. Terminal and latest on the source child.
    if !matches!(
        source.run_status,
        DelegationRunStatus::Completed
            | DelegationRunStatus::Failed
            | DelegationRunStatus::Canceled
    ) || !is_latest_run_on_child_txn(txn, source.child_conversation_id, &source.task_id).await?
    {
        return Err(TaskStoreError::InvalidReplacement(
            "replaced run is not the latest terminal run on its child".into(),
        ));
    }
    // 5. Durable reason eligibility.
    let unexpected_continue_exhausted =
        unexpected_continue_at_limit_txn(txn, &source.lineage_root_task_id).await?
            || work_unit_unexpected_continue_at_limit_txn(
                txn,
                source.parent_conversation_id,
                source.work_unit_key.as_deref(),
            )
            .await?;
    let missing_external_session =
        crate::db::entities::conversation::Entity::find_by_id(source.child_conversation_id)
            .one(txn)
            .await
            .map_err(map_db_err)?
            .and_then(|child| child.external_id)
            .map(|external_id| external_id.trim().is_empty())
            .unwrap_or(true);
    let agent_supports_reuse =
        crate::acp::delegation::capability::agent_supports_session_reuse(source.agent_type);
    if !replacement_reason_matches_source(
        reason,
        &source,
        agent_supports_reuse,
        unexpected_continue_exhausted,
        missing_external_session,
    ) {
        return Err(TaskStoreError::InvalidReplacement(format!(
            "replacement_reason {reason} does not match durable state"
        )));
    }
    // 6. Replacement shares the source lineage. Counter-room preflight and
    // 7. insert occur in `insert_reserving_txn` immediately after this check.
    if insert.lineage_root_task_id != source.lineage_root_task_id {
        return Err(TaskStoreError::InvalidReplacement(
            "lineage_root_task_id must inherit replaced run".into(),
        ));
    }
    Ok(())
}

/// SQLite-backed store for `delegation_task_runs` + conversation projection fence.
pub struct RunStore {
    db: Arc<AppDatabase>,
    /// Test-only: one-shot mid-settle gate so parent-end can race a producer
    /// already parked in broker `settling` during CAS.
    #[cfg(any(test, feature = "test-utils"))]
    settle_gate: tokio::sync::Mutex<Option<RunStoreSettleGate>>,
    /// Test-only: holds continuation admission after eligibility and before
    /// reserving insertion, so replacement races stay reproducible.
    #[cfg(any(test, feature = "test-utils"))]
    continue_admission_gate: tokio::sync::Mutex<Option<RunStoreContinueAdmissionGate>>,
    /// Test-only: FIFO faults applied inside each promote attempt (after claim
    /// / after budget), not before the transaction opens.
    #[cfg(any(test, feature = "test-utils"))]
    promote_faults: Arc<tokio::sync::Mutex<VecDeque<PromoteTestFault>>>,
    /// Test-only: one-shot gate after the write-first claim, so a concurrent
    /// writer can interleave while the promote transaction still holds the
    /// SQLite writer lock.
    #[cfg(any(test, feature = "test-utils"))]
    promote_claim_gate: tokio::sync::Mutex<Option<RunStoreSettleGate>>,
}

/// Bound for test-only RunStore settle / continue-admission gate release waits.
/// Unreleased or dropped release senders must fail fast — never hang CI.
#[cfg(any(test, feature = "test-utils"))]
pub(crate) const TEST_RUN_STORE_GATE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(5);

#[cfg(any(test, feature = "test-utils"))]
struct RunStoreSettleGate {
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: Option<tokio::sync::oneshot::Receiver<()>>,
}

#[cfg(any(test, feature = "test-utils"))]
struct RunStoreContinueAdmissionGate {
    entered: Option<tokio::sync::oneshot::Sender<()>>,
    release: Option<tokio::sync::oneshot::Receiver<()>>,
}

impl RunStore {
    pub fn new(db: Arc<AppDatabase>) -> Self {
        Self {
            db,
            #[cfg(any(test, feature = "test-utils"))]
            settle_gate: tokio::sync::Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            continue_admission_gate: tokio::sync::Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            promote_faults: Arc::new(tokio::sync::Mutex::new(VecDeque::new())),
            #[cfg(any(test, feature = "test-utils"))]
            promote_claim_gate: tokio::sync::Mutex::new(None),
        }
    }

    pub fn db(&self) -> &Arc<AppDatabase> {
        &self.db
    }

    /// Test-only settle race gate.
    ///
    /// - [`Self::settle_terminal`]: signals `entered` then waits on `release`
    ///   before applying the durable CAS (entry of settle path).
    /// - [`Self::settle_pre_admission_failure_if_owned`]: signals after a
    ///   still-`Reserving` own/unbound snapshot (would settle) and waits
    ///   **before** the ownership-CAS write transaction (no lock held).
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn install_settle_gate(
        &self,
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) {
        *self.settle_gate.lock().await = Some(RunStoreSettleGate {
            entered: Some(entered),
            release: Some(release),
        });
    }

    /// Test-only: next [`Self::admit_continue_reserving`] signals after it has
    /// evaluated continuability, then waits before reserving its child run.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn install_continue_admission_gate(
        &self,
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) {
        *self.continue_admission_gate.lock().await = Some(RunStoreContinueAdmissionGate {
            entered: Some(entered),
            release: Some(release),
        });
    }

    /// Test-only: queue promote faults applied FIFO **inside** each attempt
    /// (after claim / after budget), so retries re-enter a full transaction.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn push_promote_faults(&self, faults: impl IntoIterator<Item = PromoteTestFault>) {
        self.promote_faults.lock().await.extend(faults);
    }

    /// Test-only: next promote attempt signals after the write-first claim,
    /// then waits on `release` before budget charge / status update.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn install_promote_claim_gate(
        &self,
        entered: tokio::sync::oneshot::Sender<()>,
        release: tokio::sync::oneshot::Receiver<()>,
    ) {
        *self.promote_claim_gate.lock().await = Some(RunStoreSettleGate {
            entered: Some(entered),
            release: Some(release),
        });
    }

    #[cfg(any(test, feature = "test-utils"))]
    async fn take_pre_txn_promote_fault(&self) -> Option<PromoteTestFault> {
        let mut q = self.promote_faults.lock().await;
        match q.front() {
            Some(PromoteTestFault::AmbiguousPermanent { .. }) => q.pop_front(),
            _ => None,
        }
    }

    /// Insert a durable `reserving` claim before ACP spawn / resume.
    ///
    /// Preflights platform recovery rails (generation ceiling + counter room)
    /// for `unexpected_continue` / `replacement`. Counters are **not** charged
    /// here — only at [`Self::promote_running`].
    ///
    /// Unique collisions map to typed errors: parent-tool →
    /// [`TaskStoreError::DuplicateParentTool`]; non-terminal work-unit / child
    /// fences → [`TaskStoreError::BusyThread`].
    pub async fn insert_reserving(&self, insert: ReservingRunInsert) -> Result<(), TaskStoreError> {
        let outcome = self
            .db
            .conn
            .transaction::<_, (), TaskStoreError>(|txn| {
                let insert = insert.clone();
                Box::pin(async move { insert_reserving_txn(txn, &insert).await })
            })
            .await;

        match outcome {
            Ok(()) => Ok(()),
            Err(sea_orm::TransactionError::Connection(e)) => Err(map_gen1_insert_err(e)),
            Err(sea_orm::TransactionError::Transaction(e)) => Err(e),
        }
    }

    /// Load the run bound to `(parent_conversation_id, parent_tool_use_id)`.
    pub async fn load_by_parent_tool_use(
        &self,
        parent_conversation_id: i32,
        parent_tool_use_id: &str,
    ) -> Result<Option<PersistedRun>, TaskStoreError> {
        let row = DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ParentConversationId.eq(parent_conversation_id))
            .filter(delegation_task_run::Column::ParentToolUseId.eq(parent_tool_use_id))
            .one(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        Ok(row.and_then(model_to_persisted_run))
    }

    /// Abandon a pure pre-spawn gen-1 claim so provisional compensation can
    /// still terminalize + soft-delete the unused child shell (no durable
    /// cancelled run left behind).
    ///
    /// Matches either:
    /// 1. still-`reserving`, never reached running, unbound connection; or
    /// 2. just-settled `canceled` of the same pre-spawn shape when the child
    ///    never acquired an external session (parent-end durable sweep raced
    ///    admit commit before the inflight owner could abandon).
    ///
    /// Returns `true` when a matching claim row was removed.
    pub async fn abandon_reserving_claim(&self, task_id: &str) -> Result<bool, TaskStoreError> {
        let result = DelegationTaskRun::delete_many()
            .filter(delegation_task_run::Column::TaskId.eq(task_id))
            .filter(delegation_task_run::Column::Status.eq(DelegationRunStatus::Reserving))
            .filter(delegation_task_run::Column::ReachedRunningAt.is_null())
            .filter(delegation_task_run::Column::ChildConnectionId.is_null())
            .exec(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        if result.rows_affected > 0 {
            return Ok(true);
        }

        // Parent-end durable sweep may have first-written the pure reserving
        // claim to canceled between admit commit and this abandon. Reclaim
        // only provisional-like terminals: never running, unbound, no external
        // session on the child.
        let outcome = self
            .db
            .conn
            .transaction::<_, bool, TaskStoreError>(|txn| {
                let task_id = task_id.to_string();
                Box::pin(async move {
                    let row = DelegationTaskRun::find_by_id(&task_id)
                        .one(txn)
                        .await
                        .map_err(map_db_err)?;
                    let Some(row) = row else {
                        return Ok(false);
                    };
                    if row.status != DelegationRunStatus::Canceled
                        || row.reached_running_at.is_some()
                        || row.child_connection_id.is_some()
                    {
                        return Ok(false);
                    }
                    let child = conversation::Entity::find_by_id(row.child_conversation_id)
                        .one(txn)
                        .await
                        .map_err(map_db_err)?;
                    let Some(child) = child else {
                        return Ok(false);
                    };
                    if child
                        .external_id
                        .as_deref()
                        .is_some_and(|s| !s.trim().is_empty())
                    {
                        return Ok(false);
                    }
                    let deleted = DelegationTaskRun::delete_many()
                        .filter(delegation_task_run::Column::TaskId.eq(&task_id))
                        .filter(
                            delegation_task_run::Column::Status
                                .eq(DelegationRunStatus::Canceled),
                        )
                        .filter(delegation_task_run::Column::ReachedRunningAt.is_null())
                        .filter(delegation_task_run::Column::ChildConnectionId.is_null())
                        .exec(txn)
                        .await
                        .map_err(map_db_err)?;
                    Ok(deleted.rows_affected > 0)
                })
            })
            .await;

        match outcome {
            Ok(reclaimed) => Ok(reclaimed),
            Err(sea_orm::TransactionError::Connection(e)) => Err(map_db_err(e)),
            Err(sea_orm::TransactionError::Transaction(e)) => Err(e),
        }
    }

    /// Gen-1 durable reserve with parent-tool fingerprint idempotency.
    ///
    /// Precedence (design): matching `request_fingerprint` → idempotent return
    /// of the existing run; mismatch or legacy-missing fingerprint →
    /// `duplicate_parent_tool`; concurrent non-terminal work-unit / child fence
    /// → `busy_thread`.
    pub async fn admit_gen1_reserving(
        &self,
        insert: ReservingRunInsert,
    ) -> Result<Gen1AdmitOutcome, TaskStoreError> {
        let outcome = self
            .db
            .conn
            .transaction::<_, Option<PersistedRun>, TaskStoreError>(|txn| {
                let insert = insert.clone();
                Box::pin(async move {
                    if let Some(tool_id) = insert.parent_tool_use_id.as_deref() {
                        let existing = DelegationTaskRun::find()
                            .filter(
                                delegation_task_run::Column::ParentConversationId
                                    .eq(insert.parent_conversation_id),
                            )
                            .filter(delegation_task_run::Column::ParentToolUseId.eq(tool_id))
                            .one(txn)
                            .await
                            .map_err(map_db_err)?
                            .and_then(model_to_persisted_run);
                        if let Some(existing) = existing {
                            return match (
                                existing.request_fingerprint.as_deref(),
                                insert.request_fingerprint.as_deref(),
                            ) {
                                (Some(a), Some(b)) if a == b => Ok(Some(existing)),
                                _ => Err(TaskStoreError::DuplicateParentTool(format!(
                                    "parent_tool_use_id {tool_id} already bound under parent {}",
                                    insert.parent_conversation_id
                                ))),
                            };
                        }
                    }

                    match (
                        insert.replaced_task_id.is_some(),
                        insert.replacement_reason.is_some(),
                    ) {
                        (true, true) => validate_replacement_insert_txn(txn, &insert).await?,
                        (true, false) | (false, true) => {
                            return Err(TaskStoreError::InvalidReplacement(
                                "replaces_task_id and replacement_reason must be paired".into(),
                            ));
                        }
                        (false, false) if insert.generation == 1 => {
                            if let Some(key) = insert.work_unit_key.as_deref() {
                                // Prefer busy_thread while a peer claim is still
                                // non-terminal (reserving or running). Only after
                                // lineage has terminalized does a bare gen-1
                                // re-dispatch require the replacement protocol.
                                if work_unit_has_nonterminal_txn(
                                    txn,
                                    insert.parent_conversation_id,
                                    key,
                                )
                                .await?
                                {
                                    return Err(TaskStoreError::BusyThread(format!(
                                        "work_unit_key {key} already has a non-terminal claim under parent {}",
                                        insert.parent_conversation_id
                                    )));
                                }
                                if work_unit_has_reached_running_txn(
                                    txn,
                                    insert.parent_conversation_id,
                                    key,
                                )
                                .await?
                                {
                                    return Err(TaskStoreError::InvalidReplacement(format!(
                                        "work_unit_key {key} already has established lineage under parent {}",
                                        insert.parent_conversation_id
                                    )));
                                }
                            }
                        }
                        (false, false) => {}
                    }

                    insert_reserving_txn(txn, &insert).await?;
                    Ok(None)
                })
            })
            .await;

        let result = match outcome {
            Ok(existing) => Ok(existing),
            Err(sea_orm::TransactionError::Connection(e)) => Err(map_gen1_insert_err(e)),
            Err(sea_orm::TransactionError::Transaction(e)) => Err(e),
        };
        match result {
            Ok(Some(existing)) => Ok(Gen1AdmitOutcome::Idempotent(existing)),
            Ok(None) => {
                let run = self
                    .load_by_task_id(&insert.task_id)
                    .await?
                    .ok_or_else(|| TaskStoreError::NotFound(insert.task_id.clone()))?;
                Ok(Gen1AdmitOutcome::Created(run))
            }
            Err(TaskStoreError::DuplicateParentTool(_)) => {
                // A concurrent inserter won the partial unique index after our
                // transaction snapshot. Re-load and apply fingerprint rules.
                let tool_id = insert
                    .parent_tool_use_id
                    .as_deref()
                    .ok_or_else(|| TaskStoreError::DuplicateParentTool("parent tool".into()))?;
                let existing = self
                    .load_by_parent_tool_use(insert.parent_conversation_id, tool_id)
                    .await?
                    .ok_or_else(|| {
                        TaskStoreError::DuplicateParentTool(format!(
                            "parent_tool_use_id {tool_id} conflict without row"
                        ))
                    })?;
                match (
                    existing.request_fingerprint.as_deref(),
                    insert.request_fingerprint.as_deref(),
                ) {
                    (Some(a), Some(b)) if a == b => Ok(Gen1AdmitOutcome::Idempotent(existing)),
                    _ => Err(TaskStoreError::DuplicateParentTool(format!(
                        "parent_tool_use_id {tool_id} already bound under parent {}",
                        insert.parent_conversation_id
                    ))),
                }
            }
            Err(e) => Err(e),
        }
    }

    async fn is_latest_run_on_child(
        &self,
        child_conversation_id: i32,
        task_id: &str,
    ) -> Result<bool, TaskStoreError> {
        use sea_orm::QueryOrder;
        let latest = DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ChildConversationId.eq(child_conversation_id))
            .order_by_desc(delegation_task_run::Column::Generation)
            .one(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        Ok(latest.map(|r| r.task_id == task_id).unwrap_or(false))
    }

    /// Durable continue reserve: parent-tool fingerprint, continuability, and
    /// reserving insertion share one writer transaction.
    pub async fn admit_continue_reserving(
        &self,
        admission: ContinueRunAdmission,
    ) -> Result<ContinueAdmitOutcome, TaskStoreError> {
        #[cfg(any(test, feature = "test-utils"))]
        let mut continue_admission_gate = self.continue_admission_gate.lock().await.take();

        let outcome = self
            .db
            .conn
            .transaction::<_, Option<PersistedRun>, TaskStoreError>(|txn| {
                let admission = admission.clone();
                Box::pin(async move {
                    // SQLite read transactions cannot safely upgrade after a
                    // concurrent replacement writes. Take the writer reservation
                    // before every eligibility read so replacement and continue
                    // serialize around the source child.
                    lock_continue_admission_txn(txn, &admission.target_task_id).await?;

                    // Parent-tool exact-duplicate precedes busy/stale.
                    if !admission.parent_tool_use_id.is_empty() {
                        let existing = DelegationTaskRun::find()
                            .filter(
                                delegation_task_run::Column::ParentConversationId
                                    .eq(admission.parent_conversation_id),
                            )
                            .filter(
                                delegation_task_run::Column::ParentToolUseId
                                    .eq(&admission.parent_tool_use_id),
                            )
                            .one(txn)
                            .await
                            .map_err(map_db_err)?
                            .and_then(model_to_persisted_run);
                        if let Some(existing) = existing {
                            return match existing.request_fingerprint.as_deref() {
                                Some(prev) if prev == admission.request_fingerprint => {
                                    Ok(Some(existing))
                                }
                                _ => Err(TaskStoreError::DuplicateParentTool(format!(
                                    "parent_tool_use_id {} already bound under parent {}",
                                    admission.parent_tool_use_id, admission.parent_conversation_id
                                ))),
                            };
                        }
                    }

                    let target = DelegationTaskRun::find_by_id(&admission.target_task_id)
                        .one(txn)
                        .await
                        .map_err(map_db_err)?
                        .and_then(model_to_persisted_run)
                        .ok_or_else(|| {
                            TaskStoreError::NotFound(admission.target_task_id.clone())
                        })?;
                    if target.parent_conversation_id != admission.parent_conversation_id {
                        // Cross-parent: do not reveal existence.
                        return Err(TaskStoreError::NotFound(admission.target_task_id.clone()));
                    }

                    // Precedence: not_found → fingerprint → (capability outside
                    // store) → busy → stale → not_continuable → budget / insert.
                    let eligibility = build_continue_eligibility_txn(txn, &target).await?;
                    let admission_class = match decide_continue_eligibility(&eligibility) {
                        ContinueDecision::BusyThread => Err(TaskStoreError::BusyThread(format!(
                            "child of {} has active run",
                            admission.target_task_id
                        )))?,
                        ContinueDecision::StaleTaskId => Err(TaskStoreError::StaleTaskId(
                            admission.target_task_id.clone(),
                        ))?,
                        ContinueDecision::NotContinuable => Err(TaskStoreError::NotContinuable(
                            admission.target_task_id.clone(),
                        ))?,
                        ContinueDecision::Admit(admission_class) => admission_class,
                    };
                    if let Some(requested_key) = admission.work_unit_key.as_deref() {
                        if target.work_unit_key.as_deref() != Some(requested_key) {
                            return Err(TaskStoreError::NotContinuable(format!(
                                "work_unit_key does not match continued thread {}",
                                admission.target_task_id
                            )));
                        }
                    }

                    #[cfg(any(test, feature = "test-utils"))]
                    {
                        if let Some(mut gate) = continue_admission_gate.take() {
                            if let Some(tx) = gate.entered.take() {
                                let _ = tx.send(());
                            }
                            if let Some(rx) = gate.release.take() {
                                tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, rx)
                                    .await
                                    .map_err(|_| {
                                        TaskStoreError::Permanent(
                                            "test run_store continue_admission gate timed out"
                                                .into(),
                                        )
                                    })?
                                    .map_err(|_| {
                                        TaskStoreError::Permanent(
                                            "test run_store continue_admission gate release dropped"
                                                .into(),
                                        )
                                    })?;
                            }
                        }
                    }

                    let insert = ReservingRunInsert {
                        task_id: admission.task_id.clone(),
                        root_task_id: target.root_task_id.clone(),
                        previous_task_id: Some(target.task_id.clone()),
                        generation: target.generation + 1,
                        parent_conversation_id: admission.parent_conversation_id,
                        parent_tool_use_id: (!admission.parent_tool_use_id.is_empty())
                            .then(|| admission.parent_tool_use_id.clone()),
                        child_conversation_id: target.child_conversation_id,
                        agent_type: serde_json::to_value(target.agent_type)
                            .ok()
                            .and_then(|value| value.as_str().map(String::from))
                            .unwrap_or_else(|| {
                                format!("{:?}", target.agent_type).to_ascii_lowercase()
                            }),
                        profile_id: target.profile_id.clone(),
                        workspace_path: target.workspace_path.clone(),
                        route_fingerprint: target.route_fingerprint.clone(),
                        launch_snapshot_version: target.launch_snapshot_version.clone(),
                        mode_id: target.mode_id.clone(),
                        config_values_json: target.config_values_json.clone(),
                        task_preview: Some(admission.task_preview),
                        request_fingerprint: Some(admission.request_fingerprint),
                        admission_class,
                        lineage_root_task_id: target.lineage_root_task_id.clone(),
                        work_unit_key: admission
                            .work_unit_key
                            .or_else(|| target.work_unit_key.clone()),
                        history_only: false,
                        replaced_task_id: None,
                        replacement_reason: None,
                        started_at: Some(Utc::now()),
                    };
                    insert_reserving_txn(txn, &insert).await?;
                    Ok(None)
                })
            })
            .await;

        let result = match outcome {
            Ok(existing) => Ok(existing),
            Err(sea_orm::TransactionError::Connection(err)) => Err(map_gen1_insert_err(err)),
            Err(sea_orm::TransactionError::Transaction(err)) => Err(err),
        };
        match result {
            Ok(Some(existing)) => Ok(ContinueAdmitOutcome::Idempotent(existing)),
            Ok(None) => {
                let run = self
                    .load_by_task_id(&admission.task_id)
                    .await?
                    .ok_or_else(|| TaskStoreError::NotFound(admission.task_id.clone()))?;
                Ok(ContinueAdmitOutcome::Created(run))
            }
            Err(TaskStoreError::DuplicateParentTool(_)) => {
                let existing = self
                    .load_by_parent_tool_use(
                        admission.parent_conversation_id,
                        &admission.parent_tool_use_id,
                    )
                    .await?
                    .ok_or_else(|| {
                        TaskStoreError::DuplicateParentTool(
                            "parent tool conflict without row".into(),
                        )
                    })?;
                match existing.request_fingerprint.as_deref() {
                    Some(prev) if prev == admission.request_fingerprint => {
                        Ok(ContinueAdmitOutcome::Idempotent(existing))
                    }
                    _ => Err(TaskStoreError::DuplicateParentTool(
                        admission.parent_tool_use_id,
                    )),
                }
            }
            Err(err) => Err(err),
        }
    }

    /// Build decision-table inputs for a loaded target run.
    pub async fn build_continue_eligibility(
        &self,
        target: &PersistedRun,
    ) -> Result<ContinueEligibility, TaskStoreError> {
        let has_active = self
            .child_has_non_terminal(target.child_conversation_id)
            .await?;
        let is_latest = self
            .is_latest_run_on_child(target.child_conversation_id, &target.task_id)
            .await?;
        let child_superseded = self
            .child_is_superseded(target.child_conversation_id)
            .await?;
        let (child_ownership_valid, agent_type_matches, external_id_present) =
            self.child_continue_facts(target).await?;
        let snapshot_complete = launch_snapshot_from_run(target)
            .map(|snapshot| snapshot_is_complete(&snapshot))
            .unwrap_or(false);

        // Load termination audit from row (not on PersistedRun view).
        let termination_audit_json = {
            let row = DelegationTaskRun::find_by_id(&target.task_id)
                .one(&self.db.conn)
                .await
                .map_err(map_db_err)?;
            row.and_then(|r| r.termination_audit_json)
        };

        Ok(ContinueEligibility {
            history_only: target.history_only,
            is_latest,
            has_active_run: has_active,
            child_superseded,
            child_ownership_valid,
            agent_type_matches,
            snapshot_complete,
            external_id_present,
            run_status: target.run_status.clone(),
            error_code: target.error_code.clone(),
            admission_class: target.admission_class.clone(),
            reached_running: target.reached_running_at.is_some(),
            termination_audit_json,
        })
    }

    async fn child_has_non_terminal(
        &self,
        child_conversation_id: i32,
    ) -> Result<bool, TaskStoreError> {
        let hit = DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ChildConversationId.eq(child_conversation_id))
            .filter(
                delegation_task_run::Column::Status
                    .is_in([DelegationRunStatus::Reserving, DelegationRunStatus::Running]),
            )
            .limit(1)
            .all(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        Ok(!hit.is_empty())
    }

    async fn child_is_superseded(
        &self,
        child_conversation_id: i32,
    ) -> Result<bool, TaskStoreError> {
        // Any run with replaced_task_id pointing at a run belonging to this child.
        let child_task_ids: Vec<String> = DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ChildConversationId.eq(child_conversation_id))
            .all(&self.db.conn)
            .await
            .map_err(map_db_err)?
            .into_iter()
            .map(|r| r.task_id)
            .collect();
        if child_task_ids.is_empty() {
            return Ok(false);
        }
        let hit = DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ReplacedTaskId.is_in(child_task_ids))
            .limit(1)
            .all(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        Ok(!hit.is_empty())
    }

    async fn child_continue_facts(
        &self,
        target: &PersistedRun,
    ) -> Result<(bool, bool, bool), TaskStoreError> {
        use crate::db::entities::conversation;
        let child = conversation::Entity::find_by_id(target.child_conversation_id)
            .one(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        let Some(child) = child else {
            return Ok((false, false, false));
        };
        // Fail closed on deleted parent/child ownership.
        if child.deleted_at.is_some() {
            return Ok((false, false, false));
        }
        if child.parent_id != Some(target.parent_conversation_id) {
            return Ok((false, false, false));
        }
        let parent = conversation::Entity::find_by_id(target.parent_conversation_id)
            .one(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        if !parent.is_some_and(|parent| parent.deleted_at.is_none()) {
            return Ok((false, false, false));
        }
        let run = DelegationTaskRun::find_by_id(&target.task_id)
            .one(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        let Some(run) = run else {
            return Ok((false, false, false));
        };
        let Some(run_agent) = parse_known_agent_type(&run.agent_type) else {
            return Ok((false, false, false));
        };
        let Some(child_agent) = parse_known_agent_type(&child.agent_type) else {
            return Ok((false, false, false));
        };
        let agent_matches = run.agent_type == child.agent_type
            && run_agent == target.agent_type
            && child_agent == target.agent_type;
        let external_id_present = child
            .external_id
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        Ok((true, agent_matches, external_id_present))
    }

    /// Bind `child_connection_id` onto a still-`reserving` run **before**
    /// prompt admission / `promote_running`.
    ///
    /// Enables cold terminal resolution during the pre-bootstrap admission
    /// window (ResumeExistingOnly identity refuse) when the live registration
    /// map is unavailable. Returns `Ok(())` on first bind or idempotent
    /// same-connection re-bind **while still reserving**.
    ///
    /// Classification on zero-row reread (order is intentional):
    /// 1. Already bound to a **different** connection at **any** status →
    ///    [`TaskStoreError::BindOwnershipConflict`] (caller must not settle).
    /// 2. Same connection while still reserving → `Ok(())` (idempotent).
    /// 3. Same connection / unbound but not reserving → `Permanent` not-reserving
    ///    (caller must not terminalize a running/terminal winner).
    pub async fn bind_child_connection_while_reserving(
        &self,
        task_id: &str,
        child_connection_id: impl Into<String>,
    ) -> Result<(), TaskStoreError> {
        let child_connection_id = child_connection_id.into();
        let task_id_owned = task_id.to_string();
        let now = Utc::now();
        let result = DelegationTaskRun::update_many()
            .col_expr(
                delegation_task_run::Column::ChildConnectionId,
                Expr::value(child_connection_id.clone()),
            )
            .col_expr(delegation_task_run::Column::UpdatedAt, Expr::value(now))
            .filter(delegation_task_run::Column::TaskId.eq(&task_id_owned))
            .filter(delegation_task_run::Column::Status.eq(DelegationRunStatus::Reserving))
            .filter(delegation_task_run::Column::ChildConnectionId.is_null())
            .exec(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        if result.rows_affected == 0 {
            // Already bound, not reserving, or missing.
            if let Some(run) = self.load_by_task_id(task_id).await? {
                // Ownership fence first (any status): a different durable owner
                // must never be misclassified as generic Permanent, or a
                // challenger BindFailed path could settle_terminal the owner.
                if let Some(owner) = run.child_connection_id.as_deref() {
                    if owner != child_connection_id.as_str() {
                        return Err(TaskStoreError::BindOwnershipConflict(format!(
                            "bind_child_connection_while_reserving: task {task_id} already bound \
                             to different connection (status={:?})",
                            run.run_status
                        )));
                    }
                    // Same connection: only idempotent while still reserving.
                    if run.run_status != DelegationRunStatus::Reserving {
                        return Err(TaskStoreError::Permanent(format!(
                            "bind_child_connection_while_reserving: task {task_id} not reserving \
                             (status={:?})",
                            run.run_status
                        )));
                    }
                    return Ok(());
                }
                // Unbound row: not-reserving is a state conflict (no settle of
                // foreign winners — row has no connection claim for us either).
                if run.run_status != DelegationRunStatus::Reserving {
                    return Err(TaskStoreError::Permanent(format!(
                        "bind_child_connection_while_reserving: task {task_id} not reserving \
                         (status={:?})",
                        run.run_status
                    )));
                }
                // Unbound reserving but CAS missed (concurrent writer). Fail closed.
                return Err(TaskStoreError::Permanent(format!(
                    "bind_child_connection_while_reserving: task {task_id} bind CAS miss \
                     while unbound reserving"
                )));
            }
            return Err(TaskStoreError::NotFound(task_id.to_string()));
        }
        Ok(())
    }

    /// Pre-admission `spawn_failed` settle with **atomic ownership CAS**.
    ///
    /// The terminal write is filtered in the same transaction as:
    /// - `status = reserving` (never rewrites `Running`)
    /// - `child_connection_id IS NULL OR child_connection_id = expected`
    ///
    /// So a concurrent bind/promote of a foreign owner cannot be terminalized by
    /// a pre-read that went stale. Zero-row CAS outcomes:
    /// - durable terminal → [`Settlement::Existing`]
    /// - running / foreign reserving / missing claim → `Ok(None)` (no mutate)
    pub async fn settle_pre_admission_failure_if_owned(
        &self,
        task_id: &str,
        expected_child_connection_id: &str,
        terminal: TerminalTaskWrite,
    ) -> Result<Option<Settlement>, TaskStoreError> {
        // Phase 1: snapshot outside a long write lock so a mid-path race gate
        // can let concurrent bind/promote commit before the ownership CAS.
        let Some(snapshot) = self.load_by_task_id(task_id).await? else {
            return Err(TaskStoreError::NotFound(task_id.to_string()));
        };
        match snapshot.run_status {
            DelegationRunStatus::Completed
            | DelegationRunStatus::Failed
            | DelegationRunStatus::Canceled => {
                return Ok(Some(Settlement::Existing(
                    snapshot.to_persisted_task().to_report(None),
                )));
            }
            DelegationRunStatus::Running => {
                return Ok(None);
            }
            DelegationRunStatus::Reserving => {}
        }
        if let Some(owner) = snapshot.child_connection_id.as_deref() {
            if owner != expected_child_connection_id {
                return Ok(None);
            }
        }

        // Phase 2 (test-only): after observing still-Reserving own/unbound
        // ("would settle"), before the ownership-fenced write. Concurrent
        // foreign bind/promote must be able to commit here (no write txn open).
        #[cfg(any(test, feature = "test-utils"))]
        {
            let gate = self.settle_gate.lock().await.take();
            if let Some(mut gate) = gate {
                if let Some(tx) = gate.entered.take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = gate.release.take() {
                    tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, rx)
                        .await
                        .map_err(|_| {
                            TaskStoreError::Permanent(
                                "test run_store settle gate timed out".into(),
                            )
                        })?
                        .map_err(|_| {
                            TaskStoreError::Permanent(
                                "test run_store settle gate release dropped".into(),
                            )
                        })?;
                }
            }
        }

        // Phase 3: ownership-CAS write only (no reliance on the stale snapshot
        // for the mutate). Filters are the sole correctness fence.
        let run_status = task_status_to_run_status(terminal.status)?;
        let proj_status = task_status_to_delegation_task_status(terminal.status)?;
        let finished_at = terminal.finished_at;
        let error_code = terminal.error_code.clone();
        let conversation_status = terminal.conversation_status.clone();
        let card_summary_json = terminal.card_summary_json.clone();
        let termination_audit_json = terminal.termination_audit_json.clone();
        let expected = expected_child_connection_id.to_string();
        let final_stats = match terminal.runtime_stats.as_ref() {
            Some(stats) => Some(encoded_runtime_stats(stats)?),
            None => None,
        };

        let outcome = self
            .db
            .conn
            .transaction::<_, Option<Settlement>, TaskStoreError>(|txn| {
                let task_id = task_id.to_string();
                let error_code = error_code.clone();
                let conversation_status = conversation_status.clone();
                let card_summary_json = card_summary_json.clone();
                let termination_audit_json = termination_audit_json.clone();
                let final_stats = final_stats.clone();
                let expected = expected.clone();
                Box::pin(async move {
                    let now = Utc::now();
                    let mut update = DelegationTaskRun::update_many()
                        .col_expr(
                            delegation_task_run::Column::Status,
                            Expr::value(run_status),
                        )
                        .col_expr(
                            delegation_task_run::Column::ErrorCode,
                            Expr::value(error_code.clone()),
                        )
                        .col_expr(
                            delegation_task_run::Column::FinishedAt,
                            Expr::value(finished_at),
                        )
                        .col_expr(delegation_task_run::Column::UpdatedAt, Expr::value(now));

                    if let Some(ref summary) = card_summary_json {
                        update = update.col_expr(
                            delegation_task_run::Column::CardSummaryJson,
                            Expr::value(summary.clone()),
                        );
                    }
                    if let Some(ref audit) = termination_audit_json {
                        update = update.col_expr(
                            delegation_task_run::Column::TerminationAuditJson,
                            Expr::value(audit.clone()),
                        );
                    }
                    if let Some(ref stats) = final_stats {
                        update = apply_encoded_runtime_stats_to_run_update(update, stats);
                    }

                    // Atomic ownership fence: only unbound or this connection,
                    // and only while still reserving.
                    let ownership = sea_orm::Condition::any()
                        .add(delegation_task_run::Column::ChildConnectionId.is_null())
                        .add(
                            delegation_task_run::Column::ChildConnectionId
                                .eq(expected.clone()),
                        );
                    let result = update
                        .filter(delegation_task_run::Column::TaskId.eq(&task_id))
                        .filter(
                            delegation_task_run::Column::Status
                                .eq(DelegationRunStatus::Reserving),
                        )
                        .filter(ownership)
                        .exec(txn)
                        .await
                        .map_err(map_db_err)?;

                    if result.rows_affected == 0 {
                        let again = DelegationTaskRun::find_by_id(&task_id)
                            .one(txn)
                            .await
                            .map_err(map_db_err)?
                            .ok_or_else(|| TaskStoreError::NotFound(task_id.clone()))?;
                        let persisted = model_to_persisted_run(again).ok_or_else(|| {
                            TaskStoreError::Permanent(format!(
                                "run {task_id} unreadable after ownership CAS miss"
                            ))
                        })?;
                        return match persisted.run_status {
                            DelegationRunStatus::Completed
                            | DelegationRunStatus::Failed
                            | DelegationRunStatus::Canceled => Ok(Some(Settlement::Existing(
                                persisted.to_persisted_task().to_report(None),
                            ))),
                            DelegationRunStatus::Running | DelegationRunStatus::Reserving => {
                                Ok(None)
                            }
                        };
                    }

                    let won = DelegationTaskRun::find_by_id(&task_id)
                        .one(txn)
                        .await
                        .map_err(map_db_err)?
                        .ok_or_else(|| TaskStoreError::NotFound(task_id.clone()))?;
                    let generation = won.generation;
                    let child_id = won.child_conversation_id;
                    let mut projection = ConversationProjection {
                        generation,
                        task_status: Some(proj_status),
                        error_code: Some(error_code.clone()),
                        finished_at: Some(Some(finished_at)),
                        conversation_status: Some(conversation_status),
                        started_at: None,
                        tool_call_count: None,
                        edit_tool_call_count: None,
                        touched_files_json: None,
                        touched_files_truncated: None,
                        additions: None,
                        deletions: None,
                        line_counts_complete: None,
                        reset_generation_rollups: false,
                    };
                    if let Some(ref stats) = final_stats {
                        fill_projection_runtime_stats(&mut projection, stats);
                    }
                    project_conversation_in_txn(txn, child_id, projection)
                        .await
                        .map_err(map_db_err)?;

                    let persisted = model_to_persisted_run(won).ok_or_else(|| {
                        TaskStoreError::Permanent(format!("settled run {task_id} unreadable"))
                    })?;
                    Ok(Some(Settlement::Won(
                        persisted.to_persisted_task().to_report(None),
                    )))
                })
            })
            .await;

        match outcome {
            Ok(s) => Ok(s),
            Err(sea_orm::TransactionError::Connection(e)) => Err(map_db_err(e)),
            Err(sea_orm::TransactionError::Transaction(e)) => Err(e),
        }
    }

    /// If the durable run is already terminal, return its winner report for
    /// first-terminal-wins replay (e.g. bind ownership conflict against a
    /// completed/failed/canceled owner). Non-terminal → `Ok(None)`.
    pub async fn load_terminal_winner_report(
        &self,
        task_id: &str,
    ) -> Result<Option<crate::acp::delegation::types::DelegationTaskReport>, TaskStoreError> {
        let Some(run) = self.load_by_task_id(task_id).await? else {
            return Ok(None);
        };
        match run.run_status {
            DelegationRunStatus::Completed
            | DelegationRunStatus::Failed
            | DelegationRunStatus::Canceled => {
                Ok(Some(run.to_persisted_task().to_report(None)))
            }
            DelegationRunStatus::Reserving | DelegationRunStatus::Running => Ok(None),
        }
    }

    /// Compatibility wrapper for Err-only callers.
    ///
    /// Maps [`PromoteRunningKind::Promoted`] / [`PromoteRunningKind::AlreadyRunning`]
    /// to `Ok(run)`. All other outcomes become `Err(...)`. Prefer
    /// [`Self::promote_running_detailed`] for typed handling (Task 4+).
    pub async fn promote_running(
        &self,
        task_id: &str,
        child_connection_id: impl Into<String>,
        prompt_accepted_at: DateTime<Utc>,
    ) -> Result<PersistedRun, TaskStoreError> {
        let child_connection_id = child_connection_id.into();
        let outcome = self
            .promote_running_detailed(task_id, &child_connection_id, prompt_accepted_at)
            .await?;
        match outcome.kind {
            PromoteRunningKind::Promoted { run }
            | PromoteRunningKind::AlreadyRunning { run } => Ok(run),
            PromoteRunningKind::BudgetExhausted { message } => {
                Err(TaskStoreError::BudgetExhausted(message))
            }
            PromoteRunningKind::TerminalWinner { run } => Err(TaskStoreError::Permanent(format!(
                "promote_running: terminal winner for task {}",
                run.task_id
            ))),
            PromoteRunningKind::StateConflict { message, .. } => {
                Err(TaskStoreError::Permanent(message))
            }
            PromoteRunningKind::RetryExhausted { message, .. } => {
                Err(TaskStoreError::Permanent(message))
            }
            PromoteRunningKind::Permanent { message } => Err(TaskStoreError::Permanent(message)),
        }
    }

    /// Write-first `reserving` → `running` promote with typed outcomes.
    ///
    /// Transaction order: claim write → read/validate → budget charge →
    /// status/timestamps → commit. Conversation projection is deferred to a
    /// later task. Uses [`PromoteRetryPolicy`] (3 attempts; 10 ms then 25 ms)
    /// for ordinary BUSY/LOCKED and defensive BUSY_SNAPSHOT(517).
    pub async fn promote_running_detailed(
        &self,
        task_id: &str,
        child_connection_id: &str,
        prompt_accepted_at: DateTime<Utc>,
    ) -> Result<PromoteRunningOutcome, TaskStoreError> {
        let policy = PromoteRetryPolicy::production();
        let mut meta = PromoteAttemptMeta::default();
        let mut last_retry: Option<(PromoteRetryClass, String)> = None;

        for attempt in 1..=policy.max_attempts {
            meta.attempts = attempt;
            match self
                .promote_running_once(task_id, child_connection_id, prompt_accepted_at)
                .await
            {
                Ok(kind) => {
                    return Ok(PromoteRunningOutcome { kind, meta });
                }
                Err(PromoteOnceError::Retry { class, message }) => {
                    match class {
                        PromoteRetryClass::Busy => meta.busy_retries += 1,
                        PromoteRetryClass::Locked => meta.locked_retries += 1,
                        PromoteRetryClass::BusySnapshot => meta.busy_snapshot_retries += 1,
                    }
                    last_retry = Some((class, message));
                    if attempt >= policy.max_attempts {
                        break;
                    }
                    tokio::time::sleep(policy.delay_after_failed_attempt(attempt)).await;
                }
            }
        }

        let (class, message) = last_retry.unwrap_or((
            PromoteRetryClass::Busy,
            "promote_running: retry exhausted without class".into(),
        ));
        Ok(PromoteRunningOutcome {
            kind: PromoteRunningKind::RetryExhausted { class, message },
            meta,
        })
    }

    async fn promote_running_once(
        &self,
        task_id: &str,
        child_connection_id: &str,
        prompt_accepted_at: DateTime<Utc>,
    ) -> Result<PromoteRunningKind, PromoteOnceError> {
        // Ambiguous-permanent inject stays pre-txn (reread durable truth as if
        // commit I/O failed after an unknown outcome). Transient faults are
        // applied **inside** the transaction after claim/budget so retries
        // re-run a complete write-first body and roll back partial work.
        #[cfg(any(test, feature = "test-utils"))]
        if let Some(PromoteTestFault::AmbiguousPermanent { message }) =
            self.take_pre_txn_promote_fault().await
        {
            let kind = self
                .classify_promote_reread(task_id, child_connection_id, /*ambiguous*/ true)
                .await
                .unwrap_or_else(|msg| PromoteRunningKind::Permanent { message: msg });
            return Ok(match kind {
                PromoteRunningKind::Permanent { .. } => {
                    PromoteRunningKind::Permanent { message }
                }
                other => other,
            });
        }

        let promote_at = max_utc(Utc::now(), prompt_accepted_at);
        let task_id_owned = task_id.to_string();
        let child_connection_id_owned = child_connection_id.to_string();

        #[cfg(any(test, feature = "test-utils"))]
        let promote_faults = self.promote_faults.clone();
        #[cfg(any(test, feature = "test-utils"))]
        let mut claim_gate = self.promote_claim_gate.lock().await.take();

        let outcome = self
            .db
            .conn
            .transaction::<_, PromoteTxnResult, PromoteTxnError>(|txn| {
                let task_id = task_id_owned.clone();
                let child_connection_id = child_connection_id_owned.clone();
                #[cfg(any(test, feature = "test-utils"))]
                let promote_faults = promote_faults.clone();
                Box::pin(async move {
                    // WRITE FIRST — claim writer lock before any read so a
                    // concurrent commit cannot strand a deferred read snapshot
                    // (SQLITE_BUSY_SNAPSHOT / 517). Preserve raw DbErr so the
                    // outer path can extract SQLite codes before stringifying.
                    let claimed = DelegationTaskRun::update_many()
                        .col_expr(
                            delegation_task_run::Column::UpdatedAt,
                            Expr::value(promote_at),
                        )
                        .filter(delegation_task_run::Column::TaskId.eq(&task_id))
                        .filter(
                            delegation_task_run::Column::Status.eq(DelegationRunStatus::Reserving),
                        )
                        .exec(txn)
                        .await
                        .map_err(PromoteTxnError::Db)?;
                    if claimed.rows_affected == 0 {
                        return Ok(PromoteTxnResult::ZeroRowClaim);
                    }

                    #[cfg(any(test, feature = "test-utils"))]
                    if let Some(mut gate) = claim_gate.take() {
                        if let Some(tx) = gate.entered.take() {
                            let _ = tx.send(());
                        }
                        if let Some(rx) = gate.release.take() {
                            tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, rx)
                                .await
                                .map_err(|_| {
                                    PromoteTxnError::Permanent(
                                        "test run_store promote claim gate timed out".into(),
                                    )
                                })?
                                .map_err(|_| {
                                    PromoteTxnError::Permanent(
                                        "test run_store promote claim gate release dropped".into(),
                                    )
                                })?;
                        }
                    }

                    #[cfg(any(test, feature = "test-utils"))]
                    {
                        // Hold one guard only — re-queuing later-stage faults must
                        // not re-lock the same tokio Mutex (non-reentrant → hang).
                        let mut faults = promote_faults.lock().await;
                        if let Some(fault) = faults.pop_front() {
                            match fault {
                                PromoteTestFault::AfterClaimTransient(class) => {
                                    return Err(PromoteTxnError::Db(synthetic_transient_db_err(
                                        class,
                                    )));
                                }
                                PromoteTestFault::AfterBudgetTransient(_)
                                | PromoteTestFault::AfterProjectionTransient(_) => {
                                    // Re-queue: later body steps have not run yet.
                                    faults.push_front(fault);
                                }
                                PromoteTestFault::AmbiguousPermanent { message } => {
                                    return Err(PromoteTxnError::Permanent(message));
                                }
                            }
                        }
                    }

                    let row = DelegationTaskRun::find_by_id(&task_id)
                        .one(txn)
                        .await
                        .map_err(PromoteTxnError::Db)?
                        .ok_or_else(|| PromoteTxnError::NotFound(task_id.clone()))?;

                    if row.status != DelegationRunStatus::Reserving {
                        // Lost the claim race after writer lock; treat as zero-row.
                        return Ok(PromoteTxnResult::ZeroRowClaim);
                    }

                    match row.admission_class {
                        AdmissionClass::UnexpectedContinue => {
                            charge_unexpected_continue_promote(
                                txn,
                                &row.lineage_root_task_id,
                                row.parent_conversation_id,
                                row.work_unit_key.as_deref(),
                            )
                            .await?;
                        }
                        AdmissionClass::Replacement => {
                            charge_replacement_promote(
                                txn,
                                &row.lineage_root_task_id,
                                row.parent_conversation_id,
                                row.work_unit_key.as_deref(),
                            )
                            .await?;
                        }
                        AdmissionClass::NormalRevision => {}
                    }

                    #[cfg(any(test, feature = "test-utils"))]
                    {
                        let mut faults = promote_faults.lock().await;
                        if let Some(fault) = faults.pop_front() {
                            match fault {
                                PromoteTestFault::AfterBudgetTransient(class)
                                | PromoteTestFault::AfterClaimTransient(class) => {
                                    return Err(PromoteTxnError::Db(synthetic_transient_db_err(
                                        class,
                                    )));
                                }
                                PromoteTestFault::AfterProjectionTransient(_) => {
                                    // Re-queue until after the status write.
                                    faults.push_front(fault);
                                }
                                PromoteTestFault::AmbiguousPermanent { message } => {
                                    return Err(PromoteTxnError::Permanent(message));
                                }
                            }
                        }
                    }

                    let result = DelegationTaskRun::update_many()
                        .col_expr(
                            delegation_task_run::Column::Status,
                            Expr::value(DelegationRunStatus::Running),
                        )
                        .col_expr(
                            delegation_task_run::Column::StartedAt,
                            Expr::value(prompt_accepted_at),
                        )
                        .col_expr(
                            delegation_task_run::Column::ReachedRunningAt,
                            Expr::value(promote_at),
                        )
                        .col_expr(
                            delegation_task_run::Column::ChildConnectionId,
                            Expr::value(child_connection_id.clone()),
                        )
                        .col_expr(
                            delegation_task_run::Column::UpdatedAt,
                            Expr::value(promote_at),
                        )
                        .filter(delegation_task_run::Column::TaskId.eq(&task_id))
                        .filter(
                            delegation_task_run::Column::Status.eq(DelegationRunStatus::Reserving),
                        )
                        .exec(txn)
                        .await
                        .map_err(PromoteTxnError::Db)?;
                    if result.rows_affected == 0 {
                        return Ok(PromoteTxnResult::ZeroRowClaim);
                    }

                    // Atomic running conversation projection: set InProgress
                    // status, clear prior terminal fields, and reset rollups
                    // all within the same write-first transaction.
                    let promote_projection = ConversationProjection {
                        generation: row.generation,
                        task_status: Some(DelegationTaskStatus::Running),
                        error_code: Some(None),
                        finished_at: Some(None),
                        conversation_status: Some(ConversationStatus::InProgress),
                        started_at: Some(prompt_accepted_at),
                        tool_call_count: None,
                        edit_tool_call_count: None,
                        touched_files_json: None,
                        touched_files_truncated: None,
                        additions: None,
                        deletions: None,
                        line_counts_complete: None,
                        reset_generation_rollups: true,
                    };
                    // Projection DB errors stay raw DbErr so outer
                    // map_promote_db_err can classify BUSY/LOCKED before stringify.
                    #[cfg(any(test, feature = "test-utils"))]
                    if let Some(fault) = promote_faults.lock().await.pop_front() {
                        match fault {
                            PromoteTestFault::AfterProjectionTransient(class)
                            | PromoteTestFault::AfterBudgetTransient(class)
                            | PromoteTestFault::AfterClaimTransient(class) => {
                                return Err(PromoteTxnError::Db(synthetic_transient_db_err(
                                    class,
                                )));
                            }
                            PromoteTestFault::AmbiguousPermanent { message } => {
                                return Err(PromoteTxnError::Permanent(message));
                            }
                        }
                    }

                    let projected = project_conversation_in_txn(
                        txn,
                        row.child_conversation_id,
                        promote_projection,
                    )
                    .await
                    .map_err(PromoteTxnError::Db)?;
                    if !projected {
                        // Newer generation already owns the conversation row;
                        // roll back the promote transaction — do not leave a
                        // running run under a stale generation claim.
                        // Logical fence soft-miss is a typed state conflict
                        // (not Permanent / not SQLite transient).
                        return Err(PromoteTxnError::StateConflict {
                            class: PromoteConflictClass::Status,
                            message: format!(
                                "promote_running: generation fence rejected gen {} for child {child_id}",
                                row.generation,
                                child_id = row.child_conversation_id
                            ),
                        });
                    }

                    let promoted = DelegationTaskRun::find_by_id(&task_id)
                        .one(txn)
                        .await
                        .map_err(PromoteTxnError::Db)?
                        .ok_or_else(|| {
                            PromoteTxnError::Permanent(format!(
                                "promote_running: task {task_id} missing after promote write"
                            ))
                        })?;
                    let run = model_to_persisted_run(promoted).ok_or_else(|| {
                        PromoteTxnError::Permanent(format!(
                            "promote_running: task {task_id} unreadable after promote"
                        ))
                    })?;
                    Ok(PromoteTxnResult::Promoted(run))
                })
            })
            .await;

        match outcome {
            Ok(PromoteTxnResult::Promoted(run)) => Ok(PromoteRunningKind::Promoted { run }),
            Ok(PromoteTxnResult::ZeroRowClaim) => Ok(self
                .classify_promote_reread(task_id, child_connection_id, /*ambiguous*/ false)
                .await
                .unwrap_or_else(|message| PromoteRunningKind::Permanent { message })),
            Err(sea_orm::TransactionError::Connection(e)) => {
                self.map_promote_db_err(task_id, child_connection_id, e)
                    .await
            }
            Err(sea_orm::TransactionError::Transaction(PromoteTxnError::Db(e))) => {
                self.map_promote_db_err(task_id, child_connection_id, e)
                    .await
            }
            Err(sea_orm::TransactionError::Transaction(PromoteTxnError::BudgetExhausted(
                message,
            ))) => Ok(PromoteRunningKind::BudgetExhausted { message }),
            Err(sea_orm::TransactionError::Transaction(PromoteTxnError::NotFound(id))) => {
                Ok(PromoteRunningKind::StateConflict {
                    class: PromoteConflictClass::Missing,
                    message: format!("promote_running: task {id} not found"),
                })
            }
            Err(sea_orm::TransactionError::Transaction(PromoteTxnError::StateConflict {
                class,
                message,
            })) => Ok(PromoteRunningKind::StateConflict { class, message }),
            Err(sea_orm::TransactionError::Transaction(PromoteTxnError::Permanent(message))) => {
                // Commit/invariant ambiguity: reread durable truth.
                Ok(self
                    .classify_promote_reread(task_id, child_connection_id, /*ambiguous*/ true)
                    .await
                    .unwrap_or(PromoteRunningKind::Permanent { message }))
            }
        }
    }

    async fn map_promote_db_err(
        &self,
        task_id: &str,
        child_connection_id: &str,
        err: sea_orm::DbErr,
    ) -> Result<PromoteRunningKind, PromoteOnceError> {
        // Classify from the raw DbErr (code extraction before stringification).
        if let Some(class) = classify_sqlite_transient(&err) {
            if matches!(class, SqliteTransientClass::BusySnapshot) {
                tracing::error!(
                    task_id = %task_id,
                    error = %err,
                    codes = ?crate::acp::delegation::store::extract_sqlite_codes(&err),
                    "[delegation] promote_running observed SQLITE_BUSY_SNAPSHOT; write-first invariant regression"
                );
            }
            return Err(PromoteOnceError::Retry {
                class: class.into(),
                message: err.to_string(),
            });
        }
        // Permanent / ambiguous connection error → reread.
        Ok(self
            .classify_promote_reread(task_id, child_connection_id, /*ambiguous*/ true)
            .await
            .unwrap_or_else(|_| PromoteRunningKind::Permanent {
                message: err.to_string(),
            }))
    }

    /// Reread durable truth after a zero-row claim or ambiguous permanent error.
    async fn classify_promote_reread(
        &self,
        task_id: &str,
        child_connection_id: &str,
        ambiguous: bool,
    ) -> Result<PromoteRunningKind, String> {
        let Some(run) = self
            .load_by_task_id(task_id)
            .await
            .map_err(|e| e.to_string())?
        else {
            return Ok(PromoteRunningKind::StateConflict {
                class: PromoteConflictClass::Missing,
                message: format!("promote_running: task {task_id} not found on reread"),
            });
        };

        match run.run_status {
            DelegationRunStatus::Running => {
                if promote_connection_matches(&run, child_connection_id) {
                    Ok(if ambiguous {
                        // Commit may have succeeded; treat matching running as promoted.
                        PromoteRunningKind::Promoted { run }
                    } else {
                        PromoteRunningKind::AlreadyRunning { run }
                    })
                } else {
                    Ok(PromoteRunningKind::StateConflict {
                        class: PromoteConflictClass::Ownership,
                        message: format!(
                            "promote_running: task {task_id} running under different child connection"
                        ),
                    })
                }
            }
            DelegationRunStatus::Completed
            | DelegationRunStatus::Failed
            | DelegationRunStatus::Canceled => Ok(PromoteRunningKind::TerminalWinner { run }),
            DelegationRunStatus::Reserving => {
                if ambiguous {
                    Ok(PromoteRunningKind::Permanent {
                        message: format!(
                            "promote_running: ambiguous failure; task {task_id} still reserving"
                        ),
                    })
                } else {
                    Ok(PromoteRunningKind::StateConflict {
                        class: PromoteConflictClass::Status,
                        message: format!(
                            "promote_running: zero-row claim but task {task_id} still reserving"
                        ),
                    })
                }
            }
        }
    }

    /// Conditional terminal settle on the run row + monotonic conversation projection.
    ///
    /// When [`TerminalTaskWrite::runtime_stats`] is `Some`, the final runtime
    /// snapshot is written onto the run row and projected onto the child
    /// conversation **in the same transaction** as the terminal status. After
    /// commit the run is frozen; later `write_runtime_stats` calls are no-ops.
    pub async fn settle_terminal(
        &self,
        task_id: &str,
        terminal: TerminalTaskWrite,
    ) -> Result<Settlement, TaskStoreError> {
        #[cfg(any(test, feature = "test-utils"))]
        {
            let gate = self.settle_gate.lock().await.take();
            if let Some(mut gate) = gate {
                if let Some(tx) = gate.entered.take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = gate.release.take() {
                    tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, rx)
                        .await
                        .map_err(|_| {
                            TaskStoreError::Permanent(
                                "test run_store settle gate timed out".into(),
                            )
                        })?
                        .map_err(|_| {
                            TaskStoreError::Permanent(
                                "test run_store settle gate release dropped".into(),
                            )
                        })?;
                }
            }
        }
        let run_status = task_status_to_run_status(terminal.status)?;
        let proj_status = task_status_to_delegation_task_status(terminal.status)?;
        let finished_at = terminal.finished_at;
        let error_code = terminal.error_code.clone();
        let conversation_status = terminal.conversation_status.clone();
        let card_summary_json = terminal.card_summary_json.clone();
        let termination_audit_json = terminal.termination_audit_json.clone();
        let final_stats = match terminal.runtime_stats.as_ref() {
            Some(stats) => Some(encoded_runtime_stats(stats)?),
            None => None,
        };

        let outcome =
            self.db
                .conn
                .transaction::<_, Settlement, TaskStoreError>(|txn| {
                    let task_id = task_id.to_string();
                    let error_code = error_code.clone();
                    let conversation_status = conversation_status.clone();
                    let card_summary_json = card_summary_json.clone();
                    let termination_audit_json = termination_audit_json.clone();
                    let final_stats = final_stats.clone();
                    Box::pin(async move {
                        let row = DelegationTaskRun::find_by_id(&task_id)
                            .one(txn)
                            .await
                            .map_err(map_db_err)?
                            .ok_or_else(|| TaskStoreError::NotFound(task_id.clone()))?;

                        match row.status {
                            DelegationRunStatus::Completed
                            | DelegationRunStatus::Failed
                            | DelegationRunStatus::Canceled => {
                                let persisted = model_to_persisted_run(row).ok_or_else(|| {
                                    TaskStoreError::Permanent(format!(
                                        "terminal run {task_id} unreadable"
                                    ))
                                })?;
                                return Ok(Settlement::Existing(
                                    persisted.to_persisted_task().to_report(None),
                                ));
                            }
                            DelegationRunStatus::Reserving | DelegationRunStatus::Running => {}
                        }

                        let generation = row.generation;
                        let child_id = row.child_conversation_id;
                        let now = Utc::now();

                        let mut update = DelegationTaskRun::update_many()
                            .col_expr(
                                delegation_task_run::Column::Status,
                                sea_orm::sea_query::Expr::value(run_status),
                            )
                            .col_expr(
                                delegation_task_run::Column::ErrorCode,
                                sea_orm::sea_query::Expr::value(error_code.clone()),
                            )
                            .col_expr(
                                delegation_task_run::Column::FinishedAt,
                                sea_orm::sea_query::Expr::value(finished_at),
                            )
                            .col_expr(
                                delegation_task_run::Column::UpdatedAt,
                                sea_orm::sea_query::Expr::value(now),
                            );

                        if let Some(ref summary) = card_summary_json {
                            update = update.col_expr(
                                delegation_task_run::Column::CardSummaryJson,
                                sea_orm::sea_query::Expr::value(summary.clone()),
                            );
                        }
                        if let Some(ref audit) = termination_audit_json {
                            update = update.col_expr(
                                delegation_task_run::Column::TerminationAuditJson,
                                sea_orm::sea_query::Expr::value(audit.clone()),
                            );
                        }

                        if let Some(ref stats) = final_stats {
                            update = apply_encoded_runtime_stats_to_run_update(update, stats);
                        }

                        let result = update
                            .filter(delegation_task_run::Column::TaskId.eq(&task_id))
                            .filter(delegation_task_run::Column::Status.is_in([
                                DelegationRunStatus::Reserving,
                                DelegationRunStatus::Running,
                            ]))
                            .exec(txn)
                            .await
                            .map_err(map_db_err)?;

                        if result.rows_affected == 0 {
                            let again = DelegationTaskRun::find_by_id(&task_id)
                                .one(txn)
                                .await
                                .map_err(map_db_err)?
                                .ok_or_else(|| TaskStoreError::NotFound(task_id.clone()))?;
                            let persisted = model_to_persisted_run(again).ok_or_else(|| {
                                TaskStoreError::Permanent(format!(
                                    "run {task_id} unreadable after CAS miss"
                                ))
                            })?;
                            if matches!(
                                persisted.run_status,
                                DelegationRunStatus::Reserving | DelegationRunStatus::Running
                            ) {
                                return Err(TaskStoreError::Permanent(format!(
                                    "settle CAS missed but task {task_id} still non-terminal"
                                )));
                            }
                            return Ok(Settlement::Existing(
                                persisted.to_persisted_task().to_report(None),
                            ));
                        }

                        let mut projection = ConversationProjection {
                            generation,
                            task_status: Some(proj_status),
                            error_code: Some(error_code.clone()),
                            finished_at: Some(Some(finished_at)),
                            conversation_status: Some(conversation_status),
                            started_at: None,
                            tool_call_count: None,
                            edit_tool_call_count: None,
                            touched_files_json: None,
                            touched_files_truncated: None,
                            additions: None,
                            deletions: None,
                            line_counts_complete: None,
                            reset_generation_rollups: false,
                        };
                        if let Some(ref stats) = final_stats {
                            fill_projection_runtime_stats(&mut projection, stats);
                        }

                        project_conversation_in_txn(txn, child_id, projection)
                            .await
                            .map_err(map_db_err)?;

                        let won = DelegationTaskRun::find_by_id(&task_id)
                            .one(txn)
                            .await
                            .map_err(map_db_err)?
                            .ok_or_else(|| TaskStoreError::NotFound(task_id.clone()))?;
                        let persisted = model_to_persisted_run(won).ok_or_else(|| {
                            TaskStoreError::Permanent(format!("settled run {task_id} unreadable"))
                        })?;
                        Ok(Settlement::Won(
                            persisted.to_persisted_task().to_report(None),
                        ))
                    })
                })
                .await;

        match outcome {
            Ok(s) => Ok(s),
            Err(sea_orm::TransactionError::Connection(e)) => Err(map_db_err(e)),
            Err(sea_orm::TransactionError::Transaction(e)) => Err(e),
        }
    }

    pub async fn load_by_task_id(
        &self,
        task_id: &str,
    ) -> Result<Option<PersistedRun>, TaskStoreError> {
        let row = DelegationTaskRun::find_by_id(task_id)
            .one(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        Ok(row.and_then(model_to_persisted_run))
    }

    /// Parent-scoped unique task-id prefix recovery over **run** rows.
    pub async fn resolve_unique_owned_prefix(
        &self,
        parent_id: i32,
        prefix: &str,
    ) -> Result<Option<String>, TaskStoreError> {
        if !is_valid_task_id_prefix(prefix) {
            return Ok(None);
        }
        let rows = DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ParentConversationId.eq(parent_id))
            .filter(delegation_task_run::Column::TaskId.starts_with(prefix))
            .limit(2)
            .all(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        if rows.len() != 1 {
            return Ok(None);
        }
        Ok(rows.into_iter().next().map(|r| r.task_id))
    }

    /// Monotonic latest-run projection onto the child conversation.
    ///
    /// Succeeds only when `delegation_run_generation IS NULL OR <= incoming`.
    /// Returns `true` when the conversation row was updated.
    pub async fn project_conversation(
        &self,
        child_conversation_id: i32,
        projection: ConversationProjection,
    ) -> Result<bool, TaskStoreError> {
        let txn = self.db.conn.begin().await.map_err(map_db_err)?;
        let updated = project_conversation_in_txn(&txn, child_conversation_id, projection)
            .await
            .map_err(map_db_err)?;
        txn.commit().await.map_err(map_db_err)?;
        Ok(updated)
    }

    /// Startup reconcile of non-terminal runs **before** the delegation listener
    /// accepts requests.
    ///
    /// Status + audit split (design-mandated):
    /// - Unbound `reserving` (child_connection_id IS NULL) → `failed` / `host_restarted`
    ///   (pre-send, no counter was charged; Skill may inherit `admission_class`
    ///   for continue eligibility)
    /// - Bound `reserving` (child_connection_id IS NOT NULL) → `failed` / `admission_unknown`
    ///   (prompt may have been sent; explicit replacement only, never auto-continue)
    /// - `running` → `canceled` / `host_restarted` (counters kept; eligible for
    ///   unexpected_continue when budget remains)
    ///
    /// Each settlement carries a structured termination audit. Zero non-terminal
    /// rows remain after a successful gate.
    pub async fn reconcile_non_terminal(&self, at: DateTime<Utc>) -> Result<u64, TaskStoreError> {
        let rows = DelegationTaskRun::find()
            .filter(
                delegation_task_run::Column::Status
                    .is_in([DelegationRunStatus::Reserving, DelegationRunStatus::Running]),
            )
            .all(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        let mut n = 0u64;
        for row in rows {
            let audit = host_restarted_termination_audit(&row.status, row.admission_class);
            let write = match row.status {
                DelegationRunStatus::Reserving => {
                    if row.child_connection_id.is_some() {
                        // Bound reserving — prompt may have been accepted.
                        // Classify as admission_unknown; explicit replacement
                        // only, never auto-continuable.
                        let admission_unknown_audit =
                            host_restarted_bound_reserving_audit();
                        TerminalTaskWrite::failed(
                            "admission_unknown",
                            at,
                            ConversationStatus::Cancelled,
                        )
                        .with_termination_audit_json(admission_unknown_audit)
                    } else {
                        // Unbound reserving — pre-send, safe host_restarted.
                        TerminalTaskWrite::failed(
                            "host_restarted",
                            at,
                            ConversationStatus::Cancelled,
                        )
                        .with_termination_audit_json(audit)
                    }
                }
                DelegationRunStatus::Running => {
                    TerminalTaskWrite::canceled("host_restarted", at, ConversationStatus::Cancelled)
                        .with_termination_audit_json(audit)
                }
                _ => continue,
            };
            match self.settle_terminal(&row.task_id, write).await {
                Ok(Settlement::Won(_)) => n += 1,
                Ok(Settlement::Existing(_)) => {}
                Err(TaskStoreError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    /// Load the single non-terminal run whose persisted `child_connection_id`
    /// matches (cold terminal resolution). Never resolves by conversation root
    /// `delegation_call_id`.
    pub async fn load_non_terminal_by_child_connection(
        &self,
        child_connection_id: &str,
    ) -> Result<Option<PersistedRun>, TaskStoreError> {
        let row = DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ChildConnectionId.eq(child_connection_id))
            .filter(
                delegation_task_run::Column::Status
                    .is_in([DelegationRunStatus::Reserving, DelegationRunStatus::Running]),
            )
            .one(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        Ok(row.and_then(model_to_persisted_run))
    }

    /// List every non-terminal run owned by `parent_conversation_id`
    /// (`reserving` or `running`). Used by parent-tree end to settle durable
    /// rows that are not yet visible in in-memory coordination maps.
    pub async fn list_non_terminal_for_parent(
        &self,
        parent_conversation_id: i32,
    ) -> Result<Vec<PersistedRun>, TaskStoreError> {
        let rows = DelegationTaskRun::find()
            .filter(delegation_task_run::Column::ParentConversationId.eq(parent_conversation_id))
            .filter(
                delegation_task_run::Column::Status
                    .is_in([DelegationRunStatus::Reserving, DelegationRunStatus::Running]),
            )
            .all(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        Ok(rows.into_iter().filter_map(model_to_persisted_run).collect())
    }

    /// Whether any non-terminal run remains (startup gate invariant).
    pub async fn count_non_terminal(&self) -> Result<u64, TaskStoreError> {
        let rows = DelegationTaskRun::find()
            .filter(
                delegation_task_run::Column::Status
                    .is_in([DelegationRunStatus::Reserving, DelegationRunStatus::Running]),
            )
            .all(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        Ok(rows.len() as u64)
    }

    /// Write runtime stats onto a **running** run and project them onto the
    /// child conversation under the run's generation CAS fence (one
    /// transaction).
    ///
    /// After settlement the run is frozen: terminal rows are never mutated by
    /// this path (including `finished_at: Some` snapshots). Stale running
    /// writes and post-settle attempts are benign no-ops. Terminal final
    /// snapshots must be supplied via [`Self::settle_terminal`] instead.
    pub async fn write_runtime_stats(
        &self,
        task_id: &str,
        stats: &DelegationRuntimeStats,
    ) -> Result<(), TaskStoreError> {
        let encoded = encoded_runtime_stats(stats)?;

        let outcome = self
            .db
            .conn
            .transaction::<_, (), TaskStoreError>(|txn| {
                let task_id = task_id.to_string();
                let encoded = encoded.clone();
                Box::pin(async move {
                    // True freeze after settle: only `running` rows accept writes.
                    // Terminal (and reserving) rows are never mutated here.
                    let result = apply_encoded_runtime_stats_to_run_update(
                        DelegationTaskRun::update_many()
                            .col_expr(
                                delegation_task_run::Column::UpdatedAt,
                                sea_orm::sea_query::Expr::value(Utc::now()),
                            ),
                        &encoded,
                    )
                    .filter(delegation_task_run::Column::TaskId.eq(&task_id))
                    .filter(delegation_task_run::Column::Status.eq(DelegationRunStatus::Running))
                    .exec(txn)
                    .await
                    .map_err(map_db_err)?;

                    if result.rows_affected == 0 {
                        let row = DelegationTaskRun::find_by_id(&task_id)
                            .one(txn)
                            .await
                            .map_err(map_db_err)?;
                        return match row {
                            Some(r)
                                if matches!(
                                    r.status,
                                    DelegationRunStatus::Completed
                                        | DelegationRunStatus::Failed
                                        | DelegationRunStatus::Canceled
                                ) =>
                            {
                                // Frozen terminal: benign no-op.
                                Ok(())
                            }
                            Some(_) => Err(TaskStoreError::Permanent(format!(
                                "running runtime_stats write matched no rows for still-running task {task_id}"
                            ))),
                            None => Err(TaskStoreError::Permanent(format!(
                                "running runtime_stats write matched no rows; task {task_id} missing"
                            ))),
                        };
                    }

                    let row = DelegationTaskRun::find_by_id(&task_id)
                        .one(txn)
                        .await
                        .map_err(map_db_err)?
                        .ok_or_else(|| {
                            TaskStoreError::Permanent(format!(
                                "run {task_id} missing after runtime_stats write"
                            ))
                        })?;

                    // Monotonic conversation projection for this generation.
                    // Nested Option on line totals: always write (including NULL).
                    let mut projection = ConversationProjection {
                        generation: row.generation,
                        task_status: None,
                        error_code: None,
                        finished_at: None,
                        conversation_status: None,
                        started_at: None,
                        tool_call_count: None,
                        edit_tool_call_count: None,
                        touched_files_json: None,
                        touched_files_truncated: None,
                        additions: None,
                        deletions: None,
                        line_counts_complete: None,
                        reset_generation_rollups: false,
                    };
                    fill_projection_runtime_stats(&mut projection, &encoded);
                    project_conversation_in_txn(txn, row.child_conversation_id, projection)
                        .await
                        .map_err(map_db_err)?;
                    Ok(())
                })
            })
            .await;

        match outcome {
            Ok(()) => Ok(()),
            Err(sea_orm::TransactionError::Connection(e)) => Err(map_db_err(e)),
            Err(sea_orm::TransactionError::Transaction(e)) => Err(e),
        }
    }
}

fn max_utc(a: DateTime<Utc>, b: DateTime<Utc>) -> DateTime<Utc> {
    if a >= b {
        a
    } else {
        b
    }
}

fn promote_connection_matches(run: &PersistedRun, expected: &str) -> bool {
    match run.child_connection_id.as_deref() {
        None => true, // Task 1 legacy unbound running
        Some(id) => id == expected,
    }
}

enum PromoteTxnResult {
    Promoted(PersistedRun),
    ZeroRowClaim,
}

/// Promote transaction error: preserves raw [`sea_orm::DbErr`] so SQLite codes
/// can be extracted before stringification. Logic outcomes stay typed.
#[derive(Debug)]
enum PromoteTxnError {
    Db(sea_orm::DbErr),
    BudgetExhausted(String),
    NotFound(String),
    /// Known logical conflict (e.g. generation fence soft-miss). Rolls back
    /// the txn and surfaces as [`PromoteRunningKind::StateConflict`] without
    /// ambiguous commit reread.
    StateConflict {
        class: PromoteConflictClass,
        message: String,
    },
    Permanent(String),
}

impl std::fmt::Display for PromoteTxnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(e) => write!(f, "{e}"),
            Self::BudgetExhausted(m) => write!(f, "budget exhausted: {m}"),
            Self::NotFound(id) => write!(f, "not found: {id}"),
            Self::StateConflict { message, .. } => write!(f, "{message}"),
            Self::Permanent(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for PromoteTxnError {}

enum PromoteOnceError {
    Retry {
        class: PromoteRetryClass,
        message: String,
    },
}

/// Synthetic DbErr for in-txn inject tests. Display carries primary/extended
/// markers so [`classify_sqlite_transient`] / message fallback classify the
/// same way as real sqlx Database errors when codes are present in text.
#[cfg(any(test, feature = "test-utils"))]
fn synthetic_transient_db_err(class: SqliteTransientClass) -> sea_orm::DbErr {
    let msg = match class {
        SqliteTransientClass::Busy => "(code: 5) database is locked",
        SqliteTransientClass::Locked => "(code: 6) database table is locked",
        SqliteTransientClass::BusySnapshot => "(code: 517) database is locked",
    };
    sea_orm::DbErr::Custom(msg.into())
}

/// Budget charge for promote: SQL errors stay as raw [`PromoteTxnError::Db`].
async fn charge_unexpected_continue_promote(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
    parent_conversation_id: i32,
    work_unit_key: Option<&str>,
) -> Result<(), PromoteTxnError> {
    ensure_budget_rows_promote(
        txn,
        lineage_root_task_id,
        parent_conversation_id,
        work_unit_key,
    )
    .await?;

    let lineage_result = LineageBudget::update_many()
        .col_expr(
            delegation_lineage_budget::Column::UnexpectedContinueCount,
            Expr::col(delegation_lineage_budget::Column::UnexpectedContinueCount).add(1),
        )
        .filter(delegation_lineage_budget::Column::LineageRootTaskId.eq(lineage_root_task_id))
        .filter(
            delegation_lineage_budget::Column::UnexpectedContinueCount
                .lt(UNEXPECTED_CONTINUE_LIMIT),
        )
        .exec(txn)
        .await
        .map_err(PromoteTxnError::Db)?;
    if lineage_result.rows_affected != 1 {
        return Err(PromoteTxnError::BudgetExhausted(format!(
            "unexpected_continue lineage charge refused for {lineage_root_task_id}"
        )));
    }

    if let Some(key) = work_unit_key {
        let wu_result = WorkUnitBudget::update_many()
            .col_expr(
                delegation_work_unit_budget::Column::UnexpectedContinueCount,
                Expr::col(delegation_work_unit_budget::Column::UnexpectedContinueCount).add(1),
            )
            .filter(
                delegation_work_unit_budget::Column::ParentConversationId
                    .eq(parent_conversation_id),
            )
            .filter(delegation_work_unit_budget::Column::WorkUnitKey.eq(key))
            .filter(
                delegation_work_unit_budget::Column::UnexpectedContinueCount
                    .lt(UNEXPECTED_CONTINUE_LIMIT),
            )
            .exec(txn)
            .await
            .map_err(PromoteTxnError::Db)?;
        if wu_result.rows_affected != 1 {
            return Err(PromoteTxnError::BudgetExhausted(format!(
                "unexpected_continue work-unit charge refused for ({parent_conversation_id}, {key})"
            )));
        }
    }
    Ok(())
}

async fn charge_replacement_promote(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
    parent_conversation_id: i32,
    work_unit_key: Option<&str>,
) -> Result<(), PromoteTxnError> {
    ensure_budget_rows_promote(
        txn,
        lineage_root_task_id,
        parent_conversation_id,
        work_unit_key,
    )
    .await?;

    let lineage_result = LineageBudget::update_many()
        .col_expr(
            delegation_lineage_budget::Column::ReplacementCount,
            Expr::col(delegation_lineage_budget::Column::ReplacementCount).add(1),
        )
        .filter(delegation_lineage_budget::Column::LineageRootTaskId.eq(lineage_root_task_id))
        .filter(delegation_lineage_budget::Column::ReplacementCount.lt(REPLACEMENT_LIMIT))
        .exec(txn)
        .await
        .map_err(PromoteTxnError::Db)?;
    if lineage_result.rows_affected != 1 {
        return Err(PromoteTxnError::BudgetExhausted(format!(
            "replacement lineage charge refused for {lineage_root_task_id}"
        )));
    }

    if let Some(key) = work_unit_key {
        let wu_result = WorkUnitBudget::update_many()
            .col_expr(
                delegation_work_unit_budget::Column::ReplacementCount,
                Expr::col(delegation_work_unit_budget::Column::ReplacementCount).add(1),
            )
            .filter(
                delegation_work_unit_budget::Column::ParentConversationId
                    .eq(parent_conversation_id),
            )
            .filter(delegation_work_unit_budget::Column::WorkUnitKey.eq(key))
            .filter(delegation_work_unit_budget::Column::ReplacementCount.lt(REPLACEMENT_LIMIT))
            .exec(txn)
            .await
            .map_err(PromoteTxnError::Db)?;
        if wu_result.rows_affected != 1 {
            return Err(PromoteTxnError::BudgetExhausted(format!(
                "replacement work-unit charge refused for ({parent_conversation_id}, {key})"
            )));
        }
    }
    Ok(())
}

async fn ensure_budget_rows_promote(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
    parent_conversation_id: i32,
    work_unit_key: Option<&str>,
) -> Result<(), PromoteTxnError> {
    ensure_lineage_budget_promote(txn, lineage_root_task_id).await?;
    if let Some(key) = work_unit_key {
        ensure_work_unit_budget_promote(txn, parent_conversation_id, key).await?;
    }
    Ok(())
}

async fn ensure_lineage_budget_promote(
    txn: &DatabaseTransaction,
    lineage_root_task_id: &str,
) -> Result<(), PromoteTxnError> {
    let model = delegation_lineage_budget::ActiveModel {
        lineage_root_task_id: Set(lineage_root_task_id.to_string()),
        unexpected_continue_count: Set(0),
        replacement_count: Set(0),
    };
    match LineageBudget::insert(model)
        .on_conflict(
            OnConflict::column(delegation_lineage_budget::Column::LineageRootTaskId)
                .do_nothing()
                .to_owned(),
        )
        .exec(txn)
        .await
    {
        Ok(_) => Ok(()),
        Err(sea_orm::DbErr::RecordNotInserted) => Ok(()),
        Err(err) => Err(PromoteTxnError::Db(err)),
    }
}

async fn ensure_work_unit_budget_promote(
    txn: &DatabaseTransaction,
    parent_conversation_id: i32,
    work_unit_key: &str,
) -> Result<(), PromoteTxnError> {
    let model = delegation_work_unit_budget::ActiveModel {
        parent_conversation_id: Set(parent_conversation_id),
        work_unit_key: Set(work_unit_key.to_string()),
        unexpected_continue_count: Set(0),
        replacement_count: Set(0),
    };
    match WorkUnitBudget::insert(model)
        .on_conflict(
            OnConflict::columns([
                delegation_work_unit_budget::Column::ParentConversationId,
                delegation_work_unit_budget::Column::WorkUnitKey,
            ])
            .do_nothing()
            .to_owned(),
        )
        .exec(txn)
        .await
    {
        Ok(_) => Ok(()),
        Err(sea_orm::DbErr::RecordNotInserted) => Ok(()),
        Err(err) => Err(PromoteTxnError::Db(err)),
    }
}

/// Encoded runtime rollup columns ready for SeaORM writes.
#[derive(Debug, Clone)]
struct EncodedRuntimeStats {
    tool_call_count: i64,
    edit_tool_call_count: i64,
    touched_files_json: String,
    touched_files_truncated: bool,
    additions: Option<i64>,
    deletions: Option<i64>,
    line_counts_complete: bool,
}

fn encoded_runtime_stats(
    stats: &DelegationRuntimeStats,
) -> Result<EncodedRuntimeStats, TaskStoreError> {
    let tool_call_count = i64::try_from(stats.tool_call_count)
        .map_err(|_| TaskStoreError::Permanent("runtime tool_call_count exceeds i64".into()))?;
    let edit_tool_call_count = i64::try_from(stats.edit_tool_call_count).map_err(|_| {
        TaskStoreError::Permanent("runtime edit_tool_call_count exceeds i64".into())
    })?;
    let additions = stats
        .additions
        .map(i64::try_from)
        .transpose()
        .map_err(|_| TaskStoreError::Permanent("runtime additions exceeds i64".into()))?;
    let deletions = stats
        .deletions
        .map(i64::try_from)
        .transpose()
        .map_err(|_| TaskStoreError::Permanent("runtime deletions exceeds i64".into()))?;
    let touched_files_json = serde_json::to_string(&stats.touched_files).map_err(|err| {
        TaskStoreError::Permanent(format!("serialize touched_files failed: {err}"))
    })?;
    Ok(EncodedRuntimeStats {
        tool_call_count,
        edit_tool_call_count,
        touched_files_json,
        touched_files_truncated: stats.touched_files_truncated,
        additions,
        deletions,
        line_counts_complete: stats.line_counts_complete,
    })
}

fn apply_encoded_runtime_stats_to_run_update(
    update: sea_orm::UpdateMany<delegation_task_run::Entity>,
    stats: &EncodedRuntimeStats,
) -> sea_orm::UpdateMany<delegation_task_run::Entity> {
    update
        .col_expr(
            delegation_task_run::Column::ToolCallCount,
            sea_orm::sea_query::Expr::value(stats.tool_call_count),
        )
        .col_expr(
            delegation_task_run::Column::EditToolCallCount,
            sea_orm::sea_query::Expr::value(stats.edit_tool_call_count),
        )
        .col_expr(
            delegation_task_run::Column::TouchedFilesJson,
            sea_orm::sea_query::Expr::value(stats.touched_files_json.clone()),
        )
        .col_expr(
            delegation_task_run::Column::TouchedFilesTruncated,
            sea_orm::sea_query::Expr::value(stats.touched_files_truncated),
        )
        .col_expr(
            delegation_task_run::Column::Additions,
            sea_orm::sea_query::Expr::value(stats.additions),
        )
        .col_expr(
            delegation_task_run::Column::Deletions,
            sea_orm::sea_query::Expr::value(stats.deletions),
        )
        .col_expr(
            delegation_task_run::Column::LineCountsComplete,
            sea_orm::sea_query::Expr::value(stats.line_counts_complete),
        )
}

fn fill_projection_runtime_stats(
    projection: &mut ConversationProjection,
    stats: &EncodedRuntimeStats,
) {
    projection.tool_call_count = Some(stats.tool_call_count);
    projection.edit_tool_call_count = Some(stats.edit_tool_call_count);
    projection.touched_files_json = Some(stats.touched_files_json.clone());
    projection.touched_files_truncated = Some(stats.touched_files_truncated);
    // Always write line totals, including NULL when unknown.
    projection.additions = Some(stats.additions);
    projection.deletions = Some(stats.deletions);
    projection.line_counts_complete = Some(stats.line_counts_complete);
}

/// Returns whether the generation fence accepted the write.
///
/// Preserves raw [`sea_orm::DbErr`] so promote can classify SQLite transient
/// codes **before** stringification. Callers that want [`TaskStoreError`]
/// should map with [`map_db_err`] outside the promote path.
async fn project_conversation_in_txn(
    txn: &DatabaseTransaction,
    child_conversation_id: i32,
    projection: ConversationProjection,
) -> Result<bool, sea_orm::DbErr> {
    let now = Utc::now();
    let mut update = conversation::Entity::update_many()
        .col_expr(
            conversation::Column::DelegationRunGeneration,
            sea_orm::sea_query::Expr::value(projection.generation),
        )
        .col_expr(
            conversation::Column::UpdatedAt,
            sea_orm::sea_query::Expr::value(now),
        )
        .filter(conversation::Column::Id.eq(child_conversation_id))
        .filter(
            conversation::Column::DelegationRunGeneration
                .is_null()
                .or(conversation::Column::DelegationRunGeneration.lte(projection.generation)),
        );

    if let Some(ref status) = projection.task_status {
        update = update.col_expr(
            conversation::Column::DelegationTaskStatus,
            sea_orm::sea_query::Expr::value(status.clone()),
        );
    }
    // Nested Option for error_code: outer Some = write (inner may be NULL).
    if let Some(ref error_code) = projection.error_code {
        update = update.col_expr(
            conversation::Column::DelegationErrorCode,
            sea_orm::sea_query::Expr::value(error_code.clone()),
        );
    } else if projection.task_status.is_some() {
        // Backward compat: when task_status was set without explicit error_code,
        // leave the column unchanged.
    }
    // Nested Option for finished_at: outer Some = write (inner may be NULL).
    if let Some(finished_at) = projection.finished_at {
        update = update.col_expr(
            conversation::Column::DelegationFinishedAt,
            sea_orm::sea_query::Expr::value(finished_at),
        );
    }
    if let Some(started_at) = projection.started_at {
        update = update.col_expr(
            conversation::Column::DelegationStartedAt,
            sea_orm::sea_query::Expr::value(started_at),
        );
    }
    if let Some(ref status) = projection.conversation_status {
        update = update.col_expr(
            conversation::Column::Status,
            sea_orm::sea_query::Expr::value(status.clone()),
        );
    }
    if projection.tool_call_count.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationToolCallCount,
            sea_orm::sea_query::Expr::value(projection.tool_call_count),
        );
    }
    if projection.edit_tool_call_count.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationEditToolCallCount,
            sea_orm::sea_query::Expr::value(projection.edit_tool_call_count),
        );
    }
    if projection.touched_files_json.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationTouchedFilesJson,
            sea_orm::sea_query::Expr::value(projection.touched_files_json.clone()),
        );
    }
    if projection.touched_files_truncated.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationTouchedFilesTruncated,
            sea_orm::sea_query::Expr::value(projection.touched_files_truncated),
        );
    }
    // Nested Option: outer Some means "write this value" (inner may be NULL).
    if let Some(additions) = projection.additions {
        update = update.col_expr(
            conversation::Column::DelegationAdditions,
            sea_orm::sea_query::Expr::value(additions),
        );
    }
    if let Some(deletions) = projection.deletions {
        update = update.col_expr(
            conversation::Column::DelegationDeletions,
            sea_orm::sea_query::Expr::value(deletions),
        );
    }
    if projection.line_counts_complete.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationLineCountsComplete,
            sea_orm::sea_query::Expr::value(projection.line_counts_complete),
        );
    }
    // Promote-time generation rollup reset writes NULL to all runtime and
    // line-count columns, regardless of their per-field is_some guards above.
    if projection.reset_generation_rollups {
        for (col, val) in [
            (
                conversation::Column::DelegationToolCallCount,
                sea_orm::sea_query::Expr::value(None::<i64>),
            ),
            (
                conversation::Column::DelegationEditToolCallCount,
                sea_orm::sea_query::Expr::value(None::<i64>),
            ),
            (
                conversation::Column::DelegationTouchedFilesJson,
                sea_orm::sea_query::Expr::value(None::<String>),
            ),
            (
                conversation::Column::DelegationTouchedFilesTruncated,
                sea_orm::sea_query::Expr::value(None::<bool>),
            ),
            (
                conversation::Column::DelegationAdditions,
                sea_orm::sea_query::Expr::value(None::<i64>),
            ),
            (
                conversation::Column::DelegationDeletions,
                sea_orm::sea_query::Expr::value(None::<i64>),
            ),
            (
                conversation::Column::DelegationLineCountsComplete,
                sea_orm::sea_query::Expr::value(None::<bool>),
            ),
        ] {
            update = update.col_expr(col, val);
        }
    }

    let result = update.exec(txn).await?;
    Ok(result.rows_affected > 0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::spawner::DelegationLink;
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
    use crate::models::AgentType;

    // ---- task_preview -------------------------------------------------------

    #[test]
    fn preview_redacts_bearer_token() {
        let preview = derive_task_preview("Auth: Bearer sk-live-secret-value-here end");
        assert!(preview.contains("[redacted]"), "{preview}");
        assert!(!preview.contains("Bearer "), "{preview}");
        assert!(!preview.to_ascii_lowercase().contains("sk-live"));
    }

    #[test]
    fn preview_redacts_sk_prefix() {
        let preview = derive_task_preview("key=sk-abc123XYZ rest");
        assert!(preview.contains("[redacted]"));
        assert!(!preview.contains("sk-abc123XYZ"));
    }

    #[test]
    fn preview_redacts_github_tokens() {
        let ghp = derive_task_preview("token ghp_abcdefghijklmnopqrstuv");
        assert!(ghp.contains("[redacted]"));
        assert!(!ghp.contains("ghp_"));

        let pat = derive_task_preview("token github_pat_11AAAA_abcdefghijklmnopqrstuvwxyz");
        assert!(pat.contains("[redacted]"));
        assert!(!pat.contains("github_pat_"));
    }

    #[test]
    fn preview_redacts_gitlab_slack_aws_pem() {
        let gl = derive_task_preview("glpat-abc_def-123");
        assert!(gl.contains("[redacted]"));
        assert!(!gl.contains("glpat-"));

        let xox = derive_task_preview("xoxb-1234567890-token");
        assert!(xox.contains("[redacted]"));
        assert!(!xox.contains("xoxb-"));

        let akia = derive_task_preview("AKIAIOSFODNN7EXAMPLE");
        assert!(akia.contains("[redacted]"));
        assert!(!akia.contains("AKIA"));

        let pem = derive_task_preview(
            "before\n-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\nafter",
        );
        assert!(pem.contains("[redacted]"));
        assert!(!pem.contains("BEGIN PRIVATE KEY"));
        assert!(pem.contains("before"));
        assert!(pem.contains("after"));
    }

    #[test]
    fn preview_length_bound_after_redaction() {
        let long = "a".repeat(500);
        let preview = derive_task_preview(&long);
        assert_eq!(preview.chars().count(), TASK_PREVIEW_SCALAR_CAP);

        // Redaction expansion cannot push past 200 scalars.
        let secret = format!("prefix {} suffix", "sk-".to_string() + &"x".repeat(300));
        let preview = derive_task_preview(&secret);
        assert!(preview.chars().count() <= TASK_PREVIEW_SCALAR_CAP);
        assert!(preview.contains("[redacted]"));
    }

    #[test]
    fn preview_fail_closed_empty() {
        assert_eq!(derive_task_preview(""), "");
    }

    #[test]
    fn preview_counts_unicode_scalars_not_bytes() {
        // Each emoji is one scalar; 201 emojis → 200 kept.
        let s = "😀".repeat(201);
        let preview = derive_task_preview(&s);
        assert_eq!(preview.chars().count(), 200);
    }

    // ---- request_fingerprint ------------------------------------------------

    #[test]
    fn fingerprint_is_stable_for_identical_inputs() {
        let a = request_fingerprint(
            "delegate_to_agent",
            "do the thing",
            Some("work-1"),
            None,
            None,
            None,
            "AbCdEf",
        );
        let b = request_fingerprint(
            "delegate_to_agent",
            "do the thing",
            Some("work-1"),
            None,
            None,
            None,
            "abcdef", // case-normalized
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, a.to_ascii_lowercase());
    }

    #[test]
    fn fingerprint_empty_optionals_match_explicit_empty_strings() {
        let none_side = request_fingerprint(
            "continue_delegation",
            "revise",
            None,
            None,
            None,
            Some("task-1"),
            "deadbeef",
        );
        let empty_side = request_fingerprint(
            "continue_delegation",
            "revise",
            Some(""),
            Some(""),
            Some(""),
            Some("task-1"),
            "deadbeef",
        );
        assert_eq!(none_side, empty_side);
    }

    #[test]
    fn fingerprint_nfc_normalizes_task_text() {
        // U+00E9 (é) vs e + combining acute U+0301
        let composed = "caf\u{00e9}";
        let decomposed = "cafe\u{0301}";
        assert_ne!(composed.as_bytes(), decomposed.as_bytes());
        let a = request_fingerprint("delegate_to_agent", composed, None, None, None, None, "aa");
        let b = request_fingerprint(
            "delegate_to_agent",
            decomposed,
            None,
            None,
            None,
            None,
            "aa",
        );
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_field_order_matters() {
        let a = request_fingerprint("t1", "task", Some("w"), None, None, None, "r1");
        let b = request_fingerprint("t1", "task", None, Some("w"), None, None, "r1");
        assert_ne!(a, b);
    }

    #[test]
    fn request_fingerprint_api_excludes_correlation_id() {
        // Call-instance correlation_id must never enter durable request identity.
        // The API surface has no correlation parameter; equal semantic inputs
        // (including two "would-be" different correlation tokens) yield one
        // fingerprint. This pins the Task 4 invariant at the hash boundary.
        let a = request_fingerprint(
            "continue_delegation",
            "revise",
            Some("work-1"),
            None,
            None,
            Some("task-1"),
            "deadbeef",
        );
        let b = request_fingerprint(
            "continue_delegation",
            "revise",
            Some("work-1"),
            None,
            None,
            Some("task-1"),
            "deadbeef",
        );
        assert_eq!(a, b);
        // Stable 64-char hex — no room for an opaque correlation token.
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fingerprint_delimiter_in_fields_does_not_collide() {
        // Raw U+001E join collides these distinct 7-tuples:
        //   tool="a", task="b\u{1e}c"  →  a RS b RS c …
        //   tool="a\u{1e}b", task="c" →  a RS b RS c …
        // Length-framed / JSON encoding must keep them distinct.
        let rs = '\u{1e}';
        let left = request_fingerprint("a", &format!("b{rs}c"), None, None, None, None, "deadbeef");
        let right =
            request_fingerprint(&format!("a{rs}b"), "c", None, None, None, None, "deadbeef");
        assert_ne!(
            left, right,
            "in-field RS must not create identical canonical bytes"
        );

        // Same for optional work_unit_key / replaces_task_id boundary.
        let left = request_fingerprint(
            "t",
            "task",
            Some(&format!("w{rs}x")),
            None,
            None,
            None,
            "aa",
        );
        let right = request_fingerprint("t", "task", Some("w"), Some("x"), None, None, "aa");
        assert_ne!(left, right);
    }

    // ---- RunStore -----------------------------------------------------------

    async fn seed_parent_child(db: &AppDatabase, call_id: &str) -> (i32, i32) {
        let folder = seed_folder(db, "/tmp/codeg-run-store").await;
        let parent = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .expect("parent");
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: format!("tu-{call_id}"),
                delegation_call_id: call_id.into(),
            }),
        )
        .await
        .expect("child");
        (parent.id, child.id)
    }

    fn sample_insert(
        task_id: &str,
        parent_id: i32,
        child_id: i32,
        generation: i64,
        previous: Option<&str>,
    ) -> ReservingRunInsert {
        ReservingRunInsert {
            task_id: task_id.into(),
            root_task_id: previous
                .map(|_| "root-task".into())
                .unwrap_or_else(|| task_id.into()),
            previous_task_id: previous.map(|s| s.into()),
            generation,
            parent_conversation_id: parent_id,
            parent_tool_use_id: Some(format!("tu-{task_id}")),
            child_conversation_id: child_id,
            agent_type: "codex".into(),
            profile_id: None,
            workspace_path: Some("/tmp/ws".into()),
            route_fingerprint: Some("aabbccdd".into()),
            launch_snapshot_version: Some("v1".into()),
            mode_id: Some("default".into()),
            config_values_json: Some("{}".into()),
            task_preview: Some(derive_task_preview("do work")),
            request_fingerprint: Some(request_fingerprint(
                "delegate_to_agent",
                "do work",
                None,
                None,
                None,
                None,
                "aabbccdd",
            )),
            admission_class: AdmissionClass::NormalRevision,
            lineage_root_task_id: previous
                .map(|_| "root-task".into())
                .unwrap_or_else(|| task_id.into()),
            work_unit_key: Some("unit-a".into()),
            history_only: false,
            replaced_task_id: None,
            replacement_reason: None,
            started_at: Some(Utc::now()),
        }
    }

    /// Parent-end durable sweep may settle a pure reserving claim before
    /// inflight abandon runs. Abandon must reclaim that canceled provisional
    /// shape so compensation can hide the unused child shell.
    #[tokio::test]
    async fn abandon_reserving_claim_reclaims_just_settled_canceled_provisional() {
        let db = Arc::new(fresh_in_memory_db().await);
        let store = RunStore::new(db.clone());
        let task_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let (parent_id, child_id) = seed_parent_child(&db, task_id).await;
        let insert = sample_insert(task_id, parent_id, child_id, 1, None);
        store
            .admit_gen1_reserving(insert)
            .await
            .expect("admit");

        store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::canceled(
                    "parent_canceled",
                    Utc::now(),
                    ConversationStatus::Cancelled,
                ),
            )
            .await
            .expect("parent-end settle");

        let abandoned = store
            .abandon_reserving_claim(task_id)
            .await
            .expect("abandon");
        assert!(
            abandoned,
            "must reclaim just-settled pure provisional canceled claim"
        );
        assert!(
            store
                .load_by_task_id(task_id)
                .await
                .expect("load")
                .is_none(),
            "reclaimed claim must not remain durable"
        );
        // Child without external session remains for compensation; run is gone.
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("child")
            .expect("row");
        assert!(child.external_id.is_none());
    }

    #[tokio::test]
    async fn abandon_reserving_claim_does_not_reclaim_when_external_session_exists() {
        use sea_orm::{ActiveModelTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let store = RunStore::new(db.clone());
        let task_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let (parent_id, child_id) = seed_parent_child(&db, task_id).await;
        let insert = sample_insert(task_id, parent_id, child_id, 1, None);
        store
            .admit_gen1_reserving(insert)
            .await
            .expect("admit");
        store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::canceled(
                    "parent_canceled",
                    Utc::now(),
                    ConversationStatus::Cancelled,
                ),
            )
            .await
            .expect("settle");

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("child")
            .expect("row");
        let mut active = child.into_active_model();
        active.external_id = Set(Some("sess-real".into()));
        active.update(&db.conn).await.expect("set external");

        let abandoned = store
            .abandon_reserving_claim(task_id)
            .await
            .expect("abandon");
        assert!(
            !abandoned,
            "must not reclaim when child has an external session"
        );
        assert!(
            store
                .load_by_task_id(task_id)
                .await
                .expect("load")
                .is_some(),
            "non-provisional canceled run must remain"
        );
    }

    #[tokio::test]
    async fn insert_promote_settle_round_trip() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "aaaaaaaa-1111-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let task_id = "aaaaaaaa-1111-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");

        let loaded = store.load_by_task_id(task_id).await.unwrap().unwrap();
        assert_eq!(loaded.run_status, DelegationRunStatus::Reserving);
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.parent_conversation_id, parent_id);

        store
            .promote_running(task_id, "conn-1", Utc::now())
            .await
            .expect("promote");
        let running = store.load_by_task_id(task_id).await.unwrap().unwrap();
        assert_eq!(running.run_status, DelegationRunStatus::Running);
        assert_eq!(running.child_connection_id.as_deref(), Some("conn-1"));
        assert!(running.reached_running_at.is_some());

        let settlement = store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .expect("settle");
        assert!(settlement.won());
        let done = store.load_by_task_id(task_id).await.unwrap().unwrap();
        assert_eq!(done.status, TaskStatus::Completed);
        assert!(done.finished_at.is_some());

        // Conversation projection fence.
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_run_generation, Some(1));
        assert_eq!(
            child.delegation_task_status,
            Some(DelegationTaskStatus::Completed)
        );
    }

    #[tokio::test]
    async fn settle_cas_has_one_winner() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bbbbbbbb-2222-4222-8222-222222222222").await;
        let store = RunStore::new(db);
        let task_id = "bbbbbbbb-2222-4222-8222-222222222222";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(task_id, "c", Utc::now())
            .await
            .unwrap();

        let (a, b) = tokio::join!(
            store.settle_terminal(
                task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            ),
            store.settle_terminal(
                task_id,
                TerminalTaskWrite::canceled(
                    "usercancel",
                    Utc::now(),
                    ConversationStatus::Cancelled
                ),
            ),
        );
        let ra = a.unwrap();
        let rb = b.unwrap();
        assert_eq!(ra.report().status, rb.report().status);
        assert_eq!(ra.report().error_code, rb.report().error_code);
        assert!(ra.won() ^ rb.won(), "exactly one winner");
    }

    #[tokio::test]
    async fn projection_cas_is_monotonic() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (_parent_id, child_id) =
            seed_parent_child(&db, "cccccccc-3333-4333-8333-333333333333").await;
        let store = RunStore::new(db.clone());

        let applied = store
            .project_conversation(
                child_id,
                ConversationProjection {
                    generation: 2,
                    task_status: Some(DelegationTaskStatus::Running),
                    error_code: None,
                    finished_at: None,
                    conversation_status: Some(ConversationStatus::InProgress),
                    started_at: Some(Utc::now()),
                    tool_call_count: None,
                    edit_tool_call_count: None,
                    touched_files_json: None,
                    touched_files_truncated: None,
                    additions: None,
                    deletions: None,
                    line_counts_complete: None,
                    reset_generation_rollups: false,
                },
            )
            .await
            .unwrap();
        assert!(applied);

        // Older generation must not overwrite.
        let older = store
            .project_conversation(
                child_id,
                ConversationProjection {
                    generation: 1,
                    task_status: Some(DelegationTaskStatus::Failed),
                    error_code: Some(Some("stale".into())),
                    finished_at: Some(Some(Utc::now())),
                    conversation_status: Some(ConversationStatus::Cancelled),
                    started_at: None,
                    tool_call_count: None,
                    edit_tool_call_count: None,
                    touched_files_json: None,
                    touched_files_truncated: None,
                    additions: None,
                    deletions: None,
                    line_counts_complete: None,
                    reset_generation_rollups: false,
                },
            )
            .await
            .unwrap();
        assert!(!older);

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_run_generation, Some(2));
        assert_eq!(
            child.delegation_task_status,
            Some(DelegationTaskStatus::Running)
        );
        assert!(child.delegation_error_code.is_none());

        // Equal generation is allowed (idempotent re-project).
        let same = store
            .project_conversation(
                child_id,
                ConversationProjection {
                    generation: 2,
                    task_status: Some(DelegationTaskStatus::Completed),
                    error_code: None,
                    finished_at: Some(Some(Utc::now())),
                    conversation_status: Some(ConversationStatus::PendingReview),
                    started_at: None,
                    tool_call_count: None,
                    edit_tool_call_count: None,
                    touched_files_json: None,
                    touched_files_truncated: None,
                    additions: None,
                    deletions: None,
                    line_counts_complete: None,
                    reset_generation_rollups: false,
                },
            )
            .await
            .unwrap();
        assert!(same);
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            child.delegation_task_status,
            Some(DelegationTaskStatus::Completed)
        );
    }

    #[tokio::test]
    async fn prefix_recovery_is_parent_scoped_on_run_rows() {
        let db = Arc::new(fresh_in_memory_db().await);
        let folder = seed_folder(&db, "/tmp/codeg-run-prefix").await;
        let parent_a = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("pa".into()),
            None,
        )
        .await
        .unwrap();
        let parent_b = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("pb".into()),
            None,
        )
        .await
        .unwrap();
        let child_a = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("ca".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_a.id,
                parent_tool_use_id: "tu-a".into(),
                delegation_call_id: "6b8f8330-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            }),
        )
        .await
        .unwrap();
        let child_b = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("cb".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_b.id,
                parent_tool_use_id: "tu-b".into(),
                // Same 8-char prefix, different parent — must not collide.
                delegation_call_id: "6b8f8330-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
            }),
        )
        .await
        .unwrap();

        let store = RunStore::new(db);
        // Generation-2 continued run: task_id differs from root call_id.
        let cont = "6b8f8330-cccc-4ccc-8ccc-cccccccccccc";
        let root_a = "6b8f8330-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        store
            .insert_reserving(sample_insert(root_a, parent_a.id, child_a.id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(root_a, "ca1", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                root_a,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .unwrap();
        // Second non-terminal run on the same child (partial unique allows one).
        store
            .insert_reserving(sample_insert(
                cont,
                parent_a.id,
                child_a.id,
                2,
                Some(root_a),
            ))
            .await
            .unwrap();
        store
            .insert_reserving(sample_insert(
                "6b8f8330-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                parent_b.id,
                child_b.id,
                1,
                None,
            ))
            .await
            .unwrap();

        // Ambiguous under parent_a (two runs share prefix) → None.
        assert!(store
            .resolve_unique_owned_prefix(parent_a.id, "6b8f8330")
            .await
            .unwrap()
            .is_none());

        // Unique under parent_b.
        assert_eq!(
            store
                .resolve_unique_owned_prefix(parent_b.id, "6b8f8330")
                .await
                .unwrap()
                .as_deref(),
            Some("6b8f8330-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
        );

        // Invalid prefix.
        assert!(store
            .resolve_unique_owned_prefix(parent_b.id, "%%%%%%%%")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .resolve_unique_owned_prefix(parent_b.id, "dead")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn continued_run_load_keys_by_task_id_not_root_call_id() {
        let db = Arc::new(fresh_in_memory_db().await);
        let root = "dddddddd-4444-4444-8444-444444444444";
        let cont = "eeeeeeee-5555-4555-8555-555555555555";
        let (parent_id, child_id) = seed_parent_child(&db, root).await;
        let store = RunStore::new(db);
        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store.promote_running(root, "c1", Utc::now()).await.unwrap();
        store
            .settle_terminal(
                root,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .unwrap();
        store
            .insert_reserving(sample_insert(cont, parent_id, child_id, 2, Some(root)))
            .await
            .unwrap();
        store.promote_running(cont, "c2", Utc::now()).await.unwrap();

        let cont_row = store.load_by_task_id(cont).await.unwrap().unwrap();
        assert_eq!(cont_row.generation, 2);
        assert_eq!(cont_row.previous_task_id.as_deref(), Some(root));
        assert_eq!(cont_row.child_conversation_id, child_id);
        assert_eq!(cont_row.run_status, DelegationRunStatus::Running);

        // Root remains independently loadable and terminal.
        let root_row = store.load_by_task_id(root).await.unwrap().unwrap();
        assert_eq!(root_row.status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn running_runtime_stats_project_conversation_with_cas() {
        use crate::acp::delegation::runtime_stats::{
            DelegationRuntimeStats, DelegationTouchedFile,
        };

        let db = Arc::new(fresh_in_memory_db().await);
        let task_id = "ffffffff-6666-4666-8666-666666666666";
        let (parent_id, child_id) = seed_parent_child(&db, task_id).await;
        let store = RunStore::new(db.clone());
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(task_id, "conn-rt", Utc::now())
            .await
            .unwrap();

        let started = store
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap()
            .started_at
            .expect("started_at");
        let stats = DelegationRuntimeStats {
            started_at: started,
            finished_at: None,
            tool_call_count: 4,
            edit_tool_call_count: 1,
            touched_files: vec![DelegationTouchedFile {
                path: "src/lib.rs".into(),
                outside_workspace: false,
                additions: Some(3),
                deletions: Some(1),
            }],
            touched_files_truncated: false,
            additions: Some(3),
            deletions: Some(1),
            line_counts_complete: true,
        };
        store
            .write_runtime_stats(task_id, &stats)
            .await
            .expect("running stats write");

        let run = store.load_by_task_id(task_id).await.unwrap().unwrap();
        let run_stats = run.runtime_stats.expect("run runtime_stats");
        assert_eq!(run_stats.tool_call_count, 4);
        assert_eq!(run_stats.edit_tool_call_count, 1);
        assert_eq!(run_stats.additions, Some(3));
        assert_eq!(run_stats.deletions, Some(1));
        assert!(run_stats.line_counts_complete);

        // Conversation latest-run projection must receive the same rollup under
        // the run's generation fence.
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_run_generation, Some(1));
        assert_eq!(child.delegation_tool_call_count, Some(4));
        assert_eq!(child.delegation_edit_tool_call_count, Some(1));
        assert_eq!(child.delegation_additions, Some(3));
        assert_eq!(child.delegation_deletions, Some(1));
        assert_eq!(child.delegation_line_counts_complete, Some(true));
        assert_eq!(child.delegation_touched_files_truncated, Some(false));
        let files = child.delegation_touched_files_json.expect("touched json");
        assert!(files.contains("src/lib.rs"));
    }

    #[tokio::test]
    async fn terminal_run_runtime_stats_frozen_after_settle() {
        use crate::acp::delegation::runtime_stats::{
            DelegationRuntimeStats, DelegationTouchedFile,
        };

        let db = Arc::new(fresh_in_memory_db().await);
        let task_id = "10101010-7777-4777-8777-777777777777";
        let (parent_id, child_id) = seed_parent_child(&db, task_id).await;
        let store = RunStore::new(db.clone());
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(task_id, "conn-freeze", Utc::now())
            .await
            .unwrap();

        let started = store
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap()
            .started_at
            .expect("started_at");
        let running_stats = DelegationRuntimeStats {
            started_at: started,
            finished_at: None,
            tool_call_count: 2,
            edit_tool_call_count: 1,
            touched_files: vec![DelegationTouchedFile {
                path: "running.rs".into(),
                outside_workspace: false,
                additions: Some(1),
                deletions: Some(0),
            }],
            touched_files_truncated: false,
            additions: Some(1),
            deletions: Some(0),
            // Must satisfy decode invariants (complete ⇔ additions present).
            line_counts_complete: true,
        };
        store
            .write_runtime_stats(task_id, &running_stats)
            .await
            .expect("running snapshot");

        let finished = Utc::now();
        store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::completed(finished, ConversationStatus::PendingReview),
            )
            .await
            .expect("settle");

        let frozen = store
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap()
            .runtime_stats
            .clone();

        // Post-settle terminal write must not mutate the frozen run.
        let terminal_attempt = DelegationRuntimeStats {
            started_at: started,
            finished_at: Some(finished),
            tool_call_count: 99,
            edit_tool_call_count: 88,
            touched_files: vec![DelegationTouchedFile {
                path: "after-settle.rs".into(),
                outside_workspace: false,
                additions: Some(9),
                deletions: Some(9),
            }],
            touched_files_truncated: true,
            additions: Some(9),
            deletions: Some(9),
            line_counts_complete: true,
        };
        store
            .write_runtime_stats(task_id, &terminal_attempt)
            .await
            .expect("frozen terminal write is benign no-op");

        let after_terminal = store.load_by_task_id(task_id).await.unwrap().unwrap();
        assert_eq!(after_terminal.runtime_stats, frozen);
        assert_eq!(
            after_terminal
                .runtime_stats
                .as_ref()
                .map(|s| s.tool_call_count),
            Some(2)
        );

        // Stale running write remains benign and non-mutating.
        let stale_running = DelegationRuntimeStats {
            started_at: started,
            finished_at: None,
            tool_call_count: 1,
            edit_tool_call_count: 0,
            touched_files: vec![],
            touched_files_truncated: false,
            additions: None,
            deletions: None,
            line_counts_complete: false,
        };
        store
            .write_runtime_stats(task_id, &stale_running)
            .await
            .expect("stale running write is benign");
        let after_stale = store.load_by_task_id(task_id).await.unwrap().unwrap();
        assert_eq!(after_stale.runtime_stats, frozen);

        // Conversation projection must retain the pre-settle rollup (not the
        // rejected terminal attempt).
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_tool_call_count, Some(2));
        assert_eq!(child.delegation_edit_tool_call_count, Some(1));
        let files = child.delegation_touched_files_json.unwrap_or_default();
        assert!(files.contains("running.rs"));
        assert!(!files.contains("after-settle.rs"));
    }

    #[tokio::test]
    async fn older_generation_runtime_stats_do_not_overwrite_newer_projection() {
        use crate::acp::delegation::runtime_stats::DelegationRuntimeStats;

        let db = Arc::new(fresh_in_memory_db().await);
        let root = "20202020-8888-4888-8888-888888888888";
        let cont = "30303030-9999-4999-8999-999999999999";
        let (parent_id, child_id) = seed_parent_child(&db, root).await;
        let store = RunStore::new(db.clone());

        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(root, "c-root", Utc::now())
            .await
            .unwrap();
        // Leave gen-1 running so we can attempt a late stats write after gen-2
        // has projected — first settle gen-1 then start gen-2.
        let finished = Utc::now();
        store
            .settle_terminal(
                root,
                TerminalTaskWrite::completed(finished, ConversationStatus::PendingReview),
            )
            .await
            .unwrap();

        store
            .insert_reserving(sample_insert(cont, parent_id, child_id, 2, Some(root)))
            .await
            .unwrap();
        store
            .promote_running(cont, "c-cont", Utc::now())
            .await
            .unwrap();
        let cont_started = store
            .load_by_task_id(cont)
            .await
            .unwrap()
            .unwrap()
            .started_at
            .expect("started");
        let gen2_stats = DelegationRuntimeStats {
            started_at: cont_started,
            finished_at: None,
            tool_call_count: 7,
            edit_tool_call_count: 3,
            touched_files: vec![],
            touched_files_truncated: false,
            additions: Some(5),
            deletions: Some(2),
            line_counts_complete: true,
        };
        store
            .write_runtime_stats(cont, &gen2_stats)
            .await
            .expect("gen2 stats");

        // Force a delayed gen-1 path: re-open is impossible once settled, but
        // project_conversation already fenced at gen 2. Verify CAS rejects an
        // explicit older projection attempt with gen-1 stats payload.
        let rejected = store
            .project_conversation(
                child_id,
                ConversationProjection {
                    generation: 1,
                    task_status: None,
                    error_code: None,
                    finished_at: None,
                    conversation_status: None,
                    started_at: None,
                    tool_call_count: Some(999),
                    edit_tool_call_count: Some(999),
                    touched_files_json: Some("[]".into()),
                    touched_files_truncated: Some(true),
                    additions: Some(Some(999)),
                    deletions: Some(Some(999)),
                    line_counts_complete: Some(false),
                    reset_generation_rollups: false,
                },
            )
            .await
            .unwrap();
        assert!(!rejected);

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_run_generation, Some(2));
        assert_eq!(child.delegation_tool_call_count, Some(7));
        assert_eq!(child.delegation_edit_tool_call_count, Some(3));
        assert_eq!(child.delegation_additions, Some(5));
    }

    #[tokio::test]
    async fn settle_terminal_with_final_runtime_stats_writes_atomically() {
        use crate::acp::delegation::runtime_stats::{
            DelegationRuntimeStats, DelegationTouchedFile,
        };

        let db = Arc::new(fresh_in_memory_db().await);
        let task_id = "40404040-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let (parent_id, child_id) = seed_parent_child(&db, task_id).await;
        let store = RunStore::new(db.clone());
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(task_id, "conn-final", Utc::now())
            .await
            .unwrap();

        let started = store
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap()
            .started_at
            .expect("started_at");
        // Intermediate running snapshot (will be superseded by final settle stats).
        store
            .write_runtime_stats(
                task_id,
                &DelegationRuntimeStats {
                    started_at: started,
                    finished_at: None,
                    tool_call_count: 1,
                    edit_tool_call_count: 0,
                    touched_files: vec![],
                    touched_files_truncated: false,
                    additions: None,
                    deletions: None,
                    line_counts_complete: false,
                },
            )
            .await
            .expect("running snapshot");

        let finished = Utc::now();
        let final_stats = DelegationRuntimeStats {
            started_at: started,
            finished_at: Some(finished),
            tool_call_count: 6,
            edit_tool_call_count: 2,
            touched_files: vec![DelegationTouchedFile {
                path: "final.rs".into(),
                outside_workspace: false,
                additions: Some(4),
                deletions: Some(1),
            }],
            touched_files_truncated: false,
            additions: Some(4),
            deletions: Some(1),
            line_counts_complete: true,
        };
        let settlement = store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::completed(finished, ConversationStatus::PendingReview)
                    .with_runtime_stats(final_stats.clone()),
            )
            .await
            .expect("settle with final stats");
        assert!(settlement.won());

        let run = store.load_by_task_id(task_id).await.unwrap().unwrap();
        let run_stats = run.runtime_stats.expect("terminal runtime_stats");
        assert_eq!(run_stats.tool_call_count, 6);
        assert_eq!(run_stats.edit_tool_call_count, 2);
        assert_eq!(run_stats.additions, Some(4));
        assert_eq!(run_stats.deletions, Some(1));
        assert!(run_stats.line_counts_complete);
        assert_eq!(run_stats.finished_at, Some(finished));

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_run_generation, Some(1));
        assert_eq!(child.delegation_tool_call_count, Some(6));
        assert_eq!(child.delegation_edit_tool_call_count, Some(2));
        assert_eq!(child.delegation_additions, Some(4));
        assert_eq!(child.delegation_deletions, Some(1));
        assert_eq!(child.delegation_line_counts_complete, Some(true));
        let files = child.delegation_touched_files_json.unwrap_or_default();
        assert!(files.contains("final.rs"));

        // Post-terminal write remains frozen (no mutation of settle snapshot).
        let mut after = final_stats.clone();
        after.tool_call_count = 99;
        store
            .write_runtime_stats(task_id, &after)
            .await
            .expect("post-terminal write is benign no-op");
        let frozen = store
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap()
            .runtime_stats
            .expect("still present");
        assert_eq!(frozen.tool_call_count, 6);
    }

    #[tokio::test]
    async fn running_runtime_stats_known_to_unknown_clears_line_totals_on_conversation() {
        use crate::acp::delegation::runtime_stats::{
            DelegationRuntimeStats, DelegationTouchedFile,
        };

        let db = Arc::new(fresh_in_memory_db().await);
        let task_id = "50505050-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let (parent_id, child_id) = seed_parent_child(&db, task_id).await;
        let store = RunStore::new(db.clone());
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(task_id, "conn-clear", Utc::now())
            .await
            .unwrap();

        let started = store
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap()
            .started_at
            .expect("started_at");

        // Known line totals first.
        store
            .write_runtime_stats(
                task_id,
                &DelegationRuntimeStats {
                    started_at: started,
                    finished_at: None,
                    tool_call_count: 3,
                    edit_tool_call_count: 1,
                    touched_files: vec![DelegationTouchedFile {
                        path: "known.rs".into(),
                        outside_workspace: false,
                        additions: Some(10),
                        deletions: Some(2),
                    }],
                    touched_files_truncated: false,
                    additions: Some(10),
                    deletions: Some(2),
                    line_counts_complete: true,
                },
            )
            .await
            .expect("known snapshot");

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_additions, Some(10));
        assert_eq!(child.delegation_deletions, Some(2));
        assert_eq!(child.delegation_line_counts_complete, Some(true));

        // Later incomplete snapshot (projector known→unknown). Must clear
        // nullable line totals on the conversation projection under CAS.
        store
            .write_runtime_stats(
                task_id,
                &DelegationRuntimeStats {
                    started_at: started,
                    finished_at: None,
                    tool_call_count: 4,
                    edit_tool_call_count: 2,
                    touched_files: vec![
                        DelegationTouchedFile {
                            path: "known.rs".into(),
                            outside_workspace: false,
                            additions: Some(10),
                            deletions: Some(2),
                        },
                        DelegationTouchedFile {
                            path: "unknown.rs".into(),
                            outside_workspace: false,
                            additions: None,
                            deletions: None,
                        },
                    ],
                    touched_files_truncated: false,
                    additions: None,
                    deletions: None,
                    line_counts_complete: false,
                },
            )
            .await
            .expect("unknown snapshot");

        let run = store.load_by_task_id(task_id).await.unwrap().unwrap();
        let run_stats = run.runtime_stats.expect("run stats");
        assert_eq!(run_stats.tool_call_count, 4);
        assert_eq!(run_stats.additions, None);
        assert_eq!(run_stats.deletions, None);
        assert!(!run_stats.line_counts_complete);

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_run_generation, Some(1));
        assert_eq!(child.delegation_tool_call_count, Some(4));
        assert_eq!(child.delegation_edit_tool_call_count, Some(2));
        assert_eq!(
            child.delegation_additions, None,
            "known→unknown must clear stale additions"
        );
        assert_eq!(
            child.delegation_deletions, None,
            "known→unknown must clear stale deletions"
        );
        assert_eq!(child.delegation_line_counts_complete, Some(false));
    }

    // ---- Platform recovery budget rails (Task 3) ----------------------------

    #[allow(clippy::too_many_arguments)]
    fn sample_insert_with(
        task_id: &str,
        parent_id: i32,
        child_id: i32,
        generation: i64,
        previous: Option<&str>,
        admission_class: AdmissionClass,
        lineage_root: &str,
        work_unit_key: Option<&str>,
    ) -> ReservingRunInsert {
        let mut insert = sample_insert(task_id, parent_id, child_id, generation, previous);
        insert.admission_class = admission_class;
        insert.lineage_root_task_id = lineage_root.into();
        insert.root_task_id = if generation == 1 {
            task_id.into()
        } else {
            lineage_root.into()
        };
        insert.work_unit_key = work_unit_key.map(|s| s.into());
        insert
    }

    async fn lineage_counts(db: &AppDatabase, lineage_root: &str) -> (i64, i64) {
        use crate::db::entities::delegation_lineage_budget;
        let row = delegation_lineage_budget::Entity::find_by_id(lineage_root)
            .one(&db.conn)
            .await
            .unwrap();
        match row {
            Some(r) => (r.unexpected_continue_count, r.replacement_count),
            None => (0, 0),
        }
    }

    async fn work_unit_counts(db: &AppDatabase, parent_id: i32, work_unit_key: &str) -> (i64, i64) {
        use crate::db::entities::delegation_work_unit_budget::{self, Column};
        use sea_orm::ColumnTrait;
        let row = delegation_work_unit_budget::Entity::find()
            .filter(Column::ParentConversationId.eq(parent_id))
            .filter(Column::WorkUnitKey.eq(work_unit_key))
            .one(&db.conn)
            .await
            .unwrap();
        match row {
            Some(r) => (r.unexpected_continue_count, r.replacement_count),
            None => (0, 0),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn promote_unexpected(
        store: &RunStore,
        parent_id: i32,
        child_id: i32,
        task_id: &str,
        lineage_root: &str,
        work_unit_key: Option<&str>,
        generation: i64,
        previous: Option<&str>,
    ) -> Result<(), TaskStoreError> {
        store
            .insert_reserving(sample_insert_with(
                task_id,
                parent_id,
                child_id,
                generation,
                previous,
                AdmissionClass::UnexpectedContinue,
                lineage_root,
                work_unit_key,
            ))
            .await?;
        store
            .promote_running(task_id, format!("conn-{task_id}"), Utc::now())
            .await
            .map(|_| ())
    }

    async fn settle_completed(store: &RunStore, task_id: &str) {
        store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .expect("settle completed");
    }

    #[tokio::test]
    async fn third_unexpected_continue_is_budget_exhausted() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "budget-uc-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-uc-root";
        let unit = Some("unit-uc");

        promote_unexpected(
            &store,
            parent_id,
            child_id,
            "uc-1",
            lineage,
            unit,
            2,
            Some(lineage),
        )
        .await
        .expect("first unexpected continue");
        // One non-terminal per child: settle before the next reserving insert.
        settle_completed(&store, "uc-1").await;

        promote_unexpected(
            &store,
            parent_id,
            child_id,
            "uc-2",
            lineage,
            unit,
            3,
            Some("uc-1"),
        )
        .await
        .expect("second unexpected continue");
        settle_completed(&store, "uc-2").await;

        let (uc, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc, 2);
        let (wuc, _) = work_unit_counts(&db, parent_id, "unit-uc").await;
        assert_eq!(wuc, 2);

        let err = promote_unexpected(
            &store,
            parent_id,
            child_id,
            "uc-3",
            lineage,
            unit,
            4,
            Some("uc-2"),
        )
        .await
        .expect_err("third unexpected continue must exhaust");
        assert!(
            err.is_budget_exhausted(),
            "expected BudgetExhausted, got {err:?}"
        );

        let (uc_after, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc_after, 2, "counter must not advance past limit");
        let (wuc_after, _) = work_unit_counts(&db, parent_id, "unit-uc").await;
        assert_eq!(wuc_after, 2);
        // No successful third admission.
        assert!(
            store.load_by_task_id("uc-3").await.unwrap().is_none()
                || store
                    .load_by_task_id("uc-3")
                    .await
                    .unwrap()
                    .map(|r| r.run_status != DelegationRunStatus::Running)
                    .unwrap_or(true)
        );
    }

    #[tokio::test]
    async fn second_replacement_is_budget_exhausted() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "budget-rp-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-rp-root";
        let unit = Some("unit-rp");

        store
            .insert_reserving(sample_insert_with(
                "rp-1",
                parent_id,
                child_id,
                1,
                None,
                AdmissionClass::Replacement,
                lineage,
                unit,
            ))
            .await
            .expect("first replacement insert");
        store
            .promote_running("rp-1", "conn-rp-1", Utc::now())
            .await
            .expect("first replacement promote");

        let (_, rc) = lineage_counts(&db, lineage).await;
        assert_eq!(rc, 1);
        let (_, wrc) = work_unit_counts(&db, parent_id, "unit-rp").await;
        assert_eq!(wrc, 1);

        // Terminal settle so the gen-1 work-unit partial unique no longer blocks;
        // the budget rail (not the unique index) must refuse the second attempt.
        store
            .settle_terminal(
                "rp-1",
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        // Replacement is always gen-1 on a new child conversation.
        let folder = seed_folder(&db, "/tmp/codeg-run-store-rp2").await;
        let child2 = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child-rp2".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-rp-2".into(),
                delegation_call_id: "rp-2".into(),
            }),
        )
        .await
        .expect("child2");

        let err = store
            .insert_reserving(sample_insert_with(
                "rp-2",
                parent_id,
                child2.id,
                1,
                None,
                AdmissionClass::Replacement,
                lineage,
                unit,
            ))
            .await
            .expect_err("second replacement must exhaust at preflight");
        assert!(err.is_budget_exhausted(), "got {err:?}");

        let (_, rc_after) = lineage_counts(&db, lineage).await;
        assert_eq!(rc_after, 1, "still one replacement charge after refuse");
        assert!(store.load_by_task_id("rp-2").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn dual_row_lineage_at_limit_rejects_without_partial_work_unit_charge() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "budget-dual-001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-dual-root";

        // Exhaust lineage unexpected-continue via unit-A, leave unit-B free.
        promote_unexpected(
            &store,
            parent_id,
            child_id,
            "dual-a1",
            lineage,
            Some("unit-A"),
            2,
            Some(lineage),
        )
        .await
        .unwrap();
        settle_completed(&store, "dual-a1").await;
        promote_unexpected(
            &store,
            parent_id,
            child_id,
            "dual-a2",
            lineage,
            Some("unit-A"),
            3,
            Some("dual-a1"),
        )
        .await
        .unwrap();
        settle_completed(&store, "dual-a2").await;

        let (uc, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc, 2);
        let (wub, _) = work_unit_counts(&db, parent_id, "unit-B").await;
        assert_eq!(wub, 0, "unit-B starts free");

        // Lineage at limit, work-unit free → stricter wins; no partial charge.
        // Use generation 4 so (child, generation) unique is free after a1/a2.
        let err = promote_unexpected(
            &store,
            parent_id,
            child_id,
            "dual-b1",
            lineage,
            Some("unit-B"),
            4,
            Some("dual-a2"),
        )
        .await
        .expect_err("lineage limit must refuse even if work-unit free");
        assert!(err.is_budget_exhausted(), "got {err:?}");

        let (uc_after, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc_after, 2);
        let (wub_after, _) = work_unit_counts(&db, parent_id, "unit-B").await;
        assert_eq!(
            wub_after, 0,
            "work-unit must not be partially charged when lineage fails"
        );
    }

    #[tokio::test]
    async fn reserving_insert_does_not_charge_counters() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "budget-pre-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-pre-root";

        store
            .insert_reserving(sample_insert_with(
                "pre-uc",
                parent_id,
                child_id,
                2,
                Some(lineage),
                AdmissionClass::UnexpectedContinue,
                lineage,
                Some("unit-pre"),
            ))
            .await
            .expect("insert reserving");

        let (uc, rc) = lineage_counts(&db, lineage).await;
        assert_eq!((uc, rc), (0, 0), "pre-running must not charge");
        let (wuc, wrc) = work_unit_counts(&db, parent_id, "unit-pre").await;
        assert_eq!((wuc, wrc), (0, 0));

        let run = store.load_by_task_id("pre-uc").await.unwrap().unwrap();
        assert_eq!(run.run_status, DelegationRunStatus::Reserving);
        assert!(run.reached_running_at.is_none());
    }

    #[tokio::test]
    async fn post_running_cancel_fail_do_not_refund_charged_counter() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "budget-nr-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-nr-root";

        promote_unexpected(
            &store,
            parent_id,
            child_id,
            "nr-1",
            lineage,
            Some("unit-nr"),
            2,
            Some(lineage),
        )
        .await
        .unwrap();
        let (uc, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc, 1);

        store
            .settle_terminal(
                "nr-1",
                TerminalTaskWrite::canceled("canceled", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();
        let (uc_after_cancel, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc_after_cancel, 1, "cancel must not refund");

        promote_unexpected(
            &store,
            parent_id,
            child_id,
            "nr-2",
            lineage,
            Some("unit-nr"),
            3,
            Some("nr-1"),
        )
        .await
        .unwrap();
        let (uc2, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc2, 2);

        store
            .settle_terminal(
                "nr-2",
                TerminalTaskWrite::failed(
                    "host_restarted",
                    Utc::now(),
                    ConversationStatus::Cancelled,
                ),
            )
            .await
            .unwrap();
        let (uc_after_fail, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc_after_fail, 2, "fail/restart must not refund");
    }

    /// REQUIRED disk-WAL + dual one-connection pools: a shared
    /// `sqlite::memory:` pool serializes writers, so concurrent promote cannot
    /// prove budget contention. Two independent WAL pools race the last slot.
    #[tokio::test]
    async fn concurrent_promote_races_one_budget_winner() {
        use std::time::Duration;

        use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};
        use tokio::sync::Barrier;

        use crate::db::test_helpers::fresh_disk_db;

        let dir = tempfile::tempdir().expect("tempdir");
        // Migrate + seed once; reopen as two independent one-connection pools
        // on the same WAL file so promote transactions can truly contend.
        let migrate = Arc::new(fresh_disk_db(dir.path()).await);
        let (parent_id, child_seed) =
            seed_parent_child(&migrate, "budget-race-001-4111-8111-111111111111").await;
        let store_seed = RunStore::new(migrate.clone());
        let lineage = "lineage-race-root";
        let unit = Some("unit-race");

        // Spend 1 of 2 slots so only one concurrent promote can win the last slot.
        promote_unexpected(
            &store_seed,
            parent_id,
            child_seed,
            "race-seed",
            lineage,
            unit,
            2,
            Some(lineage),
        )
        .await
        .unwrap();
        settle_completed(&store_seed, "race-seed").await;

        // Two children share lineage/work-unit budget (one non-terminal per child).
        let folder = seed_folder(&migrate, "/tmp/codeg-run-store-race").await;
        let child_a = conversation_service::create_with_delegation(
            &migrate.conn,
            folder,
            AgentType::Codex,
            Some("child-race-a".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-race-a".into(),
                delegation_call_id: "race-a".into(),
            }),
        )
        .await
        .expect("child_a");
        let child_b = conversation_service::create_with_delegation(
            &migrate.conn,
            folder,
            AgentType::Codex,
            Some("child-race-b".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-race-b".into(),
                delegation_call_id: "race-b".into(),
            }),
        )
        .await
        .expect("child_b");

        store_seed
            .insert_reserving(sample_insert_with(
                "race-a",
                parent_id,
                child_a.id,
                2,
                Some("race-seed"),
                AdmissionClass::UnexpectedContinue,
                lineage,
                unit,
            ))
            .await
            .unwrap();
        store_seed
            .insert_reserving(sample_insert_with(
                "race-b",
                parent_id,
                child_b.id,
                2,
                Some("race-seed"),
                AdmissionClass::UnexpectedContinue,
                lineage,
                unit,
            ))
            .await
            .unwrap();

        // Release the migrator pool so WAL writers are just the two racers.
        drop(store_seed);
        let migrate = Arc::try_unwrap(migrate).unwrap_or_else(|_| {
            panic!("migrator Arc unique after seed");
        });
        migrate.conn.close().await.expect("close migrator pool");

        let path = dir.path().join("source.db");
        async fn open_wal_pool(path: &std::path::Path) -> AppDatabase {
            let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
            let mut opts = ConnectOptions::new(url);
            opts.max_connections(1)
                .min_connections(1)
                .connect_timeout(Duration::from_secs(10))
                .sqlx_logging(false);
            let conn = Database::connect(opts).await.expect("open wal pool");
            for pragma in [
                "PRAGMA journal_mode=WAL;",
                "PRAGMA busy_timeout=5000;",
                "PRAGMA foreign_keys=ON;",
            ] {
                conn.execute(Statement::from_string(DbBackend::Sqlite, pragma.to_owned()))
                    .await
                    .expect("pragma");
            }
            AppDatabase { conn }
        }

        let pool_a = Arc::new(open_wal_pool(&path).await);
        let pool_b = Arc::new(open_wal_pool(&path).await);
        let store_a = Arc::new(RunStore::new(pool_a.clone()));
        let store_b = Arc::new(RunStore::new(pool_b.clone()));
        let barrier = Arc::new(Barrier::new(2));

        /// Retry only SQLite busy/locked under dual-pool contention until a
        /// durable outcome (`Ok` or `BudgetExhausted`) is observed — mirrors
        /// production persistence retry around promote/charge.
        async fn promote_with_busy_retry(
            store: &RunStore,
            task_id: &str,
            conn_id: &str,
        ) -> Result<(), TaskStoreError> {
            use crate::acp::delegation::store::PersistenceRetryPolicy;
            let policy = PersistenceRetryPolicy::production();
            let mut attempt = 0u32;
            loop {
                match store.promote_running(task_id, conn_id, Utc::now()).await {
                    Ok(_) => return Ok(()),
                    Err(e) if e.is_budget_exhausted() => return Err(e),
                    Err(e) if e.is_transient() && attempt + 1 < policy.max_attempts => {
                        let delay = policy.delay_for_attempt(attempt);
                        attempt += 1;
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        let barrier_a = barrier.clone();
        let barrier_b = barrier.clone();
        let sa = store_a.clone();
        let sb = store_b.clone();
        let (res_a, res_b) = tokio::join!(
            async move {
                barrier_a.wait().await;
                promote_with_busy_retry(&sa, "race-a", "conn-a").await
            },
            async move {
                barrier_b.wait().await;
                promote_with_busy_retry(&sb, "race-b", "conn-b").await
            },
        );

        let wins = [res_a.is_ok(), res_b.is_ok()]
            .into_iter()
            .filter(|w| *w)
            .count();
        let losses = [res_a.as_ref().err(), res_b.as_ref().err()]
            .into_iter()
            .filter(|e| e.map(|err| err.is_budget_exhausted()).unwrap_or(false))
            .count();
        assert_eq!(
            wins, 1,
            "exactly one promote must win the last slot under WAL dual-pool contention: {res_a:?} {res_b:?}"
        );
        assert_eq!(
            losses, 1,
            "loser must be BudgetExhausted under WAL dual-pool contention: {res_a:?} {res_b:?}"
        );

        let (uc, _) = lineage_counts(&pool_a, lineage).await;
        assert_eq!(uc, 2);
        let (wuc, _) = work_unit_counts(&pool_a, parent_id, "unit-race").await;
        assert_eq!(wuc, 2);

        let a = store_a.load_by_task_id("race-a").await.unwrap().unwrap();
        let b = store_a.load_by_task_id("race-b").await.unwrap().unwrap();
        let running = [&a, &b]
            .into_iter()
            .filter(|r| r.run_status == DelegationRunStatus::Running)
            .count();
        assert_eq!(running, 1);
        let still_reserving = [&a, &b]
            .into_iter()
            .filter(|r| r.run_status == DelegationRunStatus::Reserving)
            .count();
        assert_eq!(still_reserving, 1);
    }

    #[tokio::test]
    async fn generation_over_100_is_budget_exhausted() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "budget-gen-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());

        // Generation 100 is the hard ceiling — allowed.
        store
            .insert_reserving(sample_insert_with(
                "gen-100",
                parent_id,
                child_id,
                MAX_GENERATION,
                Some("root"),
                AdmissionClass::NormalRevision,
                "root",
                None,
            ))
            .await
            .expect("generation 100 allowed");

        let err = store
            .insert_reserving(sample_insert_with(
                "gen-101",
                parent_id,
                child_id,
                MAX_GENERATION + 1,
                Some("root"),
                AdmissionClass::NormalRevision,
                "root",
                None,
            ))
            .await
            .expect_err("generation 101 must exhaust");
        assert!(err.is_budget_exhausted(), "got {err:?}");
        assert!(store.load_by_task_id("gen-101").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn normal_revision_promote_does_not_charge() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "budget-nrv-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-nrv-root";

        store
            .insert_reserving(sample_insert_with(
                "nrv-1",
                parent_id,
                child_id,
                2,
                Some(lineage),
                AdmissionClass::NormalRevision,
                lineage,
                Some("unit-nrv"),
            ))
            .await
            .unwrap();
        store
            .promote_running("nrv-1", "conn-nrv", Utc::now())
            .await
            .unwrap();

        let (uc, rc) = lineage_counts(&db, lineage).await;
        assert_eq!((uc, rc), (0, 0));
        // Lazy budget rows are only required for charging classes; normal
        // revision may leave rows absent (counts helper returns zeros).
        let (wuc, wrc) = work_unit_counts(&db, parent_id, "unit-nrv").await;
        assert_eq!((wuc, wrc), (0, 0));
    }

    // ---- Task 4: gen-1 admit + snapshot + concurrent fence -------------------

    fn gen1_insert(
        task_id: &str,
        parent_id: i32,
        child_id: i32,
        tool_use: &str,
        task_text: &str,
        work_unit_key: Option<&str>,
        route_fp: &str,
    ) -> ReservingRunInsert {
        use crate::acp::delegation::launch_snapshot::{
            build_live_launch_config, LAUNCH_SNAPSHOT_VERSION,
        };
        use crate::acp::delegation::types::DELEGATE_TO_AGENT_TOOL;
        use std::collections::BTreeMap;

        let mut live = BTreeMap::new();
        live.insert("model".into(), "gpt-test".into());
        live.insert("api_key".into(), "sk-should-not-persist".into());
        let launch = build_live_launch_config(
            AgentType::Codex,
            Some("profile-1"),
            "/tmp/ws-gen1",
            Some("default".into()),
            live,
        );
        ReservingRunInsert {
            task_id: task_id.into(),
            root_task_id: task_id.into(),
            previous_task_id: None,
            generation: 1,
            parent_conversation_id: parent_id,
            parent_tool_use_id: Some(tool_use.into()),
            child_conversation_id: child_id,
            agent_type: "codex".into(),
            profile_id: launch.snapshot.profile_id.clone(),
            workspace_path: Some(launch.snapshot.workspace_path.clone()),
            route_fingerprint: Some(if route_fp.is_empty() {
                launch.snapshot.route_fingerprint.clone()
            } else {
                route_fp.into()
            }),
            launch_snapshot_version: Some(LAUNCH_SNAPSHOT_VERSION.into()),
            mode_id: launch.snapshot.mode_id.clone(),
            config_values_json: Some(launch.snapshot.config_values_json.clone()),
            task_preview: Some(derive_task_preview(task_text)),
            request_fingerprint: Some(request_fingerprint(
                DELEGATE_TO_AGENT_TOOL,
                task_text,
                work_unit_key,
                None,
                None,
                None,
                if route_fp.is_empty() {
                    &launch.snapshot.route_fingerprint
                } else {
                    route_fp
                },
            )),
            admission_class: AdmissionClass::NormalRevision,
            lineage_root_task_id: task_id.into(),
            work_unit_key: work_unit_key.map(|s| s.into()),
            history_only: false,
            replaced_task_id: None,
            replacement_reason: None,
            started_at: Some(Utc::now()),
        }
    }

    #[tokio::test]
    async fn gen1_admit_creates_full_launch_snapshot() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "gen1-snap-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db);
        let task_id = "gen1-snap-0001-4111-8111-111111111111";
        let outcome = store
            .admit_gen1_reserving(gen1_insert(
                task_id,
                parent_id,
                child_id,
                "tu-gen1-snap",
                "implement feature X",
                Some("unit-snap"),
                "",
            ))
            .await
            .expect("admit");
        let Gen1AdmitOutcome::Created(run) = outcome else {
            panic!("expected Created, got {outcome:?}");
        };
        assert_eq!(run.generation, 1);
        assert_eq!(run.task_id, task_id);
        assert_eq!(run.root_task_id, task_id);
        assert_eq!(run.lineage_root_task_id, task_id);
        assert_eq!(run.admission_class, AdmissionClass::NormalRevision);
        assert_eq!(run.workspace_path.as_deref(), Some("/tmp/ws-gen1"));
        assert_eq!(
            run.launch_snapshot_version.as_deref(),
            Some(crate::acp::delegation::launch_snapshot::LAUNCH_SNAPSHOT_VERSION)
        );
        assert_eq!(run.mode_id.as_deref(), Some("default"));
        assert_eq!(run.profile_id.as_deref(), Some("profile-1"));
        assert_eq!(run.work_unit_key.as_deref(), Some("unit-snap"));
        let cfg = run.config_values_json.as_deref().unwrap_or("");
        assert!(cfg.contains("gpt-test"), "allowlisted model present");
        assert!(
            !cfg.contains("sk-should-not-persist"),
            "secrets must not land in config_values_json"
        );
        assert!(run.request_fingerprint.is_some());
        assert!(run.route_fingerprint.is_some());
        assert_eq!(
            run.task_preview.as_deref(),
            Some(derive_task_preview("implement feature X").as_str())
        );
        assert!(!run.history_only);
        assert_eq!(run.run_status, DelegationRunStatus::Reserving);
    }

    #[tokio::test]
    async fn gen1_fingerprint_match_returns_same_run() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "gen1-idem-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db);
        let task_id = "gen1-idem-0001-4111-8111-111111111111";
        let insert = gen1_insert(
            task_id,
            parent_id,
            child_id,
            "tu-idem",
            "same task body",
            None,
            "routehex01",
        );
        let first = store.admit_gen1_reserving(insert.clone()).await.unwrap();
        assert!(matches!(first, Gen1AdmitOutcome::Created(_)));

        // Second admit with same parent tool + fingerprint must be idempotent
        // even if a new task_id/child would otherwise be used.
        let mut second_insert = insert.clone();
        second_insert.task_id = "gen1-idem-0002-4111-8111-111111111112".into();
        second_insert.root_task_id = second_insert.task_id.clone();
        second_insert.lineage_root_task_id = second_insert.task_id.clone();
        let second = store.admit_gen1_reserving(second_insert).await.unwrap();
        match second {
            Gen1AdmitOutcome::Idempotent(run) => {
                assert_eq!(run.task_id, task_id);
            }
            other => panic!("expected Idempotent, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn gen1_fingerprint_mismatch_rejects_duplicate_parent_tool() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "gen1-dup-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db);
        let first = gen1_insert(
            "gen1-dup-0001-4111-8111-111111111111",
            parent_id,
            child_id,
            "tu-dup",
            "task A",
            None,
            "routehex01",
        );
        store.admit_gen1_reserving(first).await.unwrap();

        let mismatch = gen1_insert(
            "gen1-dup-0002-4111-8111-111111111112",
            parent_id,
            child_id,
            "tu-dup",
            "task B different body",
            None,
            "routehex01",
        );
        let err = store.admit_gen1_reserving(mismatch).await.unwrap_err();
        assert!(
            err.is_duplicate_parent_tool(),
            "mismatch must be duplicate_parent_tool, got {err:?}"
        );
        assert_eq!(err.wire_code(), Some("duplicate_parent_tool"));
    }

    #[tokio::test]
    async fn gen1_legacy_missing_fingerprint_rejects() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "gen1-leg-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let mut first = gen1_insert(
            "gen1-leg-0001-4111-8111-111111111111",
            parent_id,
            child_id,
            "tu-leg",
            "legacy row",
            None,
            "routehex01",
        );
        first.request_fingerprint = None; // simulate backfill/history row
        store.insert_reserving(first).await.unwrap();

        let retry = gen1_insert(
            "gen1-leg-0002-4111-8111-111111111112",
            parent_id,
            child_id,
            "tu-leg",
            "legacy row",
            None,
            "routehex01",
        );
        let err = store.admit_gen1_reserving(retry).await.unwrap_err();
        assert!(err.is_duplicate_parent_tool());
    }

    #[tokio::test]
    async fn concurrent_gen1_same_work_unit_one_winner_busy_thread_loser() {
        use tokio::sync::Barrier;

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_a) =
            seed_parent_child(&db, "gen1-fence-a-4111-8111-111111111111").await;
        // Second child for the concurrent gen-1 attempt (one non-terminal per child).
        let folder = seed_folder(&db, "/tmp/codeg-gen1-fence").await;
        let child_b = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child-b".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-fence-b".into(),
                delegation_call_id: "gen1-fence-b-4111-8111-111111111112".into(),
            }),
        )
        .await
        .expect("child_b");

        let store = Arc::new(RunStore::new(db));
        let barrier = Arc::new(Barrier::new(2));
        let insert_a = gen1_insert(
            "gen1-fence-a-4111-8111-111111111111",
            parent_id,
            child_a,
            "tu-fence-a",
            "work unit task",
            Some("shared-unit"),
            "routehex01",
        );
        let insert_b = gen1_insert(
            "gen1-fence-b-4111-8111-111111111112",
            parent_id,
            child_b.id,
            "tu-fence-b",
            "work unit task other",
            Some("shared-unit"),
            "routehex02",
        );

        let s1 = store.clone();
        let b1 = barrier.clone();
        let t1 = tokio::spawn(async move {
            b1.wait().await;
            s1.admit_gen1_reserving(insert_a).await
        });
        let s2 = store.clone();
        let b2 = barrier.clone();
        let t2 = tokio::spawn(async move {
            b2.wait().await;
            s2.admit_gen1_reserving(insert_b).await
        });

        let (r1, r2) = tokio::join!(t1, t2);
        let r1 = r1.expect("join a");
        let r2 = r2.expect("join b");
        let created = matches!(r1, Ok(Gen1AdmitOutcome::Created(_))) as u8
            + matches!(r2, Ok(Gen1AdmitOutcome::Created(_))) as u8;
        let busy = matches!(r1, Err(TaskStoreError::BusyThread(_))) as u8
            + matches!(r2, Err(TaskStoreError::BusyThread(_))) as u8;
        assert_eq!(created, 1, "exactly one gen-1 winner: {r1:?} {r2:?}");
        assert_eq!(busy, 1, "loser must be busy_thread: {r1:?} {r2:?}");
    }

    /// After the winner has promoted to running (still non-terminal), a late
    /// gen-1 claim on the same work unit must be `busy_thread`, not
    /// `invalid_replacement` (reserved for terminal established lineage).
    #[tokio::test]
    async fn work_unit_with_running_claim_rejects_new_gen1_as_busy_thread() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "gen1-run-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let first = gen1_insert(
            "gen1-run-0001-4111-8111-111111111111",
            parent_id,
            child_id,
            "tu-run-1",
            "first admission",
            Some("unit-running"),
            "routehex01",
        );
        store.admit_gen1_reserving(first).await.unwrap();
        store
            .promote_running(
                "gen1-run-0001-4111-8111-111111111111",
                "conn-run",
                Utc::now(),
            )
            .await
            .unwrap();

        let folder = seed_folder(&db, "/tmp/codeg-gen1-running-busy").await;
        let child2 = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child2".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-run-2".into(),
                delegation_call_id: "gen1-run-0002-4111-8111-111111111112".into(),
            }),
        )
        .await
        .unwrap();
        let late = gen1_insert(
            "gen1-run-0002-4111-8111-111111111112",
            parent_id,
            child2.id,
            "tu-run-2",
            "late gen1 while first still running",
            Some("unit-running"),
            "routehex02",
        );
        let err = store.admit_gen1_reserving(late).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::BusyThread(_)),
            "running non-terminal claim → busy_thread, got {err:?}"
        );
        assert_eq!(err.wire_code(), Some("busy_thread"));
    }

    #[tokio::test]
    async fn work_unit_with_established_lineage_rejects_new_gen1_as_invalid_replacement() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "gen1-est-0001-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let first = gen1_insert(
            "gen1-est-0001-4111-8111-111111111111",
            parent_id,
            child_id,
            "tu-est-1",
            "first admission",
            Some("unit-est"),
            "routehex01",
        );
        store.admit_gen1_reserving(first).await.unwrap();
        store
            .promote_running(
                "gen1-est-0001-4111-8111-111111111111",
                "conn-est",
                Utc::now(),
            )
            .await
            .unwrap();
        store
            .settle_terminal(
                "gen1-est-0001-4111-8111-111111111111",
                TerminalTaskWrite::failed("taskfail", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        let folder = seed_folder(&db, "/tmp/codeg-gen1-est").await;
        let child2 = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child2".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-est-2".into(),
                delegation_call_id: "gen1-est-0002-4111-8111-111111111112".into(),
            }),
        )
        .await
        .unwrap();
        let bypass = gen1_insert(
            "gen1-est-0002-4111-8111-111111111112",
            parent_id,
            child2.id,
            "tu-est-2",
            "bypass without replaces",
            Some("unit-est"),
            "routehex02",
        );
        let err = store.admit_gen1_reserving(bypass).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::InvalidReplacement(_)),
            "established lineage gen-1 re-dispatch without replaces → invalid_replacement, got {err:?}"
        );
        assert_eq!(err.wire_code(), Some("invalid_replacement"));
    }

    #[tokio::test]
    async fn reconcile_status_and_audit_split_reserving_vs_running() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_a, child_a) = seed_parent_child(&db, "recon-root-4111-8111-111111111111").await;
        let (parent_b, child_b) = seed_parent_child(&db, "recon-root-4111-8111-111111111112").await;
        let store = RunStore::new(db.clone());

        // reserving run (never promoted)
        let mut resv = sample_insert("recon-resv", parent_a, child_a, 1, None);
        resv.work_unit_key = None;
        store.insert_reserving(resv).await.unwrap();

        // running run on a different parent/child so work-unit fence is irrelevant
        let mut run_ins = sample_insert("recon-run", parent_b, child_b, 1, None);
        run_ins.work_unit_key = None;
        store.insert_reserving(run_ins).await.unwrap();
        store
            .promote_running("recon-run", "conn-run", Utc::now())
            .await
            .unwrap();

        let at = Utc::now();
        let n = store.reconcile_non_terminal(at).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.count_non_terminal().await.unwrap(), 0);

        let resv = store.load_by_task_id("recon-resv").await.unwrap().unwrap();
        assert_eq!(resv.run_status, DelegationRunStatus::Failed);
        assert_eq!(resv.error_code.as_deref(), Some("host_restarted"));
        // audit preserved on row
        let resv_row = DelegationTaskRun::find_by_id("recon-resv")
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let audit = resv_row.termination_audit_json.expect("audit");
        assert!(audit.contains("reserving"));
        assert!(audit.contains("host_restart"));
        // reserving inherits admission_class eligibility (normal_revision here)
        assert_eq!(resv.admission_class, AdmissionClass::NormalRevision);

        let run = store.load_by_task_id("recon-run").await.unwrap().unwrap();
        assert_eq!(run.run_status, DelegationRunStatus::Canceled);
        assert_eq!(run.error_code.as_deref(), Some("host_restarted"));
        let run_row = DelegationTaskRun::find_by_id("recon-run")
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let audit2 = run_row.termination_audit_json.expect("audit");
        assert!(audit2.contains("running"));
        // running was promoted → eligible for unexpected_continue recovery path
        assert!(run.reached_running_at.is_some());

        // Restart backstop: child conversation projections leave no running
        // orphan after successful reconcile.
        let reserving_child = conversation::Entity::find_by_id(child_a)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            reserving_child.delegation_task_status,
            Some(DelegationTaskStatus::Failed)
        );
        assert_eq!(reserving_child.status, ConversationStatus::Cancelled);

        let running_child = conversation::Entity::find_by_id(child_b)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            running_child.delegation_task_status,
            Some(DelegationTaskStatus::Canceled)
        );
        assert_eq!(running_child.status, ConversationStatus::Cancelled);
    }

    #[tokio::test]
    async fn cold_resolve_by_child_connection_match_and_noop() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "cold-root-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        store
            .insert_reserving(sample_insert("cold-1", parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running("cold-1", "conn-cold", Utc::now())
            .await
            .unwrap();

        let hit = store
            .load_non_terminal_by_child_connection("conn-cold")
            .await
            .unwrap()
            .expect("match");
        assert_eq!(hit.task_id, "cold-1");

        assert!(store
            .load_non_terminal_by_child_connection("conn-other")
            .await
            .unwrap()
            .is_none());

        // After settle, no non-terminal match.
        store
            .settle_terminal(
                "cold-1",
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .unwrap();
        assert!(store
            .load_non_terminal_by_child_connection("conn-cold")
            .await
            .unwrap()
            .is_none());
    }

    /// Task 3: different already-bound connection is a typed ownership
    /// conflict (fail-closed), not silent Ok / first-bind-wins.
    #[tokio::test]
    async fn bind_different_connection_is_permanent_conflict() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bind-root-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let task_id = "bind-task-1";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert reserving");

        store
            .bind_child_connection_while_reserving(task_id, "conn-owner")
            .await
            .expect("first bind");
        // Same connection is idempotent while still reserving.
        store
            .bind_child_connection_while_reserving(task_id, "conn-owner")
            .await
            .expect("same-connection rebind");

        let err = store
            .bind_child_connection_while_reserving(task_id, "conn-other")
            .await
            .expect_err("different connection must fail closed");
        match err {
            TaskStoreError::BindOwnershipConflict(msg) => {
                assert!(
                    msg.contains("different connection") || msg.contains("already bound"),
                    "expected ownership conflict wording, got {msg}"
                );
            }
            other => panic!("expected BindOwnershipConflict, got {other:?}"),
        }

        let run = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(
            run.child_connection_id.as_deref(),
            Some("conn-owner"),
            "owner must not be overwritten"
        );
        assert_eq!(run.run_status, DelegationRunStatus::Reserving);
    }

    /// Different-connection on an already-Running owner is ownership conflict
    /// (not generic Permanent), so broker BindFailed cannot settle the owner.
    #[tokio::test]
    async fn bind_different_connection_running_is_ownership_conflict() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bind-ownrun-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let task_id = "bind-running-owner";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");
        store
            .bind_child_connection_while_reserving(task_id, "conn-owner")
            .await
            .expect("owner bind");
        store
            .promote_running(task_id, "conn-owner", Utc::now())
            .await
            .expect("promote");

        let err = store
            .bind_child_connection_while_reserving(task_id, "conn-challenger")
            .await
            .expect_err("different connection on running owner");
        match err {
            TaskStoreError::BindOwnershipConflict(msg) => {
                assert!(
                    msg.contains("different connection"),
                    "expected ownership wording, got {msg}"
                );
            }
            other => panic!("expected BindOwnershipConflict, got {other:?}"),
        }
        let run = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(run.run_status, DelegationRunStatus::Running);
        assert_eq!(run.child_connection_id.as_deref(), Some("conn-owner"));
    }

    /// Ownership-filtered pre-admission settle refuses Running winners.
    #[tokio::test]
    async fn settle_pre_admission_if_owned_skips_running_winner() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bind-settle-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let task_id = "bind-settle-run";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");
        store
            .bind_child_connection_while_reserving(task_id, "conn-owner")
            .await
            .expect("bind");
        store
            .promote_running(task_id, "conn-owner", Utc::now())
            .await
            .expect("promote");

        let outcome = store
            .settle_pre_admission_failure_if_owned(
                task_id,
                "conn-challenger",
                TerminalTaskWrite::failed(
                    "spawn_failed",
                    Utc::now(),
                    ConversationStatus::Cancelled,
                ),
            )
            .await
            .expect("settle helper");
        assert!(outcome.is_none(), "must not settle running winner");
        let run = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(run.run_status, DelegationRunStatus::Running);
        assert!(run.error_code.is_none());
    }

    /// Ownership predicate necessity (mid-path race): gate fires **after** the
    /// helper reads a still-`Reserving` unbound row and **before** the
    /// ownership-fenced terminal UPDATE. Concurrent foreign bind commits in
    /// that window and **remains Reserving** (no promote). CAS must miss
    /// (zero-row) and leave the foreign claim Reserving / unbound-from-
    /// challenger.
    ///
    /// Predicate necessity: if the UPDATE dropped only the
    /// `child_connection_id` ownership filter and kept `status = Reserving`,
    /// the write would terminalize the foreign reserving claim as
    /// `spawn_failed` and this test would fail. Promote-to-Running is a
    /// separate status-fence scenario (see concurrent_promote test).
    #[tokio::test]
    async fn settle_pre_admission_ownership_cas_survives_concurrent_foreign_bind() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bind-cas-own-4111-8111-111111111111").await;
        let store = Arc::new(RunStore::new(db.clone()));
        let task_id = "bind-cas-own-race";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert unbound reserving");

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        store
            .install_settle_gate(entered_tx, release_rx)
            .await;

        let store_settle = store.clone();
        let settle_handle = tokio::spawn(async move {
            store_settle
                .settle_pre_admission_failure_if_owned(
                    task_id,
                    "conn-challenger",
                    TerminalTaskWrite::failed(
                        "spawn_failed",
                        Utc::now(),
                        ConversationStatus::Cancelled,
                    ),
                )
                .await
        });

        // Gate is mid-path: helper already saw unbound Reserving (would settle)
        // and has not yet issued the ownership-CAS write.
        entered_rx
            .await
            .expect("mid-path settle gate after Reserving read");
        let still_before_cas = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(
            still_before_cas.run_status,
            DelegationRunStatus::Reserving,
            "row must still be reserving when mid-path gate fires"
        );
        assert!(
            still_before_cas.child_connection_id.is_none(),
            "row must still be unbound at mid-path gate (stale settle view)"
        );

        // Concurrent foreign bind only — stay Reserving so status-fence alone
        // cannot zero-row the CAS; ownership predicate is the sole miss.
        store
            .bind_child_connection_while_reserving(task_id, "conn-owner")
            .await
            .expect("foreign bind under mid-path gate");
        let mid = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(
            mid.run_status,
            DelegationRunStatus::Reserving,
            "foreign claim must remain Reserving (do not promote before CAS)"
        );
        assert_eq!(mid.child_connection_id.as_deref(), Some("conn-owner"));
        let _ = release_tx.send(());

        let outcome = settle_handle
            .await
            .expect("join")
            .expect("settle helper ok");
        assert!(
            outcome.is_none(),
            "ownership CAS must miss after concurrent foreign bind: {outcome:?}"
        );
        let owner = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(
            owner.run_status,
            DelegationRunStatus::Reserving,
            "CAS miss must leave concurrent foreign claim Reserving"
        );
        assert_eq!(owner.child_connection_id.as_deref(), Some("conn-owner"));
        assert_ne!(
            owner.error_code.as_deref(),
            Some("spawn_failed"),
            "must not terminalize concurrent foreign reserving claim"
        );
        assert!(owner.error_code.is_none());
    }

    /// Status-fence mid-path race: concurrent bind+promote to Running before
    /// CAS. Complements ownership-predicate coverage (foreign still-Reserving);
    /// here `status = Reserving` alone zeros the write even without ownership.
    #[tokio::test]
    async fn settle_pre_admission_status_cas_survives_concurrent_promote() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bind-cas-st-4111-8111-111111111111").await;
        let store = Arc::new(RunStore::new(db.clone()));
        let task_id = "bind-cas-status-race";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert unbound reserving");

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        store
            .install_settle_gate(entered_tx, release_rx)
            .await;

        let store_settle = store.clone();
        let settle_handle = tokio::spawn(async move {
            store_settle
                .settle_pre_admission_failure_if_owned(
                    task_id,
                    "conn-challenger",
                    TerminalTaskWrite::failed(
                        "spawn_failed",
                        Utc::now(),
                        ConversationStatus::Cancelled,
                    ),
                )
                .await
        });

        entered_rx
            .await
            .expect("mid-path settle gate after Reserving read");
        store
            .bind_child_connection_while_reserving(task_id, "conn-owner")
            .await
            .expect("owner bind under mid-path gate");
        store
            .promote_running(task_id, "conn-owner", Utc::now())
            .await
            .expect("owner promote under mid-path gate");
        let mid = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(mid.run_status, DelegationRunStatus::Running);
        assert_eq!(mid.child_connection_id.as_deref(), Some("conn-owner"));
        let _ = release_tx.send(());

        let outcome = settle_handle
            .await
            .expect("join")
            .expect("settle helper ok");
        assert!(
            outcome.is_none(),
            "status CAS must miss after concurrent promote: {outcome:?}"
        );
        let owner = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(
            owner.run_status,
            DelegationRunStatus::Running,
            "CAS miss must leave concurrent owner Running"
        );
        assert_eq!(owner.child_connection_id.as_deref(), Some("conn-owner"));
        assert!(owner.error_code.is_none());
    }

    /// Different-owner terminal row: bind ownership conflict + terminal winner
    /// report available for challenger replay.
    #[tokio::test]
    async fn bind_different_owner_terminal_exposes_winner_for_replay() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bind-term-own-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let task_id = "bind-term-owner";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");
        store
            .bind_child_connection_while_reserving(task_id, "conn-owner")
            .await
            .expect("bind");
        store
            .promote_running(task_id, "conn-owner", Utc::now())
            .await
            .expect("promote");
        store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .expect("owner terminal");

        let err = store
            .bind_child_connection_while_reserving(task_id, "conn-challenger")
            .await
            .expect_err("ownership conflict");
        assert!(
            matches!(err, TaskStoreError::BindOwnershipConflict(_)),
            "{err:?}"
        );
        let winner = store
            .load_terminal_winner_report(task_id)
            .await
            .expect("load winner")
            .expect("terminal winner");
        assert_eq!(winner.status, TaskStatus::Completed);
        assert_ne!(winner.error_code.as_deref(), Some("spawn_failed"));
        let run = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(run.run_status, DelegationRunStatus::Completed);
        assert_eq!(run.child_connection_id.as_deref(), Some("conn-owner"));
    }

    /// Same-connection rebind is only idempotent while status is reserving.
    #[tokio::test]
    async fn bind_same_connection_running_is_rejected() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bind-run-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let task_id = "bind-running-1";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");
        store
            .bind_child_connection_while_reserving(task_id, "conn-same")
            .await
            .expect("bind");
        store
            .promote_running(task_id, "conn-same", Utc::now())
            .await
            .expect("promote");

        let err = store
            .bind_child_connection_while_reserving(task_id, "conn-same")
            .await
            .expect_err("running same-connection must not pass bind fence");
        match err {
            TaskStoreError::Permanent(msg) => {
                assert!(
                    msg.contains("not reserving"),
                    "expected not-reserving wording, got {msg}"
                );
            }
            other => panic!("expected Permanent not-reserving, got {other:?}"),
        }
        let run = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(run.run_status, DelegationRunStatus::Running);
        assert_eq!(run.child_connection_id.as_deref(), Some("conn-same"));
    }

    /// Terminal same-connection rebind is rejected (not reserving).
    #[tokio::test]
    async fn bind_same_connection_terminal_is_rejected() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bind-term-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let task_id = "bind-terminal-1";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");
        store
            .bind_child_connection_while_reserving(task_id, "conn-term")
            .await
            .expect("bind");
        store
            .promote_running(task_id, "conn-term", Utc::now())
            .await
            .expect("promote");
        store
            .settle_terminal(
                task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .expect("settle");

        let err = store
            .bind_child_connection_while_reserving(task_id, "conn-term")
            .await
            .expect_err("terminal same-connection must not pass bind fence");
        match err {
            TaskStoreError::Permanent(msg) => {
                assert!(
                    msg.contains("not reserving"),
                    "expected not-reserving wording, got {msg}"
                );
            }
            other => panic!("expected Permanent not-reserving, got {other:?}"),
        }
        let run = store
            .load_by_task_id(task_id)
            .await
            .expect("load")
            .expect("run");
        assert_eq!(run.run_status, DelegationRunStatus::Completed);
        assert_eq!(run.child_connection_id.as_deref(), Some("conn-term"));
    }

    #[tokio::test]
    async fn settle_terminal_persists_card_summary_json() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) = seed_parent_child(&db, "sum-root-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        store
            .insert_reserving(sample_insert("sum-1", parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running("sum-1", "conn-sum", Utc::now())
            .await
            .unwrap();
        let summary = r#"{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"ok"}"#;
        store
            .settle_terminal(
                "sum-1",
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview)
                    .with_card_summary_json(summary),
            )
            .await
            .unwrap();
        let row = DelegationTaskRun::find_by_id("sum-1")
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.card_summary_json.as_deref(), Some(summary));
    }

    fn eligible_continue() -> ContinueEligibility {
        ContinueEligibility {
            history_only: false,
            is_latest: true,
            has_active_run: false,
            child_superseded: false,
            child_ownership_valid: true,
            agent_type_matches: true,
            snapshot_complete: true,
            external_id_present: true,
            run_status: DelegationRunStatus::Completed,
            error_code: None,
            admission_class: AdmissionClass::NormalRevision,
            reached_running: true,
            termination_audit_json: None,
        }
    }

    #[test]
    fn continue_eligibility_decision_table_obeys_precedence_and_recovery_rules() {
        let normal = eligible_continue();
        assert_eq!(
            decide_continue_eligibility(&normal),
            ContinueDecision::Admit(AdmissionClass::NormalRevision)
        );

        let mut failed = eligible_continue();
        failed.run_status = DelegationRunStatus::Failed;
        failed.error_code = Some("child_refusal".into());
        assert_eq!(
            decide_continue_eligibility(&failed),
            ContinueDecision::Admit(AdmissionClass::NormalRevision)
        );

        let mut restarted_reserving = failed.clone();
        restarted_reserving.error_code = Some("host_restarted".into());
        restarted_reserving.reached_running = false;
        restarted_reserving.admission_class = AdmissionClass::UnexpectedContinue;
        restarted_reserving.termination_audit_json = Some(
            r#"{"source":"host_restart","reason":"host_restarted","prior_status":"reserving"}"#
                .into(),
        );
        assert_eq!(
            decide_continue_eligibility(&restarted_reserving),
            ContinueDecision::Admit(AdmissionClass::UnexpectedContinue)
        );

        let mut unexpected_cancel = eligible_continue();
        unexpected_cancel.run_status = DelegationRunStatus::Canceled;
        unexpected_cancel.error_code = Some("host_restarted".into());
        unexpected_cancel.termination_audit_json = Some(
            r#"{"source":"host_restart","reason":"host_restarted","prior_status":"running"}"#
                .into(),
        );
        assert_eq!(
            decide_continue_eligibility(&unexpected_cancel),
            ContinueDecision::Admit(AdmissionClass::UnexpectedContinue)
        );

        let mut unknown_cancel = unexpected_cancel.clone();
        unknown_cancel.termination_audit_json = None;
        assert_eq!(
            decide_continue_eligibility(&unknown_cancel),
            ContinueDecision::NotContinuable
        );

        let mut unknown_origin_cancel = unexpected_cancel.clone();
        unknown_origin_cancel.termination_audit_json =
            Some(r#"{"reason":"host_restarted","prior_status":"running"}"#.into());
        assert_eq!(
            decide_continue_eligibility(&unknown_origin_cancel),
            ContinueDecision::NotContinuable,
            "a reason without an auditable interruption source must fail closed"
        );

        let mut policy_reject = failed.clone();
        policy_reject.error_code = Some("route_policy_rejected".into());
        assert_eq!(
            decide_continue_eligibility(&policy_reject),
            ContinueDecision::NotContinuable
        );

        let mut replacement_restart = restarted_reserving.clone();
        replacement_restart.admission_class = AdmissionClass::Replacement;
        assert_eq!(
            decide_continue_eligibility(&replacement_restart),
            ContinueDecision::NotContinuable
        );

        let mut superseded = normal.clone();
        superseded.child_superseded = true;
        assert_eq!(
            decide_continue_eligibility(&superseded),
            ContinueDecision::NotContinuable
        );

        let mut deleted_child = normal.clone();
        deleted_child.child_ownership_valid = false;
        assert_eq!(
            decide_continue_eligibility(&deleted_child),
            ContinueDecision::NotContinuable
        );

        let mut agent_mismatch = normal.clone();
        agent_mismatch.agent_type_matches = false;
        assert_eq!(
            decide_continue_eligibility(&agent_mismatch),
            ContinueDecision::NotContinuable
        );

        let mut busy_stale = normal.clone();
        busy_stale.has_active_run = true;
        busy_stale.is_latest = false;
        assert_eq!(
            decide_continue_eligibility(&busy_stale),
            ContinueDecision::BusyThread,
            "busy must win before stale_task_id"
        );
    }

    #[tokio::test]
    async fn continue_parent_tool_idempotency_precedes_busy_and_stale() {
        use crate::db::entities::conversation;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "continue-root-4111-8111-111111111111").await;
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut child = child.into_active_model();
        child.external_id = Set(Some("session-continue".into()));
        child.update(&db.conn).await.unwrap();

        let store = RunStore::new(db.clone());
        let root = "continue-root-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(root, "conn-root", Utc::now())
            .await
            .unwrap();
        settle_completed(&store, root).await;

        let fingerprint = request_fingerprint(
            "continue_delegation",
            "review the revision",
            Some("unit-a"),
            None,
            None,
            Some(root),
            "aabbccdd",
        );
        let admission = ContinueRunAdmission {
            task_id: "continue-next".into(),
            parent_conversation_id: parent_id,
            parent_tool_use_id: "tu-continue".into(),
            target_task_id: root.into(),
            task_preview: derive_task_preview("review the revision"),
            request_fingerprint: fingerprint.clone(),
            work_unit_key: Some("unit-a".into()),
        };
        let first = store
            .admit_continue_reserving(admission.clone())
            .await
            .expect("first continuation reserve");
        assert!(matches!(first, ContinueAdmitOutcome::Created(_)));

        let mut replay = admission.clone();
        replay.task_id = "continue-replay".into();
        let second = store
            .admit_continue_reserving(replay)
            .await
            .expect("same parent tool must return idempotently before busy");
        assert!(matches!(second, ContinueAdmitOutcome::Idempotent(_)));

        let mut mismatch = admission;
        mismatch.task_id = "continue-mismatch".into();
        mismatch.request_fingerprint = "different".into();
        let err = store.admit_continue_reserving(mismatch).await.unwrap_err();
        assert!(matches!(err, TaskStoreError::DuplicateParentTool(_)));

        // Make the first continuation the latest terminal run so this request
        // isolates the work-unit mismatch rather than correctly hitting busy.
        settle_completed(&store, "continue-next").await;
        let mismatched_work_unit = ContinueRunAdmission {
            task_id: "continue-wrong-unit".into(),
            parent_conversation_id: parent_id,
            parent_tool_use_id: "tu-wrong-unit".into(),
            target_task_id: "continue-next".into(),
            task_preview: derive_task_preview("review with wrong unit"),
            request_fingerprint: "wrong-unit-fingerprint".into(),
            work_unit_key: Some("unit-b".into()),
        };
        let err = store
            .admit_continue_reserving(mismatched_work_unit)
            .await
            .unwrap_err();
        assert!(matches!(err, TaskStoreError::NotContinuable(_)));
    }

    /// Overlap precedence: busy_thread / stale_task_id beat work_unit_key mismatch.
    #[tokio::test]
    async fn continue_error_precedence_busy_and_stale_before_work_unit_mismatch() {
        use crate::db::entities::conversation;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "prec-root-4111-8111-111111111111").await;
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut child = child.into_active_model();
        child.external_id = Set(Some("session-prec".into()));
        child.update(&db.conn).await.unwrap();

        let store = RunStore::new(db.clone());
        let root = "prec-root-4111-8111-111111111111";
        let mut root_insert = sample_insert(root, parent_id, child_id, 1, None);
        root_insert.work_unit_key = Some("unit-a".into());
        store.insert_reserving(root_insert).await.unwrap();
        store
            .promote_running(root, "conn-root", Utc::now())
            .await
            .unwrap();
        // Root still running → busy. Wrong work_unit_key must not preempt busy.
        let busy_wrong_key = ContinueRunAdmission {
            task_id: "cont-busy".into(),
            parent_conversation_id: parent_id,
            parent_tool_use_id: "tu-busy".into(),
            target_task_id: root.into(),
            task_preview: derive_task_preview("x"),
            request_fingerprint: "fp-busy".into(),
            work_unit_key: Some("unit-wrong".into()),
        };
        let err = store
            .admit_continue_reserving(busy_wrong_key)
            .await
            .unwrap_err();
        assert!(
            matches!(err, TaskStoreError::BusyThread(_)),
            "busy must win over work_unit mismatch: {err:?}"
        );

        settle_completed(&store, root).await;
        // Stale: older generation with a newer sibling on same child.
        store
            .insert_reserving(sample_insert(
                "prec-gen2-4111-8111-111111111111",
                parent_id,
                child_id,
                2,
                Some(root),
            ))
            .await
            .unwrap();
        store
            .promote_running("prec-gen2-4111-8111-111111111111", "conn-g2", Utc::now())
            .await
            .unwrap();
        settle_completed(&store, "prec-gen2-4111-8111-111111111111").await;

        let stale_wrong_key = ContinueRunAdmission {
            task_id: "cont-stale".into(),
            parent_conversation_id: parent_id,
            parent_tool_use_id: "tu-stale".into(),
            target_task_id: root.into(), // stale: not latest on child
            task_preview: derive_task_preview("x"),
            request_fingerprint: "fp-stale".into(),
            work_unit_key: Some("unit-wrong".into()),
        };
        let err = store
            .admit_continue_reserving(stale_wrong_key)
            .await
            .unwrap_err();
        assert!(
            matches!(err, TaskStoreError::StaleTaskId(_)),
            "stale must win over work_unit mismatch: {err:?}"
        );
    }

    #[tokio::test]
    async fn continue_rejects_soft_deleted_parent() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "deleted-parent-root-4111-8111-111111111111").await;
        conversation_service::update_external_id(
            &db.conn,
            child_id,
            "session-deleted-parent".into(),
        )
        .await
        .unwrap();

        let store = RunStore::new(db.clone());
        let root = "deleted-parent-root-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(root, "conn-deleted-parent", Utc::now())
            .await
            .unwrap();
        settle_completed(&store, root).await;

        conversation_service::soft_delete(&db.conn, parent_id)
            .await
            .unwrap();

        let err = store
            .admit_continue_reserving(ContinueRunAdmission {
                task_id: "deleted-parent-continue".into(),
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-deleted-parent-continue".into(),
                target_task_id: root.into(),
                task_preview: derive_task_preview("continue after parent deletion"),
                request_fingerprint: "deleted-parent-fingerprint".into(),
                work_unit_key: Some("unit-a".into()),
            })
            .await
            .unwrap_err();

        assert!(
            matches!(err, TaskStoreError::NotContinuable(_)),
            "soft-deleted parent must fail closed: {err:?}"
        );
    }

    #[tokio::test]
    async fn continue_rejects_unrecognized_agent_identity() {
        use crate::db::entities::conversation;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "unknown-agent-root-4111-8111-111111111111").await;
        conversation_service::update_external_id(
            &db.conn,
            child_id,
            "session-unknown-agent".into(),
        )
        .await
        .unwrap();

        let store = RunStore::new(db.clone());
        let root = "unknown-agent-root-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(root, "conn-unknown-agent", Utc::now())
            .await
            .unwrap();
        settle_completed(&store, root).await;

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut child = child.into_active_model();
        child.agent_type = Set("retired-agent".into());
        child.update(&db.conn).await.unwrap();
        let run = DelegationTaskRun::find_by_id(root)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut run = run.into_active_model();
        run.agent_type = Set("retired-agent".into());
        run.update(&db.conn).await.unwrap();

        let err = store
            .admit_continue_reserving(ContinueRunAdmission {
                task_id: "unknown-agent-continue".into(),
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-unknown-agent-continue".into(),
                target_task_id: root.into(),
                task_preview: derive_task_preview("continue unknown agent"),
                request_fingerprint: "unknown-agent-fingerprint".into(),
                work_unit_key: Some("unit-a".into()),
            })
            .await
            .unwrap_err();

        assert!(
            matches!(err, TaskStoreError::NotContinuable(_)),
            "unknown raw agent identity must not fall back to another agent: {err:?}"
        );
    }

    /// A continuation that has evaluated a source child must not admit after a
    /// replacement has superseded that child. The gate makes the old
    /// eligibility-to-insert gap deterministic.
    #[tokio::test]
    async fn continue_and_replacement_admission_cannot_revive_a_superseded_child() {
        use crate::db::entities::conversation;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};
        use std::time::Duration;

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, source_child_id) =
            seed_parent_child(&db, "continue-replacement-race-source").await;
        let child = conversation::Entity::find_by_id(source_child_id)
            .one(&db.conn)
            .await
            .expect("load source child")
            .expect("source child");
        let mut child = child.into_active_model();
        child.agent_type = Set("cursor".into());
        child.external_id = Set(Some("cursor-session-race".into()));
        child.update(&db.conn).await.expect("update source child");

        let store = Arc::new(RunStore::new(db.clone()));
        let source_task_id = "continue-replacement-race-source";
        let mut source = sample_insert(source_task_id, parent_id, source_child_id, 1, None);
        source.agent_type = "cursor".into();
        store
            .insert_reserving(source)
            .await
            .expect("source reserve");
        store
            .promote_running(source_task_id, "source-connection", Utc::now())
            .await
            .expect("source promote");
        settle_completed(store.as_ref(), source_task_id).await;

        let replacement_folder = seed_folder(&db, "/tmp/codeg-continue-replacement-race").await;
        let replacement_child = conversation_service::create_with_delegation(
            &db.conn,
            replacement_folder,
            AgentType::Cursor,
            Some("replacement child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-continue-replacement-race".into(),
                delegation_call_id: "continue-replacement-race-replacement".into(),
            }),
        )
        .await
        .expect("replacement child");
        let mut replacement = base_replacement_insert(
            "continue-replacement-race-replacement",
            parent_id,
            replacement_child.id,
            source_task_id,
            REPLACEMENT_REASON_NOT_SUPPORTED,
        );
        replacement.agent_type = "cursor".into();

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        store
            .install_continue_admission_gate(entered_tx, release_rx)
            .await;
        let continuation = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .admit_continue_reserving(ContinueRunAdmission {
                        task_id: "continue-replacement-race-continuation".into(),
                        parent_conversation_id: parent_id,
                        parent_tool_use_id: "tu-continue-replacement-race-continuation".into(),
                        target_task_id: source_task_id.into(),
                        task_preview: derive_task_preview("continue source child"),
                        request_fingerprint: "continue-replacement-race-fingerprint".into(),
                        work_unit_key: Some("unit-a".into()),
                    })
                    .await
            })
        };
        tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, entered_rx)
            .await
            .expect("continuation eligibility did not enter gate within 5s")
            .expect("continuation eligibility entered");

        let mut replacement = {
            let store = store.clone();
            tokio::spawn(async move { store.admit_gen1_reserving(replacement).await })
        };

        // The fixed transaction owns the SQLite writer before this gate. A
        // replacement therefore cannot commit while the continuation is held.
        let replacement_before_release =
            tokio::time::timeout(Duration::from_millis(100), &mut replacement).await;
        let early_replacement = match replacement_before_release {
            Ok(joined) => {
                let outcome = joined.expect("replacement join");
                assert!(
                    !matches!(outcome, Ok(Gen1AdmitOutcome::Created(_))),
                    "replacement committed during held continuation admission: {outcome:?}"
                );
                Some(outcome)
            }
            Err(_) => None,
        };

        let _ = release_tx.send(());
        let continuation = continuation
            .await
            .expect("continuation join")
            .expect("continuation must own admission");
        assert!(matches!(continuation, ContinueAdmitOutcome::Created(_)));

        let replacement = match early_replacement {
            Some(outcome) => outcome,
            None => replacement.await.expect("replacement join"),
        };
        assert!(
            !matches!(replacement, Ok(Gen1AdmitOutcome::Created(_))),
            "replacement must lose once continuation reserved the source child: {replacement:?}"
        );
        assert!(
            store
                .load_by_task_id("continue-replacement-race-replacement")
                .await
                .expect("load replacement")
                .is_none(),
            "a replacement must not supersede the source after its continuation wins"
        );
    }

    #[tokio::test]
    async fn replacement_admission_checks_reason_and_charges_only_on_running() {
        use crate::db::service::conversation_service;

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "replacement-root-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let root = "replacement-root-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(root, "conn-root", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                root,
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        let folder = seed_folder(&db, "/tmp/codeg-replacement-admission").await;
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("replacement".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-replacement".into(),
                delegation_call_id: "replacement-1".into(),
            }),
        )
        .await
        .unwrap();
        let mut replacement = sample_insert("replacement-1", parent_id, child.id, 1, None);
        replacement.lineage_root_task_id = root.into();
        replacement.work_unit_key = Some("unit-a".into());
        replacement.admission_class = AdmissionClass::Replacement;
        replacement.replaced_task_id = Some(root.into());
        replacement.replacement_reason = Some("not_supported".into());
        let err = store
            .admit_gen1_reserving(replacement.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, TaskStoreError::InvalidReplacement(_)));

        replacement.replacement_reason = Some("unresumable".into());
        store
            .admit_gen1_reserving(replacement.clone())
            .await
            .expect("matching owned latest terminal source is a replacement candidate");
        let (_, replacement_count) = lineage_counts(&db, root).await;
        assert_eq!(replacement_count, 0, "reserving replacement is free");

        // A pre-admission replacement failure has not consumed the rail. The
        // Skill may retry the same replacement linkage and only the retry that
        // reaches running is charged.
        store
            .settle_terminal(
                "replacement-1",
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();
        let retry_child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("replacement retry".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-replacement-retry".into(),
                delegation_call_id: "replacement-retry".into(),
            }),
        )
        .await
        .unwrap();
        let mut retry = replacement.clone();
        retry.task_id = "replacement-retry".into();
        retry.root_task_id = "replacement-retry".into();
        retry.parent_tool_use_id = Some("tu-replacement-retry".into());
        retry.child_conversation_id = retry_child.id;
        store
            .admit_gen1_reserving(retry.clone())
            .await
            .expect("pre-admission replacement retry is allowed");
        let (_, replacement_count) = lineage_counts(&db, root).await;
        assert_eq!(replacement_count, 0, "retry remains free while reserving");
        store
            .promote_running("replacement-retry", "conn-replacement", Utc::now())
            .await
            .unwrap();
        let (_, replacement_count) = lineage_counts(&db, root).await;
        assert_eq!(
            replacement_count, 1,
            "only running admission charges replacement"
        );

        let second_child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("second replacement".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-replacement-second".into(),
                delegation_call_id: "replacement-second".into(),
            }),
        )
        .await
        .unwrap();
        let mut second = replacement;
        second.task_id = "replacement-second".into();
        second.root_task_id = "replacement-second".into();
        second.parent_tool_use_id = Some("tu-replacement-second".into());
        second.child_conversation_id = second_child.id;
        let err = store.admit_gen1_reserving(second).await.unwrap_err();
        assert!(matches!(err, TaskStoreError::BudgetExhausted(_)));
    }

    #[tokio::test]
    async fn replacement_missing_or_foreign_source_is_not_found() {
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_a, child_a) =
            seed_parent_child(&db, "replacement-source-4111-8111-111111111111").await;
        let (parent_b, child_b) =
            seed_parent_child(&db, "replacement-target-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());

        let mut missing = sample_insert("replacement-missing", parent_b, child_b, 1, None);
        missing.admission_class = AdmissionClass::Replacement;
        missing.replaced_task_id = Some("missing-source".into());
        missing.replacement_reason = Some("unresumable".into());
        missing.lineage_root_task_id = "missing-source".into();
        let err = store.admit_gen1_reserving(missing).await.unwrap_err();
        assert!(matches!(err, TaskStoreError::NotFound(_)));

        let source_task_id = "replacement-source";
        store
            .insert_reserving(sample_insert(source_task_id, parent_a, child_a, 1, None))
            .await
            .expect("source reserve");
        // Even a malformed source identity must remain redacted to a foreign
        // parent. Ownership checks take precedence over source validation.
        let source = DelegationTaskRun::find_by_id(source_task_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut source = source.into_active_model();
        source.agent_type = Set("retired-agent".into());
        source.update(&db.conn).await.unwrap();
        let mut foreign = sample_insert("replacement-foreign", parent_b, child_b, 1, None);
        foreign.admission_class = AdmissionClass::Replacement;
        foreign.replaced_task_id = Some(source_task_id.into());
        foreign.replacement_reason = Some("unresumable".into());
        foreign.lineage_root_task_id = source_task_id.into();
        let err = store.admit_gen1_reserving(foreign).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::NotFound(_)),
            "cross-parent replacement source must not reveal ownership: {err:?}"
        );
    }

    #[tokio::test]
    async fn replacement_rejects_soft_deleted_parent() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-deleted-parent-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-deleted-parent-src-4111-8111-111111111111";
        seed_unresumable_latest_source(&store, parent_id, child_id, source, Some("unit-a")).await;

        let replacement_child = new_replacement_child(
            &db,
            parent_id,
            "tu-repl-deleted-parent",
            "repl-deleted-parent",
        )
        .await;
        conversation_service::soft_delete(&db.conn, parent_id)
            .await
            .unwrap();

        let err = store
            .admit_gen1_reserving(base_replacement_insert(
                "repl-deleted-parent",
                parent_id,
                replacement_child,
                source,
                "unresumable",
            ))
            .await
            .unwrap_err();

        assert!(
            matches!(err, TaskStoreError::NotFound(_)),
            "soft-deleted parent must fail closed without revealing the source: {err:?}"
        );
    }

    #[tokio::test]
    async fn replacement_rejects_unrecognized_agent_identity() {
        use crate::db::entities::conversation;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-unknown-agent-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-unknown-agent-src-4111-8111-111111111111";
        seed_unresumable_latest_source(&store, parent_id, child_id, source, Some("unit-a")).await;

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut child = child.into_active_model();
        child.agent_type = Set("retired-agent".into());
        child.update(&db.conn).await.unwrap();
        let source_run = DelegationTaskRun::find_by_id(source)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut source_run = source_run.into_active_model();
        source_run.agent_type = Set("retired-agent".into());
        source_run.update(&db.conn).await.unwrap();

        let replacement_child = new_replacement_child(
            &db,
            parent_id,
            "tu-repl-unknown-agent",
            "repl-unknown-agent",
        )
        .await;
        let child = conversation::Entity::find_by_id(replacement_child)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut child = child.into_active_model();
        child.agent_type = Set("claude_code".into());
        child.update(&db.conn).await.unwrap();

        let mut insert = base_replacement_insert(
            "repl-unknown-agent",
            parent_id,
            replacement_child,
            source,
            "unresumable",
        );
        insert.agent_type = "claude_code".into();
        let err = store.admit_gen1_reserving(insert).await.unwrap_err();

        assert!(
            matches!(err, TaskStoreError::InvalidReplacement(_)),
            "unknown source agent must not permit a different replacement agent: {err:?}"
        );
    }

    /// Helper: insert → promote → fail terminal as `unresumable` so the source
    /// is a valid replacement candidate for the ownership/route matrix cases.
    async fn seed_unresumable_latest_source(
        store: &RunStore,
        parent_id: i32,
        child_id: i32,
        source_task_id: &str,
        work_unit_key: Option<&str>,
    ) {
        let mut source = sample_insert(source_task_id, parent_id, child_id, 1, None);
        source.work_unit_key = work_unit_key.map(|s| s.into());
        store.insert_reserving(source).await.expect("source reserve");
        store
            .promote_running(source_task_id, format!("conn-{source_task_id}"), Utc::now())
            .await
            .expect("source promote");
        store
            .settle_terminal(
                source_task_id,
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .expect("source terminal");
    }

    async fn new_replacement_child(
        db: &AppDatabase,
        parent_id: i32,
        tool: &str,
        call_id: &str,
    ) -> i32 {
        use crate::db::service::conversation_service;
        let folder = seed_folder(db, &format!("/tmp/codeg-repl-{call_id}")).await;
        conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("replacement child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: tool.into(),
                delegation_call_id: call_id.into(),
            }),
        )
        .await
        .expect("replacement child")
        .id
    }

    fn base_replacement_insert(
        task_id: &str,
        parent_id: i32,
        child_id: i32,
        source_task_id: &str,
        reason: &str,
    ) -> ReservingRunInsert {
        let mut insert = sample_insert(task_id, parent_id, child_id, 1, None);
        insert.admission_class = AdmissionClass::Replacement;
        insert.replaced_task_id = Some(source_task_id.into());
        insert.replacement_reason = Some(reason.into());
        insert.lineage_root_task_id = source_task_id.into();
        insert
    }

    #[tokio::test]
    async fn replacement_rejects_agent_type_mismatch() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-agent-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-agent-src-4111-8111-111111111111";
        seed_unresumable_latest_source(&store, parent_id, child_id, source, Some("unit-a")).await;

        let repl_child = new_replacement_child(&db, parent_id, "tu-agent", "repl-agent").await;
        let mut insert =
            base_replacement_insert("repl-agent", parent_id, repl_child, source, "unresumable");
        insert.agent_type = "claude-code".into();
        let err = store.admit_gen1_reserving(insert).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::InvalidReplacement(ref m) if m.contains("agent_type")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn replacement_rejects_profile_mismatch() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-prof-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-prof-src-4111-8111-111111111111";
        let mut src = sample_insert(source, parent_id, child_id, 1, None);
        src.profile_id = Some("profile-a".into());
        store.insert_reserving(src).await.unwrap();
        store
            .promote_running(source, "conn-prof", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                source,
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        let repl_child = new_replacement_child(&db, parent_id, "tu-prof", "repl-prof").await;
        let mut insert =
            base_replacement_insert("repl-prof", parent_id, repl_child, source, "unresumable");
        insert.profile_id = Some("profile-b".into());
        let err = store.admit_gen1_reserving(insert).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::InvalidReplacement(ref m) if m.contains("profile_id")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn replacement_rejects_workspace_mismatch() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-ws-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-ws-src-4111-8111-111111111111";
        seed_unresumable_latest_source(&store, parent_id, child_id, source, Some("unit-a")).await;

        let repl_child = new_replacement_child(&db, parent_id, "tu-ws", "repl-ws").await;
        let mut insert =
            base_replacement_insert("repl-ws", parent_id, repl_child, source, "unresumable");
        insert.workspace_path = Some("/tmp/other-workspace".into());
        let err = store.admit_gen1_reserving(insert).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::InvalidReplacement(ref m) if m.contains("workspace")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn replacement_rejects_non_terminal_or_not_latest_source() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-nt-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-nt-src-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(source, parent_id, child_id, 1, None))
            .await
            .unwrap();
        // Still reserving / non-terminal → reject.
        let repl_child = new_replacement_child(&db, parent_id, "tu-nt", "repl-nt").await;
        let insert =
            base_replacement_insert("repl-nt", parent_id, repl_child, source, "unresumable");
        let err = store.admit_gen1_reserving(insert).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::InvalidReplacement(ref m) if m.contains("latest terminal")),
            "non-terminal: {err:?}"
        );

        store
            .promote_running(source, "conn-nt", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                source,
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        // Newer run on same child supersedes "latest".
        let mut gen2 = sample_insert("repl-nt-gen2", parent_id, child_id, 2, Some(source));
        gen2.lineage_root_task_id = source.into();
        gen2.root_task_id = source.into();
        store.insert_reserving(gen2).await.unwrap();
        store
            .promote_running("repl-nt-gen2", "conn-nt-gen2", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                "repl-nt-gen2",
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        let repl_child2 = new_replacement_child(&db, parent_id, "tu-nt2", "repl-nt2").await;
        let insert =
            base_replacement_insert("repl-nt2", parent_id, repl_child2, source, "unresumable");
        let err = store.admit_gen1_reserving(insert).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::InvalidReplacement(ref m) if m.contains("latest terminal")),
            "not-latest: {err:?}"
        );
    }

    #[tokio::test]
    async fn replacement_rejects_reason_mismatch_for_each_reason() {
        use crate::db::entities::conversation;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-reason-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-reason-src-4111-8111-111111111111";
        // Terminal with plain completed status — no unresumable / not_supported /
        // budget-exhausted durable signals. External session id must be present
        // so unresumable does not match via missing_external_session.
        store
            .insert_reserving(sample_insert(source, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(source, "conn-reason", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                source,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .unwrap();
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut child = child.into_active_model();
        child.external_id = Set(Some("reason-mismatch-session".into()));
        child.update(&db.conn).await.unwrap();

        for reason in [
            "unresumable",
            "budget_exhausted_continue",
            "not_supported",
            "unknown_reason",
        ] {
            let repl_child =
                new_replacement_child(&db, parent_id, &format!("tu-{reason}"), &format!("repl-{reason}"))
                    .await;
            let insert =
                base_replacement_insert(&format!("repl-{reason}"), parent_id, repl_child, source, reason);
            let err = store.admit_gen1_reserving(insert).await.unwrap_err();
            assert!(
                matches!(err, TaskStoreError::InvalidReplacement(_)),
                "reason {reason} must mismatch completed durable state: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn replacement_charges_lineage_and_work_unit_counters_only_on_running() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-charge-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-charge-src-4111-8111-111111111111";
        seed_unresumable_latest_source(&store, parent_id, child_id, source, Some("unit-charge")).await;

        let repl_child = new_replacement_child(&db, parent_id, "tu-charge", "repl-charge").await;
        let mut insert =
            base_replacement_insert("repl-charge", parent_id, repl_child, source, "unresumable");
        insert.work_unit_key = Some("unit-charge".into());
        store
            .admit_gen1_reserving(insert)
            .await
            .expect("matching replacement admits");

        let (_, lineage_repl) = lineage_counts(&db, source).await;
        let (_, wu_repl) = work_unit_counts(&db, parent_id, "unit-charge").await;
        assert_eq!(lineage_repl, 0, "reserving must not charge lineage");
        assert_eq!(wu_repl, 0, "reserving must not charge work-unit");

        store
            .promote_running("repl-charge", "conn-charge", Utc::now())
            .await
            .expect("promote charges");
        let (_, lineage_repl) = lineage_counts(&db, source).await;
        let (_, wu_repl) = work_unit_counts(&db, parent_id, "unit-charge").await;
        assert_eq!(lineage_repl, 1, "running charges lineage replacement");
        assert_eq!(wu_repl, 1, "running charges work-unit replacement");
    }

    #[tokio::test]
    async fn replacement_second_running_is_budget_exhausted() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "repl-budget-src-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let source = "repl-budget-src-4111-8111-111111111111";
        seed_unresumable_latest_source(&store, parent_id, child_id, source, Some("unit-budget")).await;

        let first_child = new_replacement_child(&db, parent_id, "tu-b1", "repl-b1").await;
        let mut first =
            base_replacement_insert("repl-b1", parent_id, first_child, source, "unresumable");
        first.work_unit_key = Some("unit-budget".into());
        store.admit_gen1_reserving(first).await.unwrap();
        store
            .promote_running("repl-b1", "conn-b1", Utc::now())
            .await
            .unwrap();
        // Settle so the gen-1 work-unit partial unique no longer blocks; the
        // budget rail (not the unique index) must refuse the second attempt.
        store
            .settle_terminal(
                "repl-b1",
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        // Fresh terminal source on another child (ownership/latest), inheriting
        // the exhausted lineage root.
        let second_source_child =
            new_replacement_child(&db, parent_id, "tu-b2-src", "repl-b2-src").await;
        let mut second_source =
            sample_insert("repl-b2-src", parent_id, second_source_child, 1, None);
        second_source.lineage_root_task_id = source.into();
        second_source.work_unit_key = Some("unit-budget".into());
        store.insert_reserving(second_source).await.unwrap();
        store
            .promote_running("repl-b2-src", "conn-b2-src", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                "repl-b2-src",
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        let second_child = new_replacement_child(&db, parent_id, "tu-b2", "repl-b2").await;
        let mut second =
            base_replacement_insert("repl-b2", parent_id, second_child, "repl-b2-src", "unresumable");
        second.lineage_root_task_id = source.into();
        second.work_unit_key = Some("unit-budget".into());
        let err = store.admit_gen1_reserving(second).await.unwrap_err();
        assert!(
            matches!(err, TaskStoreError::BudgetExhausted(_)),
            "second replacement after charged first must exhaust: {err:?}"
        );
    }

    #[tokio::test]
    async fn work_unit_bypass_rejects_established_lineage_but_ignores_never_running_prior() {
        use crate::db::service::conversation_service;

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "bypass-root-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let root = "bypass-root-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .unwrap();
        store
            .promote_running(root, "conn-root", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                root,
                TerminalTaskWrite::failed("unresumable", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();

        let folder = seed_folder(&db, "/tmp/codeg-work-unit-bypass").await;
        let replacement_child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("bypass candidate".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-bypass".into(),
                delegation_call_id: "bypass-candidate".into(),
            }),
        )
        .await
        .unwrap();
        let mut bypass =
            sample_insert("bypass-candidate", parent_id, replacement_child.id, 1, None);
        bypass.work_unit_key = Some("unit-a".into());
        let err = store.admit_gen1_reserving(bypass).await.unwrap_err();
        assert!(matches!(err, TaskStoreError::InvalidReplacement(_)));

        let never_running_child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("never running prior".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-never-running".into(),
                delegation_call_id: "never-running".into(),
            }),
        )
        .await
        .unwrap();
        let mut never_running =
            sample_insert("never-running", parent_id, never_running_child.id, 1, None);
        never_running.work_unit_key = Some("unit-never-running".into());
        store.insert_reserving(never_running).await.unwrap();
        store
            .settle_terminal(
                "never-running",
                TerminalTaskWrite::failed(
                    "spawn_failed",
                    Utc::now(),
                    ConversationStatus::Cancelled,
                ),
            )
            .await
            .unwrap();

        let retry_child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("fresh first dispatch".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent_id,
                parent_tool_use_id: "tu-never-running-retry".into(),
                delegation_call_id: "never-running-retry".into(),
            }),
        )
        .await
        .unwrap();
        let mut retry = sample_insert("never-running-retry", parent_id, retry_child.id, 1, None);
        retry.work_unit_key = Some("unit-never-running".into());
        assert!(matches!(
            store.admit_gen1_reserving(retry).await,
            Ok(Gen1AdmitOutcome::Created(_))
        ));
    }

    // ---- RunStore test gate fail-fast (Task 5) -------------------------------

    /// Pause only after SQLite setup: `start_paused` races the sqlx pool
    /// connect timeout against virtual time and flaking as `PoolTimedOut`.
    #[tokio::test]
    async fn settle_gate_release_timeout_returns_permanent_error() {
        // Install a settle gate, keep the release sender alive, start
        // settle_terminal, observe `entered`, advance five seconds, and assert
        // TaskStoreError::Permanent contains "test run_store settle gate timed out".
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "settle-gate-timeout-4111-8111-111111111111").await;
        let store = Arc::new(RunStore::new(db));
        let task_id = "settle-gate-timeout-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");
        store
            .promote_running(task_id, "conn-settle-gate-timeout", Utc::now())
            .await
            .expect("promote");

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
        store
            .install_settle_gate(entered_tx, release_rx)
            .await;

        let settle = {
            let store = store.clone();
            let task_id = task_id.to_string();
            tokio::spawn(async move {
                store
                    .settle_terminal(
                        &task_id,
                        TerminalTaskWrite::completed(
                            Utc::now(),
                            ConversationStatus::PendingReview,
                        ),
                    )
                    .await
            })
        };
        tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, entered_rx)
            .await
            .expect("settlement did not enter gate within 5s")
            .expect("settlement gate dropped before entry");

        // Gate is parked on the release oneshot; freeze clock and jump 5s.
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(5)).await;

        let err = tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, settle)
            .await
            .expect("settle join did not complete within 5s")
            .expect("join settle")
            .expect_err("unreleased settle gate must fail settle");
        match err {
            TaskStoreError::Permanent(msg) => {
                assert!(
                    msg.contains("test run_store settle gate timed out"),
                    "unexpected permanent message: {msg}"
                );
            }
            other => panic!("expected Permanent timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn settle_gate_release_dropped_returns_permanent_error() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "settle-gate-drop-4111-8111-111111111111").await;
        let store = Arc::new(RunStore::new(db));
        let task_id = "settle-gate-drop-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");
        store
            .promote_running(task_id, "conn-settle-gate-drop", Utc::now())
            .await
            .expect("promote");

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        store
            .install_settle_gate(entered_tx, release_rx)
            .await;

        let settle = {
            let store = store.clone();
            let task_id = task_id.to_string();
            tokio::spawn(async move {
                store
                    .settle_terminal(
                        &task_id,
                        TerminalTaskWrite::completed(
                            Utc::now(),
                            ConversationStatus::PendingReview,
                        ),
                    )
                    .await
            })
        };
        tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, entered_rx)
            .await
            .expect("settlement did not enter gate within 5s")
            .expect("settlement gate dropped before entry");

        drop(release_tx);

        let err = tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, settle)
            .await
            .expect("settle join did not complete within 5s")
            .expect("join settle")
            .expect_err("dropped settle release sender must fail settle");
        match err {
            TaskStoreError::Permanent(msg) => {
                assert!(
                    msg.contains("test run_store settle gate release dropped")
                        || msg.contains("test run_store settle gate timed out"),
                    "unexpected permanent message: {msg}"
                );
            }
            other => panic!("expected Permanent on dropped release, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn continue_admission_gate_release_timeout_returns_permanent_error() {
        use crate::db::entities::conversation;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "continue-gate-timeout-4111-8111-111111111111").await;
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("load child")
            .expect("child");
        let mut child = child.into_active_model();
        child.external_id = Set(Some("session-continue-gate-timeout".into()));
        child.update(&db.conn).await.expect("set external id");

        let store = Arc::new(RunStore::new(db));
        let root = "continue-gate-timeout-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .expect("insert root");
        store
            .promote_running(root, "conn-continue-gate-timeout", Utc::now())
            .await
            .expect("promote root");
        settle_completed(store.as_ref(), root).await;

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (_release_tx, release_rx) = tokio::sync::oneshot::channel();
        store
            .install_continue_admission_gate(entered_tx, release_rx)
            .await;

        let continuation = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .admit_continue_reserving(ContinueRunAdmission {
                        task_id: "continue-gate-timeout-child".into(),
                        parent_conversation_id: parent_id,
                        parent_tool_use_id: "tu-continue-gate-timeout".into(),
                        target_task_id: root.into(),
                        task_preview: derive_task_preview("continue under unreleased gate"),
                        request_fingerprint: "continue-gate-timeout-fp".into(),
                        work_unit_key: Some("unit-a".into()),
                    })
                    .await
            })
        };
        tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, entered_rx)
            .await
            .expect("continuation did not enter gate within 5s")
            .expect("continue admission gate dropped before entry");

        // Trip the mid-txn gate with virtual time, then resume wall clock.
        // The gate sits inside an open SQLite writer txn; after Permanent the
        // txn must roll back on real I/O. A second paused-clock outer timeout
        // races that unwind (auto-advance / idle timer) and flakes.
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        tokio::time::resume();

        let err = tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, continuation)
            .await
            .expect("continuation join did not complete within 5s")
            .expect("join continuation")
            .expect_err("unreleased continue-admission gate must fail admit");
        match err {
            TaskStoreError::Permanent(msg) => {
                assert!(
                    msg.contains("test run_store continue_admission gate timed out"),
                    "unexpected permanent message: {msg}"
                );
            }
            other => panic!("expected Permanent timeout, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn continue_admission_gate_release_dropped_returns_permanent_error() {
        use crate::db::entities::conversation;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "continue-gate-drop-4111-8111-111111111111").await;
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .expect("load child")
            .expect("child");
        let mut child = child.into_active_model();
        child.external_id = Set(Some("session-continue-gate-drop".into()));
        child.update(&db.conn).await.expect("set external id");

        let store = Arc::new(RunStore::new(db));
        let root = "continue-gate-drop-4111-8111-111111111111";
        store
            .insert_reserving(sample_insert(root, parent_id, child_id, 1, None))
            .await
            .expect("insert root");
        store
            .promote_running(root, "conn-continue-gate-drop", Utc::now())
            .await
            .expect("promote root");
        settle_completed(store.as_ref(), root).await;

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        store
            .install_continue_admission_gate(entered_tx, release_rx)
            .await;

        let continuation = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .admit_continue_reserving(ContinueRunAdmission {
                        task_id: "continue-gate-drop-child".into(),
                        parent_conversation_id: parent_id,
                        parent_tool_use_id: "tu-continue-gate-drop".into(),
                        target_task_id: root.into(),
                        task_preview: derive_task_preview("continue under dropped gate"),
                        request_fingerprint: "continue-gate-drop-fp".into(),
                        work_unit_key: Some("unit-a".into()),
                    })
                    .await
            })
        };
        tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, entered_rx)
            .await
            .expect("continuation did not enter gate within 5s")
            .expect("continue admission gate dropped before entry");

        drop(release_tx);

        let err = tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, continuation)
            .await
            .expect("continuation join did not complete within 5s")
            .expect("join continuation")
            .expect_err("dropped continue-admission release must fail admit");
        match err {
            TaskStoreError::Permanent(msg) => {
                assert!(
                    msg.contains("test run_store continue_admission gate release dropped")
                        || msg.contains("test run_store continue_admission gate timed out"),
                    "unexpected permanent message: {msg}"
                );
            }
            other => panic!("expected Permanent on dropped release, got {other:?}"),
        }
    }

    // ---- write-first promote_running (Task 1) --------------------------------

    async fn seed_reserving_promote(
        task_id: &str,
    ) -> (Arc<AppDatabase>, RunStore, i32, i32) {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) = seed_parent_child(&db, task_id).await;
        let store = RunStore::new(db.clone());
        store
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert reserving");
        (db, store, parent_id, child_id)
    }

    fn assert_promoted(kind: &PromoteRunningKind) -> &PersistedRun {
        match kind {
            PromoteRunningKind::Promoted { run } => run,
            other => panic!("expected Promoted, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn promote_write_first_survives_concurrent_writer() {
        use std::time::Duration;

        use sea_orm::{
            ConnectOptions, ConnectionTrait, Database, DbBackend, EntityTrait, Statement,
            TransactionTrait,
        };

        use crate::acp::delegation::store::{
            classify_sqlite_transient, extract_sqlite_codes, SqliteTransientClass,
        };
        use crate::db::test_helpers::fresh_disk_db;

        // Seed on a migrator pool, then reopen two single-connection WAL pools
        // so a concurrent writer can contend with promote's held claim lock.
        let dir = tempfile::tempdir().expect("tempdir");
        let migrate = Arc::new(fresh_disk_db(dir.path()).await);
        let task_id = "wf-promote-4111-8111-111111111111";
        let (parent_id, child_id) = seed_parent_child(&migrate, task_id).await;
        let store_seed = RunStore::new(migrate.clone());
        store_seed
            .insert_reserving(sample_insert(task_id, parent_id, child_id, 1, None))
            .await
            .expect("insert");
        let before_parent = conversation::Entity::find_by_id(parent_id)
            .one(&migrate.conn)
            .await
            .unwrap()
            .unwrap();
        let parent_updated_before = before_parent.updated_at;
        drop(store_seed);
        let migrate = Arc::try_unwrap(migrate).unwrap_or_else(|_| {
            panic!("migrator Arc unique after seed");
        });
        migrate.conn.close().await.expect("close migrator pool");
        let path = dir.path().join("source.db");

        async fn open_wal(path: &std::path::Path, busy_timeout_ms: u32) -> AppDatabase {
            let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
            let mut opts = ConnectOptions::new(url);
            opts.max_connections(1)
                .min_connections(1)
                .connect_timeout(Duration::from_secs(10))
                .sqlx_logging(false);
            let conn = Database::connect(opts).await.expect("open wal");
            for pragma in [
                "PRAGMA journal_mode=WAL;".to_owned(),
                format!("PRAGMA busy_timeout={busy_timeout_ms};"),
                "PRAGMA foreign_keys=ON;".to_owned(),
            ] {
                conn.execute(Statement::from_string(DbBackend::Sqlite, pragma))
                    .await
                    .expect("pragma");
            }
            AppDatabase { conn }
        }

        // Promote keeps a normal timeout; the probing writer uses 0 so a held
        // claim produces an immediate SQLITE_BUSY (not a long wait).
        let pool_a = Arc::new(open_wal(&path, 5000).await);
        let pool_b = Arc::new(open_wal(&path, 0).await);
        let store = Arc::new(RunStore::new(pool_a.clone()));

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        store
            .install_promote_claim_gate(entered_tx, release_rx)
            .await;

        let promote_store = store.clone();
        let promote = tokio::spawn(async move {
            promote_store
                .promote_running_detailed(task_id, "conn-wf", Utc::now())
                .await
        });

        // Wait until promote has taken the write-first claim (writer lock held).
        tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, entered_rx)
            .await
            .expect("promote did not enter claim gate")
            .expect("claim gate dropped");

        let writer_stamp = Utc::now() + chrono::Duration::seconds(42);

        // While promote holds the claim, a concurrent write must fail BUSY with
        // extractable SQLite codes (same raw DbErr shape as txn-body failures).
        let busy_err = pool_b
            .conn
            .transaction::<_, u64, sea_orm::DbErr>(|txn| {
                Box::pin(async move {
                    let res = conversation::Entity::update_many()
                        .col_expr(
                            conversation::Column::UpdatedAt,
                            Expr::value(writer_stamp),
                        )
                        .filter(conversation::Column::Id.eq(parent_id))
                        .exec(txn)
                        .await?;
                    Ok(res.rows_affected)
                })
            })
            .await
            .expect_err("writer must observe SQLITE_BUSY while promote holds claim");
        let busy_db = match busy_err {
            sea_orm::TransactionError::Connection(e)
            | sea_orm::TransactionError::Transaction(e) => e,
        };
        let codes = extract_sqlite_codes(&busy_db)
            .expect("real SQLite BUSY must expose primary/extended codes");
        assert_eq!(codes.primary, 5, "SQLITE_BUSY primary code");
        assert_eq!(
            classify_sqlite_transient(&busy_db),
            Some(SqliteTransientClass::Busy),
            "txn-body-shaped DbErr classifies via codes before stringification"
        );

        // Release promote; it must finish Promoted. Then the writer commits.
        release_tx.send(()).expect("release promote claim gate");
        let outcome = promote
            .await
            .expect("join promote")
            .expect("promote detailed result");
        assert_promoted(&outcome.kind);
        assert!(
            outcome.meta.attempts >= 1 && outcome.meta.attempts <= 3,
            "attempts in promote policy range: {:?}",
            outcome.meta
        );

        // Writer may keep busy_timeout=0; promote has committed so this succeeds.
        let writer_rows = pool_b
            .conn
            .transaction::<_, u64, sea_orm::DbErr>(|txn| {
                Box::pin(async move {
                    let res = conversation::Entity::update_many()
                        .col_expr(
                            conversation::Column::UpdatedAt,
                            Expr::value(writer_stamp),
                        )
                        .filter(conversation::Column::Id.eq(parent_id))
                        .exec(txn)
                        .await?;
                    Ok(res.rows_affected)
                })
            })
            .await
            .expect("writer transaction must succeed after promote commits");
        assert_eq!(writer_rows, 1, "writer must update the parent row once");

        // Durable final state: run promoted; concurrent parent write committed.
        let run = store.load_by_task_id(task_id).await.unwrap().unwrap();
        assert_eq!(run.run_status, DelegationRunStatus::Running);
        assert_eq!(run.child_connection_id.as_deref(), Some("conn-wf"));
        let parent_after = conversation::Entity::find_by_id(parent_id)
            .one(&pool_a.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            parent_after.updated_at, writer_stamp,
            "concurrent writer stamp must persist; before={parent_updated_before:?}"
        );
    }

    #[tokio::test]
    async fn promote_retries_busy_then_succeeds() {
        let (_db, store, _, _) =
            seed_reserving_promote("busy-ok-4111-8111-111111111111").await;
        store
            .push_promote_faults([PromoteTestFault::AfterClaimTransient(
                SqliteTransientClass::Busy,
            )])
            .await;
        let outcome = store
            .promote_running_detailed("busy-ok-4111-8111-111111111111", "conn-busy", Utc::now())
            .await
            .expect("detailed");
        assert_promoted(&outcome.kind);
        assert_eq!(outcome.meta.attempts, 2);
        assert_eq!(outcome.meta.busy_retries, 1);
        assert_eq!(outcome.meta.locked_retries, 0);
        assert_eq!(outcome.meta.busy_snapshot_retries, 0);
        let run = store
            .load_by_task_id("busy-ok-4111-8111-111111111111")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.run_status, DelegationRunStatus::Running);
    }

    #[tokio::test]
    async fn promote_retries_locked_then_succeeds() {
        let (_db, store, _, _) =
            seed_reserving_promote("locked-ok-4111-8111-111111111111").await;
        store
            .push_promote_faults([PromoteTestFault::AfterClaimTransient(
                SqliteTransientClass::Locked,
            )])
            .await;
        let outcome = store
            .promote_running_detailed(
                "locked-ok-4111-8111-111111111111",
                "conn-locked",
                Utc::now(),
            )
            .await
            .expect("detailed");
        assert_promoted(&outcome.kind);
        assert_eq!(outcome.meta.attempts, 2);
        assert_eq!(outcome.meta.locked_retries, 1);
    }

    #[tokio::test]
    async fn promote_retries_busy_snapshot_517_then_succeeds() {
        let (_db, store, _, _) =
            seed_reserving_promote("snap-ok-4111-8111-111111111111").await;
        store
            .push_promote_faults([PromoteTestFault::AfterClaimTransient(
                SqliteTransientClass::BusySnapshot,
            )])
            .await;
        let outcome = store
            .promote_running_detailed("snap-ok-4111-8111-111111111111", "conn-snap", Utc::now())
            .await
            .expect("detailed");
        assert_promoted(&outcome.kind);
        assert_eq!(outcome.meta.attempts, 2);
        assert_eq!(outcome.meta.busy_snapshot_retries, 1);
    }

    /// Projection-step SQLite transient must retry via raw DbErr classification,
    /// not collapse into Permanent (Important 1 residual).
    #[tokio::test]
    async fn promote_retries_projection_busy_then_succeeds() {
        let (_db, store, _, _) =
            seed_reserving_promote("proj-busy-4111-8111-111111111111").await;
        store
            .push_promote_faults([PromoteTestFault::AfterProjectionTransient(
                SqliteTransientClass::Busy,
            )])
            .await;
        let outcome = store
            .promote_running_detailed(
                "proj-busy-4111-8111-111111111111",
                "conn-proj-busy",
                Utc::now(),
            )
            .await
            .expect("detailed");
        assert_promoted(&outcome.kind);
        assert_eq!(outcome.meta.attempts, 2);
        assert_eq!(outcome.meta.busy_retries, 1);
        assert_eq!(outcome.meta.locked_retries, 0);
        assert_eq!(outcome.meta.busy_snapshot_retries, 0);
        let run = store
            .load_by_task_id("proj-busy-4111-8111-111111111111")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.run_status, DelegationRunStatus::Running);
    }

    #[tokio::test]
    async fn promote_retry_exhausted_no_partial_writes() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "retry-ex-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-retry-ex";
        // UnexpectedContinue so each failed attempt charges (then rolls back).
        store
            .insert_reserving(sample_insert_with(
                "retry-ex-4111-8111-111111111111",
                parent_id,
                child_id,
                2,
                Some(lineage),
                AdmissionClass::UnexpectedContinue,
                lineage,
                Some("unit-retry-ex"),
            ))
            .await
            .unwrap();
        store
            .push_promote_faults([
                PromoteTestFault::AfterBudgetTransient(SqliteTransientClass::Busy),
                PromoteTestFault::AfterBudgetTransient(SqliteTransientClass::Busy),
                PromoteTestFault::AfterBudgetTransient(SqliteTransientClass::Busy),
            ])
            .await;
        let outcome = store
            .promote_running_detailed("retry-ex-4111-8111-111111111111", "conn-ex", Utc::now())
            .await
            .expect("detailed");
        match outcome.kind {
            PromoteRunningKind::RetryExhausted {
                class: PromoteRetryClass::Busy,
                ..
            } => {}
            other => panic!("expected RetryExhausted Busy, got {other:?}"),
        }
        assert_eq!(outcome.meta.attempts, 3);
        assert_eq!(outcome.meta.busy_retries, 3);
        let run = store
            .load_by_task_id("retry-ex-4111-8111-111111111111")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.run_status, DelegationRunStatus::Reserving);
        assert!(run.reached_running_at.is_none());
        let (uc, rc) = lineage_counts(&db, lineage).await;
        assert_eq!(
            (uc, rc),
            (0, 0),
            "after-budget failures must roll back charge on every attempt"
        );
        let (wuc, _) = work_unit_counts(&db, parent_id, "unit-retry-ex").await;
        assert_eq!(wuc, 0, "work-unit charge must also roll back");
    }

    #[tokio::test]
    async fn promote_budget_exhaust_rolls_back_no_charge() {
        use crate::db::entities::delegation_lineage_budget;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "budget-roll-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-budget-roll";
        // Insert while rails still allow (preflight at insert is separate).
        store
            .insert_reserving(sample_insert_with(
                "uc-br-3",
                parent_id,
                child_id,
                2,
                Some(lineage),
                AdmissionClass::UnexpectedContinue,
                lineage,
                Some("unit-br"),
            ))
            .await
            .expect("reserving insert");
        // Fill lineage rail to the limit so promote charge refuses and rolls back.
        let row = delegation_lineage_budget::Entity::find_by_id(lineage)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("budget row after insert preflight");
        let mut active = row.into_active_model();
        active.unexpected_continue_count = Set(UNEXPECTED_CONTINUE_LIMIT);
        active.update(&db.conn).await.expect("max out lineage rail");

        let outcome = store
            .promote_running_detailed("uc-br-3", "conn-br3", Utc::now())
            .await
            .expect("detailed");
        match outcome.kind {
            PromoteRunningKind::BudgetExhausted { .. } => {}
            other => panic!("expected BudgetExhausted, got {other:?}"),
        }
        let run = store.load_by_task_id("uc-br-3").await.unwrap().unwrap();
        assert_eq!(run.run_status, DelegationRunStatus::Reserving);
        let (uc, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc, UNEXPECTED_CONTINUE_LIMIT, "counter must not advance past limit");
    }

    #[tokio::test]
    async fn promote_success_charges_recovery_budget_exactly_once() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "charge-once-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-charge-once";
        store
            .insert_reserving(sample_insert_with(
                "charge-once-run",
                parent_id,
                child_id,
                2,
                Some(lineage),
                AdmissionClass::UnexpectedContinue,
                lineage,
                Some("unit-charge-once"),
            ))
            .await
            .unwrap();
        let outcome = store
            .promote_running_detailed("charge-once-run", "conn-once", Utc::now())
            .await
            .unwrap();
        assert_promoted(&outcome.kind);
        let (uc, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc, 1);
        let (wuc, _) = work_unit_counts(&db, parent_id, "unit-charge-once").await;
        assert_eq!(wuc, 1);
        // Idempotent re-promote must not double-charge.
        let again = store
            .promote_running_detailed("charge-once-run", "conn-once", Utc::now())
            .await
            .unwrap();
        match again.kind {
            PromoteRunningKind::AlreadyRunning { .. } => {}
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
        let (uc2, _) = lineage_counts(&db, lineage).await;
        assert_eq!(uc2, 1, "idempotent promote must not re-charge");
    }

    #[tokio::test]
    async fn promote_zero_row_already_running_idempotent() {
        let (_db, store, _, _) =
            seed_reserving_promote("zero-run-4111-8111-111111111111").await;
        store
            .promote_running("zero-run-4111-8111-111111111111", "conn-z", Utc::now())
            .await
            .unwrap();
        let outcome = store
            .promote_running_detailed("zero-run-4111-8111-111111111111", "conn-z", Utc::now())
            .await
            .unwrap();
        match outcome.kind {
            PromoteRunningKind::AlreadyRunning { run } => {
                assert_eq!(run.child_connection_id.as_deref(), Some("conn-z"));
            }
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn promote_zero_row_terminal_replays_winner() {
        let (_db, store, _, _) =
            seed_reserving_promote("zero-term-4111-8111-111111111111").await;
        store
            .promote_running("zero-term-4111-8111-111111111111", "conn-t", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                "zero-term-4111-8111-111111111111",
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
            )
            .await
            .unwrap();
        let outcome = store
            .promote_running_detailed("zero-term-4111-8111-111111111111", "conn-t", Utc::now())
            .await
            .unwrap();
        match outcome.kind {
            PromoteRunningKind::TerminalWinner { run } => {
                assert_eq!(run.run_status, DelegationRunStatus::Completed);
            }
            other => panic!("expected TerminalWinner, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn promote_zero_row_ownership_conflict() {
        let (_db, store, _, _) =
            seed_reserving_promote("zero-own-4111-8111-111111111111").await;
        store
            .promote_running("zero-own-4111-8111-111111111111", "conn-owner-a", Utc::now())
            .await
            .unwrap();
        let outcome = store
            .promote_running_detailed(
                "zero-own-4111-8111-111111111111",
                "conn-owner-b",
                Utc::now(),
            )
            .await
            .unwrap();
        match outcome.kind {
            PromoteRunningKind::StateConflict {
                class: PromoteConflictClass::Ownership,
                ..
            } => {}
            other => panic!("expected Ownership conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn promote_commit_ambiguity_reread_running_is_success() {
        let (_db, store, _, _) =
            seed_reserving_promote("amb-run-4111-8111-111111111111").await;
        store
            .promote_running("amb-run-4111-8111-111111111111", "conn-amb", Utc::now())
            .await
            .unwrap();
        store
            .push_promote_faults([PromoteTestFault::AmbiguousPermanent {
                message: "simulated commit I/O".into(),
            }])
            .await;
        let outcome = store
            .promote_running_detailed("amb-run-4111-8111-111111111111", "conn-amb", Utc::now())
            .await
            .unwrap();
        match outcome.kind {
            PromoteRunningKind::Promoted { run } | PromoteRunningKind::AlreadyRunning { run } => {
                assert_eq!(run.run_status, DelegationRunStatus::Running);
            }
            other => panic!("expected success on running reread, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn promote_commit_ambiguity_reread_terminal_winner() {
        let (_db, store, _, _) =
            seed_reserving_promote("amb-term-4111-8111-111111111111").await;
        store
            .promote_running("amb-term-4111-8111-111111111111", "conn-amb-t", Utc::now())
            .await
            .unwrap();
        store
            .settle_terminal(
                "amb-term-4111-8111-111111111111",
                TerminalTaskWrite::failed("x", Utc::now(), ConversationStatus::Cancelled),
            )
            .await
            .unwrap();
        store
            .push_promote_faults([PromoteTestFault::AmbiguousPermanent {
                message: "simulated commit I/O".into(),
            }])
            .await;
        let outcome = store
            .promote_running_detailed(
                "amb-term-4111-8111-111111111111",
                "conn-amb-t",
                Utc::now(),
            )
            .await
            .unwrap();
        match outcome.kind {
            PromoteRunningKind::TerminalWinner { .. } => {}
            other => panic!("expected TerminalWinner, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn promote_commit_ambiguity_reread_still_reserving_is_permanent() {
        let (_db, store, _, _) =
            seed_reserving_promote("amb-res-4111-8111-111111111111").await;
        store
            .push_promote_faults([PromoteTestFault::AmbiguousPermanent {
                message: "simulated commit I/O while reserving".into(),
            }])
            .await;
        let outcome = store
            .promote_running_detailed("amb-res-4111-8111-111111111111", "conn-amb-r", Utc::now())
            .await
            .unwrap();
        match outcome.kind {
            PromoteRunningKind::Permanent { message } => {
                assert!(
                    message.contains("simulated commit I/O") || message.contains("still reserving"),
                    "{message}"
                );
            }
            other => panic!("expected Permanent, got {other:?}"),
        }
        let run = store
            .load_by_task_id("amb-res-4111-8111-111111111111")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(run.run_status, DelegationRunStatus::Reserving);
    }

    #[tokio::test]
    async fn promote_commit_ambiguity_reread_mismatched_is_conflict() {
        let (_db, store, _, _) =
            seed_reserving_promote("amb-mis-4111-8111-111111111111").await;
        store
            .promote_running("amb-mis-4111-8111-111111111111", "conn-a", Utc::now())
            .await
            .unwrap();
        store
            .push_promote_faults([PromoteTestFault::AmbiguousPermanent {
                message: "simulated commit I/O".into(),
            }])
            .await;
        let outcome = store
            .promote_running_detailed("amb-mis-4111-8111-111111111111", "conn-b", Utc::now())
            .await
            .unwrap();
        match outcome.kind {
            PromoteRunningKind::StateConflict {
                class: PromoteConflictClass::Ownership,
                ..
            } => {}
            other => panic!("expected Ownership conflict, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn promote_success_meta_reports_per_class_retry_counts() {
        let (_db, store, _, _) =
            seed_reserving_promote("meta-mix-4111-8111-111111111111").await;
        store
            .push_promote_faults([
                PromoteTestFault::AfterClaimTransient(SqliteTransientClass::Busy),
                PromoteTestFault::AfterClaimTransient(SqliteTransientClass::Locked),
            ])
            .await;
        let outcome = store
            .promote_running_detailed("meta-mix-4111-8111-111111111111", "conn-mix", Utc::now())
            .await
            .unwrap();
        assert_promoted(&outcome.kind);
        assert_eq!(outcome.meta.attempts, 3);
        assert_eq!(outcome.meta.busy_retries, 1);
        assert_eq!(outcome.meta.locked_retries, 1);
        assert_eq!(outcome.meta.busy_snapshot_retries, 0);
    }

    #[tokio::test]
    async fn promote_reached_running_at_ge_started_at() {
        let (_db, store, _, _) =
            seed_reserving_promote("ts-order-4111-8111-111111111111").await;
        // prompt_accepted_at in the future forces promote_at clamp.
        let accepted = Utc::now() + chrono::Duration::seconds(30);
        let outcome = store
            .promote_running_detailed("ts-order-4111-8111-111111111111", "conn-ts", accepted)
            .await
            .unwrap();
        let run = assert_promoted(&outcome.kind);
        let started = run.started_at.expect("started_at");
        let reached = run.reached_running_at.expect("reached_running_at");
        assert_eq!(started, accepted, "started_at = prompt_accepted_at");
        assert!(
            reached >= started,
            "reached_running_at ({reached}) >= started_at ({started})"
        );
    }

    #[tokio::test]
    async fn promote_running_compat_maps_budget_exhausted_to_err() {
        use crate::db::entities::delegation_lineage_budget;
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) =
            seed_parent_child(&db, "compat-be-4111-8111-111111111111").await;
        let store = RunStore::new(db.clone());
        let lineage = "lineage-compat-be";
        store
            .insert_reserving(sample_insert_with(
                "uc-cb-3",
                parent_id,
                child_id,
                2,
                Some(lineage),
                AdmissionClass::UnexpectedContinue,
                lineage,
                Some("unit-cb"),
            ))
            .await
            .unwrap();
        let row = delegation_lineage_budget::Entity::find_by_id(lineage)
            .one(&db.conn)
            .await
            .unwrap()
            .expect("budget row");
        let mut active = row.into_active_model();
        active.unexpected_continue_count = Set(UNEXPECTED_CONTINUE_LIMIT);
        active.update(&db.conn).await.unwrap();

        let err = store
            .promote_running("uc-cb-3", "conn-cb3", Utc::now())
            .await
            .expect_err("compat must map BudgetExhausted to Err");
        assert!(err.is_budget_exhausted(), "got {err:?}");
    }

    // ---- Task 2: Projection tests ----------------------------------------

    async fn seed_reserving_promote_project(
        task_id: &str,
    ) -> (
        Arc<AppDatabase>,
        RunStore,
        i32, // parent_conversation_id
        i32, // child_conversation_id
        i64, // generation
    ) {
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        let db = Arc::new(fresh_in_memory_db().await);
        let store = RunStore::new(db.clone());
        let (parent_id, child_id) = seed_parent_child(&db, task_id).await;
        let generation: i64 = 1;
        // Minimal reserving seed (same as Task 1 promote helpers). Full gen-1
        // admit/bind is not required for projection compile-unblock tests.
        store
            .insert_reserving(sample_insert(
                task_id,
                parent_id,
                child_id,
                generation,
                None,
            ))
            .await
            .expect("insert reserving");

        // Pre-set a terminal overlay + stale generation rollups on the child
        // conversation so projection clearing can be observed.
        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        let mut active = child.into_active_model();
        active.delegation_finished_at = Set(Some(Utc::now()));
        active.delegation_error_code = Set(Some("prior-error".to_string()));
        active.delegation_tool_call_count = Set(Some(7));
        active.delegation_edit_tool_call_count = Set(Some(3));
        active.delegation_touched_files_json =
            Set(Some(r#"[{"path":"stale.rs"}]"#.to_string()));
        active.delegation_touched_files_truncated = Set(Some(true));
        active.delegation_additions = Set(Some(11));
        active.delegation_deletions = Set(Some(5));
        active.delegation_line_counts_complete = Set(Some(true));
        active.update(&db.conn).await.unwrap();

        (db, store, parent_id, child_id, generation)
    }

    #[tokio::test]
    async fn promote_projects_running_generation_and_started_at() {
        let (db, store, _, child_id, generation) =
            seed_reserving_promote_project("p3-prj-gen1-4111-8111-111111111111").await;
        let accepted_at = Utc::now();

        let outcome = store
            .promote_running_detailed(
                "p3-prj-gen1-4111-8111-111111111111",
                "conn-project",
                accepted_at,
            )
            .await
            .unwrap();
        assert_promoted(&outcome.kind);

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_run_generation, Some(generation));
        assert_eq!(
            child.delegation_task_status,
            Some(DelegationTaskStatus::Running)
        );
        assert_eq!(child.status, ConversationStatus::InProgress);
        assert_eq!(child.delegation_started_at, Some(accepted_at));
    }

    #[tokio::test]
    async fn promote_clears_prior_terminal_finished_at_and_error_code() {
        let (db, store, _, child_id, _) =
            seed_reserving_promote_project("p3-clr-4111-8111-111111111111").await;

        // Confirm prior terminal fields exist before promote.
        let child_before = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert!(child_before.delegation_finished_at.is_some());
        assert_eq!(
            child_before.delegation_error_code.as_deref(),
            Some("prior-error")
        );

        let outcome = store
            .promote_running_detailed(
                "p3-clr-4111-8111-111111111111",
                "conn-project",
                Utc::now(),
            )
            .await
            .unwrap();
        assert_promoted(&outcome.kind);

        let child_after = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert!(
            child_after.delegation_finished_at.is_none(),
            "finished_at must be cleared to NULL"
        );
        assert!(
            child_after.delegation_error_code.is_none(),
            "error_code must be cleared to NULL"
        );
    }

    #[tokio::test]
    async fn promote_resets_generation_rollups() {
        let (db, store, _, child_id, _) =
            seed_reserving_promote_project("p3-rst-4111-8111-111111111111").await;

        // Prove rollups were non-null before promote so the clear is observable.
        let before = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.delegation_tool_call_count, Some(7));
        assert_eq!(before.delegation_edit_tool_call_count, Some(3));
        assert!(before.delegation_touched_files_json.is_some());
        assert_eq!(before.delegation_touched_files_truncated, Some(true));
        assert_eq!(before.delegation_additions, Some(11));
        assert_eq!(before.delegation_deletions, Some(5));
        assert_eq!(before.delegation_line_counts_complete, Some(true));

        let outcome = store
            .promote_running_detailed(
                "p3-rst-4111-8111-111111111111",
                "conn-project",
                Utc::now(),
            )
            .await
            .unwrap();
        assert_promoted(&outcome.kind);

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_tool_call_count, None);
        assert_eq!(child.delegation_edit_tool_call_count, None);
        assert_eq!(child.delegation_touched_files_json, None);
        assert_eq!(child.delegation_touched_files_truncated, None);
        assert_eq!(child.delegation_additions, None);
        assert_eq!(child.delegation_deletions, None);
        assert_eq!(child.delegation_line_counts_complete, None);
    }

    #[tokio::test]
    async fn promote_gen2_overwrites_projection_gen1_fence_rolls_back() {
        use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};

        // --- Part A: gen-2 overwrites gen-1 projection; delayed gen-1 project rejects ---
        let (db, store, _, child_id, _gen1) =
            seed_reserving_promote_project("p3-fen-m-4111-8111-m111111m11").await;
        let accepted_at = Utc::now();

        let outcome = store
            .promote_running_detailed(
                "p3-fen-m-4111-8111-m111111m11",
                "conn-project",
                accepted_at,
            )
            .await
            .unwrap();
        assert_promoted(&outcome.kind);

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child.delegation_run_generation, Some(1));

        // Now project a gen-2 claim (simulating gen-2 overwrite). The gen-1
        // projection fence should be overwritten by gen-2.
        let gen2_proj = ConversationProjection {
            generation: 2,
            task_status: Some(DelegationTaskStatus::Running),
            error_code: Some(None),
            finished_at: Some(None),
            conversation_status: Some(ConversationStatus::InProgress),
            started_at: Some(Utc::now()),
            tool_call_count: None,
            edit_tool_call_count: None,
            touched_files_json: None,
            touched_files_truncated: None,
            additions: None,
            deletions: None,
            line_counts_complete: None,
            reset_generation_rollups: false,
        };
        let ok = store.project_conversation(child_id, gen2_proj).await.unwrap();
        assert!(ok);

        let child2 = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child2.delegation_run_generation, Some(2));

        // A delayed gen-1 projection must be rejected.
        let gen1_proj = ConversationProjection {
            generation: 1,
            task_status: Some(DelegationTaskStatus::Failed),
            error_code: Some(Some("stale".into())),
            finished_at: Some(Some(Utc::now())),
            conversation_status: Some(ConversationStatus::Cancelled),
            started_at: None,
            tool_call_count: None,
            edit_tool_call_count: None,
            touched_files_json: None,
            touched_files_truncated: None,
            additions: None,
            deletions: None,
            line_counts_complete: None,
            reset_generation_rollups: false,
        };
        let rejected = store
            .project_conversation(child_id, gen1_proj)
            .await
            .unwrap();
        assert!(
            !rejected,
            "gen-1 projection must be rejected after gen-2 fence"
        );

        // --- Part B: promote of gen-1 against a newer conversation fence rolls
        // back the entire promote txn as a typed StateConflict (no running
        // write, no started_at/reached_running_at, conversation gen stays 2). ---
        let task_id = "p3-fen-rb-4111-8111-m111111rb1";
        let (db_b, store_b, _, child_b, _) = seed_reserving_promote_project(task_id).await;
        let child_row = conversation::Entity::find_by_id(child_b)
            .one(&db_b.conn)
            .await
            .unwrap()
            .unwrap();
        let mut active = child_row.into_active_model();
        active.delegation_run_generation = Set(Some(2));
        // Leave prior-error / finished_at so a partial projection would be visible.
        active.update(&db_b.conn).await.unwrap();

        let run_before = store_b
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap();
        let provisional_started = run_before.started_at;
        let accepted_stale = provisional_started
            .map(|t| t + chrono::Duration::seconds(30))
            .unwrap_or_else(Utc::now);

        let fence_outcome = store_b
            .promote_running_detailed(task_id, "conn-fence", accepted_stale)
            .await
            .unwrap();
        match fence_outcome.kind {
            PromoteRunningKind::StateConflict {
                class: PromoteConflictClass::Status,
                message,
            } => {
                assert!(
                    message.contains("generation fence"),
                    "expected fence message, got {message}"
                );
            }
            other => panic!("expected StateConflict Status for fence miss, got {other:?}"),
        }

        let run = store_b
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            run.run_status,
            DelegationRunStatus::Reserving,
            "fence miss must roll back run promote write"
        );
        assert!(
            run.reached_running_at.is_none(),
            "fence miss must not set reached_running_at"
        );
        assert_eq!(
            run.started_at, provisional_started,
            "fence miss must roll back promote started_at overwrite"
        );
        let child_after = conversation::Entity::find_by_id(child_b)
            .one(&db_b.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            child_after.delegation_run_generation,
            Some(2),
            "stale gen-1 promote must not lower the generation fence"
        );
        // Prior terminal overlay must remain (projection rolled back with txn).
        assert!(
            child_after.delegation_finished_at.is_some(),
            "rolled-back promote must not clear finished_at"
        );
        assert_eq!(
            child_after.delegation_error_code.as_deref(),
            Some("prior-error"),
            "rolled-back promote must not clear error_code"
        );
    }

    #[tokio::test]
    async fn promote_equal_generation_reproject_succeeds() {
        let (db, store, _, child_id, _) =
            seed_reserving_promote_project("p3-eq-4111-8111-111111111111").await;
        let accepted_at = Utc::now();

        let outcome = store
            .promote_running_detailed(
                "p3-eq-4111-8111-111111111111",
                "conn-project",
                accepted_at,
            )
            .await
            .unwrap();
        assert_promoted(&outcome.kind);

        // Equal generation re-projection must succeed (idempotent).
        let same_gen_proj = ConversationProjection {
            generation: 1,
            task_status: Some(DelegationTaskStatus::Completed),
            error_code: Some(None),
            finished_at: Some(Some(Utc::now())),
            conversation_status: Some(ConversationStatus::PendingReview),
            started_at: None,
            tool_call_count: None,
            edit_tool_call_count: None,
            touched_files_json: None,
            touched_files_truncated: None,
            additions: None,
            deletions: None,
            line_counts_complete: None,
            reset_generation_rollups: false,
        };
        let ok = store
            .project_conversation(child_id, same_gen_proj)
            .await
            .unwrap();
        assert!(ok, "equal-generation re-project must succeed");

        let child = conversation::Entity::find_by_id(child_id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            child.delegation_task_status,
            Some(DelegationTaskStatus::Completed)
        );
    }
}
