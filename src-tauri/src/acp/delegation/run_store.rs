//! Authoritative store for `delegation_task_runs`.
//!
//! Conversation columns remain a latest-run **projection** only. Settlement and
//! runtime updates write the run row plus a monotonic
//! `conversation.delegation_run_generation` fence in one transaction.
//!
//! Also owns server-side `task_preview` derivation and `request_fingerprint`
//! canonicalization used by both `delegate_to_agent` and `continue_delegation`.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use regex::Regex;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::acp::delegation::runtime_stats::{
    decode_persisted_runtime_stats, DelegationRuntimeStats, PersistedRuntimeStatsColumns,
};
use crate::acp::delegation::store::{
    is_transient_sqlite, PersistedTask, Settlement, TaskStoreError, TerminalTaskWrite,
};
use crate::acp::delegation::types::TaskStatus;
use crate::db::entities::conversation::{self, ConversationStatus, DelegationTaskStatus};
use crate::db::entities::delegation_task_run::{
    self, AdmissionClass, DelegationRunStatus, Entity as DelegationTaskRun,
};
use crate::db::AppDatabase;
use crate::models::AgentType;

/// Maximum Unicode scalars retained in a durable `task_preview` after redaction.
pub const TASK_PREVIEW_SCALAR_CAP: usize = 200;

/// Field separator for request-fingerprint canonicalization (ASCII RS).
const FP_FIELD_SEP: char = '\u{1e}';

fn is_valid_task_id_prefix(prefix: &str) -> bool {
    prefix.len() == 8 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
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
/// Returns lowercase hex SHA-256 of the joined fields.
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
    let mut canonical = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            canonical.push(FP_FIELD_SEP);
        }
        canonical.push_str(f);
    }
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
#[derive(Debug, Clone)]
pub struct ConversationProjection {
    pub generation: i64,
    pub task_status: Option<DelegationTaskStatus>,
    pub error_code: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
    pub conversation_status: Option<ConversationStatus>,
    pub started_at: Option<DateTime<Utc>>,
    /// Optional runtime rollup fields projected onto conversation columns.
    pub tool_call_count: Option<i64>,
    pub edit_tool_call_count: Option<i64>,
    pub touched_files_json: Option<String>,
    pub touched_files_truncated: Option<bool>,
    pub additions: Option<i64>,
    pub deletions: Option<i64>,
    pub line_counts_complete: Option<bool>,
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
    pub runtime_stats: Option<DelegationRuntimeStats>,
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

fn map_db_err(err: sea_orm::DbErr) -> TaskStoreError {
    let msg = err.to_string();
    if is_transient_sqlite(&msg) {
        TaskStoreError::Transient(msg)
    } else {
        TaskStoreError::Permanent(msg)
    }
}

fn parse_agent_type(s: &str) -> AgentType {
    match serde_json::from_value(serde_json::Value::String(s.to_string())) {
        Ok(at) => at,
        Err(_) => AgentType::ClaudeCode,
    }
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
        runtime_stats,
    })
}

/// SQLite-backed store for `delegation_task_runs` + conversation projection fence.
pub struct RunStore {
    db: Arc<AppDatabase>,
}

impl RunStore {
    pub fn new(db: Arc<AppDatabase>) -> Self {
        Self { db }
    }

    pub fn db(&self) -> &Arc<AppDatabase> {
        &self.db
    }

    /// Insert a durable `reserving` claim before ACP spawn / resume.
    pub async fn insert_reserving(&self, insert: ReservingRunInsert) -> Result<(), TaskStoreError> {
        let now = Utc::now();
        let model = delegation_task_run::ActiveModel {
            task_id: Set(insert.task_id),
            root_task_id: Set(insert.root_task_id),
            previous_task_id: Set(insert.previous_task_id),
            generation: Set(insert.generation),
            parent_conversation_id: Set(insert.parent_conversation_id),
            parent_tool_use_id: Set(insert.parent_tool_use_id),
            child_conversation_id: Set(insert.child_conversation_id),
            agent_type: Set(insert.agent_type),
            profile_id: Set(insert.profile_id),
            workspace_path: Set(insert.workspace_path),
            route_fingerprint: Set(insert.route_fingerprint),
            launch_snapshot_version: Set(insert.launch_snapshot_version),
            mode_id: Set(insert.mode_id),
            config_values_json: Set(insert.config_values_json),
            task_preview: Set(insert.task_preview),
            request_fingerprint: Set(insert.request_fingerprint),
            admission_class: Set(insert.admission_class),
            reached_running_at: Set(None),
            lineage_root_task_id: Set(insert.lineage_root_task_id),
            work_unit_key: Set(insert.work_unit_key),
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
            replaced_task_id: Set(insert.replaced_task_id),
            replacement_reason: Set(insert.replacement_reason),
            created_at: Set(now),
            updated_at: Set(now),
        };
        model.insert(&self.db.conn).await.map_err(map_db_err)?;
        Ok(())
    }

    /// Transition `reserving` → `running` after successful prompt admission.
    /// Budget charging is Task 3 and hooks here later.
    pub async fn promote_running(
        &self,
        task_id: &str,
        child_connection_id: impl Into<String>,
        at: DateTime<Utc>,
    ) -> Result<(), TaskStoreError> {
        let child_connection_id = child_connection_id.into();
        let result = DelegationTaskRun::update_many()
            .col_expr(
                delegation_task_run::Column::Status,
                sea_orm::sea_query::Expr::value(DelegationRunStatus::Running),
            )
            .col_expr(
                delegation_task_run::Column::ReachedRunningAt,
                sea_orm::sea_query::Expr::value(at),
            )
            .col_expr(
                delegation_task_run::Column::ChildConnectionId,
                sea_orm::sea_query::Expr::value(child_connection_id),
            )
            .col_expr(
                delegation_task_run::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(at),
            )
            .filter(delegation_task_run::Column::TaskId.eq(task_id))
            .filter(delegation_task_run::Column::Status.eq(DelegationRunStatus::Reserving))
            .exec(&self.db.conn)
            .await
            .map_err(map_db_err)?;
        if result.rows_affected == 0 {
            return Err(TaskStoreError::Permanent(format!(
                "promote_running CAS missed for task {task_id}"
            )));
        }
        Ok(())
    }

    /// Conditional terminal settle on the run row + monotonic conversation projection.
    pub async fn settle_terminal(
        &self,
        task_id: &str,
        terminal: TerminalTaskWrite,
    ) -> Result<Settlement, TaskStoreError> {
        let run_status = task_status_to_run_status(terminal.status)?;
        let proj_status = task_status_to_delegation_task_status(terminal.status)?;
        let finished_at = terminal.finished_at;
        let error_code = terminal.error_code.clone();
        let conversation_status = terminal.conversation_status.clone();

        let outcome = self
            .db
            .conn
            .transaction::<_, Settlement, TaskStoreError>(|txn| {
                let task_id = task_id.to_string();
                let error_code = error_code.clone();
                let conversation_status = conversation_status.clone();
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
                            return Ok(Settlement::Existing(persisted.to_persisted_task().to_report(None)));
                        }
                        DelegationRunStatus::Reserving | DelegationRunStatus::Running => {}
                    }

                    let generation = row.generation;
                    let child_id = row.child_conversation_id;
                    let now = Utc::now();

                    let result = DelegationTaskRun::update_many()
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
                        )
                        .filter(delegation_task_run::Column::TaskId.eq(&task_id))
                        .filter(
                            delegation_task_run::Column::Status
                                .is_in([DelegationRunStatus::Reserving, DelegationRunStatus::Running]),
                        )
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
                            TaskStoreError::Permanent(format!("run {task_id} unreadable after CAS miss"))
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

                    project_conversation_in_txn(
                        txn,
                        child_id,
                        ConversationProjection {
                            generation,
                            task_status: Some(proj_status),
                            error_code: error_code.clone(),
                            finished_at: Some(finished_at),
                            conversation_status: Some(conversation_status),
                            started_at: None,
                            tool_call_count: None,
                            edit_tool_call_count: None,
                            touched_files_json: None,
                            touched_files_truncated: None,
                            additions: None,
                            deletions: None,
                            line_counts_complete: None,
                        },
                    )
                    .await?;

                    let won = DelegationTaskRun::find_by_id(&task_id)
                        .one(txn)
                        .await
                        .map_err(map_db_err)?
                        .ok_or_else(|| TaskStoreError::NotFound(task_id.clone()))?;
                    let persisted = model_to_persisted_run(won).ok_or_else(|| {
                        TaskStoreError::Permanent(format!("settled run {task_id} unreadable"))
                    })?;
                    Ok(Settlement::Won(persisted.to_persisted_task().to_report(None)))
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
        let updated = project_conversation_in_txn(&txn, child_conversation_id, projection).await?;
        txn.commit().await.map_err(map_db_err)?;
        Ok(updated)
    }

    /// Settle every non-terminal run (`reserving` / `running`) as
    /// `failed`/`host_restarted` and project each conversation monotonically.
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
            let write = TerminalTaskWrite::failed(
                "host_restarted",
                at,
                ConversationStatus::Cancelled,
            );
            match self.settle_terminal(&row.task_id, write).await {
                Ok(Settlement::Won(_)) => n += 1,
                Ok(Settlement::Existing(_)) => {}
                Err(TaskStoreError::NotFound(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(n)
    }

    /// Write runtime stats onto a run; when `finished_at` is set, only the
    /// matching terminal row accepts the write. Running snapshots require
    /// `running` status.
    pub async fn write_runtime_stats(
        &self,
        task_id: &str,
        stats: &DelegationRuntimeStats,
    ) -> Result<(), TaskStoreError> {
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

        let mut update = DelegationTaskRun::update_many()
            .col_expr(
                delegation_task_run::Column::ToolCallCount,
                sea_orm::sea_query::Expr::value(tool_call_count),
            )
            .col_expr(
                delegation_task_run::Column::EditToolCallCount,
                sea_orm::sea_query::Expr::value(edit_tool_call_count),
            )
            .col_expr(
                delegation_task_run::Column::TouchedFilesJson,
                sea_orm::sea_query::Expr::value(touched_files_json),
            )
            .col_expr(
                delegation_task_run::Column::TouchedFilesTruncated,
                sea_orm::sea_query::Expr::value(stats.touched_files_truncated),
            )
            .col_expr(
                delegation_task_run::Column::Additions,
                sea_orm::sea_query::Expr::value(additions),
            )
            .col_expr(
                delegation_task_run::Column::Deletions,
                sea_orm::sea_query::Expr::value(deletions),
            )
            .col_expr(
                delegation_task_run::Column::LineCountsComplete,
                sea_orm::sea_query::Expr::value(stats.line_counts_complete),
            )
            .col_expr(
                delegation_task_run::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(delegation_task_run::Column::TaskId.eq(task_id));

        if stats.finished_at.is_none() {
            update = update
                .filter(delegation_task_run::Column::Status.eq(DelegationRunStatus::Running));
        } else {
            update = update
                .filter(
                    delegation_task_run::Column::Status
                        .is_in([
                            DelegationRunStatus::Completed,
                            DelegationRunStatus::Failed,
                            DelegationRunStatus::Canceled,
                        ]),
                )
                .filter(delegation_task_run::Column::FinishedAt.eq(stats.finished_at));
        }

        let result = update.exec(&self.db.conn).await.map_err(map_db_err)?;
        if result.rows_affected > 0 {
            return Ok(());
        }

        if stats.finished_at.is_none() {
            match self.load_by_task_id(task_id).await? {
                Some(row)
                    if !matches!(
                        row.run_status,
                        DelegationRunStatus::Running | DelegationRunStatus::Reserving
                    ) =>
                {
                    Ok(())
                }
                Some(_) => Err(TaskStoreError::Permanent(format!(
                    "running runtime_stats write matched no rows for still-running task {task_id}"
                ))),
                None => Err(TaskStoreError::Permanent(format!(
                    "running runtime_stats write matched no rows; task {task_id} missing"
                ))),
            }
        } else {
            Err(TaskStoreError::Permanent(format!(
                "terminal runtime_stats write matched no rows for task {task_id}"
            )))
        }
    }
}

async fn project_conversation_in_txn(
    txn: &DatabaseTransaction,
    child_conversation_id: i32,
    projection: ConversationProjection,
) -> Result<bool, TaskStoreError> {
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
    if projection.error_code.is_some() || projection.task_status.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationErrorCode,
            sea_orm::sea_query::Expr::value(projection.error_code.clone()),
        );
    }
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
    if projection.additions.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationAdditions,
            sea_orm::sea_query::Expr::value(projection.additions),
        );
    }
    if projection.deletions.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationDeletions,
            sea_orm::sea_query::Expr::value(projection.deletions),
        );
    }
    if projection.line_counts_complete.is_some() {
        update = update.col_expr(
            conversation::Column::DelegationLineCountsComplete,
            sea_orm::sea_query::Expr::value(projection.line_counts_complete),
        );
    }

    let result = update.exec(txn).await.map_err(map_db_err)?;
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
        let s: String = std::iter::repeat('😀').take(201).collect();
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

    // ---- RunStore -----------------------------------------------------------

    async fn seed_parent_child(
        db: &AppDatabase,
        call_id: &str,
    ) -> (i32, i32) {
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
            root_task_id: previous.map(|_| "root-task".into()).unwrap_or_else(|| task_id.into()),
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

    #[tokio::test]
    async fn insert_promote_settle_round_trip() {
        let db = Arc::new(fresh_in_memory_db().await);
        let (parent_id, child_id) = seed_parent_child(&db, "aaaaaaaa-1111-4111-8111-111111111111").await;
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
        let (parent_id, child_id) = seed_parent_child(&db, "bbbbbbbb-2222-4222-8222-222222222222").await;
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
                TerminalTaskWrite::canceled("usercancel", Utc::now(), ConversationStatus::Cancelled),
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
        let (_parent_id, child_id) = seed_parent_child(&db, "cccccccc-3333-4333-8333-333333333333").await;
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
                    error_code: Some("stale".into()),
                    finished_at: Some(Utc::now()),
                    conversation_status: Some(ConversationStatus::Cancelled),
                    started_at: None,
                    tool_call_count: None,
                    edit_tool_call_count: None,
                    touched_files_json: None,
                    touched_files_truncated: None,
                    additions: None,
                    deletions: None,
                    line_counts_complete: None,
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
                    finished_at: Some(Utc::now()),
                    conversation_status: Some(ConversationStatus::PendingReview),
                    started_at: None,
                    tool_call_count: None,
                    edit_tool_call_count: None,
                    touched_files_json: None,
                    touched_files_truncated: None,
                    additions: None,
                    deletions: None,
                    line_counts_complete: None,
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
            .insert_reserving(sample_insert(cont, parent_a.id, child_a.id, 2, Some(root_a)))
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
        store
            .promote_running(root, "c1", Utc::now())
            .await
            .unwrap();
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
        store
            .promote_running(cont, "c2", Utc::now())
            .await
            .unwrap();

        let cont_row = store.load_by_task_id(cont).await.unwrap().unwrap();
        assert_eq!(cont_row.generation, 2);
        assert_eq!(cont_row.previous_task_id.as_deref(), Some(root));
        assert_eq!(cont_row.child_conversation_id, child_id);
        assert_eq!(cont_row.run_status, DelegationRunStatus::Running);

        // Root remains independently loadable and terminal.
        let root_row = store.load_by_task_id(root).await.unwrap().unwrap();
        assert_eq!(root_row.status, TaskStatus::Completed);
    }
}
