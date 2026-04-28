use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptInputBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        mime_type: String,
        #[serde(default)]
        uri: Option<String>,
    },
    Resource {
        uri: String,
        #[serde(default)]
        mime_type: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        blob: Option<String>,
    },
    ResourceLink {
        uri: String,
        name: String,
        #[serde(default)]
        mime_type: Option<String>,
        #[serde(default)]
        description: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptCapabilitiesInfo {
    pub image: bool,
    pub audio: bool,
    pub embedded_context: bool,
}

/// 所有 ACP 事件统一通过此 envelope 发出。
/// `seq` 用于前端去重锚点（Phase 0 占位 0，Phase 1 起严格递增）。
/// `connection_id` 上提到顶层，配合 `#[serde(flatten)]` 让 JSON 保持平铺：
/// `{ seq, connection_id, type, ...变体字段 }`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u64,
    pub connection_id: String,
    #[serde(flatten)]
    pub payload: AcpEvent,
}

/// Events pushed from Rust backend to frontend via Tauri event system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AcpEvent {
    /// Agent returned text content (streaming delta)
    ContentDelta { text: String },
    /// Agent thinking/reasoning
    Thinking { text: String },
    /// Raw SDK message forwarded from Claude ACP extension notification
    ClaudeSdkMessage {
        session_id: String,
        message: serde_json::Value,
    },
    /// Agent initiated a tool call
    ToolCall {
        tool_call_id: String,
        title: String,
        kind: String,
        status: String,
        content: Option<String>,
        raw_input: Option<String>,
        raw_output: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        locations: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<serde_json::Value>,
    },
    /// Tool call status/content updated
    ToolCallUpdate {
        tool_call_id: String,
        title: Option<String>,
        status: Option<String>,
        content: Option<String>,
        raw_input: Option<String>,
        raw_output: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        raw_output_append: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        locations: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        meta: Option<serde_json::Value>,
    },
    /// Agent requests permission
    PermissionRequest {
        request_id: String,
        tool_call: serde_json::Value,
        options: Vec<PermissionOptionInfo>,
    },
    /// Turn completed
    TurnComplete {
        session_id: String,
        stop_reason: String,
        agent_type: String,
    },
    /// Session established with agent-assigned session ID
    SessionStarted { session_id: String },
    /// Backend has bound this connection to a conversation row. Emitted exactly
    /// once per connection lifetime, on first prompt that creates the row.
    /// Frontend uses this to associate the connection_id with conversation_id
    /// without polling the DB.
    ConversationLinked {
        conversation_id: i32,
        folder_id: i32,
    },
    /// Backend has transitioned the conversation row's `status` column.
    /// Emitted by `send_prompt_linked` (`InProgress`) and the lifecycle
    /// subscriber on `TurnComplete` (`PendingReview`). The frontend mirrors
    /// the new status onto its sidebar/list state without re-querying the DB.
    /// `completed` / `cancelled` transitions remain frontend-driven and are
    /// NOT emitted via this event.
    ConversationStatusChanged {
        conversation_id: i32,
        status: crate::db::entities::conversation::ConversationStatus,
    },
    /// Session modes are available for this connection
    SessionModes { modes: SessionModeStateInfo },
    /// Session configuration options are available/updated for this connection
    SessionConfigOptions {
        config_options: Vec<SessionConfigOptionInfo>,
    },
    /// Initial selector payloads (modes/config options) have been emitted
    SelectorsReady,
    /// Prompt capabilities for this connection
    PromptCapabilities {
        prompt_capabilities: PromptCapabilitiesInfo,
    },
    /// Whether the agent supports session/fork
    ForkSupported { supported: bool },
    /// Current session mode changed
    ModeChanged { mode_id: String },
    /// Agent reported plan update for current turn
    PlanUpdate { entries: Vec<PlanEntryInfo> },
    /// Connection status changed
    StatusChanged { status: ConnectionStatus },
    /// Error occurred
    Error {
        message: String,
        agent_type: String,
        /// Stable machine-readable identifier (e.g. "initialize_timeout").
        /// When present, the frontend renders a localized message keyed on
        /// this code; otherwise it falls back to `message`.
        code: Option<String>,
    },
    /// Available slash commands updated
    AvailableCommands {
        commands: Vec<AvailableCommandInfo>,
    },
    /// Session usage/context window updated during conversation
    UsageUpdate { used: u64, size: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionOptionInfo {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionModeInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionModeStateInfo {
    pub current_mode_id: String,
    pub available_modes: Vec<SessionModeInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfigSelectOptionInfo {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfigSelectGroupInfo {
    pub group: String,
    pub name: String,
    pub options: Vec<SessionConfigSelectOptionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfigSelectInfo {
    pub current_value: String,
    pub options: Vec<SessionConfigSelectOptionInfo>,
    pub groups: Vec<SessionConfigSelectGroupInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionConfigKindInfo {
    Select(SessionConfigSelectInfo),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfigOptionInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub kind: SessionConfigKindInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntryInfo {
    pub content: String,
    pub priority: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Prompting,
    Disconnected,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionInfo {
    pub id: String,
    pub agent_type: crate::models::agent::AgentType,
    pub status: ConnectionStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct AcpAgentInfo {
    pub agent_type: crate::models::agent::AgentType,
    pub registry_id: String,
    pub registry_version: Option<String>,
    pub name: String,
    pub description: String,
    pub available: bool,
    pub distribution_type: String,
    pub enabled: bool,
    pub sort_order: i32,
    pub installed_version: Option<String>,
    pub env: BTreeMap<String, String>,
    pub config_json: Option<String>,
    pub config_file_path: Option<String>,
    pub opencode_auth_json: Option<String>,
    pub codex_auth_json: Option<String>,
    pub codex_config_toml: Option<String>,
    pub cline_secrets_json: Option<String>,
    pub model_provider_id: Option<i32>,
}

/// Lightweight status info for a single agent, used by connect() pre-check.
#[derive(Debug, Clone, Serialize)]
pub struct AcpAgentStatus {
    pub agent_type: crate::models::agent::AgentType,
    pub available: bool,
    pub enabled: bool,
    pub installed_version: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentSkillScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentSkillLayout {
    MarkdownFile,
    SkillDirectory,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSkillLocation {
    pub scope: AgentSkillScope,
    pub path: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSkillItem {
    pub id: String,
    pub name: String,
    pub scope: AgentSkillScope,
    pub layout: AgentSkillLayout,
    pub path: String,
    /// True for skills bundled by the agent CLI itself (e.g. Codex's
    /// `~/.codex/skills/.system/*`). Surfaced so the UI can show them but
    /// refuse to edit or delete; the backend also refuses such writes.
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSkillsListResult {
    pub supported: bool,
    pub message: Option<String>,
    pub locations: Vec<AgentSkillLocation>,
    pub skills: Vec<AgentSkillItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentSkillContent {
    pub skill: AgentSkillItem,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableCommandInfo {
    pub name: String,
    pub description: String,
    pub input_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkResultInfo {
    pub forked_session_id: String,
    pub original_session_id: String,
}

#[cfg(test)]
mod envelope_tests {
    use super::*;

    #[test]
    fn event_envelope_serializes_with_flat_payload() {
        let env = EventEnvelope {
            seq: 5,
            connection_id: "conn-1".to_string(),
            payload: AcpEvent::ContentDelta {
                text: "hello".to_string(),
            },
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["seq"], 5);
        assert_eq!(json["connection_id"], "conn-1");
        assert_eq!(json["type"], "content_delta");
        assert_eq!(json["text"], "hello");
        assert!(
            json.get("payload").is_none(),
            "flatten means no nested 'payload' key in JSON"
        );
    }

    #[test]
    fn conversation_status_changed_round_trips_with_flat_payload() {
        use crate::db::entities::conversation::ConversationStatus;
        let env = EventEnvelope {
            seq: 12,
            connection_id: "conn-x".to_string(),
            payload: AcpEvent::ConversationStatusChanged {
                conversation_id: 99,
                status: ConversationStatus::PendingReview,
            },
        };
        let json = serde_json::to_value(&env).unwrap();
        assert_eq!(json["seq"], 12);
        assert_eq!(json["connection_id"], "conn-x");
        assert_eq!(json["type"], "conversation_status_changed");
        assert_eq!(json["conversation_id"], 99);
        assert_eq!(json["status"], "pending_review");
        assert!(
            json.get("payload").is_none(),
            "flatten means no nested 'payload' key in JSON"
        );

        // Round-trip back to verify Deserialize matches Serialize.
        let back: EventEnvelope = serde_json::from_value(json).unwrap();
        match back.payload {
            AcpEvent::ConversationStatusChanged {
                conversation_id,
                status,
            } => {
                assert_eq!(conversation_id, 99);
                assert_eq!(status, ConversationStatus::PendingReview);
            }
            other => panic!("expected ConversationStatusChanged, got {other:?}"),
        }
    }
}
