//! Persisted parent-decision (attention) repository for event-driven Join.
//!
//! Production truth lives in `delegation_attention_requests`. Opening a request
//! is one atomic `INSERT ... SELECT` gated on a direct parent/child edge and
//! `delegation_task_status='running'`. Reply and task closure use conditional
//! `open -> resolved` updates so concurrent writers produce one durable winner.
//! Database failures stay as [`AttentionStoreError::Database`] and are never
//! downgraded to a public missing outcome.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, Statement,
};
use serde::{Deserialize, Serialize};

use crate::acp::delegation::store::is_transient_sqlite;
use crate::db::entities::conversation::{self, DelegationTaskStatus};
use crate::db::entities::delegation_attention_request;
use crate::db::AppDatabase;

pub const ATTENTION_PAYLOAD_MAX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionResolutionCode {
    ParentReply,
    TaskTerminal,
    ParentCanceled,
    ParentTurnFailed,
    JoinAbandoned,
    ParentDisconnected,
    HostRestarted,
}

impl AttentionResolutionCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParentReply => "parent_reply",
            Self::TaskTerminal => "task_terminal",
            Self::ParentCanceled => "parent_canceled",
            Self::ParentTurnFailed => "parent_turn_failed",
            Self::JoinAbandoned => "join_abandoned",
            Self::ParentDisconnected => "parent_disconnected",
            Self::HostRestarted => "host_restarted",
        }
    }

    pub fn from_storage(value: &str) -> Result<Self, AttentionStoreError> {
        match value {
            "parent_reply" => Ok(Self::ParentReply),
            "task_terminal" => Ok(Self::TaskTerminal),
            "parent_canceled" => Ok(Self::ParentCanceled),
            "parent_turn_failed" => Ok(Self::ParentTurnFailed),
            "join_abandoned" => Ok(Self::JoinAbandoned),
            "parent_disconnected" => Ok(Self::ParentDisconnected),
            "host_restarted" => Ok(Self::HostRestarted),
            _ => Err(AttentionStoreError::Database(
                "attention row has an unknown resolution code".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionRequestSummary {
    pub request_id: String,
    pub task_id: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionRecord {
    pub summary: AttentionRequestSummary,
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub child_tool_call_id: String,
    pub reply: Option<String>,
    pub resolution_code: Option<AttentionResolutionCode>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewAttentionRequest {
    pub task_id: String,
    pub parent_conversation_id: i32,
    pub child_conversation_id: i32,
    pub child_tool_call_id: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub enum AttentionOpenResult {
    Opened(AttentionRecord),
    Recovered(AttentionRecord),
}

impl AttentionOpenResult {
    pub fn record(&self) -> &AttentionRecord {
        match self {
            Self::Opened(record) | Self::Recovered(record) => record,
        }
    }

    pub fn into_record(self) -> AttentionRecord {
        match self {
            Self::Opened(record) | Self::Recovered(record) => record,
        }
    }
}

#[derive(Debug, Clone)]
pub enum AttentionResolveResult {
    Resolved(AttentionRecord),
    Idempotent(AttentionRecord),
    Conflict(AttentionRecord),
    Missing,
    Unauthorized,
}

#[derive(Debug, thiserror::Error)]
pub enum AttentionStoreError {
    #[error("attention payload exceeds 16 KiB")]
    PayloadTooLarge,
    #[error("attention payload must not be blank")]
    BlankPayload,
    #[error("attention request does not belong to the direct parent/child edge")]
    Unauthorized,
    #[error("task already has an open attention request")]
    AlreadyOpen,
    #[error("delegation task is not running")]
    TaskNotRunning,
    #[error("attention request was not found")]
    NotFound,
    #[error("attention database error: {0}")]
    Database(String),
}

/// Reject oversized or blank payloads. Does not trim content for storage —
/// only uses whitespace checks for the blank check so exact nonblank bytes
/// that pass are the exact bytes returned later.
pub fn validate_attention_payload(text: &str) -> Result<(), AttentionStoreError> {
    if text.len() > ATTENTION_PAYLOAD_MAX_BYTES {
        return Err(AttentionStoreError::PayloadTooLarge);
    }
    if text.trim().is_empty() {
        return Err(AttentionStoreError::BlankPayload);
    }
    Ok(())
}

#[async_trait]
pub trait DelegationAttentionStore: Send + Sync {
    async fn open_or_recover(
        &self,
        request: NewAttentionRequest,
    ) -> Result<AttentionOpenResult, AttentionStoreError>;
    async fn list_open_for_tasks(
        &self,
        parent_conversation_id: i32,
        task_ids: &[String],
    ) -> Result<Vec<AttentionRequestSummary>, AttentionStoreError>;
    async fn wait_snapshot(&self, request_id: &str)
        -> Result<AttentionRecord, AttentionStoreError>;
    async fn reply(
        &self,
        parent_conversation_id: i32,
        request_id: &str,
        reply: &str,
        at: DateTime<Utc>,
    ) -> Result<AttentionResolveResult, AttentionStoreError>;
    async fn resolve_task(
        &self,
        task_id: &str,
        code: AttentionResolutionCode,
        at: DateTime<Utc>,
    ) -> Result<Option<AttentionRecord>, AttentionStoreError>;
    async fn reconcile_open(
        &self,
        at: DateTime<Utc>,
    ) -> Result<Vec<AttentionRecord>, AttentionStoreError>;
}

fn map_db(err: sea_orm::DbErr) -> AttentionStoreError {
    AttentionStoreError::Database(err.to_string())
}

fn is_unique_violation(err: &sea_orm::DbErr) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("unique constraint failed")
        || msg.contains("unique constraint")
        || msg.contains("2067") // SQLITE_CONSTRAINT_UNIQUE
        || msg.contains("1555") // SQLITE_CONSTRAINT_PRIMARYKEY
}

const INVARIANT_VIOLATION: &str = "attention row violates its persisted invariant";

fn model_to_record(
    row: delegation_attention_request::Model,
) -> Result<AttentionRecord, AttentionStoreError> {
    let status = row.status.as_str();
    let resolution = match row.resolution_code.as_deref() {
        None => None,
        Some(code) => Some(AttentionResolutionCode::from_storage(code)?),
    };

    match status {
        "open" => {
            if row.reply.is_some() || resolution.is_some() || row.resolved_at.is_some() {
                return Err(AttentionStoreError::Database(INVARIANT_VIOLATION.into()));
            }
        }
        "resolved" => {
            let Some(code) = resolution else {
                return Err(AttentionStoreError::Database(INVARIANT_VIOLATION.into()));
            };
            match code {
                AttentionResolutionCode::ParentReply => {
                    if row.reply.is_none() || row.resolved_at.is_none() {
                        return Err(AttentionStoreError::Database(INVARIANT_VIOLATION.into()));
                    }
                }
                _ => {
                    if row.reply.is_some() || row.resolved_at.is_none() {
                        return Err(AttentionStoreError::Database(INVARIANT_VIOLATION.into()));
                    }
                }
            }
        }
        _ => {
            return Err(AttentionStoreError::Database(INVARIANT_VIOLATION.into()));
        }
    }

    Ok(AttentionRecord {
        summary: AttentionRequestSummary {
            request_id: row.request_id,
            task_id: row.task_id,
            message: row.message,
            created_at: row.created_at,
        },
        parent_conversation_id: row.parent_conversation_id,
        child_conversation_id: row.child_conversation_id,
        child_tool_call_id: row.child_tool_call_id,
        reply: row.reply,
        resolution_code: resolution,
        resolved_at: row.resolved_at,
    })
}

fn classify_reply_outcome(record: AttentionRecord, reply: &str) -> AttentionResolveResult {
    match (&record.resolution_code, &record.reply) {
        (Some(AttentionResolutionCode::ParentReply), Some(stored)) if stored == reply => {
            AttentionResolveResult::Idempotent(record)
        }
        (Some(_), _) => AttentionResolveResult::Conflict(record),
        (None, _) => AttentionResolveResult::Missing,
    }
}

/// Production SQLite-backed attention store.
pub struct DbDelegationAttentionStore {
    db: Arc<AppDatabase>,
}

impl DbDelegationAttentionStore {
    pub fn new(db: Arc<AppDatabase>) -> Self {
        Self { db }
    }

    async fn load_required(
        &self,
        request_id: &str,
    ) -> Result<AttentionRecord, AttentionStoreError> {
        match self.find_by_id(request_id).await? {
            Some(record) => Ok(record),
            None => Err(AttentionStoreError::Database(
                "attention uniqueness conflict has no matching row".into(),
            )),
        }
    }

    async fn find_by_id(
        &self,
        request_id: &str,
    ) -> Result<Option<AttentionRecord>, AttentionStoreError> {
        let row = delegation_attention_request::Entity::find_by_id(request_id.to_string())
            .one(&self.db.conn)
            .await
            .map_err(map_db)?;
        row.map(model_to_record).transpose()
    }

    async fn find_by_task_and_tool(
        &self,
        task_id: &str,
        child_tool_call_id: &str,
    ) -> Result<Option<AttentionRecord>, AttentionStoreError> {
        let row = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(task_id))
            .filter(delegation_attention_request::Column::ChildToolCallId.eq(child_tool_call_id))
            .one(&self.db.conn)
            .await
            .map_err(map_db)?;
        row.map(model_to_record).transpose()
    }

    async fn find_open_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<AttentionRecord>, AttentionStoreError> {
        let row = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(task_id))
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .one(&self.db.conn)
            .await
            .map_err(map_db)?;
        row.map(model_to_record).transpose()
    }

    async fn load_child_edge(
        &self,
        request: &NewAttentionRequest,
    ) -> Result<Option<conversation::Model>, AttentionStoreError> {
        conversation::Entity::find_by_id(request.child_conversation_id)
            .one(&self.db.conn)
            .await
            .map_err(map_db)
    }

    async fn resolve_open_by_request_id(
        &self,
        request_id: &str,
        code: AttentionResolutionCode,
        reply: Option<String>,
        at: DateTime<Utc>,
    ) -> Result<Option<AttentionRecord>, AttentionStoreError> {
        let result = delegation_attention_request::Entity::update_many()
            .col_expr(
                delegation_attention_request::Column::Status,
                Expr::value("resolved"),
            )
            .col_expr(
                delegation_attention_request::Column::Reply,
                Expr::value(reply),
            )
            .col_expr(
                delegation_attention_request::Column::ResolutionCode,
                Expr::value(Some(code.as_str().to_string())),
            )
            .col_expr(
                delegation_attention_request::Column::ResolvedAt,
                Expr::value(Some(at)),
            )
            .filter(delegation_attention_request::Column::RequestId.eq(request_id))
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .exec(&self.db.conn)
            .await
            .map_err(map_db)?;

        if result.rows_affected > 0 {
            Ok(Some(self.load_required(request_id).await?))
        } else {
            Ok(None)
        }
    }
}

#[async_trait]
impl DelegationAttentionStore for DbDelegationAttentionStore {
    async fn open_or_recover(
        &self,
        request: NewAttentionRequest,
    ) -> Result<AttentionOpenResult, AttentionStoreError> {
        validate_attention_payload(&request.message)?;
        let request_id = uuid::Uuid::new_v4().to_string();
        let insert = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            r#"
            INSERT INTO delegation_attention_requests
              (request_id, task_id, parent_conversation_id, child_conversation_id,
               child_tool_call_id, status, message, reply, resolution_code,
               created_at, resolved_at)
            SELECT ?, c.delegation_call_id, ?, c.id, ?, 'open', ?, NULL, NULL, ?, NULL
            FROM conversation AS c
            WHERE c.id = ?
              AND c.delegation_call_id = ?
              AND c.parent_id = ?
              AND c.delegation_task_status = 'running'
            "#,
            vec![
                request_id.clone().into(),
                request.parent_conversation_id.into(),
                request.child_tool_call_id.clone().into(),
                request.message.clone().into(),
                request.created_at.into(),
                request.child_conversation_id.into(),
                request.task_id.clone().into(),
                request.parent_conversation_id.into(),
            ],
        );

        match self.db.conn.execute(insert).await {
            Ok(result) if result.rows_affected() == 1 => Ok(AttentionOpenResult::Opened(
                self.load_required(&request_id).await?,
            )),
            Ok(_) => match conversation::Entity::find_by_id(request.child_conversation_id)
                .one(&self.db.conn)
                .await
                .map_err(map_db)?
            {
                Some(child)
                    if child.delegation_call_id.as_deref() == Some(request.task_id.as_str())
                        && child.parent_id == Some(request.parent_conversation_id) =>
                {
                    Err(AttentionStoreError::TaskNotRunning)
                }
                _ => Err(AttentionStoreError::Unauthorized),
            },
            Err(error) if is_unique_violation(&error) => {
                if let Some(existing) = self
                    .find_by_task_and_tool(&request.task_id, &request.child_tool_call_id)
                    .await?
                {
                    return Ok(AttentionOpenResult::Recovered(existing));
                }
                if self.find_open_for_task(&request.task_id).await?.is_some() {
                    return Err(AttentionStoreError::AlreadyOpen);
                }
                // Covers the astronomically unlikely request UUID collision without
                // misreporting it as another open request.
                Err(AttentionStoreError::Database(
                    "attention uniqueness conflict has no matching row".into(),
                ))
            }
            Err(error) if is_transient_sqlite(&error.to_string()) => {
                // A terminal writer may have won while SQLite was retrying the write.
                // Re-read truth; never translate an unrelated busy error to terminal.
                match self.load_child_edge(&request).await? {
                    Some(child)
                        if child.delegation_task_status != Some(DelegationTaskStatus::Running) =>
                    {
                        Err(AttentionStoreError::TaskNotRunning)
                    }
                    _ => Err(map_db(error)),
                }
            }
            Err(error) => Err(map_db(error)),
        }
    }

    async fn list_open_for_tasks(
        &self,
        parent_conversation_id: i32,
        task_ids: &[String],
    ) -> Result<Vec<AttentionRequestSummary>, AttentionStoreError> {
        if task_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = delegation_attention_request::Entity::find()
            .filter(
                delegation_attention_request::Column::ParentConversationId
                    .eq(parent_conversation_id),
            )
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .filter(delegation_attention_request::Column::TaskId.is_in(task_ids.to_vec()))
            .order_by_asc(delegation_attention_request::Column::CreatedAt)
            .order_by_asc(delegation_attention_request::Column::RequestId)
            .all(&self.db.conn)
            .await
            .map_err(map_db)?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(model_to_record(row)?.summary);
        }
        Ok(out)
    }

    async fn wait_snapshot(
        &self,
        request_id: &str,
    ) -> Result<AttentionRecord, AttentionStoreError> {
        match self.find_by_id(request_id).await? {
            Some(record) => Ok(record),
            None => Err(AttentionStoreError::NotFound),
        }
    }

    async fn reply(
        &self,
        parent_conversation_id: i32,
        request_id: &str,
        reply: &str,
        at: DateTime<Utc>,
    ) -> Result<AttentionResolveResult, AttentionStoreError> {
        validate_attention_payload(reply)?;

        let existing = match self.find_by_id(request_id).await? {
            Some(record) => record,
            None => return Ok(AttentionResolveResult::Missing),
        };
        if existing.parent_conversation_id != parent_conversation_id {
            return Ok(AttentionResolveResult::Unauthorized);
        }
        if existing.resolution_code.is_some() {
            return Ok(classify_reply_outcome(existing, reply));
        }

        match self
            .resolve_open_by_request_id(
                request_id,
                AttentionResolutionCode::ParentReply,
                Some(reply.to_string()),
                at,
            )
            .await?
        {
            Some(record) => Ok(AttentionResolveResult::Resolved(record)),
            None => {
                let after = match self.find_by_id(request_id).await? {
                    Some(record) => record,
                    None => return Ok(AttentionResolveResult::Missing),
                };
                if after.parent_conversation_id != parent_conversation_id {
                    return Ok(AttentionResolveResult::Unauthorized);
                }
                Ok(classify_reply_outcome(after, reply))
            }
        }
    }

    async fn resolve_task(
        &self,
        task_id: &str,
        code: AttentionResolutionCode,
        at: DateTime<Utc>,
    ) -> Result<Option<AttentionRecord>, AttentionStoreError> {
        let Some(open) = self.find_open_for_task(task_id).await? else {
            return Ok(None);
        };
        self.resolve_open_by_request_id(&open.summary.request_id, code, None, at)
            .await
    }

    async fn reconcile_open(
        &self,
        at: DateTime<Utc>,
    ) -> Result<Vec<AttentionRecord>, AttentionStoreError> {
        let open_rows = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .all(&self.db.conn)
            .await
            .map_err(map_db)?;

        let mut won = Vec::new();
        for row in open_rows {
            let record = model_to_record(row)?;
            let child = conversation::Entity::find_by_id(record.child_conversation_id)
                .one(&self.db.conn)
                .await
                .map_err(map_db)?;
            let code = match child
                .as_ref()
                .and_then(|c| c.delegation_task_status.as_ref())
            {
                Some(DelegationTaskStatus::Running) => AttentionResolutionCode::HostRestarted,
                _ => AttentionResolutionCode::TaskTerminal,
            };
            if let Some(resolved) = self
                .resolve_open_by_request_id(&record.summary.request_id, code, None, at)
                .await?
            {
                won.push(resolved);
            }
        }
        Ok(won)
    }
}

/// Legacy-test / unconfigured stand-in that never opens attention rows.
#[derive(Debug, Default)]
pub struct NoopDelegationAttentionStore;

#[async_trait]
impl DelegationAttentionStore for NoopDelegationAttentionStore {
    async fn open_or_recover(
        &self,
        _request: NewAttentionRequest,
    ) -> Result<AttentionOpenResult, AttentionStoreError> {
        Err(AttentionStoreError::Database(
            "attention store is not configured".into(),
        ))
    }

    async fn list_open_for_tasks(
        &self,
        _parent_conversation_id: i32,
        _task_ids: &[String],
    ) -> Result<Vec<AttentionRequestSummary>, AttentionStoreError> {
        Ok(Vec::new())
    }

    async fn wait_snapshot(
        &self,
        _request_id: &str,
    ) -> Result<AttentionRecord, AttentionStoreError> {
        Err(AttentionStoreError::Database(
            "attention store is not configured".into(),
        ))
    }

    async fn reply(
        &self,
        _parent_conversation_id: i32,
        _request_id: &str,
        _reply: &str,
        _at: DateTime<Utc>,
    ) -> Result<AttentionResolveResult, AttentionStoreError> {
        Ok(AttentionResolveResult::Missing)
    }

    async fn resolve_task(
        &self,
        _task_id: &str,
        _code: AttentionResolutionCode,
        _at: DateTime<Utc>,
    ) -> Result<Option<AttentionRecord>, AttentionStoreError> {
        Ok(None)
    }

    async fn reconcile_open(
        &self,
        _at: DateTime<Utc>,
    ) -> Result<Vec<AttentionRecord>, AttentionStoreError> {
        Ok(Vec::new())
    }
}

/// In-memory attention store with the same conditional winner and ordering
/// semantics as the SQLite implementation (for focused Broker unit tests).
#[cfg(any(test, feature = "test-utils"))]
pub mod mock {
    use std::collections::HashMap;

    use tokio::sync::Mutex;

    use super::*;

    #[derive(Debug, Default)]
    pub struct MemoryDelegationAttentionStore {
        inner: Mutex<HashMap<String, AttentionRecord>>,
    }

    impl MemoryDelegationAttentionStore {
        pub fn new() -> Self {
            Self::default()
        }

        /// Record the authorized direct edge for a task. Open stays permissive
        /// for existing Join unit tests that call `open_or_recover` without
        /// seeding; Broker coordination tests call this for API parity with
        /// [`crate::acp::delegation::store::mock::MockTaskStore::seed_edge`].
        pub async fn seed_edge(
            &self,
            _task_id: &str,
            _parent_conversation_id: i32,
            _child_conversation_id: i32,
        ) {
            // Memory open does not gate on conversation-row edges; the Broker
            // loads child_conversation_id from the task store instead.
        }

        /// Test helper: any attention row for `task_id` (open or resolved).
        pub async fn record_for_task(&self, task_id: &str) -> Option<AttentionRecord> {
            self.inner
                .lock()
                .await
                .values()
                .find(|r| r.summary.task_id == task_id)
                .cloned()
        }

        /// Test helper: 1 when a request has a durable resolution, else 0.
        /// Memory store CAS guarantees at most one winner per request id.
        pub async fn resolution_winner_count(&self, request_id: &str) -> usize {
            self.inner
                .lock()
                .await
                .get(request_id)
                .map(|r| if r.resolution_code.is_some() { 1 } else { 0 })
                .unwrap_or(0)
        }

        fn find_by_task_and_tool_locked(
            rows: &HashMap<String, AttentionRecord>,
            task_id: &str,
            tool: &str,
        ) -> Option<AttentionRecord> {
            rows.values()
                .find(|r| r.summary.task_id == task_id && r.child_tool_call_id == tool)
                .cloned()
        }

        fn find_open_for_task_locked(
            rows: &HashMap<String, AttentionRecord>,
            task_id: &str,
        ) -> Option<AttentionRecord> {
            rows.values()
                .find(|r| r.summary.task_id == task_id && r.resolution_code.is_none())
                .cloned()
        }
    }

    #[async_trait]
    impl DelegationAttentionStore for MemoryDelegationAttentionStore {
        async fn open_or_recover(
            &self,
            request: NewAttentionRequest,
        ) -> Result<AttentionOpenResult, AttentionStoreError> {
            validate_attention_payload(&request.message)?;
            let mut rows = self.inner.lock().await;
            if let Some(existing) = Self::find_by_task_and_tool_locked(
                &rows,
                &request.task_id,
                &request.child_tool_call_id,
            ) {
                return Ok(AttentionOpenResult::Recovered(existing));
            }
            if Self::find_open_for_task_locked(&rows, &request.task_id).is_some() {
                return Err(AttentionStoreError::AlreadyOpen);
            }
            let request_id = uuid::Uuid::new_v4().to_string();
            let record = AttentionRecord {
                summary: AttentionRequestSummary {
                    request_id: request_id.clone(),
                    task_id: request.task_id,
                    message: request.message,
                    created_at: request.created_at,
                },
                parent_conversation_id: request.parent_conversation_id,
                child_conversation_id: request.child_conversation_id,
                child_tool_call_id: request.child_tool_call_id,
                reply: None,
                resolution_code: None,
                resolved_at: None,
            };
            rows.insert(request_id, record.clone());
            Ok(AttentionOpenResult::Opened(record))
        }

        async fn list_open_for_tasks(
            &self,
            parent_conversation_id: i32,
            task_ids: &[String],
        ) -> Result<Vec<AttentionRequestSummary>, AttentionStoreError> {
            let rows = self.inner.lock().await;
            let mut out: Vec<AttentionRequestSummary> = rows
                .values()
                .filter(|r| {
                    r.parent_conversation_id == parent_conversation_id
                        && r.resolution_code.is_none()
                        && task_ids.iter().any(|id| id == &r.summary.task_id)
                })
                .map(|r| r.summary.clone())
                .collect();
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.request_id.cmp(&b.request_id))
            });
            Ok(out)
        }

        async fn wait_snapshot(
            &self,
            request_id: &str,
        ) -> Result<AttentionRecord, AttentionStoreError> {
            self.inner
                .lock()
                .await
                .get(request_id)
                .cloned()
                .ok_or(AttentionStoreError::NotFound)
        }

        async fn reply(
            &self,
            parent_conversation_id: i32,
            request_id: &str,
            reply: &str,
            at: DateTime<Utc>,
        ) -> Result<AttentionResolveResult, AttentionStoreError> {
            validate_attention_payload(reply)?;
            let mut rows = self.inner.lock().await;
            let Some(existing) = rows.get(request_id).cloned() else {
                return Ok(AttentionResolveResult::Missing);
            };
            if existing.parent_conversation_id != parent_conversation_id {
                return Ok(AttentionResolveResult::Unauthorized);
            }
            if existing.resolution_code.is_some() {
                return Ok(classify_reply_outcome(existing, reply));
            }
            let mut updated = existing;
            updated.reply = Some(reply.to_string());
            updated.resolution_code = Some(AttentionResolutionCode::ParentReply);
            updated.resolved_at = Some(at);
            rows.insert(request_id.to_string(), updated.clone());
            Ok(AttentionResolveResult::Resolved(updated))
        }

        async fn resolve_task(
            &self,
            task_id: &str,
            code: AttentionResolutionCode,
            at: DateTime<Utc>,
        ) -> Result<Option<AttentionRecord>, AttentionStoreError> {
            let mut rows = self.inner.lock().await;
            let Some(open) = Self::find_open_for_task_locked(&rows, task_id) else {
                return Ok(None);
            };
            let mut updated = open;
            updated.reply = None;
            updated.resolution_code = Some(code);
            updated.resolved_at = Some(at);
            rows.insert(updated.summary.request_id.clone(), updated.clone());
            Ok(Some(updated))
        }

        async fn reconcile_open(
            &self,
            at: DateTime<Utc>,
        ) -> Result<Vec<AttentionRecord>, AttentionStoreError> {
            // Memory store has no child conversation table; treat every open
            // row as host-restarted (same as still-running tasks at startup).
            let mut rows = self.inner.lock().await;
            let open_ids: Vec<String> = rows
                .values()
                .filter(|r| r.resolution_code.is_none())
                .map(|r| r.summary.request_id.clone())
                .collect();
            let mut won = Vec::new();
            for id in open_ids {
                if let Some(record) = rows.get_mut(&id) {
                    if record.resolution_code.is_none() {
                        record.resolution_code = Some(AttentionResolutionCode::HostRestarted);
                        record.resolved_at = Some(at);
                        record.reply = None;
                        won.push(record.clone());
                    }
                }
            }
            Ok(won)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use sea_orm::{ConnectionTrait, Database, Statement};
    use tokio::sync::Barrier;

    use crate::acp::delegation::spawner::DelegationLink;
    use crate::acp::delegation::store::{
        DbDelegationTaskStore, DelegationTaskStore, TerminalTaskWrite,
    };
    use crate::db::entities::conversation::ConversationStatus;
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_disk_db, fresh_in_memory_db, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::AgentType;

    struct Fixture {
        db: Arc<AppDatabase>,
        parent: conversation::Model,
        child: conversation::Model,
        folder_id: i32,
    }

    impl Fixture {
        async fn new() -> Self {
            let db = Arc::new(fresh_in_memory_db().await);
            let folder_id = seed_folder(&db, "/tmp/codeg-attention").await;
            let parent = conversation_service::create(
                &db.conn,
                folder_id,
                AgentType::ClaudeCode,
                Some("parent".into()),
                None,
            )
            .await
            .expect("parent");
            let child = conversation_service::create_with_delegation(
                &db.conn,
                folder_id,
                AgentType::Codex,
                Some("child".into()),
                None,
                Some(DelegationLink {
                    parent_conversation_id: parent.id,
                    parent_tool_use_id: "tu-task-1".into(),
                    delegation_call_id: "task-1".into(),
                }),
            )
            .await
            .expect("child");
            Self {
                db,
                parent,
                child,
                folder_id,
            }
        }

        fn request(&self, tool_call_id: &str, message: &str) -> NewAttentionRequest {
            NewAttentionRequest {
                task_id: "task-1".into(),
                parent_conversation_id: self.parent.id,
                child_conversation_id: self.child.id,
                child_tool_call_id: tool_call_id.into(),
                message: message.into(),
                created_at: Utc::now(),
            }
        }

        async fn request_for(
            &self,
            task_id: &str,
            tool_call_id: &str,
            message: &str,
        ) -> NewAttentionRequest {
            let child = conversation_service::get_by_delegation_call_id(&self.db.conn, task_id)
                .await
                .expect("load by task")
                .expect("child exists");
            NewAttentionRequest {
                task_id: task_id.into(),
                parent_conversation_id: self.parent.id,
                child_conversation_id: child.id,
                child_tool_call_id: tool_call_id.into(),
                message: message.into(),
                created_at: Utc::now(),
            }
        }

        async fn add_running_child(&self, task_id: &str) -> String {
            conversation_service::create_with_delegation(
                &self.db.conn,
                self.folder_id,
                AgentType::Codex,
                Some(task_id.into()),
                None,
                Some(DelegationLink {
                    parent_conversation_id: self.parent.id,
                    parent_tool_use_id: format!("tu-{task_id}"),
                    delegation_call_id: task_id.into(),
                }),
            )
            .await
            .expect("add child");
            task_id.to_string()
        }

        async fn set_task_terminal(&self, task_id: &str) {
            let store = DbDelegationTaskStore::new(self.db.clone());
            store
                .settle(
                    task_id,
                    TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
                )
                .await
                .expect("settle terminal");
        }

        async fn set_task_terminal_without_attention(&self, task_id: &str) {
            // Only the conversation task-status CAS — deliberately bypasses
            // the Broker attention hook to simulate a crash between writes.
            self.set_task_terminal(task_id).await;
        }
    }

    #[tokio::test]
    async fn open_recover_reply_and_conflict_preserve_one_durable_winner() {
        let fixture = Fixture::new().await;
        let store = DbDelegationAttentionStore::new(fixture.db.clone());
        let new = fixture.request("tool-call-1", "Choose A or B");

        let first = store.open_or_recover(new.clone()).await.unwrap();
        let replay = store.open_or_recover(new).await.unwrap();
        assert_eq!(
            first.record().summary.request_id.as_str(),
            replay.record().summary.request_id.as_str()
        );

        let won = store
            .reply(
                fixture.parent.id,
                &first.record().summary.request_id,
                "Use A",
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(won, AttentionResolveResult::Resolved(_)));
        let same = store
            .reply(
                fixture.parent.id,
                &first.record().summary.request_id,
                "Use A",
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(same, AttentionResolveResult::Idempotent(_)));
        let conflict = store
            .reply(
                fixture.parent.id,
                &first.record().summary.request_id,
                "Use B",
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(conflict, AttentionResolveResult::Conflict(_)));
    }

    #[tokio::test]
    async fn rejects_foreign_edges_second_open_and_oversized_payloads() {
        let fixture = Fixture::new().await;
        let store = DbDelegationAttentionStore::new(fixture.db.clone());
        let mut foreign = fixture.request("tc-1", "question");
        foreign.parent_conversation_id += 100;
        assert!(matches!(
            store.open_or_recover(foreign).await.unwrap_err(),
            AttentionStoreError::Unauthorized
        ));

        store
            .open_or_recover(fixture.request("tc-1", "first"))
            .await
            .unwrap();
        assert!(matches!(
            store
                .open_or_recover(fixture.request("tc-2", "second"))
                .await
                .unwrap_err(),
            AttentionStoreError::AlreadyOpen
        ));
        assert!(matches!(
            validate_attention_payload(&"x".repeat(ATTENTION_PAYLOAD_MAX_BYTES + 1)),
            Err(AttentionStoreError::PayloadTooLarge)
        ));

        fixture.set_task_terminal("task-1").await;
        assert!(matches!(
            store
                .open_or_recover(fixture.request("tc-after-terminal", "late"))
                .await
                .unwrap_err(),
            AttentionStoreError::TaskNotRunning
        ));
    }

    #[tokio::test]
    async fn startup_reconciliation_distinguishes_running_from_already_terminal_tasks() {
        let fixture = Fixture::new().await;
        let store = DbDelegationAttentionStore::new(fixture.db.clone());
        let running = store
            .open_or_recover(fixture.request("tc-run", "running"))
            .await
            .unwrap()
            .into_record();
        let terminal_task_id = fixture.add_running_child("task-terminal").await;
        let terminal_req = store
            .open_or_recover(
                fixture
                    .request_for(&terminal_task_id, "tc-done", "done")
                    .await,
            )
            .await
            .unwrap()
            .into_record();
        // Simulate a crash after task CAS but before attention closure. Going
        // through the normal Broker terminal path would close the row and would
        // not exercise startup reconciliation.
        fixture
            .set_task_terminal_without_attention(&terminal_task_id)
            .await;

        let reconciled = store.reconcile_open(Utc::now()).await.unwrap();
        assert_eq!(reconciled.len(), 2);
        assert_eq!(
            store
                .wait_snapshot(&running.summary.request_id)
                .await
                .unwrap()
                .resolution_code,
            Some(AttentionResolutionCode::HostRestarted)
        );
        assert_eq!(
            store
                .wait_snapshot(&terminal_req.summary.request_id)
                .await
                .unwrap()
                .resolution_code,
            Some(AttentionResolutionCode::TaskTerminal)
        );
    }

    #[tokio::test]
    async fn blank_payload_and_preserved_nonblank_bytes() {
        assert!(matches!(
            validate_attention_payload(""),
            Err(AttentionStoreError::BlankPayload)
        ));
        assert!(matches!(
            validate_attention_payload("   \n\t  "),
            Err(AttentionStoreError::BlankPayload)
        ));
        let padded = "  keep me  ";
        validate_attention_payload(padded).unwrap();

        let fixture = Fixture::new().await;
        let store = DbDelegationAttentionStore::new(fixture.db.clone());
        let opened = store
            .open_or_recover(fixture.request("tc-pad", padded))
            .await
            .unwrap()
            .into_record();
        assert_eq!(opened.summary.message, padded);

        let reply = "  answer  ";
        let resolved = store
            .reply(
                fixture.parent.id,
                &opened.summary.request_id,
                reply,
                Utc::now(),
            )
            .await
            .unwrap();
        match resolved {
            AttentionResolveResult::Resolved(r) => assert_eq!(r.reply.as_deref(), Some(reply)),
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn wait_snapshot_not_found_is_distinct_from_database() {
        let fixture = Fixture::new().await;
        let store = DbDelegationAttentionStore::new(fixture.db.clone());
        assert!(matches!(
            store.wait_snapshot("missing-request").await.unwrap_err(),
            AttentionStoreError::NotFound
        ));
    }

    #[tokio::test]
    async fn list_open_for_tasks_orders_by_created_then_request_id() {
        let fixture = Fixture::new().await;
        let store = DbDelegationAttentionStore::new(fixture.db.clone());
        let t_a = fixture.add_running_child("task-a").await;
        let t_b = fixture.add_running_child("task-b").await;
        let t0 = Utc::now();
        let mut req_b = fixture.request_for(&t_b, "tc-b", "B").await;
        req_b.created_at = t0;
        let mut req_a = fixture.request_for(&t_a, "tc-a", "A").await;
        req_a.created_at = t0;

        store.open_or_recover(req_b).await.unwrap();
        store.open_or_recover(req_a).await.unwrap();

        let listed = store
            .list_open_for_tasks(fixture.parent.id, &["task-a".into(), "task-b".into()])
            .await
            .unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed[0].request_id <= listed[1].request_id);
        assert_eq!(listed[0].created_at, listed[1].created_at);
    }

    fn corrupt_model(
        status: &str,
        reply: Option<&str>,
        resolution_code: Option<&str>,
        resolved_at: Option<DateTime<Utc>>,
    ) -> delegation_attention_request::Model {
        delegation_attention_request::Model {
            request_id: "corrupt".into(),
            task_id: "task-1".into(),
            parent_conversation_id: 1,
            child_conversation_id: 2,
            child_tool_call_id: "tc".into(),
            status: status.into(),
            message: "payload-secret".into(),
            reply: reply.map(str::to_string),
            resolution_code: resolution_code.map(str::to_string),
            created_at: Utc::now(),
            resolved_at,
        }
    }

    #[test]
    fn model_to_record_rejects_each_invariant_violation() {
        let at = Utc::now();
        let cases = [
            corrupt_model("open", Some("x"), None, None),
            corrupt_model("open", None, Some("parent_reply"), None),
            corrupt_model("open", None, None, Some(at)),
            corrupt_model("resolved", None, Some("parent_reply"), Some(at)),
            corrupt_model("resolved", Some("x"), Some("parent_reply"), None),
            corrupt_model("resolved", Some("x"), Some("task_terminal"), Some(at)),
            corrupt_model("resolved", None, Some("task_terminal"), None),
            corrupt_model("bogus", None, None, None),
            corrupt_model("resolved", None, Some("not_a_code"), Some(at)),
        ];

        for (i, model) in cases.into_iter().enumerate() {
            let err = model_to_record(model).unwrap_err();
            match err {
                AttentionStoreError::Database(msg) => {
                    assert!(
                        msg.contains(INVARIANT_VIOLATION)
                            || msg.contains("unknown resolution code"),
                        "case {i}: unexpected db message {msg}"
                    );
                    // Fixed privacy: never embed row content.
                    assert!(
                        !msg.contains("payload-secret"),
                        "case {i}: leaked message content"
                    );
                    assert!(
                        !msg.contains("not_a_code"),
                        "case {i}: leaked resolution code"
                    );
                }
                other => panic!("case {i}: expected Database, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn noop_store_legacy_semantics() {
        let store = NoopDelegationAttentionStore;
        assert!(matches!(
            store
                .open_or_recover(NewAttentionRequest {
                    task_id: "t".into(),
                    parent_conversation_id: 1,
                    child_conversation_id: 2,
                    child_tool_call_id: "tc".into(),
                    message: "q".into(),
                    created_at: Utc::now(),
                })
                .await
                .unwrap_err(),
            AttentionStoreError::Database(msg) if msg.contains("not configured")
        ));
        assert!(store.list_open_for_tasks(1, &[]).await.unwrap().is_empty());
        assert!(matches!(
            store.wait_snapshot("x").await.unwrap_err(),
            AttentionStoreError::Database(_)
        ));
        assert!(matches!(
            store.reply(1, "x", "y", Utc::now()).await.unwrap(),
            AttentionResolveResult::Missing
        ));
        assert!(store
            .resolve_task("t", AttentionResolutionCode::TaskTerminal, Utc::now())
            .await
            .unwrap()
            .is_none());
        assert!(store.reconcile_open(Utc::now()).await.unwrap().is_empty());
    }

    async fn open_wal_pool(path: &std::path::Path) -> AppDatabase {
        use sea_orm::ConnectOptions;
        use std::time::Duration;

        let url = format!("sqlite:{}?mode=rwc", path.to_string_lossy());
        let mut opts = ConnectOptions::new(url);
        opts.max_connections(4)
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

    #[tokio::test]
    async fn attention_open_racing_terminal_cas() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Migrate once; subsequent pools reopen the same WAL file.
        let _migrate = fresh_disk_db(dir.path()).await;
        let path = dir.path().join("source.db");

        const ITERATIONS: usize = 64;
        for i in 0..ITERATIONS {
            let pool_a = Arc::new(open_wal_pool(&path).await);
            let pool_b = Arc::new(open_wal_pool(&path).await);

            let folder_id = seed_folder(&pool_a, &format!("/tmp/attention-race-{i}")).await;
            let parent = conversation_service::create(
                &pool_a.conn,
                folder_id,
                AgentType::ClaudeCode,
                Some(format!("parent-{i}")),
                None,
            )
            .await
            .expect("parent");
            let task_id = format!("task-race-{i}");
            let child = conversation_service::create_with_delegation(
                &pool_a.conn,
                folder_id,
                AgentType::Codex,
                Some(format!("child-{i}")),
                None,
                Some(DelegationLink {
                    parent_conversation_id: parent.id,
                    parent_tool_use_id: format!("tu-{i}"),
                    delegation_call_id: task_id.clone(),
                }),
            )
            .await
            .expect("child");

            let attention_a = DbDelegationAttentionStore::new(pool_a.clone());
            let attention_b = DbDelegationAttentionStore::new(pool_b.clone());
            let task_store_b = DbDelegationTaskStore::new(pool_b.clone());

            let request = NewAttentionRequest {
                task_id: task_id.clone(),
                parent_conversation_id: parent.id,
                child_conversation_id: child.id,
                child_tool_call_id: format!("tc-race-{i}"),
                message: "race question".into(),
                created_at: Utc::now(),
            };

            let barrier = Arc::new(Barrier::new(2));
            let barrier_open = barrier.clone();
            let barrier_term = barrier.clone();
            let req_clone = request.clone();
            let task_id_term = task_id.clone();

            let (open_res, _term_res) = tokio::join!(
                async move {
                    barrier_open.wait().await;
                    attention_a.open_or_recover(req_clone).await
                },
                async move {
                    barrier_term.wait().await;
                    let settle = task_store_b
                        .settle(
                            &task_id_term,
                            TerminalTaskWrite::completed(
                                Utc::now(),
                                ConversationStatus::PendingReview,
                            ),
                        )
                        .await;
                    // Terminal path: if an open attention row exists, resolve it.
                    let _ = attention_b
                        .resolve_task(
                            &task_id_term,
                            AttentionResolutionCode::TaskTerminal,
                            Utc::now(),
                        )
                        .await;
                    settle
                }
            );

            // Re-read durable truth on a fresh connection.
            let check = Arc::new(open_wal_pool(&path).await);
            let attention_check = DbDelegationAttentionStore::new(check.clone());
            let task_check = DbDelegationTaskStore::new(check.clone());
            let task_row = task_check
                .load(&task_id)
                .await
                .expect("load task")
                .expect("task exists");
            assert_ne!(
                task_row.status,
                crate::acp::delegation::types::TaskStatus::Running,
                "iter {i}: task must be terminal after race"
            );

            let open_after = delegation_attention_request::Entity::find()
                .filter(delegation_attention_request::Column::TaskId.eq(task_id.clone()))
                .filter(delegation_attention_request::Column::Status.eq("open"))
                .one(&check.conn)
                .await
                .expect("query open");
            assert!(
                open_after.is_none(),
                "iter {i}: terminal child must not keep an open attention row"
            );

            match open_res {
                Ok(opened) => {
                    let snap = attention_check
                        .wait_snapshot(&opened.record().summary.request_id)
                        .await
                        .expect("snapshot after open win");
                    assert_eq!(
                        snap.resolution_code,
                        Some(AttentionResolutionCode::TaskTerminal),
                        "iter {i}: insert-first path must be resolved by terminal path"
                    );
                }
                Err(AttentionStoreError::TaskNotRunning) => {
                    // Terminal CAS won first — legal ordering.
                }
                Err(other) => panic!("iter {i}: unexpected open error: {other:?}"),
            }
        }
    }
}
