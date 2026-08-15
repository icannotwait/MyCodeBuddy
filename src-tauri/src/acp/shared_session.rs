use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{
    acp::{
        session_attach::SessionAttachMode,
        types::{ConnectionStatus, PromptInputBlock},
    },
    auto_title::{ConnectionPurpose, PromptCaptureContext},
    models::agent::AgentType,
};

pub const MAX_WAITING_PROMPTS: usize = 64;
pub const MAX_WAITING_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_ACTIVE_LEASES: usize = 256;
pub const MAX_CONNECT_LEDGER_ENTRIES: usize = 4_096;
pub const MAX_PROMPT_LEDGER_ENTRIES: usize = 65_536;
pub const MAX_EXPIRED_LEASE_TOMBSTONES: usize = 1_024;
pub const MAX_REPLACED_CONNECTION_TOMBSTONES: usize = 4_096;
pub const MAX_CLIENT_LABEL_LEN: usize = 128;
pub const MAX_QUEUE_VISIBLE_TEXT_CHARS: usize = 512;
pub const DEFAULT_CLIENT_LEASE_TTL: Duration = Duration::from_secs(90);

const STABLE_SHARED_SESSION_ERROR_CODES: &[&str] = &[
    "shared_session_config_conflict",
    "shared_session_protocol_required",
    "shared_session_generation_stale",
    "shared_session_closing",
    "shared_session_cleanup_in_progress",
    "client_lease_missing",
    "client_lease_expired",
    "client_lease_capacity_exceeded",
    "connect_idempotency_capacity_exceeded",
    "prompt_idempotency_capacity_exceeded",
    "prompt_queue_full",
    "idempotency_key_conflict",
    "queue_item_not_found",
    "queue_item_already_dispatching",
    "interaction_already_resolved",
    "stale_turn",
    "session_unavailable",
    "companion_initialization_failed",
    "shared_session_conversation_key_conflict",
    "invalid_shared_session_field",
];

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum SharedSessionKey {
    Conversation(i32),
    ExternalSession {
        agent_type: AgentType,
        normalized_working_dir: String,
        external_session_id: String,
    },
    Ephemeral(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedConfigConflictKind {
    AgentType,
    WorkingDirectory,
    ExternalSession,
    AttachMode,
    DelegationRoute,
    TerminalShell,
    Purpose,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SharedLaunchIdentity {
    pub agent_type: AgentType,
    pub working_dir_fingerprint: String,
    pub external_session_id: Option<String>,
    pub attach_mode: SessionAttachMode,
    pub route_fingerprint: String,
    pub terminal_shell_fingerprint: String,
    pub purpose: ConnectionPurpose,
}

impl SharedLaunchIdentity {
    #[cfg(test)]
    fn fixture() -> Self {
        Self {
            agent_type: AgentType::Codex,
            working_dir_fingerprint: "cwd-fixture".into(),
            external_session_id: None,
            attach_mode: SessionAttachMode::Default,
            route_fingerprint: "route-fixture".into(),
            terminal_shell_fingerprint: "shell-fixture".into(),
            purpose: ConnectionPurpose::User,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum SharedSessionPhase {
    Reserved,
    Bootstrapping,
    Ready,
    Failed {
        error_code: String,
        cleanup_complete: bool,
    },
    Closing,
}

impl SharedSessionPhase {
    pub fn connection_status(&self) -> ConnectionStatus {
        match self {
            Self::Reserved | Self::Bootstrapping => ConnectionStatus::Connecting,
            Self::Ready => ConnectionStatus::Connected,
            Self::Failed { .. } => ConnectionStatus::Error,
            Self::Closing => ConnectionStatus::Disconnected,
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedMutationGuard {
    pub connection_id: String,
    pub generation: u64,
    pub lease_id: String,
}

impl fmt::Debug for SharedMutationGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedMutationGuard")
            .field("connection_id", &self.connection_id)
            .field("generation", &self.generation)
            .field("lease_id", &"***")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedDisposition {
    Created,
    Attached,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedQueuedPromptState {
    Queued,
    Dispatching,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedQueuedPromptSummary {
    pub queue_item_id: String,
    pub enqueue_seq: u64,
    pub client_message_id: String,
    pub visible_text: Option<String>,
    pub visible_text_truncated: bool,
    pub attachment_count: u32,
    pub submitted_at: DateTime<Utc>,
    pub state: SharedQueuedPromptState,
}

impl SharedQueuedPromptSummary {
    #[allow(clippy::too_many_arguments)]
    pub fn from_prompt(
        queue_item_id: String,
        enqueue_seq: u64,
        client_message_id: String,
        blocks: &[PromptInputBlock],
        _capture_context: Option<&PromptCaptureContext>,
        submitted_at: DateTime<Utc>,
        state: SharedQueuedPromptState,
    ) -> Self {
        let mut text = String::new();
        let mut attachment_count = 0_u32;
        for block in blocks {
            match block {
                PromptInputBlock::Text { text: block_text } => text.push_str(block_text),
                PromptInputBlock::Image { .. }
                | PromptInputBlock::Resource { .. }
                | PromptInputBlock::ResourceLink { .. } => {
                    attachment_count = attachment_count.saturating_add(1);
                }
            }
        }

        let char_count = text.chars().count();
        let visible_text_truncated = char_count > MAX_QUEUE_VISIBLE_TEXT_CHARS;
        let visible_text = if text.is_empty() {
            None
        } else if visible_text_truncated {
            Some(text.chars().take(MAX_QUEUE_VISIBLE_TEXT_CHARS).collect())
        } else {
            Some(text)
        };

        Self {
            queue_item_id,
            enqueue_seq,
            client_message_id,
            visible_text,
            visible_text_truncated,
            attachment_count,
            submitted_at,
            state,
        }
    }
}

impl fmt::Debug for SharedQueuedPromptSummary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedQueuedPromptSummary")
            .field("queue_item_id", &self.queue_item_id)
            .field("enqueue_seq", &self.enqueue_seq)
            .field("client_message_id", &self.client_message_id)
            .field("state", &self.state)
            .field("attachment_count", &self.attachment_count)
            .field("visible_text_present", &self.visible_text.is_some())
            .field("visible_text_truncated", &self.visible_text_truncated)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedActiveTurnProjection {
    pub turn_id: String,
    pub queue_item_id: String,
    pub enqueue_seq: u64,
    pub client_message_id: String,
    pub stop_requested: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedTurnOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone)]
pub struct SharedSessionAttachment {
    pub connection_id: String,
    pub generation: u64,
    pub lease_id: String,
    pub lease_expires_at: DateTime<Utc>,
    pub disposition: SharedDisposition,
    pub phase: SharedSessionPhase,
}

impl fmt::Debug for SharedSessionAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedSessionAttachment")
            .field("connection_id", &self.connection_id)
            .field("generation", &self.generation)
            .field("lease_id", &"***")
            .field("lease_expires_at", &self.lease_expires_at)
            .field("disposition", &self.disposition)
            .field("phase", &self.phase)
            .finish()
    }
}

#[derive(Clone)]
pub struct SharedReserveOutcome {
    pub attachment: SharedSessionAttachment,
    pub created: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SharedSessionProjection {
    pub generation: u64,
    pub phase: SharedSessionPhase,
    pub queue: Vec<SharedQueuedPromptSummary>,
    pub active_turn: Option<SharedActiveTurnProjection>,
    pub lease_expires_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for SharedSessionProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedSessionProjection")
            .field("generation", &self.generation)
            .field("phase", &self.phase)
            .field("queue_count", &self.queue.len())
            .field("queue", &self.queue)
            .field("active_turn", &self.active_turn)
            .field("lease_expires_at", &self.lease_expires_at)
            .finish()
    }
}

#[derive(Clone)]
pub struct SharedReserveRequest {
    pub key: SharedSessionKey,
    pub connection_id: String,
    pub launch_identity: SharedLaunchIdentity,
    pub client_instance_id: String,
    pub device_id: String,
    pub request_id: String,
    pub retry_failed_generation: Option<u64>,
    pub now: tokio::time::Instant,
    pub now_utc: DateTime<Utc>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SharedSessionError {
    #[error("shared session configuration conflicts with connection {connection_id}")]
    ConfigConflict {
        connection_id: String,
        conflict_kind: SharedConfigConflictKind,
    },
    #[error("shared session fencing is required")]
    ProtocolRequired,
    #[error("shared session generation is stale")]
    GenerationStale,
    #[error("shared session is closing")]
    Closing,
    #[error("shared session cleanup is in progress")]
    CleanupInProgress,
    #[error("client lease is missing")]
    LeaseMissing,
    #[error("client lease has expired")]
    LeaseExpired,
    #[error("client lease capacity is exhausted")]
    ClientLeaseCapacityExceeded,
    #[error("connect idempotency capacity is exhausted")]
    ConnectLedgerCapacityExceeded,
    #[error("prompt idempotency capacity is exhausted")]
    PromptLedgerCapacityExceeded,
    #[error("prompt queue is full")]
    PromptQueueFull,
    #[error("idempotency key was reused with different content")]
    IdempotencyKeyConflict,
    #[error("queued prompt was not found")]
    QueueItemNotFound,
    #[error("queued prompt is already dispatching")]
    QueueItemAlreadyDispatching,
    #[error("interaction was already resolved")]
    InteractionAlreadyResolved,
    #[error("turn id is stale")]
    StaleTurn,
    #[error("shared session is unavailable")]
    SessionUnavailable,
    #[error("required Codeg companion initialization failed")]
    CompanionInitializationFailed,
    #[error("conversation is already bound to another shared session")]
    ConversationKeyConflict,
    #[error("invalid shared-session field: {field}")]
    InvalidField { field: &'static str },
}

impl SharedSessionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ConfigConflict { .. } => "shared_session_config_conflict",
            Self::ProtocolRequired => "shared_session_protocol_required",
            Self::GenerationStale => "shared_session_generation_stale",
            Self::Closing => "shared_session_closing",
            Self::CleanupInProgress => "shared_session_cleanup_in_progress",
            Self::LeaseMissing => "client_lease_missing",
            Self::LeaseExpired => "client_lease_expired",
            Self::ClientLeaseCapacityExceeded => "client_lease_capacity_exceeded",
            Self::ConnectLedgerCapacityExceeded => "connect_idempotency_capacity_exceeded",
            Self::PromptLedgerCapacityExceeded => "prompt_idempotency_capacity_exceeded",
            Self::PromptQueueFull => "prompt_queue_full",
            Self::IdempotencyKeyConflict => "idempotency_key_conflict",
            Self::QueueItemNotFound => "queue_item_not_found",
            Self::QueueItemAlreadyDispatching => "queue_item_already_dispatching",
            Self::InteractionAlreadyResolved => "interaction_already_resolved",
            Self::StaleTurn => "stale_turn",
            Self::SessionUnavailable => "session_unavailable",
            Self::CompanionInitializationFailed => "companion_initialization_failed",
            Self::ConversationKeyConflict => "shared_session_conversation_key_conflict",
            Self::InvalidField { .. } => "invalid_shared_session_field",
        }
    }

    fn is_capacity_error(&self) -> bool {
        matches!(
            self,
            Self::ClientLeaseCapacityExceeded
                | Self::ConnectLedgerCapacityExceeded
                | Self::PromptLedgerCapacityExceeded
                | Self::PromptQueueFull
        )
    }
}

pub fn validate_client_label(field: &'static str, value: &str) -> Result<(), SharedSessionError> {
    if !(1..=MAX_CLIENT_LABEL_LEN).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(SharedSessionError::InvalidField { field });
    }
    Ok(())
}

fn validate_failure_code(value: &str) -> Result<(), SharedSessionError> {
    if !STABLE_SHARED_SESSION_ERROR_CODES.contains(&value) {
        return Err(SharedSessionError::InvalidField {
            field: "error_code",
        });
    }
    Ok(())
}

#[derive(Default)]
pub struct SharedSessionMetrics {
    created_total: AtomicU64,
    attached_total: AtomicU64,
    live_sessions: AtomicU64,
    active_leases: AtomicU64,
    bootstrap_ready_total: AtomicU64,
    bootstrap_failed_total: StdMutex<BTreeMap<String, u64>>,
    bootstrap_duration_ms_total: AtomicU64,
    bootstrap_duration_samples: AtomicU64,
    waiting_prompts: AtomicU64,
    waiting_bytes: AtomicU64,
    enqueue_total: AtomicU64,
    cancel_total: AtomicU64,
    dispatch_total: AtomicU64,
    capacity_rejected_total: AtomicU64,
    queue_item_failed_total: AtomicU64,
    interaction_winner_total: AtomicU64,
    interaction_stale_total: AtomicU64,
    stale_stop_total: AtomicU64,
    lease_expired_total: AtomicU64,
    lease_released_total: AtomicU64,
    idle_candidate_total: AtomicU64,
    idle_cas_lost_total: AtomicU64,
    idle_reclaimed_total: AtomicU64,
    cleanup_duration_ms_total: AtomicU64,
    cleanup_duration_samples: AtomicU64,
    cleanup_incomplete_total: AtomicU64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SharedSessionMetricsSnapshot {
    pub created_total: u64,
    pub attached_total: u64,
    pub live_sessions: u64,
    pub active_leases: u64,
    pub bootstrap_ready_total: u64,
    pub bootstrap_failed_total: BTreeMap<String, u64>,
    pub bootstrap_duration_ms_total: u64,
    pub bootstrap_duration_samples: u64,
    pub waiting_prompts: u64,
    pub waiting_bytes: u64,
    pub enqueue_total: u64,
    pub cancel_total: u64,
    pub dispatch_total: u64,
    pub capacity_rejected_total: u64,
    pub queue_item_failed_total: u64,
    pub interaction_winner_total: u64,
    pub interaction_stale_total: u64,
    pub stale_stop_total: u64,
    pub lease_expired_total: u64,
    pub lease_released_total: u64,
    pub idle_candidate_total: u64,
    pub idle_cas_lost_total: u64,
    pub idle_reclaimed_total: u64,
    pub cleanup_duration_ms_total: u64,
    pub cleanup_duration_samples: u64,
    pub cleanup_incomplete_total: u64,
}

impl SharedSessionMetrics {
    pub fn snapshot(&self) -> SharedSessionMetricsSnapshot {
        let bootstrap_failed_total = self
            .bootstrap_failed_total
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        SharedSessionMetricsSnapshot {
            created_total: load(&self.created_total),
            attached_total: load(&self.attached_total),
            live_sessions: load(&self.live_sessions),
            active_leases: load(&self.active_leases),
            bootstrap_ready_total: load(&self.bootstrap_ready_total),
            bootstrap_failed_total,
            bootstrap_duration_ms_total: load(&self.bootstrap_duration_ms_total),
            bootstrap_duration_samples: load(&self.bootstrap_duration_samples),
            waiting_prompts: load(&self.waiting_prompts),
            waiting_bytes: load(&self.waiting_bytes),
            enqueue_total: load(&self.enqueue_total),
            cancel_total: load(&self.cancel_total),
            dispatch_total: load(&self.dispatch_total),
            capacity_rejected_total: load(&self.capacity_rejected_total),
            queue_item_failed_total: load(&self.queue_item_failed_total),
            interaction_winner_total: load(&self.interaction_winner_total),
            interaction_stale_total: load(&self.interaction_stale_total),
            stale_stop_total: load(&self.stale_stop_total),
            lease_expired_total: load(&self.lease_expired_total),
            lease_released_total: load(&self.lease_released_total),
            idle_candidate_total: load(&self.idle_candidate_total),
            idle_cas_lost_total: load(&self.idle_cas_lost_total),
            idle_reclaimed_total: load(&self.idle_reclaimed_total),
            cleanup_duration_ms_total: load(&self.cleanup_duration_ms_total),
            cleanup_duration_samples: load(&self.cleanup_duration_samples),
            cleanup_incomplete_total: load(&self.cleanup_incomplete_total),
        }
    }

    fn record_connect(&self, created: bool) {
        if created {
            self.created_total.fetch_add(1, Ordering::Relaxed);
        } else {
            self.attached_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn add_live_session(&self) {
        self.live_sessions.fetch_add(1, Ordering::Relaxed);
    }

    fn add_active_leases(&self, count: usize) {
        self.active_leases
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    fn remove_active_leases(&self, count: usize) {
        let count = count as u64;
        let _ = self
            .active_leases
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(count))
            });
    }

    fn record_capacity_rejection(&self) {
        self.capacity_rejected_total.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
pub struct SharedSessionBroker {
    index: Arc<Mutex<SharedSessionIndex>>,
    metrics: Arc<SharedSessionMetrics>,
    lease_ttl: Duration,
    limits: BrokerLimits,
}

impl Default for SharedSessionBroker {
    fn default() -> Self {
        Self {
            index: Arc::new(Mutex::new(SharedSessionIndex::default())),
            metrics: Arc::new(SharedSessionMetrics::default()),
            lease_ttl: DEFAULT_CLIENT_LEASE_TTL,
            limits: BrokerLimits::default(),
        }
    }
}

impl SharedSessionBroker {
    pub fn metrics(&self) -> &SharedSessionMetrics {
        &self.metrics
    }

    #[cfg(test)]
    fn with_limits_for_test(max_active_leases: usize, max_connect_ledger_entries: usize) -> Self {
        Self {
            limits: BrokerLimits {
                max_active_leases,
                max_connect_ledger_entries,
            },
            ..Self::default()
        }
    }

    pub async fn reserve_or_attach(
        &self,
        request: SharedReserveRequest,
    ) -> Result<SharedReserveOutcome, SharedSessionError> {
        validate_client_label("device_id", &request.device_id)?;
        validate_client_label("client_instance_id", &request.client_instance_id)?;
        validate_client_label("request_id", &request.request_id)?;

        loop {
            let lookup = {
                let mut index = self.index.lock().await;
                if let Some(record) = index.sessions.get(&request.key) {
                    ReserveLookup::Existing(record.clone())
                } else {
                    let mut initial = SharedSessionRecord::reserved(&request, 1, None);
                    let attachment = match initial.attach_or_renew_lease(
                        &request,
                        self.lease_ttl,
                        SharedDisposition::Created,
                        self.limits,
                    ) {
                        Ok((attachment, _)) => attachment,
                        Err(error) => {
                            if error.is_capacity_error() {
                                self.metrics.record_capacity_rejection();
                            }
                            return Err(error);
                        }
                    };
                    let record = Arc::new(Mutex::new(initial));
                    index
                        .by_connection
                        .insert(request.connection_id.clone(), request.key.clone());
                    index.sessions.insert(request.key.clone(), record);
                    self.metrics.add_active_leases(1);
                    self.metrics.add_live_session();
                    ReserveLookup::Created(attachment)
                }
            };

            let record = match lookup {
                ReserveLookup::Created(attachment) => {
                    self.metrics.record_connect(true);
                    return Ok(SharedReserveOutcome {
                        attachment,
                        created: true,
                    });
                }
                ReserveLookup::Existing(record) => record,
            };

            let decision = {
                let mut current = record.lock().await;
                current.check_attach_identity(&request.launch_identity)?;
                match current.retry_decision(&request)? {
                    FailedRetryDecision::Attach => {
                        let expired = current.prune_expired_leases(request.now);
                        self.metrics.remove_active_leases(expired);
                        match current.attach_or_renew_lease(
                            &request,
                            self.lease_ttl,
                            SharedDisposition::Attached,
                            self.limits,
                        ) {
                            Ok((attachment, added_lease)) => {
                                if added_lease {
                                    self.metrics.add_active_leases(1);
                                }
                                ReserveDecision::Attach(attachment)
                            }
                            Err(error) => {
                                if error.is_capacity_error() {
                                    self.metrics.record_capacity_rejection();
                                }
                                return Err(error);
                            }
                        }
                    }
                    FailedRetryDecision::Replace { failed_generation } => {
                        ReserveDecision::Replace { failed_generation }
                    }
                }
            };

            match decision {
                ReserveDecision::Attach(attachment) => {
                    self.metrics.record_connect(false);
                    return Ok(SharedReserveOutcome {
                        attachment,
                        created: false,
                    });
                }
                ReserveDecision::Replace { failed_generation } => {
                    if let Some(outcome) = self
                        .replace_failed_generation(&request, &record, failed_generation)
                        .await?
                    {
                        self.metrics.record_connect(true);
                        return Ok(outcome);
                    }
                }
            }
        }
    }

    pub async fn mark_failed(
        &self,
        connection_id: &str,
        generation: u64,
        error_code: impl Into<String>,
        cleanup_complete: bool,
    ) -> Result<(), SharedSessionError> {
        let error_code = error_code.into();
        validate_failure_code(&error_code)?;
        loop {
            let contended = {
                let index = self.index.lock().await;
                let Some(record) = index.record_for_connection(connection_id) else {
                    return Err(SharedSessionError::SessionUnavailable);
                };
                let record_contended = match record.try_lock() {
                    Ok(mut record) => {
                        if record.connection_id != connection_id || record.generation != generation
                        {
                            return Err(SharedSessionError::GenerationStale);
                        }
                        record.cleanup_complete = cleanup_complete;
                        record.phase = SharedSessionPhase::Failed {
                            error_code,
                            cleanup_complete,
                        };
                        return Ok(());
                    }
                    Err(_) => true,
                };
                record_contended
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    pub async fn mark_cleanup_complete(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<(), SharedSessionError> {
        loop {
            let contended = {
                let index = self.index.lock().await;
                let Some(record) = index.record_for_connection(connection_id) else {
                    return Err(SharedSessionError::SessionUnavailable);
                };
                let record_contended = match record.try_lock() {
                    Ok(mut record) => {
                        if record.connection_id != connection_id || record.generation != generation
                        {
                            return Err(SharedSessionError::GenerationStale);
                        }
                        let error_code = match &record.phase {
                            SharedSessionPhase::Failed { error_code, .. } => error_code.clone(),
                            _ => return Err(SharedSessionError::SessionUnavailable),
                        };
                        record.cleanup_complete = true;
                        record.phase = SharedSessionPhase::Failed {
                            error_code,
                            cleanup_complete: true,
                        };
                        return Ok(());
                    }
                    Err(_) => true,
                };
                record_contended
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    pub async fn diagnostic_for_connection(
        &self,
        connection_id: &str,
    ) -> Option<SharedSessionProjection> {
        loop {
            let contended = {
                let index = self.index.lock().await;
                let record = index.record_for_connection(connection_id)?;
                let record_contended = match record.try_lock() {
                    Ok(record) => {
                        if record.connection_id != connection_id {
                            return None;
                        }
                        return Some(SharedSessionProjection {
                            generation: record.generation,
                            phase: record.phase.clone(),
                            queue: Vec::new(),
                            active_turn: None,
                            lease_expires_at: None,
                        });
                    }
                    Err(_) => true,
                };
                record_contended
            };
            if contended {
                tokio::task::yield_now().await;
            }
        }
    }

    async fn replace_failed_generation(
        &self,
        request: &SharedReserveRequest,
        expected_record: &Arc<Mutex<SharedSessionRecord>>,
        failed_generation: u64,
    ) -> Result<Option<SharedReserveOutcome>, SharedSessionError> {
        let mut index = self.index.lock().await;
        let is_authoritative = index
            .sessions
            .get(&request.key)
            .is_some_and(|current| Arc::ptr_eq(current, expected_record));
        if !is_authoritative {
            return Ok(None);
        }

        let current = match expected_record.try_lock() {
            Ok(current) => current,
            Err(_) => {
                drop(index);
                tokio::task::yield_now().await;
                return Ok(None);
            }
        };
        if current.generation != failed_generation {
            return Err(SharedSessionError::GenerationStale);
        }
        if !matches!(current.phase, SharedSessionPhase::Failed { .. }) {
            return Err(SharedSessionError::GenerationStale);
        }
        if !current.cleanup_complete {
            return Err(SharedSessionError::CleanupInProgress);
        }

        let old_connection_id = current.connection_id.clone();
        let old_active_leases = current.active_leases.len();
        let next_generation = failed_generation
            .checked_add(1)
            .ok_or(SharedSessionError::GenerationStale)?;
        let mut replacement =
            SharedSessionRecord::reserved(request, next_generation, Some(failed_generation));
        let (attachment, added_lease) = match replacement.attach_or_renew_lease(
            request,
            self.lease_ttl,
            SharedDisposition::Created,
            self.limits,
        ) {
            Ok(result) => result,
            Err(error) => {
                if error.is_capacity_error() {
                    self.metrics.record_capacity_rejection();
                }
                return Err(error);
            }
        };
        debug_assert!(added_lease);
        let replacement = Arc::new(Mutex::new(replacement));

        index.by_connection.remove(&old_connection_id);
        index
            .by_connection
            .insert(request.connection_id.clone(), request.key.clone());
        index.sessions.insert(request.key.clone(), replacement);
        self.metrics.remove_active_leases(old_active_leases);
        self.metrics.add_active_leases(1);

        Ok(Some(SharedReserveOutcome {
            attachment,
            created: true,
        }))
    }
}

#[derive(Clone, Copy)]
struct BrokerLimits {
    max_active_leases: usize,
    max_connect_ledger_entries: usize,
}

impl Default for BrokerLimits {
    fn default() -> Self {
        Self {
            max_active_leases: MAX_ACTIVE_LEASES,
            max_connect_ledger_entries: MAX_CONNECT_LEDGER_ENTRIES,
        }
    }
}

#[derive(Default)]
struct SharedSessionIndex {
    sessions: HashMap<SharedSessionKey, Arc<Mutex<SharedSessionRecord>>>,
    by_connection: HashMap<String, SharedSessionKey>,
}

impl SharedSessionIndex {
    fn record_for_connection(
        &self,
        connection_id: &str,
    ) -> Option<&Arc<Mutex<SharedSessionRecord>>> {
        let key = self.by_connection.get(connection_id)?;
        self.sessions.get(key)
    }
}

struct SharedSessionRecord {
    generation: u64,
    connection_id: String,
    launch_identity: SharedLaunchIdentity,
    phase: SharedSessionPhase,
    cleanup_complete: bool,
    active_leases: HashMap<ClientIdentity, ActiveLease>,
    connect_ledger: HashMap<ConnectIdentity, SharedSessionAttachment>,
    expired_leases: VecDeque<String>,
    replaced_failed_generation: Option<u64>,
    _created_at: tokio::time::Instant,
    _created_at_utc: DateTime<Utc>,
    connect_count: u64,
}

impl SharedSessionRecord {
    fn reserved(
        request: &SharedReserveRequest,
        generation: u64,
        replaced_failed_generation: Option<u64>,
    ) -> Self {
        Self {
            generation,
            connection_id: request.connection_id.clone(),
            launch_identity: request.launch_identity.clone(),
            phase: SharedSessionPhase::Reserved,
            cleanup_complete: false,
            active_leases: HashMap::new(),
            connect_ledger: HashMap::new(),
            expired_leases: VecDeque::new(),
            replaced_failed_generation,
            _created_at: request.now,
            _created_at_utc: request.now_utc,
            connect_count: 0,
        }
    }

    fn check_attach_identity(
        &self,
        requested: &SharedLaunchIdentity,
    ) -> Result<(), SharedSessionError> {
        let conflict_kind = if self.launch_identity.agent_type != requested.agent_type {
            Some(SharedConfigConflictKind::AgentType)
        } else if self.launch_identity.working_dir_fingerprint != requested.working_dir_fingerprint
        {
            Some(SharedConfigConflictKind::WorkingDirectory)
        } else if self.launch_identity.external_session_id != requested.external_session_id {
            Some(SharedConfigConflictKind::ExternalSession)
        } else if self.launch_identity.attach_mode != requested.attach_mode {
            Some(SharedConfigConflictKind::AttachMode)
        } else if self.launch_identity.route_fingerprint != requested.route_fingerprint {
            Some(SharedConfigConflictKind::DelegationRoute)
        } else if self.launch_identity.terminal_shell_fingerprint
            != requested.terminal_shell_fingerprint
        {
            Some(SharedConfigConflictKind::TerminalShell)
        } else if self.launch_identity.purpose != requested.purpose {
            Some(SharedConfigConflictKind::Purpose)
        } else {
            None
        };

        if let Some(conflict_kind) = conflict_kind {
            return Err(SharedSessionError::ConfigConflict {
                connection_id: self.connection_id.clone(),
                conflict_kind,
            });
        }
        Ok(())
    }

    fn retry_decision(
        &self,
        request: &SharedReserveRequest,
    ) -> Result<FailedRetryDecision, SharedSessionError> {
        match &self.phase {
            SharedSessionPhase::Closing => Err(SharedSessionError::Closing),
            SharedSessionPhase::Failed { .. } => {
                let retry_generation = request
                    .retry_failed_generation
                    .ok_or(SharedSessionError::SessionUnavailable)?;
                if retry_generation != self.generation {
                    return Err(SharedSessionError::GenerationStale);
                }
                if !self.cleanup_complete {
                    return Err(SharedSessionError::CleanupInProgress);
                }
                Ok(FailedRetryDecision::Replace {
                    failed_generation: retry_generation,
                })
            }
            SharedSessionPhase::Reserved
            | SharedSessionPhase::Bootstrapping
            | SharedSessionPhase::Ready => match request.retry_failed_generation {
                None => Ok(FailedRetryDecision::Attach),
                Some(failed_generation)
                    if self.replaced_failed_generation == Some(failed_generation)
                        && self.generation == failed_generation.saturating_add(1) =>
                {
                    Ok(FailedRetryDecision::Attach)
                }
                Some(_) => Err(SharedSessionError::GenerationStale),
            },
        }
    }

    fn prune_expired_leases(&mut self, now: tokio::time::Instant) -> usize {
        let expired_clients: Vec<_> = self
            .active_leases
            .iter()
            .filter(|(_, lease)| lease.expires_at <= now)
            .map(|(client, _)| client.clone())
            .collect();
        for client in &expired_clients {
            if let Some(lease) = self.active_leases.remove(client) {
                self.expired_leases.push_back(lease.lease_id);
                if self.expired_leases.len() > MAX_EXPIRED_LEASE_TOMBSTONES {
                    self.expired_leases.pop_front();
                }
            }
        }
        expired_clients.len()
    }

    fn attach_or_renew_lease(
        &mut self,
        request: &SharedReserveRequest,
        lease_ttl: Duration,
        disposition: SharedDisposition,
        limits: BrokerLimits,
    ) -> Result<(SharedSessionAttachment, bool), SharedSessionError> {
        let client_identity = ClientIdentity::from_request(request);
        let connect_identity = ConnectIdentity::from_request(request);

        if let Some(previous) = self.connect_ledger.get(&connect_identity) {
            let is_active = self
                .active_leases
                .get(&client_identity)
                .is_some_and(|lease| lease.lease_id == previous.lease_id);
            if is_active {
                self.connect_count = self.connect_count.saturating_add(1);
                return Ok((previous.clone(), false));
            }
        }

        let is_new_client = !self.active_leases.contains_key(&client_identity);
        if is_new_client && self.active_leases.len() >= limits.max_active_leases {
            return Err(SharedSessionError::ClientLeaseCapacityExceeded);
        }
        if !self.connect_ledger.contains_key(&connect_identity)
            && self.connect_ledger.len() >= limits.max_connect_ledger_entries
        {
            return Err(SharedSessionError::ConnectLedgerCapacityExceeded);
        }

        let monotonic_expiry = request.now + lease_ttl;
        let wall_expiry = request.now_utc
            + chrono::Duration::from_std(lease_ttl)
                .expect("shared session lease TTL must fit chrono::Duration");
        let lease = self
            .active_leases
            .entry(client_identity)
            .or_insert_with(|| ActiveLease {
                lease_id: uuid::Uuid::new_v4().to_string(),
                expires_at: monotonic_expiry,
                expires_at_utc: wall_expiry,
            });
        lease.expires_at = monotonic_expiry;
        lease.expires_at_utc = wall_expiry;

        let attachment = SharedSessionAttachment {
            connection_id: self.connection_id.clone(),
            generation: self.generation,
            lease_id: lease.lease_id.clone(),
            lease_expires_at: lease.expires_at_utc,
            disposition,
            phase: self.phase.clone(),
        };
        self.connect_ledger
            .insert(connect_identity, attachment.clone());
        self.connect_count = self.connect_count.saturating_add(1);
        Ok((attachment, is_new_client))
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ClientIdentity {
    client_instance_id: String,
    device_id: String,
}

impl ClientIdentity {
    fn from_request(request: &SharedReserveRequest) -> Self {
        Self {
            client_instance_id: request.client_instance_id.clone(),
            device_id: request.device_id.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ConnectIdentity {
    client_instance_id: String,
    device_id: String,
    request_id: String,
}

impl ConnectIdentity {
    fn from_request(request: &SharedReserveRequest) -> Self {
        Self {
            client_instance_id: request.client_instance_id.clone(),
            device_id: request.device_id.clone(),
            request_id: request.request_id.clone(),
        }
    }
}

struct ActiveLease {
    lease_id: String,
    expires_at: tokio::time::Instant,
    expires_at_utc: DateTime<Utc>,
}

enum ReserveLookup {
    Created(SharedSessionAttachment),
    Existing(Arc<Mutex<SharedSessionRecord>>),
}

enum FailedRetryDecision {
    Attach,
    Replace { failed_generation: u64 },
}

enum ReserveDecision {
    Attach(SharedSessionAttachment),
    Replace { failed_generation: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        key: SharedSessionKey,
        connection_id: &str,
        client: &str,
        request_id: &str,
    ) -> SharedReserveRequest {
        SharedReserveRequest {
            key,
            connection_id: connection_id.into(),
            launch_identity: SharedLaunchIdentity::fixture(),
            client_instance_id: client.into(),
            device_id: "device-a".into(),
            request_id: request_id.into(),
            retry_failed_generation: None,
            now: tokio::time::Instant::now(),
            now_utc: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn one_hundred_reservations_share_one_incarnation() {
        let broker = SharedSessionBroker::default();
        let mut joins = Vec::new();
        for n in 0..100 {
            let broker = broker.clone();
            joins.push(tokio::spawn(async move {
                broker
                    .reserve_or_attach(request(
                        SharedSessionKey::Conversation(42),
                        &format!("candidate-{n}"),
                        &format!("client-{n}"),
                        &format!("request-{n}"),
                    ))
                    .await
                    .unwrap()
            }));
        }
        let outcomes = futures::future::join_all(joins).await;
        let ids: std::collections::HashSet<_> = outcomes
            .into_iter()
            .map(|result| result.unwrap().attachment.connection_id)
            .collect();
        assert_eq!(ids.len(), 1);
        let metrics = broker.metrics().snapshot();
        assert_eq!(metrics.created_total, 1);
        assert_eq!(metrics.attached_total, 99);
        assert_eq!(metrics.live_sessions, 1);
        assert_eq!(metrics.active_leases, 100);
    }

    #[tokio::test]
    async fn immutable_launch_conflict_does_not_mutate_live_record() {
        let broker = SharedSessionBroker::default();
        let first = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(7),
                "conn-a",
                "client-a",
                "req-a",
            ))
            .await
            .unwrap();
        let mut conflicting = request(
            SharedSessionKey::Conversation(7),
            "conn-b",
            "client-b",
            "req-b",
        );
        conflicting.launch_identity.working_dir_fingerprint = "different".into();
        assert!(matches!(
            broker.reserve_or_attach(conflicting).await,
            Err(SharedSessionError::ConfigConflict {
                conflict_kind: SharedConfigConflictKind::WorkingDirectory,
                ..
            })
        ));
        assert_eq!(
            broker
                .diagnostic_for_connection(&first.attachment.connection_id)
                .await
                .unwrap()
                .generation,
            1
        );
    }

    #[tokio::test]
    async fn failed_retry_requires_cleanup_and_increments_generation() {
        let broker = SharedSessionBroker::default();
        let first = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(8),
                "conn-a",
                "client-a",
                "req-a",
            ))
            .await
            .unwrap();
        broker
            .mark_failed(
                &first.attachment.connection_id,
                1,
                "companion_initialization_failed",
                false,
            )
            .await
            .unwrap();
        let mut retry = request(
            SharedSessionKey::Conversation(8),
            "conn-b",
            "client-a",
            "req-b",
        );
        retry.retry_failed_generation = Some(1);
        assert!(matches!(
            broker.reserve_or_attach(retry.clone()).await,
            Err(SharedSessionError::CleanupInProgress)
        ));
        broker
            .mark_cleanup_complete(&first.attachment.connection_id, 1)
            .await
            .unwrap();
        let replacement = broker.reserve_or_attach(retry).await.unwrap();
        assert_eq!(replacement.attachment.generation, 2);
        assert_eq!(replacement.attachment.connection_id, "conn-b");
        let metrics = broker.metrics().snapshot();
        assert_eq!(metrics.created_total, 2);
        assert_eq!(metrics.attached_total, 0);
        assert_eq!(metrics.live_sessions, 1);
        assert_eq!(metrics.active_leases, 1);
    }

    #[tokio::test]
    async fn concurrent_failed_retries_create_one_next_generation() {
        let broker = failed_cleanup_complete_fixture(18).await;
        let outcomes = futures::future::join_all((0..10).map(|n| {
            let broker = broker.clone();
            async move {
                let mut retry = request(
                    SharedSessionKey::Conversation(18),
                    &format!("retry-{n}"),
                    &format!("client-{n}"),
                    &format!("request-{n}"),
                );
                retry.retry_failed_generation = Some(1);
                broker.reserve_or_attach(retry).await.unwrap()
            }
        }))
        .await;
        let ids: std::collections::HashSet<_> = outcomes
            .iter()
            .map(|outcome| outcome.attachment.connection_id.as_str())
            .collect();
        assert_eq!(ids.len(), 1);
        assert!(outcomes
            .iter()
            .all(|outcome| outcome.attachment.generation == 2));
    }

    #[tokio::test]
    async fn diagnostics_never_expose_lease_or_client_identity() {
        let broker = SharedSessionBroker::default();
        let result = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(9),
                "conn",
                "private-client",
                "req",
            ))
            .await
            .unwrap();
        let value = serde_json::to_value(
            broker
                .diagnostic_for_connection(&result.attachment.connection_id)
                .await
                .unwrap(),
        )
        .unwrap();
        let encoded = value.to_string();
        assert!(!encoded.contains(&result.attachment.lease_id));
        assert!(!encoded.contains("private-client"));
    }

    #[tokio::test]
    async fn detached_generation_cannot_accept_failure_mutation() {
        let broker = failed_cleanup_complete_fixture(19).await;
        let old_record = record_for_connection_for_test(&broker, "failed-connection").await;
        let old_guard = old_record.lock().await;
        let mut stale_mutation = Box::pin(broker.mark_failed(
            "failed-connection",
            1,
            "companion_initialization_failed",
            false,
        ));
        assert!(matches!(
            futures::poll!(stale_mutation.as_mut()),
            std::task::Poll::Pending
        ));

        install_replacement_pointer(&broker, 19).await;
        drop(old_guard);

        assert!(matches!(
            stale_mutation.await,
            Err(SharedSessionError::SessionUnavailable) | Err(SharedSessionError::GenerationStale)
        ));
    }

    #[tokio::test]
    async fn detached_generation_cannot_complete_cleanup() {
        let broker = failed_cleanup_complete_fixture(20).await;
        let old_record = record_for_connection_for_test(&broker, "failed-connection").await;
        let old_guard = old_record.lock().await;
        let mut stale_cleanup = Box::pin(broker.mark_cleanup_complete("failed-connection", 1));
        assert!(matches!(
            futures::poll!(stale_cleanup.as_mut()),
            std::task::Poll::Pending
        ));

        install_replacement_pointer(&broker, 20).await;
        drop(old_guard);

        assert!(matches!(
            stale_cleanup.await,
            Err(SharedSessionError::SessionUnavailable) | Err(SharedSessionError::GenerationStale)
        ));
    }

    #[tokio::test]
    async fn diagnostics_do_not_return_a_detached_generation() {
        let broker = failed_cleanup_complete_fixture(21).await;
        let old_record = record_for_connection_for_test(&broker, "failed-connection").await;
        let old_guard = old_record.lock().await;
        let mut stale_diagnostic = Box::pin(broker.diagnostic_for_connection("failed-connection"));
        assert!(matches!(
            futures::poll!(stale_diagnostic.as_mut()),
            std::task::Poll::Pending
        ));

        install_replacement_pointer(&broker, 21).await;
        drop(old_guard);

        assert!(stale_diagnostic.await.is_none());
    }

    #[tokio::test]
    async fn failed_phase_rejects_unrecognized_diagnostic_text() {
        let broker = SharedSessionBroker::default();
        let reservation = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(22),
                "conn-a",
                "client-a",
                "request-a",
            ))
            .await
            .unwrap();
        for private in [
            "/Users/private/project/token.txt",
            "super-secret-token-value",
        ] {
            assert!(matches!(
                broker
                    .mark_failed(&reservation.attachment.connection_id, 1, private, false)
                    .await,
                Err(SharedSessionError::InvalidField {
                    field: "error_code"
                })
            ));
            let diagnostic = broker
                .diagnostic_for_connection(&reservation.attachment.connection_id)
                .await
                .unwrap();
            let serialized = serde_json::to_string(&diagnostic).unwrap();
            let debug = format!("{diagnostic:?}");
            assert!(!serialized.contains(private));
            assert!(!debug.contains(private));
        }
    }

    #[test]
    fn client_labels_have_exact_ascii_bounds() {
        let max = "a".repeat(128);
        let too_long = "a".repeat(129);
        assert!(validate_client_label("client_instance_id", "aZ09._:-").is_ok());
        assert!(validate_client_label("client_instance_id", &max).is_ok());
        for invalid in ["", too_long.as_str(), "contains space", "非ascii"] {
            assert!(matches!(
                validate_client_label("client_instance_id", invalid),
                Err(SharedSessionError::InvalidField {
                    field: "client_instance_id"
                })
            ));
        }
    }

    #[tokio::test]
    async fn capacity_limits_reject_only_new_identities() {
        assert_eq!(MAX_ACTIVE_LEASES, 256);
        assert_eq!(MAX_CONNECT_LEDGER_ENTRIES, 4_096);
        assert_eq!(MAX_WAITING_PROMPTS, 64);
        assert_eq!(MAX_WAITING_BYTES, 32 * 1024 * 1024);
        assert_eq!(MAX_PROMPT_LEDGER_ENTRIES, 65_536);
        assert_eq!(MAX_EXPIRED_LEASE_TOMBSTONES, 1_024);
        assert_eq!(MAX_REPLACED_CONNECTION_TOMBSTONES, 4_096);

        let fixture = broker_at_identity_limits().await;
        assert!(fixture.retry_existing_connect().await.is_ok());
        assert!(matches!(
            fixture.connect_new_identity().await,
            Err(SharedSessionError::ConnectLedgerCapacityExceeded)
        ));
        assert!(matches!(
            fixture.attach_new_client().await,
            Err(SharedSessionError::ClientLeaseCapacityExceeded)
        ));
        let metrics = fixture.broker.metrics().snapshot();
        assert_eq!(metrics.live_sessions, 1);
        assert_eq!(metrics.active_leases, 3);
        assert_eq!(metrics.capacity_rejected_total, 2);
    }

    #[tokio::test]
    async fn identical_connect_retry_returns_original_attachment() {
        let broker = SharedSessionBroker::default();
        let original_request = request(
            SharedSessionKey::Conversation(11),
            "conn-a",
            "client-a",
            "req-a",
        );
        let first = broker
            .reserve_or_attach(original_request.clone())
            .await
            .unwrap();
        let retry = broker.reserve_or_attach(original_request).await.unwrap();

        assert_eq!(
            retry.attachment.connection_id,
            first.attachment.connection_id
        );
        assert_eq!(retry.attachment.lease_id, first.attachment.lease_id);
        assert_eq!(
            retry.attachment.lease_expires_at,
            first.attachment.lease_expires_at
        );
        assert_eq!(retry.attachment.disposition, SharedDisposition::Created);
        assert!(!retry.created);
    }

    #[test]
    fn debug_output_redacts_visible_text_and_lease_ids() {
        let summary = SharedQueuedPromptSummary {
            queue_item_id: "queue-a".into(),
            enqueue_seq: 1,
            client_message_id: "message-a".into(),
            visible_text: Some("private prompt".into()),
            visible_text_truncated: false,
            attachment_count: 0,
            submitted_at: chrono::Utc::now(),
            state: SharedQueuedPromptState::Queued,
        };
        let projection = SharedSessionProjection {
            generation: 1,
            phase: SharedSessionPhase::Ready,
            queue: vec![summary],
            active_turn: None,
            lease_expires_at: None,
        };
        let guard = SharedMutationGuard {
            connection_id: "conn-a".into(),
            generation: 1,
            lease_id: "private-lease".into(),
        };

        let encoded = format!("{projection:?} {guard:?}");
        assert!(!encoded.contains("private prompt"));
        assert!(!encoded.contains("private-lease"));
        assert!(encoded.contains("lease_id: \"***\""));
    }

    #[test]
    fn prompt_summaries_expose_only_bounded_text_and_attachment_count() {
        let private_capture = crate::auto_title::PromptCaptureContext::new(
            Some("private capture context".into()),
            None,
        );
        let long_text = format!("safe:{}", "界".repeat(MAX_QUEUE_VISIBLE_TEXT_CHARS));
        let summary = SharedQueuedPromptSummary::from_prompt(
            "queue-a".into(),
            1,
            "message-a".into(),
            &[
                crate::acp::types::PromptInputBlock::Text {
                    text: long_text.clone(),
                },
                crate::acp::types::PromptInputBlock::Image {
                    data: "private-base64".into(),
                    mime_type: "private-mime".into(),
                    uri: Some("private-image-uri".into()),
                },
                crate::acp::types::PromptInputBlock::Resource {
                    uri: "private-resource-uri".into(),
                    mime_type: Some("private-resource-mime".into()),
                    text: Some("private-resource-text".into()),
                    blob: Some("private-resource-blob".into()),
                },
                crate::acp::types::PromptInputBlock::ResourceLink {
                    uri: "private-link-uri".into(),
                    name: "private-link-name".into(),
                    mime_type: Some("private-link-mime".into()),
                    description: Some("private-link-description".into()),
                },
            ],
            Some(&private_capture),
            chrono::Utc::now(),
            SharedQueuedPromptState::Queued,
        );

        let visible = summary.visible_text.as_deref().unwrap();
        assert_eq!(visible.chars().count(), MAX_QUEUE_VISIBLE_TEXT_CHARS);
        assert!(long_text.starts_with(visible));
        assert!(summary.visible_text_truncated);
        assert_eq!(summary.attachment_count, 3);
        let serialized = serde_json::to_string(&summary).unwrap();
        for private in [
            "private capture context",
            "private-base64",
            "private-mime",
            "private-image-uri",
            "private-resource-uri",
            "private-resource-text",
            "private-resource-blob",
            "private-link-uri",
            "private-link-name",
            "private-link-description",
        ] {
            assert!(!serialized.contains(private));
        }
    }

    #[test]
    fn shared_error_codes_are_stable() {
        let conflict = SharedSessionError::ConfigConflict {
            connection_id: "conn-a".into(),
            conflict_kind: SharedConfigConflictKind::AgentType,
        };
        for (error, expected) in [
            (conflict, "shared_session_config_conflict"),
            (
                SharedSessionError::ProtocolRequired,
                "shared_session_protocol_required",
            ),
            (
                SharedSessionError::GenerationStale,
                "shared_session_generation_stale",
            ),
            (SharedSessionError::Closing, "shared_session_closing"),
            (
                SharedSessionError::CleanupInProgress,
                "shared_session_cleanup_in_progress",
            ),
            (SharedSessionError::LeaseMissing, "client_lease_missing"),
            (SharedSessionError::LeaseExpired, "client_lease_expired"),
            (
                SharedSessionError::ClientLeaseCapacityExceeded,
                "client_lease_capacity_exceeded",
            ),
            (
                SharedSessionError::ConnectLedgerCapacityExceeded,
                "connect_idempotency_capacity_exceeded",
            ),
            (
                SharedSessionError::PromptLedgerCapacityExceeded,
                "prompt_idempotency_capacity_exceeded",
            ),
            (SharedSessionError::PromptQueueFull, "prompt_queue_full"),
            (
                SharedSessionError::IdempotencyKeyConflict,
                "idempotency_key_conflict",
            ),
            (
                SharedSessionError::QueueItemNotFound,
                "queue_item_not_found",
            ),
            (
                SharedSessionError::QueueItemAlreadyDispatching,
                "queue_item_already_dispatching",
            ),
            (
                SharedSessionError::InteractionAlreadyResolved,
                "interaction_already_resolved",
            ),
            (SharedSessionError::StaleTurn, "stale_turn"),
            (
                SharedSessionError::SessionUnavailable,
                "session_unavailable",
            ),
            (
                SharedSessionError::CompanionInitializationFailed,
                "companion_initialization_failed",
            ),
            (
                SharedSessionError::ConversationKeyConflict,
                "shared_session_conversation_key_conflict",
            ),
            (
                SharedSessionError::InvalidField { field: "device_id" },
                "invalid_shared_session_field",
            ),
        ] {
            assert_eq!(error.code(), expected);
            assert!(validate_failure_code(expected).is_ok());
        }
    }

    async fn failed_cleanup_complete_fixture(id: i32) -> SharedSessionBroker {
        let broker = SharedSessionBroker::default();
        let first = broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(id),
                "failed-connection",
                "failed-client",
                "failed-request",
            ))
            .await
            .unwrap();
        broker
            .mark_failed(
                &first.attachment.connection_id,
                1,
                "companion_initialization_failed",
                false,
            )
            .await
            .unwrap();
        broker
            .mark_cleanup_complete(&first.attachment.connection_id, 1)
            .await
            .unwrap();
        broker
    }

    async fn install_replacement_pointer(broker: &SharedSessionBroker, id: i32) {
        let replacement_request = request(
            SharedSessionKey::Conversation(id),
            "replacement-connection",
            "replacement-client",
            "replacement-request",
        );
        let mut replacement = SharedSessionRecord::reserved(&replacement_request, 2, Some(1));
        replacement
            .attach_or_renew_lease(
                &replacement_request,
                DEFAULT_CLIENT_LEASE_TTL,
                SharedDisposition::Created,
                BrokerLimits::default(),
            )
            .unwrap();
        let mut index = broker.index.lock().await;
        index.by_connection.remove("failed-connection");
        index.by_connection.insert(
            replacement_request.connection_id.clone(),
            replacement_request.key.clone(),
        );
        index
            .sessions
            .insert(replacement_request.key, Arc::new(Mutex::new(replacement)));
    }

    async fn record_for_connection_for_test(
        broker: &SharedSessionBroker,
        connection_id: &str,
    ) -> Arc<Mutex<SharedSessionRecord>> {
        let index = broker.index.lock().await;
        index.record_for_connection(connection_id).unwrap().clone()
    }

    struct BrokerAtIdentityLimits {
        broker: SharedSessionBroker,
        retry: SharedReserveRequest,
    }

    impl BrokerAtIdentityLimits {
        async fn retry_existing_connect(&self) -> Result<SharedReserveOutcome, SharedSessionError> {
            self.broker.reserve_or_attach(self.retry.clone()).await
        }

        async fn connect_new_identity(&self) -> Result<SharedReserveOutcome, SharedSessionError> {
            self.broker
                .reserve_or_attach(request(
                    SharedSessionKey::Conversation(12),
                    "ignored-connect-candidate",
                    "client-0",
                    "request-over-ledger-limit",
                ))
                .await
        }

        async fn attach_new_client(&self) -> Result<SharedReserveOutcome, SharedSessionError> {
            self.broker
                .reserve_or_attach(request(
                    SharedSessionKey::Conversation(12),
                    "ignored-lease-candidate",
                    "client-over-lease-limit",
                    "request-over-both-limits",
                ))
                .await
        }
    }

    async fn broker_at_identity_limits() -> BrokerAtIdentityLimits {
        const TEST_MAX_ACTIVE_LEASES: usize = 3;
        const TEST_MAX_CONNECT_LEDGER_ENTRIES: usize = 4;

        let broker = SharedSessionBroker::with_limits_for_test(
            TEST_MAX_ACTIVE_LEASES,
            TEST_MAX_CONNECT_LEDGER_ENTRIES,
        );
        let retry = request(
            SharedSessionKey::Conversation(12),
            "conn-a",
            "client-0",
            "request-0",
        );
        broker.reserve_or_attach(retry.clone()).await.unwrap();

        for n in 1..TEST_MAX_ACTIVE_LEASES {
            broker
                .reserve_or_attach(request(
                    SharedSessionKey::Conversation(12),
                    "ignored-candidate",
                    &format!("client-{n}"),
                    &format!("request-{n}"),
                ))
                .await
                .unwrap();
        }
        broker
            .reserve_or_attach(request(
                SharedSessionKey::Conversation(12),
                "ignored-candidate",
                "client-0",
                "request-fill-ledger",
            ))
            .await
            .unwrap();

        BrokerAtIdentityLimits { broker, retry }
    }
}
