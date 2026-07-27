use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

/// A single tool call record from a subagent's execution transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentToolCall {
    pub tool_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentExecutionStats {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tool_use_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bash_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edit_file_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other_tool_count: Option<u32>,
    /// Tool calls extracted from the subagent's own JSONL transcript.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tool_calls: Vec<AgentToolCall>,
}

/// Image payload shared by content blocks and ACP wire events.
///
/// Same shape used in three places:
/// 1. `ContentBlock::Image` (user-attached or assistant inline image)
/// 2. `ContentBlock::ImageGeneration.images` (codex-acp v0.14+ image generation)
/// 3. `ToolCallState.images` (snapshot recovery for in-flight image generation)
///
/// Re-exported from `acp::types` as `ToolCallImageInfo` for ACP wire callsites.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageData {
    pub data: String,
    pub mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        uri: Option<String>,
    },
    /// codex-acp v0.14+ image generation (PR #271). Distinct from `Image`
    /// because codex-acp positions image generation as a first-class
    /// `ToolCall(title="Image generation")` carrying a `revised_prompt` +
    /// generated image — not a generic image attachment. Modeling it as
    /// its own variant lets us keep `revised_prompt` and route to a
    /// dedicated renderer without title-string heuristics.
    ///
    /// Singular `image` (not Vec): codex-acp emits exactly one image per
    /// `ToolCall` (each `image_generation_begin/end` event pair has its
    /// own `call_id`). When a turn produces N images, the agent emits N
    /// separate ToolCalls — so the right unit is "one block, one image".
    /// `None` represents the in-flight placeholder state during streaming
    /// (`ImageGenerationBegin` arrived, `ImageGenerationEnd` hasn't yet).
    ImageGeneration {
        #[serde(skip_serializing_if = "Option::is_none")]
        revised_prompt: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        image: Option<ImageData>,
    },
    ToolUse {
        tool_use_id: Option<String>,
        tool_name: String,
        input_preview: Option<String>,
        /// ACP extensibility metadata associated with the tool call. The
        /// `delegate_to_agent` lifecycle writes
        /// `meta["codeg.delegation"] = { status, child_connection_id,
        /// child_conversation_id, error_code? }` here so a snapshot or DB
        /// re-fetch can re-bind the parent UI to the child conversation
        /// without depending on the live event stream having survived.
        ///
        /// `None` for tool uses without any meta (the agent didn't emit
        /// one, or the field predates the meta-on-ToolUse schema change).
        /// The shape is intentionally opaque — `serde_json::Value` —
        /// because the convention is agent-defined and may grow.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<serde_json::Value>,
    },
    ToolResult {
        tool_use_id: Option<String>,
        output_preview: Option<String>,
        is_error: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_stats: Option<AgentExecutionStats>,
        /// Images returned in a tool result (e.g. Claude Code's `Read` of a
        /// PNG/JPEG, or a multi-page PDF read returning one image per page).
        /// The agent JSONL embeds these as base64 `image` content blocks; the
        /// adapter renders them in-position as image cards so the historical
        /// (JSONL replay) path matches the live ACP stream, which surfaces the
        /// same bytes via `ToolCallState.images`. Empty for the common
        /// text-only tool result.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageData>,
    },
    Thinking {
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TurnUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning / thinking effort for this message when the agent archive
    /// records it (e.g. Codex `turn_context.payload.effort`). Absent for
    /// agents that only keep session-level effort or do not persist it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Wall-clock time the message finished. Each parser sets this to the
    /// best end-marker it has access to (e.g. Codex's `token_count` event,
    /// OpenCode's `completed_ms`, or just the event-log `timestamp` for
    /// agents that log post-generation). Crucially this is NOT computed as
    /// `timestamp + duration_ms` — those two fields encode unrelated spans
    /// in most parsers and adding them produces wrong completion times.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
    System,
}

/// Closed wire value for `TurnOutcome.status` (`"interrupted"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcomeStatus {
    Interrupted,
}

/// Closed wire value for interrupted-turn `stop_reason` (`"cancelled"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcomeStopReason {
    Cancelled,
}

/// Closed origin for user-initiated stop (wire: `"user_stop"`).
///
/// Shared by `TurnOutcome.source` and `AcpEvent::TurnComplete.termination_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnTerminationSource {
    UserStop,
}

/// Terminal metadata for a turn that ended abnormally (e.g. user Stop).
///
/// Optional on `MessageTurn`; absent means legacy/normal completion.
/// Not a content block — presentation renders it as footer metadata only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnOutcome {
    /// Wire value today: `"interrupted"`.
    pub status: TurnOutcomeStatus,
    /// Wire value for user-stop path: `"cancelled"`.
    pub stop_reason: TurnOutcomeStopReason,
    /// Origin of the interruption when known (wire: `"user_stop"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<TurnTerminationSource>,
    /// Provider-side turn id used as the reconcile fence key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<String>,
    /// Optional display timing; same ISO-8601 convention as `MessageTurn.completed_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTurn {
    pub id: String,
    pub role: TurnRole,
    pub blocks: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TurnUsage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Reasoning / thinking effort used for this turn when known from the
    /// agent archive (Codex per-turn `effort`, Grok session
    /// `reasoning_effort` stamped onto turns, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Wall-clock time the turn finished, propagated from the last
    /// `UnifiedMessage` absorbed into this turn. Not computed from
    /// `timestamp + duration_ms` — those fields encode unrelated spans in
    /// most parsers (event-log time vs. full turn span).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// Optional terminal outcome (e.g. user Stop interruption metadata).
    /// Absent on ordinary end_turn and all legacy payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TurnOutcome>,
}
