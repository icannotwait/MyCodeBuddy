use super::{SharedConfigConflictKind, MAX_CLIENT_LABEL_LEN};

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

    pub(super) fn is_capacity_error(&self) -> bool {
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

pub(super) fn validate_failure_code(value: &str) -> Result<(), SharedSessionError> {
    if !STABLE_SHARED_SESSION_ERROR_CODES.contains(&value) {
        return Err(SharedSessionError::InvalidField {
            field: "error_code",
        });
    }
    Ok(())
}
