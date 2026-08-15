//! 会话级状态结构。后端权威：流式累积、in-flight tool calls、待处理 permission 等
//! 全部住在这里。Phase 2 的 snapshot 端点直接从此处读取 live 部分。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::acp::delegation::continuation::types::ContinuationWaitingProjection;
use crate::acp::delegation::route::{
    DelegationRoutePlan, DelegationRoutePolicy, DelegationRouteSource, RouteDegradedReason,
};
use crate::acp::event_stream::{ConnectionEventStream, RecentEventsBuffer};
use crate::acp::feedback::{FeedbackItem, FeedbackStatus};
use crate::acp::plan_approval::PendingPlanApprovalState;
use crate::acp::question::PendingQuestionState;
use crate::acp::types::{
    AcpEvent, AvailableCommandInfo, ConfigStaleKind, ConnectionStatus, EventEnvelope,
    GrokEffortSpec, PromptCapabilitiesInfo, SessionConfigOptionInfo, SessionModeStateInfo,
    ToolCallImageInfo,
};
use crate::auto_title::{ConnectionPurpose, TurnCompletionSnapshot};
use crate::models::agent::AgentType;
use crate::models::message::MessageRole;
use crate::models::system::AppLocale;

/// Opaque per-turn token + originating locale for automatic title coordination.
#[derive(Debug, Clone)]
pub struct ActiveTurnContext {
    pub token: String,
    pub locale: AppLocale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InternalPromptAdmission {
    pub continuation_id: String,
    pub continuation_generation: u64,
    pub internal_prompt_id: String,
    pub admitted_turn_generation: u64,
}

/// Immutable route plan plus the one mutable post-ready availability bit.
/// Carried on live snapshots so attach payloads have one stable shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRouteSnapshot {
    pub requested: DelegationRoutePolicy,
    pub effective: DelegationRoutePolicy,
    pub source: DelegationRouteSource,
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<RouteDegradedReason>,
    pub delegation_available: bool,
}

impl DelegationRouteSnapshot {
    /// Build from an immutable launch plan. Availability starts false until
    /// the authenticated ready lease succeeds (Codeg) or stays false (native).
    pub fn from_plan(plan: &DelegationRoutePlan, delegation_available: bool) -> Self {
        Self {
            requested: plan.requested,
            effective: plan.effective,
            source: plan.source,
            managed: plan.managed,
            degraded_reason: plan.degraded_reason,
            delegation_available,
        }
    }
}

/// Serde default for mixed-version clients: unmanaged native, unavailable.
pub fn legacy_unmanaged_route_snapshot() -> DelegationRouteSnapshot {
    DelegationRouteSnapshot {
        requested: DelegationRoutePolicy::Native,
        effective: DelegationRoutePolicy::Native,
        source: DelegationRouteSource::FeatureDisabled,
        managed: false,
        degraded_reason: None,
        delegation_available: false,
    }
}

/// 当前 streaming 中的 turn 的累积内容。turn 完成后清空。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveMessage {
    pub id: String,
    pub role: MessageRole,
    pub content: Vec<LiveContentBlock>,
    pub started_at: DateTime<Utc>,
}

/// 流式 turn 的内容块。事件按到达顺序追加。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveContentBlock {
    Text {
        text: String,
        /// Subagent attribution (`_meta.claudeCode.parentToolUseId`,
        /// claude-agent-acp ≥0.63 with `subagent-transcript` advertised).
        /// `None` = main-thread content. `default` keeps snapshots written
        /// by older backends parseable; skip-none keeps every other agent's
        /// snapshot byte-identical.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
    },
    Thinking {
        text: String,
        /// Same contract as `Text::parent_tool_use_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
    },
    ToolCallRef {
        tool_call_id: String,
    },
    Plan {
        entries: serde_json::Value,
    },
}

/// Final visible assistant answer from a live turn: `Text` blocks after the
/// last `ToolCallRef` only. No truncation; no thinking/tool fallback.
///
/// Shared by `TurnComplete` assembly of `last_assistant_text` and by auto-title
/// partials so both paths stay byte-identical. `None` or no concluding text
/// yields `""` (callers that store `Option` map trim-empty → `None`).
pub fn visible_assistant_text(live: Option<&LiveMessage>) -> String {
    let Some(live) = live else {
        return String::new();
    };
    let after_last_tool_call = live
        .content
        .iter()
        .rposition(|b| matches!(b, LiveContentBlock::ToolCallRef { .. }))
        .map(|i| i + 1)
        .unwrap_or(0);
    live.content[after_last_tool_call..]
        .iter()
        .filter_map(|b| match b {
            LiveContentBlock::Text {
                text,
                parent_tool_use_id: None,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// 工具调用的运行态。turn 完成时统一 clear。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallState {
    pub id: String,
    pub kind: ToolKind,
    pub label: String,
    pub status: ToolCallStatus,
    pub input: Option<serde_json::Value>,
    pub output: Option<ToolCallOutput>,
    /// Latest rendered content blocks reported by the agent (markdown / text).
    /// Distinct from `output` (which is the parsed `raw_output`); kept as the
    /// most recent value (replace-on-update, not append) for snapshot fidelity.
    pub content: Option<String>,
    /// File locations affected by this tool call (e.g. paths of edits).
    /// Forwarded verbatim from the agent's ToolCall/ToolCallUpdate event.
    /// `None` if the agent didn't supply it. Partial-update preservation:
    /// an incoming `None` from a `ToolCallUpdate` (which typically carries
    /// only changed fields) must NOT clobber a previously-set value.
    pub locations: Option<serde_json::Value>,
    /// ACP extensibility metadata. Used by frontend Phase 1 parent
    /// extraction. `None` if the agent didn't supply it. Same partial-update
    /// preservation semantic as `locations`.
    ///
    /// Convention used by codeg's multi-agent delegation (the `delegate_to_agent`
    /// MCP tool) — `DelegationBroker` writes the following object under
    /// `meta["codeg.delegation"]` on the parent's active tool call:
    ///
    /// ```jsonc
    /// {
    ///   "child_connection_id": "<uuid>",
    ///   "child_conversation_id": <i32>,
    ///   "status": "pending" | "running" | "completed" | "failed"
    /// }
    /// ```
    ///
    /// The frontend reads this to render "Delegating to <agent>…" on the live
    /// tool-call, and to anchor the inline `<DelegatedSubThread>` to the
    /// correct child conversation.
    pub meta: Option<serde_json::Value>,
    /// Latest images attached to this tool call (e.g. codex-acp v0.14+
    /// image generation). Replace-on-update semantics matching `content`:
    /// a fresh `ToolCallUpdate` carrying `Some(images)` replaces the prior
    /// vec, `None` preserves it. Persisted on snapshot so a frontend
    /// reconnecting mid-turn or after refresh sees the same image that was
    /// streamed live. ⚠ base64 image data can be multi-MB per entry; the
    /// snapshot endpoint payload grows accordingly. This is the cost of
    /// surviving page refresh without re-fetching from JSONL.
    #[serde(default)]
    pub images: Vec<ToolCallImageInfo>,
    /// 流式拼接的 input chunks（serde 不输出，仅运行时用）
    #[serde(skip)]
    pub raw_input_chunks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// 工具种类。沿用 ACP 协议层枚举。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

/// 工具调用输出。可能是文本、错误、结构化结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolCallOutput {
    Text { content: String },
    Error { message: String },
    Json { value: serde_json::Value },
}

/// 待处理的权限请求。重连后从 SessionState 恢复，跨 UI 关闭不丢。
/// 注意：与 chat_channel::PendingPermission 不同（后者有 sent_message_id）。
///
/// `tool_call` 是 agent 原样转发的 JSON——保留 rawInput / content / locations /
/// patch / plan 等所有结构，前端 `parsePermissionToolCall` 依赖它来渲染 diff、
/// shell 命令、plan 列表等审批必备信息。压成 `description: String` 那种摘要
/// 字符串会让"刷新后继续审批"变成"盲签"。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPermissionState {
    pub request_id: String,
    pub tool_call_id: String,
    pub tool_call: serde_json::Value,
    pub options: Vec<crate::acp::types::PermissionOptionInfo>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub queued: u32,
}

/// 上下文 / 模型用量。
/// Snapshot of the most recent `AcpEvent::Error`. Carried on
/// `SessionState` so post-mortem readers (e.g. the delegation-settings
/// probe) can surface the agent's own error after the connection task
/// has already cleaned up.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionLastError {
    pub message: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsageInfo {
    pub used: u64,
    pub size: u64,
}

/// Snapshot-recoverable record of an IN-FLIGHT (running) sub-agent delegation,
/// keyed (in `SessionState.active_delegations`) by the parent's
/// `parent_tool_use_id`.
///
/// This is the live "currently delegating" SET, not a history log:
/// `DelegationStarted` inserts an entry; `DelegationCompleted` REMOVES it. So
/// its size tracks live concurrency (bounded by what the machine actually runs)
/// — there is no cap and no cumulative growth over the parent connection's
/// lifetime.
///
/// Completed delegations are recovered without this field: a live page keeps the
/// binding in `DelegationProvider` for its lifetime, and a cold load / refresh
/// rebuilds `meta["codeg.delegation"]` (status + child id) from the child's
/// persisted DB row via `commands::conversations::inject_delegation_meta`
/// (authoritative, uncapped). The snapshot only has to recover the *running*
/// binding, which the transient `DelegationStarted` event cannot supply on the
/// snapshot attach path (cold attach, lagged re-attach, refresh) — that gap is
/// exactly what this field closes.
///
/// UNLIKE `active_tool_calls`, entries are NOT cleared on `TurnComplete`: an
/// async delegation's child runs in the background long after the parent's
/// `delegate_to_agent` tool call returns and the parent turn completes. The
/// broker emits `DelegationStarted`/`DelegationCompleted` only for a REAL
/// (non-synthetic) `parent_tool_use_id`, so synthetic-fallback cards never
/// create a phantom entry here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveDelegationState {
    pub parent_tool_use_id: String,
    pub child_connection_id: String,
    pub child_conversation_id: i32,
    pub agent_type: AgentType,
    /// Bounded task text preview mirrored from `DelegationStarted` so a
    /// snapshot re-attach can label identity-less parent tool calls.
    #[serde(default)]
    pub task_preview: String,
    /// Durable Broker task id — guards runtime/attention replacements so a
    /// stale event for a previous task on the same tool id cannot clobber
    /// the live card.
    pub task_id: String,
    /// Authoritative accepted-start timestamp (rebased from durable child row).
    pub started_at: DateTime<Utc>,
    /// Latest projected runtime rollup for the visible card.
    pub runtime_stats: crate::acp::delegation::runtime_stats::DelegationRuntimeStats,
    /// Open parent-decision request, if any. Cleared by
    /// `DelegationAttentionChanged { attention_request: None }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_request: Option<crate::acp::delegation::attention::AttentionRequestSummary>,
    /// Soft-watchdog health for this still-running card. Absent until the
    /// supervisor publishes; cleared only when the card is removed on complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<crate::acp::delegation::types::TaskObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agent_activity_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_since: Option<DateTime<Utc>>,
}

/// The in-flight user prompt for the current turn. Captured from
/// `AcpEvent::UserMessage` into `SessionState.pending_user_message` and carried
/// on `to_snapshot()` so a client attaching mid-turn can render the user turn
/// even though the one-shot `UserMessage` event won't replay for it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PendingUserMessage {
    pub message_id: String,
    pub blocks: Vec<crate::acp::types::UserMessageBlock>,
}

/// 后端权威的会话状态。每个 AgentConnection 持有一个 Arc<RwLock<SessionState>>。
///
/// 字段范围：仅当前 turn 的 in-flight 数据 + 元信息 + 协商出的能力。
/// 已完成的 turn 不存在这里——它们由 parser 从 agent JSONL 读。
#[derive(Debug)]
pub struct SessionState {
    // 身份
    pub connection_id: String,
    /// Immutable host UUID minted at AgentConnection spawn. Owner-window rebind
    /// does **not** change this; reconnect/replacement mints a new value.
    pub connection_incarnation: String,
    /// Process-scoped lease registry Arc (owned by ConnectionManager, cloned here
    /// so the connection loop can attribute progress without map lookups).
    pub(crate) tool_lease_registry:
        std::sync::Arc<crate::acp::tool_watchdog::ToolExecutionLeaseRegistry>,
    /// Process-scoped MCP cancel token registry (same ownership model as leases).
    pub(crate) mcp_cancel_registry: std::sync::Arc<crate::acp::tool_watchdog::McpCancelRegistry>,
    pub conversation_id: Option<i32>,
    pub external_id: Option<String>,
    /// Wall-clock instant `external_id` last CHANGED value (SessionStarted
    /// for a new/loaded/forked session). The transcript watcher uses this as
    /// its re-arm epoch: records appended to the (forked) transcript between
    /// the session change and the watcher's next poll tick must still count
    /// as this session's — an epoch taken at the tick itself would classify
    /// them as copied history and drop them.
    pub external_id_changed_at: Option<std::time::SystemTime>,
    pub agent_type: AgentType,
    pub working_dir: Option<PathBuf>,
    pub owner_window_label: String,
    pub folder_id: Option<i32>,
    /// Shared-session broker projection retained across same-generation route
    /// fallback. Public snapshots expose only this redacted DTO.
    pub shared_session: Option<crate::acp::shared_session::SharedSessionProjection>,

    // 状态
    pub status: ConnectionStatus,
    pub live_message: Option<LiveMessage>,
    pub active_tool_calls: BTreeMap<String, ToolCallState>,
    pub pending_permission: Option<PendingPermissionState>,

    /// The agent's in-flight `ask_user_question` (one set of multiple-choice
    /// questions awaiting the user's answer). Set by `QuestionRequest`, cleared
    /// by a matching `QuestionResolved` (and defensively on `TurnComplete` /
    /// `UserMessage`). Carried on `to_snapshot()` so a client attaching mid-turn
    /// re-renders the interactive card the one-shot event won't replay for it.
    /// At most one is pending at a time (the agent is blocked in the tool call);
    /// the backend's `pending_questions` registry keys the answer one-shot.
    pub pending_question: Option<PendingQuestionState>,

    /// Durable suspension projection for a parent waiting on delegated work.
    pub waiting_for_subagents: Option<ContinuationWaitingProjection>,

    /// The agent's in-flight Grok `exit_plan_mode` approval (the plan awaiting the
    /// user's Approve / Request-changes / Abandon decision). Set by
    /// `PlanApprovalRequest`, cleared by a matching `PlanApprovalResolved` (and
    /// defensively on `TurnComplete`). Carried on `to_snapshot()` so a client
    /// attaching mid-turn re-renders the approval card the one-shot event won't
    /// replay for it. At most one is pending (the agent is blocked in its
    /// `exit_plan_mode` tool call); the connection parks the ext responder keyed
    /// by `approval_id`.
    pub pending_plan_approval: Option<PendingPlanApprovalState>,

    /// In-flight (running) sub-agent delegations keyed by `parent_tool_use_id`.
    /// `DelegationStarted` inserts; `DelegationCompleted` removes. UNLIKE
    /// `active_tool_calls`, NOT cleared on `TurnComplete` (an async delegation
    /// outlives the parent turn). Carried on `to_snapshot()` so a web/server
    /// attach on the snapshot path (cold attach, lagged re-attach, refresh) can
    /// recover the running parent↔child binding the transient `DelegationStarted`
    /// event can't supply there. Size tracks live concurrency — no cap, no
    /// cumulative growth; completed delegations are recovered from the child's
    /// persisted DB row, not from here. See `ActiveDelegationState`.
    pub active_delegations: BTreeMap<String, ActiveDelegationState>,

    /// Currently actionable tool-execution watchdog projections keyed by
    /// `lease_id`. Upserted by `ToolWatchdogChanged` for warning / grace /
    /// cancelling only; removed on `cleared` or `timed_out` (per-lease,
    /// version-aware). Terminal diagnostics (`timed_out`) are events, not a
    /// durable map ledger — capacity tracks live actionable leases only.
    /// Never evict Warning/Grace/Cancelling to make room for siblings. Carried
    /// on `to_snapshot()` so attach/replay restores every concurrent Grace
    /// control surface.
    pub tool_watchdog_projections:
        BTreeMap<String, crate::acp::tool_watchdog::ToolWatchdogProjection>,

    /// Per-lease max projection version accepted (including after terminal
    /// remove). Prevents a late lower-version `Cancelling` emission from
    /// resurrecting a banner after `TimedOut`/`Cleared` cleared the map.
    /// Carried on `to_snapshot()` so cold attach/replay seeds full FE
    /// tombstones across multi-lease terminal history (not only the latest
    /// `last_tool_watchdog_diagnostic`).
    tool_watchdog_max_versions: BTreeMap<String, u64>,

    /// Latest secret-safe watchdog transition for session-details diagnostics.
    /// Retained after `timed_out` / `cleared` remove the lease from the
    /// actionable map so reattach still shows the most recent transition
    /// (ordered by `transition_at`, not per-lease version).
    pub last_tool_watchdog_diagnostic: Option<crate::acp::tool_watchdog::ToolWatchdogProjection>,

    /// Live user-feedback ("steering") notes for the current turn. Appended by
    /// `FeedbackSubmitted` (a user note while the agent works), flipped to
    /// `Delivered` by `FeedbackConsumed` (the agent read them via the
    /// `check_user_feedback` MCP tool), and cleared on the next turn's
    /// `UserMessage` (notes are turn-scoped steering, not durable history).
    /// Carried on `to_snapshot()` so a client attaching mid-turn renders the
    /// pending notes the one-shot `FeedbackSubmitted` event won't replay for it.
    /// Size is human-bounded (one entry per note the user types this turn).
    pub feedback: Vec<FeedbackItem>,

    /// Launched-but-unresolved background tasks (async sub-agents + background
    /// shell tasks), mirrored from the transcript watcher's authoritative
    /// accounting via `AcpEvent::BackgroundActivity` (`apply_event` is the only
    /// writer). Drives `has_active_background_work()` — the idle-sweep
    /// exemption that keeps the agent CLI alive through a silent background
    /// build (killing the connection kills the CLI, and the background work
    /// dies with it). Carried on `to_snapshot()` so a client attaching
    /// mid-episode recovers the pending count without replaying events.
    pub background_outstanding: u32,
    /// Instant of the most recent `BackgroundActivity` event. Bounds the sweep
    /// exemption: if the watcher stops reporting (task died, bug) the
    /// exemption lapses after `background_keepalive_max_age()` instead of
    /// pinning the connection alive forever. Backend-internal; not serialized.
    pub background_activity_at: Option<DateTime<Utc>>,

    // ACP 协商出的能力
    pub modes: Option<SessionModeStateInfo>,
    pub current_mode: Option<String>,
    pub config_options: Option<Vec<SessionConfigOptionInfo>>,
    /// Grok only: per-model reasoning-effort specs, parsed from the top-level
    /// `models` of the session-establishment response (guaranteed on
    /// `session/new`; opportunistic on resume/fork). Grok never re-sends this on
    /// `set_model`, so it is cached here to rebuild the composer's effort
    /// selector for the target model on a mid-session model switch. `None` for
    /// non-Grok agents and when the response carried no `models` (flat fallback).
    /// Backend-internal — not serialized.
    pub grok_effort_specs: Option<std::collections::HashMap<String, GrokEffortSpec>>,
    pub prompt_capabilities: Option<PromptCapabilitiesInfo>,
    pub fork_supported: bool,
    pub available_commands: Vec<AvailableCommandInfo>,
    pub usage: Option<UsageInfo>,
    /// True once the agent's initial selectors handshake (modes +
    /// config_options) has finished and `SelectorsReady` has fired. Persisted
    /// on the snapshot so a frontend that reconnects after refresh can see
    /// "init complete" without waiting for an event that already fired.
    pub selectors_ready: bool,

    /// Most recent unresolved `AcpEvent::Error` payload. Cleared when a new
    /// prompt starts, matching the frontend reducer's live-event behavior. The
    /// probe path reads this after `wait_for_session_options` errors so it can
    /// fold the agent's own error message into the returned `AcpError` instead
    /// of surfacing a generic "connection not found" once the connection task
    /// has cleaned up its map entry.
    ///
    /// Exposed on `to_snapshot()` so clients that reconnect after missing the
    /// live `AcpEvent::Error` can still surface the latest agent failure.
    pub last_error: Option<SessionLastError>,

    /// Single-fire signal that fires when `SessionStarted` applies (i.e.
    /// `external_id` transitioned from None → Some). `ConnectionManager::
    /// spawn_agent` holds the per-(agent, working_dir, session_id) dedup
    /// lock until this fires (or times out), so a concurrent acp_connect
    /// for the same logical session sees the populated `external_id` and
    /// reuses instead of spawning a duplicate. `Some` immediately after
    /// `install_session_started_signal()`; `take()`'d in `apply_event::
    /// SessionStarted`; `None` thereafter (the signal is one-shot per
    /// connection). Lives only on the in-memory `SessionState`; not
    /// transmitted on the wire (`LiveSessionSnapshot` doesn't include it).
    pub(crate) session_started_tx: Option<tokio::sync::oneshot::Sender<()>>,

    // 事件锚点
    pub event_seq: u64,
    /// Idle-sweep / general liveness timestamp. Bumped on every emit and
    /// frontend keepalive. **Not** the soft-watchdog agent activity clock.
    pub last_activity_at: DateTime<Utc>,

    /// Soft-watchdog agent activity clock. Advanced only by normalized Agent
    /// transcript/thinking, tool start/update/progress, and plan activity (or
    /// first successful child prompt enqueue). Independent of `last_activity_at`.
    pub last_agent_activity_at: DateTime<Utc>,

    /// Optional wake handle for the soft supervisor. Default is noop so unit
    /// tests that never install a wake stay silent. Not serialized / not on
    /// wire snapshots.
    pub(crate) supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake,

    /// Per-connection event broadcaster used by the WS attach protocol.
    /// New subscribers register receivers here while holding the SessionState
    /// read lock; `emit_with_state` broadcasts after releasing the write
    /// lock. Wrapped in `Arc` so subscriber tasks can hold a reference
    /// independent of the SessionState lock.
    pub(crate) event_stream: Arc<ConnectionEventStream>,

    /// Bounded ring buffer of recent envelopes (most-recent-last). Pushed
    /// by `emit_with_state` inside the write-lock critical section, kept in
    /// strict lockstep with `event_seq`. Read by attach handlers under the
    /// read lock to decide between sending a snapshot or a batched replay.
    /// See `event_stream` module for size limits.
    pub(crate) recent_events: RecentEventsBuffer,

    /// Per-launch token registered with the delegation broker's
    /// `TokenRegistry` when `codeg-mcp` is injected at init.
    /// Revoked when the connection tears down so a leaked binary can't
    /// keep round-tripping after the parent session ends.
    pub delegation_token: Option<String>,

    /// Whether the `check_user_feedback` MCP tool was exposed to THIS agent at
    /// launch (the `feedback` feature was on when its companion was injected).
    /// Fixed for the connection's lifetime — tool exposure can't change after
    /// launch. The authoritative gate for both the submit path and the UI: a
    /// session started before the feature was enabled has no tool, so notes
    /// would strand; one started after has it. Carried on `to_snapshot()` so the
    /// frontend gates the feedback bar on the agent's actual capability, not the
    /// (possibly later-toggled) global setting.
    pub feedback_tool_available: bool,

    /// Concatenated text content of the just-completed turn's assistant
    /// message. Captured at TurnComplete (just before live_message is
    /// cleared) so the lifecycle subscriber can surface it as the
    /// `delegation_call_id`-bound child outcome. Cleared on the next prompt.
    pub last_assistant_text: Option<String>,

    /// The in-flight user prompt for the current turn, captured from
    /// `AcpEvent::UserMessage` and cleared on `TurnComplete` (alongside
    /// `live_message`). Carried on `to_snapshot()` so a client attaching
    /// mid-turn renders the user turn even though no `UserMessage` event will
    /// replay for it. `None` outside an active turn.
    pub pending_user_message: Option<PendingUserMessage>,

    /// Backend wall-clock instant the in-flight turn started, captured alongside
    /// `pending_user_message` from `AcpEvent::UserMessage` and cleared on
    /// `TurnComplete`. The detail endpoint uses it to tell the in-flight prompt
    /// — persisted at/after this instant by the agent CLI, a local subprocess
    /// sharing this machine's clock — apart from a prior identical prompt
    /// persisted during an earlier turn (see `apply_in_flight_message_id`). Not
    /// serialized: backend-internal, like `turn_in_flight`. `None` outside an
    /// active turn.
    pub pending_user_message_started_at: Option<DateTime<Utc>>,

    /// True between a prompt being accepted (enqueued to the connection loop)
    /// and that turn completing. Set by the manager BEFORE the enqueue (so it
    /// is guaranteed set before the loop can dequeue) and cleared on
    /// `TurnComplete`. The manager rejects a second prompt with
    /// `AcpError::TurnInProgress` while this is set — otherwise the second
    /// `Prompt` would queue behind the active turn and be silently dropped by
    /// the loop's in-turn command handler (`_ => {}`), with the caller still
    /// seeing success. Not serialized: it is a connection-loop liveness flag,
    /// not part of the client-visible snapshot.
    pub turn_in_flight: bool,

    /// Connection-local Codex provider turn id from
    /// `session_info_update._meta.codex.activeTurnId`.
    ///
    /// Not DB-persisted and not part of live snapshots. Accepted only while
    /// `turn_in_flight` (including while a suspension lease is held for the
    /// same prompt). Snapshotted onto user-stop `TurnComplete.provider_turn_id`
    /// and cleared on every terminal finalization; retained across
    /// `DelegationSuspended` only. Not serialized: backend-internal, like
    /// `turn_in_flight`.
    pub active_provider_turn_id: Option<String>,

    /// Monotonic, connection-lifetime parent-turn fence. Internal only: never
    /// copied into live snapshots or public events.
    pub parent_turn_generation: u64,
    /// Generation currently owned by the active prompt, if any.
    pub active_turn_generation: Option<u64>,
    /// Last generation whose bound prompt response completed a suspension.
    pub last_suspended_turn_generation: Option<u64>,
    /// Session-scoped dedup fence for a continuation's internal wake prompt.
    #[allow(dead_code)] // Task 6 installs and consumes this session-scoped fence.
    pub(crate) last_internal_prompt_admission: Option<InternalPromptAdmission>,
    /// Whether the most recently completed turn ended via a stop reason other
    /// than `"end_turn"` (cancelled, refusal, max_tokens, max_turn_requests,
    /// empty, unknown — the same "abnormal ending" bucket `connection.rs`
    /// already treats uniformly for cascade-cancelling child delegations). Set
    /// by `AcpEvent::TurnComplete`, alongside `pending_user_message`/
    /// `turn_in_flight` clearing. The transcript watcher reads this at the
    /// Prompting→Connected falling edge: an abnormal ending means the turn's
    /// content never reached the wire (the ACP call was torn down before a
    /// held sub-agent's real completion), so `current_turn_launched_ids`
    /// must release immediately instead of waiting for the next turn — that
    /// content has nowhere else to render. Not serialized: backend-internal,
    /// like `turn_in_flight`.
    pub last_turn_ended_abnormally: bool,

    /// True when the agent's effective settings changed after this connection
    /// was spawned — the running process is still on its launch-time config and
    /// needs a restart to pick up the change. Set/cleared by
    /// `AcpEvent::SessionConfigStale` (emitted from
    /// `ConnectionManager::refresh_connection_staleness` after a settings save).
    /// Carried on `to_snapshot()` so a client attaching via the snapshot path
    /// (web reconnect, window refresh, a newly-tiled panel) sees the staleness
    /// the transient event won't replay for it.
    pub config_stale: bool,
    /// Which settings surface drifted, for the banner's wording. `Some` iff
    /// `config_stale`; reset to `None` when staleness clears.
    pub config_stale_kind: Option<ConfigStaleKind>,

    /// Managed route snapshot: immutable plan fields + mutable
    /// `delegation_available`. New sessions always supply a real plan-derived
    /// value; wire default for legacy payloads is unmanaged native unavailable.
    pub delegation_route: DelegationRouteSnapshot,

    /// Why this connection was launched (user, delegation, internal probe/title).
    /// Internal purposes bypass title capture on prompt admission.
    pub purpose: ConnectionPurpose,

    /// Latest resolved title/locale for this connection. Initialized from the
    /// launch context's inherited locale (English when absent) and refreshed on
    /// every accepted capture.
    pub effective_locale: AppLocale,

    /// In-flight turn capture context set only after successful admission
    /// capture (linked, non-internal). `None` outside an admitted turn or when
    /// capture was bypassed/failed.
    pub active_turn: Option<ActiveTurnContext>,
}

impl SessionState {
    pub fn new(
        connection_id: String,
        agent_type: AgentType,
        working_dir: Option<PathBuf>,
        owner_window_label: String,
        folder_id: Option<i32>,
    ) -> Self {
        Self {
            connection_id,
            // Production spawn overwrites both with the manager-owned registry
            // and a spawn-time incarnation shared with AgentConnection.
            connection_incarnation: uuid::Uuid::new_v4().to_string(),
            tool_lease_registry: std::sync::Arc::new(
                crate::acp::tool_watchdog::ToolExecutionLeaseRegistry::new(
                    crate::acp::tool_watchdog::ToolWatchdogSettings::default(),
                ),
            ),
            mcp_cancel_registry: crate::acp::tool_watchdog::McpCancelRegistry::new_shared(),
            conversation_id: None,
            external_id: None,
            external_id_changed_at: None,
            agent_type,
            working_dir,
            owner_window_label,
            folder_id,
            shared_session: None,
            status: ConnectionStatus::Connecting,
            live_message: None,
            active_tool_calls: BTreeMap::new(),
            pending_permission: None,
            pending_question: None,
            waiting_for_subagents: None,
            pending_plan_approval: None,
            active_delegations: BTreeMap::new(),
            tool_watchdog_projections: BTreeMap::new(),
            tool_watchdog_max_versions: BTreeMap::new(),
            last_tool_watchdog_diagnostic: None,
            feedback: Vec::new(),
            background_outstanding: 0,
            background_activity_at: None,
            modes: None,
            current_mode: None,
            config_options: None,
            grok_effort_specs: None,
            prompt_capabilities: None,
            fork_supported: false,
            available_commands: Vec::new(),
            usage: None,
            selectors_ready: false,
            last_error: None,
            session_started_tx: None,
            event_seq: 0,
            last_activity_at: Utc::now(),
            last_agent_activity_at: Utc::now(),
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            event_stream: Arc::new(ConnectionEventStream::new()),
            recent_events: RecentEventsBuffer::new(),
            delegation_token: None,
            feedback_tool_available: false,
            last_assistant_text: None,
            pending_user_message: None,
            pending_user_message_started_at: None,
            turn_in_flight: false,
            active_provider_turn_id: None,
            parent_turn_generation: 0,
            active_turn_generation: None,
            last_suspended_turn_generation: None,
            last_internal_prompt_admission: None,
            last_turn_ended_abnormally: false,
            config_stale: false,
            config_stale_kind: None,
            // Placeholder until spawn installs the real plan snapshot; tests
            // that never set a plan still deserialize/serialize as legacy default.
            delegation_route: legacy_unmanaged_route_snapshot(),
            // Test/helper default: User purpose + English. Production spawn paths
            // overwrite these from `ConnectionLaunchContext` (Task 4B temporary
            // defaults; Task 4C wires real persisted/channel/parent locales).
            purpose: ConnectionPurpose::User,
            effective_locale: AppLocale::En,
            active_turn: None,
        }
    }

    /// Install a fresh driver-owned connection state while retaining the one
    /// public session identity and replay stream already held by attachers.
    pub fn prepare_registered_replacement(&mut self, replacement: SessionState) {
        assert_eq!(
            self.connection_id, replacement.connection_id,
            "registered replacement must retain the public connection id"
        );

        let previous = std::mem::replace(self, replacement);
        self.conversation_id = previous.conversation_id;
        self.folder_id = previous.folder_id;
        self.shared_session = previous.shared_session;
        self.event_seq = previous.event_seq;
        self.event_stream = previous.event_stream;
        self.recent_events = previous.recent_events;
        self.status = ConnectionStatus::Connecting;
    }

    /// Clear only the active turn fenced by `generation`, retaining session
    /// identity, history/route state, live delegation projections, and the
    /// latest internal-prompt admission fence.
    pub fn clear_suspended_turn(&mut self, generation: u64) -> bool {
        if self.active_turn_generation != Some(generation) {
            return false;
        }

        self.last_suspended_turn_generation = Some(generation);
        self.active_turn_generation = None;
        self.active_turn = None;
        self.live_message = None;
        self.active_tool_calls.clear();
        self.pending_permission = None;
        self.pending_question = None;
        self.pending_user_message = None;
        self.pending_user_message_started_at = None;
        self.feedback.clear();
        self.turn_in_flight = false;
        self.supervisor_wake.notify();
        true
    }

    /// Mark real agent-side progress for the soft watchdog. Does **not** touch
    /// `last_activity_at` (idle sweep). Wakes the supervisor when a handle is set.
    pub fn mark_agent_activity(&mut self, at: DateTime<Utc>) {
        self.last_agent_activity_at = at;
        self.supervisor_wake.notify();
    }

    /// Build a turn stamp for the active generation, if any.
    pub fn tool_watchdog_turn_stamp(&self) -> Option<crate::acp::tool_watchdog::TurnStamp> {
        let turn_generation = self.active_turn_generation?;
        Some(crate::acp::tool_watchdog::turn_stamp(
            self.connection_id.clone(),
            self.connection_incarnation.clone(),
            self.external_id.clone().unwrap_or_default(),
            turn_generation,
        ))
    }

    /// Attribution facade sharing this connection's registry Arc.
    pub fn lease_attribution(&self) -> crate::acp::tool_watchdog::LeaseAttribution {
        crate::acp::tool_watchdog::LeaseAttribution::new(self.tool_lease_registry.clone())
    }

    /// Install the plan-derived route snapshot (availability still false until
    /// ready lease / native path marks connected without delegation).
    pub fn set_route_plan_snapshot(&mut self, plan: &DelegationRoutePlan) {
        self.delegation_route = DelegationRouteSnapshot::from_plan(plan, false);
    }

    /// Post-ready availability flip only — never mutates plan fields.
    pub fn set_delegation_available(&mut self, available: bool) {
        self.delegation_route.delegation_available = available;
    }

    /// Clone the broadcaster handle so attach handlers and subscriber tasks
    /// can hold an independent reference. Cheap (Arc clone).
    pub fn event_stream(&self) -> Arc<ConnectionEventStream> {
        Arc::clone(&self.event_stream)
    }

    /// Return events buffered after `since_seq`, or `None` if the cursor is
    /// older than what the ring buffer holds (caller must fall back to a
    /// snapshot). See `RecentEventsBuffer::range_after`.
    pub fn recent_events_after(&self, since_seq: u64) -> Option<Vec<Arc<EventEnvelope>>> {
        self.recent_events.range_after(since_seq)
    }

    /// Push an envelope into the ring buffer. Must be called under the
    /// write lock from `emit_with_state`, immediately after `event_seq`
    /// is incremented, so the buffer's tail seq matches `event_seq`.
    ///
    /// Returns the eviction count (events dropped from the buffer's head to
    /// stay within count/byte caps, plus any wholesale clear triggered by an
    /// oversized event). Caller propagates this into the
    /// `EventBusMetrics::ring_buffer_evict_count` counter.
    #[must_use = "evicted count feeds the ring_buffer_evict_count metric"]
    pub(crate) fn push_recent_event(&mut self, envelope: Arc<EventEnvelope>) -> usize {
        self.recent_events.push(envelope)
    }

    /// Install a one-shot signal that fires when `SessionStarted` applies.
    /// Returns the receiver; caller (typically `spawn_agent_connection`)
    /// passes it back to the dedup waiter in `spawn_agent`. Calling this
    /// more than once on the same state replaces the previous sender,
    /// silently dropping it — the contract is "exactly one install per
    /// connection lifetime" and that's what `spawn_agent_connection` does.
    pub fn install_session_started_signal(&mut self) -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.session_started_tx = Some(tx);
        rx
    }

    /// 单一分发器：把一个 AcpEvent 应用到 self。注意此方法**不**自增 event_seq——
    /// seq 由 emit_with_state 在外层管理（这样 apply_event 可独立单元测试）。
    ///
    /// Returns an event-owned [`TurnCompletionSnapshot`] only for `TurnComplete`
    /// when both `conversation_id` and `active_turn` are present. Built after
    /// assembling `last_assistant_text` and before clearing `active_turn`.
    pub fn apply_event(&mut self, payload: &AcpEvent) -> Option<TurnCompletionSnapshot> {
        let mut completion = None;
        match payload {
            AcpEvent::SessionStarted { session_id } => {
                if self.external_id.as_deref() != Some(session_id.as_str()) {
                    self.external_id_changed_at = Some(std::time::SystemTime::now());
                }
                self.external_id = Some(session_id.clone());
                self.status = ConnectionStatus::Connected;
                // Fire the dedup waiter (if any). Take()-and-send is
                // single-shot: a duplicate SessionStarted (replay, agent
                // re-init) finds None here and is a no-op, which is
                // exactly the desired idempotent behavior. send returns
                // Err only when the receiver dropped (timeout already
                // fired in spawn_agent) — also a no-op.
                if let Some(tx) = self.session_started_tx.take() {
                    let _ = tx.send(());
                }
            }
            AcpEvent::StatusChanged { status } => {
                // Diagnostic only (no behavior change): StatusChanged was
                // never logged anywhere, so there was no way to confirm from
                // the log alone whether a held-open turn (claude-agent-acp
                // v0.59.0's #870) actually stayed `Prompting` through an async
                // sub-agent's full lifecycle, or settled earlier than assumed.
                // The suppression filter reads live `Prompting` status and is
                // only correct if the hold behaves as documented.
                tracing::info!(
                    "[ACP] status_changed session={:?} {:?} -> {status:?}",
                    self.external_id,
                    self.status
                );
                if matches!(status, ConnectionStatus::Prompting) {
                    // Match the live frontend reducer: a new prompt starts a
                    // new error scope, so stale recoverable errors must not be
                    // resurrected by a later snapshot attach.
                    self.last_error = None;
                }
                self.status = status.clone();
            }
            AcpEvent::SharedSessionPhaseChanged { generation, phase } => {
                if let Some(shared) = self
                    .shared_session
                    .as_mut()
                    .filter(|shared| shared.generation == *generation)
                {
                    shared.phase = phase.clone();
                    self.status = phase.connection_status();
                }
            }
            AcpEvent::PromptQueued { generation, item } => {
                if let Some(shared) = self
                    .shared_session
                    .as_mut()
                    .filter(|shared| shared.generation == *generation)
                {
                    shared
                        .queue
                        .retain(|queued| queued.queue_item_id != item.queue_item_id);
                    shared.queue.push(item.clone());
                    shared.queue.sort_by_key(|queued| queued.enqueue_seq);
                }
            }
            AcpEvent::PromptQueueItemCancelled {
                generation,
                queue_item_id,
            }
            | AcpEvent::PromptQueueItemFailed {
                generation,
                queue_item_id,
                ..
            } => {
                if let Some(shared) = self
                    .shared_session
                    .as_mut()
                    .filter(|shared| shared.generation == *generation)
                {
                    shared
                        .queue
                        .retain(|queued| queued.queue_item_id != *queue_item_id);
                }
            }
            AcpEvent::PromptDispatchStarted { generation, turn } => {
                if let Some(shared) = self
                    .shared_session
                    .as_mut()
                    .filter(|shared| shared.generation == *generation)
                {
                    shared
                        .queue
                        .retain(|queued| queued.queue_item_id != turn.queue_item_id);
                    shared.active_turn = Some(turn.clone());
                }
            }
            AcpEvent::PromptQueueDepthChanged { .. } => {
                // Queue entries remain the authoritative replayable projection.
                // The aggregate depth event is intentionally presentation-only.
            }
            AcpEvent::SharedTurnSettled {
                generation,
                turn_id,
                ..
            } => {
                if let Some(shared) = self
                    .shared_session
                    .as_mut()
                    .filter(|shared| shared.generation == *generation)
                {
                    if shared
                        .active_turn
                        .as_ref()
                        .is_some_and(|turn| turn.turn_id == *turn_id)
                    {
                        shared.active_turn = None;
                    }
                }
            }
            AcpEvent::SessionModes { modes } => {
                self.current_mode = Some(modes.current_mode_id.clone());
                self.modes = Some(modes.clone());
            }
            AcpEvent::ModeChanged { mode_id } => {
                self.current_mode = Some(mode_id.clone());
                // Keep `modes.current_mode_id` consistent with the latched
                // `current_mode`. Snapshot consumers read `modes.current_mode_id`
                // directly (the frontend's `denormalizeSnapshot` does not look
                // at the separate `current_mode` field), so without this sync
                // a session that has switched modes would hydrate post-refresh
                // showing the original default — even though the live event
                // stream has long since corrected it.
                if let Some(modes) = self.modes.as_mut() {
                    modes.current_mode_id = mode_id.clone();
                }
            }
            AcpEvent::SessionConfigOptions { config_options } => {
                self.config_options = Some(config_options.clone());
            }
            AcpEvent::SessionConfigStale { stale, kind } => {
                self.config_stale = *stale;
                self.config_stale_kind = if *stale { Some(*kind) } else { None };
            }
            AcpEvent::DelegationAvailabilityChanged { available } => {
                self.delegation_route.delegation_available = *available;
            }
            AcpEvent::PromptCapabilities {
                prompt_capabilities,
            } => {
                self.prompt_capabilities = Some(prompt_capabilities.clone());
            }
            AcpEvent::ForkSupported { supported } => {
                self.fork_supported = *supported;
            }
            AcpEvent::AvailableCommands { commands } => {
                self.available_commands = commands.clone();
            }
            AcpEvent::UsageUpdate { used, size } => {
                self.usage = Some(UsageInfo {
                    used: *used,
                    size: *size,
                });
            }
            AcpEvent::ContentDelta {
                text,
                parent_tool_use_id,
            } => {
                // Subagent-attributed chunks accumulate only while the turn
                // is live. Out-of-turn parented chunks (an async subagent
                // still streaming after its parent turn settled) must not
                // resurrect a stale `live_message` via `ensure_live_message`
                // — a snapshot would then hand that ghost to every client
                // (the same disease the #870 held-turn work fenced off).
                // Main-thread chunks keep today's unconditional append.
                if parent_tool_use_id.is_none() || self.status == ConnectionStatus::Prompting {
                    self.append_text_delta(text, parent_tool_use_id.as_deref());
                }
            }
            AcpEvent::Thinking {
                text,
                parent_tool_use_id,
            } => {
                if parent_tool_use_id.is_none() || self.status == ConnectionStatus::Prompting {
                    self.append_thinking_delta(text, parent_tool_use_id.as_deref());
                }
            }
            AcpEvent::TurnAttemptRollback { .. } => {
                if let Some(live) = self.live_message.as_mut() {
                    let accepted_len = live
                        .content
                        .iter()
                        .rposition(|block| matches!(block, LiveContentBlock::ToolCallRef { .. }))
                        .map_or(0, |index| index + 1);
                    live.content.truncate(accepted_len);
                }
            }
            AcpEvent::ToolCall {
                tool_call_id,
                title,
                kind,
                status,
                content,
                raw_input,
                raw_output,
                locations,
                meta,
                images,
            } => {
                self.upsert_tool_call(
                    tool_call_id,
                    Some(kind),
                    Some(title),
                    Some(status),
                    content.as_deref(),
                    raw_input.as_deref(),
                    raw_output.as_deref(),
                    false,
                    locations.as_ref(),
                    meta.as_ref(),
                    images.as_deref(),
                );
                // Anchor the tool call in `live_message.content` so snapshot
                // reload preserves position relative to surrounding text /
                // thinking blocks. Idempotent by id: a second ToolCall (or a
                // ToolCallUpdate, see below) for the same id must not push a
                // duplicate ref. Mirrors text/thinking deltas in lazily
                // creating `live_message` if absent.
                self.push_tool_call_ref_if_absent(tool_call_id);
            }
            AcpEvent::ToolCallUpdate {
                tool_call_id,
                title,
                status,
                content,
                raw_input,
                raw_output,
                raw_output_append,
                locations,
                meta,
                images,
            } => {
                self.upsert_tool_call(
                    tool_call_id,
                    None,
                    title.as_deref(),
                    status.as_deref(),
                    content.as_deref(),
                    raw_input.as_deref(),
                    raw_output.as_deref(),
                    *raw_output_append == Some(true),
                    locations.as_ref(),
                    meta.as_ref(),
                    images.as_deref(),
                );
                // Defensive: if a ToolCallUpdate arrives before its initial
                // ToolCall (unusual ordering / replay), ensure the ref block
                // still gets anchored. Idempotent so the normal-flow case is
                // a no-op here.
                self.push_tool_call_ref_if_absent(tool_call_id);
            }
            AcpEvent::PermissionRequest {
                request_id,
                tool_call,
                options,
                queued,
            } => {
                let tc_id = extract_tool_call_id(tool_call);
                self.pending_permission = Some(PendingPermissionState {
                    request_id: request_id.clone(),
                    tool_call_id: tc_id,
                    tool_call: tool_call.clone(),
                    options: options.clone(),
                    created_at: Utc::now(),
                    queued: *queued,
                });
                // Waiting-input observation changes — wake soft supervisor.
                self.supervisor_wake.notify();
            }
            AcpEvent::PermissionQueueDepth { depth } => {
                if let Some(pending) = self.pending_permission.as_mut() {
                    pending.queued = *depth;
                }
            }
            AcpEvent::PermissionResolved { request_id } => {
                // Drop the snapshot's pending_permission iff the resolved
                // request matches the current one. Without the id check, a
                // late-arriving resolved event for an already-replaced
                // request could wipe the live dialog out from under the
                // user.
                if matches!(
                    &self.pending_permission,
                    Some(p) if p.request_id == *request_id,
                ) {
                    self.pending_permission = None;
                    self.supervisor_wake.notify();
                }
            }
            AcpEvent::QuestionRequest {
                question_id,
                questions,
            } => {
                self.pending_question = Some(PendingQuestionState {
                    question_id: question_id.clone(),
                    questions: questions.clone(),
                    created_at: Utc::now(),
                });
                self.supervisor_wake.notify();
            }
            AcpEvent::QuestionResolved { question_id } => {
                // Mirror `PermissionResolved`: only clear when the resolved id
                // matches the current one, so a late event for an already-
                // replaced question can't wipe a live card from under the user.
                if matches!(
                    &self.pending_question,
                    Some(p) if p.question_id == *question_id,
                ) {
                    self.pending_question = None;
                    self.supervisor_wake.notify();
                }
            }
            AcpEvent::PlanApprovalRequest {
                approval_id,
                tool_call_id,
                plan_markdown,
            } => {
                self.pending_plan_approval = Some(PendingPlanApprovalState {
                    approval_id: approval_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    plan_markdown: plan_markdown.clone(),
                    created_at: Utc::now(),
                });
            }
            AcpEvent::PlanApprovalResolved { approval_id } => {
                // Mirror `QuestionResolved`: only clear when the resolved id
                // matches the current one, so a late event for an already-
                // replaced approval can't wipe a live card from under the user.
                if matches!(
                    &self.pending_plan_approval,
                    Some(p) if p.approval_id == *approval_id,
                ) {
                    self.pending_plan_approval = None;
                }
            }
            AcpEvent::TurnComplete { stop_reason, .. } => {
                // Diagnostic only (no behavior change): pairs with the
                // StatusChanged log above. This is the ACTUAL point the turn
                // settles (`self.status` flips to `Connected` right below,
                // bypassing StatusChanged entirely) — needed to tell whether
                // claude-agent-acp v0.59.0's #870 held the turn open through
                // an async sub-agent's full lifecycle, or settled earlier.
                // `background_outstanding` at this instant shows whether a
                // sub-agent/shell the watcher still considers live was
                // outstanding when the ORIGINAL turn settled.
                tracing::info!(
                    "[ACP] turn_complete session={:?} stop_reason={stop_reason} background_outstanding={}",
                    self.external_id,
                    self.background_outstanding
                );
                // See `last_turn_ended_abnormally`'s doc comment: any reason
                // other than a normal end-of-turn means this turn's content
                // may never have reached the wire.
                self.last_turn_ended_abnormally = stop_reason != "end_turn";
                // Snapshot the just-finished turn's FINAL assistant text — what
                // `get_delegation_status` returns as the child result. Shared
                // with auto-title via `visible_assistant_text`: Text after the
                // last tool call only (no thinking/tool/subagent fallback).
                // Always re-assign so a missing `live_message` clears stale
                // text rather than leaking a prior turn's answer.
                let assembled = visible_assistant_text(self.live_message.as_ref());
                self.last_assistant_text = if assembled.trim().is_empty() {
                    None
                } else {
                    Some(assembled)
                };
                // Event-owned completion snapshot: capture under this lock after
                // text assembly and before clearing active_turn so lifecycle
                // never re-reads mutable SessionState for title context.
                completion = match (self.conversation_id, self.active_turn.as_ref()) {
                    (Some(conversation_id), Some(turn)) => {
                        let final_text: Arc<str> = match &self.last_assistant_text {
                            Some(text) => Arc::from(text.as_str()),
                            None => Arc::from(""),
                        };
                        Some(TurnCompletionSnapshot {
                            conversation_id,
                            turn_token: turn.token.clone(),
                            locale: turn.locale,
                            final_text,
                        })
                    }
                    _ => None,
                };
                self.active_turn = None;
                self.active_turn_generation = None;
                self.live_message = None;
                self.active_tool_calls.clear();
                // The turn's user prompt is no longer "in flight" — the
                // assistant reply is done and the transcript is the source of
                // truth. Clear it so a post-turn snapshot doesn't carry a stale
                // pending user message into a fresh attach.
                self.pending_user_message = None;
                self.pending_user_message_started_at = None;
                // Turn finished: release the concurrency gate so the next prompt
                // is accepted. (All connection-alive turn endings — normal,
                // cancel, stop-reason — emit TurnComplete; disconnect/error
                // discard the state entirely, so no stale flag can outlive them.)
                self.turn_in_flight = false;
                // Terminal finalizations clear the provider fence id so a later
                // Stop cannot reuse an old id. UserCancelled takes/snapshots
                // first; other TurnComplete paths clear here. DelegationSuspended
                // does not emit TurnComplete and intentionally retains the id.
                self.active_provider_turn_id = None;
                // NOTE: `active_delegations` is intentionally NOT cleared here.
                // A running delegation's child runs in the background long after
                // the parent's `delegate_to_agent` tool call returns and this
                // turn completes; clearing it would drop the running binding from
                // the snapshot the instant the parent turn ends (the original
                // web-only bug). It's removed per-entry by `DelegationCompleted`.
                let had_waiting =
                    self.pending_permission.is_some() || self.pending_question.is_some();
                self.pending_permission = None;
                // A blocked `ask_user_question` can't outlive its turn: if the
                // turn ends (cancel / stop) the card is moot. The backend's
                // answer one-shot is cleaned via the listener's peer-close race;
                // this just keeps the snapshot honest.
                self.pending_question = None;
                if had_waiting {
                    self.supervisor_wake.notify();
                }
                // Likewise a blocked `exit_plan_mode` approval: the parked ext
                // responder is drained by the connection's teardown/cancel path;
                // this just keeps the snapshot honest if the turn settles first.
                self.pending_plan_approval = None;
                self.status = ConnectionStatus::Connected;
            }
            AcpEvent::UserMessage { message_id, blocks } => {
                // Capture the in-flight user prompt so a client attaching
                // mid-turn renders the user turn from the snapshot (the
                // one-shot event won't replay for it). Cleared on TurnComplete.
                self.pending_user_message = Some(PendingUserMessage {
                    message_id: message_id.clone(),
                    blocks: blocks.clone(),
                });
                // Reference instant for the in-flight prompt's recency check in
                // `apply_in_flight_message_id`. Set here (not at manager enqueue)
                // so it tracks `pending_user_message` exactly. Truncated to
                // whole milliseconds: the gate compares this against parsed
                // turn timestamps that carry at most millisecond precision
                // (Cursor's journal upgrade rewrites the in-flight user turn
                // to a millisecond send stamp taken right after this event
                // applies — sub-ms residue here would push the threshold past
                // that stamp and unstamp the turn). The shed sub-ms window
                // cannot admit a prior identical prompt: no agent turn
                // round-trips in under a millisecond.
                let now = Utc::now();
                self.pending_user_message_started_at =
                    DateTime::from_timestamp_millis(now.timestamp_millis());
                // Live-feedback notes are turn-scoped steering: a new user turn
                // starts with a clean slate. The previous turn's notes (read or
                // not) are history at this point; the frontend's "agent didn't
                // read your note → resend" fallback already had its post-turn
                // window before this next prompt arrives.
                self.feedback.clear();
                // A new user turn supersedes any stale pending question.
                if self.pending_question.is_some() {
                    self.pending_question = None;
                    self.supervisor_wake.notify();
                }
                // Likewise a stale plan approval: a new turn started without a
                // clean TurnComplete (fork/resume re-prompt, error recovery, or a
                // queued prompt sent instead of answering) must not leave a dead
                // approval in the snapshot for a mid-turn attach to render.
                self.pending_plan_approval = None;
            }
            AcpEvent::ConversationLinked {
                conversation_id,
                folder_id,
                ..
            } => {
                self.conversation_id = Some(*conversation_id);
                self.folder_id = Some(*folder_id);
            }
            AcpEvent::PlanUpdate { entries } => {
                // Replace any existing Plan block, then append at end.
                // Mirrors the frontend's PLAN_UPDATE reducer semantic: there
                // is at most one plan block, always at the current end of
                // content. `Vec<PlanEntryInfo>` is converted to
                // `serde_json::Value` because the wire-side `Plan` variant
                // stores it opaquely (frontend casts back to PlanEntryInfo[]).
                let live = self.ensure_live_message();
                live.content
                    .retain(|b| !matches!(b, LiveContentBlock::Plan { .. }));
                live.content.push(LiveContentBlock::Plan {
                    entries: serde_json::to_value(entries).unwrap_or(serde_json::Value::Null),
                });
            }
            AcpEvent::ConversationStatusChanged { .. } => {
                // No-op on purpose. Conversation row `status` is row-level
                // metadata persisted by the lifecycle subscriber / send_prompt
                // path, not in-flight session state — snapshot consumers read
                // status via the conversation list endpoints, not via
                // `LiveSessionSnapshot`. Listed explicitly (rather than swept
                // up by the catchall) so the no-op is intentional and grep-able.
            }
            AcpEvent::ContinuationWaitingChanged {
                conversation_id,
                waiting,
            } => {
                if self.conversation_id == Some(*conversation_id) {
                    self.waiting_for_subagents = waiting.clone();
                }
            }
            AcpEvent::SelectorsReady => {
                // Latches once. Snapshot exposes this so a fresh frontend (e.g.
                // after browser refresh) can tell the initial handshake is
                // already done — the event fires only once per connection.
                self.selectors_ready = true;
            }
            AcpEvent::Error { message, code, .. } => {
                // Capture so post-mortem readers (probe path, debug
                // snapshots) can surface the agent's own error message
                // after the connection task has cleaned up its map
                // entry. The same payload is independently emitted
                // through the event channel for live chat-side UX.
                self.last_error = Some(SessionLastError {
                    message: message.clone(),
                    code: code.clone(),
                });
            }
            AcpEvent::DelegationStarted {
                parent_tool_use_id,
                child_connection_id,
                child_conversation_id,
                agent_type,
                task_preview,
                task_id,
                started_at,
                runtime_stats,
                attention_request,
                ..
            } => {
                // Record the full running card so the binding is snapshot-
                // recoverable (survives this connection's TurnComplete and any
                // re-attach on the snapshot path). The broker only emits this for
                // a REAL (non-synthetic) parent_tool_use_id, so synthetic-fallback
                // cards never create a phantom entry here — they rely on the
                // parent tool output (see DelegatedSubThread's ack fallback).
                self.active_delegations.insert(
                    parent_tool_use_id.clone(),
                    ActiveDelegationState {
                        parent_tool_use_id: parent_tool_use_id.clone(),
                        child_connection_id: child_connection_id.clone(),
                        child_conversation_id: *child_conversation_id,
                        agent_type: *agent_type,
                        task_preview: task_preview.clone(),
                        task_id: task_id.clone(),
                        started_at: *started_at,
                        runtime_stats: runtime_stats.clone(),
                        attention_request: attention_request.clone(),
                        observation: None,
                        last_agent_activity_at: None,
                        stalled_since: None,
                    },
                );
            }
            AcpEvent::DelegationRuntimeStatsChanged {
                parent_tool_use_id,
                task_id,
                runtime_stats,
            } => {
                // Replace-only: update an existing card whose task_id matches.
                // Unknown tool id or mismatched task id is ignored (replay-safe
                // for reordered / stale events).
                if let Some(card) = self.active_delegations.get_mut(parent_tool_use_id) {
                    if card.task_id == *task_id {
                        card.runtime_stats = runtime_stats.clone();
                    }
                }
            }
            AcpEvent::DelegationAttentionChanged {
                parent_tool_use_id,
                task_id,
                attention_request,
            } => {
                // Replace-only (including clear when attention_request is None).
                // Task-id guarded like runtime replacements.
                if let Some(card) = self.active_delegations.get_mut(parent_tool_use_id) {
                    if card.task_id == *task_id {
                        card.attention_request = attention_request.clone();
                    }
                }
            }
            AcpEvent::DelegationObservationChanged {
                parent_tool_use_id,
                task_id,
                observation,
                last_agent_activity_at,
                stalled_since,
            } => {
                // Observe-only: update an existing card. Never insert, remove,
                // or synthesize Completion from a health transition. Task-id
                // guarded so a stale observation cannot clobber a new task.
                if let Some(card) = self.active_delegations.get_mut(parent_tool_use_id) {
                    if card.task_id == *task_id {
                        card.observation = Some(*observation);
                        card.last_agent_activity_at = Some(*last_agent_activity_at);
                        card.stalled_since = *stalled_since;
                    }
                }
            }
            AcpEvent::DelegationCompleted {
                parent_tool_use_id,
                task_id,
                ..
            } => {
                // A running delegation finished: drop it from the live set only
                // when the task_id matches (guards against a late complete for a
                // prior task on a recycled tool id). Terminal state reaches the
                // LLM via `get_delegation_status` and the UI via the live event
                // or, on a cold load, the child's DB row (`inject_delegation_meta`).
                if let Some(card) = self.active_delegations.get(parent_tool_use_id) {
                    if card.task_id == *task_id {
                        self.active_delegations.remove(parent_tool_use_id);
                    }
                }
            }
            AcpEvent::FeedbackSubmitted { item } => {
                // Idempotent by id (replay / double-attach safe): append only if
                // this note isn't already tracked. The authoritative append is
                // here so snapshot replay reconstructs the same list the live
                // node holds.
                if !self.feedback.iter().any(|f| f.id == item.id) {
                    self.feedback.push(item.clone());
                }
            }
            AcpEvent::FeedbackConsumed { ids, delivered_at } => {
                // Flip the named pending notes to Delivered. Idempotent: an id
                // already Delivered (the emitting node marked it directly under
                // the write lock; this re-apply is for replay/attach nodes) is
                // skipped. Order-independent and safe to apply more than once.
                for f in self.feedback.iter_mut() {
                    if f.status == FeedbackStatus::Pending && ids.contains(&f.id) {
                        f.status = FeedbackStatus::Delivered;
                        f.delivered_at = Some(*delivered_at);
                    }
                }
            }
            AcpEvent::BackgroundActivity { outstanding, .. } => {
                // Mirror the watcher's authoritative accounting so the idle
                // sweeps can exempt this connection while background work is
                // pending. The turns/settled payloads are frontend-only; the
                // trailing `last_activity_at = now` below additionally resets
                // the backend idle timer on every batch of transcript activity.
                self.background_outstanding = *outstanding;
                self.background_activity_at = Some(Utc::now());
            }
            AcpEvent::ToolWatchdogChanged { projection } => {
                // Per-lease actionable map: upsert warning/grace/cancelling;
                // remove on cleared or timed_out (terminal events are not kept
                // as a durable ledger). Older versions never replace newer
                // projections so multi-window attach cannot regress CAS.
                // A tombstone of max version seen per lease survives terminal
                // remove so a late lower-version Cancelling cannot resurrect.
                // Separately, retain the latest transition for session-details
                // (including timed_out after map removal).
                use crate::acp::tool_watchdog::ToolWatchdogPhase;
                let floor = self
                    .tool_watchdog_max_versions
                    .get(&projection.lease_id)
                    .copied()
                    .unwrap_or(0);
                let in_map = self
                    .tool_watchdog_projections
                    .get(&projection.lease_id)
                    .map(|existing| existing.version);
                match projection.phase {
                    ToolWatchdogPhase::Cleared | ToolWatchdogPhase::TimedOut => {
                        // Accept when not older than the floor / live entry.
                        let accept = projection.version >= floor
                            && in_map.map(|v| projection.version >= v).unwrap_or(true);
                        if accept {
                            self.tool_watchdog_projections.remove(&projection.lease_id);
                            self.tool_watchdog_max_versions
                                .insert(projection.lease_id.clone(), projection.version);
                            self.remember_watchdog_diagnostic(projection);
                        }
                    }
                    ToolWatchdogPhase::Warning
                    | ToolWatchdogPhase::Grace
                    | ToolWatchdogPhase::Cancelling => {
                        // Reject strictly older versions. After terminal remove
                        // the tombstone floor blocks equal-version resurrection
                        // of a stale Cancelling that lost the emit race
                        // (`version == floor && not in map`).
                        let blocked_by_tombstone = projection.version < floor
                            || (projection.version == floor && in_map.is_none() && floor > 0);
                        let accept = !blocked_by_tombstone
                            && in_map.map(|v| projection.version >= v).unwrap_or(true);
                        if accept {
                            self.tool_watchdog_projections
                                .insert(projection.lease_id.clone(), projection.clone());
                            self.tool_watchdog_max_versions
                                .insert(projection.lease_id.clone(), projection.version);
                            self.remember_watchdog_diagnostic(projection);
                        }
                    }
                }
            }
            AcpEvent::ClaudeSdkMessage { .. }
            | AcpEvent::SessionLoadFailed { .. }
            | AcpEvent::TurnRetrying { .. }
            | AcpEvent::UserPromptSent { .. } => {
                // 这些事件不直接修改 SessionState 的可见字段。
                // UserPromptSent 是纯通知事件，仅供 chat-channel 推送消费。
                // TurnRetrying 与 Claude 的 api_retry 一样是前端瞬态提示（重试横幅），
                // 不进快照——回合边界会清除它。
            }
        }
        self.last_activity_at = Utc::now();
        completion
    }

    /// Whether this connection has launched background work (async sub-agent /
    /// background shell task) that hasn't settled yet — the idle sweeps must
    /// not reap it (disconnecting drops the `sacp` connection, which
    /// terminates the agent CLI process, which kills the background work).
    ///
    /// Bounded by `background_keepalive_max_age()`: the exemption requires a
    /// `BackgroundActivity` event within the window, so a wedged/dead watcher
    /// can't pin a connection alive forever. (The watcher itself also expires
    /// tasks past the same age and emits `outstanding: 0`, which resets
    /// `background_outstanding` here — this check is the belt to that
    /// suspenders.)
    pub fn has_active_background_work(&self, now: DateTime<Utc>) -> bool {
        if self.background_outstanding == 0 {
            return false;
        }
        match self.background_activity_at {
            Some(at) => now.signed_duration_since(at) < background_keepalive_max_age(),
            None => false,
        }
    }

    pub(crate) fn has_live_agent_output(&self) -> bool {
        self.live_message
            .as_ref()
            .is_some_and(|live| !live.content.is_empty())
            || !self.active_tool_calls.is_empty()
    }

    /// A single-line "what the sub-agent is doing right now" hint, used by the
    /// delegation broker so `get_delegation_status` can prove a running child is
    /// genuinely making progress instead of returning a bare "Running.".
    ///
    /// Reads the still-streaming `live_message` — unlike `last_assistant_text`,
    /// which is only snapshotted at `TurnComplete` and so is empty/stale while a
    /// turn is in flight. Preference order, each reduced to one trimmed line
    /// capped at `max_chars` chars (char-based → never splits a UTF-8 codepoint;
    /// an `…` marks truncation):
    ///
    /// 1. the answer-in-progress — `Text` after the last `ToolCallRef`, mirroring
    ///    the `TurnComplete` answer extraction;
    /// 2. else the latest `Thinking` block (`thinking: …`);
    /// 3. else the most recent tool call's label (`running tool: …`).
    ///
    /// `None` when the turn hasn't produced anything renderable yet.
    pub fn latest_live_reply(&self, max_chars: usize) -> Option<String> {
        let live = self.live_message.as_ref()?;

        // (1) Answer-in-progress: the `Text` after the last tool call.
        //
        // Consecutive text deltas merge into a single block (see
        // `append_text_delta`), so this is almost always ONE block — borrow it
        // and take its last non-empty line without copying a potentially large
        // streaming answer on every poll (this runs under the `SessionState`
        // read lock on the `get_delegation_status` path). Only when the answer
        // is split across multiple `Text` blocks (a `Thinking` block interleaved
        // mid-answer) do we stitch them, which is rare.
        let after_last_tool_call = live
            .content
            .iter()
            .rposition(|b| matches!(b, LiveContentBlock::ToolCallRef { .. }))
            .map(|i| i + 1)
            .unwrap_or(0);
        // Main-thread blocks only (`parent_tool_use_id: None`): a Claude
        // subagent's parented transcript chunks describe the CHILD's work and
        // must not surface as the parent's live reply.
        let mut texts = live.content[after_last_tool_call..]
            .iter()
            .filter_map(|b| match b {
                LiveContentBlock::Text {
                    text,
                    parent_tool_use_id: None,
                } => Some(text.as_str()),
                _ => None,
            });
        match (texts.next(), texts.next()) {
            (None, _) => {}
            (Some(only), None) => {
                if let Some(line) = last_nonempty_line(only) {
                    return Some(truncate_one_line(line, max_chars));
                }
            }
            (Some(first), Some(second)) => {
                let mut joined = String::with_capacity(first.len() + second.len());
                joined.push_str(first);
                joined.push_str(second);
                for rest in texts {
                    joined.push_str(rest);
                }
                if let Some(line) = last_nonempty_line(&joined) {
                    return Some(truncate_one_line(line, max_chars));
                }
            }
        }

        // (2) Latest main-thread thinking block — the agent is reasoning, not
        // silent. Parented (subagent) thinking is excluded for the same
        // reason as (1).
        if let Some(line) = live
            .content
            .iter()
            .rev()
            .find_map(|b| match b {
                LiveContentBlock::Thinking {
                    text,
                    parent_tool_use_id: None,
                } => Some(text.as_str()),
                _ => None,
            })
            .and_then(last_nonempty_line)
        {
            return Some(format!("thinking: {}", truncate_one_line(line, max_chars)));
        }

        // (3) Most recent tool call's label — work is happening in a tool.
        if let Some(label) = live
            .content
            .iter()
            .rev()
            .find_map(|b| match b {
                LiveContentBlock::ToolCallRef { tool_call_id } => Some(tool_call_id.as_str()),
                _ => None,
            })
            .and_then(|id| self.active_tool_calls.get(id))
            .map(|tc| tc.label.trim())
            .filter(|l| !l.is_empty())
        {
            return Some(format!(
                "running tool: {}",
                truncate_one_line(label, max_chars)
            ));
        }

        None
    }

    /// What, if anything, the session is currently blocked on waiting for the user.
    pub fn blocking_prompt(
        &self,
        max_chars: usize,
    ) -> Option<crate::acp::delegation::types::BlockedOn> {
        use crate::acp::delegation::types::{BlockedKind, BlockedOn};
        if let Some(p) = self.pending_permission.as_ref() {
            let title = p
                .tool_call
                .get("title")
                .and_then(|v| v.as_str())
                .and_then(last_nonempty_line)
                .map(|l| truncate_one_line(l, max_chars));
            return Some(BlockedOn {
                kind: BlockedKind::Permission,
                request_id: p.request_id.clone(),
                title,
            });
        }
        if let Some(q) = self.pending_question.as_ref() {
            let title = q
                .questions
                .first()
                .map(|first| first.question.as_str())
                .and_then(last_nonempty_line)
                .map(|l| truncate_one_line(l, max_chars));
            return Some(BlockedOn {
                kind: BlockedKind::Question,
                request_id: q.question_id.clone(),
                title,
            });
        }
        if let Some(a) = self.pending_plan_approval.as_ref() {
            let title = a
                .plan_markdown
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| truncate_one_line(l, max_chars));
            return Some(BlockedOn {
                kind: BlockedKind::PlanApproval,
                request_id: a.approval_id.clone(),
                title,
            });
        }
        None
    }

    /// Lazily initialize `self.live_message` and return a mutable reference
    /// to it. Centralizes the "create-if-absent" pattern shared by the
    /// text/thinking delta appenders, the tool-call ref pusher, and the
    /// plan-update applier.
    fn ensure_live_message(&mut self) -> &mut LiveMessage {
        if self.live_message.is_none() {
            self.live_message = Some(LiveMessage {
                id: format!("live-{}", uuid::Uuid::new_v4()),
                role: MessageRole::Assistant,
                content: Vec::new(),
                started_at: Utc::now(),
            });
        }
        self.live_message
            .as_mut()
            .expect("live_message just initialized")
    }

    fn append_text_delta(&mut self, text: &str, parent_tool_use_id: Option<&str>) {
        let live = self.ensure_live_message();
        // Merge only into a trailing block of the same kind AND the same
        // subagent attribution — main text → subagent text → main text must
        // produce three blocks, never one. The frontend reducer applies the
        // identical predicate over the same seq-ordered stream, so a client
        // hydrated from a snapshot converges on the same block boundaries as
        // one that streamed live.
        match live.content.last_mut() {
            Some(LiveContentBlock::Text {
                text: existing,
                parent_tool_use_id: p,
            }) if p.as_deref() == parent_tool_use_id => existing.push_str(text),
            _ => live.content.push(LiveContentBlock::Text {
                text: text.to_string(),
                parent_tool_use_id: parent_tool_use_id.map(str::to_owned),
            }),
        }
    }

    fn append_thinking_delta(&mut self, text: &str, parent_tool_use_id: Option<&str>) {
        let live = self.ensure_live_message();
        match live.content.last_mut() {
            Some(LiveContentBlock::Thinking {
                text: existing,
                parent_tool_use_id: p,
            }) if p.as_deref() == parent_tool_use_id => existing.push_str(text),
            _ => live.content.push(LiveContentBlock::Thinking {
                text: text.to_string(),
                parent_tool_use_id: parent_tool_use_id.map(str::to_owned),
            }),
        }
    }

    /// Push a `ToolCallRef` block onto `live_message.content` for the given
    /// tool-call id, but only if no existing block in `content` already
    /// references that id. Called by both `ToolCall` and `ToolCallUpdate`
    /// arms so a tool's position survives any event-ordering edge case
    /// without ever duplicating.
    fn push_tool_call_ref_if_absent(&mut self, tool_call_id: &str) {
        let live = self.ensure_live_message();
        let already_present = live.content.iter().any(|b| {
            matches!(
                b,
                LiveContentBlock::ToolCallRef { tool_call_id: id } if id == tool_call_id
            )
        });
        if !already_present {
            live.content.push(LiveContentBlock::ToolCallRef {
                tool_call_id: tool_call_id.to_string(),
            });
        }
    }

    /// Insert-or-update a tool call entry. Used by both `ToolCall` (initial) and
    /// `ToolCallUpdate` events. `kind` is `Some` only on the initial event;
    /// title/status/content/raw_input/raw_output/locations/meta are merged
    /// when present. Partial-update preservation: a `None` value passed in
    /// from a `ToolCallUpdate` (which typically carries only the fields that
    /// changed) must NOT clobber a previously-set value on the entry.
    ///
    /// When `raw_output_append` is true and both the prior and incoming
    /// outputs are text, the strings are concatenated (mirrors the frontend
    /// reducer and agent delta emission with `raw_output_append=true`).
    #[allow(clippy::too_many_arguments)]
    fn upsert_tool_call(
        &mut self,
        id: &str,
        kind: Option<&str>,
        title: Option<&str>,
        status: Option<&str>,
        content: Option<&str>,
        raw_input: Option<&str>,
        raw_output: Option<&str>,
        raw_output_append: bool,
        locations: Option<&serde_json::Value>,
        meta: Option<&serde_json::Value>,
        images: Option<&[ToolCallImageInfo]>,
    ) {
        let entry = self
            .active_tool_calls
            .entry(id.to_string())
            .or_insert_with(|| ToolCallState {
                id: id.to_string(),
                kind: ToolKind::Other,
                label: String::new(),
                status: ToolCallStatus::Pending,
                input: None,
                output: None,
                content: None,
                locations: None,
                meta: None,
                images: Vec::new(),
                raw_input_chunks: Vec::new(),
            });
        if let Some(k) = kind {
            entry.kind = parse_tool_kind(k);
        }
        if let Some(t) = title {
            entry.label = t.to_string();
        }
        if let Some(s) = status {
            entry.status = parse_tool_call_status(s);
        }
        if let Some(c) = content {
            entry.content = Some(c.to_string());
        }
        if let Some(chunk) = raw_input {
            entry.raw_input_chunks.push(chunk.to_string());
            // 后端目前发送的是已序列化的 JSON 文本（完整或正在累积）。
            // 对最新片段做尽力解析；解析失败则尝试拼接历史片段。
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(chunk) {
                entry.input = Some(value);
            } else if let Ok(value) =
                serde_json::from_str::<serde_json::Value>(&entry.raw_input_chunks.join(""))
            {
                entry.input = Some(value);
            }
        }
        if let Some(text) = raw_output {
            let next = parse_tool_call_output_text(text);
            if raw_output_append {
                match (&mut entry.output, next) {
                    (
                        Some(ToolCallOutput::Text { content }),
                        ToolCallOutput::Text { content: add },
                    ) => {
                        content.push_str(&add);
                    }
                    (None, next) => {
                        entry.output = Some(next);
                    }
                    (_, next) => {
                        // Type change under append: fall back to replace so
                        // structured/error outputs still land.
                        entry.output = Some(next);
                    }
                }
            } else {
                entry.output = Some(next);
            }
        }
        if let Some(loc) = locations {
            entry.locations = Some(loc.clone());
        }
        if let Some(m) = meta {
            entry.meta = Some(m.clone());
        }
        if let Some(imgs) = images {
            // Replace-on-update: the agent re-sends the full image list on
            // every ToolCallUpdate that carries content (see
            // extract_tool_call_images in connection.rs). Absent images
            // (None at the AcpEvent layer) preserve the prior vec.
            entry.images = imgs.to_vec();
        }
    }

    /// Keep the newest secret-safe transition for session-details (by
    /// `transition_at`, not per-lease version).
    fn remember_watchdog_diagnostic(
        &mut self,
        projection: &crate::acp::tool_watchdog::ToolWatchdogProjection,
    ) {
        use crate::acp::tool_watchdog::is_newer_diagnostic;
        let replace = match &self.last_tool_watchdog_diagnostic {
            None => true,
            Some(current) => is_newer_diagnostic(projection, current),
        };
        if replace {
            self.last_tool_watchdog_diagnostic = Some(projection.clone());
        }
    }

    /// 拷贝出对外可见的 wire-friendly snapshot。Phase 2 snapshot 端点直接调用此方法。
    pub fn to_snapshot(&self) -> LiveSessionSnapshot {
        LiveSessionSnapshot {
            connection_id: self.connection_id.clone(),
            conversation_id: self.conversation_id,
            folder_id: self.folder_id,
            shared_session: self.shared_session.clone(),
            status: self.status.clone(),
            external_id: self.external_id.clone(),
            live_message: self.live_message.clone(),
            active_tool_calls: self.active_tool_calls.values().cloned().collect(),
            pending_permission: self.pending_permission.clone(),
            pending_question: self.pending_question.clone(),
            waiting_for_subagents: self.waiting_for_subagents.clone(),
            pending_plan_approval: self.pending_plan_approval.clone(),
            pending_user_message: self.pending_user_message.clone(),
            active_delegations: self.active_delegations.values().cloned().collect(),
            tool_watchdog_projections: self.tool_watchdog_projections.clone(),
            tool_watchdog_max_versions: self.tool_watchdog_max_versions.clone(),
            last_tool_watchdog_diagnostic: self.last_tool_watchdog_diagnostic.clone(),
            feedback: self.feedback.clone(),
            background_outstanding: self.background_outstanding,
            feedback_tool_available: self.feedback_tool_available,
            modes: self.modes.clone(),
            current_mode: self.current_mode.clone(),
            config_options: self.config_options.clone(),
            prompt_capabilities: self.prompt_capabilities.clone(),
            usage: self.usage.clone(),
            fork_supported: self.fork_supported,
            available_commands: self.available_commands.clone(),
            selectors_ready: self.selectors_ready,
            config_stale: self.config_stale,
            config_stale_kind: self.config_stale_kind,
            delegation_route: self.delegation_route.clone(),
            last_error: self.last_error.clone(),
            event_seq: self.event_seq,
        }
    }
}

/// Max age of the background keep-alive: how long a connection with
/// launched-but-unresolved background work stays exempt from the idle sweeps
/// after the LAST `BackgroundActivity` event. Configurable via
/// `CODEG_ACP_BACKGROUND_KEEPALIVE_MAX_SECS` (seconds; invalid → default 3600;
/// `0` disables the exemption entirely). Read once per process.
pub(crate) fn background_keepalive_max_age() -> chrono::Duration {
    static SECS: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let secs = *SECS.get_or_init(|| {
        std::env::var("CODEG_ACP_BACKGROUND_KEEPALIVE_MAX_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|v| *v >= 0)
            .unwrap_or(3600)
    });
    chrono::Duration::seconds(secs)
}

/// `to_snapshot()` 的输出——前端可消费的 wire shape。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSessionSnapshot {
    pub connection_id: String,
    pub conversation_id: Option<i32>,
    pub folder_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_session: Option<crate::acp::shared_session::SharedSessionProjection>,
    pub status: ConnectionStatus,
    pub external_id: Option<String>,
    pub live_message: Option<LiveMessage>,
    pub active_tool_calls: Vec<ToolCallState>,
    pub pending_permission: Option<PendingPermissionState>,
    /// The agent's in-flight `ask_user_question` (see
    /// `SessionState.pending_question`). `#[serde(default)]` so older payloads
    /// deserialize; `skip_serializing_if` keeps the common no-question case off
    /// the wire so every snapshot stays byte-identical with the pre-feature shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_question: Option<PendingQuestionState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_for_subagents: Option<ContinuationWaitingProjection>,

    /// The agent's in-flight Grok `exit_plan_mode` approval (see
    /// `SessionState.pending_plan_approval`). `#[serde(default)]` so older
    /// payloads deserialize; `skip_serializing_if` keeps the common no-approval
    /// case off the wire so every snapshot stays byte-identical with the
    /// pre-feature shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_plan_approval: Option<PendingPlanApprovalState>,
    /// The in-flight user prompt for the current turn (see
    /// `SessionState.pending_user_message`). `#[serde(default)]` so older
    /// payloads still deserialize; `skip_serializing_if` so the no-pending case
    /// keeps the wire shape byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_user_message: Option<PendingUserMessage>,
    /// Running sub-agent delegations recoverable from the snapshot (see
    /// `SessionState.active_delegations`). `#[serde(default)]` so older server
    /// payloads without this field still deserialize; `skip_serializing_if` so
    /// the common no-delegation case keeps the wire shape byte-identical and
    /// doesn't bloat every snapshot with an empty array.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_delegations: Vec<ActiveDelegationState>,
    /// Currently actionable tool-watchdog projections keyed by `lease_id`
    /// (see `SessionState.tool_watchdog_projections`). Concurrent Grace leases
    /// are all present so attach/replay never loses Stop/Extend controls.
    /// `#[serde(default)]` for older payloads; omitted when empty so the common
    /// no-warning case stays byte-identical with the pre-feature wire shape.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_watchdog_projections:
        BTreeMap<String, crate::acp::tool_watchdog::ToolWatchdogProjection>,
    /// Per-lease max projection version floor (including terminal tombstones).
    /// Cold clients seed FE reduce floors from this so a late lower-version
    /// Cancelling for lease A cannot resurrect after A timed out and B became
    /// the sole retained diagnostic. Omitted when empty; `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_watchdog_max_versions: BTreeMap<String, u64>,
    /// Latest secret-safe diagnostic transition (including post-timeout).
    /// Omitted when none; `#[serde(default)]` for older payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_tool_watchdog_diagnostic: Option<crate::acp::tool_watchdog::ToolWatchdogProjection>,
    /// Live user-feedback notes for the current turn (see `SessionState.feedback`).
    /// `#[serde(default)]` so older server payloads without this field still
    /// deserialize; `skip_serializing_if` keeps the common empty case off the
    /// wire so every snapshot stays byte-identical with the pre-feature shape.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub feedback: Vec<FeedbackItem>,
    /// Launched-but-unresolved background tasks (see
    /// `SessionState.background_outstanding`) — lets a client attaching
    /// mid-episode (web reconnect, new window) recover the pending count the
    /// one-shot `BackgroundActivity` events won't replay for it. `#[serde(
    /// default)]` so older payloads deserialize to `0`; skipped when `0` so the
    /// common no-background case keeps the wire shape byte-identical.
    #[serde(default, skip_serializing_if = "u32_is_zero")]
    pub background_outstanding: u32,
    /// Whether this agent has the `check_user_feedback` tool (see
    /// `SessionState.feedback_tool_available`). `#[serde(default)]` so older
    /// payloads deserialize to `false`; the frontend gates the feedback bar on
    /// it. Always serialized (a plain bool) so the frontend can rely on it.
    #[serde(default)]
    pub feedback_tool_available: bool,
    pub modes: Option<SessionModeStateInfo>,
    pub current_mode: Option<String>,
    pub config_options: Option<Vec<SessionConfigOptionInfo>>,
    pub prompt_capabilities: Option<PromptCapabilitiesInfo>,
    pub usage: Option<UsageInfo>,
    pub fork_supported: bool,
    pub available_commands: Vec<AvailableCommandInfo>,
    pub selectors_ready: bool,
    /// Whether the running session is on stale (launch-time) config after a
    /// later settings save (see `SessionState.config_stale`). `#[serde(default)]`
    /// so older server payloads without the field deserialize to `false`; always
    /// serialized so the frontend can rely on it from the snapshot path.
    #[serde(default)]
    pub config_stale: bool,
    /// Which settings surface drifted (see `SessionState.config_stale_kind`).
    /// `#[serde(default)]` + `skip_serializing_if` keep the common not-stale case
    /// byte-identical with the pre-feature wire shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_stale_kind: Option<ConfigStaleKind>,
    /// Managed route + availability (see [`DelegationRouteSnapshot`]).
    /// Default preserves mixed-version clients and Rust deserialization tests.
    #[serde(default = "legacy_unmanaged_route_snapshot")]
    pub delegation_route: DelegationRouteSnapshot,
    /// Most recent agent/runtime error for this live connection. Omitted when
    /// no error has occurred so older clients and common snapshots stay small.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<SessionLastError>,
    pub event_seq: u64,
}

/// `skip_serializing_if` helper for `LiveSessionSnapshot.background_outstanding`.
fn u32_is_zero(v: &u32) -> bool {
    *v == 0
}

/// Last non-empty line of `s`, trimmed. `None` if every line is blank.
fn last_nonempty_line(s: &str) -> Option<&str> {
    s.lines().map(str::trim).rev().find(|l| !l.is_empty())
}

/// Cap `line` at `max_chars` characters, appending `…` when truncated. Operates
/// on `char`s so multi-byte text never splits mid-codepoint. Expects an
/// already single, trimmed line (see [`last_nonempty_line`]). Single-pass: takes
/// at most `max_chars + 1` chars total, so a huge (e.g. MB) input line never
/// triggers a second full scan to decide whether to mark truncation.
fn truncate_one_line(line: &str, max_chars: usize) -> String {
    let mut chars = line.chars();
    let mut out: String = (&mut chars).take(max_chars).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn parse_tool_kind(s: &str) -> ToolKind {
    match s {
        "read" => ToolKind::Read,
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Execute,
        "think" => ToolKind::Think,
        "fetch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

fn parse_tool_call_status(s: &str) -> ToolCallStatus {
    match s {
        "in_progress" => ToolCallStatus::InProgress,
        "completed" => ToolCallStatus::Completed,
        "failed" => ToolCallStatus::Failed,
        _ => ToolCallStatus::Pending,
    }
}

/// `raw_output` 是已序列化的 JSON 文本。尽力解析为结构化 JSON；解析失败时回退为
/// 文本。如果解析后的 JSON 顶层有 `"error"` 字段，提升为 `Error` 变体。
fn parse_tool_call_output_text(text: &str) -> ToolCallOutput {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => {
            if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
                ToolCallOutput::Error {
                    message: err.to_string(),
                }
            } else if let Some(s) = value.as_str() {
                ToolCallOutput::Text {
                    content: s.to_string(),
                }
            } else {
                ToolCallOutput::Json { value }
            }
        }
        Err(_) => ToolCallOutput::Text {
            content: text.to_string(),
        },
    }
}

/// Permission 事件的 `tool_call` 字段是 ACP 的 ToolCall JSON。提取 id 用作
/// `PendingPermissionState.tool_call_id`——快查路径（match by id 时不必每次重
/// 解析整个 tool_call value）。完整 tool_call value 由调用方另行保留，前端
/// 依赖它做 diff / 命令 / plan 渲染。同时兼容 camelCase / snake_case。
fn extract_tool_call_id(tool_call: &serde_json::Value) -> String {
    tool_call
        .as_object()
        .and_then(|o| {
            o.get("toolCallId")
                .or_else(|| o.get("tool_call_id"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::types::{
        AcpEvent, ConnectionStatus, DelegationResultSummary, EventEnvelope, PromptCapabilitiesInfo,
        SessionConfigKindInfo, SessionConfigOptionInfo, SessionConfigSelectInfo, SessionModeInfo,
        SessionModeStateInfo, UserMessageBlock,
    };

    fn fresh_state() -> SessionState {
        SessionState::new(
            "conn-test".to_string(),
            AgentType::ClaudeCode,
            None,
            "win-test".to_string(),
            None,
        )
    }

    #[test]
    fn shared_session_projection_reconstructs_queue_and_active_turn() {
        use crate::acp::shared_session::{
            SharedActiveTurnProjection, SharedQueuedPromptState, SharedQueuedPromptSummary,
            SharedSessionPhase, SharedSessionProjection,
        };

        let mut state = fresh_state();
        assert!(serde_json::to_value(state.to_snapshot())
            .unwrap()
            .get("shared_session")
            .is_none());
        state.shared_session = Some(SharedSessionProjection {
            generation: 3,
            phase: SharedSessionPhase::Bootstrapping,
            queue: Vec::new(),
            active_turn: None,
            lease_expires_at: None,
        });
        let submitted_at = chrono::DateTime::parse_from_rfc3339("2026-08-16T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        state.apply_event(&AcpEvent::SharedSessionPhaseChanged {
            generation: 3,
            phase: SharedSessionPhase::Ready,
        });
        state.apply_event(&AcpEvent::PromptQueued {
            generation: 3,
            item: SharedQueuedPromptSummary {
                queue_item_id: "q2".into(),
                enqueue_seq: 2,
                client_message_id: "m2".into(),
                visible_text: Some("later".into()),
                visible_text_truncated: false,
                attachment_count: 0,
                submitted_at,
                state: SharedQueuedPromptState::Queued,
            },
        });
        state.apply_event(&AcpEvent::PromptQueued {
            generation: 3,
            item: SharedQueuedPromptSummary {
                queue_item_id: "q1".into(),
                enqueue_seq: 1,
                client_message_id: "m1".into(),
                visible_text: Some("first".into()),
                visible_text_truncated: false,
                attachment_count: 0,
                submitted_at,
                state: SharedQueuedPromptState::Queued,
            },
        });
        state.apply_event(&AcpEvent::PromptQueueItemCancelled {
            generation: 3,
            queue_item_id: "q1".into(),
        });
        state.apply_event(&AcpEvent::PromptDispatchStarted {
            generation: 3,
            turn: SharedActiveTurnProjection {
                turn_id: "turn-1".into(),
                queue_item_id: "q1".into(),
                enqueue_seq: 1,
                client_message_id: "m1".into(),
                stop_requested: false,
            },
        });

        let snapshot = state.to_snapshot();
        let shared = snapshot.shared_session.expect("shared projection");
        assert_eq!(
            shared
                .queue
                .iter()
                .map(|item| item.enqueue_seq)
                .collect::<Vec<_>>(),
            [2]
        );
        assert_eq!(
            shared
                .active_turn
                .as_ref()
                .map(|turn| turn.turn_id.as_str()),
            Some("turn-1")
        );
    }

    #[test]
    fn shared_session_projection_ignores_stale_events_and_settles_matching_turn() {
        use crate::acp::shared_session::{
            SharedActiveTurnProjection, SharedSessionPhase, SharedSessionProjection,
            SharedTurnOutcome,
        };

        let mut state = fresh_state();
        state.shared_session = Some(SharedSessionProjection {
            generation: 3,
            phase: SharedSessionPhase::Ready,
            queue: Vec::new(),
            active_turn: Some(SharedActiveTurnProjection {
                turn_id: "turn-1".into(),
                queue_item_id: "q1".into(),
                enqueue_seq: 1,
                client_message_id: "m1".into(),
                stop_requested: false,
            }),
            lease_expires_at: None,
        });

        state.apply_event(&AcpEvent::SharedSessionPhaseChanged {
            generation: 2,
            phase: SharedSessionPhase::Failed {
                error_code: "stale".into(),
                cleanup_complete: false,
            },
        });
        state.apply_event(&AcpEvent::SharedTurnSettled {
            generation: 3,
            turn_id: "other-turn".into(),
            outcome: SharedTurnOutcome::Completed,
        });
        assert_eq!(
            state.shared_session.as_ref().map(|shared| &shared.phase),
            Some(&SharedSessionPhase::Ready)
        );
        assert_eq!(
            state
                .shared_session
                .as_ref()
                .and_then(|shared| shared.active_turn.as_ref())
                .map(|turn| turn.turn_id.as_str()),
            Some("turn-1")
        );

        state.apply_event(&AcpEvent::SharedTurnSettled {
            generation: 3,
            turn_id: "turn-1".into(),
            outcome: SharedTurnOutcome::Completed,
        });
        assert!(state
            .shared_session
            .as_ref()
            .is_some_and(|shared| shared.active_turn.is_none()));
    }

    fn codeg_plan(agent_type: AgentType) -> crate::acp::delegation::route::DelegationRoutePlan {
        use crate::acp::delegation::route::{
            DelegationRoutePlan, DelegationRoutePolicy, DelegationRouteSource,
            NativeSuppressionPlan, ROUTE_ADAPTER_CONTRACT_VERSION,
        };
        let _ = agent_type;
        DelegationRoutePlan {
            managed: true,
            requested: DelegationRoutePolicy::Codeg,
            effective: DelegationRoutePolicy::Codeg,
            source: DelegationRouteSource::GlobalDefault,
            native_suppression: NativeSuppressionPlan::CodexMultiAgentFalse,
            expose_codeg_delegation: true,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: "task10-route".into(),
        }
    }

    fn state_with_route(plan: crate::acp::delegation::route::DelegationRoutePlan) -> SessionState {
        let mut s = fresh_state();
        s.agent_type = AgentType::Codex;
        s.set_route_plan_snapshot(&plan);
        s.set_delegation_available(true);
        s
    }

    #[test]
    fn snapshot_keeps_route_immutable_while_availability_changes() {
        let mut state = state_with_route(codeg_plan(AgentType::Codex));
        let original = state.to_snapshot().delegation_route;
        state.apply_event(&AcpEvent::DelegationAvailabilityChanged { available: false });
        let changed = state.to_snapshot().delegation_route;
        assert_eq!(original.requested, changed.requested);
        assert_eq!(original.effective, changed.effective);
        assert_eq!(original.source, changed.source);
        assert!(!changed.delegation_available);
    }

    #[tokio::test]
    async fn prepare_registered_replacement_preserves_public_state_and_event_sequence() {
        let state = Arc::new(tokio::sync::RwLock::new(fresh_state()));
        let original_arc = Arc::clone(&state);
        let original_stream = state.read().await.event_stream();
        {
            let mut current = state.write().await;
            current.connection_incarnation = "incarnation-old".into();
            current.conversation_id = Some(57);
            current.folder_id = Some(9);
            current.event_seq = 41;
            current.status = ConnectionStatus::Error;
        }

        let mut replacement = SessionState::new(
            "conn-test".into(),
            AgentType::Codex,
            Some(PathBuf::from("/tmp/shared-replacement")),
            "shared-server".into(),
            None,
        );
        replacement.connection_incarnation = "incarnation-new".into();
        replacement.set_route_plan_snapshot(&codeg_plan(AgentType::Codex));

        state
            .write()
            .await
            .prepare_registered_replacement(replacement);

        assert!(Arc::ptr_eq(&state, &original_arc));
        let current = state.read().await;
        assert_eq!(current.connection_incarnation, "incarnation-new");
        assert_eq!(current.conversation_id, Some(57));
        assert_eq!(current.folder_id, Some(9));
        assert_eq!(current.event_seq, 41);
        assert!(Arc::ptr_eq(&current.event_stream(), &original_stream));
        assert_eq!(current.status, ConnectionStatus::Connecting);
        assert_eq!(current.agent_type, AgentType::Codex);
        assert_eq!(
            current.working_dir.as_deref(),
            Some(std::path::Path::new("/tmp/shared-replacement"))
        );
    }

    #[test]
    fn observation_changed_updates_active_card_without_completing() {
        let mut s = fresh_state();
        s.apply_event(&delegation_started("pt-1", 99));
        assert!(s.active_delegations.contains_key("pt-1"));
        let at = Utc::now();
        s.apply_event(&AcpEvent::DelegationObservationChanged {
            parent_tool_use_id: "pt-1".into(),
            task_id: "task-1".into(),
            observation: crate::acp::delegation::types::TaskObservation::Stalled,
            last_agent_activity_at: at,
            stalled_since: Some(at + chrono::Duration::seconds(300)),
        });
        let card = s.active_delegations.get("pt-1").expect("card remains");
        assert_eq!(
            card.observation,
            Some(crate::acp::delegation::types::TaskObservation::Stalled)
        );
        assert_eq!(card.last_agent_activity_at, Some(at));
        assert!(card.stalled_since.is_some());
        // Snapshot recovers observation + route availability bit together.
        let snap = s.to_snapshot();
        assert_eq!(snap.active_delegations.len(), 1);
        assert_eq!(
            snap.active_delegations[0].observation,
            Some(crate::acp::delegation::types::TaskObservation::Stalled)
        );
        // Never synthesizes completion.
        assert!(s.active_delegations.contains_key("pt-1"));
    }

    #[test]
    fn observation_changed_unknown_tool_use_does_not_create_card() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::DelegationObservationChanged {
            parent_tool_use_id: "ghost".into(),
            task_id: "t".into(),
            observation: crate::acp::delegation::types::TaskObservation::Active,
            last_agent_activity_at: Utc::now(),
            stalled_since: None,
        });
        assert!(s.active_delegations.is_empty());
    }

    #[test]
    fn delegation_route_snapshot_round_trips_on_live_snapshot() {
        let mut s = state_with_route(codeg_plan(AgentType::Codex));
        s.set_delegation_available(true);
        let snap = s.to_snapshot();
        assert!(snap.delegation_route.managed);
        assert!(snap.delegation_route.delegation_available);
        let json = serde_json::to_value(&snap).unwrap();
        let back: LiveSessionSnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(back.delegation_route, snap.delegation_route);
    }

    #[test]
    fn mark_agent_activity_does_not_advance_idle_sweep_timestamp() {
        let mut s = fresh_state();
        let idle_before = s.last_activity_at;
        let agent_before = s.last_agent_activity_at;
        // Ensure a distinct later instant for agent activity.
        let later = agent_before + chrono::Duration::seconds(42);
        s.mark_agent_activity(later);
        assert_eq!(
            s.last_agent_activity_at, later,
            "agent activity clock advances"
        );
        assert_eq!(
            s.last_activity_at, idle_before,
            "idle-sweep last_activity_at must stay unchanged"
        );
    }

    #[test]
    fn plan_approval_applies_clears_by_id_and_survives_snapshot() {
        let mut s = fresh_state();
        // Request → pending set + carried on the snapshot for mid-turn attach.
        s.apply_event(&AcpEvent::PlanApprovalRequest {
            approval_id: "ap-1".into(),
            tool_call_id: "call-1".into(),
            plan_markdown: "# Plan".into(),
        });
        let pending = s.pending_plan_approval.clone().expect("pending set");
        assert_eq!(pending.approval_id, "ap-1");
        assert_eq!(pending.tool_call_id, "call-1");
        assert_eq!(pending.plan_markdown, "# Plan");
        assert!(s.to_snapshot().pending_plan_approval.is_some());

        // A resolve for a DIFFERENT id must not wipe the live approval.
        s.apply_event(&AcpEvent::PlanApprovalResolved {
            approval_id: "other".into(),
        });
        assert!(s.pending_plan_approval.is_some());

        // Matching resolve clears it (and the snapshot).
        s.apply_event(&AcpEvent::PlanApprovalResolved {
            approval_id: "ap-1".into(),
        });
        assert!(s.pending_plan_approval.is_none());
        assert!(s.to_snapshot().pending_plan_approval.is_none());
    }

    #[test]
    fn turn_complete_clears_pending_plan_approval() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PlanApprovalRequest {
            approval_id: "ap-1".into(),
            tool_call_id: "c".into(),
            plan_markdown: String::new(),
        });
        assert!(s.pending_plan_approval.is_some());
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "sid".into(),
            stop_reason: "end_turn".into(),
            agent_type: "grok".into(),
            mark_awaiting_reply: false,
            termination_source: None,
            provider_turn_id: None,
        });
        assert!(s.pending_plan_approval.is_none());
    }

    #[test]
    fn user_message_supersedes_stale_pending_plan_approval() {
        // A new turn starting without a clean TurnComplete (fork/resume re-prompt,
        // queued prompt sent instead of answering) must not leave a dead approval
        // in the snapshot for a mid-turn attach to render.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PlanApprovalRequest {
            approval_id: "ap-1".into(),
            tool_call_id: "c".into(),
            plan_markdown: "# plan".into(),
        });
        assert!(s.pending_plan_approval.is_some());
        s.apply_event(&AcpEvent::UserMessage {
            message_id: "m1".into(),
            blocks: vec![],
        });
        assert!(s.pending_plan_approval.is_none());
    }

    #[test]
    fn background_activity_mirrors_outstanding_and_gates_keepalive() {
        let mut s = fresh_state();
        assert!(!s.has_active_background_work(Utc::now()));

        s.apply_event(&AcpEvent::BackgroundActivity {
            session_id: "sid".into(),
            turns: vec![],
            outstanding: 2,
            settled: vec![],
            watermark: 42,
        });
        assert_eq!(s.background_outstanding, 2);
        let now = Utc::now();
        assert!(s.has_active_background_work(now));

        // The exemption lapses once the last watcher heartbeat is older than
        // the max age — a dead/wedged watcher can't pin a connection forever.
        let long_after = now + background_keepalive_max_age() + chrono::Duration::seconds(1);
        assert!(!s.has_active_background_work(long_after));

        // Settled back to zero: no exemption regardless of recency.
        s.apply_event(&AcpEvent::BackgroundActivity {
            session_id: "sid".into(),
            turns: vec![],
            outstanding: 0,
            settled: vec![],
            watermark: 43,
        });
        assert!(!s.has_active_background_work(Utc::now()));
    }

    #[test]
    fn snapshot_carries_background_outstanding_and_skips_zero_on_wire() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::BackgroundActivity {
            session_id: "sid".into(),
            turns: vec![],
            outstanding: 3,
            settled: vec![],
            watermark: 0,
        });
        assert_eq!(s.to_snapshot().background_outstanding, 3);
        let json = serde_json::to_value(s.to_snapshot()).unwrap();
        assert_eq!(
            json.get("background_outstanding").and_then(|v| v.as_u64()),
            Some(3)
        );

        // Zero is skipped so the common no-background snapshot stays
        // byte-identical with the pre-feature wire shape.
        let zero = fresh_state();
        let json = serde_json::to_value(zero.to_snapshot()).unwrap();
        assert!(json.get("background_outstanding").is_none());
    }

    #[test]
    fn new_session_starts_with_seq_zero_and_connecting_status() {
        let s = fresh_state();
        assert_eq!(s.event_seq, 0);
        assert_eq!(s.status, ConnectionStatus::Connecting);
        assert!(s.external_id.is_none());
        assert!(s.live_message.is_none());
        assert!(s.active_tool_calls.is_empty());
        assert!(s.pending_permission.is_none());
        assert!(!s.fork_supported);
        assert!(s.available_commands.is_empty());
        assert!(!s.selectors_ready);
        assert!(s.pending_user_message.is_none());
    }

    fn text_user_message(id: &str, text: &str) -> AcpEvent {
        AcpEvent::UserMessage {
            message_id: id.to_string(),
            blocks: vec![UserMessageBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn user_message_event_captures_pending_user_message() {
        // The in-flight user prompt is captured so a mid-turn attacher renders
        // the user turn from the snapshot (the one-shot event won't replay).
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-1", "hello agent"));
        let pending = s.pending_user_message.as_ref().expect("pending set");
        assert_eq!(pending.message_id, "user-1");
        assert_eq!(
            pending.blocks,
            vec![UserMessageBlock::Text {
                text: "hello agent".into()
            }]
        );
        assert!(
            s.pending_user_message_started_at.is_some(),
            "the turn-start instant is captured alongside the pending prompt"
        );
    }

    #[test]
    fn pending_user_message_started_at_has_no_sub_ms_residue() {
        // The recency gate in `apply_in_flight_message_id` compares this
        // stamp against millisecond-precision parsed-turn timestamps
        // (Cursor's journal upgrade rewrites the in-flight user turn to a
        // ms send stamp taken right after this event applies). Sub-ms
        // residue would order the threshold AFTER a stamp taken later in
        // real time and unstamp the turn.
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-1", "hello"));
        let at = s.pending_user_message_started_at.expect("stamp set");
        assert_eq!(at.timestamp_subsec_nanos() % 1_000_000, 0);
    }

    #[test]
    fn turn_complete_clears_pending_user_message() {
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-1", "hi"));
        assert!(s.pending_user_message.is_some());
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "sess".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });
        assert!(
            s.pending_user_message.is_none(),
            "a completed turn must clear the pending user message (no stale snapshot)"
        );
        assert!(
            s.pending_user_message_started_at.is_none(),
            "the turn-start instant is cleared in lockstep with the pending prompt"
        );
    }

    #[test]
    fn to_snapshot_carries_pending_user_message() {
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-7", "snapshot me"));
        let pending = s
            .to_snapshot()
            .pending_user_message
            .expect("snapshot carries pending");
        assert_eq!(pending.message_id, "user-7");
    }

    #[test]
    fn snapshot_round_trips_pending_user_message_and_omits_when_absent() {
        let mut s = fresh_state();
        s.apply_event(&text_user_message("user-9", "round trip"));
        let snap = s.to_snapshot();
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: LiveSessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.pending_user_message, snap.pending_user_message);
        // No-pending snapshot keeps the field off the wire (byte-identical with
        // the pre-feature shape).
        let empty_json = serde_json::to_string(&fresh_state().to_snapshot()).expect("serialize");
        assert!(
            !empty_json.contains("pending_user_message"),
            "no-pending snapshot must omit the field"
        );
    }

    #[test]
    fn snapshot_carries_last_error_and_clears_on_next_prompt() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::Error {
            message: "ACP protocol error: forbidden".into(),
            agent_type: "claude_code".into(),
            code: Some("forbidden".into()),
            terminal: true,
        });

        let snap = s.to_snapshot();
        assert_eq!(
            snap.last_error,
            Some(SessionLastError {
                message: "ACP protocol error: forbidden".into(),
                code: Some("forbidden".into()),
            })
        );

        let json = serde_json::to_string(&snap).expect("serialize");
        let back: LiveSessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.last_error, snap.last_error);

        let empty_json = serde_json::to_string(&fresh_state().to_snapshot()).expect("serialize");
        assert!(
            !empty_json.contains("last_error"),
            "no-error snapshot must omit last_error"
        );

        s.apply_event(&AcpEvent::StatusChanged {
            status: ConnectionStatus::Prompting,
        });
        assert!(
            s.to_snapshot().last_error.is_none(),
            "new prompts clear stale snapshot-recoverable errors"
        );
    }

    // --- tool_watchdog_snapshot: lossless multi-lease attach/replay ---

    fn sample_watchdog_projection(
        lease_id: &str,
        version: u64,
        phase: crate::acp::tool_watchdog::ToolWatchdogPhase,
    ) -> crate::acp::tool_watchdog::ToolWatchdogProjection {
        sample_watchdog_projection_at(lease_id, version, phase, "2026-07-22T12:10:00Z")
    }

    fn sample_watchdog_projection_at(
        lease_id: &str,
        version: u64,
        phase: crate::acp::tool_watchdog::ToolWatchdogPhase,
        transition_at: &str,
    ) -> crate::acp::tool_watchdog::ToolWatchdogProjection {
        use crate::acp::tool_watchdog::{
            CancellationScope, ToolCategory, ToolWatchdogPhase, ToolWatchdogProjection,
        };
        let grace_deadline = matches!(
            phase,
            ToolWatchdogPhase::Warning | ToolWatchdogPhase::Grace | ToolWatchdogPhase::Cancelling
        )
        .then(|| "2026-07-22T12:20:00Z".to_string());
        let error_code = matches!(phase, ToolWatchdogPhase::TimedOut)
            .then(|| "tool_stalled_timeout".to_string());
        ToolWatchdogProjection {
            lease_id: lease_id.into(),
            version,
            tool_title: ToolCategory::Terminal,
            phase,
            last_progress_at: "2026-07-22T12:00:00Z".into(),
            transition_at: transition_at.into(),
            transition_seq: 0,
            grace_deadline,
            cancellation_scope: Some(CancellationScope::Terminal),
            error_code,
        }
    }

    #[test]
    fn tool_watchdog_snapshot_replays_concurrent_grace_leases() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 2, ToolWatchdogPhase::Grace),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-b", 3, ToolWatchdogPhase::Grace),
        });

        let snap = s.to_snapshot();
        assert_eq!(snap.tool_watchdog_projections.len(), 2);
        assert_eq!(snap.tool_watchdog_projections["lease-a"].version, 2);
        assert_eq!(
            snap.tool_watchdog_projections["lease-b"].phase,
            ToolWatchdogPhase::Grace
        );

        let json = serde_json::to_string(&snap).expect("serialize");
        let back: LiveSessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.tool_watchdog_projections,
            snap.tool_watchdog_projections
        );

        let empty_json = serde_json::to_string(&fresh_state().to_snapshot()).expect("serialize");
        assert!(
            !empty_json.contains("tool_watchdog_projections"),
            "empty map must stay off the wire"
        );
    }

    #[test]
    fn tool_watchdog_snapshot_stale_version_cannot_replace_newer() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 5, ToolWatchdogPhase::Grace),
        });
        // Intermediate warning with older version must not regress the map.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 4, ToolWatchdogPhase::Warning),
        });
        let snap = s.to_snapshot();
        let stored = &snap.tool_watchdog_projections["lease-1"];
        assert_eq!(stored.version, 5);
        assert_eq!(stored.phase, ToolWatchdogPhase::Grace);

        // Equal or newer version replaces (Grace → Cancelling).
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 6, ToolWatchdogPhase::Cancelling),
        });
        assert_eq!(
            s.to_snapshot().tool_watchdog_projections["lease-1"].phase,
            ToolWatchdogPhase::Cancelling
        );
    }

    #[test]
    fn tool_watchdog_snapshot_warning_then_grace_keeps_actionable_grace_version() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        // Publish transition Warning (v1) then final Grace (v2) — actionable
        // client phase is the final Grace version so first click is not stale.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 1, ToolWatchdogPhase::Warning),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 2, ToolWatchdogPhase::Grace),
        });
        let proj = &s.to_snapshot().tool_watchdog_projections["lease-1"];
        assert_eq!(proj.phase, ToolWatchdogPhase::Grace);
        assert_eq!(proj.version, 2);
    }

    #[test]
    fn tool_watchdog_snapshot_per_lease_clear_leaves_siblings_intact() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 2, ToolWatchdogPhase::Grace),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-b", 2, ToolWatchdogPhase::Grace),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 3, ToolWatchdogPhase::Cleared),
        });

        let snap = s.to_snapshot();
        assert!(!snap.tool_watchdog_projections.contains_key("lease-a"));
        assert!(snap.tool_watchdog_projections.contains_key("lease-b"));
        assert_eq!(snap.tool_watchdog_projections["lease-b"].version, 2);

        // Stale clear must not remove a newer re-opened warning on the same id.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-b", 4, ToolWatchdogPhase::Grace),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-b", 3, ToolWatchdogPhase::Cleared),
        });
        assert_eq!(
            s.to_snapshot().tool_watchdog_projections["lease-b"].version,
            4
        );
    }

    #[test]
    fn tool_watchdog_snapshot_lossless_over_32_concurrent_grace_leases() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        const N: usize = 40;
        for i in 0..N {
            let id = format!("lease-{i:03}");
            s.apply_event(&AcpEvent::ToolWatchdogChanged {
                projection: sample_watchdog_projection(
                    &id,
                    (i as u64) + 1,
                    ToolWatchdogPhase::Grace,
                ),
            });
        }

        let snap = s.to_snapshot();
        assert_eq!(
            snap.tool_watchdog_projections.len(),
            N,
            "map capacity tracks live leases; never evict Grace for UI soft guidance"
        );
        for i in 0..N {
            let id = format!("lease-{i:03}");
            assert_eq!(snap.tool_watchdog_projections[&id].version, (i as u64) + 1);
        }

        // Fresh-attach replay path: round-trip JSON preserves every lease.
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: LiveSessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.tool_watchdog_projections.len(), N);
    }

    /// Grace → progress (Cleared) must leave attach snapshot without the lease.
    #[test]
    fn tool_watchdog_snapshot_grace_then_progress_clear_is_empty() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 2, ToolWatchdogPhase::Grace),
        });
        assert_eq!(s.to_snapshot().tool_watchdog_projections.len(), 1);

        // Host emits Cleared when progress demotes Grace → Running.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 3, ToolWatchdogPhase::Cleared),
        });
        assert!(
            s.to_snapshot().tool_watchdog_projections.is_empty(),
            "progress clear must drop Grace so attach cannot replay stale Stop/Extend"
        );
    }

    /// Grace → normal complete (Cleared, no error_code) must leave snapshot empty.
    #[test]
    fn tool_watchdog_snapshot_grace_then_complete_clear_is_empty() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 2, ToolWatchdogPhase::Grace),
        });
        // Normal complete_tool produces Cleared with error_code=None.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-1", 3, ToolWatchdogPhase::Cleared),
        });
        assert!(s.to_snapshot().tool_watchdog_projections.is_empty());
    }

    /// TimedOut is a terminal event: publish then remove — never a durable ledger.
    #[test]
    fn tool_watchdog_snapshot_timed_out_does_not_accumulate() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        // Cancelling → TimedOut settle for lease-a.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 2, ToolWatchdogPhase::Cancelling),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 3, ToolWatchdogPhase::TimedOut),
        });
        assert!(
            !s.to_snapshot()
                .tool_watchdog_projections
                .contains_key("lease-a"),
            "TimedOut must not remain in actionable attach map"
        );

        // A later timeout on a different lease also leaves no residue.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-b", 1, ToolWatchdogPhase::Cancelling),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-b", 2, ToolWatchdogPhase::TimedOut),
        });
        assert!(
            s.to_snapshot().tool_watchdog_projections.is_empty(),
            "actionable map must not grow as a terminal timeout ledger"
        );

        // Live Grace sibling remains while an unrelated timeout settles.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-live", 4, ToolWatchdogPhase::Grace),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-dead", 1, ToolWatchdogPhase::Cancelling),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-dead", 2, ToolWatchdogPhase::TimedOut),
        });
        let snap = s.to_snapshot();
        assert_eq!(snap.tool_watchdog_projections.len(), 1);
        assert!(snap.tool_watchdog_projections.contains_key("lease-live"));
        assert!(!snap.tool_watchdog_projections.contains_key("lease-dead"));
    }

    /// I1: TimedOut first (emit race), then stale lower-version Cancelling must
    /// not resurrect an actionable banner after the terminal tombstone.
    #[test]
    fn tool_watchdog_rejects_stale_cancelling_after_timed_out() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        // Claim commits Cancelling at v2 in the registry; concurrent final
        // settles TimedOut at v3 and that emission wins the SessionState lock.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 1, ToolWatchdogPhase::Grace),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 3, ToolWatchdogPhase::TimedOut),
        });
        assert!(
            s.to_snapshot().tool_watchdog_projections.is_empty(),
            "TimedOut must clear actionable map"
        );

        // Late Cancelling projection from the claim emission (version 2).
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 2, ToolWatchdogPhase::Cancelling),
        });
        assert!(
            s.to_snapshot().tool_watchdog_projections.is_empty(),
            "stale Cancelling must not resurrect after TimedOut tombstone"
        );

        // Equal-version actionable after terminal also rejected.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 3, ToolWatchdogPhase::Cancelling),
        });
        assert!(
            s.to_snapshot().tool_watchdog_projections.is_empty(),
            "equal-version Cancelling after TimedOut must not resurrect"
        );
    }

    /// I1 R3: multi-lease cold attach must expose complete terminal floors so
    /// a late lower-version Cancelling for A is rejected after B replaced
    /// last_tool_watchdog_diagnostic.
    #[test]
    fn tool_watchdog_snapshot_carries_multi_lease_max_version_floors() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        // A: Grace → TimedOut(v3). Tombstone floor for A is 3.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 1, ToolWatchdogPhase::Grace),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 3, ToolWatchdogPhase::TimedOut),
        });
        // B: newer diagnostic replaces last_* — sole retained diagnostic is B.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-b", 1, ToolWatchdogPhase::Grace),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-b", 2, ToolWatchdogPhase::Warning),
        });

        let snap = s.to_snapshot();
        assert_eq!(
            snap.last_tool_watchdog_diagnostic
                .as_ref()
                .map(|d| d.lease_id.as_str()),
            Some("lease-b"),
            "last diagnostic is B only"
        );
        assert!(
            !snap.tool_watchdog_projections.contains_key("lease-a"),
            "A must not be in live map after TimedOut"
        );
        assert_eq!(
            snap.tool_watchdog_max_versions.get("lease-a").copied(),
            Some(3),
            "snapshot must carry A's terminal floor across cold attach"
        );
        assert_eq!(
            snap.tool_watchdog_max_versions.get("lease-b").copied(),
            Some(2)
        );

        // Wire round-trip preserves floors for cold hydrate.
        let json = serde_json::to_string(&snap).expect("serialize");
        let back: LiveSessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.tool_watchdog_max_versions.get("lease-a").copied(),
            Some(3)
        );

        // Simulate cold client SessionState that only had live map + last diag
        // would miss A — applying floors from snapshot blocks late Cancelling.
        let mut cold = fresh_state();
        // Seed floors as attach would (from snapshot field).
        for (id, ver) in &back.tool_watchdog_max_versions {
            cold.tool_watchdog_max_versions.insert(id.clone(), *ver);
        }
        for (id, p) in &back.tool_watchdog_projections {
            cold.tool_watchdog_projections.insert(id.clone(), p.clone());
        }
        cold.last_tool_watchdog_diagnostic = back.last_tool_watchdog_diagnostic.clone();

        cold.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection("lease-a", 2, ToolWatchdogPhase::Cancelling),
        });
        assert!(
            !cold
                .to_snapshot()
                .tool_watchdog_projections
                .contains_key("lease-a"),
            "late A Cancelling(v2) must not resurrect after cold multi-lease hydrate"
        );
    }

    /// Concurrent leases: high per-lease version must not hide a newer
    /// transition from a different lease (versions restart at 1 per lease).
    #[test]
    fn last_diagnostic_prefers_transition_at_over_lease_version() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        // Older lease extended many times → high version, old transition wall time.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection_at(
                "lease-old",
                9,
                ToolWatchdogPhase::Grace,
                "2026-07-22T12:05:00Z",
            ),
        });
        // Newer lease first warning → version 1, later wall time.
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection_at(
                "lease-new",
                1,
                ToolWatchdogPhase::Warning,
                "2026-07-22T12:15:00Z",
            ),
        });

        let snap = s.to_snapshot();
        let diag = snap
            .last_tool_watchdog_diagnostic
            .expect("diagnostic retained");
        assert_eq!(diag.lease_id, "lease-new");
        assert_eq!(diag.phase, ToolWatchdogPhase::Warning);
        assert_eq!(diag.transition_at, "2026-07-22T12:15:00Z");
        assert_eq!(snap.tool_watchdog_projections.len(), 2);
    }

    /// Two concurrent leases transitioning inside the same wall second: the
    /// later sub-second transition wins even when the older lease has a later
    /// grace_deadline (lease-local field, not a global sequence).
    #[test]
    fn last_diagnostic_same_second_prefers_later_millis_not_grace() {
        use crate::acp::tool_watchdog::{
            CancellationScope, ToolCategory, ToolWatchdogPhase, ToolWatchdogProjection,
        };

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: ToolWatchdogProjection {
                lease_id: "lease-old".into(),
                version: 5,
                tool_title: ToolCategory::Terminal,
                phase: ToolWatchdogPhase::Grace,
                last_progress_at: "2026-07-22T11:50:00.000Z".into(),
                transition_at: "2026-07-22T12:00:00.100Z".into(),
                transition_seq: 0,
                grace_deadline: Some("2026-07-22T12:10:00.000Z".into()),
                cancellation_scope: Some(CancellationScope::Terminal),
                error_code: None,
            },
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: ToolWatchdogProjection {
                lease_id: "lease-new".into(),
                version: 1,
                tool_title: ToolCategory::Mcp,
                phase: ToolWatchdogPhase::Warning,
                last_progress_at: "2026-07-22T11:59:00.000Z".into(),
                transition_at: "2026-07-22T12:00:00.900Z".into(),
                transition_seq: 0,
                grace_deadline: Some("2026-07-22T12:05:00.000Z".into()),
                cancellation_scope: Some(CancellationScope::McpRequest),
                error_code: None,
            },
        });

        let snap = s.to_snapshot();
        let diag = snap
            .last_tool_watchdog_diagnostic
            .expect("diagnostic retained");
        assert_eq!(diag.lease_id, "lease-new");
        assert_eq!(diag.phase, ToolWatchdogPhase::Warning);
        assert_eq!(diag.transition_at, "2026-07-22T12:00:00.900Z");
    }

    /// After timed_out leaves the actionable map, reattach still sees the
    /// secret-safe diagnostic via snapshot history.
    #[test]
    fn last_diagnostic_survives_timed_out_reattach_snapshot() {
        use crate::acp::tool_watchdog::ToolWatchdogPhase;

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection_at(
                "lease-t",
                2,
                ToolWatchdogPhase::Cancelling,
                "2026-07-22T12:19:00Z",
            ),
        });
        s.apply_event(&AcpEvent::ToolWatchdogChanged {
            projection: sample_watchdog_projection_at(
                "lease-t",
                3,
                ToolWatchdogPhase::TimedOut,
                "2026-07-22T12:20:00Z",
            ),
        });

        let snap = s.to_snapshot();
        assert!(
            snap.tool_watchdog_projections.is_empty(),
            "actionable map empty after timeout"
        );
        let diag = snap
            .last_tool_watchdog_diagnostic
            .as_ref()
            .expect("timed_out diagnostic must survive attach");
        assert_eq!(diag.phase, ToolWatchdogPhase::TimedOut);
        assert_eq!(diag.error_code.as_deref(), Some("tool_stalled_timeout"));
        assert_eq!(diag.transition_at, "2026-07-22T12:20:00Z");
        assert!(!diag.lease_id.is_empty());

        let json = serde_json::to_string(&snap).expect("serialize");
        let back: LiveSessionSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.last_tool_watchdog_diagnostic.as_ref().map(|d| d.phase),
            Some(ToolWatchdogPhase::TimedOut)
        );
        assert!(
            !json.contains("raw_input") && !json.contains("tool_call_id"),
            "diagnostic wire must stay secret-safe"
        );
    }

    #[test]
    fn latest_live_reply_prefers_answer_after_last_tool_call() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "let me check".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "Found 3 files.\nDetails here".into(),
            parent_tool_use_id: None,
        });
        // Last non-empty line of the text that follows the final tool call.
        assert_eq!(s.latest_live_reply(100).as_deref(), Some("Details here"));
    }

    #[test]
    fn latest_live_reply_falls_back_to_thinking_then_tool() {
        // Thinking only → `thinking:` prefix.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::Thinking {
            text: "pondering options".into(),
            parent_tool_use_id: None,
        });
        assert_eq!(
            s.latest_live_reply(100).as_deref(),
            Some("thinking: pondering options")
        );

        // A tool call with no trailing text / thinking → `running tool:` prefix.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-9".into(),
            title: "grep files".into(),
            kind: "search".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        assert_eq!(
            s.latest_live_reply(100).as_deref(),
            Some("running tool: grep files")
        );
    }

    #[test]
    fn latest_live_reply_truncates_to_char_budget_and_handles_empty() {
        // No live message yet → nothing to report.
        assert_eq!(fresh_state().latest_live_reply(100), None);

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "0123456789abcdef".into(),
            parent_tool_use_id: None,
        });
        assert_eq!(s.latest_live_reply(10).as_deref(), Some("0123456789…"));
    }

    #[test]
    fn latest_live_reply_extracts_last_line_from_large_multiline_and_truncates_utf8() {
        let mut s = fresh_state();
        // A large multi-line streamed answer, a final multi-byte line, then
        // trailing blank lines (which must be skipped). The tail extraction must
        // not copy the whole answer, and truncation must land on a codepoint
        // boundary.
        let huge = "x".repeat(5000);
        let last = "résumé 完成 ▸ 配置已更新";
        s.apply_event(&AcpEvent::ContentDelta {
            text: format!("{huge}\nintermediate\n{last}\n   \n"),
            parent_tool_use_id: None,
        });
        let out = s.latest_live_reply(8).unwrap();
        // First 8 chars of `last` are r é s u m é <space> 完, then a truncation
        // marker — codepoint-safe (8 multi-byte chars + the ellipsis), proving
        // the cap counts chars, not bytes.
        assert_eq!(out, "résumé 完…");
        assert_eq!(out.chars().count(), 9);
    }

    #[test]
    fn latest_live_reply_stitches_text_split_by_interleaved_thinking() {
        // A Thinking block between two text deltas yields two separate Text
        // blocks; their concatenation forms the single answer line.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "Answer ".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::Thinking {
            text: "hmm".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "continues here".into(),
            parent_tool_use_id: None,
        });
        assert_eq!(
            s.latest_live_reply(100).as_deref(),
            Some("Answer continues here")
        );
    }

    #[test]
    fn selectors_ready_event_latches_state_and_snapshot() {
        let mut s = fresh_state();
        assert!(!s.selectors_ready);
        assert!(!s.to_snapshot().selectors_ready);
        s.apply_event(&AcpEvent::SelectorsReady);
        assert!(s.selectors_ready);
        assert!(s.to_snapshot().selectors_ready);
        // Idempotent — staying true on a second apply.
        s.apply_event(&AcpEvent::SelectorsReady);
        assert!(s.selectors_ready);
    }

    #[test]
    fn conversation_status_changed_event_is_a_visible_field_noop() {
        use crate::db::entities::conversation::ConversationStatus;
        // Seed a fully-populated state so we can verify nothing visible mutates
        // when ConversationStatusChanged is applied.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-1".into(),
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "hello".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::ConversationLinked {
            conversation_id: 7,
            folder_id: 3,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        });
        let before = s.to_snapshot();
        let before_status = s.status.clone();
        let before_conversation_id = s.conversation_id;
        let before_external_id = s.external_id.clone();

        s.apply_event(&AcpEvent::ConversationStatusChanged {
            conversation_id: 7,
            status: ConversationStatus::InProgress,
        });

        // Visible state fields unchanged.
        assert_eq!(s.status, before_status);
        assert_eq!(s.conversation_id, before_conversation_id);
        assert_eq!(s.external_id, before_external_id);
        assert!(
            s.live_message.is_some(),
            "live_message must be preserved across status-changed event"
        );
        assert_eq!(s.active_tool_calls.len(), 1);
        assert!(s.active_tool_calls.contains_key("tc-1"));

        // Snapshot output unchanged (modulo last_activity_at which is internal).
        let after = s.to_snapshot();
        assert_eq!(
            serde_json::to_value(&before).unwrap(),
            serde_json::to_value(&after).unwrap(),
            "snapshot must be byte-identical after no-op event"
        );
    }

    #[test]
    fn conversation_linked_event_writes_ids_into_state_and_snapshot() {
        let mut s = fresh_state();
        assert_eq!(s.conversation_id, None);
        assert_eq!(s.folder_id, None);
        s.apply_event(&AcpEvent::ConversationLinked {
            conversation_id: 42,
            folder_id: 7,
            parent_conversation_id: None,
            parent_tool_use_id: None,
        });
        assert_eq!(s.conversation_id, Some(42));
        assert_eq!(s.folder_id, Some(7));
        let snap = s.to_snapshot();
        assert_eq!(snap.conversation_id, Some(42));
        assert_eq!(snap.folder_id, Some(7));
    }

    #[test]
    fn session_started_sets_external_id_and_connected_status() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-42".into(),
        });
        assert_eq!(s.external_id.as_deref(), Some("ext-42"));
        assert_eq!(s.status, ConnectionStatus::Connected);
    }

    #[tokio::test]
    async fn session_started_signal_fires_when_session_started_applies() {
        let mut s = fresh_state();
        let rx = s.install_session_started_signal();
        // Pre-fire: rx not ready.
        assert!(s.session_started_tx.is_some());

        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-1".into(),
        });

        // tx was take()'d.
        assert!(s.session_started_tx.is_none());
        // rx resolves with Ok(()) — bounded timeout because the test must
        // never hang if the signal logic regresses.
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx).await;
        assert!(
            matches!(result, Ok(Ok(()))),
            "rx must fire on SessionStarted; got {result:?}"
        );
    }

    #[tokio::test]
    async fn session_started_signal_is_single_shot_safe_against_replay() {
        let mut s = fresh_state();
        let rx = s.install_session_started_signal();
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-1".into(),
        });
        // Replay (or any second SessionStarted) must not panic / double-fire.
        s.apply_event(&AcpEvent::SessionStarted {
            session_id: "ext-2".into(),
        });
        // The first send delivered; rx is consumed.
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx).await;
        assert!(matches!(result, Ok(Ok(()))));
    }

    #[tokio::test]
    async fn session_started_rx_aborts_when_state_drops_before_session_started() {
        // Mirrors the production "agent died before SessionStarted" path:
        // SessionState owns tx, gets dropped → rx receives RecvError. The
        // dedup waiter in `spawn_agent` treats this as "abort, release
        // dedup_lock, let next caller proceed".
        let rx = {
            let mut s = fresh_state();
            s.install_session_started_signal()
            // s drops here, taking tx with it.
        };
        let result = tokio::time::timeout(std::time::Duration::from_millis(50), rx).await;
        assert!(
            matches!(result, Ok(Err(_))),
            "rx must receive Err when sender drops without sending; got {result:?}"
        );
    }

    #[test]
    fn content_delta_creates_live_message_then_appends() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "hello ".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "world".into(),
            parent_tool_use_id: None,
        });
        let live = s.live_message.as_ref().expect("live_message expected");
        assert_eq!(
            live.content.len(),
            1,
            "consecutive text deltas merge into one block"
        );
        match &live.content[0] {
            LiveContentBlock::Text { text, .. } => assert_eq!(text, "hello world"),
            _ => panic!("expected text block"),
        }
        assert!(matches!(live.role, MessageRole::Assistant));
    }

    #[test]
    fn thinking_delta_creates_separate_block_from_text() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "T".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::Thinking {
            text: "X".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "Y".into(),
            parent_tool_use_id: None,
        });
        let live = s.live_message.as_ref().unwrap();
        assert_eq!(live.content.len(), 3);
        match &live.content[0] {
            LiveContentBlock::Text { text, .. } => assert_eq!(text, "T"),
            _ => panic!("expected text"),
        }
        match &live.content[1] {
            LiveContentBlock::Thinking { text, .. } => assert_eq!(text, "X"),
            _ => panic!("expected thinking"),
        }
        match &live.content[2] {
            LiveContentBlock::Text { text, .. } => assert_eq!(text, "Y"),
            _ => panic!("expected text"),
        }
    }

    /// Parent → subagent → parent interleave must produce three blocks: the
    /// merge predicate requires the SAME `parent_tool_use_id`, so subagent
    /// prose can never concatenate onto the main thread (and vice versa).
    #[test]
    fn parented_delta_interleave_never_merges_across_attribution() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: "main ".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "sub".into(),
            parent_tool_use_id: Some("toolu_parent".into()),
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "more main".into(),
            parent_tool_use_id: None,
        });
        let live = s.live_message.as_ref().unwrap();
        assert_eq!(live.content.len(), 3, "attribution boundaries split blocks");
        match &live.content[1] {
            LiveContentBlock::Text {
                text,
                parent_tool_use_id,
            } => {
                assert_eq!(text, "sub");
                assert_eq!(parent_tool_use_id.as_deref(), Some("toolu_parent"));
            }
            other => panic!("expected parented text block, got {other:?}"),
        }
    }

    #[test]
    fn parented_deltas_with_same_parent_merge() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: "a".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "b".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        // A different parent's thinking starts its own block.
        s.apply_event(&AcpEvent::Thinking {
            text: "t1".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        s.apply_event(&AcpEvent::Thinking {
            text: "t2".into(),
            parent_tool_use_id: Some("toolu_q".into()),
        });
        let live = s.live_message.as_ref().unwrap();
        assert_eq!(live.content.len(), 3);
        assert!(
            matches!(&live.content[0], LiveContentBlock::Text { text, .. } if text == "ab"),
            "same-parent text deltas merge"
        );
        assert!(
            matches!(&live.content[2], LiveContentBlock::Thinking { text, parent_tool_use_id }
                if text == "t2" && parent_tool_use_id.as_deref() == Some("toolu_q")),
            "different-parent thinking splits"
        );
    }

    /// Out-of-turn parented chunks (async subagent still streaming after the
    /// parent turn settled) must not resurrect a live_message via
    /// `ensure_live_message` — the snapshot would hand that ghost to every
    /// attaching client. Main-thread chunks keep the unconditional append.
    #[test]
    fn parented_delta_outside_prompting_does_not_touch_live_message() {
        let mut s = fresh_state();
        assert_ne!(s.status, ConnectionStatus::Prompting);
        s.apply_event(&AcpEvent::ContentDelta {
            text: "late sub text".into(),
            parent_tool_use_id: Some("toolu_gone".into()),
        });
        s.apply_event(&AcpEvent::Thinking {
            text: "late sub think".into(),
            parent_tool_use_id: Some("toolu_gone".into()),
        });
        assert!(
            s.live_message.is_none(),
            "parented chunks must not create live_message outside a turn"
        );
        s.apply_event(&AcpEvent::ContentDelta {
            text: "main".into(),
            parent_tool_use_id: None,
        });
        assert!(
            s.live_message.is_some(),
            "main-thread append stays unconditional"
        );
    }

    /// Snapshot round-trip: `parent_tool_use_id` survives serialization, and a
    /// snapshot written by an older backend (no field) still deserializes.
    #[test]
    fn live_block_parent_survives_snapshot_and_old_snapshots_parse() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: "sub".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        let snap = s.to_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let back: LiveSessionSnapshot = serde_json::from_str(&json).unwrap();
        let live = back.live_message.expect("live message in snapshot");
        assert!(matches!(
            &live.content[0],
            LiveContentBlock::Text { parent_tool_use_id, .. }
                if parent_tool_use_id.as_deref() == Some("toolu_p")
        ));

        let legacy: LiveContentBlock =
            serde_json::from_str(r#"{"kind":"text","text":"old"}"#).unwrap();
        assert!(matches!(
            legacy,
            LiveContentBlock::Text {
                parent_tool_use_id: None,
                ..
            }
        ));
    }

    /// `last_assistant_text` is the delegation child's result — a subagent's
    /// trailing prose is the CHILD's voice and must not read as the answer.
    #[test]
    fn last_assistant_text_ignores_parented_blocks() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::ContentDelta {
            text: "final answer".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: " SUBAGENT NOISE".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "sess-1".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
            mark_awaiting_reply: false,
            termination_source: None,
            provider_turn_id: None,
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("final answer"));
    }

    #[test]
    fn latest_live_reply_ignores_parented_blocks() {
        let mut s = fresh_state();
        s.status = ConnectionStatus::Prompting;
        s.apply_event(&AcpEvent::Thinking {
            text: "sub thinking".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "sub text".into(),
            parent_tool_use_id: Some("toolu_p".into()),
        });
        assert_eq!(
            s.latest_live_reply(200),
            None,
            "parented-only content must not surface as the parent's live reply"
        );
        s.apply_event(&AcpEvent::ContentDelta {
            text: "main progress".into(),
            parent_tool_use_id: None,
        });
        assert_eq!(s.latest_live_reply(200).as_deref(), Some("main progress"));
    }

    #[test]
    fn tool_call_inserts_pending_entry() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").expect("tc-1 inserted");
        assert_eq!(entry.status, ToolCallStatus::Pending);
        assert_eq!(entry.kind, ToolKind::Execute);
        assert_eq!(entry.label, "ls");
        assert!(entry.input.is_none());
        assert!(entry.output.is_none());
    }

    #[test]
    fn snapshot_active_tool_calls_are_sorted_by_id() {
        let mut s = fresh_state();
        for id in ["tc-z", "tc-a", "tc-m"] {
            s.apply_event(&AcpEvent::ToolCall {
                tool_call_id: id.into(),
                title: id.into(),
                kind: "read".into(),
                status: "pending".into(),
                content: None,
                raw_input: None,
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            });
        }
        let snap = s.to_snapshot();
        let ids: Vec<&str> = snap
            .active_tool_calls
            .iter()
            .map(|tc| tc.id.as_str())
            .collect();
        assert_eq!(ids, vec!["tc-a", "tc-m", "tc-z"]);
    }

    #[test]
    fn tool_call_content_field_is_preserved_on_state() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "ls".into(),
            kind: "execute".into(),
            status: "pending".into(),
            content: Some("line one\nline two".into()),
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").expect("tc-1 inserted");
        assert_eq!(entry.content.as_deref(), Some("line one\nline two"));

        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: None,
            content: Some("line three".into()),
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").unwrap();
        // Phase 2 chooses replace-on-update semantics: update == latest known content.
        assert_eq!(entry.content.as_deref(), Some("line three"));
    }

    #[test]
    fn tool_call_update_merges_status_and_output() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "cat foo.txt".into(),
            kind: "read".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        // raw_output text "\"file contents\"" — i.e. JSON-encoded string.
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: Some("\"file contents\"".into()),
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").unwrap();
        assert_eq!(entry.status, ToolCallStatus::Completed);
        assert_eq!(entry.kind, ToolKind::Read);
        assert_eq!(entry.label, "cat foo.txt");
        match &entry.output {
            Some(ToolCallOutput::Text { content }) => assert_eq!(content, "file contents"),
            other => panic!("expected text output, got {:?}", other),
        }
    }

    #[test]
    fn turn_complete_clears_live_and_tool_calls_and_pending_permission() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "hi".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "x".into(),
            kind: "read".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-1".into(),
            tool_call: serde_json::json!({"toolCallId": "tc-1", "title": "danger"}),
            options: vec![],
            queued: 0,
        });
        assert!(s.live_message.is_some());
        assert!(s.pending_permission.is_some());
        assert_eq!(s.active_tool_calls.len(), 1);
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });
        assert!(s.live_message.is_none());
        assert!(s.active_tool_calls.is_empty());
        assert!(s.pending_permission.is_none());
        assert_eq!(s.status, ConnectionStatus::Connected);
    }

    // --- active_delegations: running-only, snapshot-recoverable binding ---

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    fn empty_stats(
        started: DateTime<Utc>,
    ) -> crate::acp::delegation::runtime_stats::DelegationRuntimeStats {
        crate::acp::delegation::runtime_stats::DelegationRuntimeStats::empty(started)
    }

    fn delegation_started(parent_tool_use_id: &str, child_conv: i32) -> AcpEvent {
        let started = dt("2026-07-17T10:00:00Z");
        AcpEvent::DelegationStarted {
            parent_connection_id: "conn-test".into(),
            parent_tool_use_id: parent_tool_use_id.into(),
            child_connection_id: "child-conn-1".into(),
            child_conversation_id: child_conv,
            agent_type: AgentType::Codex,
            task_preview: "run the tests".into(),
            task_id: "task-1".into(),
            started_at: started,
            runtime_stats: empty_stats(started),
            attention_request: None,
        }
    }

    fn delegation_started_with(
        parent_tool_use_id: &str,
        task_id: &str,
        child_conv: i32,
        runtime_stats: crate::acp::delegation::runtime_stats::DelegationRuntimeStats,
    ) -> AcpEvent {
        AcpEvent::DelegationStarted {
            parent_connection_id: "conn-test".into(),
            parent_tool_use_id: parent_tool_use_id.into(),
            child_connection_id: "child-conn-1".into(),
            child_conversation_id: child_conv,
            agent_type: AgentType::Codex,
            task_preview: "run the tests".into(),
            task_id: task_id.into(),
            started_at: runtime_stats.started_at,
            runtime_stats,
            attention_request: None,
        }
    }

    fn delegation_completed(parent_tool_use_id: &str, child_conv: i32) -> AcpEvent {
        let started = dt("2026-07-17T10:00:00Z");
        AcpEvent::DelegationCompleted {
            parent_connection_id: "conn-test".into(),
            parent_tool_use_id: parent_tool_use_id.into(),
            child_connection_id: "child-conn-1".into(),
            child_conversation_id: child_conv,
            agent_type: AgentType::Codex,
            task_id: "task-1".into(),
            runtime_stats: empty_stats(started),
            result: DelegationResultSummary::Ok {
                duration_ms: 1,
                text_preview: None,
            },
            card_summary: None,
        }
    }

    fn delegation_completed_with(
        parent_tool_use_id: &str,
        task_id: &str,
        child_conv: i32,
        runtime_stats: crate::acp::delegation::runtime_stats::DelegationRuntimeStats,
    ) -> AcpEvent {
        AcpEvent::DelegationCompleted {
            parent_connection_id: "conn-test".into(),
            parent_tool_use_id: parent_tool_use_id.into(),
            child_connection_id: "child-conn-1".into(),
            child_conversation_id: child_conv,
            agent_type: AgentType::Codex,
            task_id: task_id.into(),
            runtime_stats,
            result: DelegationResultSummary::Ok {
                duration_ms: 1,
                text_preview: None,
            },
            card_summary: None,
        }
    }

    #[test]
    fn delegation_projection_events_replace_idempotently_and_snapshot_latest_state() {
        let mut state = fresh_state();
        let initial = empty_stats(dt("2026-07-17T10:00:00Z"));
        state.apply_event(&delegation_started_with(
            "tool-1",
            "task-1",
            99,
            initial.clone(),
        ));

        let mut changed = initial.clone();
        changed.tool_call_count = 3;
        let runtime_event = AcpEvent::DelegationRuntimeStatsChanged {
            parent_tool_use_id: "tool-1".into(),
            task_id: "task-1".into(),
            runtime_stats: changed.clone(),
        };
        state.apply_event(&runtime_event);
        state.apply_event(&runtime_event);

        let request = crate::acp::delegation::attention::AttentionRequestSummary {
            request_id: "req-1".into(),
            task_id: "task-1".into(),
            message: "Choose A or B".into(),
            created_at: dt("2026-07-17T10:01:00Z"),
        };
        let open = AcpEvent::DelegationAttentionChanged {
            parent_tool_use_id: "tool-1".into(),
            task_id: "task-1".into(),
            attention_request: Some(request.clone()),
        };
        state.apply_event(&open);
        state.apply_event(&open);
        let card = state.active_delegations.get("tool-1").unwrap();
        assert_eq!(card.runtime_stats, changed);
        assert_eq!(card.attention_request.as_ref().unwrap().request_id, "req-1");

        state.apply_event(&AcpEvent::DelegationAttentionChanged {
            parent_tool_use_id: "tool-1".into(),
            task_id: "task-1".into(),
            attention_request: None,
        });
        let snapshot = state.to_snapshot();
        assert_eq!(snapshot.active_delegations[0].task_id, "task-1");
        assert_eq!(
            snapshot.active_delegations[0].runtime_stats.tool_call_count,
            3
        );
        assert_eq!(snapshot.active_delegations[0].attention_request, None);

        state.apply_event(&delegation_completed_with("tool-1", "task-1", 99, changed));
        assert!(state.active_delegations.is_empty());
    }

    #[test]
    fn delegation_attention_changed_none_omits_field_on_wire_and_replays_as_clear() {
        let clear = AcpEvent::DelegationAttentionChanged {
            parent_tool_use_id: "tool-1".into(),
            task_id: "task-1".into(),
            attention_request: None,
        };
        let json = serde_json::to_value(&clear).expect("serialize");
        assert!(
            json.get("attention_request").is_none(),
            "optional clear must omit the field on the wire (Task 10 maps missing → null)"
        );
        let back: AcpEvent = serde_json::from_value(json).expect("deserialize");
        match back {
            AcpEvent::DelegationAttentionChanged {
                attention_request: None,
                ..
            } => {}
            other => panic!("expected clear attention, got {other:?}"),
        }

        let mut state = fresh_state();
        let started = empty_stats(dt("2026-07-17T10:00:00Z"));
        state.apply_event(&delegation_started_with("tool-1", "task-1", 1, started));
        state.apply_event(&AcpEvent::DelegationAttentionChanged {
            parent_tool_use_id: "tool-1".into(),
            task_id: "task-1".into(),
            attention_request: Some(crate::acp::delegation::attention::AttentionRequestSummary {
                request_id: "req-1".into(),
                task_id: "task-1".into(),
                message: "q".into(),
                created_at: dt("2026-07-17T10:01:00Z"),
            }),
        });
        state.apply_event(&back);
        assert_eq!(
            state
                .active_delegations
                .get("tool-1")
                .unwrap()
                .attention_request,
            None
        );
    }

    #[test]
    fn runtime_and_attention_events_ignore_mismatched_task_id() {
        let mut state = fresh_state();
        let started = empty_stats(dt("2026-07-17T10:00:00Z"));
        state.apply_event(&delegation_started_with(
            "tool-1",
            "task-1",
            1,
            started.clone(),
        ));
        let mut other = started;
        other.tool_call_count = 9;
        state.apply_event(&AcpEvent::DelegationRuntimeStatsChanged {
            parent_tool_use_id: "tool-1".into(),
            task_id: "task-other".into(),
            runtime_stats: other,
        });
        state.apply_event(&AcpEvent::DelegationAttentionChanged {
            parent_tool_use_id: "tool-1".into(),
            task_id: "task-other".into(),
            attention_request: Some(crate::acp::delegation::attention::AttentionRequestSummary {
                request_id: "x".into(),
                task_id: "task-other".into(),
                message: "no".into(),
                created_at: dt("2026-07-17T10:01:00Z"),
            }),
        });
        let card = state.active_delegations.get("tool-1").unwrap();
        assert_eq!(card.runtime_stats.tool_call_count, 0);
        assert!(card.attention_request.is_none());
    }

    #[test]
    fn delegation_started_populates_active_delegations_and_snapshot() {
        let mut s = fresh_state();
        s.apply_event(&delegation_started("pt-1", 99));

        let d = s
            .active_delegations
            .get("pt-1")
            .expect("active delegation recorded");
        assert_eq!(d.child_conversation_id, 99);
        assert_eq!(d.child_connection_id, "child-conn-1");
        assert_eq!(d.agent_type, AgentType::Codex);

        // Surfaced on the snapshot, and survives the JSON round-trip the web
        // client hydrates from.
        let snap = s.to_snapshot();
        assert_eq!(snap.active_delegations.len(), 1);
        let json = serde_json::to_string(&snap).unwrap();
        let back: LiveSessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.active_delegations.len(), 1);
        assert_eq!(back.active_delegations[0].parent_tool_use_id, "pt-1");
        assert_eq!(back.active_delegations[0].child_conversation_id, 99);
    }

    #[test]
    fn active_delegations_survives_turn_complete() {
        // Core regression for the web-only bug: an async delegation's child runs
        // in the background AFTER the parent's `delegate_to_agent` tool call
        // returns and the parent turn completes. TurnComplete clears
        // live_message / active_tool_calls but MUST NOT clear active_delegations
        // — otherwise the running binding vanishes from the snapshot the instant
        // the parent turn ends, and a web/server attach (snapshot path) can't
        // recover it.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "pt-1".into(),
            title: "delegate_to_agent".into(),
            kind: "other".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&delegation_started("pt-1", 99));
        assert!(s.active_tool_calls.contains_key("pt-1"));
        assert!(s.active_delegations.contains_key("pt-1"));

        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });

        assert!(
            s.active_tool_calls.is_empty(),
            "TurnComplete still clears in-flight tool calls"
        );
        assert!(
            s.active_delegations.contains_key("pt-1"),
            "running delegation binding must survive TurnComplete"
        );
        assert_eq!(
            s.to_snapshot().active_delegations.len(),
            1,
            "binding still on the snapshot a post-turn attach would receive"
        );
    }

    #[test]
    fn delegation_completed_removes_entry() {
        // Completed delegations are NOT retained here — their terminal state is
        // recovered from the child's persisted DB row (inject_delegation_meta)
        // and the live DelegationProvider binding, not from this in-flight set.
        let mut s = fresh_state();
        s.apply_event(&delegation_started("pt-1", 99));
        assert!(s.active_delegations.contains_key("pt-1"));
        s.apply_event(&delegation_completed("pt-1", 99));
        assert!(
            !s.active_delegations.contains_key("pt-1"),
            "completed delegation removed from the in-flight set"
        );
        assert!(s.to_snapshot().active_delegations.is_empty());
    }

    #[test]
    fn delegation_completed_without_started_is_noop() {
        // A stream that only delivered the completion (started never observed on
        // this connection) must not synthesize a phantom entry: removing an
        // absent key is a no-op, and there is no running child to bind.
        let mut s = fresh_state();
        s.apply_event(&delegation_completed("pt-unknown", 7));
        assert!(s.active_delegations.is_empty());
    }

    #[test]
    fn active_delegations_unbounded_by_running_fanout() {
        // No cap: a parent fanning out far past any old soft bound keeps every
        // running binding (size tracks live concurrency, not an artificial
        // limit). Completing them drains the set back to empty.
        let mut s = fresh_state();
        let n: i32 = 200;
        for i in 0..n {
            s.apply_event(&delegation_started(&format!("pt-{i}"), 1000 + i));
        }
        assert_eq!(s.active_delegations.len(), n as usize);
        assert_eq!(s.to_snapshot().active_delegations.len(), n as usize);
        for i in 0..n {
            s.apply_event(&delegation_completed(&format!("pt-{i}"), 1000 + i));
        }
        assert!(s.active_delegations.is_empty());
    }

    #[test]
    fn delegation_binding_survives_snapshot_split_like_live() {
        // Path A (live): apply started + completed straight through.
        // Path B (reconnect): apply started, snapshot round-trip mid-flight,
        // then apply completed. Both must converge — proving a running
        // delegation recovered from the snapshot ends identically to one tracked
        // live. This is the exact web-attach path the original bug broke.
        let mut a = fresh_state();
        a.apply_event(&delegation_started("tc-1", 99));
        a.apply_event(&delegation_completed("tc-1", 99));

        let mut b = fresh_state();
        b.apply_event(&delegation_started("tc-1", 99));
        // Snapshot round-trip while the child is still running: the running
        // binding must ride along on the wire shape the web client hydrates from.
        let snap = b.to_snapshot();
        assert_eq!(snap.active_delegations.len(), 1);
        assert_eq!(snap.active_delegations[0].parent_tool_use_id, "tc-1");
        let wire = serde_json::to_string(&snap).unwrap();
        let _back: LiveSessionSnapshot = serde_json::from_str(&wire).unwrap();
        b.apply_event(&delegation_completed("tc-1", 99));

        assert_eq!(
            serde_json::to_value(a.to_snapshot().active_delegations).unwrap(),
            serde_json::to_value(b.to_snapshot().active_delegations).unwrap(),
            "snapshot-recovered delegation must match the live-tracked one"
        );
    }

    #[test]
    fn turn_complete_captures_only_trailing_text_block() {
        // last_assistant_text (the delegation result text surfaced by
        // get_delegation_status) keeps only the final text run — the answer
        // after the last tool call — not intermediate narration.
        let mut s = fresh_state();
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "let me check ".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::ToolCallRef {
                    tool_call_id: "tc".into(),
                },
                LiveContentBlock::Text {
                    text: "the answer is 42".into(),
                    parent_tool_use_id: None,
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("the answer is 42"));
    }

    #[test]
    fn turn_complete_no_tool_calls_captures_full_text() {
        // With no tool call to split on, the trailing run is the whole answer.
        let mut s = fresh_state();
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "part 1 ".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::Text {
                    text: "part 2".into(),
                    parent_tool_use_id: None,
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("part 1 part 2"));
    }

    #[test]
    fn turn_complete_keeps_final_text_before_a_trailing_plan_block() {
        // `PlanUpdate` re-appends a Plan block at the END of content, so the
        // agent's concluding answer often sits BEFORE a trailing Plan. The
        // result must still be the text after the last tool call, not empty.
        let mut s = fresh_state();
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "let me check".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::ToolCallRef {
                    tool_call_id: "tc".into(),
                },
                LiveContentBlock::Text {
                    text: "the answer is 42".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::Plan {
                    entries: serde_json::json!([]),
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });
        assert_eq!(s.last_assistant_text.as_deref(), Some("the answer is 42"));
    }

    #[test]
    fn turn_complete_clears_stale_last_assistant_text() {
        // A turn that ends with no concluding text must CLEAR any prior value
        // rather than leak it as this turn's delegation result.
        let mut s = fresh_state();
        s.last_assistant_text = Some("stale text from an earlier turn".into());
        s.live_message = Some(LiveMessage {
            id: "m1".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "working".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::ToolCallRef {
                    tool_call_id: "tc".into(),
                },
            ],
            started_at: Utc::now(),
        });
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });
        assert_eq!(s.last_assistant_text, None);
    }

    #[test]
    fn visible_assistant_text_uses_text_after_last_tool_only() {
        let live = LiveMessage {
            id: "m".into(),
            role: MessageRole::Assistant,
            content: vec![
                LiveContentBlock::Text {
                    text: "before ".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::ToolCallRef {
                    tool_call_id: "t1".into(),
                },
                LiveContentBlock::Thinking {
                    text: "noise".into(),
                    parent_tool_use_id: None,
                },
                LiveContentBlock::Text {
                    text: "answer".into(),
                    parent_tool_use_id: None,
                },
            ],
            started_at: Utc::now(),
        };
        assert_eq!(visible_assistant_text(Some(&live)), "answer");
    }

    #[test]
    fn visible_assistant_text_none_and_thinking_only_are_empty() {
        assert_eq!(visible_assistant_text(None), "");
        let live = LiveMessage {
            id: "m".into(),
            role: MessageRole::Assistant,
            content: vec![LiveContentBlock::Thinking {
                text: "…".into(),
                parent_tool_use_id: None,
            }],
            started_at: Utc::now(),
        };
        assert_eq!(visible_assistant_text(Some(&live)), "");
    }

    #[test]
    fn turn_complete_clears_stale_when_live_message_is_none() {
        // When live_message is already gone, TurnComplete must still clear
        // stale last_assistant_text rather than leave it for the next consumer.
        let mut s = fresh_state();
        s.last_assistant_text = Some("stale".into());
        s.live_message = None;
        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "codex".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });
        assert_eq!(s.last_assistant_text, None);
    }

    #[test]
    fn permission_resolved_clears_matching_request() {
        // Mirrors the pet snapshot semantics: when the user (or auto-approve)
        // responds, the snapshot's pending_permission must drop *before*
        // TurnComplete, otherwise a snapshot-recovering frontend (WS attach
        // after a refresh) would re-render a dialog the user has already
        // answered.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-1".into(),
            tool_call: serde_json::json!({"toolCallId": "tc-1"}),
            options: vec![],
            queued: 0,
        });
        assert!(s.pending_permission.is_some());

        s.apply_event(&AcpEvent::PermissionResolved {
            request_id: "p-1".into(),
        });
        assert!(
            s.pending_permission.is_none(),
            "matching PermissionResolved must clear the pending permission"
        );
    }

    #[test]
    fn permission_queue_depth_updates_only_the_visible_request() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-visible".into(),
            tool_call: serde_json::json!({"toolCallId": "tc-visible"}),
            options: vec![],
            queued: 0,
        });
        s.apply_event(&AcpEvent::PermissionQueueDepth { depth: 2 });

        let pending = s.pending_permission.as_ref().expect("visible request");
        assert_eq!(pending.request_id, "p-visible");
        assert_eq!(pending.queued, 2);

        s.apply_event(&AcpEvent::PermissionResolved {
            request_id: "p-stale".into(),
        });
        assert_eq!(
            s.pending_permission
                .as_ref()
                .expect("stale resolution is ignored")
                .request_id,
            "p-visible"
        );
    }

    #[test]
    fn permission_resolved_stale_request_is_noop() {
        // A late `PermissionResolved` for an already-replaced request must
        // not wipe out the *new* outstanding permission — id mismatch is
        // the only thing distinguishing the two, since the snapshot only
        // tracks one pending permission at a time.
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-2".into(),
            tool_call: serde_json::json!({"toolCallId": "tc-2"}),
            options: vec![],
            queued: 0,
        });

        s.apply_event(&AcpEvent::PermissionResolved {
            request_id: "p-stale".into(),
        });
        let p = s
            .pending_permission
            .as_ref()
            .expect("stale PermissionResolved must not clear a non-matching pending permission");
        assert_eq!(p.request_id, "p-2");
    }

    #[test]
    fn permission_request_preserves_full_tool_call_value() {
        let mut s = fresh_state();
        // Realistic permission payload: title + kind + rawInput (used by the
        // frontend's permission parser to extract command / diff / plan).
        // After the refresh-survives-permission fix, all of this must round
        // trip via the snapshot — losing rawInput would force the user to
        // approve blind.
        let raw_tool_call = serde_json::json!({
            "toolCallId": "tc-9",
            "title": "Run rm -rf /",
            "kind": "execute",
            "rawInput": { "command": "rm -rf /" },
            "locations": [{ "path": "/", "line": 1 }],
        });
        s.apply_event(&AcpEvent::PermissionRequest {
            request_id: "p-1".into(),
            tool_call: raw_tool_call.clone(),
            options: vec![],
            queued: 0,
        });
        let p = s.pending_permission.as_ref().expect("permission set");
        assert_eq!(p.request_id, "p-1");
        assert_eq!(p.tool_call_id, "tc-9");
        assert_eq!(
            p.tool_call, raw_tool_call,
            "full tool_call JSON must round-trip into PendingPermissionState"
        );

        // Snapshot round-trip preserves it byte-for-byte (the load-bearing
        // property — frontend re-renders the approval dialog from this).
        let snap = s.to_snapshot();
        let snap_perm = snap.pending_permission.as_ref().unwrap();
        assert_eq!(snap_perm.tool_call, raw_tool_call);
    }

    #[test]
    fn mode_changed_updates_current_mode_and_session_modes_seeds_state() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::SessionModes {
            modes: SessionModeStateInfo {
                current_mode_id: "default".into(),
                available_modes: vec![SessionModeInfo {
                    id: "default".into(),
                    name: "Default".into(),
                    description: None,
                }],
            },
        });
        assert_eq!(s.current_mode.as_deref(), Some("default"));
        assert!(s.modes.is_some());
        s.apply_event(&AcpEvent::ModeChanged {
            mode_id: "edit".into(),
        });
        assert_eq!(s.current_mode.as_deref(), Some("edit"));
        // Snapshot consistency invariant: ModeChanged must keep
        // `modes.current_mode_id` in sync with the scalar `current_mode`.
        // The frontend's `denormalizeSnapshot` reads `modes.current_mode_id`
        // exclusively; without this sync a post-refresh hydration would
        // show the stale default even though the live event stream had
        // long since switched modes.
        assert_eq!(
            s.modes.as_ref().unwrap().current_mode_id,
            "edit",
            "ModeChanged must keep modes.current_mode_id consistent for snapshot consumers"
        );
    }

    #[test]
    fn snapshot_excludes_internal_chunk_buffers_and_carries_negotiated_caps() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::PromptCapabilities {
            prompt_capabilities: PromptCapabilitiesInfo {
                image: true,
                audio: false,
                embedded_context: true,
            },
        });
        s.apply_event(&AcpEvent::ForkSupported { supported: true });
        s.apply_event(&AcpEvent::SessionConfigOptions {
            config_options: vec![SessionConfigOptionInfo {
                id: "model".into(),
                name: "Model".into(),
                description: None,
                category: None,
                kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                    current_value: "sonnet".into(),
                    options: vec![],
                    groups: vec![],
                }),
            }],
        });
        s.apply_event(&AcpEvent::UsageUpdate {
            used: 1234,
            size: 200_000,
        });
        // Two raw_input fragments; the second is a complete JSON object
        // and should overwrite `entry.input` with the parsed value.
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "edit".into(),
            kind: "edit".into(),
            status: "pending".into(),
            content: None,
            raw_input: Some("{\"a\":".into()),
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        });
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: None,
            content: None,
            raw_input: Some("{\"a\":1}".into()),
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").unwrap();
        assert_eq!(entry.input, Some(serde_json::json!({"a": 1})));
        assert_eq!(entry.raw_input_chunks.len(), 2);

        let snapshot = s.to_snapshot();
        assert_eq!(snapshot.connection_id, "conn-test");
        assert!(snapshot.fork_supported);
        assert_eq!(
            snapshot.usage,
            Some(UsageInfo {
                used: 1234,
                size: 200_000,
            })
        );
        assert!(snapshot.prompt_capabilities.is_some());
        assert_eq!(snapshot.config_options.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(snapshot.active_tool_calls.len(), 1);

        // Wire shape: raw_input_chunks must NOT be serialized.
        let json = serde_json::to_value(&snapshot).unwrap();
        let tc_json = json["active_tool_calls"][0].clone();
        assert!(
            tc_json.get("raw_input_chunks").is_none(),
            "raw_input_chunks must be #[serde(skip)] (got {})",
            tc_json
        );
        assert_eq!(tc_json["input"], serde_json::json!({"a": 1}));
    }

    fn scripted_event_sequence() -> Vec<AcpEvent> {
        vec![
            AcpEvent::SessionStarted {
                session_id: "ext-1".into(),
            },
            AcpEvent::ContentDelta {
                text: "Hello ".into(),
                parent_tool_use_id: None,
            },
            AcpEvent::ContentDelta {
                text: "world".into(),
                parent_tool_use_id: None,
            },
            AcpEvent::ToolCall {
                tool_call_id: "tc-1".into(),
                title: "ls".into(),
                kind: "execute".into(),
                status: "pending".into(),
                content: None,
                raw_input: None,
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            },
            AcpEvent::ToolCallUpdate {
                tool_call_id: "tc-1".into(),
                title: None,
                status: Some("completed".into()),
                content: None,
                raw_input: None,
                raw_output: Some("\"done\"".into()),
                raw_output_append: None,
                locations: None,
                meta: None,
                images: None,
            },
            AcpEvent::Thinking {
                text: "considering".into(),
                parent_tool_use_id: None,
            },
            AcpEvent::ContentDelta {
                text: " More text".into(),
                parent_tool_use_id: None,
            },
            AcpEvent::UsageUpdate {
                used: 1234,
                size: 200_000,
            },
        ]
    }

    #[test]
    fn full_turn_lifecycle_increments_seq_monotonically() {
        let mut s = fresh_state();
        let events = scripted_event_sequence();
        let mut seq = 0u64;
        for e in &events {
            s.apply_event(e);
            seq += 1;
            s.event_seq = seq;
        }
        assert_eq!(s.event_seq, events.len() as u64);
    }

    /// Strip volatile fields that legitimately differ between Path A and Path B
    /// (e.g. `LiveMessage.id` is generated via `uuid::new_v4()` and `started_at`
    /// uses `Utc::now()`) but don't matter for snapshot/live consistency.
    fn normalize_snapshot(snap: &LiveSessionSnapshot) -> serde_json::Value {
        let mut v = serde_json::to_value(snap).unwrap();
        if let Some(lm) = v.get_mut("live_message") {
            if let Some(obj) = lm.as_object_mut() {
                obj.remove("id");
                obj.remove("started_at");
            }
        }
        v
    }

    /// 对账测试：从初始状态全程 apply 到 N 个事件 == 从 snapshot
    /// (apply 完前 K 个) + apply 剩下 N-K 个事件，最终状态等价。
    #[test]
    fn snapshot_filtered_events_yield_same_state_as_live_subscriber() {
        let events = scripted_event_sequence();
        let split = events.len() / 2;

        // Path A: live subscriber——全程 apply
        let mut a = fresh_state();
        for (i, e) in events.iter().enumerate() {
            a.apply_event(e);
            a.event_seq = (i + 1) as u64;
        }

        // Path B: snapshot 重连
        // 1) apply 前 split 个事件
        let mut b = fresh_state();
        for (i, e) in events.iter().take(split).enumerate() {
            b.apply_event(e);
            b.event_seq = (i + 1) as u64;
        }
        // 2) snapshot round-trip 通过 JSON
        let snapshot = b.to_snapshot();
        let _wire = serde_json::to_string(&snapshot).unwrap();
        // 3) 继续 apply 剩下事件
        for (i, e) in events.iter().enumerate().skip(split) {
            b.apply_event(e);
            b.event_seq = (i + 1) as u64;
        }

        let snap_a = a.to_snapshot();
        let snap_b = b.to_snapshot();

        assert_eq!(snap_a.event_seq, snap_b.event_seq);
        assert_eq!(snap_a.status, snap_b.status);
        assert_eq!(snap_a.external_id, snap_b.external_id);
        assert_eq!(snap_a.usage, snap_b.usage);

        // Full structural equivalence (with volatile fields stripped + tool
        // calls sorted by id). This is the load-bearing consistency check.
        assert_eq!(normalize_snapshot(&snap_a), normalize_snapshot(&snap_b));
    }

    // ---------- Phase 3c-3: snapshot fidelity ----------

    /// Helper: returns the kind discriminator + payload-id of each block in
    /// `live_message.content`, suitable for asserting block ordering.
    fn live_block_summary(s: &SessionState) -> Vec<(&'static str, String)> {
        s.live_message
            .as_ref()
            .map(|lm| {
                lm.content
                    .iter()
                    .map(|b| match b {
                        LiveContentBlock::Text { text, .. } => ("text", text.clone()),
                        LiveContentBlock::Thinking { text, .. } => ("thinking", text.clone()),
                        LiveContentBlock::ToolCallRef { tool_call_id } => {
                            ("tool_call_ref", tool_call_id.clone())
                        }
                        LiveContentBlock::Plan { entries } => {
                            ("plan", serde_json::to_string(entries).unwrap_or_default())
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn tool_call_event(id: &str, title: &str) -> AcpEvent {
        AcpEvent::ToolCall {
            tool_call_id: id.into(),
            title: title.into(),
            kind: "execute".into(),
            status: "pending".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: None,
        }
    }

    #[test]
    fn retry_rollback_clears_speculative_content_without_tool_boundary() {
        use crate::acp::types::PlanEntryInfo;

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "speculative answer".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::Thinking {
            text: "speculative reasoning".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "speculative plan".into(),
                priority: "high".into(),
                status: "pending".into(),
            }],
        });

        s.apply_event(&AcpEvent::TurnAttemptRollback { attempt: 1 });

        assert!(
            s.live_message
                .as_ref()
                .is_some_and(|live| live.content.is_empty()),
            "rollback keeps the live-message shell but clears its speculative blocks"
        );
        assert!(!s.has_live_agent_output());
    }

    #[test]
    fn retry_rollback_retains_content_through_last_tool_boundary() {
        use crate::acp::types::PlanEntryInfo;

        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "accepted prefix".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::Thinking {
            text: "accepted reasoning".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&tool_call_event("tc-1", "ls"));
        s.apply_event(&AcpEvent::Thinking {
            text: "speculative reasoning".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "speculative answer".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "speculative plan".into(),
                priority: "high".into(),
                status: "pending".into(),
            }],
        });

        s.apply_event(&AcpEvent::TurnAttemptRollback { attempt: 1 });

        assert_eq!(
            live_block_summary(&s),
            vec![
                ("text", "accepted prefix".into()),
                ("thinking", "accepted reasoning".into()),
                ("tool_call_ref", "tc-1".into()),
            ]
        );
        assert!(s.active_tool_calls.contains_key("tc-1"));
        assert!(s.has_live_agent_output());
    }

    #[test]
    fn tool_call_pushes_ref_block_at_current_position() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "before ".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&tool_call_event("tc-1", "ls"));
        s.apply_event(&AcpEvent::ContentDelta {
            text: "between".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&tool_call_event("tc-2", "pwd"));

        let summary = live_block_summary(&s);
        assert_eq!(
            summary,
            vec![
                ("text", "before ".to_string()),
                ("tool_call_ref", "tc-1".to_string()),
                ("text", "between".to_string()),
                ("tool_call_ref", "tc-2".to_string()),
            ],
            "tool-call refs must anchor at the position they arrived in the stream"
        );
    }

    #[test]
    fn tool_call_ref_push_is_idempotent() {
        let mut s = fresh_state();
        s.apply_event(&tool_call_event("tc-1", "ls"));
        // Defensive: second ToolCall with the same id (replay/unusual ordering)
        // must NOT push a duplicate ref block.
        s.apply_event(&tool_call_event("tc-1", "ls (retry)"));

        let summary = live_block_summary(&s);
        let ref_count = summary
            .iter()
            .filter(|(kind, id)| *kind == "tool_call_ref" && id == "tc-1")
            .count();
        assert_eq!(ref_count, 1, "duplicate ToolCall must not duplicate ref");
    }

    #[test]
    fn tool_call_update_does_not_duplicate_ref() {
        let mut s = fresh_state();
        s.apply_event(&tool_call_event("tc-1", "ls"));
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: Some("\"done\"".into()),
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });

        let summary = live_block_summary(&s);
        let ref_count = summary
            .iter()
            .filter(|(kind, id)| *kind == "tool_call_ref" && id == "tc-1")
            .count();
        assert_eq!(
            ref_count, 1,
            "ToolCall + ToolCallUpdate for same id yields exactly one ref"
        );
    }

    #[test]
    fn tool_call_state_carries_locations_and_meta() {
        let mut s = fresh_state();
        let locs = serde_json::json!([{ "path": "/tmp/foo.rs", "line": 12 }]);
        let meta = serde_json::json!({ "parent_tool_use_id": "abc", "session": "ext-1" });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "edit".into(),
            kind: "edit".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: Some(locs.clone()),
            meta: Some(meta.clone()),
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").expect("tc-1 inserted");
        assert_eq!(entry.locations.as_ref(), Some(&locs));
        assert_eq!(entry.meta.as_ref(), Some(&meta));

        // Snapshot round-trip preserves both.
        let snap = s.to_snapshot();
        let tc = snap
            .active_tool_calls
            .iter()
            .find(|t| t.id == "tc-1")
            .unwrap();
        assert_eq!(tc.locations.as_ref(), Some(&locs));
        assert_eq!(tc.meta.as_ref(), Some(&meta));
    }

    #[test]
    fn tool_call_update_preserves_locations_when_omitted() {
        let mut s = fresh_state();
        let locs = serde_json::json!([{ "path": "/tmp/foo.rs" }]);
        let meta = serde_json::json!({ "k": "v" });
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "tc-1".into(),
            title: "edit".into(),
            kind: "edit".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: Some(locs.clone()),
            meta: Some(meta.clone()),
            images: None,
        });
        // Subsequent partial update without locations/meta — must not clobber.
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "tc-1".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: Some("\"ok\"".into()),
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("tc-1").unwrap();
        assert_eq!(entry.status, ToolCallStatus::Completed);
        assert_eq!(
            entry.locations.as_ref(),
            Some(&locs),
            "ToolCallUpdate without locations must NOT clobber previously-set value"
        );
        assert_eq!(
            entry.meta.as_ref(),
            Some(&meta),
            "ToolCallUpdate without meta must NOT clobber previously-set value"
        );
    }

    #[test]
    fn tool_call_images_replace_or_preserve_on_update() {
        let mut s = fresh_state();
        let img_v1 = ToolCallImageInfo {
            data: "AAAA".into(),
            mime_type: "image/png".into(),
            uri: Some("/tmp/v1.png".into()),
        };
        let img_v2 = ToolCallImageInfo {
            data: "BBBB".into(),
            mime_type: "image/jpeg".into(),
            uri: None,
        };

        // Initial ToolCall carries one image — should be persisted.
        s.apply_event(&AcpEvent::ToolCall {
            tool_call_id: "ig-1".into(),
            title: "Image generation".into(),
            kind: "other".into(),
            status: "in_progress".into(),
            content: None,
            raw_input: None,
            raw_output: None,
            locations: None,
            meta: None,
            images: Some(vec![img_v1.clone()]),
        });
        let entry = s.active_tool_calls.get("ig-1").unwrap();
        assert_eq!(entry.images.len(), 1);
        assert_eq!(entry.images[0].data, "AAAA");

        // Update without images field — must preserve prior images.
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "ig-1".into(),
            title: None,
            status: Some("in_progress".into()),
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: None,
        });
        let entry = s.active_tool_calls.get("ig-1").unwrap();
        assert_eq!(
            entry.images.len(),
            1,
            "ToolCallUpdate with images=None must preserve prior images"
        );
        assert_eq!(entry.images[0].data, "AAAA");

        // Update with Some(new_vec) — must replace.
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "ig-1".into(),
            title: None,
            status: Some("completed".into()),
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: Some(vec![img_v2.clone()]),
        });
        let entry = s.active_tool_calls.get("ig-1").unwrap();
        assert_eq!(entry.images.len(), 1, "Some(vec) replaces prior images");
        assert_eq!(entry.images[0].data, "BBBB");
        assert_eq!(entry.images[0].mime_type, "image/jpeg");
        assert!(entry.images[0].uri.is_none());

        // Snapshot round-trip preserves images.
        let snap = s.to_snapshot();
        let tc = snap
            .active_tool_calls
            .iter()
            .find(|t| t.id == "ig-1")
            .unwrap();
        assert_eq!(tc.images.len(), 1);
        assert_eq!(tc.images[0].data, "BBBB");

        // Update with Some(empty) — must clear images (allows the agent to
        // explicitly drop a prior image if needed).
        s.apply_event(&AcpEvent::ToolCallUpdate {
            tool_call_id: "ig-1".into(),
            title: None,
            status: None,
            content: None,
            raw_input: None,
            raw_output: None,
            raw_output_append: None,
            locations: None,
            meta: None,
            images: Some(vec![]),
        });
        let entry = s.active_tool_calls.get("ig-1").unwrap();
        assert!(
            entry.images.is_empty(),
            "Some(empty vec) clears prior images"
        );
    }

    #[test]
    fn plan_update_appends_at_end_replacing_existing() {
        use crate::acp::types::PlanEntryInfo;
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "A".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "step v1".into(),
                priority: "high".into(),
                status: "pending".into(),
            }],
        });
        s.apply_event(&AcpEvent::ContentDelta {
            text: "B".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "step v2".into(),
                priority: "high".into(),
                status: "in_progress".into(),
            }],
        });

        let summary = live_block_summary(&s);
        // Expect: text("A"), text("B"), plan(v2). The old plan block is
        // removed and the fresh one is appended at end (after all current
        // text), matching the frontend reducer's replace-then-append.
        assert_eq!(summary.len(), 3, "summary was: {:?}", summary);
        assert_eq!(summary[0], ("text", "A".to_string()));
        assert_eq!(summary[1], ("text", "B".to_string()));
        assert_eq!(summary[2].0, "plan");
        assert!(
            summary[2].1.contains("step v2"),
            "plan block must be the v2 entries, not v1; got: {}",
            summary[2].1
        );
        assert!(
            !summary[2].1.contains("step v1"),
            "old plan block must be removed; got: {}",
            summary[2].1
        );
    }

    #[test]
    fn plan_update_creates_live_message_when_absent() {
        use crate::acp::types::PlanEntryInfo;
        let mut s = fresh_state();
        assert!(s.live_message.is_none());
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "first step".into(),
                priority: "medium".into(),
                status: "pending".into(),
            }],
        });
        let live = s
            .live_message
            .as_ref()
            .expect("PlanUpdate must lazily create live_message");
        assert_eq!(live.content.len(), 1);
        match &live.content[0] {
            LiveContentBlock::Plan { entries } => {
                assert!(
                    entries.to_string().contains("first step"),
                    "plan must carry the entries payload; got: {}",
                    entries
                );
            }
            other => panic!("expected Plan block, got {:?}", other),
        }
    }

    #[test]
    fn turn_complete_clears_plan_and_tool_refs() {
        use crate::acp::types::PlanEntryInfo;
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::ContentDelta {
            text: "x".into(),
            parent_tool_use_id: None,
        });
        s.apply_event(&tool_call_event("tc-1", "ls"));
        s.apply_event(&AcpEvent::PlanUpdate {
            entries: vec![PlanEntryInfo {
                content: "step".into(),
                priority: "low".into(),
                status: "pending".into(),
            }],
        });
        // Sanity precondition: live now has text, ref, plan.
        assert_eq!(live_block_summary(&s).len(), 3);
        assert_eq!(s.active_tool_calls.len(), 1);

        s.apply_event(&AcpEvent::TurnComplete {
            session_id: "ext".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
            mark_awaiting_reply: false,

            termination_source: None,
            provider_turn_id: None,
        });
        // The existing `live_message = None` clear handles the new block kinds
        // automatically — they live inside live_message, not as siblings.
        assert!(s.live_message.is_none());
        assert!(s.active_tool_calls.is_empty());
    }

    /// 验证 envelope 序列化 + 反序列化 round-trip
    #[test]
    fn event_envelope_round_trips_through_json() {
        let env = EventEnvelope {
            seq: 7,
            connection_id: "conn-x".into(),
            payload: AcpEvent::ContentDelta {
                text: "abc".into(),
                parent_tool_use_id: None,
            },
        };
        let json = serde_json::to_string(&env).unwrap();
        let back: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.seq, 7);
        assert_eq!(back.connection_id, "conn-x");
        match back.payload {
            AcpEvent::ContentDelta { text, .. } => assert_eq!(text, "abc"),
            _ => panic!("expected ContentDelta"),
        }
    }

    // --- live feedback: apply_event + snapshot --------------------------

    fn feedback_note(id: &str, text: &str) -> FeedbackItem {
        FeedbackItem::new_pending(id.into(), text.into(), Utc::now())
    }

    #[test]
    fn feedback_submitted_appends_idempotently() {
        let mut s = fresh_state();
        let item = feedback_note("f1", "use UserService");
        s.apply_event(&AcpEvent::FeedbackSubmitted { item: item.clone() });
        assert_eq!(s.feedback.len(), 1);
        // Replay / double-attach: a second apply with the same id is a no-op.
        s.apply_event(&AcpEvent::FeedbackSubmitted { item });
        assert_eq!(s.feedback.len(), 1, "duplicate id must not append twice");
        assert_eq!(s.feedback[0].status, FeedbackStatus::Pending);
        // A different id appends.
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f2", "skip the migration"),
        });
        assert_eq!(s.feedback.len(), 2);
    }

    #[test]
    fn feedback_consumed_marks_named_notes_delivered() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f1", "a"),
        });
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f2", "b"),
        });
        let at = Utc::now();
        s.apply_event(&AcpEvent::FeedbackConsumed {
            ids: vec!["f1".into()],
            delivered_at: at,
        });
        let f1 = s.feedback.iter().find(|f| f.id == "f1").unwrap();
        let f2 = s.feedback.iter().find(|f| f.id == "f2").unwrap();
        assert_eq!(f1.status, FeedbackStatus::Delivered);
        assert_eq!(f1.delivered_at, Some(at));
        assert_eq!(f2.status, FeedbackStatus::Pending, "unnamed note untouched");
        // Idempotent: re-applying the same consumption leaves f1 delivered and
        // does not flip its delivered_at to a new instant.
        s.apply_event(&AcpEvent::FeedbackConsumed {
            ids: vec!["f1".into()],
            delivered_at: Utc::now(),
        });
        let f1 = s.feedback.iter().find(|f| f.id == "f1").unwrap();
        assert_eq!(f1.delivered_at, Some(at), "delivered_at must not change");
    }

    #[test]
    fn user_message_clears_feedback_for_new_turn() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f1", "a"),
        });
        assert_eq!(s.feedback.len(), 1);
        // A new turn's user prompt resets the turn-scoped feedback set.
        s.apply_event(&text_user_message("user-1", "next prompt"));
        assert!(
            s.feedback.is_empty(),
            "feedback is turn-scoped; a new user_message clears it"
        );
    }

    #[test]
    fn snapshot_carries_feedback_and_omits_when_empty() {
        let mut s = fresh_state();
        s.apply_event(&AcpEvent::FeedbackSubmitted {
            item: feedback_note("f1", "snapshot me"),
        });
        let snap = s.to_snapshot();
        assert_eq!(snap.feedback.len(), 1);
        assert_eq!(snap.feedback[0].id, "f1");
        // Round-trips through the wire shape the web client hydrates from.
        let json = serde_json::to_string(&snap).unwrap();
        let back: LiveSessionSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.feedback.len(), 1);
        // The empty case keeps the NOTES array off the wire (the always-present
        // `feedback_tool_available` bool is a separate field).
        let empty = serde_json::to_string(&fresh_state().to_snapshot()).unwrap();
        assert!(
            !empty.contains("\"feedback\":"),
            "no-feedback snapshot must omit the notes array"
        );
    }

    #[test]
    fn live_session_snapshot_legacy_default_is_unmanaged_native_unavailable() {
        // Missing delegation_route field deserializes to legacy default.
        let json = r#"{
            "connection_id": "c1",
            "conversation_id": null,
            "folder_id": null,
            "status": "connecting",
            "external_id": null,
            "live_message": null,
            "active_tool_calls": [],
            "pending_permission": null,
            "modes": null,
            "current_mode": null,
            "config_options": null,
            "prompt_capabilities": null,
            "usage": null,
            "fork_supported": false,
            "available_commands": [],
            "selectors_ready": false,
            "event_seq": 0
        }"#;
        let snap: LiveSessionSnapshot = serde_json::from_str(json).expect("deserialize");
        assert_eq!(snap.delegation_route, legacy_unmanaged_route_snapshot());
        assert!(!snap.delegation_route.managed);
        assert_eq!(
            snap.delegation_route.effective,
            crate::acp::delegation::route::DelegationRoutePolicy::Native
        );
        assert!(!snap.delegation_route.delegation_available);
    }

    #[test]
    fn new_session_state_supplies_real_plan_snapshot_not_only_legacy_default() {
        use crate::acp::delegation::route::{
            DelegationRoutePlan, DelegationRoutePolicy, DelegationRouteSource,
            NativeSuppressionPlan, ROUTE_ADAPTER_CONTRACT_VERSION,
        };
        let plan = DelegationRoutePlan {
            managed: true,
            requested: DelegationRoutePolicy::Codeg,
            effective: DelegationRoutePolicy::Codeg,
            source: DelegationRouteSource::GlobalDefault,
            native_suppression: NativeSuppressionPlan::CodexMultiAgentFalse,
            expose_codeg_delegation: true,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: "snap-test".into(),
        };
        let mut s = fresh_state();
        s.set_route_plan_snapshot(&plan);
        s.set_delegation_available(true);
        let snap = s.to_snapshot();
        assert!(snap.delegation_route.managed);
        assert_eq!(
            snap.delegation_route.effective,
            DelegationRoutePolicy::Codeg
        );
        assert!(snap.delegation_route.delegation_available);
        // Wire uses snake_case; no secret token fields.
        let json = serde_json::to_value(&snap).unwrap();
        let route = &json["delegation_route"];
        assert_eq!(route["effective"], "codeg");
        assert_eq!(route["requested"], "codeg");
        assert_eq!(route["source"], "global_default");
        assert!(route.get("token").is_none());
    }
}
