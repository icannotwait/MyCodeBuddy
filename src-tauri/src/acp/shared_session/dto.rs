use std::{fmt, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
    pub(super) fn fixture() -> Self {
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
