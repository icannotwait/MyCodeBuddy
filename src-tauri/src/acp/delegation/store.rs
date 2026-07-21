//! Durable accepted/terminal state for Codeg delegation tasks.
//!
//! Authoritative lifecycle identity is `delegation_task_runs.task_id`.
//! Conversation columns (`delegation_task_status` / `delegation_error_code` /
//! timestamps / runtime rollups / `delegation_run_generation`) remain a
//! latest-run **projection** only. When a run row is present, load/settle/
//! prefix-recovery/reconcile go through [`crate::acp::delegation::run_store::RunStore`].
//! Conversation-keyed paths remain as a temporary fallback for gen-1 rows that
//! predate live run inserts (Task 4).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
use tokio::sync::Mutex;

use crate::acp::delegation::run_store::RunStore;
use crate::acp::delegation::runtime_stats::{
    decode_persisted_runtime_stats, DelegationRuntimeStats, PersistedRuntimeStatsColumns,
};
use crate::acp::delegation::types::{DelegationTaskReport, TaskStatus};
use crate::db::entities::conversation::{self, ConversationStatus, DelegationTaskStatus};
use crate::db::AppDatabase;
use crate::models::AgentType;

fn is_valid_task_id_prefix(prefix: &str) -> bool {
    prefix.len() == 8 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// One attempted durable terminal write (CAS payload).
#[derive(Debug, Clone)]
pub struct TerminalTaskWrite {
    pub status: TaskStatus,
    pub error_code: Option<String>,
    pub finished_at: DateTime<Utc>,
    pub conversation_status: ConversationStatus,
    /// Optional final runtime snapshot written **inside** the settlement
    /// transaction. After that commit the run is frozen; post-terminal
    /// `write_runtime_stats` remains a no-op for run-backed rows.
    pub runtime_stats: Option<DelegationRuntimeStats>,
}

impl TerminalTaskWrite {
    pub fn completed(finished_at: DateTime<Utc>, conversation_status: ConversationStatus) -> Self {
        Self {
            status: TaskStatus::Completed,
            error_code: None,
            finished_at,
            conversation_status,
            runtime_stats: None,
        }
    }

    pub fn failed(
        error_code: impl Into<String>,
        finished_at: DateTime<Utc>,
        conversation_status: ConversationStatus,
    ) -> Self {
        Self {
            status: TaskStatus::Failed,
            error_code: Some(error_code.into()),
            finished_at,
            conversation_status,
            runtime_stats: None,
        }
    }

    pub fn canceled(
        error_code: impl Into<String>,
        finished_at: DateTime<Utc>,
        conversation_status: ConversationStatus,
    ) -> Self {
        Self {
            status: TaskStatus::Canceled,
            error_code: Some(error_code.into()),
            finished_at,
            conversation_status,
            runtime_stats: None,
        }
    }

    pub fn with_runtime_stats(mut self, stats: DelegationRuntimeStats) -> Self {
        self.runtime_stats = Some(stats);
        self
    }

    fn to_persisted_status(&self) -> Result<DelegationTaskStatus, TaskStoreError> {
        match self.status {
            TaskStatus::Completed => Ok(DelegationTaskStatus::Completed),
            TaskStatus::Failed => Ok(DelegationTaskStatus::Failed),
            TaskStatus::Canceled => Ok(DelegationTaskStatus::Canceled),
            TaskStatus::Running | TaskStatus::Unknown => Err(TaskStoreError::Permanent(
                "terminal write must not use running/unknown status".into(),
            )),
        }
    }
}

/// Durable snapshot of a delegation task row.
#[derive(Debug, Clone)]
pub struct PersistedTask {
    pub task_id: String,
    pub child_conversation_id: i32,
    pub parent_id: Option<i32>,
    pub agent_type: AgentType,
    pub status: TaskStatus,
    pub error_code: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub runtime_stats: Option<DelegationRuntimeStats>,
}

impl PersistedTask {
    pub fn to_report(&self, result_text: Option<String>) -> DelegationTaskReport {
        let message = match self.status {
            TaskStatus::Running => Some("Running.".to_string()),
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Canceled => Some(format!(
                "Result no longer cached; open child session {} for the full output.",
                self.child_conversation_id
            )),
            TaskStatus::Unknown => Some(
                "Unknown task id — it never existed, isn't owned by this session, \
                 or its result was evicted with no stored record."
                    .to_string(),
            ),
        };
        DelegationTaskReport {
            task_id: Some(self.task_id.clone()),
            status: self.status,
            child_conversation_id: Some(self.child_conversation_id),
            agent_type: Some(self.agent_type),
            text: result_text,
            error_code: self.error_code.clone(),
            message,
            duration_ms: None,
            observation: None,
            last_agent_activity_at: None,
            stalled_since: None,
        }
    }
}

/// Result of a conditional terminal settle.
#[derive(Debug, Clone)]
pub enum Settlement {
    Won(DelegationTaskReport),
    Existing(DelegationTaskReport),
}

impl Settlement {
    pub fn report(&self) -> &DelegationTaskReport {
        match self {
            Settlement::Won(r) | Settlement::Existing(r) => r,
        }
    }

    pub fn into_report(self) -> DelegationTaskReport {
        match self {
            Settlement::Won(r) | Settlement::Existing(r) => r,
        }
    }

    pub fn won(&self) -> bool {
        matches!(self, Settlement::Won(_))
    }
}

/// Process-local record for a terminal write that failed after retries.
#[derive(Debug, Clone)]
pub struct PendingTerminalRetry {
    pub task_id: String,
    pub terminal: TerminalTaskWrite,
    pub child_conversation_id: i32,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum TaskStoreError {
    #[error("transient database error: {0}")]
    Transient(String),
    #[error("permanent database error: {0}")]
    Permanent(String),
    #[error("task not found: {0}")]
    NotFound(String),
    /// Platform recovery rail refused the operation (unexpected-continue,
    /// replacement, generation ceiling, or dual-row stricter-wins).
    /// Wire code: `budget_exhausted`.
    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),
    /// Concurrent insert lost a partial unique fence (non-terminal gen-1
    /// work unit, non-terminal child, etc.). Wire code: `busy_thread`.
    #[error("busy thread: {0}")]
    BusyThread(String),
    /// Same `(parent_conversation_id, parent_tool_use_id)` already bound with
    /// a different or missing request fingerprint. Wire code:
    /// `duplicate_parent_tool`.
    #[error("duplicate parent tool: {0}")]
    DuplicateParentTool(String),
    /// Replacement-qualified dual first-dispatch loser (orchestrated key
    /// with established lineage without replaces). Wire code:
    /// `invalid_replacement`.
    #[error("invalid replacement: {0}")]
    InvalidReplacement(String),
}

impl TaskStoreError {
    pub fn is_transient(&self) -> bool {
        matches!(self, TaskStoreError::Transient(_))
    }

    pub fn is_budget_exhausted(&self) -> bool {
        matches!(self, TaskStoreError::BudgetExhausted(_))
    }

    pub fn is_busy_thread(&self) -> bool {
        matches!(self, TaskStoreError::BusyThread(_))
    }

    pub fn is_duplicate_parent_tool(&self) -> bool {
        matches!(self, TaskStoreError::DuplicateParentTool(_))
    }

    /// Stable wire code for MCP / broker reports when this error is
    /// caller-facing. Returns `None` for internal permanent/transient cases.
    pub fn wire_code(&self) -> Option<&'static str> {
        match self {
            Self::BudgetExhausted(_) => Some("budget_exhausted"),
            Self::BusyThread(_) => Some("busy_thread"),
            Self::DuplicateParentTool(_) => Some("duplicate_parent_tool"),
            Self::InvalidReplacement(_) => Some("invalid_replacement"),
            Self::NotFound(_) => Some("not_found"),
            Self::Transient(_) | Self::Permanent(_) => None,
        }
    }
}

/// Retry policy for transient SQLite busy/locked errors.
#[derive(Debug, Clone)]
pub struct PersistenceRetryPolicy {
    /// Total settle attempts including the first try.
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl PersistenceRetryPolicy {
    pub fn new(max_attempts: u32, base_delay: Duration) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            base_delay,
            max_delay: Duration::from_secs(1),
        }
    }

    pub fn production() -> Self {
        // Initial try + three retries, capped exponential backoff.
        Self::new(4, Duration::from_millis(25))
    }

    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let factor = 1u32 << attempt.min(4);
        let d = self.base_delay.saturating_mul(factor);
        d.min(self.max_delay)
    }
}

impl Default for PersistenceRetryPolicy {
    fn default() -> Self {
        Self::production()
    }
}

#[async_trait]
pub trait DelegationTaskStore: Send + Sync {
    async fn load(&self, task_id: &str) -> Result<Option<PersistedTask>, TaskStoreError>;
    async fn resolve_unique_owned_prefix(
        &self,
        _parent_id: i32,
        _prefix: &str,
    ) -> Result<Option<String>, TaskStoreError> {
        Ok(None)
    }
    async fn settle(
        &self,
        task_id: &str,
        terminal: TerminalTaskWrite,
    ) -> Result<Settlement, TaskStoreError>;
    async fn reconcile_running(&self, at: DateTime<Utc>) -> Result<u64, TaskStoreError>;
    async fn write_runtime_stats(
        &self,
        task_id: &str,
        stats: &DelegationRuntimeStats,
    ) -> Result<(), TaskStoreError>;
    async fn put_retry(&self, retry: PendingTerminalRetry);
    async fn remove_retry(&self, task_id: &str);
    async fn has_retry_record(&self, task_id: &str) -> bool;
    /// Peek the process-local retry payload (first-wins record) without removing it.
    async fn get_retry(&self, task_id: &str) -> Option<PendingTerminalRetry>;
}

/// Default store for broker unit tests that do **not** exercise durability.
///
/// **Always returns `Settlement::Won`** with a synthetic report derived from
/// the write — never `Existing`, never a real row. Suitable only for race /
/// setup / routing unit tests that ignore store semantics. Durability,
/// CAS-loser, and cold-load tests must use [`mock::MockTaskStore`] or
/// [`DbDelegationTaskStore`].
#[derive(Default)]
pub struct NoopTaskStore {
    retries: Mutex<HashMap<String, PendingTerminalRetry>>,
}

#[async_trait]
impl DelegationTaskStore for NoopTaskStore {
    async fn load(&self, _task_id: &str) -> Result<Option<PersistedTask>, TaskStoreError> {
        Ok(None)
    }

    async fn settle(
        &self,
        task_id: &str,
        terminal: TerminalTaskWrite,
    ) -> Result<Settlement, TaskStoreError> {
        Ok(Settlement::Won(report_from_terminal(
            task_id, &terminal, None,
        )))
    }

    async fn reconcile_running(&self, _at: DateTime<Utc>) -> Result<u64, TaskStoreError> {
        Ok(0)
    }

    async fn write_runtime_stats(
        &self,
        _task_id: &str,
        _stats: &DelegationRuntimeStats,
    ) -> Result<(), TaskStoreError> {
        Ok(())
    }

    async fn put_retry(&self, retry: PendingTerminalRetry) {
        self.retries
            .lock()
            .await
            .entry(retry.task_id.clone())
            .or_insert(retry);
    }

    async fn remove_retry(&self, task_id: &str) {
        self.retries.lock().await.remove(task_id);
    }

    async fn has_retry_record(&self, task_id: &str) -> bool {
        self.retries.lock().await.contains_key(task_id)
    }

    async fn get_retry(&self, task_id: &str) -> Option<PendingTerminalRetry> {
        self.retries.lock().await.get(task_id).cloned()
    }
}

/// Production SQLite-backed store.
pub struct DbDelegationTaskStore {
    db: Arc<AppDatabase>,
    retries: Mutex<HashMap<String, PendingTerminalRetry>>,
}

impl DbDelegationTaskStore {
    pub fn new(db: Arc<AppDatabase>) -> Self {
        Self {
            db,
            retries: Mutex::new(HashMap::new()),
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

    fn model_to_persisted(row: conversation::Model) -> Option<PersistedTask> {
        let task_id = row.delegation_call_id?;
        let status = match row.delegation_task_status {
            Some(DelegationTaskStatus::Running) => TaskStatus::Running,
            Some(DelegationTaskStatus::Completed) => TaskStatus::Completed,
            Some(DelegationTaskStatus::Failed) => TaskStatus::Failed,
            Some(DelegationTaskStatus::Canceled) => TaskStatus::Canceled,
            None => return None,
        };
        let runtime_stats = match decode_persisted_runtime_stats(PersistedRuntimeStatsColumns {
            started_at: row.delegation_started_at,
            finished_at: row.delegation_finished_at,
            tool_call_count: row.delegation_tool_call_count,
            edit_tool_call_count: row.delegation_edit_tool_call_count,
            touched_files_json: row.delegation_touched_files_json.as_deref(),
            touched_files_truncated: row.delegation_touched_files_truncated,
            additions: row.delegation_additions,
            deletions: row.delegation_deletions,
            line_counts_complete: row.delegation_line_counts_complete,
        }) {
            Ok(stats) => stats,
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id,
                    error = ?err,
                    "[delegation_store] failed to decode runtime_stats"
                );
                None
            }
        };
        Some(PersistedTask {
            task_id,
            child_conversation_id: row.id,
            parent_id: row.parent_id,
            agent_type: parse_agent_type(&row.agent_type),
            status,
            error_code: row.delegation_error_code,
            started_at: row.delegation_started_at,
            finished_at: row.delegation_finished_at,
            runtime_stats,
        })
    }
}

fn parse_agent_type(s: &str) -> AgentType {
    match serde_json::from_value(serde_json::Value::String(s.to_string())) {
        Ok(at) => at,
        Err(_) => AgentType::ClaudeCode,
    }
}

pub fn is_transient_sqlite(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("database is locked")
        || lower.contains("database is busy")
        || lower.contains("sqlite_busy")
        || lower.contains("sqlite_locked")
        || lower.contains("code: 5")
        || lower.contains("code: 6")
}

fn report_from_terminal(
    task_id: &str,
    terminal: &TerminalTaskWrite,
    child_conversation_id: Option<i32>,
) -> DelegationTaskReport {
    DelegationTaskReport {
        task_id: Some(task_id.to_string()),
        status: terminal.status,
        child_conversation_id,
        agent_type: None,
        text: None,
        error_code: terminal.error_code.clone(),
        message: None,
        duration_ms: None,
        observation: None,
        last_agent_activity_at: None,
        stalled_since: None,
    }
}

#[async_trait]
impl DelegationTaskStore for DbDelegationTaskStore {
    async fn load(&self, task_id: &str) -> Result<Option<PersistedTask>, TaskStoreError> {
        // Prefer authoritative run row (supports continued runs whose task_id
        // differs from conversation.delegation_call_id).
        let runs = RunStore::new(self.db.clone());
        if let Some(run) = runs.load_by_task_id(task_id).await? {
            return Ok(Some(run.to_persisted_task()));
        }
        // Fallback: gen-1 conversation projection before live run inserts.
        let row = conversation::Entity::find()
            .filter(conversation::Column::DelegationCallId.eq(task_id))
            .one(&self.db.conn)
            .await
            .map_err(Self::map_db_err)?;
        Ok(row.and_then(Self::model_to_persisted))
    }

    async fn resolve_unique_owned_prefix(
        &self,
        parent_id: i32,
        prefix: &str,
    ) -> Result<Option<String>, TaskStoreError> {
        if !is_valid_task_id_prefix(prefix) {
            return Ok(None);
        }
        // Authoritative: parent-scoped run rows.
        let runs = RunStore::new(self.db.clone());
        if let Some(task_id) = runs.resolve_unique_owned_prefix(parent_id, prefix).await? {
            return Ok(Some(task_id));
        }
        // If any run rows match this parent+prefix, ambiguity/emptiness from
        // RunStore is final — do not mix with conversation call_ids.
        let run_hits = crate::db::entities::delegation_task_run::Entity::find()
            .filter(
                crate::db::entities::delegation_task_run::Column::ParentConversationId
                    .eq(parent_id),
            )
            .filter(crate::db::entities::delegation_task_run::Column::TaskId.starts_with(prefix))
            .limit(1)
            .all(&self.db.conn)
            .await
            .map_err(Self::map_db_err)?;
        if !run_hits.is_empty() {
            return Ok(None);
        }
        // Fallback for pre-run-insert gen-1 rows.
        let rows = conversation::Entity::find()
            .filter(conversation::Column::ParentId.eq(parent_id))
            .filter(conversation::Column::DelegationCallId.starts_with(prefix))
            .filter(conversation::Column::DeletedAt.is_null())
            .limit(2)
            .all(&self.db.conn)
            .await
            .map_err(Self::map_db_err)?;
        if rows.len() != 1 {
            return Ok(None);
        }
        Ok(rows
            .into_iter()
            .next()
            .and_then(|row| row.delegation_call_id))
    }

    async fn settle(
        &self,
        task_id: &str,
        terminal: TerminalTaskWrite,
    ) -> Result<Settlement, TaskStoreError> {
        let runs = RunStore::new(self.db.clone());
        if runs.load_by_task_id(task_id).await?.is_some() {
            return runs.settle_terminal(task_id, terminal).await;
        }

        // Legacy conversation-only CAS (gen-1 before live run inserts).
        let persisted_status = terminal.to_persisted_status()?;
        let mut update = conversation::Entity::update_many()
            .col_expr(
                conversation::Column::DelegationTaskStatus,
                sea_orm::sea_query::Expr::value(persisted_status),
            )
            .col_expr(
                conversation::Column::DelegationErrorCode,
                sea_orm::sea_query::Expr::value(terminal.error_code.clone()),
            )
            .col_expr(
                conversation::Column::DelegationFinishedAt,
                sea_orm::sea_query::Expr::value(terminal.finished_at),
            )
            .col_expr(
                conversation::Column::Status,
                sea_orm::sea_query::Expr::value(terminal.conversation_status.clone()),
            )
            .col_expr(
                conversation::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            );

        // Optional final runtime snapshot in the same CAS update.
        if let Some(ref stats) = terminal.runtime_stats {
            let tool_call_count = i64::try_from(stats.tool_call_count).map_err(|_| {
                TaskStoreError::Permanent("runtime tool_call_count exceeds i64".into())
            })?;
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
            let touched_files_json =
                serde_json::to_string(&stats.touched_files).map_err(|err| {
                    TaskStoreError::Permanent(format!("serialize touched_files failed: {err}"))
                })?;
            update = update
                .col_expr(
                    conversation::Column::DelegationToolCallCount,
                    sea_orm::sea_query::Expr::value(tool_call_count),
                )
                .col_expr(
                    conversation::Column::DelegationEditToolCallCount,
                    sea_orm::sea_query::Expr::value(edit_tool_call_count),
                )
                .col_expr(
                    conversation::Column::DelegationTouchedFilesJson,
                    sea_orm::sea_query::Expr::value(touched_files_json),
                )
                .col_expr(
                    conversation::Column::DelegationTouchedFilesTruncated,
                    sea_orm::sea_query::Expr::value(stats.touched_files_truncated),
                )
                .col_expr(
                    conversation::Column::DelegationAdditions,
                    sea_orm::sea_query::Expr::value(additions),
                )
                .col_expr(
                    conversation::Column::DelegationDeletions,
                    sea_orm::sea_query::Expr::value(deletions),
                )
                .col_expr(
                    conversation::Column::DelegationLineCountsComplete,
                    sea_orm::sea_query::Expr::value(stats.line_counts_complete),
                );
        }

        let result = update
            .filter(conversation::Column::DelegationCallId.eq(task_id))
            .filter(conversation::Column::DelegationTaskStatus.eq(DelegationTaskStatus::Running))
            .exec(&self.db.conn)
            .await
            .map_err(Self::map_db_err)?;

        if result.rows_affected > 0 {
            let row = self
                .load(task_id)
                .await?
                .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;
            return Ok(Settlement::Won(row.to_report(None)));
        }

        // Lost the CAS — replay persisted truth, never overwrite.
        let row = self
            .load(task_id)
            .await?
            .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;
        if row.status == TaskStatus::Running {
            return Err(TaskStoreError::Permanent(format!(
                "settle CAS missed but task {task_id} still running"
            )));
        }
        Ok(Settlement::Existing(row.to_report(None)))
    }

    async fn reconcile_running(&self, at: DateTime<Utc>) -> Result<u64, TaskStoreError> {
        // Authoritative: settle non-terminal run rows (+ monotonic projection).
        let runs = RunStore::new(self.db.clone());
        let from_runs = runs.reconcile_non_terminal(at).await?;

        // Fallback: conversation-only running rows with no matching run.
        let result = conversation::Entity::update_many()
            .col_expr(
                conversation::Column::DelegationTaskStatus,
                sea_orm::sea_query::Expr::value(DelegationTaskStatus::Failed),
            )
            .col_expr(
                conversation::Column::DelegationErrorCode,
                sea_orm::sea_query::Expr::value("host_restarted"),
            )
            .col_expr(
                conversation::Column::DelegationFinishedAt,
                sea_orm::sea_query::Expr::value(at),
            )
            .col_expr(
                conversation::Column::Status,
                sea_orm::sea_query::Expr::value(ConversationStatus::Cancelled),
            )
            .col_expr(
                conversation::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(at),
            )
            .filter(conversation::Column::DelegationTaskStatus.eq(DelegationTaskStatus::Running))
            .exec(&self.db.conn)
            .await
            .map_err(Self::map_db_err)?;
        // Conversation fallback may re-touch rows already projected by run
        // settle; count only run settlements as authoritative increments and
        // still report conversation rows_affected for legacy-only orphans.
        Ok(from_runs.saturating_add(result.rows_affected))
    }

    async fn write_runtime_stats(
        &self,
        task_id: &str,
        stats: &DelegationRuntimeStats,
    ) -> Result<(), TaskStoreError> {
        let runs = RunStore::new(self.db.clone());
        if runs.load_by_task_id(task_id).await?.is_some() {
            return runs.write_runtime_stats(task_id, stats).await;
        }

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

        // True freeze after settle: only `running` conversation rows accept
        // runtime writes. Terminal final stats land inside `settle` itself;
        // matching finished_at must never reopen mutability on the legacy path.
        let result = conversation::Entity::update_many()
            .col_expr(
                conversation::Column::DelegationToolCallCount,
                sea_orm::sea_query::Expr::value(tool_call_count),
            )
            .col_expr(
                conversation::Column::DelegationEditToolCallCount,
                sea_orm::sea_query::Expr::value(edit_tool_call_count),
            )
            .col_expr(
                conversation::Column::DelegationTouchedFilesJson,
                sea_orm::sea_query::Expr::value(touched_files_json),
            )
            .col_expr(
                conversation::Column::DelegationTouchedFilesTruncated,
                sea_orm::sea_query::Expr::value(stats.touched_files_truncated),
            )
            .col_expr(
                conversation::Column::DelegationAdditions,
                sea_orm::sea_query::Expr::value(additions),
            )
            .col_expr(
                conversation::Column::DelegationDeletions,
                sea_orm::sea_query::Expr::value(deletions),
            )
            .col_expr(
                conversation::Column::DelegationLineCountsComplete,
                sea_orm::sea_query::Expr::value(stats.line_counts_complete),
            )
            .col_expr(
                conversation::Column::UpdatedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .filter(conversation::Column::DelegationCallId.eq(task_id))
            .filter(conversation::Column::DelegationTaskStatus.eq(DelegationTaskStatus::Running))
            .exec(&self.db.conn)
            .await
            .map_err(Self::map_db_err)?;

        if result.rows_affected > 0 {
            return Ok(());
        }

        // Zero rows: terminal freeze and stale running after settle are benign.
        match self.load(task_id).await? {
            Some(row) if row.status != TaskStatus::Running => Ok(()),
            Some(_) if stats.finished_at.is_some() => {
                // Terminal-shaped write against a still-running row: not a
                // post-settle path; reject so callers notice the contract misuse.
                Err(TaskStoreError::Permanent(format!(
                    "terminal runtime_stats write matched no rows for still-running task {task_id}"
                )))
            }
            Some(_) => Err(TaskStoreError::Permanent(format!(
                "running runtime_stats write matched no rows for still-running task {task_id}"
            ))),
            None => Err(TaskStoreError::Permanent(format!(
                "runtime_stats write matched no rows; task {task_id} missing"
            ))),
        }
    }

    async fn put_retry(&self, retry: PendingTerminalRetry) {
        // Deduplicated by task_id — first record wins.
        self.retries
            .lock()
            .await
            .entry(retry.task_id.clone())
            .or_insert(retry);
    }

    async fn remove_retry(&self, task_id: &str) {
        self.retries.lock().await.remove(task_id);
    }

    async fn has_retry_record(&self, task_id: &str) -> bool {
        self.retries.lock().await.contains_key(task_id)
    }

    async fn get_retry(&self, task_id: &str) -> Option<PendingTerminalRetry> {
        self.retries.lock().await.get(task_id).cloned()
    }
}

/// Scripted in-memory store for broker unit tests.
#[cfg(any(test, feature = "test-utils"))]
pub mod mock {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

    #[derive(Debug, Clone)]
    enum SettleScript {
        Ok(Settlement),
        Err(TaskStoreError),
    }

    /// In-memory task store that scripts `settle` results and records calls.
    pub struct MockTaskStore {
        tasks: Mutex<HashMap<String, PersistedTask>>,
        settle_script: Mutex<VecDeque<SettleScript>>,
        settle_calls: Mutex<Vec<(String, TerminalTaskWrite)>>,
        fail_remaining: Mutex<Option<u32>>,
        retries: Mutex<HashMap<String, PendingTerminalRetry>>,
        default_child_id: AtomicI32,
        /// When true, `load` seeds a missing id as running (for send-fail tests
        /// where the call id is only known after start_delegation mints it).
        seed_on_load: std::sync::atomic::AtomicBool,
        /// Optional gate: each `settle` waits on a oneshot after signaling entry.
        settle_gate: Mutex<Option<SettleGate>>,
        pub settle_count: AtomicUsize,
        /// Ordered runtime-stats write attempts (Task 8 coalescing tests).
        runtime_writes: Mutex<Vec<(String, DelegationRuntimeStats)>>,
        /// One queued runtime-write error (consumed on the next attempt).
        fail_next_runtime: Mutex<Option<TaskStoreError>>,
        /// When true, the next `write_runtime_stats` hangs until cancelled
        /// (for timeout tests with a paused Tokio clock).
        hang_next_runtime: std::sync::atomic::AtomicBool,
    }

    /// Deterministic settle delay for mid-settle observation tests.
    struct SettleGate {
        entered: Option<tokio::sync::oneshot::Sender<()>>,
        release: Option<tokio::sync::oneshot::Receiver<()>>,
    }

    impl MockTaskStore {
        pub fn new() -> Self {
            Self {
                tasks: Mutex::new(HashMap::new()),
                settle_script: Mutex::new(VecDeque::new()),
                settle_calls: Mutex::new(Vec::new()),
                fail_remaining: Mutex::new(None),
                retries: Mutex::new(HashMap::new()),
                default_child_id: AtomicI32::new(0),
                seed_on_load: std::sync::atomic::AtomicBool::new(false),
                settle_gate: Mutex::new(None),
                settle_count: AtomicUsize::new(0),
                runtime_writes: Mutex::new(Vec::new()),
                fail_next_runtime: Mutex::new(None),
                hang_next_runtime: std::sync::atomic::AtomicBool::new(false),
            }
        }

        /// Auto-seed any missing task as running with the given child id
        /// (on settle only, unless [`Self::with_seed_on_load`]).
        pub fn accept_any_running(child_conversation_id: i32) -> Self {
            let s = Self::new();
            s.default_child_id
                .store(child_conversation_id, Ordering::SeqCst);
            s
        }

        /// Like [`Self::accept_any_running`] but also seeds on `load` so send
        /// failure can discover the row by call id before settle.
        pub fn accept_any_running_loadable(child_conversation_id: i32) -> Self {
            let s = Self::accept_any_running(child_conversation_id);
            s.seed_on_load.store(true, Ordering::SeqCst);
            s
        }

        pub fn with_running(task_id: &str, child_conversation_id: i32) -> Self {
            let s = Self::new();
            s.default_child_id
                .store(child_conversation_id, Ordering::SeqCst);
            // Constructor is exclusive — try_lock must succeed (never silent skip).
            let mut map = s
                .tasks
                .try_lock()
                .expect("MockTaskStore::with_running: tasks mutex busy at construction");
            map.insert(
                task_id.to_string(),
                PersistedTask {
                    task_id: task_id.to_string(),
                    child_conversation_id,
                    parent_id: Some(1),
                    agent_type: AgentType::ClaudeCode,
                    status: TaskStatus::Running,
                    error_code: None,
                    started_at: Some(Utc::now()),
                    finished_at: None,
                    runtime_stats: None,
                },
            );
            drop(map);
            s
        }

        /// Fail the next `n` settle attempts with a transient error, then CAS.
        pub fn fail_settle_times(n: u32) -> Self {
            let s = Self::with_running("task-1", 42);
            *s.fail_remaining
                .try_lock()
                .expect("MockTaskStore::fail_settle_times: fail_remaining busy") = Some(n);
            s
        }

        /// Install a one-shot settle gate: next `settle` signals `entered` then
        /// waits on `release` before applying CAS. Used for mid-settle tests.
        pub async fn install_settle_gate(
            &self,
            entered: tokio::sync::oneshot::Sender<()>,
            release: tokio::sync::oneshot::Receiver<()>,
        ) {
            *self.settle_gate.lock().await = Some(SettleGate {
                entered: Some(entered),
                release: Some(release),
            });
        }

        pub async fn seed_running(
            &self,
            task_id: &str,
            child_conversation_id: i32,
            parent_id: Option<i32>,
        ) {
            self.tasks.lock().await.insert(
                task_id.to_string(),
                PersistedTask {
                    task_id: task_id.to_string(),
                    child_conversation_id,
                    parent_id,
                    agent_type: AgentType::ClaudeCode,
                    status: TaskStatus::Running,
                    error_code: None,
                    started_at: Some(Utc::now()),
                    finished_at: None,
                    runtime_stats: None,
                },
            );
        }

        /// Seed the direct parent/child conversation edge for a running task
        /// (used by coordination / attention Broker tests).
        pub async fn seed_edge(
            &self,
            task_id: &str,
            parent_conversation_id: i32,
            child_conversation_id: i32,
        ) {
            self.seed_running(task_id, child_conversation_id, Some(parent_conversation_id))
                .await;
        }

        pub async fn queue_settle_ok(&self, settlement: Settlement) {
            self.settle_script
                .lock()
                .await
                .push_back(SettleScript::Ok(settlement));
        }

        pub async fn queue_settle_err(&self, err: TaskStoreError) {
            self.settle_script
                .lock()
                .await
                .push_back(SettleScript::Err(err));
        }

        pub async fn persisted(&self, task_id: &str) -> PersistedTask {
            self.tasks
                .lock()
                .await
                .get(task_id)
                .cloned()
                .unwrap_or_else(|| panic!("no persisted task {task_id}"))
        }

        pub async fn settle_call_count(&self) -> usize {
            self.settle_calls.lock().await.len()
        }

        /// All settle attempts (including transient failures) in order.
        pub async fn settle_calls(&self) -> Vec<(String, TerminalTaskWrite)> {
            self.settle_calls.lock().await.clone()
        }

        /// Fail the next `n` settle attempts with a transient error, then CAS.
        pub fn set_fail_settle_times(&self, n: u32) {
            *self
                .fail_remaining
                .try_lock()
                .expect("MockTaskStore::set_fail_settle_times: fail_remaining busy") = Some(n);
        }

        /// Queue one permanent/transient failure for the next runtime write.
        pub fn fail_next_runtime_write(&self, err: TaskStoreError) {
            *self
                .fail_next_runtime
                .try_lock()
                .expect("MockTaskStore::fail_next_runtime_write: mutex busy") = Some(err);
        }

        /// Next `write_runtime_stats` hangs until the outer timeout cancels it.
        pub fn hang_next_runtime_write(&self) {
            self.hang_next_runtime.store(true, Ordering::SeqCst);
        }

        pub async fn runtime_write_count(&self, task_id: &str) -> usize {
            self.runtime_writes
                .lock()
                .await
                .iter()
                .filter(|(id, _)| id == task_id)
                .count()
        }

        /// Latest accepted runtime snapshot for `task_id` (row, else last attempt).
        pub async fn latest_runtime(&self, task_id: &str) -> Option<DelegationRuntimeStats> {
            if let Some(stats) = self
                .tasks
                .lock()
                .await
                .get(task_id)
                .and_then(|t| t.runtime_stats.clone())
            {
                return Some(stats);
            }
            self.runtime_writes
                .lock()
                .await
                .iter()
                .rev()
                .find(|(id, _)| id == task_id)
                .map(|(_, stats)| stats.clone())
        }

        pub async fn all_runtime_writes(&self) -> Vec<(String, DelegationRuntimeStats)> {
            self.runtime_writes.lock().await.clone()
        }

        fn seed_if_missing(map: &mut HashMap<String, PersistedTask>, task_id: &str, child_id: i32) {
            map.entry(task_id.to_string())
                .or_insert_with(|| PersistedTask {
                    task_id: task_id.to_string(),
                    child_conversation_id: child_id,
                    parent_id: Some(1),
                    agent_type: AgentType::ClaudeCode,
                    status: TaskStatus::Running,
                    error_code: None,
                    started_at: Some(Utc::now()),
                    finished_at: None,
                    runtime_stats: None,
                });
        }
    }

    impl Default for MockTaskStore {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl DelegationTaskStore for MockTaskStore {
        async fn load(&self, task_id: &str) -> Result<Option<PersistedTask>, TaskStoreError> {
            let mut map = self.tasks.lock().await;
            if self.seed_on_load.load(Ordering::SeqCst) {
                let child_id = self.default_child_id.load(Ordering::SeqCst);
                Self::seed_if_missing(&mut map, task_id, child_id);
            }
            Ok(map.get(task_id).cloned())
        }

        async fn resolve_unique_owned_prefix(
            &self,
            parent_id: i32,
            prefix: &str,
        ) -> Result<Option<String>, TaskStoreError> {
            if !is_valid_task_id_prefix(prefix) {
                return Ok(None);
            }
            let map = self.tasks.lock().await;
            let mut matches = map
                .values()
                .filter(|task| task.parent_id == Some(parent_id))
                .filter(|task| {
                    task.task_id
                        .get(..prefix.len())
                        .is_some_and(|value| value.eq_ignore_ascii_case(prefix))
                })
                .map(|task| task.task_id.clone())
                .take(2);
            let first = matches.next();
            Ok(match (first, matches.next()) {
                (Some(task_id), None) => Some(task_id),
                _ => None,
            })
        }

        async fn settle(
            &self,
            task_id: &str,
            terminal: TerminalTaskWrite,
        ) -> Result<Settlement, TaskStoreError> {
            self.settle_count.fetch_add(1, Ordering::SeqCst);
            self.settle_calls
                .lock()
                .await
                .push((task_id.to_string(), terminal.clone()));

            // Optional mid-settle gate (deterministic; no multi-second sleeps).
            let gate = self.settle_gate.lock().await.take();
            if let Some(mut gate) = gate {
                if let Some(tx) = gate.entered.take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = gate.release.take() {
                    let _ = rx.await;
                }
            }

            if let Some(remaining) = self.fail_remaining.lock().await.as_mut() {
                if *remaining > 0 {
                    *remaining -= 1;
                    return Err(TaskStoreError::Transient("database is locked".into()));
                }
            }

            if let Some(scripted) = self.settle_script.lock().await.pop_front() {
                return match scripted {
                    SettleScript::Ok(s) => Ok(s),
                    SettleScript::Err(e) => Err(e),
                };
            }

            let child_id = self.default_child_id.load(Ordering::SeqCst);
            let mut map = self.tasks.lock().await;
            Self::seed_if_missing(&mut map, task_id, child_id);
            let entry = map.get_mut(task_id).expect("just inserted");
            if entry.status != TaskStatus::Running {
                return Ok(Settlement::Existing(entry.to_report(None)));
            }
            entry.status = terminal.status;
            entry.error_code = terminal.error_code.clone();
            entry.finished_at = Some(terminal.finished_at);
            // Final runtime snapshot is part of the settle payload (mirrors
            // RunStore::settle_terminal). Post-terminal write_runtime_stats may
            // still accept matching finished_at for unit-test convenience.
            if let Some(stats) = terminal.runtime_stats {
                entry.runtime_stats = Some(stats);
            }
            Ok(Settlement::Won(entry.to_report(None)))
        }

        async fn reconcile_running(&self, at: DateTime<Utc>) -> Result<u64, TaskStoreError> {
            let mut map = self.tasks.lock().await;
            let mut n = 0u64;
            for t in map.values_mut() {
                if t.status == TaskStatus::Running {
                    t.status = TaskStatus::Failed;
                    t.error_code = Some("host_restarted".into());
                    t.finished_at = Some(at);
                    n += 1;
                }
            }
            Ok(n)
        }

        async fn write_runtime_stats(
            &self,
            task_id: &str,
            stats: &DelegationRuntimeStats,
        ) -> Result<(), TaskStoreError> {
            // Record every attempt (including hangs that complete after cancel
            // never reach here — hang is before record so timed-out calls still
            // count once the future is polled).
            if self.hang_next_runtime.swap(false, Ordering::SeqCst) {
                // Count the attempt, then hang until the outer timeout cancels.
                self.runtime_writes
                    .lock()
                    .await
                    .push((task_id.to_string(), stats.clone()));
                std::future::pending::<()>().await;
                unreachable!("hang_next_runtime future was resumed after cancel");
            }
            self.runtime_writes
                .lock()
                .await
                .push((task_id.to_string(), stats.clone()));
            if let Some(err) = self.fail_next_runtime.lock().await.take() {
                return Err(err);
            }
            let mut map = self.tasks.lock().await;
            // accept_any_running mode: seed a running row so coalesced writes
            // during setup/running can land without an explicit seed_edge.
            let child_id = self.default_child_id.load(Ordering::SeqCst);
            if !map.contains_key(task_id) && child_id != 0 {
                Self::seed_if_missing(&mut map, task_id, child_id);
            }
            let Some(entry) = map.get_mut(task_id) else {
                return Err(TaskStoreError::Permanent(format!(
                    "runtime_stats write for missing task {task_id}"
                )));
            };
            // Observability: record every attempt above. Persistence mirrors
            // production freeze — only running rows accept mutation. Terminal
            // final stats land in settle; post-terminal writes are benign no-ops.
            if entry.status == TaskStatus::Running && stats.finished_at.is_none() {
                entry.runtime_stats = Some(stats.clone());
                return Ok(());
            }
            if entry.status != TaskStatus::Running {
                // Frozen terminal (or non-running): benign no-op.
                return Ok(());
            }
            // Terminal-shaped write against a still-running row — reject.
            Err(TaskStoreError::Permanent(format!(
                "runtime_stats write rejected for task {task_id}"
            )))
        }

        async fn put_retry(&self, retry: PendingTerminalRetry) {
            self.retries
                .lock()
                .await
                .entry(retry.task_id.clone())
                .or_insert(retry);
        }

        async fn remove_retry(&self, task_id: &str) {
            self.retries.lock().await.remove(task_id);
        }

        async fn has_retry_record(&self, task_id: &str) -> bool {
            self.retries.lock().await.contains_key(task_id)
        }

        async fn get_retry(&self, task_id: &str) -> Option<PendingTerminalRetry> {
            self.retries.lock().await.get(task_id).cloned()
        }
    }
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

    async fn test_store_with_running_task(task_id: &str) -> Arc<AppDatabase> {
        let db = Arc::new(fresh_in_memory_db().await);
        let folder = seed_folder(&db, "/tmp/codeg-delegation-store-cas").await;
        let parent = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .expect("parent");
        let link = DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tu-1".into(),
            delegation_call_id: task_id.into(),
        };
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(link),
        )
        .await
        .expect("child");
        assert_eq!(
            child.delegation_task_status,
            Some(DelegationTaskStatus::Running),
            "accepted insert must stamp running task status"
        );
        db
    }

    async fn test_store_with_statuses(rows: &[(&str, DelegationTaskStatus)]) -> Arc<AppDatabase> {
        let db = Arc::new(fresh_in_memory_db().await);
        let folder = seed_folder(&db, "/tmp/codeg-delegation-store-reconcile").await;
        let parent = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .expect("parent");
        for (task_id, status) in rows {
            let link = DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: format!("tu-{task_id}"),
                delegation_call_id: (*task_id).into(),
            };
            conversation_service::create_with_delegation(
                &db.conn,
                folder,
                AgentType::Codex,
                Some((*task_id).into()),
                None,
                Some(link),
            )
            .await
            .expect("child");
            if *status != DelegationTaskStatus::Running {
                let store = DbDelegationTaskStore::new(db.clone());
                let write = match status {
                    DelegationTaskStatus::Completed => {
                        TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview)
                    }
                    DelegationTaskStatus::Failed => TerminalTaskWrite::failed(
                        "spawn_failed",
                        Utc::now(),
                        ConversationStatus::Cancelled,
                    ),
                    DelegationTaskStatus::Canceled => TerminalTaskWrite::canceled(
                        "usercancel",
                        Utc::now(),
                        ConversationStatus::Cancelled,
                    ),
                    DelegationTaskStatus::Running => unreachable!(),
                };
                store.settle(task_id, write).await.expect("seed settle");
            }
        }
        db
    }

    #[tokio::test]
    async fn terminal_cas_has_one_winner_and_replays_persisted_truth() {
        let db = test_store_with_running_task("task-1").await;
        let store = DbDelegationTaskStore::new(db.clone());
        let completed = TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview);
        let canceled =
            TerminalTaskWrite::canceled("usercancel", Utc::now(), ConversationStatus::Cancelled);

        let (a, b) = tokio::join!(
            store.settle("task-1", completed),
            store.settle("task-1", canceled),
        );
        let reports = [a.unwrap().report().clone(), b.unwrap().report().clone()];
        assert_eq!(reports[0].status, reports[1].status);
        assert_eq!(reports[0].error_code, reports[1].error_code);

        let row = store.load("task-1").await.unwrap().unwrap();
        assert_ne!(row.status, TaskStatus::Running);
        assert!(row.finished_at.is_some());
    }

    #[tokio::test]
    async fn startup_reconciliation_fails_only_running_delegate_rows() {
        let db = test_store_with_statuses(&[
            ("running", DelegationTaskStatus::Running),
            ("done", DelegationTaskStatus::Completed),
        ])
        .await;
        let store = DbDelegationTaskStore::new(db);
        let reconciled = store.reconcile_running(Utc::now()).await.unwrap();
        assert_eq!(reconciled, 1);
        let orphan = store.load("running").await.unwrap().unwrap();
        assert_eq!(orphan.status, TaskStatus::Failed);
        assert_eq!(orphan.error_code.as_deref(), Some("host_restarted"));
        assert_eq!(
            store.load("done").await.unwrap().unwrap().status,
            TaskStatus::Completed
        );
    }

    #[tokio::test]
    async fn host_restarted_reconcile_sets_conversation_cancelled() {
        let db = test_store_with_running_task("orphan-1").await;
        let store = DbDelegationTaskStore::new(db.clone());
        store.reconcile_running(Utc::now()).await.unwrap();
        let summary = conversation_service::get_by_delegation_call_id(&db.conn, "orphan-1")
            .await
            .expect("load")
            .expect("row");
        assert_eq!(summary.status, "cancelled");
        assert_eq!(
            summary.delegation_task_status,
            Some(DelegationTaskStatus::Failed)
        );
        assert_eq!(
            summary.delegation_error_code.as_deref(),
            Some("host_restarted")
        );
    }

    #[tokio::test]
    async fn cold_load_uses_delegation_columns_not_conversation_status() {
        let db = test_store_with_running_task("cold-1").await;
        let store = DbDelegationTaskStore::new(db.clone());
        store
            .settle(
                "cold-1",
                TerminalTaskWrite::failed(
                    "spawn_failed",
                    Utc::now(),
                    ConversationStatus::Cancelled,
                ),
            )
            .await
            .unwrap();
        let row = store.load("cold-1").await.unwrap().unwrap();
        assert_eq!(row.status, TaskStatus::Failed);
        assert_eq!(row.error_code.as_deref(), Some("spawn_failed"));
        let report = row.to_report(None);
        assert_eq!(report.status, TaskStatus::Failed);
        assert_eq!(report.error_code.as_deref(), Some("spawn_failed"));
    }

    #[tokio::test]
    async fn task_id_prefix_lookup_is_parent_scoped_and_rejects_ambiguity() {
        let canonical = "6b8f8330-07be-45cc-bf4c-98c10e6921ff";
        let db = test_store_with_statuses(&[
            (canonical, DelegationTaskStatus::Running),
            (
                "deadbeef-0000-4000-8000-000000000001",
                DelegationTaskStatus::Running,
            ),
            (
                "deadbeef-0000-4000-8000-000000000002",
                DelegationTaskStatus::Running,
            ),
        ])
        .await;
        let store = DbDelegationTaskStore::new(db.clone());
        let parent_id = store
            .load(canonical)
            .await
            .unwrap()
            .unwrap()
            .parent_id
            .unwrap();

        let foreign_folder = seed_folder(&db, "/tmp/codeg-delegation-prefix-foreign").await;
        let foreign_parent = conversation_service::create(
            &db.conn,
            foreign_folder,
            AgentType::ClaudeCode,
            Some("foreign-parent".into()),
            None,
        )
        .await
        .expect("foreign parent");
        let foreign_task = "6b8f8330-0000-4000-8000-000000000002";
        conversation_service::create_with_delegation(
            &db.conn,
            foreign_folder,
            AgentType::Codex,
            Some("foreign-child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: foreign_parent.id,
                parent_tool_use_id: "tu-foreign".into(),
                delegation_call_id: foreign_task.into(),
            }),
        )
        .await
        .expect("foreign child");

        assert_eq!(
            store
                .resolve_unique_owned_prefix(parent_id, "6b8f8330")
                .await
                .unwrap()
                .as_deref(),
            Some(canonical)
        );
        assert_eq!(
            store
                .resolve_unique_owned_prefix(foreign_parent.id, "6b8f8330")
                .await
                .unwrap()
                .as_deref(),
            Some(foreign_task)
        );
        assert!(store
            .resolve_unique_owned_prefix(parent_id, "deadbeef")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .resolve_unique_owned_prefix(foreign_parent.id, "%%%%%%%%")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn runtime_stats_round_trip_preserves_counts_paths_and_nullable_lines() {
        use crate::acp::delegation::runtime_stats::{
            DelegationRuntimeStats, DelegationTouchedFile,
        };

        let db = test_store_with_running_task("rt-round-1").await;
        let store = DbDelegationTaskStore::new(db.clone());
        let started = store
            .load("rt-round-1")
            .await
            .unwrap()
            .unwrap()
            .started_at
            .expect("started_at");
        let stats = DelegationRuntimeStats {
            started_at: started,
            finished_at: None,
            tool_call_count: 3,
            edit_tool_call_count: 2,
            touched_files: vec![
                DelegationTouchedFile {
                    path: "src/a.rs".into(),
                    outside_workspace: false,
                    additions: Some(4),
                    deletions: Some(1),
                },
                DelegationTouchedFile {
                    path: "/outside/b.rs".into(),
                    outside_workspace: true,
                    additions: None,
                    deletions: None,
                },
            ],
            touched_files_truncated: true,
            additions: Some(4),
            deletions: Some(1),
            line_counts_complete: true,
        };
        store
            .write_runtime_stats("rt-round-1", &stats)
            .await
            .expect("write running snapshot");
        let loaded = store
            .load("rt-round-1")
            .await
            .expect("load")
            .expect("row")
            .runtime_stats
            .expect("runtime_stats");
        assert_eq!(loaded.tool_call_count, 3);
        assert_eq!(loaded.edit_tool_call_count, 2);
        assert_eq!(loaded.touched_files, stats.touched_files);
        assert!(loaded.touched_files_truncated);
        assert_eq!(loaded.additions, Some(4));
        assert_eq!(loaded.deletions, Some(1));
        assert!(loaded.line_counts_complete);
        assert_eq!(loaded.started_at, started);
        assert!(loaded.finished_at.is_none());
    }

    #[tokio::test]
    async fn runtime_stats_stale_running_write_does_not_overwrite_terminal() {
        use crate::acp::delegation::runtime_stats::{
            DelegationRuntimeStats, DelegationTouchedFile,
        };

        let db = test_store_with_running_task("rt-stale-1").await;
        let store = DbDelegationTaskStore::new(db.clone());
        let started = store
            .load("rt-stale-1")
            .await
            .unwrap()
            .unwrap()
            .started_at
            .expect("started_at");
        let finished = Utc::now();
        let terminal = DelegationRuntimeStats {
            started_at: started,
            finished_at: Some(finished),
            tool_call_count: 5,
            edit_tool_call_count: 2,
            touched_files: vec![DelegationTouchedFile {
                path: "final.rs".into(),
                outside_workspace: false,
                additions: Some(2),
                deletions: Some(0),
            }],
            touched_files_truncated: false,
            additions: Some(2),
            deletions: Some(0),
            line_counts_complete: true,
        };
        // Final stats land in the settlement write — not via post-terminal
        // write_runtime_stats (legacy freeze).
        store
            .settle(
                "rt-stale-1",
                TerminalTaskWrite::completed(finished, ConversationStatus::PendingReview)
                    .with_runtime_stats(terminal.clone()),
            )
            .await
            .expect("settle with final stats");
        let after_settle = store
            .load("rt-stale-1")
            .await
            .unwrap()
            .unwrap()
            .runtime_stats
            .clone();
        assert_eq!(
            after_settle.as_ref().map(|s| s.tool_call_count),
            Some(5),
            "settle must persist final stats on conversation fallback"
        );

        // Matching finished_at must not reopen terminal mutability.
        let mut rewrite = terminal.clone();
        rewrite.tool_call_count = 99;
        store
            .write_runtime_stats("rt-stale-1", &rewrite)
            .await
            .expect("post-terminal write is frozen no-op");
        let after_terminal_write = store
            .load("rt-stale-1")
            .await
            .unwrap()
            .unwrap()
            .runtime_stats
            .clone();
        assert_eq!(
            after_terminal_write.as_ref().map(|s| s.tool_call_count),
            Some(5),
            "legacy conversation fallback must freeze after settle"
        );

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
            .write_runtime_stats("rt-stale-1", &stale_running)
            .await
            .expect("stale running write is benign");
        let after_stale = store
            .load("rt-stale-1")
            .await
            .unwrap()
            .unwrap()
            .runtime_stats;
        assert_eq!(after_stale, after_settle);
        assert_eq!(
            after_stale.as_ref().map(|s| s.tool_call_count),
            Some(5),
            "stale running must not clear settled stats"
        );
    }

    #[tokio::test]
    async fn runtime_stats_historical_null_load_yields_none() {
        let db = Arc::new(fresh_in_memory_db().await);
        let folder = seed_folder(&db, "/tmp/codeg-delegation-hist-null").await;
        let parent = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .expect("parent");
        // Non-delegate rows have null rollup columns; load by minting a
        // synthetic call id is not available. Instead verify a fresh
        // delegate's zero snapshot decodes, and a missing task is None.
        let store = DbDelegationTaskStore::new(db.clone());
        assert!(store.load("missing-task").await.unwrap().is_none());
        let _ = parent;
        // Regular conversation has no task id — create a delegate then clear
        // rollups via raw SQL-less update to simulate historical nulls is not
        // needed: decode path on null required fields is covered by model_to_persisted
        // when columns are null. Seed a running task and overwrite columns to null
        // through entity update.
        let link = DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tu-hist".into(),
            delegation_call_id: "hist-null".into(),
        };
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(link),
        )
        .await
        .expect("child");
        use sea_orm::{ActiveModelTrait, Set};
        let mut active: conversation::ActiveModel = child.into();
        active.delegation_tool_call_count = Set(None);
        active.delegation_edit_tool_call_count = Set(None);
        active.delegation_touched_files_json = Set(None);
        active.delegation_touched_files_truncated = Set(None);
        active.delegation_additions = Set(None);
        active.delegation_deletions = Set(None);
        active.delegation_line_counts_complete = Set(None);
        active.update(&db.conn).await.expect("null rollups");
        let loaded = store.load("hist-null").await.unwrap().unwrap();
        assert!(loaded.runtime_stats.is_none());
    }

    #[tokio::test]
    async fn db_store_settle_with_final_runtime_stats_via_run_row() {
        use crate::acp::delegation::run_store::{
            derive_task_preview, request_fingerprint, ReservingRunInsert, RunStore,
        };
        use crate::acp::delegation::runtime_stats::{
            DelegationRuntimeStats, DelegationTouchedFile,
        };
        use crate::db::entities::delegation_task_run::AdmissionClass;
        use sea_orm::EntityTrait;

        let db = Arc::new(fresh_in_memory_db().await);
        let folder = seed_folder(&db, "/tmp/codeg-delegation-settle-final").await;
        let parent = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some("parent".into()),
            None,
        )
        .await
        .expect("parent");
        let task_id = "60606060-cccc-4ccc-8ccc-cccccccccccc";
        let child = conversation_service::create_with_delegation(
            &db.conn,
            folder,
            AgentType::Codex,
            Some("child".into()),
            None,
            Some(DelegationLink {
                parent_conversation_id: parent.id,
                parent_tool_use_id: format!("tu-{task_id}"),
                delegation_call_id: task_id.into(),
            }),
        )
        .await
        .expect("child");

        let runs = RunStore::new(db.clone());
        runs.insert_reserving(ReservingRunInsert {
            task_id: task_id.into(),
            root_task_id: task_id.into(),
            previous_task_id: None,
            generation: 1,
            parent_conversation_id: parent.id,
            parent_tool_use_id: Some(format!("tu-{task_id}")),
            child_conversation_id: child.id,
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
            lineage_root_task_id: task_id.into(),
            work_unit_key: None,
            history_only: false,
            replaced_task_id: None,
            replacement_reason: None,
            started_at: Some(Utc::now()),
        })
        .await
        .expect("insert run");
        runs.promote_running(task_id, "conn-final", Utc::now())
            .await
            .expect("promote");

        let started = runs
            .load_by_task_id(task_id)
            .await
            .unwrap()
            .unwrap()
            .started_at
            .expect("started");
        let finished = Utc::now();
        let final_stats = DelegationRuntimeStats {
            started_at: started,
            finished_at: Some(finished),
            tool_call_count: 8,
            edit_tool_call_count: 3,
            touched_files: vec![DelegationTouchedFile {
                path: "db-final.rs".into(),
                outside_workspace: false,
                additions: Some(7),
                deletions: Some(2),
            }],
            touched_files_truncated: false,
            additions: Some(7),
            deletions: Some(2),
            line_counts_complete: true,
        };

        let store = DbDelegationTaskStore::new(db.clone());
        let settlement = store
            .settle(
                task_id,
                TerminalTaskWrite::completed(finished, ConversationStatus::PendingReview)
                    .with_runtime_stats(final_stats.clone()),
            )
            .await
            .expect("settle with final stats");
        assert!(settlement.won());

        let loaded = store.load(task_id).await.unwrap().unwrap();
        assert_eq!(loaded.status, TaskStatus::Completed);
        let stats = loaded.runtime_stats.expect("final stats on load");
        assert_eq!(stats.tool_call_count, 8);
        assert_eq!(stats.edit_tool_call_count, 3);
        assert_eq!(stats.additions, Some(7));
        assert_eq!(stats.deletions, Some(2));
        assert!(stats.line_counts_complete);

        let child_row = conversation::Entity::find_by_id(child.id)
            .one(&db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(child_row.delegation_tool_call_count, Some(8));
        assert_eq!(child_row.delegation_additions, Some(7));
        assert_eq!(child_row.delegation_deletions, Some(2));
        assert_eq!(child_row.delegation_line_counts_complete, Some(true));

        // Post-terminal write is frozen on the run-backed path.
        let mut after = final_stats;
        after.tool_call_count = 99;
        store
            .write_runtime_stats(task_id, &after)
            .await
            .expect("frozen no-op");
        let frozen = store
            .load(task_id)
            .await
            .unwrap()
            .unwrap()
            .runtime_stats
            .expect("still present");
        assert_eq!(frozen.tool_call_count, 8);
    }
}
