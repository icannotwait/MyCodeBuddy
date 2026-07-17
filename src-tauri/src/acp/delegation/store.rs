//! Durable accepted/terminal state for Codeg delegation tasks.
//!
//! Production truth lives on the child conversation row:
//! `delegation_task_status` / `delegation_error_code` /
//! `delegation_started_at` / `delegation_finished_at`, written with a single
//! conditional `running -> terminal` CAS. In-memory cache, notifiers, meta,
//! events, and teardown run only after that write (or after replaying the
//! persisted winner).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::Mutex;

use crate::acp::delegation::runtime_stats::{
    decode_persisted_runtime_stats, DelegationRuntimeStats, PersistedRuntimeStatsColumns,
};
use crate::acp::delegation::types::{DelegationTaskReport, TaskStatus};
use crate::db::entities::conversation::{self, ConversationStatus, DelegationTaskStatus};
use crate::db::AppDatabase;
use crate::models::AgentType;

/// One attempted durable terminal write (CAS payload).
#[derive(Debug, Clone)]
pub struct TerminalTaskWrite {
    pub status: TaskStatus,
    pub error_code: Option<String>,
    pub finished_at: DateTime<Utc>,
    pub conversation_status: ConversationStatus,
}

impl TerminalTaskWrite {
    pub fn completed(finished_at: DateTime<Utc>, conversation_status: ConversationStatus) -> Self {
        Self {
            status: TaskStatus::Completed,
            error_code: None,
            finished_at,
            conversation_status,
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
        }
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
}

impl TaskStoreError {
    pub fn is_transient(&self) -> bool {
        matches!(self, TaskStoreError::Transient(_))
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

pub(super) fn is_transient_sqlite(msg: &str) -> bool {
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
        let row = conversation::Entity::find()
            .filter(conversation::Column::DelegationCallId.eq(task_id))
            .one(&self.db.conn)
            .await
            .map_err(Self::map_db_err)?;
        Ok(row.and_then(Self::model_to_persisted))
    }

    async fn settle(
        &self,
        task_id: &str,
        terminal: TerminalTaskWrite,
    ) -> Result<Settlement, TaskStoreError> {
        let persisted_status = terminal.to_persisted_status()?;
        let result = conversation::Entity::update_many()
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
            )
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
        Ok(result.rows_affected)
    }

    async fn write_runtime_stats(
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

        let mut update = conversation::Entity::update_many()
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
            .filter(conversation::Column::DelegationCallId.eq(task_id));

        if stats.finished_at.is_none() {
            update = update.filter(
                conversation::Column::DelegationTaskStatus.eq(DelegationTaskStatus::Running),
            );
        } else {
            update = update
                .filter(
                    conversation::Column::DelegationTaskStatus.ne(DelegationTaskStatus::Running),
                )
                .filter(conversation::Column::DelegationFinishedAt.eq(stats.finished_at));
        }

        let result = update.exec(&self.db.conn).await.map_err(Self::map_db_err)?;

        if result.rows_affected > 0 {
            return Ok(());
        }

        if stats.finished_at.is_none() {
            // Running write affected zero rows — terminal supersession is benign.
            match self.load(task_id).await? {
                Some(row) if row.status != TaskStatus::Running => Ok(()),
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
            let mut map = self.tasks.lock().await;
            let Some(entry) = map.get_mut(task_id) else {
                return Err(TaskStoreError::Permanent(format!(
                    "runtime_stats write for missing task {task_id}"
                )));
            };
            let accept = if stats.finished_at.is_none() {
                entry.status == TaskStatus::Running
            } else {
                entry.status != TaskStatus::Running && entry.finished_at == stats.finished_at
            };
            if accept {
                entry.runtime_stats = Some(stats.clone());
                return Ok(());
            }
            if stats.finished_at.is_none() && entry.status != TaskStatus::Running {
                // Stale running snapshot superseded by terminal row.
                return Ok(());
            }
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
        store
            .settle(
                "rt-stale-1",
                TerminalTaskWrite::completed(finished, ConversationStatus::PendingReview),
            )
            .await
            .expect("settle");
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
        store
            .write_runtime_stats("rt-stale-1", &terminal)
            .await
            .expect("terminal write");
        let after_terminal = store
            .load("rt-stale-1")
            .await
            .unwrap()
            .unwrap()
            .runtime_stats
            .clone();
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
        assert_eq!(after_stale, after_terminal);
        let terminal_bytes = serde_json::to_vec(&after_terminal).expect("serialize");
        let stale_bytes = serde_json::to_vec(&after_stale).expect("serialize");
        assert_eq!(terminal_bytes, stale_bytes);
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
}
