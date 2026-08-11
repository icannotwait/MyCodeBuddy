use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use sacp::schema::{
    BlobResourceContents, CancelNotification, ClientCapabilities, ContentBlock, ContentChunk,
    CreateTerminalRequest, CreateTerminalResponse, ElicitationCapabilities,
    ElicitationFormCapabilities, EmbeddedResource, EmbeddedResourceResource,
    FileSystemCapabilities, ImageContent, InitializeRequest, KillTerminalRequest,
    KillTerminalResponse, LoadSessionRequest, LoadSessionResponse, Meta, NewSessionRequest,
    NewSessionResponse, PermissionOptionKind, Plan, PlanEntryPriority, PlanEntryStatus,
    PromptRequest, ProtocolVersion, ReadTextFileRequest, ReadTextFileResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, ResourceLink, ResumeSessionRequest,
    ResumeSessionResponse, SelectedPermissionOutcome, SessionConfigKind, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectGroup, SessionConfigSelectOption,
    SessionConfigSelectOptions, SessionId, SessionModeState, SessionNotification, SessionUpdate,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, SetSessionModeRequest,
    StopReason, TerminalExitStatus, TerminalOutputRequest, TerminalOutputResponse, TextContent,
    TextResourceContents, ToolCallContent, WaitForTerminalExitRequest, WaitForTerminalExitResponse,
    WriteTextFileRequest, WriteTextFileResponse,
};
use sacp::schema::{HttpHeader, McpServer, McpServerHttp, McpServerSse, McpServerStdio};
use sacp::util::MatchDispatch;
use sacp::{
    on_receive_request, Agent, Client, ConnectionTo, Dispatch, JsonRpcRequest, Responder,
    SessionMessage, UntypedMessage,
};
use sacp_tokio::AcpAgent;
use tokio::sync::{mpsc, oneshot, watch, RwLock};

use crate::acp::background_watch;
use crate::acp::error::AcpError;
use crate::acp::file_system_runtime::{
    mode_allows_outside_workspace, FileSystemRuntime, FileSystemRuntimeError, FsAccessPolicy,
};
use crate::acp::grok_retry::{GrokRetryAction, GrokRetryReconciler};
use crate::acp::registry::{self, AgentDistribution};
use crate::acp::session_state::SessionState;
use crate::acp::terminal_adapter::{adapter_for, AcpTerminalAdapter};
use crate::acp::terminal_assoc::{TerminalAssocFallback, ToolCallAssocHint};
use crate::acp::terminal_context::{terminal_metadata, TerminalPromptContext};
use crate::acp::terminal_runtime::{TerminalRuntime, TerminalRuntimeError};
use crate::acp::types::{
    AcpEvent, AvailableCommandInfo, ConfigStaleKind, ConnectionInfo, ConnectionStatus,
    GrokEffortSpec, PermissionOptionInfo, PlanEntryInfo, PromptCapabilitiesInfo, PromptInputBlock,
    SessionConfigKindInfo, SessionConfigOptionInfo, SessionConfigSelectGroupInfo,
    SessionConfigSelectInfo, SessionConfigSelectOptionInfo, SessionModeInfo, SessionModeStateInfo,
    ToolCallImageInfo, UserMessageBlock,
};
use crate::auto_title::{ConnectionLaunchContext, ConnectionPurpose};
use crate::models::agent::AgentType;
use crate::models::message::TurnTerminationSource;
use crate::models::system::AppLocale;
use crate::network::proxy;
use crate::terminal::shell::ResolvedShellSpec;
use crate::web::event_bridge::{emit_with_state, EventEmitter};

const DEFAULT_COMMAND_COLOR_ENV: [(&str, &str); 1] = [("CLICOLOR_FORCE", "1")];

/// Inject host `CODEX_PATH` into Codex launch env when a host binary is required
/// (only when experimental `CODEX_ACP_USE_CLI=1` is on). No-ops for non-Codex
/// agents; maps prepare failures to `SdkNotInstalled`.
fn apply_codex_cli_path_env(
    agent_type: AgentType,
    merged_env: Vec<(String, String)>,
) -> Result<Vec<(String, String)>, AcpError> {
    if agent_type != AgentType::Codex {
        return Ok(merged_env);
    }
    let map: BTreeMap<String, String> = merged_env.into_iter().collect();
    match crate::acp::codex_cli::prepare_codex_launch_env(map) {
        Ok(map) => {
            tracing::info!(
                "[ACP][Codex] CODEX_PATH={}",
                map.get(crate::acp::codex_cli::CODEX_PATH_ENV)
                    .map(|s| s.as_str())
                    .unwrap_or("(unset)")
            );
            Ok(map.into_iter().collect())
        }
        Err(message) => Err(AcpError::SdkNotInstalled(message)),
    }
}

fn merge_agent_env(
    env: &[(&'static str, &'static str)],
    runtime_env: &BTreeMap<String, String>,
) -> Vec<(String, String)> {
    // Env var order is not semantically meaningful; use map overwrite semantics
    // to keep precedence while avoiding repeated O(n) scans.
    let mut merged = BTreeMap::<String, String>::new();

    for (key, value) in DEFAULT_COMMAND_COLOR_ENV {
        merged.insert(key.to_string(), value.to_string());
    }

    for (key, value) in env {
        merged.insert((*key).to_string(), (*value).to_string());
    }

    for (key, value) in runtime_env {
        merged.insert(key.clone(), value.clone());
    }

    for (key, value) in proxy::current_proxy_env_vars() {
        merged.insert(key, value);
    }

    // Ensure agent-invoked `officecli …` (from an enabled office skill) resolves
    // even when codeg installed the binary outside the user's shell PATH — the
    // Windows self-managed dir, or `~/.local/bin` under a GUI launch.
    prepend_officecli_path(&mut merged);

    merged.into_iter().collect()
}

/// Cursor subscription-mode launch policy. When the user picked the official
/// subscription (browser login), guarantee the launched CLI sees NONE of the
/// custom-endpoint credentials — not even a stale `CURSOR_API_KEY` /
/// `CURSOR_API_BASE_URL` inherited from this process's environment (e.g. a dev
/// shell export). cursor-agent would otherwise validate that leaked key and
/// refuse to fall back to the login credential. An empty value tells the spawn
/// layer (vendored sacp-tokio) to `env_remove` the inherited var.
///
/// Gated on the explicit `CURSOR_AUTH_MODE` knob (written by the Cursor panel),
/// so legacy rows and operator-provided container env are left untouched. In
/// custom mode the credentials are present and non-empty, so nothing is cleared.
fn apply_cursor_env_policy(
    merged: &mut Vec<(String, String)>,
    runtime_env: &BTreeMap<String, String>,
) {
    if runtime_env.get("CURSOR_AUTH_MODE").map(String::as_str) != Some("subscription") {
        return;
    }
    for key in ["CURSOR_API_KEY", "CURSOR_API_BASE_URL"] {
        merged.retain(|(k, _)| k != key);
        merged.push((key.to_string(), String::new()));
    }
}

/// Grok's launch-time credential policy, mirroring [`apply_cursor_env_policy`].
/// When the user picked the `grok login` subscription (recorded as
/// `GROK_AUTH_MODE=subscription` by the Grok settings panel), scrub any
/// `XAI_API_KEY` inherited from this process's environment so the CLI falls back
/// to the browser-login credential in `~/.grok/auth.json` rather than a leaked
/// shell/container export. An empty value tells the spawn layer (vendored
/// sacp-tokio) to `env_remove` the inherited var. API-key, custom, legacy, and
/// no-mode rows are left untouched. Windows environment names are
/// case-insensitive, so every `XAI_API_KEY` alias is removed before the
/// canonical empty marker is appended.
fn apply_grok_env_policy_with_platform(
    merged: &mut Vec<(String, String)>,
    runtime_env: &BTreeMap<String, String>,
    windows: bool,
) {
    // BTreeMap iteration order matches the order `merge_agent_env` passes to
    // Command. On Windows, the last case-insensitive alias is effective.
    let auth_mode = runtime_env
        .iter()
        .rfind(|(key, _)| {
            if windows {
                key.eq_ignore_ascii_case("GROK_AUTH_MODE")
            } else {
                key.as_str() == "GROK_AUTH_MODE"
            }
        })
        .map(|(_, value)| value.as_str());
    if auth_mode != Some("subscription") {
        return;
    }
    let key = "XAI_API_KEY";
    merged.retain(|(candidate, _)| {
        if windows {
            !candidate.eq_ignore_ascii_case(key)
        } else {
            candidate != key
        }
    });
    merged.push((key.to_string(), String::new()));
}

fn apply_grok_env_policy(
    merged: &mut Vec<(String, String)>,
    runtime_env: &BTreeMap<String, String>,
) {
    apply_grok_env_policy_with_platform(merged, runtime_env, cfg!(windows));
}

fn apply_npx_launch_env_policy(
    agent_type: AgentType,
    merged: &mut Vec<(String, String)>,
    runtime_env: &BTreeMap<String, String>,
) {
    if agent_type == AgentType::Grok {
        apply_grok_env_policy(merged, runtime_env);
    }
}

/// Codex-only launch policy: force codex-acp's MCP name-conflict de-duplication
/// OFF. codeg injects its companion server (`codeg-mcp`) over ACP
/// `session/new.mcpServers`; codex-acp otherwise drops any ACP-passed server
/// whose name collides with a `config.toml` entry — global *or* project layer
/// (the check was widened to project `.codex/config.toml` in codex-acp #322) —
/// silently stripping codeg-mcp and with it ask_user_question / delegation /
/// feedback / session_info. The late `retain` + `push` makes the override win
/// over any user `runtime_env` twin, so the injection is guaranteed to survive.
fn apply_codex_env_policy(agent_type: AgentType, merged: &mut Vec<(String, String)>) {
    if agent_type != AgentType::Codex {
        return;
    }
    let key = "DISABLE_MCP_CONFIG_FILTERING";
    merged.retain(|(k, _)| k != key);
    merged.push((key.to_string(), "true".to_string()));
}

/// Prepend `dir` to the PATH entry of `env`, seeding from `fallback_path` when
/// `env` has no PATH key of its own. Removes any pre-existing PATH key first
/// (case-insensitively when `windows`, since Windows env keys are
/// case-insensitive) so the result has exactly one PATH entry — otherwise a
/// differently-cased duplicate (e.g. an inherited `Path` plus an inserted
/// `PATH`) could clobber the injected value when the child `Command` applies
/// them. Pure (no env/fs access) so it is unit-tested for both platforms.
fn prepend_dir_to_path_env(
    env: &mut BTreeMap<String, String>,
    dir: &str,
    fallback_path: &str,
    windows: bool,
) {
    let sep = if windows { ';' } else { ':' };
    // Collect every PATH-ish key. `BTreeMap` iterates sorted, so when several
    // differently-cased keys exist (e.g. both `Path` and `PATH`), the last is
    // the one the child `Command` applies last — i.e. the effective value under
    // Windows' case-insensitive env. Remove all of them so exactly one PATH
    // entry remains; a stale duplicate could otherwise overwrite the injected
    // value when the child applies them in order.
    let matching: Vec<String> = env
        .keys()
        .filter(|k| {
            if windows {
                k.eq_ignore_ascii_case("PATH")
            } else {
                k.as_str() == "PATH"
            }
        })
        .cloned()
        .collect();
    let mut existing_val: Option<String> = None;
    for k in &matching {
        existing_val = env.remove(k);
    }
    let existing_val = existing_val.unwrap_or_else(|| fallback_path.to_string());
    let new_path = if existing_val.is_empty() {
        dir.to_string()
    } else {
        format!("{dir}{sep}{existing_val}")
    };
    // Reuse the effective (last-sorted) key's casing when present; otherwise
    // default to the platform-conventional name (`Path` on Windows, `PATH` on Unix).
    let key = matching
        .into_iter()
        .next_back()
        .unwrap_or_else(|| if windows { "Path" } else { "PATH" }.to_string());
    env.insert(key, new_path);
}

/// Prepend codeg's known OfficeCLI install dir to `env`'s PATH when officecli is
/// installed there but not yet on the live PATH (see
/// `office_tools::officecli_agent_path_dir`). Applied to both the agent process
/// env (`merge_agent_env`) and the ACP terminal runtime's base env, so an
/// agent-invoked `officecli` resolves whether the agent execs it directly or
/// runs it through the client `terminal/create` tool. PATH-only: never forwards
/// model/API secrets.
fn prepend_officecli_path(env: &mut BTreeMap<String, String>) {
    if let Some(dir) = crate::commands::office_tools::officecli_agent_path_dir() {
        let fallback = std::env::var("PATH").unwrap_or_default();
        prepend_dir_to_path_env(env, &dir.to_string_lossy(), &fallback, cfg!(windows));
    }
}

/// The two actions codex's bespoke `_codex/session/goal_control` request
/// accepts (codex-acp #293, v1.1.4). Start / resume / re-objective are NOT part
/// of this method — those go through the `/goal` prompt (a real slash command;
/// only `/plan`, a config-option state toggle, is suppressed). Serializes to the
/// lowercase wire value codex expects (`"pause"` / `"clear"`) and deserializes
/// from the same string coming off the tauri command / HTTP endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalControlAction {
    Pause,
    Clear,
}

/// Commands sent from Tauri command handlers to the ACP connection loop.
pub const SUSPENSION_DRAIN_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuspensionAck {
    pub continuation_id: String,
    pub parent_turn_generation: u64,
}

pub enum ConnectionCommand {
    Prompt {
        blocks: Vec<PromptInputBlock>,
        /// Pre-projected cross-client user-message broadcast (`message_id` +
        /// user blocks), computed by the manager under the prompt lock. The
        /// loop emits it as `AcpEvent::UserMessage` right before issuing the
        /// agent request, so its seq strictly precedes the turn's assistant /
        /// status events (viewers apply in seq order) and it only fires for a
        /// prompt actually being processed. `None` for delegation children,
        /// empty prompts, unbound conversations, and non-linked senders.
        user_message: Option<(String, Vec<UserMessageBlock>)>,
        /// Per-turn awaiting-reply eligibility. Copied onto every real
        /// `AcpEvent::TurnComplete` emitted for this prompt so the lifecycle
        /// CAS can decide whether to mint a generation token.
        mark_awaiting_reply: bool,
        turn_generation: u64,
    },
    SetMode {
        mode_id: String,
    },
    SetConfigOption {
        config_id: String,
        value_id: String,
    },
    GoalControl {
        action: GoalControlAction,
    },
    RespondPermission {
        request_id: String,
        option_id: String,
    },
    Fork {
        reply:
            tokio::sync::oneshot::Sender<Result<crate::acp::types::ForkProtocolResult, AcpError>>,
    },
}

pub enum ConnectionControl {
    SuspendForDelegation {
        continuation_id: String,
        parent_turn_generation: u64,
        reply: oneshot::Sender<Result<SuspensionAck, AcpError>>,
    },
    /// Non-turn-ending terminal cancel. Handler must admit/ack quickly and
    /// must **not** await process-tree kill on the connection select loop.
    CancelTerminal {
        session_id: String,
        terminal_id: String,
        /// Quick admission ack only — must not wait for process-tree exit.
        reply: oneshot::Sender<Result<(), crate::acp::terminal_runtime::TerminalRuntimeError>>,
    },
    /// Explicit user stop (Stop button / user Cancel). Cascades parent-tree
    /// cancel of open delegations via [`finalize_active_user_cancel`].
    Cancel,
    /// Generation-guarded tool-watchdog turn cancel (session/cancel). Distinct
    /// from [`Self::Cancel`]: AutoTimeout must not cascade-cancel acknowledged
    /// background children; error codes stay `tool_stalled_timeout` /
    /// `user_cancelled` per cause.
    CancelTurn {
        turn_generation: u64,
        cause: crate::acp::tool_watchdog::CancelCause,
    },
    Disconnect,
}

struct LaneLiveness {
    sender_owners: std::sync::atomic::AtomicUsize,
    closed_tx: watch::Sender<bool>,
}

pub struct LaneSender<T> {
    tx: mpsc::Sender<T>,
    liveness: Arc<LaneLiveness>,
}

impl<T> Clone for LaneSender<T> {
    fn clone(&self) -> Self {
        let tx = self.tx.clone();
        let liveness = Arc::clone(&self.liveness);
        liveness
            .sender_owners
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self { tx, liveness }
    }
}

impl<T> Drop for LaneSender<T> {
    fn drop(&mut self) {
        if self
            .liveness
            .sender_owners
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel)
            == 1
        {
            self.liveness.closed_tx.send_replace(true);
        }
    }
}

impl<T> LaneSender<T> {
    pub async fn send(&self, value: T) -> Result<(), mpsc::error::SendError<T>> {
        self.tx.send(value).await
    }

    pub fn try_send(&self, value: T) -> Result<(), mpsc::error::TrySendError<T>> {
        self.tx.try_send(value)
    }

    pub async fn reserve(&self) -> Result<mpsc::Permit<'_, T>, mpsc::error::SendError<()>> {
        self.tx.reserve().await
    }

    pub fn capacity(&self) -> usize {
        self.tx.capacity()
    }

    pub fn max_capacity(&self) -> usize {
        self.tx.max_capacity()
    }
}

pub(crate) fn connection_channel<T>(
    capacity: usize,
) -> (LaneSender<T>, mpsc::Receiver<T>, watch::Receiver<bool>) {
    let (tx, rx) = mpsc::channel(capacity);
    let (closed_tx, closed_rx) = watch::channel(false);
    let liveness = Arc::new(LaneLiveness {
        sender_owners: std::sync::atomic::AtomicUsize::new(1),
        closed_tx,
    });
    (LaneSender { tx, liveness }, rx, closed_rx)
}

fn both_connection_lanes_closed(
    normal_lane_closed: bool,
    control_lane_closed: bool,
    cmd_liveness_rx: &watch::Receiver<bool>,
    control_liveness_rx: &watch::Receiver<bool>,
) -> bool {
    (normal_lane_closed || *cmd_liveness_rx.borrow())
        && (control_lane_closed || *control_liveness_rx.borrow())
}

struct SuspensionLease {
    continuation_id: String,
    parent_turn_generation: u64,
    connection_id: String,
    session_id: String,
    reply: Option<oneshot::Sender<Result<SuspensionAck, AcpError>>>,
}

enum TurnTerminalSource<'a> {
    Upstream(&'a str),
    UserCancel,
    SuspensionDrainTimeout,
}

enum TurnFinalizationDisposition {
    NaturalEnd(crate::acp::delegation::types::ParentTurnEndReason),
    UserCancelled,
    DelegationSuspended,
    SuspensionFailed,
}

/// Sentinel string embedded in a `sacp::Error` when the Initialize
/// handshake times out. Converted back to `AcpError::InitializeTimeout`
/// by the outer `.map_err(...)` in `run_connection`.
const INIT_TIMEOUT_SENTINEL: &str = "__codeg_init_timeout__";

/// RAII guard that removes the `AgentConnection` entry from the manager
/// map when dropped. Runs on both normal task exit AND task panic, so a
/// panic inside `run_connection` can't leak a stale map entry.
///
/// The `Mutex` is async, so we take two paths:
/// - If the lock is immediately available (`try_lock` succeeds), remove
///   the entry synchronously in the current context.
/// - Otherwise, spawn a short-lived cleanup task to acquire the lock
///   and remove the entry asynchronously. The guard must hold owned
///   `Arc<Mutex<_>>` and `String` so the spawned task has `'static`
///   captures.
struct ConnectionCleanupGuard {
    connections: Arc<tokio::sync::Mutex<HashMap<String, AgentConnection>>>,
    connection_id: String,
    connection_incarnation: String,
    tool_lease_registry: Arc<crate::acp::tool_watchdog::ToolExecutionLeaseRegistry>,
}

impl Drop for ConnectionCleanupGuard {
    fn drop(&mut self) {
        let connections = self.connections.clone();
        let connection_id = std::mem::take(&mut self.connection_id);
        let incarnation = std::mem::take(&mut self.connection_incarnation);
        let registry = self.tool_lease_registry.clone();
        // Always clear the incarnation from the lease registry BEFORE the map
        // entry becomes invisible to routing/scan. Manager-controlled disconnect
        // paths also clear synchronously; this path covers natural task exit
        // and panic unwind (remove_connection is idempotent).
        tokio::spawn(async move {
            let _ = registry
                .remove_connection(&connection_id, &incarnation)
                .await;
            connections.lock().await.remove(&connection_id);
        });
    }
}

/// Per-component config fingerprint captured at spawn and compared after
/// settings saves. Agent config, terminal shell, and delegation route are
/// tracked independently so one surface can stay stale while another reverts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfigFingerprint {
    pub agent_config: String,
    pub terminal_shell: String,
    pub delegation_route: String,
}

/// Latest settings values observed by staleness refresh (not necessarily the
/// spawn snapshot). `agent_kind` remembers whether the last agent-side drift
/// came from agent settings or a model-provider edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionObservedConfig {
    pub fingerprint: ConnectionConfigFingerprint,
    pub agent_kind: ConfigStaleKind,
}

/// Build matching spawn + observed fingerprints (used at connection launch and
/// by tests that seed synthetic connections).
pub fn matching_config_pair(
    agent_config: impl Into<String>,
    terminal_shell: impl Into<String>,
    delegation_route: impl Into<String>,
) -> (ConnectionConfigFingerprint, ConnectionObservedConfig) {
    let fingerprint = ConnectionConfigFingerprint {
        agent_config: agent_config.into(),
        terminal_shell: terminal_shell.into(),
        delegation_route: delegation_route.into(),
    };
    let observed = ConnectionObservedConfig {
        fingerprint: fingerprint.clone(),
        agent_kind: ConfigStaleKind::AgentConfig,
    };
    (fingerprint, observed)
}

/// Represents a single active ACP agent connection.
pub struct AgentConnection {
    pub id: String,
    pub agent_type: AgentType,
    pub status: ConnectionStatus,
    pub owner_window_label: String,
    /// Pop-out incarnation token; `None` for connections never rebound/owned by a pop-out op.
    pub owner_operation_id: Option<String>,
    /// Monotonic ownership generation for rebind CAS (0 = never rebound).
    pub ownership_generation: u64,
    /// Immutable host UUID for tool-watchdog lease identity. Minted at spawn;
    /// owner-window rebind does **not** change this value. Reconnect/replacement
    /// creates a new `AgentConnection` with a new incarnation.
    pub connection_incarnation: String,
    /// Shared with `ConnectionManager` / `SessionState` — process-scoped registry.
    pub tool_lease_registry: Arc<crate::acp::tool_watchdog::ToolExecutionLeaseRegistry>,
    /// Parent connection id for delegated children. Set at registration so
    /// concurrent rebind can find children not yet linked via
    /// `active_delegations` (conversation graph may lag spawn).
    pub parent_connection_id: Option<String>,
    /// Bounded FIFO for prompts, settings, permissions, and forks.
    pub cmd_tx: LaneSender<ConnectionCommand>,
    /// Bounded FIFO for suspension, user cancellation, and disconnect.
    pub control_tx: LaneSender<ConnectionControl>,
    /// Abort handle for the connection background task. Used by
    /// `teardown_unexposed_attempt` to terminate before `run_conversation_loop`
    /// starts (where a queued control-lane `Disconnect` would never drain).
    pub task_abort: Option<tokio::task::AbortHandle>,
    /// 后端权威的会话状态。所有 `emit_with_state` 写入此状态并自增 seq。
    /// 使用 `Arc<RwLock<_>>` 让 spawn 出的连接 task 与外部 snapshot 读取共享。
    pub state: Arc<RwLock<SessionState>>,
    /// 出口侧的事件发射器；管理器层（如 `send_prompt_linked`）需要直接发射
    /// `ConversationLinked` 等带 SessionState 写入的事件。
    pub emitter: EventEmitter,
    /// Serializes prompt sends per connection. Held across the
    /// link-check + DB write + emit + cmd_tx.send sequence so two
    /// concurrent prompts (multiple browser tabs of the same conversation,
    /// chat-channel + UI overlap) can't interleave and produce duplicate
    /// conversation rows or a confused agent that received two prompts
    /// in the same turn.
    pub prompt_lock: Arc<tokio::sync::Mutex<()>>,

    /// Component fingerprints captured when this process was spawned. The
    /// running process is locked to THESE values; comparing them against
    /// `observed_config` after a settings save detects drift. Immutable for
    /// the connection's lifetime.
    pub spawn_config: ConnectionConfigFingerprint,
    /// Most recent settings values seen by staleness refresh. Starts equal to
    /// `spawn_config`. Tracks "did anything change since we last looked" so a
    /// second real change re-emits `SessionConfigStale` while a no-op save
    /// stays silent. Agent, shell, and route components update independently.
    pub observed_config: ConnectionObservedConfig,
    /// Immutable terminal shell snapshot captured when this connection's
    /// process was spawned. Never re-read from settings after launch; reuse
    /// and reconnect keep this value for the connection's lifetime.
    pub terminal_shell: crate::terminal::shell::ResolvedShellSnapshot,
    /// Immutable managed-route plan resolved once before process launch.
    /// Settings/override changes never mutate this; they only refresh
    /// `observed_config.fingerprint.delegation_route`.
    pub route_plan: crate::acp::delegation::route::DelegationRoutePlan,
    /// Connection origin used at launch (root vs forced Codeg child).
    pub origin: crate::acp::delegation::route::DelegationConnectionOrigin,
    /// Session route preference used for comparison re-resolution.
    /// For persisted roots this mirrors the launch-time override; for
    /// row-less drafts it may be updated by
    /// `set_draft_delegation_route_preference` without touching `route_plan`.
    pub route_preference: Option<crate::acp::delegation::route::DelegationRoutePolicy>,
    /// Exact capability snapshot used when resolving `route_plan` at launch.
    /// Stale comparison re-resolves with these facts only — never optimistic.
    pub route_capability: crate::acp::delegation::route::RouteCapabilitySnapshot,
    /// OS process id of the spawned agent subprocess, published by the
    /// vendored `sacp-tokio` `on_spawn` callback. `0` until the process has
    /// launched (or if the pid was never observed). Used only as a shutdown
    /// backstop: `disconnect_all` kills this pid's whole process tree
    /// synchronously after the graceful-disconnect grace window, so agents
    /// (and their own child processes, e.g. MCP servers) never leak as orphans
    /// when the host process exits before `ChildGuard::drop` can run on the
    /// connection driver thread.
    ///
    /// Reset to `0` by the paired `on_exit` callback the moment the process is
    /// *reaped* — the only moment its pid stops naming our child and becomes
    /// reassignable. That reset is what keeps the backstop from ever aiming at
    /// a pid the OS has since handed to an unrelated process. Notably it does
    /// NOT fire merely because the connection ended: `ChildGuard::drop` signals
    /// the tree without waiting, so the agent may still be alive and still
    /// needs the backstop.
    pub child_pid: Arc<std::sync::atomic::AtomicU32>,
}

impl AgentConnection {
    pub fn info(&self) -> ConnectionInfo {
        ConnectionInfo {
            id: self.id.clone(),
            agent_type: self.agent_type,
            status: self.status.clone(),
        }
    }
}

/// Placeholder shell snapshot for unit/integration test connections that never
/// run a real terminal. Not used by production spawn (which always finalizes).
#[cfg(any(test, feature = "test-utils"))]
pub fn test_placeholder_terminal_shell() -> crate::terminal::shell::ResolvedShellSnapshot {
    use crate::terminal::shell::{
        ResolvedShellSnapshot, ResolvedShellSpec, ShellCommandStrategy, ShellDialect, ShellSource,
    };
    ResolvedShellSnapshot {
        selection_key: "system".into(),
        spec: ResolvedShellSpec {
            executable: PathBuf::from(if cfg!(windows) {
                r"C:\Windows\System32\cmd.exe"
            } else {
                "/bin/sh"
            }),
            dialect: if cfg!(windows) {
                ShellDialect::Cmd
            } else {
                ShellDialect::Posix
            },
            display_name: "test-shell".into(),
            source: ShellSource::System,
            command_strategy: if cfg!(windows) {
                ShellCommandStrategy::Cmd
            } else {
                ShellCommandStrategy::Posix
            },
        },
    }
}

/// Build an AcpAgent from registry metadata.
/// Directory handed to codex-acp via `APP_SERVER_LOGS` so its adapter-side
/// (ACP ↔ Codex app-server translation) logs land on disk for support.
///
/// Roots under the same `<cache>/app.mycodebuddy` tree as
/// [`binary_cache::cache_dir`] for consistency. Returns `None` — and the
/// caller injects nothing — when the system cache dir is unknown or the
/// directory can't be created: diagnostics must never block a connection.
fn codex_app_server_log_dir() -> Option<String> {
    let dir = dirs::cache_dir()?
        .join("app.mycodebuddy")
        .join("acp-logs")
        .join("codex-acp");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.to_string_lossy().into_owned())
}

/// Pi runs through pi-acp, which spawns the actual `pi` binary at runtime. If
/// `pi` (or the BYO-pi `PI_ACP_PI_COMMAND` override) isn't resolvable, pi-acp
/// dies mid-connection with a raw ENOENT. This preflight resolves the effective
/// command up front against the same `PATH` the child inherits and returns a
/// clear message when it can't be found; `None` means launch may proceed.
///
/// The message contains the literal substring "is not installed", which the
/// frontend matches to show the localized SDK-missing prompt with an "Open Agent
/// Settings" action (see `src/contexts/acp-connections-context.tsx`). Do not
/// change that substring.
fn pi_launch_preflight(runtime_env: &BTreeMap<String, String>) -> Option<String> {
    let custom = runtime_env
        .get("PI_ACP_PI_COMMAND")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let command = custom.unwrap_or("pi");
    if crate::commands::acp::resolve_pi_command_path(command).is_some() {
        return None;
    }
    Some(match custom {
        Some(cmd) => format!(
            "Pi is not installed: the custom pi command \"{cmd}\" was not found. \
             Update it in Agent Settings → Pi → Runtime."
        ),
        None => "Pi is not installed. Install it with: \
                 npm install -g @earendil-works/pi-coding-agent \
                 (or set a custom pi command in Agent Settings → Pi → Runtime)."
            .to_string(),
    })
}

/// Single application point for process-scoped route suppression (Codex env,
/// Grok/CodeBuddy argv). Call with the **complete** env and agent argv after
/// base flags/subcommand/registry args are assembled, and before
/// `AcpAgent::from_args`. Consumes `plan.native_suppression` only — never
/// resolves policy. Idempotent.
fn apply_process_route(
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
    agent_type: AgentType,
    env: &mut BTreeMap<String, String>,
    argv: &mut Vec<String>,
) -> Result<(), AcpError> {
    apply_route_environment(agent_type, plan, env)?;
    apply_route_argv(agent_type, plan, argv);
    Ok(())
}

/// Classify suppression application for audit (no env values / secrets).
pub(crate) fn suppression_application_for_plan(
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
) -> crate::acp::delegation::metrics::SuppressionApplication {
    use crate::acp::delegation::metrics::SuppressionApplication;
    use crate::acp::delegation::route::NativeSuppressionPlan;
    match &plan.native_suppression {
        NativeSuppressionPlan::None => SuppressionApplication::NotApplicable,
        _ => SuppressionApplication::Applied,
    }
}

/// Process env for Codeg native-suppression plans.
///
/// - Codex: merge `features.multi_agent=false` into the official `CODEX_CONFIG`
///   JSON contract
/// - Grok: set/override `GROK_SUBAGENTS=0` (documented host kill-switch; pairs
///   with argv `--no-subagents` and session `_meta.agentProfile` denylist)
///
/// Native, unmanaged, and non-matching agent plans leave keys byte-for-byte
/// untouched (including user values `0`/`1` and absence).
fn native_suppression_invalid() -> AcpError {
    AcpError::RouteUnavailable {
        reason: crate::acp::delegation::route::RouteDegradedReason::NativeSuppressionInvalid,
    }
}

fn merge_codex_official_native_suppression<F>(
    env: &mut BTreeMap<String, String>,
    windows: bool,
    inherited_config: F,
) -> Result<(), AcpError>
where
    F: FnOnce() -> Option<std::ffi::OsString>,
{
    // Match the spawn layer's effective-key behavior: explicit launch entries
    // override inherited env, and Windows env names are case-insensitive. When
    // aliases exist, BTreeMap iteration order is the order applied to Command,
    // so the last matching entry is effective.
    let matching: Vec<String> = env
        .keys()
        .filter(|key| {
            if windows {
                key.eq_ignore_ascii_case("CODEX_CONFIG")
            } else {
                key.as_str() == "CODEX_CONFIG"
            }
        })
        .cloned()
        .collect();
    let effective_key = matching.last().cloned();
    let raw = if let Some(raw) = effective_key.as_ref().and_then(|key| env.get(key)) {
        Some(raw.clone())
    } else {
        inherited_config()
            .map(|raw| raw.into_string().map_err(|_| native_suppression_invalid()))
            .transpose()?
    };

    let mut config = match raw.as_deref() {
        Some(raw) => serde_json::from_str::<serde_json::Value>(raw)
            .map_err(|_| native_suppression_invalid())?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };
    let root = config
        .as_object_mut()
        .ok_or_else(native_suppression_invalid)?;
    let features = root
        .entry("features")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let features = features
        .as_object_mut()
        .ok_or_else(native_suppression_invalid)?;
    features.insert("multi_agent".into(), serde_json::Value::Bool(false));
    let serialized = serde_json::to_string(&config).map_err(|_| native_suppression_invalid())?;

    // Commit only after every fallible step succeeds. Remove case-equivalent
    // Windows aliases so a stale later entry cannot override the merged value.
    for key in matching {
        env.remove(&key);
    }
    env.insert(
        effective_key.unwrap_or_else(|| "CODEX_CONFIG".into()),
        serialized,
    );
    Ok(())
}

fn apply_route_environment_with_inherited<F>(
    agent_type: AgentType,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
    env: &mut BTreeMap<String, String>,
    windows: bool,
    inherited_config: F,
) -> Result<(), AcpError>
where
    F: FnOnce() -> Option<std::ffi::OsString>,
{
    use crate::acp::delegation::route::NativeSuppressionPlan;

    match (&plan.native_suppression, agent_type) {
        (NativeSuppressionPlan::CodexMultiAgentFalse, AgentType::Codex) => {
            merge_codex_official_native_suppression(env, windows, inherited_config)?;
        }
        (NativeSuppressionPlan::GrokNoSubagents, AgentType::Grok) => {
            env.insert("GROK_SUBAGENTS".into(), "0".into());
        }
        _ => {}
    }
    Ok(())
}

fn apply_route_environment(
    agent_type: AgentType,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
    env: &mut BTreeMap<String, String>,
) -> Result<(), AcpError> {
    apply_route_environment_with_inherited(agent_type, plan, env, cfg!(windows), || {
        std::env::var_os("CODEX_CONFIG")
    })
}

/// Route-scoped argv tokens for Grok (`--no-subagents`) and CodeBuddy
/// (`--disallowedTools` union). Operates on the **complete** agent argv
/// (command + base flags + registry/subcommand args). Idempotent; never drops
/// or reorders unrelated tokens. No-ops for other agents / native plans.
fn apply_route_argv(
    agent_type: AgentType,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
    argv: &mut Vec<String>,
) {
    use crate::acp::delegation::route::NativeSuppressionPlan;

    match (&plan.native_suppression, agent_type) {
        (NativeSuppressionPlan::GrokNoSubagents, AgentType::Grok) => {
            // Structured insert: after root flags, before `agent stdio`.
            if argv.iter().any(|a| a == "--no-subagents") {
                return;
            }
            let insert_at = argv
                .windows(2)
                .position(|w| w[0] == "agent" && w[1] == "stdio")
                .unwrap_or(argv.len());
            argv.insert(insert_at, "--no-subagents".into());
        }
        (NativeSuppressionPlan::CodeBuddyDisallowedTools { tools }, AgentType::CodeBuddy) => {
            apply_codebuddy_disallowed_tools(argv, tools);
        }
        _ => {}
    }
}

/// Form one stable de-duplicated `--disallowedTools` union in `argv`, inserting
/// Codeg denial tools once while preserving existing user denies (including
/// `TaskOutput` / `TaskStop`). Emits before any trailing `--acp` token when
/// present; otherwise appends at the end.
fn apply_codebuddy_disallowed_tools(argv: &mut Vec<String>, suppress_tools: &[String]) {
    let mut existing: Vec<String> = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        if argv[i] == "--disallowedTools" {
            argv.remove(i);
            while i < argv.len() && !argv[i].starts_with('-') {
                existing.push(argv.remove(i));
            }
        } else {
            i += 1;
        }
    }

    let mut union = existing;
    for tool in suppress_tools {
        if !union.iter().any(|t| t == tool) {
            union.push(tool.clone());
        }
    }
    if union.is_empty() {
        return;
    }

    // Prefer immediately before `--acp` when that flag is present.
    let insert_at = argv.iter().position(|a| a == "--acp").unwrap_or(argv.len());
    let mut insert = Vec::with_capacity(1 + union.len());
    insert.push("--disallowedTools".to_string());
    insert.extend(union);
    for (offset, tok) in insert.into_iter().enumerate() {
        argv.insert(insert_at + offset, tok);
    }
}

/// Base Npx launch args only (route-independent). Grok root flags and
/// registry/subcommand tokens; **no** route suppression. Callers must build
/// the complete argv then apply route once via [`apply_process_route`].
fn append_npx_launch_args(
    parts: &mut Vec<String>,
    agent_type: AgentType,
    args: &[&str],
    grok_permission_mode: Option<&str>,
) {
    if agent_type == AgentType::Grok {
        parts.push("--no-auto-update".into());
        if let Some(mode) = grok_permission_mode {
            parts.push("--permission-mode".into());
            parts.push(mode.into());
        }
    }
    for arg in args {
        parts.push((*arg).into());
    }
}

/// Transcript directory for an agent that codeg must record itself, or `None`
/// for agents with their own store parser.
///
/// Only custom ACP agents are recorded: every built-in has a dedicated parser
/// reading the agent's native transcript, and recording those too would double
/// the storage while risking two disagreeing histories.
fn transcript_dir_for(agent_type: AgentType) -> Option<&'static str> {
    agent_type
        .custom_id()
        .map(|_| registry::registry_id_for(agent_type))
}

/// Ensure a custom agent's transcript file exists with its header. No-op for
/// built-ins, and idempotent per session (a reconnect keeps the original
/// header, so the session's original cwd/start time survive).
fn record_transcript_header(agent_type: AgentType, session_id: &str, cwd: &str) {
    record_transcript_header_continuing(agent_type, session_id, cwd, None);
}

/// [`record_transcript_header`] for a session that carries an existing
/// conversation forward.
///
/// `continues_from` is set when `session/load` failed and codeg opened a fresh
/// agent session for the same conversation: the earlier turns stay where they
/// are and this header links back to them, so the reader still sees one
/// history. See [`crate::acp_transcript::TranscriptHeader::continues_from`].
fn record_transcript_header_continuing(
    agent_type: AgentType,
    session_id: &str,
    cwd: &str,
    continues_from: Option<&str>,
) {
    let Some(dir) = transcript_dir_for(agent_type) else {
        return;
    };
    let mut header = crate::acp_transcript::TranscriptHeader::new(
        &agent_type.as_wire(),
        session_id,
        cwd,
        crate::acp_transcript::now_epoch_ms(),
    );
    if let Some(previous) = continues_from.filter(|p| !p.is_empty() && *p != session_id) {
        header = header.continuing(previous);
    }
    drop(crate::acp_transcript::record_header(dir, &header));
}

/// Record an outgoing prompt for a custom agent, and wait (briefly) for it to
/// land. No-op for agents with their own store.
///
/// Bound-waited like [`record_turn_end`], but for a sharper reason. The gate
/// that decides whether a later `session/load` replay may be recorded is
/// `acp_transcript::has_entries`, and it reads the FILE — a queued prompt is
/// invisible to it. Returning before the prompt is durable therefore leaves a
/// window in which a reconnect concludes "this conversation has no transcript",
/// records the agent's replay, and ends up with two copies of the same history.
///
/// The window is small but reachable (the writer can be behind on a slow disk,
/// and a conversation can be torn down between its first prompt and its turn
/// end, which is the other place codeg waits). A prompt happens once per turn,
/// so closing it costs one disk write per turn — nothing the user can perceive,
/// against a failure that is permanent and silent.
async fn record_prompt(agent_type: AgentType, session_id: &str, blocks: &[ContentBlock]) {
    let Some(dir) = transcript_dir_for(agent_type) else {
        return;
    };
    let Ok(payload) = serde_json::to_value(blocks) else {
        return;
    };
    let ack = crate::acp_transcript::record_entry(
        dir,
        session_id,
        crate::acp_transcript::EntryKind::Prompt,
        payload,
    );
    let _ = tokio::time::timeout(std::time::Duration::from_millis(2000), ack).await;
}

/// Record a turn's completion for a custom agent, and wait (briefly) for it to
/// land. No-op for agents with their own store.
///
/// The bounded wait exists because the frontend refetches conversation detail
/// right after `TurnComplete`; without it, a reopened conversation could be
/// read before the final lines were flushed. The bound means a stalled writer
/// delays nothing more than this.
async fn record_turn_end(
    agent_type: AgentType,
    session_id: &str,
    stop_reason: &str,
    started_at_ms: u64,
    model: Option<String>,
) {
    let Some(dir) = transcript_dir_for(agent_type) else {
        return;
    };
    let now = crate::acp_transcript::now_epoch_ms();
    let mut payload = serde_json::json!({
        "stopReason": stop_reason,
        "durationMs": now.saturating_sub(started_at_ms),
    });
    // ACP puts no model on the prompt response, so the session's model selector
    // is the only honest answer at turn end — and it is the same value the
    // composer showed while the turn ran. Recorded per turn rather than once in
    // the header because a mid-conversation model switch must not retroactively
    // relabel the turns that ran before it.
    if let (Some(obj), Some(model)) = (payload.as_object_mut(), model.filter(|m| !m.is_empty())) {
        obj.insert("model".to_string(), serde_json::Value::String(model));
    }
    let ack = crate::acp_transcript::record_entry(
        dir,
        session_id,
        crate::acp_transcript::EntryKind::TurnEnd,
        payload,
    );
    let _ = tokio::time::timeout(std::time::Duration::from_millis(2000), ack).await;
}

/// The model id a session's selectors currently report. Agent-agnostic: the
/// ACP `category: "model"` selector is the one channel every agent that has a
/// model at all publishes it on. `None` when the agent exposes no model
/// selector — most custom agents don't, and a fabricated label would be worse
/// than an empty field.
fn current_model_id_from_opts(opts: &[SessionConfigOptionInfo]) -> Option<String> {
    opts.iter()
        .find(|o| o.category.as_deref() == Some("model"))
        .map(|o| {
            let SessionConfigKindInfo::Select(sel) = &o.kind;
            sel.current_value.clone()
        })
        .filter(|m| !m.is_empty())
}

/// [`current_model_id_from_opts`] against the authoritative `SessionState`
/// snapshot.
async fn current_session_model_id(state: &Arc<RwLock<SessionState>>) -> Option<String> {
    let opts = state.read().await.config_options.clone()?;
    current_model_id_from_opts(&opts)
}

/// Queue one raw `session/update` for a custom agent, handing back the ack so
/// the caller decides whether landing it matters.
///
/// `None` when nothing was queued: not a custom agent, an update the history
/// projection never reads back (see
/// [`crate::parsers::acp_native::is_recorded_update`], which owns that call so
/// the filter cannot drift from the reader it exists to serve), or an
/// unserializable payload.
fn queue_transcript_update(
    agent_type: AgentType,
    session_id: &str,
    update: &SessionUpdate,
) -> Option<tokio::sync::oneshot::Receiver<()>> {
    let dir = transcript_dir_for(agent_type)?;
    if !crate::parsers::acp_native::is_recorded_update(update) {
        return None;
    }
    let payload = serde_json::to_value(update).ok()?;
    Some(crate::acp_transcript::record_entry(
        dir,
        session_id,
        crate::acp_transcript::EntryKind::Update,
        payload,
    ))
}

/// Record one raw `session/update` for a custom agent, fire and forget.
///
/// The ack is dropped: streamed chunks must never make the live read loop wait.
/// Turn boundaries are the only place the live path bound-waits.
fn record_transcript_update(agent_type: AgentType, session_id: &str, update: &SessionUpdate) {
    drop(queue_transcript_update(agent_type, session_id, update));
}

/// How long one hydrated line may take to land before hydration gives up on
/// recording. Only a wedged filesystem can reach it (a line costs tens of
/// microseconds), so it is not a throughput bound — it is the difference
/// between "the conversation opens with a truncated history and a warning" and
/// "opening the conversation hangs forever".
const HYDRATION_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// [`record_transcript_update`] **with backpressure**, for the `session/load`
/// hydration drain. Returns false once the writer has stopped keeping up, after
/// which the caller must stop recording.
///
/// The live path can afford to drop the ack because a lost chunk costs one
/// chunk. Hydration cannot: the replay it is draining is the ONLY copy of that
/// history, and it arrives as fast as it parses while the writer runs at disk
/// speed. Fire-and-forget there fills the bounded queue and then discards from
/// the MIDDLE of the history — silently, leaving a transcript with holes that
/// the `has_entries` gate will never let a later replay repair.
///
/// Awaiting each ack is async-native backpressure (no worker thread is blocked,
/// and one outstanding line cannot overflow a queue of thousands), and it turns
/// the pathological case from "history with random holes" into "history that
/// stops cleanly at a point" — which is what a prefix-honest reader can work
/// with.
async fn record_hydrated_update(
    agent_type: AgentType,
    session_id: &str,
    update: &SessionUpdate,
) -> bool {
    let Some(ack) = queue_transcript_update(agent_type, session_id, update) else {
        return true;
    };
    match tokio::time::timeout(HYDRATION_ACK_TIMEOUT, ack).await {
        // `Err(RecvError)` means the writer thread is gone; there is nothing
        // left to wait for and nothing more will land either.
        Ok(res) => res.is_ok(),
        Err(_) => {
            tracing::warn!(
                "[ACP] transcript writer stalled while hydrating {session_id}; \
                 stopping recording so the replay lands as a clean prefix"
            );
            false
        }
    }
}

async fn build_agent(
    agent_type: AgentType,
    runtime_env: &BTreeMap<String, String>,
    cwd: &Path,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
) -> Result<AcpAgent, AcpError> {
    // A conversation can outlive the custom-agent definition it was started
    // with (the user deleted it in settings). `get_agent_meta` cannot report
    // that — it is infallible — so it hands back a placeholder with an empty
    // command. Catch it here, before we try to spawn nothing and surface an
    // opaque ENOENT.
    if let Some(id) = agent_type.custom_id() {
        if !crate::acp::custom_registry::is_registered(id) {
            return Err(AcpError::SdkNotInstalled(format!(
                "The custom agent \"{id}\" is no longer registered. Re-add it in Settings → Agents to use this conversation."
            )));
        }
    }
    let meta = registry::get_agent_meta(agent_type);
    debug_assert_eq!(meta.agent_type, agent_type);

    let agent = match meta.distribution {
        AgentDistribution::Npx { cmd, args, env, .. } => {
            // pi-acp spawns the real `pi` binary; fail fast with a clear,
            // install-prompt-routable error if it (or a BYO-pi override) isn't
            // resolvable, rather than letting pi-acp die mid-connection on a raw
            // ENOENT that surfaces as an opaque protocol error.
            if agent_type == AgentType::Pi {
                if let Some(message) = pi_launch_preflight(runtime_env) {
                    return Err(AcpError::SdkNotInstalled(message));
                }
                // Trust the workspace codeg is launching pi into (default on, via
                // the PI_ACP_TRUST_WORKSPACE env_json key) so pi loads the
                // project's local config/skills without a redundant prompt. Gates
                // config loading only, never execution; scoped, additive, and
                // best-effort (never blocks the connect).
                crate::commands::acp::seed_pi_workspace_trust(cwd, runtime_env);
            }
            let mut merged_env = merge_agent_env(env, runtime_env);
            apply_npx_launch_env_policy(agent_type, &mut merged_env, runtime_env);
            apply_codex_env_policy(agent_type, &mut merged_env);
            // codex-acp 1.0.0 honors APP_SERVER_LOGS as a directory for its
            // adapter-side logs. Surface it only under CODEG_ACP_DEBUG so
            // default runs are unchanged; a directory-creation failure silently
            // skips injection (diagnostics must never block a connect).
            let want_codex_logs = agent_type == AgentType::Codex
                && std::env::var("CODEG_ACP_DEBUG")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
            if want_codex_logs {
                if let Some(dir) = codex_app_server_log_dir() {
                    merged_env.push(("APP_SERVER_LOGS".to_string(), dir));
                }
            }
            // Inject host CODEX_PATH when CODEX_ACP_USE_CLI=1 requires it.
            // Never overwrites an explicit user value; fails with
            // SdkNotInstalled if host Codex is missing in CLI mode.
            merged_env = apply_codex_cli_path_env(agent_type, merged_env)?;
            let mut env_map: BTreeMap<String, String> = merged_env.into_iter().collect();
            // Build complete agent argv first (command + base flags + registry
            // args), then apply route exactly once over real env + argv.
            let mut argv: Vec<String> = Vec::new();
            // Npx agents (including Codex) resolve via PATH / npm-global.
            argv.push(
                crate::commands::acp::resolve_npx_command(cmd)
                    .await
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| {
                        crate::process::normalized_program(cmd)
                            .to_string_lossy()
                            .to_string()
                    }),
            );
            // Grok's root-level launch flags go BEFORE its `agent stdio`
            // subcommand (which rejects them):
            //  - `--no-auto-update`: codeg owns the pinned version, so suppress the
            //    CLI's background self-update (it would drift off the pin and can
            //    break the ACP contract). Config twin: `[cli].auto_update = false`.
            //  - `--permission-mode <value>`: grok's real permission enum,
            //    read from `[ui].permission_mode`. Default/unset leaves it off so
            //    ACP permission requests still reach codeg's UI.
            // Route tokens (`--no-subagents`, CodeBuddy `--disallowedTools`) are
            // applied once by `apply_process_route` on this complete argv.
            let grok_permission_mode = (agent_type == AgentType::Grok)
                .then(crate::commands::acp::grok_launch_permission_mode)
                .flatten();
            append_npx_launch_args(&mut argv, agent_type, args, grok_permission_mode.as_deref());
            apply_process_route(plan, agent_type, &mut env_map, &mut argv)?;
            let mut parts: Vec<String> = env_map.iter().map(|(k, v)| format!("{k}={v}")).collect();
            parts.extend(argv);
            let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
            let agent_name = meta.name.to_string();
            AcpAgent::from_args(&refs)
                .map(|a| {
                    a.with_debug(move |line, dir| {
                        if dir == sacp_tokio::LineDirection::Stderr {
                            tracing::debug!("[ACP][{agent_name}][stderr] {line}");
                        }
                    })
                })
                .map_err(|e| AcpError::SpawnFailed(e.to_string()))
        }
        AgentDistribution::Bundled {
            cmd,
            args,
            env,
            override_env,
            platforms,
            ..
        } => {
            let platform = registry::current_platform();
            if !platforms.contains(&platform) {
                return Err(AcpError::PlatformNotSupported(format!(
                    "{} is not available on {platform}",
                    meta.name
                )));
            }
            let binary_path =
                crate::acp::bundled_agent::locate_bundled_executable(cmd, override_env)?
                    .ok_or_else(|| {
                        AcpError::SdkNotInstalled(format!(
                            "Bundled {} executable is missing; reinstall or update DrawCode.",
                            meta.name
                        ))
                    })?;
            let merged_env =
                apply_codex_cli_path_env(agent_type, merge_agent_env(env, runtime_env))?;
            let mut env_map: BTreeMap<String, String> = merged_env.into_iter().collect();
            // Bundled: complete argv first, then one route application (env + argv).
            let mut argv: Vec<String> = Vec::new();
            argv.push(binary_path.to_string_lossy().to_string());
            argv.extend(args.iter().map(|arg| (*arg).to_string()));
            apply_process_route(plan, agent_type, &mut env_map, &mut argv)?;
            let mut parts: Vec<String> = env_map
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect();
            parts.extend(argv);
            let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
            let agent_name = meta.name.to_string();
            tracing::info!(
                "[ACP][{}] Using bundled executable {}",
                meta.name,
                binary_path.display()
            );
            AcpAgent::from_args(&refs)
                .map(|agent| {
                    agent.with_debug(move |line, direction| {
                        if direction == sacp_tokio::LineDirection::Stderr {
                            tracing::debug!("[ACP][{agent_name}][stderr] {line}");
                        }
                    })
                })
                .map_err(|error| AcpError::SpawnFailed(error.to_string()))
        }
        AgentDistribution::Binary {
            version: registry_version,
            cmd,
            args,
            env,
            platforms,
            ..
        } => {
            let platform = registry::current_platform();
            let _ = platforms
                .iter()
                .find(|p| p.platform == platform)
                .ok_or_else(|| {
                    AcpError::PlatformNotSupported(format!(
                        "{} is not available on {platform}",
                        meta.name
                    ))
                })?;

            // Session-page connect must never trigger a download. Use
            // the best cached version available (tolerates users on
            // older-but-still-working binaries); return SdkNotInstalled
            // only when nothing is cached, so the frontend can prompt
            // the user to install it from the Agent Settings page.
            //
            // With nothing cached, every binary agent falls back to a
            // user-installed CLI on PATH (e.g. `cursor-agent` from the
            // official install script, a brew `opencode`, or the user's
            // own install of a custom tool) before giving up — mirroring
            // the Uvx `system_cmd` fallback.
            //
            // INVARIANT: the substring "is not installed" is matched
            // verbatim by the frontend catch block in
            // `src/contexts/acp-connections-context.tsx` to surface a
            // localized install prompt. Do not change the wording.
            let cached =
                crate::acp::binary_cache::find_best_cached_binary_for_agent(agent_type, cmd)?;
            let binary_path = match cached {
                Some((path, cached_version)) => {
                    if cached_version == registry_version {
                        tracing::info!("[ACP][{}] Using cached binary {cached_version}", meta.name);
                    } else {
                        tracing::info!(
                            "[ACP][{}] Using cached binary {cached_version} (registry recommends {registry_version})",
                            meta.name
                        );
                    }
                    path
                }
                None => {
                    let system = crate::commands::acp::resolve_system_agent_binary(cmd)
                        .ok_or_else(|| {
                            AcpError::SdkNotInstalled(format!(
                                "{} is not installed. Please install it in Agent Settings.",
                                meta.name
                            ))
                        })?;
                    tracing::info!(
                        "[ACP][{}] No cached binary; using system {} from PATH",
                        meta.name,
                        system.display()
                    );
                    system
                }
            };

            let binary_str = binary_path.to_string_lossy().to_string();
            let binary_size = std::fs::metadata(&binary_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let mut server = McpServerStdio::new(meta.name, &binary_str);
            let mut cmd_args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            // Cursor's ROOT-level `--model <id>` flag precedes the `acp`
            // subcommand and sets the session's default model. Sourced from
            // the Cursor panel's default-model control (env_json key
            // CURSOR_MODEL — a codeg-side launch knob; the CLI itself reads
            // no model env var).
            if agent_type == AgentType::Cursor {
                if let Some(model) = runtime_env
                    .get("CURSOR_MODEL")
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                {
                    cmd_args.insert(0, "--model".to_string());
                    cmd_args.insert(1, model.to_string());
                }
                // Root `--force` = Run Everything: the ACP session swaps its
                // permission prompter for an auto-allow one, so tool calls
                // never reach session/request_permission (deny rules still
                // apply, and an org policy can downgrade it to rule-based
                // approval). Sourced from the panel's permission-mode
                // control (env_json key CURSOR_FORCE — codeg-side knob; the
                // CLI reads no such env var).
                if runtime_env
                    .get("CURSOR_FORCE")
                    .map(|v| v.trim())
                    .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                {
                    cmd_args.insert(0, "--force".to_string());
                }
            }
            let cmd_args_for_log = cmd_args.clone();
            if !cmd_args.is_empty() {
                server = server.args(cmd_args);
            }
            let mut merged_env = merge_agent_env(env, runtime_env);
            if agent_type == AgentType::Cursor {
                apply_cursor_env_policy(&mut merged_env, runtime_env);
            } else if agent_type == AgentType::Grok {
                apply_grok_env_policy(&mut merged_env, runtime_env);
            }
            let env_key_list: Vec<&str> = merged_env.iter().map(|(k, _)| k.as_str()).collect();
            if !merged_env.is_empty() {
                let env_vars: Vec<sacp::schema::EnvVariable> = merged_env
                    .iter()
                    .map(|(k, v)| sacp::schema::EnvVariable::new(k, v))
                    .collect();
                server = server.env(env_vars);
            }
            // Spawn-time diagnostic dump: binary identity, args, and env
            // key list (values omitted — they may contain API keys). If
            // the connection hangs later, these lines pin down exactly
            // which binary was invoked and how.
            tracing::info!(
                "[ACP][{}] binary_path={} size={} platform={} args={:?} env_keys={:?}",
                meta.name,
                binary_str,
                binary_size,
                registry::current_platform(),
                cmd_args_for_log,
                env_key_list
            );

            // Stdio logging policy:
            // - stderr is always on: it's the agent's own diagnostic
            //   output (ANSI log lines) and does not contain user data.
            // - stdin / stdout carry JSON-RPC traffic that includes
            //   prompt text, tool-call arguments, file read/write
            //   contents, and permission-response payloads — all of
            //   which may contain API keys pasted by users or file
            //   contents the agent is editing. They are gated behind
            //   the `CODEG_ACP_DEBUG=1` env var so production builds
            //   don't persist user content into OS-level log files
            //   (Console.app on macOS, journald on Linux).
            // - Max line length is kept short so what does get logged
            //   captures the JSON-RPC envelope (method, id) rather
            //   than large payload bodies.
            let stdio_debug_enabled = std::env::var("CODEG_ACP_DEBUG")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let agent_name = meta.name.to_string();
            Ok(
                AcpAgent::new(sacp::schema::McpServer::Stdio(server)).with_debug(
                    move |line, dir| {
                        let (tag, enabled) = match dir {
                            sacp_tokio::LineDirection::Stderr => ("stderr", true),
                            sacp_tokio::LineDirection::Stdout => ("stdout", stdio_debug_enabled),
                            sacp_tokio::LineDirection::Stdin => ("stdin", stdio_debug_enabled),
                        };
                        if !enabled {
                            return;
                        }
                        const MAX: usize = 256;
                        if line.len() > MAX {
                            let head = line
                                .char_indices()
                                .take_while(|(i, _)| *i < MAX)
                                .last()
                                .map(|(i, c)| i + c.len_utf8())
                                .unwrap_or(MAX);
                            tracing::debug!(
                                "[ACP][{agent_name}][{tag}] {}... <truncated {} bytes>",
                                &line[..head],
                                line.len() - head
                            );
                        } else {
                            tracing::debug!("[ACP][{agent_name}][{tag}] {line}");
                        }
                    },
                ),
            )
        }
        AgentDistribution::Uvx {
            package,
            cmd,
            args,
            env,
            python,
            system_cmd,
            ..
        } => {
            let merged_env = merge_agent_env(env, runtime_env);
            let mut parts: Vec<String> = Vec::new();
            for (k, v) in &merged_env {
                parts.push(format!("{k}={v}"));
            }
            if let Some(uvx_path) = crate::commands::acp::resolve_uvx_command() {
                // Primary: `uvx [--python <ver>] --from <pinned package> <entry
                // script>`. uvx fetches + caches the pinned package on first use;
                // the `--python` pin keeps it on an interpreter the agent
                // supports (see the registry `python` field).
                parts.push(uvx_path.to_string_lossy().to_string());
                parts.extend(crate::commands::acp::uvx_python_args(python));
                parts.push("--from".into());
                parts.push(package.to_string());
                parts.push(cmd.to_string());
                for a in args {
                    parts.push((*a).into());
                }
            } else if let Some((sys_path, sys_args)) = system_cmd.and_then(|(c, a)| {
                crate::commands::acp::resolve_command_on_path(c).map(|path| (path, a))
            }) {
                // Fallback: the agent's own CLI is already on PATH (e.g.
                // `hermes acp`), installed via its official installer rather
                // than provisioned through uvx.
                tracing::warn!(
                    "[ACP][{}] uvx unavailable; falling back to system command {:?}",
                    meta.name,
                    sys_path
                );
                // `system_cmd` is a complete launch recipe for the PATH binary;
                // the uvx entry-script `args` don't necessarily apply to it
                // (for Hermes both are empty / `["acp"]`, so this is exact).
                parts.push(sys_path.to_string_lossy().to_string());
                for a in sys_args {
                    parts.push((*a).into());
                }
            } else {
                // INVARIANT: the substring "is not installed" is matched
                // verbatim by the frontend catch block in
                // `src/contexts/acp-connections-context.tsx` to surface a
                // localized install prompt. Do not change the wording.
                return Err(AcpError::SdkNotInstalled(format!(
                    "{} is not installed. Please install it in Agent Settings.",
                    meta.name
                )));
            }
            let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
            let agent_name = meta.name.to_string();
            AcpAgent::from_args(&refs)
                .map(|a| {
                    a.with_debug(move |line, dir| {
                        if dir == sacp_tokio::LineDirection::Stderr {
                            tracing::debug!("[ACP][{agent_name}][stderr] {line}");
                        }
                    })
                })
                .map_err(|e| AcpError::SpawnFailed(e.to_string()))
        }
    }?;

    // Run the agent subprocess in the session's working directory rather than
    // codeg's own process cwd (a desktop app launched from the Dock often
    // inherits "/"). A coding agent belongs in its project root. This is
    // required for Hermes, whose local terminal backend force-exports
    // TERMINAL_CWD = os.getcwd() at import (clobbering any inherited value)
    // and reports that as the agent's "Current working directory" in its
    // system prompt — without pinning it would believe it lives in "/". For
    // agents that already use the ACP session/new cwd this is a harmless
    // alignment (process cwd == session cwd). Guard on an existing directory
    // so a not-yet-created working_dir (e.g. a worktree path) can't make the
    // spawn fail.
    Ok(if cwd.is_dir() {
        agent.with_current_dir(cwd)
    } else {
        agent
    })
}

/// Resolve ownership stamps for a connection about to become visible.
///
/// When `parent_connection_id` is set and the parent is still in the map,
/// adopt the parent's live `(label, operation_id, generation)` — this is the
/// registration-time fence against concurrent owner rebind. Otherwise use the
/// caller's launch stamps with generation `0` (roots / cold leases).
pub(crate) fn resolve_spawn_ownership_under_lock(
    connections: &HashMap<String, AgentConnection>,
    parent_connection_id: Option<&str>,
    fallback_label: String,
    fallback_operation_id: Option<String>,
) -> (String, Option<String>, u64) {
    if let Some(pid) = parent_connection_id {
        if let Some(parent) = connections.get(pid) {
            return (
                parent.owner_window_label.clone(),
                parent.owner_operation_id.clone(),
                parent.ownership_generation,
            );
        }
    }
    (fallback_label, fallback_operation_id, 0)
}

/// Stack size for the dedicated OS thread that drives each ACP connection's
/// `run_connection` future (see the spawn site below). `run_connection` is one
/// colossal async state machine — the full per-connection message loop plus
/// every registered client-request handler — and in DEBUG builds the compiler
/// does not pack async locals, so a single poll of the connection closure needs
/// far more than a default ~2 MiB Tokio worker-thread stack. Left on the worker
/// pool it overflowed the stack and aborted the whole process under `tauri dev`
/// (release builds pack the frame small enough to fit, which is why only debug
/// crashed). 8 MiB matches the macOS main-thread stack — 4x the default that
/// overflowed, generous headroom for the debug frame as ACP features accrete.
/// The stack is reserved address space, lazily committed, so it costs no
/// physical memory beyond the pages actually touched (~the real 2-3 MiB the
/// frame uses), regardless of this cap — so the larger reservation is free. If a
/// future ACP feature ever grows the loop past even this, split `run_connection`
/// into boxed sub-futures rather than raising it further.
const ACP_CONNECTION_STACK_SIZE: usize = 8 * 1024 * 1024;

/// Spawn an ACP agent process and run the connection loop in a background task.
///
/// On success, the newly created `AgentConnection` is inserted into
/// `connections` before this function returns. The background task
/// automatically removes the entry from `connections` once `run_connection`
/// exits (timeout, error, or clean disconnect), so the manager never
/// leaks stale entries after a connection tears down.
#[allow(clippy::too_many_arguments)]
pub async fn spawn_agent_connection(
    connection_id: String,
    agent_type: AgentType,
    working_dir: Option<String>,
    session_id: Option<String>,
    runtime_env: BTreeMap<String, String>,
    terminal_shell: crate::terminal::shell::ResolvedShellSnapshot,
    route_plan: crate::acp::delegation::route::DelegationRoutePlan,
    origin: crate::acp::delegation::route::DelegationConnectionOrigin,
    route_preference: Option<crate::acp::delegation::route::DelegationRoutePolicy>,
    route_capability: crate::acp::delegation::route::RouteCapabilitySnapshot,
    owner_window_label: String,
    owner_operation_id: Option<String>,
    // When set (delegated child spawn), ownership is re-read from this parent
    // under the connections lock at insert so concurrent rebind cannot leave
    // the child on a stale pre-rebind incarnation.
    parent_connection_id: Option<String>,
    emitter: EventEmitter,
    connections: Arc<tokio::sync::Mutex<HashMap<String, AgentConnection>>>,
    preferred_mode_id: Option<String>,
    preferred_config_values: BTreeMap<String, String>,
    delegation_injection: Option<DelegationInjection>,
    workflow_child_mcp_binding: Option<crate::acp::delegation::workflow::WorkflowChildMcpBinding>,
    launch_context: ConnectionLaunchContext,
    // Continue-delegation path: resume/load only, never session/new, with
    // external-id verify before SessionStarted identity rewrite.
    session_attach_mode: crate::acp::session_attach::SessionAttachMode,
    tool_lease_registry: Arc<crate::acp::tool_watchdog::ToolExecutionLeaseRegistry>,
    mcp_cancel_registry: Arc<crate::acp::tool_watchdog::McpCancelRegistry>,
) -> Result<SpawnHandshake, AcpError> {
    // Create the authoritative session state up front. Subsequent emit_with_state
    // calls write through this state and increment its seq counter so the first
    // event the frontend sees has seq=1, not the placeholder 0 from Phase 0.
    let connection_incarnation = uuid::Uuid::new_v4().to_string();
    let mut initial_state = SessionState::new(
        connection_id.clone(),
        agent_type,
        working_dir.clone().map(PathBuf::from),
        owner_window_label.clone(),
        None, // folder_id 由后续 prompt handler 在首次 send 时绑定 (Phase 2)
    );
    initial_state.connection_incarnation = connection_incarnation.clone();
    initial_state.tool_lease_registry = tool_lease_registry.clone();
    initial_state.mcp_cancel_registry = mcp_cancel_registry;
    // Real plan-derived snapshot for every new SessionState (not the serde legacy default).
    initial_state.set_route_plan_snapshot(&route_plan);
    // Soft-supervisor wake (noop when injection lacks a handle).
    if let Some(inj) = delegation_injection.as_ref() {
        initial_state.supervisor_wake = inj.supervisor_wake.clone();
    }
    // Purpose + inherited/effective locale from launch. Task 4B may pass
    // temporary English defaults; Task 4C wires real sources.
    initial_state.purpose = launch_context.purpose;
    initial_state.effective_locale = launch_context.inherited_locale.unwrap_or(AppLocale::En);

    // Install the SessionStarted dedup signal BEFORE wrapping into Arc so the
    // first event (StatusChanged{Connecting} below) doesn't race with the
    // installer. The receiver is returned to `spawn_agent`, which holds the
    // per-session dedup lock until this rx fires (or times out / aborts).
    let session_started_rx = initial_state.install_session_started_signal();
    let (route_bootstrap_tx, route_bootstrap_rx) =
        tokio::sync::oneshot::channel::<RouteBootstrapOutcome>();

    let session_state = Arc::new(RwLock::new(initial_state));

    emit_with_state(
        &session_state,
        &emitter,
        AcpEvent::StatusChanged {
            status: ConnectionStatus::Connecting,
        },
    )
    .await;

    // Align ~/.hermes/.env's base-URL var with config.yaml's model.base_url so
    // Hermes' auxiliary tasks (title generation, compression, …) resolve the
    // same endpoint as the main conversation. Best-effort; never blocks launch.
    if agent_type == AgentType::Hermes {
        crate::commands::acp::reconcile_hermes_runtime_env(&runtime_env);
    }

    // Resolve the launch cwd from the same `working_dir` (via the same helper)
    // that run_connection uses for the session/new request, so the process
    // cwd, the ACP session cwd, and any os.getcwd()-derived agent state all
    // agree. Computed here because `working_dir` is moved into run_connection
    // below.
    let launch_cwd = resolve_working_dir(working_dir.as_deref());
    // Shared cell that receives the agent process's OS pid the instant it
    // spawns (via `on_spawn` below). Stored on the `AgentConnection` so the
    // shutdown path can `kill_tree` the process tree synchronously as a
    // backstop when the connection driver thread is torn down by process exit
    // before `ChildGuard::drop` can run. 0 = not spawned yet / unknown.
    let child_pid = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let agent = build_agent(agent_type, &runtime_env, &launch_cwd, &route_plan)
        .await?
        .on_spawn({
            let child_pid = Arc::clone(&child_pid);
            move |pid| child_pid.store(pid, std::sync::atomic::Ordering::SeqCst)
        })
        // Paired with `on_spawn`: publish 0 again once the process has been
        // reaped, so the shutdown backstop can never `kill_tree` a pid the OS
        // has already handed to someone else. Fires ONLY on a real reap — a
        // connection that merely ended keeps its pid published, because the
        // vendored `ChildGuard` signals the tree without waiting and the agent
        // may still be running.
        .on_exit({
            let child_pid = Arc::clone(&child_pid);
            move || child_pid.store(0, std::sync::atomic::Ordering::SeqCst)
        });

    // Path policy for the ACP `fs/*` channel. Built HERE rather than inside
    // `run_connection` because it needs the full `runtime_env` (only the git
    // credential keys survive into `terminal_base_env` below), and a per-agent
    // relocation like `GROK_HOME` must move the allowed root along with the
    // agent's state. Uses the same `launch_cwd` the process and ACP session get.
    let fs_policy = FsAccessPolicy::from_env(&launch_cwd, agent_type, &runtime_env);

    // Forward only the codeg git credential helper keys into the terminal
    // runtime — not the agent's API tokens or model provider credentials.
    // This makes `git fetch`/`git push` issued through the ACP
    // `terminal/create` tool authenticate via the same helper path the
    // agent process uses, while keeping unrelated secrets scoped to the
    // agent and out of arbitrary shell commands it runs.
    //
    // `runtime_env` may already carry SHELL / CODEG_TERMINAL_* declarations
    // and API keys; `build_terminal_base_env` keeps only GIT_CONFIG_* here.
    let mut terminal_base_env = crate::acp::terminal_context::build_terminal_base_env(&runtime_env);
    // Also surface a codeg-installed OfficeCLI on the terminal's PATH: agents run
    // office skills' `officecli …` through this `terminal/create` tool, not as a
    // child of the agent process, so the agent-env injection alone wouldn't reach
    // them right after install (before install.ps1's User-PATH change lands).
    prepend_officecli_path(&mut terminal_base_env);

    let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel::<ConnectionCommand>(32);
    let (control_tx, control_rx, control_liveness_rx) = connection_channel::<ConnectionControl>(32);
    let conn_id = connection_id.clone();
    let emitter_clone = emitter.clone();
    let cleanup_connections = connections.clone();
    let cleanup_connection_id = connection_id.clone();
    let state_clone = Arc::clone(&session_state);

    // Component fingerprints of what this process is launching with.
    // Agent config is derived from the same `runtime_env` we hand the agent
    // (minus per-launch volatile keys) plus native config file content; shell
    // is the selection key from the immutable launch snapshot. Later settings
    // saves compare against these independently.
    let (spawn_config, observed_config) = matching_config_pair(
        crate::commands::acp::fingerprint_config(agent_type, &runtime_env),
        terminal_shell.selection_key.clone(),
        route_plan.fingerprint.clone(),
    );

    // Insert the entry BEFORE spawning the background task so that a
    // fast-failing `run_connection` can never remove it before it was
    // inserted (would otherwise leak the entry).
    //
    // Child fence: under the same lock as insert, re-read parent ownership so a
    // concurrent `rebind_connection_owner_window` cannot leave this child on a
    // stale pre-rebind (label, generation, operation_id) snapshot. Roots keep
    // the caller's launch stamps (generation stays 0 until rebind).
    {
        let mut map = connections.lock().await;
        let (label, op, gen) = resolve_spawn_ownership_under_lock(
            &map,
            parent_connection_id.as_deref(),
            owner_window_label,
            owner_operation_id,
        );
        {
            let mut st = session_state.write().await;
            st.owner_window_label = label.clone();
        }
        map.insert(
            connection_id.clone(),
            AgentConnection {
                id: connection_id.clone(),
                agent_type,
                status: ConnectionStatus::Connecting,
                owner_window_label: label,
                owner_operation_id: op,
                ownership_generation: gen,
                connection_incarnation: connection_incarnation.clone(),
                tool_lease_registry: tool_lease_registry.clone(),
                parent_connection_id,
                cmd_tx,
                control_tx,
                task_abort: None,
                state: Arc::clone(&session_state),
                emitter: emitter.clone(),
                prompt_lock: Arc::new(tokio::sync::Mutex::new(())),
                spawn_config,
                observed_config,
                terminal_shell: terminal_shell.clone(),
                route_plan: route_plan.clone(),
                origin,
                route_preference,
                route_capability,
                child_pid,
            },
        );
    }

    // Drive `run_connection` on a dedicated, large-stack thread (see
    // ACP_CONNECTION_STACK_SIZE) rather than a Tokio worker task: its debug
    // poll frame is too big for a default ~2 MiB worker stack and was aborting
    // the process under `tauri dev`. `Handle::block_on` runs it on the SAME
    // shared runtime, so `tokio::spawn`/timers/IO inside `run_connection` still
    // use the pool — only the giant top-level frame moves to the roomy stack.
    // The connection is fire-and-forget (torn down from within via `cmd_rx` /
    // process exit; no JoinHandle is awaited), so a thread is behaviorally
    // equivalent to the previous task.
    let connection_rt = tokio::runtime::Handle::current();
    // RAII guard built OUTSIDE the thread body and moved in: on a normal exit
    // or panic unwind its Drop removes the manager map entry, AND if the thread
    // fails to spawn the dropped closure runs the same Drop — so the entry is
    // never leaked.
    let cleanup_guard = ConnectionCleanupGuard {
        connections: cleanup_connections,
        connection_id: cleanup_connection_id,
        connection_incarnation: connection_incarnation.clone(),
        tool_lease_registry,
    };

    // Keep the manager's existing Tokio AbortHandle contract while the large
    // connection future runs on a dedicated OS thread. Aborting this tiny task
    // drops the sender, which wakes the thread and drops the driver future.
    let (driver_abort_tx, driver_abort_rx) = tokio::sync::oneshot::channel::<()>();
    let abort_signal_task = tokio::spawn(async move {
        let _driver_abort_tx = driver_abort_tx;
        std::future::pending::<()>().await;
    });
    let driver_abort_handle = abort_signal_task.abort_handle();
    let spawn_failure_abort = driver_abort_handle.clone();

    let connection_thread = std::thread::Builder::new()
        .name(format!("acp-conn-{conn_id}"))
        .stack_size(ACP_CONNECTION_STACK_SIZE)
        .spawn(move || {
            let _cleanup = cleanup_guard;
            let driver = async move {
                let delegation_for_cleanup = delegation_injection.clone();
                // run_connection reports bootstrap via oneshot; map AcpError paths to
                // typed outcomes for the manager's single-attempt fallback policy.
                let result = run_connection(
                    agent,
                    conn_id.clone(),
                    agent_type,
                    working_dir,
                    session_id,
                    cmd_rx,
                    control_rx,
                    cmd_liveness_rx,
                    control_liveness_rx,
                    emitter_clone.clone(),
                    Arc::clone(&state_clone),
                    terminal_base_env,
                    terminal_shell,
                    preferred_mode_id,
                    preferred_config_values,
                    delegation_injection,
                    workflow_child_mcp_binding,
                    connection_incarnation,
                    route_plan,
                    route_bootstrap_tx,
                    session_attach_mode,
                    fs_policy,
                )
                .await;

                // Revoke the per-launch token + ready lease + cascade cancel any
                // still-pending delegations AND questions owned by this parent.
                // All are best-effort: a missing token entry is a no-op.
                if let Some(inj) = delegation_for_cleanup {
                    cleanup_delegation_parent(&inj, &conn_id, &state_clone).await;
                }

                if let Err(e) = result {
                    let code = e.code().map(String::from);
                    emit_with_state(
                        &state_clone,
                        &emitter_clone,
                        AcpEvent::Error {
                            message: e.to_string(),
                            agent_type: agent_type.to_string(),
                            code,
                            // The only genuinely terminal emit site: `run_connection`
                            // is unwinding and the next event is `Disconnected`.
                            // The lifecycle worker uses this flag to decide whether
                            // to flip the conversation row to Cancelled and to
                            // buffer the detail for the broker's cancel reason.
                            terminal: true,
                        },
                    )
                    .await;
                    // Drive the state machine through `Error` before `Disconnected`
                    // so the frontend's error-handling effect (cancelled-on-error)
                    // engages — without this hop the connection would jump straight
                    // to Disconnected and look like a clean shutdown.
                    emit_with_state(
                        &state_clone,
                        &emitter_clone,
                        AcpEvent::StatusChanged {
                            status: ConnectionStatus::Error,
                        },
                    )
                    .await;
                }

                emit_with_state(
                    &state_clone,
                    &emitter_clone,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Disconnected,
                    },
                )
                .await;
            };
            connection_rt.block_on(async move {
                tokio::select! {
                    _ = driver => {}
                    _ = driver_abort_rx => {}
                }
                abort_signal_task.abort();
            });
        });
    if let Err(e) = connection_thread {
        spawn_failure_abort.abort();
        // Thread creation only fails on OS resource exhaustion. Dropping the
        // un-spawned closure already ran `cleanup_guard`'s Drop (removing the
        // map entry), so just surface the failure and let the caller abort.
        tracing::error!("[ACP] failed to spawn connection driver thread: {e}");
        return Err(AcpError::SpawnFailed(format!(
            "connection driver thread: {e}"
        )));
    }

    // Install abort handle so unexposed teardown can terminate before the
    // conversation loop (Disconnect would never drain there).
    {
        let mut map = connections.lock().await;
        if let Some(conn) = map.get_mut(&connection_id) {
            conn.task_abort = Some(driver_abort_handle);
        }
    }

    Ok(SpawnHandshake {
        session_started_rx,
        route_bootstrap_rx,
    })
}

/// A pending permission-card responder. `Acp` is a real ACP
/// `session/request_permission`; `CodexElicitation` is a codex approval-style
/// `elicitation/create` (MCP tool-call approval / message-only confirm) routed
/// through the SAME permission card so approvals look exactly like they did
/// before codeg advertised `elicitation.form` — its chosen option answers the
/// blocked elicitation request instead (see `handle_elicitation_request`).
enum PendingPermission {
    Acp(Responder<RequestPermissionResponse>),
    CodexElicitation {
        responder: Responder<serde_json::Value>,
        approval: crate::acp::question::ElicitationApproval,
    },
}

impl PendingPermission {
    /// Resolve with the user's chosen option id.
    fn respond_selected(self, option_id: String) {
        match self {
            PendingPermission::Acp(responder) => {
                let outcome =
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id));
                let _ = responder.respond(RequestPermissionResponse::new(outcome));
            }
            PendingPermission::CodexElicitation {
                responder,
                approval,
            } => {
                let response = crate::acp::question::build_elicitation_approval_response(
                    &approval, &option_id,
                );
                let _ = responder.respond(serde_json::to_value(response).unwrap_or_default());
            }
        }
    }

    /// Resolve as cancelled — the turn ended / connection tore down before the
    /// user chose.
    fn respond_cancelled(self) {
        match self {
            PendingPermission::Acp(responder) => {
                let _ = responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
            PendingPermission::CodexElicitation { responder, .. } => {
                let _ = responder.respond(
                    serde_json::to_value(crate::acp::question::elicitation_cancel_response())
                        .unwrap_or_default(),
                );
            }
        }
    }
}

/// Shared state for pending permission responders.
type PendingPermissions = Arc<tokio::sync::Mutex<HashMap<String, PendingPermission>>>;

fn map_session_modes(mode_state: &SessionModeState) -> SessionModeStateInfo {
    SessionModeStateInfo {
        current_mode_id: mode_state.current_mode_id.to_string(),
        available_modes: mode_state
            .available_modes
            .iter()
            .map(|mode| SessionModeInfo {
                id: mode.id.to_string(),
                name: mode.name.clone(),
                description: mode.description.clone(),
            })
            .collect(),
    }
}

async fn emit_session_modes(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    modes: &Option<SessionModeState>,
) {
    if let Some(mode_state) = modes {
        emit_with_state(
            state,
            emitter,
            AcpEvent::SessionModes {
                modes: map_session_modes(mode_state),
            },
        )
        .await;
    }
}

fn map_session_config_category(category: &SessionConfigOptionCategory) -> String {
    match category {
        SessionConfigOptionCategory::Mode => "mode".to_string(),
        SessionConfigOptionCategory::Model => "model".to_string(),
        SessionConfigOptionCategory::ThoughtLevel => "thought_level".to_string(),
        SessionConfigOptionCategory::Other(value) => value.clone(),
        _ => "unknown".to_string(),
    }
}

fn map_session_config_select_option(
    option: &SessionConfigSelectOption,
) -> SessionConfigSelectOptionInfo {
    SessionConfigSelectOptionInfo {
        value: option.value.to_string(),
        name: option.name.clone(),
        description: option.description.clone(),
    }
}

fn map_session_config_select_group(
    group: &SessionConfigSelectGroup,
) -> SessionConfigSelectGroupInfo {
    SessionConfigSelectGroupInfo {
        group: group.group.to_string(),
        name: group.name.clone(),
        options: group
            .options
            .iter()
            .map(map_session_config_select_option)
            .collect(),
    }
}

fn map_session_config_option(option: &SessionConfigOption) -> Option<SessionConfigOptionInfo> {
    match &option.kind {
        SessionConfigKind::Select(select) => {
            let (flat_options, groups) = match &select.options {
                SessionConfigSelectOptions::Ungrouped(options) => (
                    options
                        .iter()
                        .map(map_session_config_select_option)
                        .collect::<Vec<_>>(),
                    Vec::new(),
                ),
                SessionConfigSelectOptions::Grouped(grouped) => (
                    grouped
                        .iter()
                        .flat_map(|group| {
                            group.options.iter().map(map_session_config_select_option)
                        })
                        .collect::<Vec<_>>(),
                    grouped
                        .iter()
                        .map(map_session_config_select_group)
                        .collect::<Vec<_>>(),
                ),
                _ => (Vec::new(), Vec::new()),
            };

            Some(SessionConfigOptionInfo {
                id: option.id.to_string(),
                name: option.name.clone(),
                description: option.description.clone(),
                category: option.category.as_ref().map(map_session_config_category),
                kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                    current_value: select.current_value.to_string(),
                    options: flat_options,
                    groups,
                }),
            })
        }
        _ => None,
    }
}

fn map_session_config_options(
    config_options: &[SessionConfigOption],
) -> Vec<SessionConfigOptionInfo> {
    config_options
        .iter()
        .filter_map(map_session_config_option)
        .collect()
}

/// Defensive fallback for Codex's approval-preset selector.
///
/// codex-acp 1.0.0 advertises its modes through *both* standard ACP
/// `SessionModes` and an `id = "mode"` config option (see `AgentMode.ts`'s
/// `toSessionModeState()` + `toConfigOption()`), so this synthesizer is
/// normally a no-op — the early return fires because the agent already
/// surfaced "mode". We keep it only as a safety net: if a future build ever
/// omits the "mode" config option (older 0.16.0 did this when the sandbox
/// policy didn't match a preset, e.g. after `writable_roots` injection), the
/// user would otherwise lose the preset picker entirely, because the composer
/// hides the standard mode selector whenever any config option exists. Codex's
/// `set_config_option` handler accepts `config_id = "mode"` regardless of
/// whether it was advertised.
///
/// The preset ids/names/descriptions below MUST match the live adapter
/// vocabulary (`read-only` / `agent` / `agent-full-access`, default `agent`);
/// the legacy 0.16.0 ids (`auto` / `full-access`) are no longer accepted.
fn ensure_codex_mode_option(options: &mut Vec<SessionConfigOptionInfo>) {
    if options.iter().any(|o| o.id == "mode") {
        return;
    }
    options.insert(
        0,
        SessionConfigOptionInfo {
            id: "mode".to_string(),
            name: "Approval Preset".to_string(),
            description: Some(
                "Choose an approval and sandboxing preset for your session".to_string(),
            ),
            category: Some("mode".to_string()),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: "agent".to_string(),
                options: vec![
                    SessionConfigSelectOptionInfo {
                        value: "read-only".to_string(),
                        name: "Read-only".to_string(),
                        description: Some(
                            "Requires approval to edit files and run commands.".to_string(),
                        ),
                    },
                    SessionConfigSelectOptionInfo {
                        value: "agent".to_string(),
                        name: "Agent".to_string(),
                        description: Some("Read and edit files, and run commands.".to_string()),
                    },
                    SessionConfigSelectOptionInfo {
                        value: "agent-full-access".to_string(),
                        name: "Agent (full access)".to_string(),
                        description: Some(
                            "Codex can edit files outside this workspace and run commands with \
                             network access."
                                .to_string(),
                        ),
                    },
                ],
                groups: vec![],
            }),
        },
    );
}

async fn emit_session_config_options_values(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    config_options: Vec<SessionConfigOption>,
) {
    let mut mapped = map_session_config_options(&config_options);
    if agent_type == AgentType::Codex {
        ensure_codex_mode_option(&mut mapped);
    }
    emit_with_state(
        state,
        emitter,
        AcpEvent::SessionConfigOptions {
            config_options: mapped,
        },
    )
    .await;
}

async fn emit_selectors_ready(state: &Arc<RwLock<SessionState>>, emitter: &EventEmitter) {
    emit_with_state(state, emitter, AcpEvent::SelectorsReady).await;
}

/// Synthesized config-option id for Grok's model picker (drives the composer's
/// grouped model selector via the frontend's `isModelConfigOption`).
const GROK_MODEL_OPTION_ID: &str = "model";

/// Synthesized config-option id for Grok's per-session reasoning-effort selector.
/// Grok ships effort choices in `x.ai/sessionConfig` under `category:"mode"`
/// (ids `low`/`medium`/`high`), and applies a live override via the
/// `session/set_model` request's `_meta.reasoningEffort` — so effort is a live
/// composer control, not just a global config.toml default.
const GROK_EFFORT_OPTION_ID: &str = "reasoning_effort";

/// Stable `AcpEvent::Error` code the frontend localizes when a Grok model switch
/// is rejected because the conversation is already bound to a different agent
/// type (see `is_grok_incompatible_agent_switch`). Recoverable, not terminal.
const GROK_INCOMPATIBLE_AGENT_ERROR_CODE: &str = "grok_model_switch_incompatible_agent";

/// Grok partitions its models by `agentType` (e.g. `grok-4.5` → `grok-build-plan`,
/// `grok-composer-2.5-fast` → `cursor`). A session may switch models freely until
/// its first turn, after which it is locked to the agent type it started with;
/// a later cross-agent-type `session/set_model` is then rejected with a stable
/// `data.code` of `MODEL_SWITCH_INCOMPATIBLE_AGENT` (`suggestion: start_new_session`).
/// Grok's own `x.ai/sessionConfig` still lists every model regardless of type, so
/// the composer offers them all and we detect this specific rejection to handle
/// it gracefully rather than leaking a raw JSON-RPC error.
fn is_grok_incompatible_agent_switch(e: &sacp::Error) -> bool {
    e.data
        .as_ref()
        .and_then(|d| d.get("code"))
        .and_then(|c| c.as_str())
        == Some("MODEL_SWITCH_INCOMPATIBLE_AGENT")
}

/// Canonical, composer-facing label for a Grok reasoning-effort tier id. Aligns
/// the composer with the settings panel's `grok.effort*` wording
/// (Low/Medium/High/Max); unknown ids fall back to the id itself. Grok's own
/// richer per-tier text (e.g. "Highest implementation quality…") is kept as the
/// option *description*, not the name.
fn grok_effort_label(id: &str) -> &str {
    match id {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Max",
        other => other,
    }
}

/// Canonical composer-facing *description* (sub-text) for a Grok reasoning-effort
/// tier. Grok ships its own per-tier `description` only for the models switchable
/// `reasoningEfforts`; the model default that lives OUTSIDE that list — grok-4.5's
/// `xhigh`/Max — carries none, so the front-injected option would otherwise be the
/// only tier with no sub-text. This supplies a fitting one (and doubles as a
/// fallback if grok ever omits a switchable tier's description). Unknown ids get
/// `None`. Grok's own, more specific text always takes precedence over this.
fn grok_effort_description(id: &str) -> Option<&'static str> {
    match id {
        "low" => Some("Quick, fast responses"),
        "medium" => Some("Balanced speed and quality"),
        "high" => Some("Extensive reasoning for high quality"),
        "xhigh" => Some("Maximum reasoning for the most complex tasks"),
        _ => None,
    }
}

/// Parse Grok's raw top-level `models` (from a session-establishment response)
/// into a per-`modelId` reasoning-effort spec map. Absent `models` /
/// `availableModels` → empty map (caller falls back to the flat
/// `x.ai/sessionConfig` effort list). Missing `_meta` fields degrade gracefully
/// (`supports=false` / `default=None` / `options=[]`).
fn parse_grok_effort_specs(models: Option<&serde_json::Value>) -> HashMap<String, GrokEffortSpec> {
    let mut out = HashMap::new();
    let Some(list) = models
        .and_then(|m| m.get("availableModels"))
        .and_then(|v| v.as_array())
    else {
        return out;
    };
    for m in list {
        let Some(model_id) = m.get("modelId").and_then(|v| v.as_str()) else {
            continue;
        };
        let meta = m.get("_meta");
        let supports = meta
            .and_then(|x| x.get("supportsReasoningEffort"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let default = meta
            .and_then(|x| x.get("reasoningEffort"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut options = Vec::new();
        if let Some(efforts) = meta
            .and_then(|x| x.get("reasoningEfforts"))
            .and_then(|v| v.as_array())
        {
            for e in efforts {
                let Some(id) = e.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let label = e
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or(id)
                    .to_string();
                let description = e
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                options.push((id.to_string(), label, description));
            }
        }
        out.insert(
            model_id.to_string(),
            GrokEffortSpec {
                options,
                default,
                supports,
            },
        );
    }
    out
}

/// Build the reasoning-effort selector for `model_id` from the per-model spec
/// map, or `None` if the model is absent from the map or does not support
/// effort. Options are the model's switchable `reasoningEfforts` (relabeled via
/// [`grok_effort_label`], keeping grok's own copy as the description); the model
/// default is injected at the FRONT when it isn't already listed, so a default
/// that lives OUTSIDE the switchable set — grok-4.5's `xhigh` — stays selectable
/// and the current value is always representable. `current_value` = the model
/// default (or the first option).
fn build_grok_effort_option(
    model_id: &str,
    specs: &HashMap<String, GrokEffortSpec>,
) -> Option<SessionConfigOptionInfo> {
    let spec = specs.get(model_id)?;
    if !spec.supports {
        return None;
    }
    let mut options: Vec<SessionConfigSelectOptionInfo> = spec
        .options
        .iter()
        .map(|(id, _grok_label, desc)| SessionConfigSelectOptionInfo {
            value: id.clone(),
            name: grok_effort_label(id).to_string(),
            // Grok's own per-tier text wins; canonical fallback fills any gap.
            description: desc
                .clone()
                .or_else(|| grok_effort_description(id).map(str::to_string)),
        })
        .collect();
    if let Some(def) = &spec.default {
        if !options.iter().any(|o| &o.value == def) {
            options.insert(
                0,
                SessionConfigSelectOptionInfo {
                    value: def.clone(),
                    name: grok_effort_label(def).to_string(),
                    // The injected default (grok-4.5's `xhigh`) is absent from grok's
                    // switchable list, so it has no grok description — supply ours.
                    description: grok_effort_description(def).map(str::to_string),
                },
            );
        }
    }
    if options.is_empty() {
        return None;
    }
    let current_value = spec
        .default
        .clone()
        .unwrap_or_else(|| options[0].value.clone());
    Some(SessionConfigOptionInfo {
        id: GROK_EFFORT_OPTION_ID.to_string(),
        name: "Reasoning effort".to_string(),
        description: None,
        category: Some("mode".to_string()),
        kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
            current_value,
            options,
            groups: Vec::new(),
        }),
    })
}

/// Re-point the effort selector in `opts` at `model_id`: drop any existing
/// effort selector, then append a freshly-built one iff the model supports
/// effort. The model selector is untouched and effort stays LAST (matching
/// `synthesize_grok_config_options`' ordering). Used on a mid-session model
/// switch, where grok never re-sends per-model effort data.
fn set_grok_effort_selector_for_model(
    opts: &mut Vec<SessionConfigOptionInfo>,
    model_id: &str,
    specs: &HashMap<String, GrokEffortSpec>,
) {
    opts.retain(|o| o.id != GROK_EFFORT_OPTION_ID);
    if let Some(effort) = build_grok_effort_option(model_id, specs) {
        opts.push(effort);
    }
}

/// Grok does not emit the standard ACP `config_options` / `modes` channels that
/// codeg's generic composer-selector pipeline reads (which is why the composer
/// showed no selectors for Grok). Instead it ships its selectors in a
/// non-standard `_meta["x.ai/sessionConfig"].options` list — a flat array of
/// `{id, category, label, description?, selected}` covering both model choices
/// (`category:"model"`) and reasoning-effort choices (`category:"mode"`). Fold
/// that list into the same `SessionConfigOptionInfo` shape every other agent's
/// selectors flow through, so Grok reaches selector parity with zero new
/// frontend code. Returns `None` when there is no usable sessionConfig.
fn synthesize_grok_config_options(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    specs: &HashMap<String, GrokEffortSpec>,
) -> Option<Vec<SessionConfigOptionInfo>> {
    let options = meta?
        .get("x.ai/sessionConfig")?
        .get("options")?
        .as_array()?;

    let mut model_opts: Vec<SessionConfigSelectOptionInfo> = Vec::new();
    let mut model_current: Option<String> = None;
    let mut effort_opts: Vec<SessionConfigSelectOptionInfo> = Vec::new();
    let mut effort_current: Option<String> = None;

    for opt in options {
        let Some(id) = opt.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        // Grok ships two composer selectors here: the MODEL list
        // (`category:"model"`) and the reasoning-EFFORT list (`category:"mode"`,
        // ids low/medium/high). Both are live over ACP — model via
        // `session/set_model`, effort via that request's `_meta.reasoningEffort`
        // (see `set_grok_model` / `set_grok_config_option`). Effort options only
        // appear when the current model advertises `supportsReasoningEffort`, so
        // the selector self-gates. Anything else is ignored.
        let (opts_vec, current) = match opt.get("category").and_then(|v| v.as_str()) {
            Some("model") => (&mut model_opts, &mut model_current),
            Some("mode") => (&mut effort_opts, &mut effort_current),
            _ => continue,
        };
        let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or(id);
        if opt
            .get("selected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            *current = Some(id.to_string());
        }
        opts_vec.push(SessionConfigSelectOptionInfo {
            value: id.to_string(),
            name: label.to_string(),
            description: opt
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        });
    }

    let mut result: Vec<SessionConfigOptionInfo> = Vec::new();
    // Current model id (the `selected` one, else the first) — needed both for
    // the model selector's `current_value` and to pick the per-model effort spec.
    let current_model = model_current
        .clone()
        .or_else(|| model_opts.first().map(|o| o.value.clone()));
    if !model_opts.is_empty() {
        let current = current_model
            .clone()
            .unwrap_or_else(|| model_opts[0].value.clone());
        result.push(SessionConfigOptionInfo {
            id: GROK_MODEL_OPTION_ID.to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: current,
                options: model_opts,
                groups: Vec::new(),
            }),
        });
    }
    // Effort selector. With per-model `specs` (parsed from the response's
    // top-level `models`), it follows the CURRENT model's advertised capability
    // — present/absent, its option set, and an `xhigh`-style out-of-list default
    // (see `build_grok_effort_option`). Without specs (no `models` in the
    // response) fall back to today's flat `x.ai/sessionConfig` "mode" list so
    // nothing regresses.
    if !specs.is_empty() {
        if let Some(effort) = current_model
            .as_deref()
            .and_then(|m| build_grok_effort_option(m, specs))
        {
            result.push(effort);
        }
    } else if !effort_opts.is_empty() {
        let current = effort_current.unwrap_or_else(|| effort_opts[0].value.clone());
        result.push(SessionConfigOptionInfo {
            id: GROK_EFFORT_OPTION_ID.to_string(),
            name: "Reasoning effort".to_string(),
            description: None,
            category: Some("mode".to_string()),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: current,
                options: effort_opts,
                groups: Vec::new(),
            }),
        });
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Emit an already-mapped `SessionConfigOptionInfo` list (used by the Grok path,
/// which synthesizes `Info` directly rather than mapping sacp `SessionConfigOption`s).
async fn emit_session_config_options_info(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    config_options: Vec<SessionConfigOptionInfo>,
) {
    emit_with_state(
        state,
        emitter,
        AcpEvent::SessionConfigOptions { config_options },
    )
    .await;
}

/// Switch Grok's active model — and, optionally, its reasoning effort — via the
/// standard ACP `session/set_model`. Sent as an `UntypedMessage` for the same
/// reason as `session/resume` / `session/set_config_option`: sacp 11.0.0's typed
/// request is gated behind the `unstable_session_model` feature (not enabled),
/// and the orphan rule blocks a local `JsonRpcRequest` impl.
///
/// Reasoning effort IS live-settable (verified against grok 0.2.99): a
/// `reasoning_effort` value carried in the request's `_meta.reasoningEffort`
/// (string `low`/`medium`/`high`) is applied on top of the model — grok logs
/// `applying reasoning_effort override from meta` and emits a `model_changed`
/// session notification echoing the effort. Passing `None` leaves the current
/// effort untouched (e.g. a pure model switch). The `~/.grok/config.toml`
/// `default_reasoning_effort` remains the at-birth global default this overrides.
async fn set_grok_model(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    model_id: String,
    reasoning_effort: Option<String>,
) -> Result<(), sacp::Error> {
    let params = build_grok_set_model_params(
        session_id.0.as_ref(),
        &model_id,
        reasoning_effort.as_deref(),
    );
    let untyped_req = UntypedMessage::new("session/set_model", params).map_err(|e| {
        sacp::util::internal_error(format!("Failed to build set_model request: {e}"))
    })?;
    cx.send_request_to(Agent, untyped_req).block_task().await?;
    Ok(())
}

/// Build the `session/set_model` params. A reasoning-effort override rides in
/// `_meta.reasoningEffort` (the exact key grok's sampling layer reads — verified
/// against 0.2.99); `None` omits `_meta` for a pure model switch.
fn build_grok_set_model_params(
    session_id: &str,
    model_id: &str,
    reasoning_effort: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::json!({
        "sessionId": session_id,
        "modelId": model_id,
    });
    if let Some(effort) = reasoning_effort {
        params["_meta"] = serde_json::json!({ "reasoningEffort": effort });
    }
    params
}

/// On reconnect, re-apply the user's last-picked Grok model AND reasoning effort
/// (both saved per agent by the frontend and shipped back as preferred config
/// values), reflecting each in its selector's `current_value`. Model is applied
/// first (a pure switch, effort untouched); effort is then re-applied on top of
/// the now-current model via `set_model`'s `_meta.reasoningEffort`.
async fn apply_grok_preferred_options(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    opts: &mut Vec<SessionConfigOptionInfo>,
    preferred_config_values: &BTreeMap<String, String>,
    specs: &HashMap<String, GrokEffortSpec>,
) {
    // Model preference — a pure `set_model` (no effort override). On success we
    // also re-point the effort selector at the newly-preferred model (grok ships
    // per-model effort only at birth, never on set_model).
    if let Some(pref) = preferred_config_values.get(GROK_MODEL_OPTION_ID).cloned() {
        // Split the eligibility read (immutable) from the rebuild (mutable) so we
        // never hold a `&mut opts` borrow across `set_grok_effort_selector_for_model`.
        let eligible = opts
            .iter()
            .find(|o| o.id == GROK_MODEL_OPTION_ID)
            .is_some_and(|o| {
                let SessionConfigKindInfo::Select(sel) = &o.kind;
                // Skip if already current, or the saved model is no longer offered.
                sel.current_value != pref && sel.options.iter().any(|x| x.value == pref)
            });
        if eligible {
            match set_grok_model(cx, session_id, pref.clone(), None).await {
                Ok(()) => {
                    if let Some(o) = opts.iter_mut().find(|o| o.id == GROK_MODEL_OPTION_ID) {
                        let SessionConfigKindInfo::Select(sel) = &mut o.kind;
                        sel.current_value = pref.clone();
                    }
                    if !specs.is_empty() {
                        set_grok_effort_selector_for_model(opts, &pref, specs);
                    }
                }
                Err(e) => tracing::error!(
                    "[ACP] failed to apply preferred grok model '{pref}' on connect: {e}"
                ),
            }
        }
    }
    // Effort preference — re-applied on top of the (possibly just-switched)
    // current model. The effort selector was rebuilt above for that model, so an
    // unsupported model (no selector) or an unoffered value is skipped here.
    if let Some(pref) = preferred_config_values.get(GROK_EFFORT_OPTION_ID) {
        let model_id = current_grok_model_id_from_opts(opts);
        if let Some(effort_opt) = opts.iter_mut().find(|o| o.id == GROK_EFFORT_OPTION_ID) {
            let SessionConfigKindInfo::Select(sel) = &mut effort_opt.kind;
            if &sel.current_value != pref && sel.options.iter().any(|o| &o.value == pref) {
                if let Some(model_id) = model_id {
                    match set_grok_model(cx, session_id, model_id, Some(pref.clone())).await {
                        Ok(()) => sel.current_value = pref.clone(),
                        Err(e) => tracing::error!(
                            "[ACP] failed to apply preferred grok effort '{pref}' on connect: {e}"
                        ),
                    }
                }
            }
        }
    }
}

/// The Grok model selector's current value, read from an in-memory options list.
fn current_grok_model_id_from_opts(opts: &[SessionConfigOptionInfo]) -> Option<String> {
    opts.iter().find(|o| o.id == GROK_MODEL_OPTION_ID).map(|o| {
        let SessionConfigKindInfo::Select(sel) = &o.kind;
        sel.current_value.clone()
    })
}

/// The Grok model selector's current value, read from the authoritative
/// `SessionState.config_options` snapshot — needed to carry a reasoning-effort
/// override on `session/set_model` (effort is applied relative to a model).
async fn current_grok_model_id(state: &Arc<RwLock<SessionState>>) -> Option<String> {
    let opts = state.read().await.config_options.clone()?;
    current_grok_model_id_from_opts(&opts)
}

/// Route a composer config-option change for Grok. Both live selectors go
/// through `session/set_model`: the model selector switches the model, and the
/// reasoning-effort selector re-sends the current model with an
/// `_meta.reasoningEffort` override (the `~/.grok/config.toml`
/// `default_reasoning_effort` stays the at-birth global default). Re-emits the
/// options with the new `current_value` so the backend snapshot stays authoritative.
///
/// A cross-agent-type switch rejected on an established conversation
/// (`is_grok_incompatible_agent_switch`) is handled in-band: re-emit the
/// authoritative options to revert the composer's optimistic pick and surface a
/// friendly, recoverable `AcpEvent::Error` (localized by the frontend via
/// `GROK_INCOMPATIBLE_AGENT_ERROR_CODE`), returning `Ok` so the caller does not
/// also emit the raw JSON-RPC error. The saved model preference is left intact,
/// so the suggested "start a new session" actually lands on the picked model
/// (a fresh session applies the preference pre-turn, where the switch succeeds).
async fn set_grok_config_option(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    config_id: String,
    value_id: String,
) -> Result<(), sacp::Error> {
    // Resolve the `set_model` args for whichever selector changed. A model pick
    // is the model itself (no effort override); an effort pick re-sends the
    // current model carrying the new `_meta.reasoningEffort`. Any other id is a
    // no-op (defensive — the composer only offers these two).
    let (model_id, effort) = if config_id == GROK_MODEL_OPTION_ID {
        (value_id.clone(), None)
    } else if config_id == GROK_EFFORT_OPTION_ID {
        match current_grok_model_id(state).await {
            Some(model_id) => (model_id, Some(value_id.clone())),
            // No model known yet — nothing to carry the effort override on.
            None => return Ok(()),
        }
    } else {
        return Ok(());
    };
    match set_grok_model(cx, session_id, model_id, effort).await {
        Ok(()) => {
            let (current, specs) = {
                let g = state.read().await;
                (g.config_options.clone(), g.grok_effort_specs.clone())
            };
            if let Some(mut opts) = current {
                if let Some(o) = opts.iter_mut().find(|o| o.id == config_id) {
                    let SessionConfigKindInfo::Select(sel) = &mut o.kind;
                    sel.current_value = value_id.clone();
                }
                // A MODEL switch must re-point the effort selector at the new
                // model — grok never re-sends per-model effort data on
                // set_model. An EFFORT change leaves the list shape alone; no
                // specs ⇒ leave as-is (flat-fallback session).
                if config_id == GROK_MODEL_OPTION_ID {
                    if let Some(specs) = &specs {
                        set_grok_effort_selector_for_model(&mut opts, &value_id, specs);
                    }
                }
                emit_session_config_options_info(state, emitter, opts).await;
            }
            Ok(())
        }
        Err(e) if is_grok_incompatible_agent_switch(&e) => {
            emit_grok_incompatible_agent_switch(state, emitter).await;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Recover from a Grok cross-agent-type model-switch rejection: revert the
/// composer's optimistic selection by re-emitting the authoritative (unchanged)
/// options, then surface a friendly, recoverable error the frontend localizes
/// via `GROK_INCOMPATIBLE_AGENT_ERROR_CODE`. Split out of `set_grok_config_option`
/// so it can be unit-tested without a live ACP connection.
async fn emit_grok_incompatible_agent_switch(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
) {
    // Clone the options out of the read guard into a local BEFORE emitting: the
    // `emit_*` helpers re-acquire this same state's WRITE lock, and an `if let`
    // scrutinee keeps its temporary (the read guard) alive across the whole body
    // in Rust 2021 — so reading inline would deadlock. `current_value` is
    // unchanged because the switch never took effect.
    let current = state.read().await.config_options.clone();
    if let Some(opts) = current {
        emit_session_config_options_info(state, emitter, opts).await;
    }
    emit_with_state(
        state,
        emitter,
        AcpEvent::Error {
            message: "Cannot switch to that model in an existing conversation. \
                      Start a new session to use it."
                .to_string(),
            agent_type: AgentType::Grok.to_string(),
            code: Some(GROK_INCOMPATIBLE_AGENT_ERROR_CODE.to_string()),
            // Recoverable: the conversation continues on its current model.
            terminal: false,
        },
    )
    .await;
}

/// Emit the composer's session config-option selectors. For Grok this reads the
/// synthesized `x.ai/sessionConfig` (parity path); for every other agent it runs
/// the standard preference-application + sacp-mapping pipeline unchanged.
#[allow(clippy::too_many_arguments)]
async fn apply_and_emit_session_config_options(
    cx: &ConnectionTo<Agent>,
    session: &mut sacp::ActiveSession<'_, Agent>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    grok_meta: Option<&serde_json::Map<String, serde_json::Value>>,
    grok_effort_specs: Option<&HashMap<String, GrokEffortSpec>>,
    preferred_mode_id: Option<&str>,
    preferred_config_values: &BTreeMap<String, String>,
    initial_config_options: Vec<SessionConfigOption>,
    file_system_runtime: &FileSystemRuntime,
) {
    if agent_type == AgentType::Grok {
        // Grok has no ACP session mode selector; full-access follows its launch
        // permission mode from `~/.grok/config.toml`.
        sync_file_system_outside_access(file_system_runtime, agent_type, None);
        let specs = grok_effort_specs.cloned().unwrap_or_default();
        if let Some(mut opts) = synthesize_grok_config_options(grok_meta, &specs) {
            // Cache the per-model effort map so a later model switch can rebuild
            // the effort selector for the target model (grok ships it only at
            // session birth). `None` when empty keeps the switch path on the
            // flat-fallback branch.
            state.write().await.grok_effort_specs = (!specs.is_empty()).then(|| specs.clone());
            let session_id = session.session_id().clone();
            apply_grok_preferred_options(
                cx,
                &session_id,
                &mut opts,
                preferred_config_values,
                &specs,
            )
            .await;
            emit_session_config_options_info(state, emitter, opts).await;
            return;
        }
        // No x.ai/sessionConfig (unexpected): fall through to the standard path,
        // which for Grok emits an empty list (no selectors) — same as before.
    }
    let updated = apply_preferred_session_options(
        cx,
        session,
        state,
        emitter,
        preferred_mode_id,
        preferred_config_values,
        initial_config_options,
        file_system_runtime,
        agent_type,
    )
    .await;
    emit_session_config_options_values(state, emitter, agent_type, updated).await;
}

async fn emit_prompt_capabilities(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    capabilities: &sacp::schema::PromptCapabilities,
) {
    emit_with_state(
        state,
        emitter,
        AcpEvent::PromptCapabilities {
            prompt_capabilities: PromptCapabilitiesInfo {
                image: capabilities.image,
                audio: capabilities.audio,
                embedded_context: capabilities.embedded_context,
            },
        },
    )
    .await;
}

fn resolve_working_dir(working_dir: Option<&str>) -> PathBuf {
    match working_dir {
        Some(dir) => {
            let path = PathBuf::from(dir);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            }
        }
        None => std::env::current_dir()
            .unwrap_or_else(|_| dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))),
    }
}

/// Initial client-side FS sandbox policy before the agent reports its mode.
///
/// Grok's `bypassPermissions` launch mode is its full-access equivalent. Other
/// agents use session mode / config-option ids recognized by
/// [`mode_allows_outside_workspace`].
fn initial_allow_outside_workspace(
    agent_type: AgentType,
    preferred_mode_id: Option<&str>,
    preferred_config_values: &BTreeMap<String, String>,
) -> bool {
    if agent_type == AgentType::Grok
        && crate::commands::acp::grok_launch_permission_mode()
            .as_deref()
            .is_some_and(mode_allows_outside_workspace)
    {
        return true;
    }
    if preferred_mode_id.is_some_and(mode_allows_outside_workspace) {
        return true;
    }
    preferred_config_values
        .get("mode")
        .is_some_and(|v| mode_allows_outside_workspace(v))
}

/// Keep `fs/read_text_file` / `fs/write_text_file` workspace sandbox in sync
/// with the agent's current full-access mode (including Grok bypassPermissions).
fn sync_file_system_outside_access(
    file_system_runtime: &FileSystemRuntime,
    agent_type: AgentType,
    mode_id: Option<&str>,
) {
    let allow = if agent_type == AgentType::Grok {
        crate::commands::acp::grok_launch_permission_mode()
            .as_deref()
            .is_some_and(mode_allows_outside_workspace)
    } else {
        mode_id.is_some_and(mode_allows_outside_workspace)
    };
    file_system_runtime.set_allow_outside_workspace(allow);
}

fn claude_raw_sdk_session_meta(
    agent_type: AgentType,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if agent_type != AgentType::ClaudeCode {
        return None;
    }

    let mut claude_code = serde_json::Map::new();
    claude_code.insert(
        "emitRawSDKMessages".to_string(),
        serde_json::Value::Bool(true),
    );

    let mut meta = serde_json::Map::new();
    meta.insert(
        "claudeCode".to_string(),
        serde_json::Value::Object(claude_code),
    );
    Some(meta)
}

/// Pure deep-merge of Claude Code route suppression into ACP `_meta`.
///
/// Validates `claudeCode` / `options` / `disallowedTools` object/array shapes.
/// Malformed shapes return `RouteUnavailable { NativeSuppressionInvalid }`
/// before any session request is sent. On Codeg, appends missing `Agent`/`Task`
/// (from the plan) exactly once. On native / non-Claude plans, returns input
/// metadata serde-value-equivalent (no Codeg deny injection).
fn merge_claude_route_meta(
    mut meta: serde_json::Map<String, serde_json::Value>,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
) -> Result<serde_json::Map<String, serde_json::Value>, AcpError> {
    use crate::acp::delegation::route::{NativeSuppressionPlan, RouteDegradedReason};

    let suppress_tools = match &plan.native_suppression {
        NativeSuppressionPlan::ClaudeDisallowedTools { tools } => tools.as_slice(),
        _ => return Ok(meta),
    };

    // Validate shapes even when we will merge (and when empty map has no claudeCode yet).
    if let Some(claude_val) = meta.get("claudeCode") {
        if !claude_val.is_object() {
            return Err(AcpError::RouteUnavailable {
                reason: RouteDegradedReason::NativeSuppressionInvalid,
            });
        }
        let claude = claude_val.as_object().expect("checked is_object");
        if let Some(options_val) = claude.get("options") {
            if !options_val.is_object() {
                return Err(AcpError::RouteUnavailable {
                    reason: RouteDegradedReason::NativeSuppressionInvalid,
                });
            }
            if let Some(tools_val) = options_val.get("disallowedTools") {
                if !tools_val.is_array() {
                    return Err(AcpError::RouteUnavailable {
                        reason: RouteDegradedReason::NativeSuppressionInvalid,
                    });
                }
            }
        }
    }

    let claude = meta
        .entry("claudeCode".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let claude_obj = claude.as_object_mut().ok_or(AcpError::RouteUnavailable {
        reason: RouteDegradedReason::NativeSuppressionInvalid,
    })?;

    let options = claude_obj
        .entry("options".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let options_obj = options.as_object_mut().ok_or(AcpError::RouteUnavailable {
        reason: RouteDegradedReason::NativeSuppressionInvalid,
    })?;

    let tools_val = options_obj
        .entry("disallowedTools".to_string())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    let tools_arr = tools_val.as_array_mut().ok_or(AcpError::RouteUnavailable {
        reason: RouteDegradedReason::NativeSuppressionInvalid,
    })?;

    for tool in suppress_tools {
        let already = tools_arr.iter().any(|v| v.as_str() == Some(tool.as_str()));
        if !already {
            tools_arr.push(serde_json::Value::String(tool.clone()));
        }
    }

    Ok(meta)
}

/// Built-in Grok tool short names to strip for hidden generation sessions.
///
/// Sourced from grok-build docs / tool IDs (`run_terminal_cmd`, `read_file`, …)
/// plus MCP meta-tools that a restrictive allowlist would otherwise keep
/// (`search_tool` / `use_tool`). Applied via `_meta.agentProfile.disallowedTools`
/// on ACP `session/new|load|resume` — the only reliable tool denylist path for
/// `grok agent stdio` (CLI `--disallowed-tools` is headless-only).
const GROK_HIDDEN_GENERATION_DISALLOWED_TOOLS: &[&str] = &[
    // Core file / shell / search
    "run_terminal_cmd",
    "run_terminal_command",
    "read_file",
    "search_replace",
    "write",
    "grep",
    "list_dir",
    // Web / media
    "web_search",
    "web_fetch",
    "image_gen",
    "image_edit",
    "image_to_video",
    "reference_to_video",
    // Task / plan / goal
    "todo_write",
    "task",
    "get_task_output",
    "kill_task",
    "wait_tasks",
    "monitor",
    "update_goal",
    "enter_plan_mode",
    "exit_plan_mode",
    "ask_user_question",
    // Subagents
    "Agent",
    "spawn_subagent",
    // MCP meta-tools (restrictive allowlists intentionally keep these)
    "search_tool",
    "use_tool",
    // Scheduler / LSP / misc
    "scheduler_create",
    "scheduler_list",
    "scheduler_delete",
    "lsp",
    "get_terminal_command_output",
    "kill_terminal_command",
];

/// Grok ACP-only gate for InternalTitle / InternalTranslate: stamp a session
/// `agentProfile` that denylists interactive tools so the model cannot call
/// shell/MCP mid-title (or mid-translate). Priority #1 over config/CLI agent
/// selection in Grok shell (`_meta.agentProfile`).
///
/// `tools: []` is intentionally omitted — empty allowlist means "inherit all"
/// in Grok's AgentDefinition; denylist is the hard strip.
fn merge_grok_hidden_generation_agent_profile(
    mut meta: serde_json::Map<String, serde_json::Value>,
    agent_type: AgentType,
    purpose: ConnectionPurpose,
) -> serde_json::Map<String, serde_json::Value> {
    if agent_type != AgentType::Grok || !purpose.is_hidden_generation() {
        return meta;
    }
    let disallowed: Vec<serde_json::Value> = GROK_HIDDEN_GENERATION_DISALLOWED_TOOLS
        .iter()
        .map(|name| serde_json::Value::String((*name).to_string()))
        .collect();
    meta.insert(
        "agentProfile".to_string(),
        serde_json::json!({
            "name": "codeg-hidden-generation",
            "description": "Codeg internal title/translate run: no interactive tools",
            "disallowedTools": disallowed,
            "agentsMd": false,
            "discoverSkills": false,
            "maxTurns": 1,
            "permissionMode": "dontAsk",
        }),
    );
    meta
}

/// Built-in Grok tool names that constitute the **native subagent creation
/// surface**. Stripped on Codeg-route user sessions via session
/// `_meta.agentProfile.disallowedTools` — the ACP-effective denylist path
/// (CLI `--no-subagents` alone does not remove these from the model toolset
/// on `agent stdio`).
///
/// Intentionally narrow: shell / read / MCP / skills stay available so the
/// parent can still work and use `codeg-mcp` delegation.
const GROK_CODEG_ROUTE_DISALLOWED_TOOLS: &[&str] = &[
    "spawn_subagent",
    "get_command_or_subagent_output",
    "kill_command_or_subagent",
    // Legacy / alternate names observed in Grok tool catalogs
    "Agent",
    "task",
];

/// Grok Codeg-route gate for ordinary (non-hidden) sessions: stamp a minimal
/// `agentProfile` that denylists only native subagent tools so creation routes
/// through `codeg-mcp` instead of `spawn_subagent`.
///
/// Skipped when:
/// - not Grok, or
/// - plan is not `GrokNoSubagents` (Native / FeatureDisabled / SafeFallback), or
/// - purpose is hidden generation (that profile already denylists subagents
///   plus shell/MCP and must not be overwritten with the narrow route profile).
///
/// Does **not** set `maxTurns`, `permissionMode`, `agentsMd`, or
/// `discoverSkills` — those would break normal chat UX.
/// `tools: []` is omitted (empty allowlist = inherit all).
fn merge_grok_codeg_route_agent_profile(
    mut meta: serde_json::Map<String, serde_json::Value>,
    agent_type: AgentType,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
    purpose: ConnectionPurpose,
) -> serde_json::Map<String, serde_json::Value> {
    use crate::acp::delegation::route::NativeSuppressionPlan;

    if agent_type != AgentType::Grok
        || purpose.is_hidden_generation()
        || !matches!(
            plan.native_suppression,
            NativeSuppressionPlan::GrokNoSubagents
        )
    {
        return meta;
    }

    let disallowed: Vec<serde_json::Value> = GROK_CODEG_ROUTE_DISALLOWED_TOOLS
        .iter()
        .map(|name| serde_json::Value::String((*name).to_string()))
        .collect();
    meta.insert(
        "agentProfile".to_string(),
        serde_json::json!({
            "name": "codeg-route-no-native-subagents",
            "description": "Codeg route: native subagent surface suppressed",
            "disallowedTools": disallowed,
        }),
    );
    meta
}

/// Grok-only timeout policy for the injected `codeg-mcp` server.
///
/// Grok defaults MCP tool calls to 6,000s; without an explicit
/// `_meta.mcpConfig.codeg-mcp` map, a hung/partial stdio response can pin a
/// tool card for the entire window. Values match the approved design
/// (`docs/superpowers/specs/2026-07-30-grok-codeg-mcp-response-budget-timeouts-design.md`).
/// Wire keys are camelCase (`startupTimeoutMs`, …) as required by Grok's
/// `McpServerMetaConfig` deserializer.
fn grok_codeg_mcp_timeout_config() -> serde_json::Value {
    serde_json::json!({
        "startupTimeoutMs": 30_000_u64,
        "toolTimeoutMs": 30_000_u64,
        "toolTimeoutsMs": {
            "get_workflow_capabilities": 5_000_u64,
            "check_user_feedback": 10_000_u64,
            "get_session_info": 15_000_u64,
            "get_workflow_state": 15_000_u64,
            "cancel_delegation": 15_000_u64,
            "reply_to_delegation": 15_000_u64,
            "publish_workflow_manifest": 30_000_u64,
            "settle_workflow_gate": 30_000_u64,
            "delegate_to_agent": 180_000_u64,
            "continue_delegation": 300_000_u64,
            "ask_user_question": 1_800_000_u64,
            "request_parent_decision": 1_800_000_u64,
            "get_delegation_status": 5_400_000_u64,
        }
    })
}

/// Merge Grok `_meta.mcpConfig.codeg-mcp` timeout policy. No-op for other
/// agents. Preserves any pre-existing `mcpConfig` entries for other servers
/// and only sets/replaces the `codeg-mcp` key (we own that companion).
fn merge_grok_codeg_mcp_timeout_config(
    mut meta: serde_json::Map<String, serde_json::Value>,
    agent_type: AgentType,
) -> serde_json::Map<String, serde_json::Value> {
    if agent_type != AgentType::Grok {
        return meta;
    }
    let codeg_cfg = grok_codeg_mcp_timeout_config();
    match meta.get_mut("mcpConfig") {
        Some(serde_json::Value::Object(map)) => {
            map.insert("codeg-mcp".to_string(), codeg_cfg);
        }
        Some(_) | None => {
            let mut map = serde_json::Map::new();
            map.insert("codeg-mcp".to_string(), codeg_cfg);
            meta.insert("mcpConfig".to_string(), serde_json::Value::Object(map));
        }
    }
    meta
}

/// Merge Claude raw-SDK meta, optional hidden-generation / Codeg-route agent
/// profiles, Grok codeg-mcp timeout policy, Claude route suppression, terminal
/// snapshot, and adapter contributions. Consumes `route_plan.native_suppression`
/// for Claude deny list and Grok Codeg-route `agentProfile`.
fn session_request_meta(
    agent_type: AgentType,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
    purpose: ConnectionPurpose,
) -> Result<Meta, AcpError> {
    let existing = claude_raw_sdk_session_meta(agent_type).unwrap_or_default();
    let with_hidden = merge_grok_hidden_generation_agent_profile(existing, agent_type, purpose);
    let with_grok_route =
        merge_grok_codeg_route_agent_profile(with_hidden, agent_type, route_plan, purpose);
    let with_timeouts = merge_grok_codeg_mcp_timeout_config(with_grok_route, agent_type);
    let with_route = merge_claude_route_meta(with_timeouts, route_plan)?;
    terminal_metadata(with_route, spec, adapter)
}

/// Build the ACP `initialize` request, declaring client capabilities and the
/// connection's terminal shell dialect under `_meta["codeg.dev/terminal"]`.
fn build_initialize_request(
    agent_type: AgentType,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
) -> Result<InitializeRequest, AcpError> {
    let meta = terminal_metadata(Meta::default(), spec, adapter)?;
    Ok(InitializeRequest::new(ProtocolVersion::LATEST)
        .client_capabilities(build_client_capabilities(agent_type))
        .meta(meta))
}

/// The client capabilities codeg advertises on Initialize, with per-agent
/// gates. Extracted for testability — each gate is a documented product
/// decision:
///
/// - Everyone: filesystem read/write + terminal, for ACP tool execution.
/// - Codex only: form elicitation, so codex's native Plan-mode
///   `request_user_input` is delivered as `elicitation/create` (handled by
///   `handle_elicitation_request`) instead of being silently answered `{}`.
///   NOTE this reroutes codex's WHOLE form-elicitation surface — MCP
///   tool-call approvals and MCP-server forms included — so the handler must
///   cover every shape (`classify_elicitation`). URL elicitation is
///   deliberately NOT advertised: codex-acp then falls back to
///   `session/request_permission`, which codeg already handles. Scoped to
///   Codex to keep the blast radius off other agents (e.g. Claude's native
///   AskUserQuestion, which would otherwise un-gate and duplicate the
///   codeg-mcp ask tool).
/// - Claude Code only: `_meta["subagent-transcript"] = true` — opt into
///   claude-agent-acp ≥0.63's subagent transcript forwarding (#881, SDK
///   `forwardSubagentText`). Subagent text/thought chunks then stream with
///   update-level `_meta.claudeCode.parentToolUseId` instead of being
///   filtered; codeg routes them into the live Agent capsule (see
///   `claude_chunk_parent_tool_use_id`). The adapter checks strictly
///   `=== true`, and a pre-0.63 binary ignores the unknown key, so this is
///   inert everywhere it isn't understood.
fn build_client_capabilities(agent_type: AgentType) -> ClientCapabilities {
    let mut client_capabilities = ClientCapabilities::new()
        // Grok otherwise moves shell process ownership to its ACP client. Its
        // native backend keeps cancellation and process teardown on one side.
        .terminal(agent_type != AgentType::Grok)
        .fs(FileSystemCapabilities::new()
            .read_text_file(true)
            .write_text_file(true));
    if agent_type == AgentType::Codex {
        client_capabilities = client_capabilities
            .elicitation(ElicitationCapabilities::new().form(ElicitationFormCapabilities::new()));
    }
    if agent_type == AgentType::ClaudeCode {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "subagent-transcript".to_string(),
            serde_json::Value::Bool(true),
        );
        client_capabilities = client_capabilities.meta(meta);
    }
    client_capabilities
}

fn build_new_session_request(
    agent_type: AgentType,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
    purpose: ConnectionPurpose,
) -> Result<NewSessionRequest, AcpError> {
    let meta = session_request_meta(agent_type, route_plan, spec, adapter, purpose)?;
    let mut req = NewSessionRequest::new(cwd.to_path_buf()).meta(meta);
    if !mcp_servers.is_empty() {
        req = req.mcp_servers(mcp_servers);
    }
    Ok(req)
}

#[allow(clippy::too_many_arguments)]
fn build_load_session_request(
    agent_type: AgentType,
    session_id: SessionId,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
    purpose: ConnectionPurpose,
) -> Result<LoadSessionRequest, AcpError> {
    let meta = session_request_meta(agent_type, route_plan, spec, adapter, purpose)?;
    let mut req = LoadSessionRequest::new(session_id, cwd.to_path_buf()).meta(meta);
    if !mcp_servers.is_empty() {
        req = req.mcp_servers(mcp_servers);
    }
    Ok(req)
}

/// Build a `session/resume` request. Mirrors `build_load_session_request`
/// (same fields + ClaudeCode raw-SDK meta + terminal meta + non-empty
/// mcp_servers); the only wire difference is that
/// `ResumeSessionRequest.mcp_servers` is `skip_serializing_if = Vec::is_empty`,
/// so an empty list is omitted from the payload rather than emitted as `[]`.
#[allow(clippy::too_many_arguments)]
fn build_resume_session_request(
    agent_type: AgentType,
    session_id: SessionId,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
    purpose: ConnectionPurpose,
) -> Result<ResumeSessionRequest, AcpError> {
    let meta = session_request_meta(agent_type, route_plan, spec, adapter, purpose)?;
    let mut req = ResumeSessionRequest::new(session_id, cwd.to_path_buf()).meta(meta);
    if !mcp_servers.is_empty() {
        req = req.mcp_servers(mcp_servers);
    }
    Ok(req)
}

/// Wire-level half of `session/resume`: send the request and deserialize the
/// reply into `ResumeSessionResponse`.
///
/// `sacp` 11.0.0 ships no `JsonRpcRequest` impl for `ResumeSessionRequest`, and
/// the orphan rule blocks codeg from adding one, so we send via `UntypedMessage`
/// — the same in-tree pattern `set_session_config_option_inner` already uses for
/// `session/set_config_option`. On a JSON-RPC error the agent returns,
/// `block_task()` yields `Err(sacp::Error)` with `.code` / `.to_string()`
/// intact, so the caller's error ladder reads identically to the
/// `session/load` arm.
///
/// Also extracts any agent-returned `sessionId` / `session_id` from the raw
/// body for [`crate::acp::session_attach::gate_session_started_for_attach`]
/// (typed `ResumeSessionResponse` has no session id field).
async fn send_resume_session(
    cx: &ConnectionTo<Agent>,
    req: ResumeSessionRequest,
) -> Result<
    (
        ResumeSessionResponse,
        Option<serde_json::Value>,
        Option<String>,
    ),
    sacp::Error,
> {
    let untyped_req = UntypedMessage::new("session/resume", req)
        .map_err(|e| sacp::util::internal_error(format!("Failed to build resume request: {e}")))?;

    let raw_response = cx.send_request_to(Agent, untyped_req).block_task().await?;
    // Capture the raw top-level `models` (per-model reasoning-effort data) BEFORE
    // deserializing into the typed response, which drops it (Grok only — the
    // field survives serde as an ignored unknown for other agents).
    let models = raw_response.get("models").cloned();
    let returned_session_id =
        crate::acp::session_attach::extract_session_id_from_raw_response(&raw_response);
    let resp = serde_json::from_value(raw_response)
        .map_err(|e| sacp::util::internal_error(format!("Failed to parse resume response: {e}")))?;
    Ok((resp, models, returned_session_id))
}

/// Wire-level `session/load` with raw session-id extraction (typed
/// `LoadSessionResponse` has no sessionId field). Used so
/// `ResumeExistingOnly` can verify external identity before SessionStarted.
async fn send_load_session_capturing_id(
    cx: &ConnectionTo<Agent>,
    req: LoadSessionRequest,
) -> Result<(LoadSessionResponse, Option<String>), sacp::Error> {
    let untyped_req = UntypedMessage::new("session/load", req)
        .map_err(|e| sacp::util::internal_error(format!("Failed to build load request: {e}")))?;
    let raw_response = cx.send_request_to(Agent, untyped_req).block_task().await?;
    let returned_session_id =
        crate::acp::session_attach::extract_session_id_from_raw_response(&raw_response);
    let resp = serde_json::from_value(raw_response)
        .map_err(|e| sacp::util::internal_error(format!("Failed to parse load response: {e}")))?;
    Ok((resp, returned_session_id))
}

/// Emit SessionLoadFailed(unresumable) + Error status and stop bootstrap without
/// SessionStarted (preserves durable external_id; no prompt enqueue).
///
/// When a delegation broker is available, durably settle the **active run** as
/// `failed`/`unresumable` via pre-bootstrap handoff registration
/// ([`DelegationBroker::settle_bootstrap_unresumable`]) so lifecycle's later
/// disconnect cancel cannot relabel the outcome as generic `canceled`.
///
/// Requires [`DelegationBroker::begin_run_admission`] before bootstrap so the
/// connection incarnation is known to the broker while the manager is still
/// awaiting route readiness (connection id is not returned until Ready).
pub(crate) async fn refuse_unresumable_bootstrap(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    session_id: &str,
    message: String,
    broker: Option<&crate::acp::delegation::broker::DelegationBroker>,
    connection_id: &str,
) {
    // Settle first so a racing disconnect cancel is second-stamp and cannot
    // win first-terminal-wins with `canceled`. Claim-first helper returns a
    // typed result callers must honor (never re-settle).
    // Classification may use the raw diagnostic string; the frontend event must
    // never carry raw ACP/SQLite/agent bodies.
    let frontend_message =
        crate::acp::delegation::broker::sanitize_bootstrap_unresumable_message(&message);
    if let Some(broker) = broker {
        if let Some(task_id) = broker
            .resolve_task_id_for_connection(connection_id)
            .await
            .or(broker
                .cold_resolve_task_id_for_connection(connection_id)
                .await)
        {
            let result = broker
                .settle_bootstrap_unresumable(&task_id, Some(connection_id), message.clone())
                .await;
            if let crate::acp::delegation::broker::BootstrapSettleResult::Existing { error_code } =
                &result
            {
                tracing::info!(
                    task_id = %task_id,
                    child_connection_id = %connection_id,
                    existing_code = ?error_code,
                    "[acp] bootstrap refuse settle lost claim to existing terminal"
                );
            }
        } else {
            tracing::info!(
                child_connection_id = %connection_id,
                "[acp] bootstrap refuse: no live/cold run for connection — skip settle"
            );
        }
    }
    emit_with_state(
        state,
        emitter,
        AcpEvent::SessionLoadFailed {
            session_id: session_id.to_string(),
            message: frontend_message,
            code: "unresumable".to_string(),
        },
    )
    .await;
    emit_with_state(
        state,
        emitter,
        AcpEvent::StatusChanged {
            status: ConnectionStatus::Error,
        },
    )
    .await;
}

/// Send `session/new`. For Grok, send it UNTYPED so the raw top-level `models`
/// (per-model reasoning-effort data — dropped by the typed `NewSessionResponse`
/// because the `unstable_session_model` feature is off) can be captured before
/// deserialization. Every other agent keeps the exact typed send, byte-for-byte,
/// and gets `None`.
async fn send_new_session_capturing_models(
    cx: &ConnectionTo<Agent>,
    agent_type: AgentType,
    req: NewSessionRequest,
) -> Result<(NewSessionResponse, Option<serde_json::Value>), sacp::Error> {
    if agent_type != AgentType::Grok {
        return Ok((cx.send_request_to(Agent, req).block_task().await?, None));
    }
    // Literal method string: the schema's `SESSION_NEW_METHOD_NAME` is
    // `pub(crate)`, and sacp ships no `JsonRpcRequest` for a raw new-session, so
    // this mirrors the `session/resume` / `session/fork` untyped sends.
    let untyped_req = UntypedMessage::new("session/new", req).map_err(|e| {
        sacp::util::internal_error(format!("Failed to build new_session request: {e}"))
    })?;
    let raw_response = cx.send_request_to(Agent, untyped_req).block_task().await?;
    let models = raw_response.get("models").cloned();
    let resp = serde_json::from_value(raw_response).map_err(|e| {
        sacp::util::internal_error(format!("Failed to parse new_session response: {e}"))
    })?;
    Ok((resp, models))
}

/// Whether MCP servers forwarded over the ACP wire (`session/new.mcpServers`)
/// actually reach the agent's model. Almost all adapters deliver them; pi-acp
/// (0.0.31) accepts the `mcpServers` field but DROPS it — it never forwards MCP
/// to the inner `pi --mode rpc` process, and pi has no native MCP. So forwarding
/// either user servers or the built-in codeg-mcp companion to pi is futile, and
/// injecting codeg-mcp would falsely mark delegation/feedback/ask as available
/// (`feedback_tool_available`, a registered delegation token pi can never use).
/// `supports_mcp` stays `true` for pi (session/new tolerates the field), so this
/// is a separate, narrower gate. Gate codeg-mcp injection on it.
pub(crate) fn agent_delivers_wire_mcp(agent_type: AgentType) -> bool {
    !matches!(agent_type, AgentType::Pi)
}

/// Load MCP servers configured for `agent_type` and convert them into the
/// ACP wire format. Errors and unsupported entries are logged and skipped so
/// a single malformed entry never blocks a session from starting.
///
/// **Host-side name dedupe (all agents):** omit any server name already present
/// in the agent's own on-disk config that it auto-loads without ACP
/// `session/new.mcpServers`. This is the DrawCode counterpart of
/// `codex-acp`'s session-config name filter — applied uniformly so CodeBuddy
/// (and others) cannot double-register the same MCP via native file + wire
/// (which hung CodeBuddy `session/new` for ~60s on duplicate `knot`).
/// The built-in `codeg-mcp` companion is injected separately by
/// [`inject_codeg_mcp`] and is never part of this user-server list.
fn load_mcp_servers_for_agent(agent_type: AgentType) -> Vec<McpServer> {
    let entries = match crate::commands::mcp::read_servers_for_agent_type(agent_type) {
        Ok(map) => map,
        Err(err) => {
            tracing::error!(
                "[ACP][{}] failed to read MCP servers from local config: {err}",
                agent_type
            );
            return Vec::new();
        }
    };

    let native_names = crate::commands::mcp::agent_native_mcp_server_names(agent_type);
    let mut out = Vec::with_capacity(entries.len());
    for (name, spec) in entries {
        if native_names.contains(&name) {
            // Same name already auto-loaded from disk — do not also put it on
            // the ACP wire (Codex adapter does the same filter in-session).
            tracing::debug!(
                "[ACP][{agent_type}] skip wire MCP '{name}': already in agent native config"
            );
            continue;
        }
        match canonical_spec_to_mcp_server(&name, &spec) {
            Ok(server) => out.push(server),
            Err(err) => {
                tracing::warn!(
                    "[ACP][{}] skip MCP server '{name}' (cannot map to ACP schema): {err}",
                    agent_type
                );
            }
        }
    }
    out
}

/// Context the connection layer needs to inject the built-in `codeg-mcp`
/// MCP entry. Built once per `run_connection` from the live AppState pieces
/// (broker config, token registry, UDS path) and passed through.
///
/// Optional because some test paths spin up `run_connection` without a
/// full delegation stack — those just skip injection.
/// Injection-time lookup of which agents the user has disabled in settings.
///
/// `delegate_to_agent`'s advertised targets must track the live toggle: a
/// disabled agent cannot spawn anyway (`build_session_runtime_env` rejects it
/// inside the delegation spawner), so listing it would only invite doomed
/// calls. Read fresh on every injection — sessions launched before a toggle
/// flip keep their launch-time list, and the spawn-time check stays the hard
/// gate for those.
#[async_trait::async_trait]
pub trait AgentAvailabilityLookup: Send + Sync {
    /// Wire slugs (`AgentType::as_wire`) of the agents disabled in settings.
    async fn disabled_agent_wire_slugs(&self) -> Vec<String>;
}

/// [`AgentAvailabilityLookup`] over the live `AppDatabase`: `agent_setting`
/// rows with `enabled = false`. An absent row means enabled (the settings
/// default). A read error fails OPEN — the enum then lists everything rather
/// than taking the whole companion injection down, and the spawn-time
/// disabled check still enforces.
pub struct DbAgentAvailabilityLookup {
    pub db: Arc<crate::db::AppDatabase>,
}

#[async_trait::async_trait]
impl AgentAvailabilityLookup for DbAgentAvailabilityLookup {
    async fn disabled_agent_wire_slugs(&self) -> Vec<String> {
        match crate::db::service::agent_setting_service::list(&self.db.conn).await {
            Ok(rows) => rows
                .into_iter()
                .filter(|row| !row.enabled)
                .filter_map(|row| serde_json::from_str::<AgentType>(&row.agent_type).ok())
                .map(|agent_type| agent_type.as_wire().into_owned())
                .collect(),
            Err(e) => {
                tracing::warn!(
                    "[delegation] reading agent settings failed ({e}); \
                     delegate targets will not be filtered this launch"
                );
                Vec::new()
            }
        }
    }
}

#[derive(Clone)]
pub struct DelegationInjection {
    pub broker: Arc<crate::acp::delegation::broker::DelegationBroker>,
    pub continuation_coordinator: std::sync::Weak<
        crate::acp::delegation::continuation::coordinator::DelegationContinuationCoordinator,
    >,
    pub(crate) parent_connection_exit_causes: Arc<ParentConnectionExitEvidence>,
    pub tokens: Arc<crate::acp::delegation::listener::TokenRegistry>,
    /// Authenticated ready-lease registry. Lease waiters are registered at
    /// MCP injection when the immutable plan exposes Codeg delegation.
    pub leases: Arc<crate::acp::delegation::lease::CompanionLeaseRegistry>,
    pub socket_path: PathBuf,
    /// Which agents are currently disabled in settings, read at injection
    /// time so `delegate_to_agent` only advertises launchable targets.
    pub agent_availability: Arc<dyn AgentAvailabilityLookup>,
    /// Hot-swappable "is live-feedback enabled?" flag. Read at injection time
    /// so `codeg-mcp` can be injected even when the plan omits Codeg delegation.
    /// Shares the same `tokens` registry and UDS socket as the ready lease.
    pub feedback: crate::acp::feedback::FeedbackRuntimeConfig,
    /// Hot-swappable "is ask-user-question enabled?" flag. Read at injection
    /// time. Non-Grok agents list companion feature `ask`; Grok omits that
    /// duplicate and uses this flag for its native `_x.ai/ask_user_question`
    /// bridge instead.
    pub ask: crate::acp::question::QuestionRuntimeConfig,
    /// Hot-swappable "is get-session-info enabled?" flag. Read at injection time
    /// so `codeg-mcp` can be injected on its own; the companion's `--features`
    /// lists `sessions` to expose `get_session_info`. No teardown handle (the
    /// lookup is stateless).
    pub sessions: crate::acp::session_info::SessionInfoRuntimeConfig,
    /// Question registry handle for the teardown cascade. The `run_connection`
    /// cleanup guard calls `cancel_questions_by_parent` through this so a pending
    /// `ask_user_question` is reclaimed synchronously on disconnect, mirroring
    /// the delegation `broker.cancel_by_parent` cleanup. Shares the same backing
    /// `ConnectionManager` as the listener's question lookup.
    pub questions: Arc<dyn crate::acp::question::SessionQuestionAccess>,
    /// Soft-supervisor wake handle. Cloned onto each `SessionState` at spawn so
    /// agent activity and permission/question changes can nudge the supervisor.
    /// Default `noop` until bootstrap installs a live channel.
    pub supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake,
    /// Process-local reliability metrics (route validation at launch).
    pub metrics: std::sync::Arc<crate::acp::delegation::metrics::DelegationMetrics>,

    /// Plan-approval registry handle for Grok's `exit_plan_mode` ext bridge.
    /// This native plan-mode path is always wired and shares the manager-backed
    /// teardown semantics used by pending questions.
    pub plan_approvals: Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>,
}

#[derive(Default)]
pub(crate) struct ParentConnectionExitEvidence {
    entries: std::sync::Mutex<HashMap<String, ParentConnectionExitEvidenceEntry>>,
}

struct ParentConnectionExitEvidenceEntry {
    termination: crate::acp::termination::AcpTerminationSummaryV1,
    suspension_drain_timeout: bool,
}

impl ParentConnectionExitEvidence {
    pub(crate) fn record_intent(
        &self,
        connection_id: &str,
        origin: crate::acp::termination::AcpDisconnectOrigin,
        at: chrono::DateTime<chrono::Utc>,
    ) {
        use crate::acp::termination::{
            AcpDisconnectOrigin, AcpTerminationClassification, AcpTerminationReason,
            AcpTerminationSource, AcpTerminationSummaryV1,
        };

        let (source, reason, classification) = match origin {
            AcpDisconnectOrigin::ExplicitUser => (
                AcpTerminationSource::Frontend,
                AcpTerminationReason::FrontendDisconnected,
                AcpTerminationClassification::Explicit,
            ),
            AcpDisconnectOrigin::LegacyUnspecified => (
                AcpTerminationSource::Legacy,
                AcpTerminationReason::LegacyUnspecified,
                AcpTerminationClassification::LegacyUnknown,
            ),
            _ => (
                AcpTerminationSource::Frontend,
                AcpTerminationReason::FrontendDisconnected,
                AcpTerminationClassification::Intentional,
            ),
        };
        let mut summary = AcpTerminationSummaryV1::new(source, reason, classification, true, at);
        if origin != AcpDisconnectOrigin::LegacyUnspecified {
            summary.frontend_origin = Some(origin);
            summary.requested_at = Some(at);
        }

        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let preserve_existing_evidence = entries.get(connection_id).is_some_and(|existing| {
            origin == AcpDisconnectOrigin::LegacyUnspecified
                || (existing.termination.frontend_origin.is_some()
                    && existing.termination.frontend_origin
                        != Some(AcpDisconnectOrigin::LegacyUnspecified))
        });
        if !preserve_existing_evidence {
            entries.insert(
                connection_id.to_string(),
                ParentConnectionExitEvidenceEntry {
                    termination: summary,
                    suspension_drain_timeout: false,
                },
            );
        }
    }

    pub(crate) fn record_observation(
        &self,
        connection_id: &str,
        summary: crate::acp::termination::AcpTerminationSummaryV1,
    ) {
        use crate::acp::termination::AcpTerminationClassification;

        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match entries.get(connection_id) {
            None => {
                entries.insert(
                    connection_id.to_string(),
                    ParentConnectionExitEvidenceEntry {
                        termination: summary,
                        suspension_drain_timeout: false,
                    },
                );
            }
            Some(existing)
                if !existing.suspension_drain_timeout
                    && existing.termination.classification
                        == AcpTerminationClassification::LegacyUnknown
                    && summary.classification == AcpTerminationClassification::Unexpected =>
            {
                entries.insert(
                    connection_id.to_string(),
                    ParentConnectionExitEvidenceEntry {
                        termination: summary,
                        suspension_drain_timeout: false,
                    },
                );
            }
            Some(_) => {}
        }
    }

    fn record_session_lost(&self, connection_id: &str, observed_at: chrono::DateTime<chrono::Utc>) {
        self.record_observation(
            connection_id,
            crate::acp::termination::AcpTerminationSummaryV1::new(
                crate::acp::termination::AcpTerminationSource::Session,
                crate::acp::termination::AcpTerminationReason::SessionLost,
                crate::acp::termination::AcpTerminationClassification::Unexpected,
                true,
                observed_at,
            ),
        );
    }

    #[cfg(test)]
    pub(crate) fn peek(
        &self,
        connection_id: &str,
    ) -> Option<crate::acp::termination::AcpTerminationSummaryV1> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(connection_id)
            .map(|entry| entry.termination.clone())
    }

    fn record_suspension_drain_timeout(&self, connection_id: &str) {
        self.record_suspension_drain_timeout_at(connection_id, chrono::Utc::now());
    }

    fn record_suspension_drain_timeout_at(
        &self,
        connection_id: &str,
        observed_at: chrono::DateTime<chrono::Utc>,
    ) {
        let termination = crate::acp::termination::AcpTerminationSummaryV1::new(
            crate::acp::termination::AcpTerminationSource::Session,
            crate::acp::termination::AcpTerminationReason::SuspensionDrainTimeout,
            crate::acp::termination::AcpTerminationClassification::AutomatedAmbiguous,
            true,
            observed_at,
        );
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let preserve_explicit_intent = entries.get(connection_id).is_some_and(|existing| {
            existing.termination.frontend_origin.is_some()
                && existing.termination.frontend_origin
                    != Some(crate::acp::termination::AcpDisconnectOrigin::LegacyUnspecified)
        });
        if !preserve_explicit_intent {
            entries.insert(
                connection_id.to_string(),
                ParentConnectionExitEvidenceEntry {
                    termination,
                    suspension_drain_timeout: true,
                },
            );
        }
    }

    fn take(
        &self,
        connection_id: &str,
    ) -> crate::acp::delegation::continuation::coordinator::ParentConnectionExitCause {
        let entry = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(connection_id);
        let Some(entry) = entry else {
            return crate::acp::delegation::continuation::coordinator::ParentConnectionExitCause::Disconnected {
                termination: crate::acp::termination::AcpTerminationSummaryV1::legacy_unspecified(
                    true, chrono::Utc::now(),
                ),
            };
        };
        if entry.suspension_drain_timeout {
            crate::acp::delegation::continuation::coordinator::ParentConnectionExitCause::SuspensionDrainTimeout {
                termination: entry.termination,
            }
        } else {
            crate::acp::delegation::continuation::coordinator::ParentConnectionExitCause::Disconnected {
                termination: entry.termination,
            }
        }
    }
}

pub(crate) type ParentConnectionExitCauses = ParentConnectionExitEvidence;

fn unexpected_connection_termination(
    source: crate::acp::termination::AcpTerminationSource,
    reason: crate::acp::termination::AcpTerminationReason,
) -> crate::acp::termination::AcpTerminationSummaryV1 {
    crate::acp::termination::AcpTerminationSummaryV1::new(
        source,
        reason,
        crate::acp::termination::AcpTerminationClassification::Unexpected,
        true,
        chrono::Utc::now(),
    )
}

fn record_session_channel_loss(injection: Option<&DelegationInjection>, connection_id: &str) {
    if let Some(injection) = injection {
        injection
            .parent_connection_exit_causes
            .record_session_lost(connection_id, chrono::Utc::now());
    }
}

fn handle_idle_session_update_error(
    _evidence: Option<&ParentConnectionExitEvidence>,
    _connection_id: &str,
    error: &sacp::Error,
) {
    tracing::warn!("[ACP] Ignoring unrecognized session update in idle loop: {error}");
}

/// Typed bootstrap outcome from the connection task to the manager.
/// Only `RouteSpecific` may trigger a root safe-native fallback.
#[derive(Debug)]
pub enum RouteBootstrapOutcome {
    Ready,
    RouteSpecific(crate::acp::delegation::route::RouteDegradedReason),
    Fatal(AcpError),
}

impl RouteBootstrapOutcome {
    pub fn into_acp_error(self) -> AcpError {
        match self {
            Self::Ready => AcpError::Protocol("unexpected Ready as error".into()),
            Self::RouteSpecific(reason) => AcpError::RouteUnavailable { reason },
            Self::Fatal(error) => error,
        }
    }
}

/// Handles returned by [`spawn_agent_connection`] for dedup + route readiness.
pub struct SpawnHandshake {
    pub session_started_rx: tokio::sync::oneshot::Receiver<()>,
    pub route_bootstrap_rx: tokio::sync::oneshot::Receiver<RouteBootstrapOutcome>,
}

/// Locate the `codeg-mcp` companion binary across the supported deployment
/// shapes:
///
/// 1. `CODEG_MCP_BIN` env override — explicit absolute path. Lets dev shells,
///    custom installs, and integration tests point at a freshly compiled
///    binary without touching the install layout.
/// 2. Sibling of the running executable — the production layout for every
///    shipping target. Tauri sidecar (`Contents/MacOS/codeg-mcp` on macOS,
///    next to the desktop executable on Windows and the unix binary on Linux
///    deb/rpm), `install.sh`/`install.ps1` (drops `codeg-mcp` next to
///    `codeg-server`), Docker image (`/usr/local/bin/codeg-mcp` next to
///    `codeg-server`), and `cargo build` dev output
///    (`target/<profile>/codeg-mcp`).
/// 3. `PATH` lookup — last-resort for atypical layouts where ops moved the
///    two binaries apart but kept both reachable on `PATH`.
///
/// Returns `None` when no candidate is an executable file. Callers MUST
/// treat `None` as "delegation is unavailable at this site" and skip
/// injection — never paper over with a phantom path, because that fails
/// inside the agent's MCP spawn loop and may take the entire ACP session
/// down on stricter agents.
pub(crate) fn locate_codeg_mcp_binary() -> Option<PathBuf> {
    let filename = if cfg!(windows) {
        "codeg-mcp.exe"
    } else {
        "codeg-mcp"
    };

    if let Some(raw) = std::env::var_os("CODEG_MCP_BIN") {
        let candidate = PathBuf::from(raw);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let candidate = dir.join(filename);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    which::which(filename)
        .ok()
        .filter(|p| is_executable_file(p))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

/// Append the built-in `codeg-mcp` MCP entry when this agent has at least one
/// applicable companion feature and the binary is present on disk. Returns the
/// registered per-launch token, or `None` when injection was skipped.
///
/// When the binary is missing we log a single-line warning and skip
/// injection rather than register the token + emit a phantom McpServerStdio
/// pointing at a non-existent path. Phantom injection would have made every
/// new ACP session ship a guaranteed-to-fail MCP server entry: stricter
/// agents (Claude Code) refuse the whole session; lax agents lose companion
/// tools silently. Skipping leaves native agent features, including Grok's
/// native question bridge, unaffected.
/// The `--features` value for a companion launch given the feature flags,
/// or `None` when none is enabled (the companion isn't injected at all).
/// Pulled out as a pure function so the inject/skip decision is unit-testable
/// without a real binary on disk or a live broker. `coordination_v1` is only
/// advertised when delegation is on (Join requires the delegation tools).
/// `workflow_v2` is Root-only mutation/recovery (A15.1); never inject for
/// delegation children.
fn companion_features_arg(
    delegation_enabled: bool,
    coordination_v1: bool,
    feedback_enabled: bool,
    ask_enabled: bool,
    sessions_enabled: bool,
    workflow_v2: bool,
) -> Option<String> {
    if !delegation_enabled && !feedback_enabled && !ask_enabled && !sessions_enabled && !workflow_v2
    {
        return None;
    }
    let mut features = Vec::new();
    if delegation_enabled {
        features.push("delegation");
        if coordination_v1 {
            features.push("coordination_v1");
        }
    }
    if feedback_enabled {
        features.push("feedback");
    }
    if ask_enabled {
        features.push("ask");
    }
    if sessions_enabled {
        features.push("sessions");
    }
    if workflow_v2 {
        features.push("workflow_v2");
    }
    Some(features.join(","))
}

/// Apply agent-specific companion policy before building `--features`.
/// Grok already exposes a native blocking question tool through
/// `_x.ai/ask_user_question`, so advertising the companion duplicate wastes
/// catalog bytes and gives the model two routes for the same interaction.
fn companion_features_arg_for_agent(
    agent_type: AgentType,
    delegation_enabled: bool,
    coordination_v1: bool,
    feedback_enabled: bool,
    ask_enabled: bool,
    sessions_enabled: bool,
    workflow_v2: bool,
) -> Option<String> {
    companion_features_arg(
        delegation_enabled,
        coordination_v1,
        feedback_enabled,
        ask_enabled && agent_type != AgentType::Grok,
        sessions_enabled,
        workflow_v2,
    )
}

fn continuation_enabled_for_launch(
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
    agent_type: AgentType,
    env_value: Option<&std::ffi::OsStr>,
) -> bool {
    // Codex + Codeg-route only. Default on; kill-switch via env
    // `CODEG_DELEGATION_CONTINUATION_V1=0` or `false`.
    if !plan.expose_codeg_delegation || agent_type != AgentType::Codex {
        return false;
    }
    match env_value {
        None => true,
        Some(value) => {
            let value = value.to_string_lossy();
            !(value.as_ref() == "0" || value.eq_ignore_ascii_case("false"))
        }
    }
}

/// Outcome of injecting the `codeg-mcp` companion: the per-launch token to
/// stash for revocation, whether feedback was exposed, and an optional ready
/// lease waiter (required only when the plan exposes Codeg delegation).
struct CompanionInjection {
    token: String,
    feedback_available: bool,
    /// Present when `plan.expose_codeg_delegation` — manager/connection wait
    /// on this before emitting Connected.
    delegation_lease: Option<crate::acp::delegation::lease::CompanionLeaseWaiter>,
}

#[allow(clippy::too_many_arguments)]
async fn inject_codeg_mcp(
    servers: &mut Vec<McpServer>,
    injection: &DelegationInjection,
    parent_connection_id: &str,
    working_dir: &Path,
    agent_type: AgentType,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
    connection_incarnation_id: &str,
    binding: Option<&crate::acp::delegation::workflow::WorkflowChildMcpBinding>,
) -> Option<CompanionInjection> {
    // Feature list follows the immutable launch plan for Codeg delegation —
    // never the live Broker settings toggle. Feedback/ask/sessions remain
    // independent launch-time snapshots of their runtime configs.
    let delegation_enabled = plan.expose_codeg_delegation;
    // Join capability is connection-bound and follows Codeg delegation exposure.
    let coordination_v1 = plan.expose_codeg_delegation;
    let delegation_continuation_v1 = continuation_enabled_for_launch(
        plan,
        agent_type,
        std::env::var_os("CODEG_DELEGATION_CONTINUATION_V1").as_deref(),
    );
    let role = if plan.source == crate::acp::delegation::route::DelegationRouteSource::ForcedChild {
        crate::acp::delegation::transport::CompanionRole::DelegationChild
    } else {
        crate::acp::delegation::transport::CompanionRole::Root
    };
    // Manifest v2 and its child completion transport are retired for every new
    // companion launch. Historical direct calls remain server-fenced.
    let workflow_v2 = false;
    let completion_v2 = false;
    let feedback_enabled = injection.feedback.is_enabled().await;
    let ask_enabled = injection.ask.is_enabled().await;
    let sessions_enabled = injection.sessions.is_enabled().await;
    // `None` (no feature enabled) short-circuits the whole injection.
    let mut features_arg = companion_features_arg_for_agent(
        agent_type,
        delegation_enabled,
        coordination_v1,
        feedback_enabled,
        ask_enabled,
        sessions_enabled,
        workflow_v2,
    )?;
    if completion_v2 {
        features_arg.push_str(",completion_v2");
    }
    let Some(binary_path) = locate_codeg_mcp_binary() else {
        tracing::warn!(
            "[delegation][WARN] codeg-mcp companion binary not found (checked CODEG_MCP_BIN, \
             exe sibling, and PATH); skipping companion features {features_arg} for connection \
             {parent_connection_id}. Reinstall codeg or set CODEG_MCP_BIN to fix."
        );
        return None;
    };
    let token = uuid::Uuid::new_v4().to_string();
    injection
        .tokens
        .register(
            token.clone(),
            crate::acp::delegation::listener::TokenEntry {
                parent_connection_id: parent_connection_id.to_string(),
                working_dir: working_dir.to_path_buf(),
                coordination_v1,
                delegation_continuation_v1,
                role,
                workflow_v2,
                completion_v2,
                bound_task_id: binding.map(|binding| binding.task_id.clone()),
            },
        )
        .await;
    // Register the ready lease BEFORE exposing the MCP entry so a fast
    // companion cannot race mark_ready against an unregistered token.
    let delegation_lease = if plan.expose_codeg_delegation {
        Some(injection.leases.register(token.clone()).await)
    } else {
        None
    };
    let role_arg = match role {
        crate::acp::delegation::transport::CompanionRole::Root => "root",
        crate::acp::delegation::transport::CompanionRole::DelegationChild => "delegation_child",
    };
    let mut server = McpServerStdio::new("codeg-mcp", binary_path);
    let mut args = vec![
        "--parent-connection-id".to_string(),
        parent_connection_id.to_string(),
        "--socket-path".to_string(),
        injection.socket_path.to_string_lossy().to_string(),
        "--token".to_string(),
        token.clone(),
        // Self-cleanup watchdog: codeg-mcp exits when this PID is gone so
        // orphaned companions can't keep the binary file locked across an
        // installer upgrade (Windows) or hold a stale broker connection
        // (any platform).
        "--parent-pid".to_string(),
        std::process::id().to_string(),
        // Tool groups to expose this launch (delegation / coordination_v1 /
        // feedback / ask / sessions / workflow_v2).
        "--features".to_string(),
        features_arg,
        "--role".to_string(),
        role_arg.to_string(),
        "--connection-incarnation-id".to_string(),
        connection_incarnation_id.to_string(),
    ];
    // Advertised built-in delegate targets track the user's enable toggles,
    // read fresh at injection time. Custom agents remain outside the closed
    // public delegation schema and are filtered in their separate UI target
    // lists. The subtraction flag is omitted when empty so the companion
    // serves its embedded schema unchanged.
    let disabled = injection
        .agent_availability
        .disabled_agent_wire_slugs()
        .await;
    let disabled_builtins = disabled_builtin_target_args(&disabled);
    if !disabled_builtins.is_empty() {
        args.push("--disabled-agents".to_string());
        args.push(disabled_builtins.join(","));
    }
    server = server.args(args);
    servers.push(McpServer::Stdio(server));
    Some(CompanionInjection {
        token,
        feedback_available: feedback_enabled,
        delegation_lease,
    })
}

/// Build the deterministic companion-side subtraction list for built-ins.
/// Custom agents are intentionally excluded from the public MCP enum.
fn disabled_builtin_target_args(disabled_wire_slugs: &[String]) -> Vec<String> {
    let mut disabled_builtins: Vec<String> = disabled_wire_slugs
        .iter()
        .filter(|slug| !slug.starts_with(crate::models::agent::CUSTOM_AGENT_WIRE_PREFIX))
        .cloned()
        .collect();
    disabled_builtins.sort();
    disabled_builtins.dedup();
    disabled_builtins
}

/// Resolve an MCP server `command` to an absolute path.
///
/// The ACP spec requires `McpServerStdio.command` to be an absolute path.
/// Users typically configure bare names like `npx` / `node` / `bunx`; if we
/// forwarded those verbatim, agents would fail to spawn the server. We try
/// `which` first, fall back to the platform-normalized form (which adds
/// `.exe`/`.cmd` on Windows), and finally to the raw input as last resort.
fn resolve_mcp_command(command: &str) -> PathBuf {
    let path = Path::new(command);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    if let Ok(found) = which::which(command) {
        return found;
    }
    PathBuf::from(crate::process::normalized_program(command))
}

fn canonical_spec_to_mcp_server(name: &str, spec: &serde_json::Value) -> Result<McpServer, String> {
    let obj = spec
        .as_object()
        .ok_or_else(|| "spec must be a JSON object".to_string())?;
    let typ = obj
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("stdio");

    match typ {
        "stdio" => {
            let command = obj
                .get("command")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| "stdio MCP entry missing 'command'".to_string())?;
            // ACP spec requires an absolute path. If users wrote a bare
            // command (e.g. "npx"), resolve it via PATH so the agent can
            // actually spawn the server. Fall back to the raw value when
            // resolution fails — the agent will surface a clearer error.
            let command_path = resolve_mcp_command(command);
            let mut server = McpServerStdio::new(name, command_path);
            if let Some(args) = obj.get("args").and_then(serde_json::Value::as_array) {
                let args: Vec<String> = args
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect();
                if !args.is_empty() {
                    server = server.args(args);
                }
            }
            if let Some(env_obj) = obj.get("env").and_then(serde_json::Value::as_object) {
                let env_vars: Vec<sacp::schema::EnvVariable> = env_obj
                    .iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| sacp::schema::EnvVariable::new(k, s)))
                    .collect();
                if !env_vars.is_empty() {
                    server = server.env(env_vars);
                }
            }
            Ok(McpServer::Stdio(server))
        }
        "http" | "sse" => {
            let url = obj
                .get("url")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| "remote MCP entry missing 'url'".to_string())?;
            let headers: Vec<HttpHeader> = obj
                .get("headers")
                .and_then(serde_json::Value::as_object)
                .map(|map| {
                    map.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| HttpHeader::new(k, s)))
                        .collect()
                })
                .unwrap_or_default();
            if typ == "http" {
                let mut server = McpServerHttp::new(name, url);
                if !headers.is_empty() {
                    server = server.headers(headers);
                }
                Ok(McpServer::Http(server))
            } else {
                let mut server = McpServerSse::new(name, url);
                if !headers.is_empty() {
                    server = server.headers(headers);
                }
                Ok(McpServer::Sse(server))
            }
        }
        other => Err(format!("unsupported MCP transport type '{other}'")),
    }
}

/// Emit the single post-ready companion-unavailable surface: availability event
/// plus secret-free `delegation_unavailable` audit. Does not touch route fields.
async fn emit_post_ready_unavailable(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    connection_id: &str,
    conversation_id: Option<i32>,
    agent_type: AgentType,
) {
    emit_with_state(
        state,
        emitter,
        AcpEvent::DelegationAvailabilityChanged { available: false },
    )
    .await;
    crate::acp::delegation::metrics::DelegationAuditRecord::availability(
        connection_id,
        conversation_id,
        agent_type,
    )
    .emit_availability();
}

/// After ACP session/new|load|resume succeeds: wait for Codeg ready lease when
/// required, emit Connected, signal bootstrap Ready, and monitor post-ready
/// availability (false → one `DelegationAvailabilityChanged` only + one
/// `delegation_unavailable` audit). Never mutates immutable route fields.
async fn finish_route_ready(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
    pending_lease: &mut Option<crate::acp::delegation::lease::CompanionLeaseWaiter>,
    route_bootstrap_tx: &Arc<
        tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<RouteBootstrapOutcome>>>,
    >,
) -> Result<(), sacp::Error> {
    use crate::acp::delegation::lease::ready_lease_timeout;
    use crate::acp::delegation::route::RouteDegradedReason;

    if route_plan.expose_codeg_delegation {
        let Some(mut waiter) = pending_lease.take() else {
            let mut guard = route_bootstrap_tx.lock().await;
            if let Some(tx) = guard.take() {
                let _ = tx.send(RouteBootstrapOutcome::RouteSpecific(
                    RouteDegradedReason::CompanionInitializationFailed,
                ));
            }
            return Err(sacp::util::internal_error(
                "codeg delegation ready lease missing",
            ));
        };
        if waiter.wait_ready(ready_lease_timeout()).await.is_err() {
            let mut guard = route_bootstrap_tx.lock().await;
            if let Some(tx) = guard.take() {
                let _ = tx.send(RouteBootstrapOutcome::RouteSpecific(
                    RouteDegradedReason::CompanionInitializationFailed,
                ));
            }
            return Err(sacp::util::internal_error(
                "codeg delegation ready lease failed",
            ));
        }
        {
            let mut s = state.write().await;
            s.set_delegation_available(true);
        }
        // Post-ready close: flip availability only; one event; no route change.
        // Check current value first so a close that races before the monitor
        // starts still emits exactly once (watch::changed misses past values).
        let mut availability = waiter.availability();
        let state_m = Arc::clone(state);
        let emitter_m = emitter.clone();
        // Capture ids for the availability audit before the monitor task;
        // route plan fields stay immutable. Metrics Arc is not recreated here —
        // availability is an audit-only surface (no counter); injection metrics
        // remain the single process-wide instance elsewhere.
        let (audit_connection_id, audit_conversation_id, audit_agent_type) = {
            let s = state.read().await;
            (s.connection_id.clone(), s.conversation_id, s.agent_type)
        };
        tokio::spawn(async move {
            loop {
                if !*availability.borrow() {
                    emit_post_ready_unavailable(
                        &state_m,
                        &emitter_m,
                        &audit_connection_id,
                        audit_conversation_id,
                        audit_agent_type,
                    )
                    .await;
                    break;
                }
                if availability.changed().await.is_err() {
                    // Sender dropped while still true — treat as unavailable.
                    if *availability.borrow() {
                        emit_post_ready_unavailable(
                            &state_m,
                            &emitter_m,
                            &audit_connection_id,
                            audit_conversation_id,
                            audit_agent_type,
                        )
                        .await;
                    }
                    break;
                }
            }
        });
    }

    emit_with_state(
        state,
        emitter,
        AcpEvent::StatusChanged {
            status: ConnectionStatus::Connected,
        },
    )
    .await;

    let mut guard = route_bootstrap_tx.lock().await;
    if let Some(tx) = guard.take() {
        let _ = tx.send(RouteBootstrapOutcome::Ready);
    }
    Ok(())
}

/// The main ACP connection loop.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "connection",
    skip_all,
    fields(
        connection_id = %connection_id,
        agent_type = ?agent_type,
        working_dir = ?working_dir,
        session_id = ?session_id,
    )
)]
async fn run_connection(
    agent: AcpAgent,
    connection_id: String,
    agent_type: AgentType,
    working_dir: Option<String>,
    session_id: Option<String>,
    mut cmd_rx: mpsc::Receiver<ConnectionCommand>,
    mut control_rx: mpsc::Receiver<ConnectionControl>,
    mut cmd_liveness_rx: watch::Receiver<bool>,
    mut control_liveness_rx: watch::Receiver<bool>,
    emitter: EventEmitter,
    state: Arc<RwLock<SessionState>>,
    terminal_base_env: BTreeMap<String, String>,
    terminal_shell: crate::terminal::shell::ResolvedShellSnapshot,
    preferred_mode_id: Option<String>,
    preferred_config_values: BTreeMap<String, String>,
    delegation_injection: Option<DelegationInjection>,
    workflow_child_mcp_binding: Option<crate::acp::delegation::workflow::WorkflowChildMcpBinding>,
    connection_incarnation_id: String,
    route_plan: crate::acp::delegation::route::DelegationRoutePlan,
    route_bootstrap_tx: tokio::sync::oneshot::Sender<RouteBootstrapOutcome>,
    session_attach_mode: crate::acp::session_attach::SessionAttachMode,
    fs_policy: FsAccessPolicy,
) -> Result<(), AcpError> {
    let parent_connection_exit_evidence = delegation_injection
        .as_ref()
        .map(|injection| Arc::clone(&injection.parent_connection_exit_causes));
    let evidence_connection_id = connection_id.clone();
    // Shared so nested session paths can complete bootstrap exactly once.
    let route_bootstrap_tx = Arc::new(tokio::sync::Mutex::new(Some(route_bootstrap_tx)));
    let pending_perms: PendingPermissions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    // `terminal_base_env` already filtered to just the credential helper
    // keys upstream — see `spawn_agent_connection` for the rationale and
    // why we don't forward the full agent runtime_env here.
    //
    // `terminal_shell` is the immutable launch snapshot; the runtime uses its
    // `ResolvedShellSpec` for ShellCommandLine execution and diagnostics and
    // never re-reads system terminal settings after the connection starts.
    let cwd = resolve_working_dir(working_dir.as_deref());
    // Default terminals to the session working directory so an agent that calls
    // `terminal/create` without a `cwd` (e.g. CodeBuddy) runs in the folder the
    // conversation runs in rather than codeg's own process cwd.
    let terminal_runtime = Arc::new(
        TerminalRuntime::new(
            terminal_base_env,
            terminal_shell.spec.clone(),
            adapter_for(agent_type),
        )
        .with_default_cwd(Some(cwd.clone())),
    );
    // Grok's ACP terminal adapter creates client terminals but omits
    // ToolCallContent::Terminal. This bridge synthesizes the association
    // only when a unique in-progress shell tool is the sole candidate.
    let terminal_assoc = Arc::new(std::sync::Mutex::new(TerminalAssocFallback::new(
        agent_type == AgentType::Grok,
    )));
    let cwd_string = cwd.to_string_lossy().to_string();
    tracing::info!("[ACP] fs policy {}", fs_policy.describe());
    let file_system_runtime = Arc::new(
        FileSystemRuntime::with_policy(fs_policy).with_allow_outside_workspace(
            initial_allow_outside_workspace(
                agent_type,
                preferred_mode_id.as_deref(),
                &preferred_config_values,
            ),
        ),
    );

    let conn_id = connection_id.clone();
    let emitter_clone = emitter.clone();
    let perms = pending_perms.clone();
    let state_outer = Arc::clone(&state);

    // Grok's native `ask_user_question` (verified against 0.2.101) arrives as an
    // `_x.ai/ask_user_question` ACP ext request that BLOCKS on the reply — rather
    // than the codeg-mcp tool. Capture the shared question access + feature toggle
    // (both live on the delegation injection) so the ext handler can register the
    // questions through the SAME interactive-card pipeline and answer grok once the
    // user submits. `None` when the companion isn't injected — the handler then
    // lets grok fall back to its inert rendering.
    let grok_ask_access = delegation_injection
        .as_ref()
        .map(|inj| (Arc::clone(&inj.questions), inj.ask.clone()));
    let grok_ask_conn_id = connection_id.clone();
    // Grok `exit_plan_mode` bridge access — always wired in production (native
    // plan mode, no feature flag). `None` only on the test paths that spin up
    // `run_connection` without a delegation stack; the handler then replies
    // disconnect and grok keeps plan mode active.
    let grok_plan_access = delegation_injection
        .as_ref()
        .map(|inj| Arc::clone(&inj.plan_approvals));
    let grok_plan_conn_id = connection_id.clone();
    // The ext handler emits the answered in-stream card (`AskQuestionResultCard`)
    // itself once the user submits — grok never emits a completed tool result into
    // the ACP stream — so it needs this connection's session state + emitter.
    let grok_ask_state = Arc::clone(&state);
    let grok_ask_emitter = emitter.clone();

    // Claude-only: tail this connection's session transcript for OUT-OF-TURN
    // activity (async sub-agent / background-shell completions, the agent's
    // continued work after them, cron//loop autonomous turns — none of which
    // the wire reliably represents) and surface it as `BackgroundActivity`
    // events; also feeds the keep-alive accounting that exempts the
    // connection from the idle sweeps while such work is pending. Created
    // HERE — per CONNECTION, not per conversation loop — so ONE watcher (and
    // one prompt ledger) spans fork restarts: `run_watch` observes the
    // session-id change and re-arms in place, carrying still-outstanding
    // tasks and settled ids across the fork (a post-fork `SendMessage`
    // resume must re-arm the keep-alive). The guard aborts the watcher when
    // this connection ends. Its spawn epoch (captured before the session
    // exists) is what lets the first arm process records written before the
    // transcript file is discovered.
    let prompt_ledger = background_watch::PromptLedger::shared();
    // Wire-only first-prompt shell context: one-shot per spawned process.
    // Shared across fork restarts of this conversation loop; browser/transport
    // reattachment reuses the live AgentConnection and never re-enters here.
    let terminal_prompt_context = Arc::new(TerminalPromptContext::new(terminal_shell.spec.clone()));
    // Hidden generation (title/translate) must not start background transcript watchers.
    let is_hidden_generation = {
        let s = state.read().await;
        s.purpose.is_hidden_generation()
    };
    let _bg_watch = if is_hidden_generation {
        None
    } else {
        background_watch::spawn_if_claude(
            &connection_id,
            agent_type,
            Arc::clone(&state),
            emitter.clone(),
            cwd_string.clone(),
            Arc::clone(&prompt_ledger),
        )
    };

    let connect_with_result = Client
        .builder()
        .name("codeg")
        .on_receive_request(
            {
                let emitter_inner = emitter_clone.clone();
                let perms = perms.clone();
                let perm_cwd = cwd_string.clone();
                let state_inner = Arc::clone(&state);
                async move |req: RequestPermissionRequest,
                            responder: Responder<RequestPermissionResponse>,
                            _cx: ConnectionTo<Agent>| {
                    handle_permission_request(
                        &state_inner,
                        &emitter_inner,
                        &perms,
                        &perm_cwd,
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = file_system_runtime.clone();
                async move |req: ReadTextFileRequest,
                            responder: Responder<ReadTextFileResponse>,
                            _cx: ConnectionTo<Agent>| {
                    respond_file_system_request(responder, runtime.read_text_file(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = file_system_runtime.clone();
                async move |req: WriteTextFileRequest,
                            responder: Responder<WriteTextFileResponse>,
                            _cx: ConnectionTo<Agent>| {
                    respond_file_system_request(responder, runtime.write_text_file(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                let assoc = terminal_assoc.clone();
                async move |req: CreateTerminalRequest,
                            responder: Responder<CreateTerminalResponse>,
                            _cx: ConnectionTo<Agent>| {
                    let session_id = req.session_id.to_string();
                    let result = runtime.create_terminal(req).await;
                    if let Ok(ref response) = result {
                        if let Ok(mut bridge) = assoc.lock() {
                            if let Some(tool_call_id) = bridge
                                .on_terminal_created(&session_id, &response.terminal_id.to_string())
                            {
                                tracing::debug!(
                                    target: "acp::terminal_assoc",
                                    session_id = %session_id,
                                    terminal_id = %response.terminal_id,
                                    tool_call_id = %tool_call_id,
                                    "fallback-bound client terminal to shell tool call"
                                );
                            }
                        }
                    }
                    respond_terminal_request(responder, result)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: TerminalOutputRequest,
                            responder: Responder<TerminalOutputResponse>,
                            _cx: ConnectionTo<Agent>| {
                    respond_terminal_request(responder, runtime.terminal_output(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: WaitForTerminalExitRequest,
                            responder: Responder<WaitForTerminalExitResponse>,
                            cx: ConnectionTo<Agent>| {
                    // `terminal/wait_for_exit` blocks until the command exits,
                    // and sacp awaits request handlers INSIDE its single
                    // dispatch loop ("the loop awaits the handler to completion
                    // before processing the next message"). Answering inline
                    // therefore freezes the ENTIRE connection for a command
                    // that never exits — an agent that backgrounds a dev server
                    // and then monitors it (grok does exactly this) would stall
                    // the turn forever, with every later session/update stuck
                    // unprocessed in the transport queue.
                    //
                    // Answer from a spawned task instead — sacp's own sanctioned
                    // escape hatch. `cx.spawn` rather than `tokio::spawn` so the
                    // wait is connection-scoped and torn down with it.
                    let runtime = runtime.clone();
                    cx.spawn(async move {
                        let result = runtime.wait_for_terminal_exit(req).await;
                        if let Err(err) = respond_terminal_request(responder, result) {
                            // Propagating this would tear down the whole
                            // connection, and a failed send only means the peer
                            // is already gone.
                            tracing::warn!(
                                "[ACP] failed to answer terminal/wait_for_exit: {err}"
                            );
                        }
                        Ok(())
                    })?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: KillTerminalRequest,
                            responder: Responder<KillTerminalResponse>,
                            _cx: ConnectionTo<Agent>| {
                    respond_terminal_request(responder, runtime.kill_terminal(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let runtime = terminal_runtime.clone();
                async move |req: ReleaseTerminalRequest,
                            responder: Responder<ReleaseTerminalResponse>,
                            _cx: ConnectionTo<Agent>| {
                    respond_terminal_request(responder, runtime.release_terminal(req).await)?;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let access = grok_ask_access.clone();
                let conn_id = grok_ask_conn_id.clone();
                let card_state = Arc::clone(&grok_ask_state);
                let card_emitter = grok_ask_emitter.clone();
                async move |req: GrokAskUserQuestionRequest,
                            responder: Responder<serde_json::Value>,
                            _cx: ConnectionTo<Agent>| {
                    handle_grok_ask_user_question(
                        &access,
                        &conn_id,
                        &card_state,
                        &card_emitter,
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let access = grok_plan_access.clone();
                let conn_id = grok_plan_conn_id.clone();
                async move |req: GrokExitPlanModeRequest,
                            responder: Responder<serde_json::Value>,
                            _cx: ConnectionTo<Agent>| {
                    handle_grok_exit_plan_mode(&access, &conn_id, req, responder).await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                // Codex `elicitation/create`: question-style requests (Plan
                // mode `request_user_input`, generic MCP forms) bridge into
                // the same ask card as the codeg-mcp ask tool (reusing the ask
                // access + kill switch); approval-style requests (MCP
                // tool-call approvals, message-only confirms) route through
                // the permission card via `pending_perms`.
                let access = grok_ask_access.clone();
                let conn_id = grok_ask_conn_id.clone();
                let perms = perms.clone();
                let state_inner = Arc::clone(&state);
                let emitter_inner = emitter_clone.clone();
                async move |req: CodexElicitationRequest,
                            responder: Responder<serde_json::Value>,
                            _cx: ConnectionTo<Agent>| {
                    handle_elicitation_request(
                        &access,
                        &perms,
                        &state_inner,
                        &emitter_inner,
                        &conn_id,
                        req,
                        responder,
                    )
                    .await;
                    Ok(())
                }
            },
            on_receive_request!(),
        )
        .connect_with(agent, {
            let route_bootstrap_tx = Arc::clone(&route_bootstrap_tx);
            async move |cx| -> Result<(), sacp::Error> {
            let state = state_outer;
            let agent_name_for_log = registry::get_agent_meta(agent_type).name;

            // Advertise filesystem, terminal and Codex elicitation capabilities
            // while preserving the connection's terminal snapshot metadata.
            let init_request = build_initialize_request(
                agent_type,
                &terminal_shell.spec,
                adapter_for(agent_type),
            )
            .map_err(|e| sacp::util::internal_error(e.to_string()))?;
            // Bound the Initialize handshake so an outdated / incompatible
            // cached binary that never responds can't leave the frontend
            // stuck on "Connecting...". A healthy agent answers in <1s; we
            // give 60s headroom for cold process startup on slow machines.
            //
            // We cannot carry a structured error code through sacp's Error
            // type, so we tag the timeout with `INIT_TIMEOUT_SENTINEL` and
            // convert it back to `AcpError::InitializeTimeout` in the
            // outer `.map_err(...)` below. The outer layer attaches a
            // stable `code` to the frontend event so it can be localized.
            tracing::info!(
                "[ACP][{agent_name_for_log}] Sending Initialize (protocol={}, timeout=60s)",
                ProtocolVersion::LATEST
            );
            let init_started = std::time::Instant::now();
            let init_resp = match tokio::time::timeout(
                std::time::Duration::from_secs(60),
                cx.send_request_to(Agent, init_request).block_task(),
            )
            .await
            {
                Ok(Ok(resp)) => {
                    tracing::info!(
                        "[ACP][{agent_name_for_log}] Initialize responded in {:?}",
                        init_started.elapsed()
                    );
                    resp
                }
                Ok(Err(e)) => {
                    tracing::error!(
                        "[ACP][{agent_name_for_log}] Initialize failed in {:?}: {e}",
                        init_started.elapsed()
                    );
                    return Err(e);
                }
                Err(_) => {
                    tracing::error!(
                        "[ACP][{agent_name_for_log}] Initialize TIMED OUT after {:?} \
                         — the agent never answered the handshake. Check the \
                         [stderr] lines above for agent-side errors. For a full \
                         JSON-RPC trace, re-launch with CODEG_ACP_DEBUG=1.",
                        init_started.elapsed()
                    );
                    return Err(sacp::util::internal_error(INIT_TIMEOUT_SENTINEL));
                }
            };
            emit_prompt_capabilities(
                &state,
                &emitter_clone,
                &init_resp.agent_capabilities.prompt_capabilities,
            )
            .await;

            let supports_fork = init_resp
                .agent_capabilities
                .session_capabilities
                .fork
                .is_some();
            let supports_resume = init_resp
                .agent_capabilities
                .session_capabilities
                .resume
                .is_some();
            tracing::info!(
                "[ACP] Agent capabilities: load_session={}, fork={}, resume={}",
                init_resp.agent_capabilities.load_session, supports_fork, supports_resume
            );

            // Whether this agent accepts MCP server entries over the ACP wire
            // (`session/new`'s `mcpServers`). This is the single chokepoint
            // feeding session/new, session/load, and the load→new fallback.
            // See `AcpAgentMeta::supports_mcp`.
            let agent_supports_mcp = registry::get_agent_meta(agent_type).supports_mcp;

            // Load MCP servers configured for this agent and filter by the
            // capabilities the agent just declared. Stdio is mandatory per
            // ACP spec; HTTP/SSE are gated on `mcp_capabilities.{http,sse}`.
            let mut mcp_servers: Vec<McpServer> = if agent_supports_mcp {
                let mcp_caps = &init_resp.agent_capabilities.mcp_capabilities;
                load_mcp_servers_for_agent(agent_type)
                    .into_iter()
                    .filter(|s| match s {
                        McpServer::Stdio(_) => true,
                        McpServer::Http(server) => {
                            if mcp_caps.http {
                                true
                            } else {
                                tracing::warn!(
                                    "[ACP][{}] skip HTTP MCP server '{}': agent does not advertise mcpCapabilities.http",
                                    agent_type, server.name
                                );
                                false
                            }
                        }
                        McpServer::Sse(server) => {
                            if mcp_caps.sse {
                                true
                            } else {
                                tracing::warn!(
                                    "[ACP][{}] skip SSE MCP server '{}': agent does not advertise mcpCapabilities.sse",
                                    agent_type, server.name
                                );
                                false
                            }
                        }
                        _ => false,
                    })
                    .collect()
            } else {
                tracing::info!(
                    "[ACP][{}] supports_mcp=false: skipping all MCP wire forwarding (user servers + codeg-mcp companion)",
                    agent_type
                );
                Vec::new()
            };

            // Inject the built-in `codeg-mcp` MCP server. Stdio is
            // unconditionally supported by the ACP wire — no `mcp_caps`
            // filter needed. The returned token is stashed on the session
            // state so connection teardown can revoke it. Skipped entirely
            // for agents that don't accept MCP over the wire (above).
            let mut delegate_injection =
                if agent_supports_mcp && agent_delivers_wire_mcp(agent_type) {
                    if let Some(inj) = delegation_injection.as_ref() {
                        inject_codeg_mcp(
                            &mut mcp_servers,
                            inj,
                            &conn_id,
                            &cwd,
                            agent_type,
                            &route_plan,
                            &connection_incarnation_id,
                            workflow_child_mcp_binding.as_ref(),
                        )
                        .await
                    } else {
                        None
                    }
                } else {
                    None
                };
            if let Some(ref injected) = delegate_injection {
                let mut s = state.write().await;
                s.delegation_token = Some(injected.token.clone());
                // The agent's actual feedback capability for this session — the
                // authoritative gate for submit + UI, fixed at launch.
                s.feedback_tool_available = injected.feedback_available;
            }
            // Take the lease waiter out so we can wait on it after ACP session.
            let mut pending_lease = delegate_injection
                .as_mut()
                .and_then(|i| i.delegation_lease.take());

            // Emit fork support capability
            emit_with_state(
                &state,
                &emitter_clone,
                AcpEvent::ForkSupported {
                    supported: supports_fork,
                },
            )
            .await;

            // Connected is deferred until ACP session succeeds AND (for Codeg
            // delegation routes) the authenticated ready lease is ready.
            // Prompts sent before run_conversation_loop are still buffered in
            // cmd_rx and processed as soon as the loop starts.

            // Launch purpose is fixed for the connection lifetime (title /
            // translate stamp a restrictive Grok agentProfile on session meta).
            let purpose = state.read().await.purpose;

            // ResumeExistingOnly must never fall through to session/new — including
            // when the caller omitted a session id entirely.
            if session_attach_mode.is_resume_existing_only()
                && !crate::acp::session_attach::resume_existing_has_session_id(
                    session_id.as_deref(),
                )
            {
                tracing::warn!(
                    "[ACP] resume_existing_only refused: missing session_id; \
                     never session/new"
                );
                refuse_unresumable_bootstrap(
                    &state,
                    &emitter_clone,
                    "",
                    "resume_existing_only: session_id required; refusing session/new"
                        .to_string(),
                    delegation_injection
                        .as_ref()
                        .map(|inj| inj.broker.as_ref()),
                    &connection_id,
                )
                .await;
                return Ok(());
            }

            if let Some(sid) = session_id {
                // Prefer session/resume when the agent advertises the
                // capability: it restores session context WITHOUT replaying
                // history (which session/load does only for us to drain and
                // discard — the transcript the user sees comes from the disk
                // parser, not the ACP wire). On any non-terminal resume failure
                // we fall through to the session/load block below, so the
                // effective chain is resume → load → new (Default) or
                // resume → load only (ResumeExistingOnly).
                if supports_resume {
                    let resume_req = match build_resume_session_request(
                        agent_type,
                        SessionId::new(sid.clone()),
                        &cwd,
                        mcp_servers.clone(),
                        &terminal_shell.spec,
                        adapter_for(agent_type),
                        &route_plan,
                        purpose,
                    ) {
                        Ok(r) => r,
                        Err(e) => {
                            return Err(
                                bridge_acp_err_for_bootstrap(e, &route_bootstrap_tx).await
                            );
                        }
                    };
                    match send_resume_session(&cx, resume_req).await {
                        Ok((resume_resp, grok_models_raw, returned_session_id)) => {
                            // Gate SessionStarted before lifecycle can rewrite
                            // conversation.external_id (esp. ResumeExistingOnly).
                            match crate::acp::session_attach::gate_session_started_for_attach(
                                session_attach_mode,
                                &sid,
                                returned_session_id.as_deref(),
                            ) {
                                crate::acp::session_attach::SessionStartedDecision::Emit {
                                    session_id: emit_sid,
                                } => {
                                    // Keep attach on the verified/expected id —
                                    // never rewrite to a divergent agent id.
                                    let _ = emit_sid;
                                }
                                crate::acp::session_attach::SessionStartedDecision::RefuseUnresumable {
                                    reason,
                                } => {
                                    tracing::warn!(
                                        expected = %sid,
                                        returned = ?returned_session_id,
                                        "[ACP] resume identity refuse: {reason}"
                                    );
                                    // Drop the attached incarnation without
                                    // emitting SessionStarted (no external_id
                                    // rewrite) and without entering the prompt loop.
                                    refuse_unresumable_bootstrap(
                                        &state,
                                        &emitter_clone,
                                        &sid,
                                        format!("resume_existing_only: {reason}"),
                                        delegation_injection
                                            .as_ref()
                                            .map(|inj| inj.broker.as_ref()),
                                        &connection_id,
                                    )
                                    .await;
                                    return Ok(());
                                }
                            }
                            let initial_config_options = resume_resp.config_options.clone();
                            let new_resp = NewSessionResponse::new(SessionId::new(sid.clone()))
                                .modes(resume_resp.modes)
                                .config_options(resume_resp.config_options)
                                .meta(resume_resp.meta);
                            let grok_meta = if agent_type == AgentType::Grok {
                                new_resp.meta.clone()
                            } else {
                                None
                            };
                            // Opportunistic: grok may include per-model effort data
                            // on resume; absent ⇒ empty specs ⇒ flat fallback.
                            let grok_effort_specs = (agent_type == AgentType::Grok)
                                .then(|| parse_grok_effort_specs(grok_models_raw.as_ref()));
                            let mut session = cx.attach_session(new_resp, Default::default())?;

                            // No drain: session/resume does not replay history,
                            // so there is nothing to discard. Any buffered
                            // notification (e.g. an early AvailableCommandsUpdate)
                            // is consumed and forwarded by run_conversation_loop.

                            // Publish SessionStarted only after identity gate.
                            // Continue-path callers pass the conversation's durable
                            // external_id as `session_id`; we never rewrite to a
                            // mismatched agent-returned id.
                            record_transcript_header(agent_type, &sid, &cwd.to_string_lossy());
                            emit_with_state(
                                &state,
                                &emitter_clone,
                                AcpEvent::SessionStarted {
                                    session_id: sid.clone(),
                                },
                            )
                            .await;
                            emit_session_modes(&state, &emitter_clone, session.modes()).await;
                            apply_and_emit_session_config_options(
                                &cx,
                                &mut session,
                                &state,
                                &emitter_clone,
                                agent_type,
                                grok_meta.as_ref(),
                                grok_effort_specs.as_ref(),
                                preferred_mode_id.as_deref(),
                                &preferred_config_values,
                                initial_config_options.unwrap_or_default(),
                                file_system_runtime.as_ref(),
                            )
                            .await;
                            emit_selectors_ready(&state, &emitter_clone).await;
                            finish_route_ready(
                                &state,
                                &emitter_clone,
                                &route_plan,
                                &mut pending_lease,
                                &route_bootstrap_tx,
                            )
                            .await?;

                            let loop_result = run_conversation_loop(
                                &mut session,
                                &conn_id,
                                &emitter_clone,
                                &state,
                                agent_type,
                                &perms,
                                &mut cmd_rx,
                                &mut control_rx,
                                &mut cmd_liveness_rx,
                                &mut control_liveness_rx,
                                terminal_runtime.clone(),
                                terminal_assoc.clone(),
                                file_system_runtime.clone(),
                                &cwd_string,
                                supports_fork,
                                &terminal_shell.spec,
                                &route_plan,
                                &prompt_ledger,
                                &terminal_prompt_context,
                                delegation_injection.as_ref(),
                            )
                            .await;
                            terminal_runtime.release_all_for_session(&sid).await;
                            if let Ok(mut bridge) = terminal_assoc.lock() {
                                bridge.clear_session(&sid);
                            }
                            drop(session);
                            // Explicit return: this arm is NOT in tail position
                            // (the session/load block follows it), so without
                            // `return` a successful resume would fall into
                            // session/load.
                            return handle_fork_or_exit(
                                loop_result,
                                &conn_id,
                                &emitter_clone,
                                &state,
                                agent_type,
                                &perms,
                                &mut cmd_rx,
                                &mut control_rx,
                                &mut cmd_liveness_rx,
                                &mut control_liveness_rx,
                                terminal_runtime.clone(),
                                terminal_assoc.clone(),
                                file_system_runtime.clone(),
                                &cwd,
                                &cwd_string,
                                &terminal_shell.spec,
                                &route_plan,
                                &prompt_ledger,
                                &terminal_prompt_context,
                                delegation_injection.as_ref(),
                            )
                            .await;
                        }
                        Err(e) => {
                            // resume is unstable and NOT guaranteed equivalent to
                            // session/load, so a resume-specific failure must
                            // never deny a load that might still succeed. EVERY
                            // resume error — ResourceNotFound, "Authentication
                            // required", "Method not found", or anything else —
                            // falls through to the session/load block below,
                            // which owns terminal decisions: under
                            // ResumeExistingOnly, refuse_unresumable_bootstrap;
                            // under Default, SessionLoadFailed for not-found /
                            // silent stop for auth / session/new otherwise. No
                            // user-facing event is emitted here: load re-derives
                            // the same outcome a moment later, so emitting now
                            // would double up (not-found) or flash a transient
                            // error that self-heals when load succeeds.
                            tracing::warn!(
                                "[ACP] session/resume failed ({e}); falling back to session/load"
                            );
                            // fall through to the session/load block below
                        }
                    }
                }

                // Load existing session via session/load.
                //
                // ACP is explicit that a client MUST NOT send `session/load` to
                // an agent that has not advertised `loadSession` (Zed enforces
                // the same gate). Skipping the RPC lands on exactly the
                // recovery its wire error would have taken — `session/new` plus
                // a `continues_from` link, so a custom agent's conversation
                // still reads as one history — without putting an unsupported
                // method on the wire.
                //
                // Only a declared **false** is trusted. A declared true is not:
                // agents that advertise `loadSession: true` and then answer
                // "Method not found" are real, so the whole error ladder below
                // stays exactly as it was.
                let attempted_load = init_resp.agent_capabilities.load_session;
                let load_result = if attempted_load {
                    let load_req = match build_load_session_request(
                        agent_type,
                        SessionId::new(sid.clone()),
                        &cwd,
                        mcp_servers.clone(),
                        &terminal_shell.spec,
                        adapter_for(agent_type),
                        &route_plan,
                        purpose,
                    ) {
                        Ok(request) => request,
                        Err(error) => {
                            return Err(
                                bridge_acp_err_for_bootstrap(error, &route_bootstrap_tx).await
                            );
                        }
                    };
                    // Capture the raw session id so ResumeExistingOnly can
                    // verify identity before SessionStarted.
                    send_load_session_capturing_id(&cx, load_req).await
                } else {
                    Err(sacp::Error::method_not_found()
                        .data("agent does not advertise the loadSession capability"))
                };

                match load_result {
                    Ok((load_resp, returned_session_id)) => {
                        match crate::acp::session_attach::gate_session_started_for_attach(
                            session_attach_mode,
                            &sid,
                            returned_session_id.as_deref(),
                        ) {
                            crate::acp::session_attach::SessionStartedDecision::Emit { .. } => {}
                            crate::acp::session_attach::SessionStartedDecision::RefuseUnresumable {
                                reason,
                            } => {
                                tracing::warn!(
                                    expected = %sid,
                                    returned = ?returned_session_id,
                                    "[ACP] load identity refuse: {reason}"
                                );
                                refuse_unresumable_bootstrap(
                                    &state,
                                    &emitter_clone,
                                    &sid,
                                    format!("resume_existing_only: {reason}"),
                                    delegation_injection
                                        .as_ref()
                                        .map(|inj| inj.broker.as_ref()),
                                    &connection_id,
                                )
                                .await;
                                return Ok(());
                            }
                        }
                        let initial_config_options = load_resp.config_options.clone();
                        let new_resp = NewSessionResponse::new(SessionId::new(sid.clone()))
                            .modes(load_resp.modes)
                            .config_options(load_resp.config_options)
                            .meta(load_resp.meta);
                        let grok_meta = if agent_type == AgentType::Grok {
                            new_resp.meta.clone()
                        } else {
                            None
                        };
                        let mut session = cx.attach_session(new_resp, Default::default())?;

                        // Drain historical replay notifications from session/load,
                        // but forward AvailableCommandsUpdate to the frontend.
                        //
                        // For a custom agent with no transcript yet — a session
                        // created outside codeg, or one whose recording was
                        // lost — this replay is the ONLY source of its history,
                        // so capture it instead of discarding it. When codeg
                        // already recorded the session live, the replay is a
                        // duplicate and stays drained.
                        let hydrate_from_replay = transcript_dir_for(agent_type).is_some_and(|dir| {
                            !crate::acp_transcript::has_recorded_history(dir, &sid)
                        });
                        if hydrate_from_replay {
                            tracing::info!(
                                "[ACP] hydrating custom agent transcript for {sid} from session/load replay"
                            );
                        }
                        // The header must land BEFORE any replayed entry:
                        // `record_header` is a no-op once the file is non-empty,
                        // so writing it after the drain would leave a hydrated
                        // transcript permanently headerless (no cwd, no start
                        // time, hence no folder in the conversation list).
                        record_transcript_header(agent_type, &sid, &cwd.to_string_lossy());
                        let mut drained = 0u32;
                        // Cleared if the writer ever stalls: from then on the
                        // drain still runs to completion (the session is not
                        // usable until the replay is consumed) but records
                        // nothing more, so the transcript ends at a line
                        // boundary instead of growing holes.
                        let mut recording = hydrate_from_replay;
                        while let Ok(Ok(msg)) = tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            session.read_update(),
                        )
                        .await
                        {
                            drained += 1;
                            if let SessionMessage::SessionMessage(dispatch) = msg {
                                let h = emitter_clone.clone();
                                let st = Arc::clone(&state);
                                let dispatch = fix_usage_update_nulls(dispatch);
                                let _ = MatchDispatch::new(dispatch)
                                    .if_notification(async |notif: SessionNotification| {
                                        if recording {
                                            recording = record_hydrated_update(
                                                agent_type,
                                                &sid,
                                                &notif.update,
                                            )
                                            .await;
                                        }
                                        if matches!(
                                            notif.update,
                                            SessionUpdate::AvailableCommandsUpdate(_)
                                        ) {
                                            // Historical-replay path only
                                            // forwards AvailableCommandsUpdate,
                                            // which never carries tool output or
                                            // tool-call titles — throwaway state
                                            // is fine.
                                            let mut replay_cache =
                                                ToolCallOutputCache::default();
                                            let mut replay_cb_state =
                                                CodeBuddyLiveState::default();
                                            emit_conversation_update(
                                                &st,
                                                &h,
                                                agent_type,
                                                notif.update,
                                                None,
                                                &mut replay_cache,
                                                &mut replay_cb_state,
                                                None,
                                            )
                                            .await;
                                        }
                                        Ok(())
                                    })
                                    .await
                                    .otherwise(async |dispatch| {
                                        // Historical replay: never counts as
                                        // agent activity for the soft watchdog.
                                        maybe_emit_ext_notification(
                                            &st,
                                            &h,
                                            agent_type,
                                            dispatch,
                                        )
                                        .await;
                                        Ok(())
                                    })
                                    .await;
                            }
                        }
                        if drained > 0 {
                            tracing::info!("[ACP] Drained {drained} historical replay notifications");
                        }

                        emit_with_state(
                            &state,
                            &emitter_clone,
                            AcpEvent::SessionStarted {
                                session_id: sid.clone(),
                            },
                        )
                        .await;
                        emit_session_modes(&state, &emitter_clone, session.modes()).await;
                        apply_and_emit_session_config_options(
                            &cx,
                            &mut session,
                            &state,
                            &emitter_clone,
                            agent_type,
                            grok_meta.as_ref(),
                            // `session/load` is a typed send with no raw `models`
                            // capture, so effort stays on the flat fallback.
                            None,
                            preferred_mode_id.as_deref(),
                            &preferred_config_values,
                            initial_config_options.unwrap_or_default(),
                            file_system_runtime.as_ref(),
                        )
                        .await;
                        emit_selectors_ready(&state, &emitter_clone).await;
                        finish_route_ready(
                            &state,
                            &emitter_clone,
                            &route_plan,
                            &mut pending_lease,
                            &route_bootstrap_tx,
                        )
                        .await?;

                        let loop_result = run_conversation_loop(
                            &mut session,
                            &conn_id,
                            &emitter_clone,
                            &state,
                            agent_type,
                            &perms,
                            &mut cmd_rx,
                            &mut control_rx,
                            &mut cmd_liveness_rx,
                            &mut control_liveness_rx,
                            terminal_runtime.clone(),
                            terminal_assoc.clone(),
                            file_system_runtime.clone(),
                            &cwd_string,
                            supports_fork,
                            &terminal_shell.spec,
                            &route_plan,
                            &prompt_ledger,
                            &terminal_prompt_context,
                            delegation_injection.as_ref(),
                        )
                        .await;
                        terminal_runtime.release_all_for_session(&sid).await;
                        if let Ok(mut bridge) = terminal_assoc.lock() {
                            bridge.clear_session(&sid);
                        }
                        drop(session);
                        handle_fork_or_exit(
                            loop_result,
                            &conn_id,
                            &emitter_clone,
                            &state,
                            agent_type,
                            &perms,
                            &mut cmd_rx,
                            &mut control_rx,
                            &mut cmd_liveness_rx,
                            &mut control_liveness_rx,
                            terminal_runtime.clone(),
                            terminal_assoc.clone(),
                            file_system_runtime.clone(),
                            &cwd,
                            &cwd_string,
                            &terminal_shell.spec,
                            &route_plan,
                            &prompt_ledger,
                            &terminal_prompt_context,
                            delegation_injection.as_ref(),
                        )
                        .await
                    }
                    Err(e) => {
                        // session/load failed. Disposition is owned by
                        // `session_load_error_action` (shared with the resume
                        // contract harness) so ResumeExistingOnly refuse cannot
                        // diverge from production when ResourceNotFound would
                        // otherwise short-circuit on Default attach.
                        let err_str = e.to_string();
                        let forgotten_session = classify_session_load_failure(e.code, &err_str);
                        let recovers_locally =
                            recovers_load_failure_locally(agent_type, forgotten_session);
                        match session_load_error_action(
                            session_attach_mode,
                            e.code,
                            &err_str,
                        ) {
                            SessionLoadErrorAction::RefuseUnresumableBootstrap => {
                                // ResumeExistingOnly (continue / design §2):
                                // ANY load RPC failure — including classified
                                // ResourceNotFound — refuses bootstrap.
                                tracing::warn!(
                                    "[ACP] session/load failed under resume_existing_only \
                                     ({err_str}); refusing session/new fallthrough"
                                );
                                refuse_unresumable_bootstrap(
                                    &state,
                                    &emitter_clone,
                                    &sid,
                                    format!(
                                        "resume_existing_only: session/load failed: {err_str}"
                                    ),
                                    delegation_injection
                                        .as_ref()
                                        .map(|inj| inj.broker.as_ref()),
                                    &connection_id,
                                )
                                .await;
                                return Ok(());
                            }
                            SessionLoadErrorAction::SurfaceClassifiedLoadFailed {
                                code,
                            } => {
                                if recovers_locally {
                                    tracing::info!(
                                        "[ACP] custom agent forgot session {sid}; recovering from codeg transcript"
                                    );
                                } else {
                                // Default attach: unrecoverable historical
                                // session — ResourceNotFound (-32002) or
                                // mid-load process death (Claude 0.58.1
                                // InternalError) → Reload / New UI, not
                                // session/new fallthrough.
                                // Keep raw agent/DB text in logs only; frontend
                                // event message must not leak SQLite/ACP bodies.
                                tracing::warn!(
                                    "[ACP] session/load failed ({err_str}); surfacing as session_load_failed={code}"
                                );
                                let frontend_message =
                                    crate::acp::delegation::broker::sanitize_bootstrap_unresumable_message(
                                        &err_str,
                                    );
                                emit_with_state(
                                    &state,
                                    &emitter_clone,
                                    AcpEvent::SessionLoadFailed {
                                        session_id: sid.clone(),
                                        message: frontend_message,
                                        code: code.to_string(),
                                    },
                                )
                                .await;
                                emit_with_state(
                                    &state,
                                    &emitter_clone,
                                    AcpEvent::StatusChanged {
                                        status: ConnectionStatus::Error,
                                    },
                                )
                                .await;
                                return Ok(());
                                }
                            }
                            SessionLoadErrorAction::ContinueDefaultFallthrough => {}
                        }
                        if attempted_load {
                            tracing::warn!(
                                "[ACP] session/load failed ({err_str}), falling back to session/new"
                            );
                        } else {
                            tracing::info!(
                                "[ACP] agent declares no loadSession support; opening a new session \
                                 for {sid} and linking its history instead of calling session/load"
                            );
                        }
                        // Only emit a visible error for unexpected failures;
                        // "Method not found" is expected for agents that don't
                        // support session resume (e.g. Cline).
                        // "Authentication required" is expected for agents whose
                        // credentials have expired (e.g. Gemini CLI) — skip
                        // session/new too since it will also fail.
                        if err_str.contains("Authentication required") {
                            return Ok(());
                        }
                        // An agent that simply forgot a session codeg recorded
                        // itself is the expected steady state after a restart,
                        // not an incident — an error toast on every reopen
                        // would be pure noise.
                        // A load codeg deliberately never sent is not a failure
                        // to report — the capability gate above is the expected
                        // path for agents that don't implement it.
                        if attempted_load && !err_str.contains("Method not found") && !recovers_locally
                        {
                            emit_with_state(
                                &state,
                                &emitter_clone,
                                AcpEvent::Error {
                                    message: format!("Failed to load session, starting new: {e}"),
                                    agent_type: agent_type.to_string(),
                                    code: None,
                                    // Recoverable: we fall through to `session/new`
                                    // below. Connection stays alive.
                                    terminal: false,
                                },
                            )
                            .await;
                        }
                        let new_session_req = match build_new_session_request(
                            agent_type,
                            &cwd,
                            mcp_servers.clone(),
                            &terminal_shell.spec,
                            adapter_for(agent_type),
                            &route_plan,
                            purpose,
                        ) {
                            Ok(r) => r,
                            Err(e) => {
                                return Err(
                                    bridge_acp_err_for_bootstrap(e, &route_bootstrap_tx).await
                                );
                            }
                        };
                        let (new_resp, grok_models_raw) = send_new_session_capturing_models(
                            &cx,
                            agent_type,
                            new_session_req,
                        )
                        .await?;
                        let fallback_sid = new_resp.session_id.0.to_string();
                        let initial_config_options = new_resp.config_options.clone();
                        let grok_meta = if agent_type == AgentType::Grok {
                            new_resp.meta.clone()
                        } else {
                            None
                        };
                        let grok_effort_specs = (agent_type == AgentType::Grok)
                            .then(|| parse_grok_effort_specs(grok_models_raw.as_ref()));
                        let mut session = cx.attach_session(new_resp, Default::default())?;
                        // Same conversation, new agent session: link the fresh
                        // transcript to the one the failed load was for, so the
                        // turns codeg already recorded keep rendering.
                        record_transcript_header_continuing(
                            agent_type,
                            &fallback_sid,
                            &cwd.to_string_lossy(),
                            Some(sid.as_str()),
                        );
                        emit_with_state(
                            &state,
                            &emitter_clone,
                            AcpEvent::SessionStarted {
                                session_id: fallback_sid.clone(),
                            },
                        )
                        .await;
                        emit_session_modes(&state, &emitter_clone, session.modes()).await;
                        apply_and_emit_session_config_options(
                            &cx,
                            &mut session,
                            &state,
                            &emitter_clone,
                            agent_type,
                            grok_meta.as_ref(),
                            grok_effort_specs.as_ref(),
                            preferred_mode_id.as_deref(),
                            &preferred_config_values,
                            initial_config_options.unwrap_or_default(),
                            file_system_runtime.as_ref(),
                        )
                        .await;
                        emit_selectors_ready(&state, &emitter_clone).await;
                        finish_route_ready(
                            &state,
                            &emitter_clone,
                            &route_plan,
                            &mut pending_lease,
                            &route_bootstrap_tx,
                        )
                        .await?;

                        let loop_result = run_conversation_loop(
                            &mut session,
                            &conn_id,
                            &emitter_clone,
                            &state,
                            agent_type,
                            &perms,
                            &mut cmd_rx,
                            &mut control_rx,
                            &mut cmd_liveness_rx,
                            &mut control_liveness_rx,
                            terminal_runtime.clone(),
                            terminal_assoc.clone(),
                            file_system_runtime.clone(),
                            &cwd_string,
                            supports_fork,
                            &terminal_shell.spec,
                            &route_plan,
                            &prompt_ledger,
                            &terminal_prompt_context,
                            delegation_injection.as_ref(),
                        )
                        .await;
                        terminal_runtime
                            .release_all_for_session(&fallback_sid)
                            .await;
                        drop(session);
                        handle_fork_or_exit(
                            loop_result,
                            &conn_id,
                            &emitter_clone,
                            &state,
                            agent_type,
                            &perms,
                            &mut cmd_rx,
                            &mut control_rx,
                            &mut cmd_liveness_rx,
                            &mut control_liveness_rx,
                            terminal_runtime.clone(),
                            terminal_assoc.clone(),
                            file_system_runtime.clone(),
                            &cwd,
                            &cwd_string,
                            &terminal_shell.spec,
                            &route_plan,
                            &prompt_ledger,
                            &terminal_prompt_context,
                            delegation_injection.as_ref(),
                        )
                        .await
                    }
                }
            } else {
                // Create new session
                let new_session_req = match build_new_session_request(
                    agent_type,
                    &cwd,
                    mcp_servers.clone(),
                    &terminal_shell.spec,
                    adapter_for(agent_type),
                    &route_plan,
                    purpose,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        return Err(bridge_acp_err_for_bootstrap(e, &route_bootstrap_tx).await);
                    }
                };
                let (new_resp, grok_models_raw) = send_new_session_capturing_models(
                    &cx,
                    agent_type,
                    new_session_req,
                )
                .await?;
                let sid = new_resp.session_id.0.to_string();
                let initial_config_options = new_resp.config_options.clone();
                let grok_meta = if agent_type == AgentType::Grok {
                    new_resp.meta.clone()
                } else {
                    None
                };
                let grok_effort_specs = (agent_type == AgentType::Grok)
                    .then(|| parse_grok_effort_specs(grok_models_raw.as_ref()));
                let mut session = cx.attach_session(new_resp, Default::default())?;
                record_transcript_header(agent_type, &sid, &cwd.to_string_lossy());
                emit_with_state(
                    &state,
                    &emitter_clone,
                    AcpEvent::SessionStarted {
                        session_id: sid.clone(),
                    },
                )
                .await;
                emit_session_modes(&state, &emitter_clone, session.modes()).await;
                apply_and_emit_session_config_options(
                    &cx,
                    &mut session,
                    &state,
                    &emitter_clone,
                    agent_type,
                    grok_meta.as_ref(),
                    grok_effort_specs.as_ref(),
                    preferred_mode_id.as_deref(),
                    &preferred_config_values,
                    initial_config_options.unwrap_or_default(),
                    file_system_runtime.as_ref(),
                )
                .await;
                emit_selectors_ready(&state, &emitter_clone).await;
                finish_route_ready(
                    &state,
                    &emitter_clone,
                    &route_plan,
                    &mut pending_lease,
                    &route_bootstrap_tx,
                )
                .await?;

                let loop_result = run_conversation_loop(
                    &mut session,
                    &conn_id,
                    &emitter_clone,
                    &state,
                    agent_type,
                    &perms,
                    &mut cmd_rx,
                    &mut control_rx,
                    &mut cmd_liveness_rx,
                    &mut control_liveness_rx,
                    terminal_runtime.clone(),
                    terminal_assoc.clone(),
                    file_system_runtime.clone(),
                    &cwd_string,
                    supports_fork,
                    &terminal_shell.spec,
                    &route_plan,
                    &prompt_ledger,
                    &terminal_prompt_context,
                    delegation_injection.as_ref(),
                )
                .await;
                terminal_runtime.release_all_for_session(&sid).await;
                if let Ok(mut bridge) = terminal_assoc.lock() {
                    bridge.clear_session(&sid);
                }
                drop(session);
                handle_fork_or_exit(
                    loop_result,
                    &conn_id,
                    &emitter_clone,
                    &state,
                    agent_type,
                    &perms,
                    &mut cmd_rx,
                    &mut control_rx,
                    &mut cmd_liveness_rx,
                    &mut control_liveness_rx,
                    terminal_runtime.clone(),
                    terminal_assoc.clone(),
                    file_system_runtime.clone(),
                    &cwd,
                    &cwd_string,
                    &terminal_shell.spec,
                    &route_plan,
                    &prompt_ledger,
                    &terminal_prompt_context,
                    delegation_injection.as_ref(),
                )
                .await
            }
        }})
        .await;
    match connect_with_result {
        Ok(()) => Ok(()),
        Err(e) => {
            if let Some(evidence) = parent_connection_exit_evidence {
                evidence.record_observation(
                    &evidence_connection_id,
                    unexpected_connection_termination(
                        crate::acp::termination::AcpTerminationSource::Process,
                        crate::acp::termination::AcpTerminationReason::ProcessExited,
                    ),
                );
            }
            let acp_err = classify_connect_error_residual(&e.to_string());
            // Deterministic awaited send: never try_lock-suppress the outcome.
            // If a typed path already took the sender (suppression preflight /
            // finish_route_ready), this is a no-op.
            send_bootstrap_outcome_once(
                &route_bootstrap_tx,
                bootstrap_outcome_from_acp_error(&acp_err),
            )
            .await;
            Err(acp_err)
        }
    }
}

/// Typed mapping from [`AcpError`] to bootstrap outcome. **No string parsing.**
/// Only `NativeSuppressionInvalid` and `CompanionInitializationFailed` yield
/// [`RouteBootstrapOutcome::RouteSpecific`]; auth/provider/SDK/process/generic
/// ACP errors (and any other `RouteUnavailable` reason) are `Fatal`.
fn bootstrap_outcome_from_acp_error(err: &AcpError) -> RouteBootstrapOutcome {
    use crate::acp::delegation::route::RouteDegradedReason;
    match err {
        AcpError::RouteUnavailable { reason }
            if matches!(
                reason,
                RouteDegradedReason::NativeSuppressionInvalid
                    | RouteDegradedReason::CompanionInitializationFailed
            ) =>
        {
            RouteBootstrapOutcome::RouteSpecific(*reason)
        }
        AcpError::InitializeTimeout => RouteBootstrapOutcome::Fatal(AcpError::InitializeTimeout),
        AcpError::SdkNotInstalled(m) => {
            RouteBootstrapOutcome::Fatal(AcpError::SdkNotInstalled(m.clone()))
        }
        AcpError::RouteUnavailable { reason } => {
            RouteBootstrapOutcome::Fatal(AcpError::RouteUnavailable { reason: *reason })
        }
        other => RouteBootstrapOutcome::Fatal(AcpError::protocol(other.to_string())),
    }
}

/// Send bootstrap outcome exactly once (first caller wins). Uses awaited lock.
async fn send_bootstrap_outcome_once(
    route_bootstrap_tx: &Arc<
        tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<RouteBootstrapOutcome>>>,
    >,
    outcome: RouteBootstrapOutcome,
) {
    let mut guard = route_bootstrap_tx.lock().await;
    if let Some(tx) = guard.take() {
        let _ = tx.send(outcome);
    }
}

/// Bridge a typed [`AcpError`] across the sacp boundary: publish the typed
/// bootstrap outcome first, then convert to a display-only sacp error.
async fn bridge_acp_err_for_bootstrap(
    e: AcpError,
    route_bootstrap_tx: &Arc<
        tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<RouteBootstrapOutcome>>>,
    >,
) -> sacp::Error {
    send_bootstrap_outcome_once(route_bootstrap_tx, bootstrap_outcome_from_acp_error(&e)).await;
    sacp::util::internal_error(e.to_string())
}

/// Residual sacp-string classification for errors that never had a typed
/// [`AcpError`] side channel. Recovers **only** the initialize-timeout
/// sentinel. Deliberately does **not** parse `RouteUnavailable` / ready-lease
/// display strings — those must arrive via typed bootstrap side channels.
fn classify_connect_error_residual(raw: &str) -> AcpError {
    if raw.contains(INIT_TIMEOUT_SENTINEL) {
        return AcpError::InitializeTimeout;
    }
    AcpError::protocol(raw)
}

/// Store the permission responder and emit event to frontend.
/// Grok's native `ask_user_question` tool issues this ACP ext request
/// (`_x.ai/ask_user_question`) and BLOCKS on the reply — it does NOT go through
/// the codeg-mcp ask tool. Transparent over the raw params object
/// (`{sessionId, toolCallId, questions, mode}`); the fields codeg needs are read
/// by [`crate::acp::question::parse_grok_ext_questions`]. sacp routes typed
/// handlers on the RAW wire method, so the derive keeps the leading `_`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonRpcRequest)]
#[request(method = "_x.ai/ask_user_question", response = serde_json::Value)]
#[serde(transparent)]
struct GrokAskUserQuestionRequest(serde_json::Value);

/// Store the plan-approval responder and render the approval card. Grok's native
/// `exit_plan_mode` tool issues this ACP ext request (`_x.ai/exit_plan_mode`) and
/// BLOCKS on the reply — the agent won't leave plan mode until the user acts.
/// Transparent over the raw params object (`{sessionId, toolCallId, planContent}`);
/// the fields codeg needs are read by
/// [`crate::acp::plan_approval::parse_grok_exit_plan_request`]. sacp routes typed
/// handlers on the RAW wire method, so the derive keeps the leading `_`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonRpcRequest)]
#[request(method = "_x.ai/exit_plan_mode", response = serde_json::Value)]
#[serde(transparent)]
struct GrokExitPlanModeRequest(serde_json::Value);

/// Every codex `elicitation/create` request — `request_user_input` (Plan
/// mode), generic MCP-server forms, MCP tool-call approvals, message-only
/// confirms — arrives here once codeg advertises `elicitation.form`. sacp
/// 11.0.0 ships no `JsonRpcRequest`/`JsonRpcResponse` impl for the schema's
/// elicitation types (and no feature to enable them), so — like the grok bridge
/// — take the raw params object and reply with a raw JSON value (the serialized
/// `CreateElicitationResponse`). sacp has no built-in elicitation handling, so
/// this custom method handler fills the gap with no dispatch conflict.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, JsonRpcRequest)]
#[request(method = "elicitation/create", response = serde_json::Value)]
#[serde(transparent)]
struct CodexElicitationRequest(serde_json::Value);

/// Bridge grok's native `_x.ai/ask_user_question` ext request into codeg's
/// interactive question card. Grok blocks on the reply, so codeg registers the
/// questions through the shared [`crate::acp::question::SessionQuestionAccess`] —
/// the SAME path the codeg-mcp ask tool uses (it sets `pending_question`,
/// broadcasts `QuestionRequest`, and the `AskQuestionCard` renders) — then answers
/// the ext request with the user's choice, serialized to grok's own format, once
/// they submit. Every early return responds with an error, which makes grok fall
/// back to its inert fire-and-forget rendering — exactly the pre-bridge behavior,
/// so no path here can regress it.
async fn handle_grok_ask_user_question(
    access: &Option<(
        Arc<dyn crate::acp::question::SessionQuestionAccess>,
        crate::acp::question::QuestionRuntimeConfig,
    )>,
    connection_id: &str,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    req: GrokAskUserQuestionRequest,
    responder: Responder<serde_json::Value>,
) {
    let Some((questions, ask_cfg)) = access else {
        let _ = responder.respond_with_internal_error("ask_user_question bridge unavailable");
        return;
    };
    // Same kill switch as the codeg-mcp ask tool: when off, let grok fall back.
    if !ask_cfg.is_enabled().await {
        let _ = responder.respond_with_internal_error("ask_user_question is disabled");
        return;
    }
    let specs = match crate::acp::question::parse_grok_ext_questions(&req.0) {
        Ok(specs) => specs,
        Err(e) => {
            tracing::warn!("[grok ask] rejecting malformed ext request: {e}");
            let _ =
                responder.respond_with_internal_error(format!("invalid ask_user_question: {e}"));
            return;
        }
    };
    // Grok's tool_call_id correlates this ext ask with the (suppressed) native
    // tool_call in the live stream; reuse it so the synthesized result card is the
    // single card for that id. Absent → still answer grok, just skip the card.
    let tool_call_id = req
        .0
        .get("toolCallId")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // register_question consumes the specs; keep a copy to render the answered
    // in-stream card once the user submits.
    let card_specs = specs.clone();
    let Some(registered) = questions.register_question(connection_id, specs).await else {
        // Connection gone, or an ask is already pending on this connection.
        let _ = responder.respond_with_internal_error("could not register ask_user_question");
        return;
    };
    // The user answers out-of-band (the HTTP `answer_question` endpoint resolves
    // the one-shot below), so await it on a task — keeping the ACP dispatch loop
    // free — then reply to grok's blocked ext request.
    let state = Arc::clone(state);
    let emitter = emitter.clone();
    tokio::spawn(async move {
        match registered.answer_rx.await {
            Ok(outcome) => {
                // Surface the answered "提问回答" capsule in-stream — the codeg-mcp
                // ask parity grok's native tool never emits into the ACP stream (it
                // resolves the answer over THIS ext round-trip). Emit BEFORE
                // unblocking grok so the card lands ahead of grok's follow-up text;
                // grok is blocked on this reply, so nothing races the emit. The
                // matching raw ask tool_call/updates are suppressed in the live loop
                // (see `grok_ask_tool_ids`), so this synthesized event — keyed by the
                // same id — is the only card for the ask.
                if let Some(tool_call_id) = tool_call_id {
                    emit_with_state(
                        &state,
                        &emitter,
                        AcpEvent::ToolCall {
                            tool_call_id,
                            title: "ask_user_question".to_string(),
                            kind: "other".to_string(),
                            status: "completed".to_string(),
                            content: None,
                            raw_input: Some(
                                crate::acp::question::grok_result_card_input(&card_specs)
                                    .to_string(),
                            ),
                            raw_output: Some(
                                crate::acp::question::grok_result_card_output(&outcome).to_string(),
                            ),
                            locations: None,
                            meta: None,
                            images: None,
                        },
                    )
                    .await;
                }
                let _ = responder.respond(crate::acp::question::build_grok_ext_response(&outcome));
            }
            // Sender dropped: the ask was canceled or the connection tore down —
            // nothing to render; let grok fall back via skip_interview.
            Err(_) => {
                let _ = responder.respond(crate::acp::question::grok_ext_skip_response());
            }
        }
    });
}

/// Bridge grok's native `_x.ai/exit_plan_mode` ext request into codeg's
/// interactive plan-approval card. Grok BLOCKS on the reply — it won't leave plan
/// mode until the user acts — so codeg registers the approval through the shared
/// [`crate::acp::plan_approval::SessionPlanApprovalAccess`] (which sets
/// `pending_plan_approval`, broadcasts `PlanApprovalRequest`, and renders the card
/// above the composer), then answers the ext request with the user's decision once
/// they submit. Unlike the ask bridge there is no synthesized in-stream card:
/// grok's own `exit_plan_mode` tool_call renders the plan in the transcript (via
/// `PlanModeCard`), mirroring how the permission dialog coexists with the tool
/// call. Every early return replies with the disconnect-shaped response so grok
/// keeps plan mode active — it can never be read as a silent approval.
async fn handle_grok_exit_plan_mode(
    access: &Option<Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>>,
    connection_id: &str,
    req: GrokExitPlanModeRequest,
    responder: Responder<serde_json::Value>,
) {
    // Log the wire SHAPE (top-level field names), not the raw request — the plan
    // body can be large and carry file paths / source. The keys are what
    // wire-format verification needs (confirm `sessionId`/`toolCallId`/`planContent`
    // on the first real run); the malformed path below logs more if parsing fails.
    tracing::info!(
        "[grok exit_plan] received _x.ai/exit_plan_mode ext request: keys={:?}",
        req.0
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect::<Vec<_>>())
    );
    let Some(access) = access else {
        let _ = responder.respond(crate::acp::plan_approval::grok_exit_plan_disconnect_response());
        return;
    };
    let (plan_markdown, tool_call_id) =
        match crate::acp::plan_approval::parse_grok_exit_plan_request(&req.0) {
            Ok(parsed) => parsed,
            Err(e) => {
                tracing::warn!("[grok exit_plan] rejecting malformed ext request: {e}");
                let _ = responder
                    .respond(crate::acp::plan_approval::grok_exit_plan_disconnect_response());
                return;
            }
        };
    tracing::info!(
        "[grok exit_plan] toolCallId={tool_call_id:?} plan_chars={}",
        plan_markdown.chars().count()
    );
    let Some(registered) = access
        .register_plan_approval(connection_id, tool_call_id, plan_markdown)
        .await
    else {
        // Connection gone, or an approval is already pending on this connection.
        let _ = responder.respond(crate::acp::plan_approval::grok_exit_plan_disconnect_response());
        return;
    };
    // The user answers out-of-band (the HTTP `answer_plan_approval` endpoint
    // resolves the one-shot below), so await it on a task — keeping the ACP
    // dispatch loop free — then reply to grok's blocked ext request. The manager's
    // `answer_plan_approval` / teardown emit `PlanApprovalResolved` to clear the
    // card; this task only unblocks grok.
    tokio::spawn(async move {
        match registered.answer_rx.await {
            Ok(answer) => {
                let _ = responder.respond(
                    crate::acp::plan_approval::build_grok_exit_plan_response(&answer),
                );
            }
            // Sender dropped: the approval was canceled or the connection tore
            // down — reply disconnect so grok keeps plan mode active.
            Err(_) => {
                let _ = responder
                    .respond(crate::acp::plan_approval::grok_exit_plan_disconnect_response());
            }
        }
    });
}

/// Bridge codex's `elicitation/create` requests into codeg's interactive
/// surfaces. Codex only sends these when codeg declares `elicitation.form`
/// (see `connect_with`), then BLOCKS on the reply, so every shape must resolve
/// to something the user can act on (see
/// [`crate::acp::question::classify_elicitation`] for the full taxonomy):
///
///   * Question-style (Plan-mode `request_user_input`, generic MCP forms) →
///     the shared [`crate::acp::question::SessionQuestionAccess`] path — the
///     SAME one the codeg-mcp ask tool and the grok bridge use (it sets
///     `pending_question`, broadcasts `QuestionRequest`, and `AskQuestionCard`
///     renders) — answered once the user submits.
///   * Approval-style (MCP tool-call approvals, message-only confirms) → the
///     permission card via `pending_perms`, exactly like the
///     `session/request_permission` fallback codex-acp used before the
///     capability was advertised. Auto-declining these would reject the tool
///     call (including codeg-mcp's own tools in consent-requiring modes).
///
/// Question-path early returns DECLINE, which makes codex proceed with its own
/// judgment — no worse than the pre-bridge `{answers:{}}`, so nothing here can
/// regress it. Codex delivers `request_user_input` ONLY as this elicitation and
/// never puts a completed tool_call on the stream, so — like the grok bridge —
/// the question path synthesizes the answered result card itself once the user
/// submits (keyed by the elicitation's tool_call_id).
#[allow(clippy::too_many_arguments)]
async fn handle_elicitation_request(
    access: &Option<(
        Arc<dyn crate::acp::question::SessionQuestionAccess>,
        crate::acp::question::QuestionRuntimeConfig,
    )>,
    perms: &PendingPermissions,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    connection_id: &str,
    req: CodexElicitationRequest,
    responder: Responder<serde_json::Value>,
) {
    // The wire reply is the serialized `CreateElicitationResponse` (see the
    // newtype above). `Decline` makes codex proceed with its own judgment.
    fn decline() -> serde_json::Value {
        serde_json::to_value(crate::acp::question::elicitation_decline_response())
            .unwrap_or_default()
    }
    let raw = req.0;
    // Everything codex-acp can send once `elicitation.form` is advertised
    // resolves to a plan here — an unhandled shape would silently reject the
    // agent's blocked request (an MCP tool-call approval, most damagingly).
    let plan = match crate::acp::question::classify_elicitation(&raw) {
        Ok(plan) => plan,
        Err(e) => {
            tracing::warn!("[codex elicitation] declining unrenderable request: {e}");
            let _ = responder.respond(decline());
            return;
        }
    };
    match plan {
        // Approval-style (MCP tool-call approval / message-only confirm):
        // render through the permission card — the exact surface these used
        // before codeg advertised `elicitation.form` (codex-acp then sent
        // `session/request_permission`). Deliberately NOT gated by the
        // ask_user_question toggle: this is consent, not an agent question,
        // and auto-declining would reject the tool call outright.
        crate::acp::question::ElicitationPlan::Approval(approval) => {
            let request_id = uuid::Uuid::new_v4().to_string();
            // Mirror codex-acp's own `request_permission` fallback tool_call
            // shape (`buildPermissionRequest`) so the frontend permission card
            // renders it identically. When codex correlated the approval to an
            // already-rendered mcpToolCall item, reuse that id so the card
            // attaches to it.
            let tool_call = serde_json::json!({
                "toolCallId": approval
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| format!("elicitation-{request_id}")),
                "title": approval.message,
                "kind": "execute",
                "status": "pending",
                "content": [{
                    "type": "content",
                    "content": {"type": "text", "text": approval.message},
                }],
            });
            let options: Vec<PermissionOptionInfo> = approval
                .options
                .iter()
                .map(|o| PermissionOptionInfo {
                    option_id: o.option_id.clone(),
                    name: o.label.clone(),
                    kind: o.kind.to_string(),
                })
                .collect();
            perms.lock().await.insert(
                request_id.clone(),
                PendingPermission::CodexElicitation {
                    responder,
                    approval,
                },
            );
            emit_with_state(
                state,
                emitter,
                AcpEvent::PermissionRequest {
                    request_id,
                    tool_call,
                    options,
                },
            )
            .await;
        }
        // Question-style (codex `request_user_input`, generic MCP forms):
        // bridge into the same ask card as the codeg-mcp ask tool.
        crate::acp::question::ElicitationPlan::Questions(questions) => {
            let Some((question_access, ask_cfg)) = access else {
                let _ = responder.respond(decline());
                return;
            };
            // Same kill switch as the codeg-mcp ask tool and the grok bridge:
            // when the user has turned ask_user_question off, decline so codex
            // proceeds.
            if !ask_cfg.is_enabled().await {
                let _ = responder.respond(decline());
                return;
            }
            // register_question consumes the specs; `questions` keeps its copy
            // to correlate the answer back to each field when building the
            // response.
            let Some(registered) = question_access
                .register_question(connection_id, questions.specs.clone())
                .await
            else {
                // Connection gone, or an ask is already pending on this connection.
                let _ = responder.respond(decline());
                return;
            };
            // Codex advertises an auto-resolution timeout on some
            // `request_user_input` asks (`_meta.codex.autoResolutionMs`):
            // codex-acp races the elicitation against it and answers
            // `{answers: {}}` itself on expiry, ABANDONING this request. Reap
            // the by-then-pointless card shortly after so it can't linger as a
            // zombie; `cancel_question` is a no-op if the user already
            // answered.
            if let Some(ms) = crate::acp::question::elicitation_auto_resolution_ms(&raw) {
                let reaper_access = Arc::clone(question_access);
                let reaper_conn = connection_id.to_string();
                let reaper_qid = registered.question_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms.saturating_add(2_000)))
                        .await;
                    reaper_access
                        .cancel_question(&reaper_conn, &reaper_qid)
                        .await;
                });
            }
            // The user answers out-of-band (the `answer_question` endpoint
            // resolves the one-shot below), so await it on a task — keeping
            // the ACP dispatch loop free — then reply to codex's blocked
            // elicitation request. Cloned here so the synthesized card below can
            // write through the session state from the spawned task.
            let card_state = Arc::clone(state);
            let card_emitter = emitter.clone();
            tokio::spawn(async move {
                let response = match registered.answer_rx.await {
                    Ok(outcome) => {
                        // Surface the answered "提问回答" capsule in-stream — the
                        // parity codex's `request_user_input` never emits itself: it
                        // resolves the answer over THIS elicitation round-trip and
                        // puts no completed tool_call on the ACP stream (so the live
                        // message has nothing to render otherwise). Emit BEFORE
                        // unblocking codex so the card lands ahead of its follow-up
                        // text; codex is blocked on this reply, so nothing races the
                        // emit. Keyed by the elicitation's tool_call_id so this is
                        // the single card for the ask and the reloaded history card
                        // (`codex.rs`, same id) replaces rather than duplicates it.
                        if let Some(tool_call_id) = questions.tool_call_id.clone() {
                            emit_with_state(
                                &card_state,
                                &card_emitter,
                                AcpEvent::ToolCall {
                                    tool_call_id,
                                    title: "request_user_input".to_string(),
                                    kind: "other".to_string(),
                                    status: "completed".to_string(),
                                    content: None,
                                    raw_input: Some(
                                        crate::acp::question::grok_result_card_input(
                                            &questions.specs,
                                        )
                                        .to_string(),
                                    ),
                                    raw_output: Some(
                                        crate::acp::question::grok_result_card_output(&outcome)
                                            .to_string(),
                                    ),
                                    locations: None,
                                    meta: None,
                                    images: None,
                                },
                            )
                            .await;
                        }
                        crate::acp::question::build_elicitation_response(&questions, &outcome)
                    }
                    // Sender dropped: canceled or the connection tore down.
                    // Decline so codex proceeds with its own judgment.
                    Err(_) => crate::acp::question::elicitation_decline_response(),
                };
                let _ = responder.respond(serde_json::to_value(response).unwrap_or_default());
            });
        }
    }
}

async fn handle_permission_request(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
    cwd: &str,
    req: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
) {
    // Hidden generation has no interactive UI path: decline immediately and
    // still emit so the private-stream runner observes Interactive failure.
    let is_hidden_generation = {
        let s = state.read().await;
        s.purpose.is_hidden_generation()
    };
    if is_hidden_generation {
        let request_id = uuid::Uuid::new_v4().to_string();
        let _ = responder.respond(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ));
        emit_with_state(
            state,
            emitter,
            AcpEvent::PermissionRequest {
                request_id,
                tool_call: serde_json::to_value(&req.tool_call).unwrap_or_default(),
                options: vec![],
            },
        )
        .await;
        return;
    }

    let request_id = uuid::Uuid::new_v4().to_string();

    let options: Vec<PermissionOptionInfo> = req
        .options
        .iter()
        .map(|opt| PermissionOptionInfo {
            option_id: opt.option_id.to_string(),
            name: opt.name.clone(),
            kind: match opt.kind {
                PermissionOptionKind::AllowOnce => "allow_once".into(),
                PermissionOptionKind::AllowAlways => "allow_always".into(),
                PermissionOptionKind::RejectOnce => "reject_once".into(),
                PermissionOptionKind::RejectAlways => "reject_always".into(),
                _ => "unknown".into(),
            },
        })
        .collect();

    let mut tool_call_value = serde_json::to_value(&req.tool_call).unwrap_or_default();

    // Resolve line numbers in rawInput for edit tool permission requests
    if let Some(obj) = tool_call_value.as_object_mut() {
        let key = ["rawInput", "raw_input"]
            .into_iter()
            .find(|k| obj.contains_key(*k));
        if let Some(key) = key {
            match obj.get_mut(key) {
                // rawInput is a JSON object: inject _start_line in place
                Some(v) if v.is_object() => {
                    inject_start_line(v, Some(cwd));
                }
                // rawInput is a JSON string: parse, inject, write back as object
                Some(serde_json::Value::String(text)) => {
                    let text = text.clone();
                    if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                        if inject_start_line(&mut parsed, Some(cwd)) {
                            obj.insert(key.to_string(), parsed);
                        }
                    } else if text.contains("@@\n") || text.contains("@@\r\n") {
                        if let Some(resolved) = crate::parsers::resolve_patch_text(&text, Some(cwd))
                        {
                            obj.insert(key.to_string(), serde_json::Value::String(resolved));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    perms
        .lock()
        .await
        .insert(request_id.clone(), PendingPermission::Acp(responder));

    emit_with_state(
        state,
        emitter,
        AcpEvent::PermissionRequest {
            request_id,
            tool_call: tool_call_value,
            options,
        },
    )
    .await;
    tool_watchdog_pause_permission(state, emitter).await;
}

async fn emit_cancelled_permission_events(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    request_ids: impl IntoIterator<Item = String>,
) {
    for request_id in request_ids {
        emit_with_state(state, emitter, AcpEvent::PermissionResolved { request_id }).await;
    }
    tool_watchdog_resume(state).await;
}

async fn cancel_pending_permissions(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
) {
    let drained = perms.lock().await.drain().collect::<Vec<_>>();
    let mut request_ids = Vec::with_capacity(drained.len());
    for (request_id, responder) in drained {
        responder.respond_cancelled();
        request_ids.push(request_id);
    }
    emit_cancelled_permission_events(state, emitter, request_ids).await;
}

fn respond_terminal_request<T: sacp::JsonRpcResponse>(
    responder: Responder<T>,
    result: Result<T, TerminalRuntimeError>,
) -> Result<(), sacp::Error> {
    match result {
        Ok(response) => responder.respond(response),
        Err(error) => responder.respond_with_error(error.into_rpc_error()),
    }
}

fn respond_file_system_request<T: sacp::JsonRpcResponse>(
    responder: Responder<T>,
    result: Result<T, FileSystemRuntimeError>,
) -> Result<(), sacp::Error> {
    match result {
        Ok(response) => responder.respond(response),
        Err(error) => responder.respond_with_error(error.into_rpc_error()),
    }
}

async fn set_session_mode(
    session: &mut sacp::ActiveSession<'_, Agent>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    mode_id: String,
    file_system_runtime: &FileSystemRuntime,
    agent_type: AgentType,
) -> Result<(), sacp::Error> {
    let req = SetSessionModeRequest::new(session.session_id().clone(), mode_id.clone());
    session
        .connection()
        .send_request_to(Agent, req)
        .block_task()
        .await?;

    sync_file_system_outside_access(file_system_runtime, agent_type, Some(&mode_id));
    emit_with_state(state, emitter, AcpEvent::ModeChanged { mode_id }).await;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn set_session_config_option(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    config_id: String,
    value_id: String,
) -> Result<(), sacp::Error> {
    let updated = set_session_config_option_inner(cx, session_id, config_id, value_id).await?;
    emit_session_config_options_values(state, emitter, agent_type, updated).await;
    Ok(())
}

/// Wire-level half of `set_session_config_option`: send the JSON-RPC request and
/// return the agent's new config-options list, without touching SessionState or
/// emitting events. Used at session-init to apply saved preferences before the
/// single emit_session_config_options call so the frontend never sees an
/// "agent default → user preference" flicker.
async fn set_session_config_option_inner(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    config_id: String,
    value_id: String,
) -> Result<Vec<SessionConfigOption>, sacp::Error> {
    let req = SetSessionConfigOptionRequest::new(session_id.clone(), config_id, value_id);
    let untyped_req = UntypedMessage::new("session/set_config_option", req).map_err(|e| {
        sacp::util::internal_error(format!("Failed to build config option request: {e}"))
    })?;

    let raw_response = cx.send_request_to(Agent, untyped_req).block_task().await?;
    let response: SetSessionConfigOptionResponse =
        serde_json::from_value(raw_response).map_err(|e| {
            sacp::util::internal_error(format!("Failed to parse config option response: {e}"))
        })?;

    Ok(response.config_options)
}

/// Send codex's bespoke `_codex/session/goal_control` extension request to pause
/// or clear the session's active goal (codex-acp #293, v1.1.4). Start / resume /
/// re-objective are NOT this method — they go through the `/goal` prompt.
///
/// codex replies with an empty object and then pushes the resulting goal
/// snapshot as a normal `session_info_update` (`_meta.codex.goal`, or `null` for
/// a clear), which the existing goal-card path renders — so the response value
/// carries nothing to parse and is intentionally discarded.
///
/// Sent via `UntypedMessage` because `_codex/…` is a codex-private extension
/// method with no sacp typed variant — the same escape hatch used for
/// `session/set_config_option` and `session/fork`.
async fn send_goal_control(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    action: GoalControlAction,
) -> Result<(), sacp::Error> {
    let params = serde_json::json!({
        "sessionId": session_id,
        "action": action,
    });
    let untyped_req = UntypedMessage::new("_codex/session/goal_control", params).map_err(|e| {
        sacp::util::internal_error(format!("Failed to build goal_control request: {e}"))
    })?;
    cx.send_request_to(Agent, untyped_req).block_task().await?;
    Ok(())
}

/// Apply user-saved mode and config-option preferences to a freshly-attached
/// session BEFORE the initial `session_modes` / `session_config_options`
/// events are emitted to the frontend.
///
/// This is the single ownership point for "preference → agent state" — the
/// frontend stores the user's last selections per agent_type and ships them
/// to the backend on connect; we then call `session/set_mode` and
/// `session/set_config_option` to align the agent process so the snapshot
/// the frontend will see (whether via WS `snapshot` frame or fetched HTTP
/// snapshot) already reflects the user's choices. No client-side
/// "intercept event and rewrite then sync back" hack — single source of truth.
///
/// Returns the (possibly updated) list of config options that the caller
/// should emit. Mode preferences trigger a `ModeChanged` event from
/// `set_session_mode`, which the caller's `emit_session_modes` immediately
/// precedes — so the frontend sees `SessionModes{default}` then
/// `ModeChanged{preferred}` and converges to the preferred value before
/// `SelectorsReady` fires. Failures on individual preferences are logged
/// and skipped so a stale/invalid preference can't block session startup.
#[allow(clippy::too_many_arguments)]
async fn apply_preferred_session_options(
    cx: &ConnectionTo<Agent>,
    session: &mut sacp::ActiveSession<'_, Agent>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    preferred_mode_id: Option<&str>,
    preferred_config_values: &BTreeMap<String, String>,
    initial_config_options: Vec<SessionConfigOption>,
    file_system_runtime: &FileSystemRuntime,
    agent_type: AgentType,
) -> Vec<SessionConfigOption> {
    if let Some(pref_mode) = preferred_mode_id {
        let needs_apply = session
            .modes()
            .as_ref()
            .map(|m| m.current_mode_id.to_string() != pref_mode)
            .unwrap_or(false);
        if needs_apply {
            if let Err(e) = set_session_mode(
                session,
                state,
                emitter,
                pref_mode.to_string(),
                file_system_runtime,
                agent_type,
            )
            .await
            {
                tracing::error!(
                    "[ACP] failed to apply preferred mode '{pref_mode}' on connect: {e}"
                );
            }
        } else {
            // Preferred mode already active — still align the client FS sandbox.
            sync_file_system_outside_access(file_system_runtime, agent_type, Some(pref_mode));
        }
    } else if let Some(current) = session
        .modes()
        .as_ref()
        .map(|m| m.current_mode_id.to_string())
    {
        sync_file_system_outside_access(file_system_runtime, agent_type, Some(&current));
    }

    if preferred_config_values.is_empty() {
        return initial_config_options;
    }

    let session_id = session.session_id().clone();
    let mut options = initial_config_options;
    for (config_id, value_id) in preferred_config_values {
        // Skip the round-trip when the agent's current value already matches.
        // Note: codex-acp 1.0.0 advertises "mode" as a config option (so the
        // match check below normally fires), but we still do NOT skip when a
        // requested config_id is absent from the advertised options — older or
        // edge-case builds accept `set_config_option` for an unadvertised "mode"
        // (see `ensure_codex_mode_option`), so let the agent decide.
        let already_matches = options.iter().any(|o| {
            o.id.to_string() == *config_id
                && matches!(
                    &o.kind,
                    SessionConfigKind::Select(s) if s.current_value.to_string() == *value_id
                )
        });
        if already_matches {
            if config_id == "mode" {
                sync_file_system_outside_access(
                    file_system_runtime,
                    agent_type,
                    Some(value_id.as_str()),
                );
            }
            continue;
        }
        match set_session_config_option_inner(cx, &session_id, config_id.clone(), value_id.clone())
            .await
        {
            Ok(updated) => {
                if config_id == "mode" {
                    sync_file_system_outside_access(
                        file_system_runtime,
                        agent_type,
                        Some(value_id.as_str()),
                    );
                }
                options = updated;
            }
            Err(e) => tracing::error!(
                "[ACP] failed to apply preferred config '{config_id}'='{value_id}' \
                 on connect: {e}"
            ),
        }
    }

    options
}

const TERMINAL_POLL_INTERVAL_MS: u64 = 200;
const TERMINAL_POLL_MISSING_LIMIT: u8 = 10;

/// Hard cap on the size of a single ACP event's `raw_output` payload.
///
/// Agents (e.g. Claude Code, Codex) frequently send `tool_call_update`
/// notifications where `raw_output` is the **full accumulated** tool output
/// rather than an incremental delta. For long-running terminal tools this
/// leads to O(N²) bytes flowing through the event pipeline and multi-GB
/// transient allocations (serde_json Value trees, IPC buffers, broadcast
/// channel backlog). This constant caps any single emitted chunk so the
/// pipeline never sees a multi-MB event.
const MAX_SINGLE_EMIT_BYTES: usize = 64 * 1024;

/// Byte length of the tail we retain per tool-call to verify that the next
/// incoming snapshot is a cumulative extension of the previous one. Small
/// enough to keep the cache bounded even in pathological sessions, large
/// enough that a matching tail is an extremely unlikely coincidence.
const MAX_CACHED_TAIL_BYTES: usize = 8 * 1024;

/// Hard cap on the number of tool-call entries the cache retains. Prevents
/// unbounded growth in long sessions where agents forget to mark tool calls
/// as completed. Entries are evicted FIFO by generation counter.
const MAX_CACHE_ENTRIES: usize = 256;

/// Prefix used when an emitted chunk had to be truncated.
const TRUNCATION_MARKER: &str = "[...truncated...]\n";

#[derive(Debug)]
struct CachedOutput {
    /// Total byte length of the last observed `raw_output`.
    total_len: usize,
    /// Tail of the last observed `raw_output`, up to `MAX_CACHED_TAIL_BYTES`
    /// bytes. Always aligned to a UTF-8 character boundary at the start.
    tail: String,
    /// Monotonic insertion/update tick used for FIFO eviction.
    generation: u64,
}

/// Per-session cache of the last `raw_output` fingerprint emitted for each
/// tool call. Enables delta detection: when an agent sends cumulative
/// snapshots, we forward only the suffix (with `raw_output_append=true`)
/// and keep the fingerprint bounded so it works even when the full output
/// grows into the multi-MB range.
#[derive(Debug, Default)]
struct ToolCallOutputCache {
    entries: HashMap<String, CachedOutput>,
    next_generation: u64,
}

impl ToolCallOutputCache {
    /// Diff an incoming full `raw_output` snapshot for `tool_call_id` against
    /// the cache and return what should be emitted downstream.
    ///
    /// Returns `None` when the incoming snapshot is identical to the
    /// previously emitted one (nothing to send). Otherwise returns
    /// `(payload, append)` where:
    /// - `append=true` — `payload` is a (possibly truncated) suffix delta;
    ///   the frontend should append it to the existing chunks.
    /// - `append=false` — `payload` is a (possibly truncated) replacement
    ///   for the full tool output; the frontend should reset chunks.
    fn consume(&mut self, tool_call_id: &str, curr: &str) -> Option<(String, bool)> {
        let curr_len = curr.len();

        let decision: Option<(String, bool)> = match self.entries.get(tool_call_id) {
            Some(prev) if curr_len >= prev.total_len && self.is_extension_of(prev, curr) => {
                if curr_len == prev.total_len {
                    // Identical output — nothing to emit. Cache stays fresh.
                    return None;
                }
                let suffix = &curr[prev.total_len..];
                Some(build_emit_payload(suffix, true))
            }
            _ => Some(build_emit_payload(curr, false)),
        };

        // Update cache snapshot to current state so the next update can
        // still detect a prefix extension.
        let tail =
            trim_partial_ansi_tail(truncate_tail_at_char_boundary(curr, MAX_CACHED_TAIL_BYTES))
                .to_string();
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        self.entries.insert(
            tool_call_id.to_string(),
            CachedOutput {
                total_len: curr_len,
                tail,
                generation,
            },
        );
        self.enforce_entry_cap();
        decision
    }

    /// Seed the cache with an initial snapshot for `tool_call_id`, WITHOUT
    /// attempting to diff against any prior state. Used for the initial
    /// `SessionUpdate::ToolCall` notification, whose frontend reducer
    /// treats `raw_output` as a full replacement.
    fn seed(&mut self, tool_call_id: &str, curr: &str) -> Option<String> {
        let (payload, _append) = build_emit_payload(curr, false);
        let tail =
            trim_partial_ansi_tail(truncate_tail_at_char_boundary(curr, MAX_CACHED_TAIL_BYTES))
                .to_string();
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1);
        self.entries.insert(
            tool_call_id.to_string(),
            CachedOutput {
                total_len: curr.len(),
                tail,
                generation,
            },
        );
        self.enforce_entry_cap();
        if payload.is_empty() {
            None
        } else {
            Some(payload)
        }
    }

    /// Drop cached state for a tool call that has finished. Keeps the
    /// session-scoped cache bounded in long-running sessions.
    fn remove_if_final(&mut self, tool_call_id: &str, status: Option<&str>) {
        if matches!(status, Some("completed" | "failed" | "cancelled" | "error")) {
            self.entries.remove(tool_call_id);
        }
    }

    /// Returns true when the cached fingerprint matches `curr` at the
    /// expected offset — i.e. `curr` is a prefix extension (or identity)
    /// of the previously observed snapshot.
    fn is_extension_of(&self, prev: &CachedOutput, curr: &str) -> bool {
        let tail_start = prev.total_len.saturating_sub(prev.tail.len());
        curr.get(tail_start..prev.total_len)
            .is_some_and(|slice| slice == prev.tail.as_str())
    }

    /// Evict oldest entries (by `generation`) once the cache exceeds the
    /// entry cap. Linear scan over a bounded map, so O(MAX_CACHE_ENTRIES)
    /// per eviction — acceptable at this size.
    fn enforce_entry_cap(&mut self) {
        while self.entries.len() > MAX_CACHE_ENTRIES {
            let Some(oldest_id) = self
                .entries
                .iter()
                .min_by_key(|(_, v)| v.generation)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.entries.remove(&oldest_id);
        }
    }
}

/// Apply the per-event size cap + truncation marker. Returns `(payload,
/// append)`. An empty `text` yields an empty `payload`; callers should
/// decide whether to suppress the emission in that case.
fn build_emit_payload(text: &str, append: bool) -> (String, bool) {
    let truncated =
        trim_partial_ansi_tail(truncate_tail_at_char_boundary(text, MAX_SINGLE_EMIT_BYTES));
    let out = if truncated.len() < text.len() {
        format!("{TRUNCATION_MARKER}{truncated}")
    } else {
        truncated.to_string()
    };
    (out, append)
}

/// Return a substring of `s` whose byte length is `<= max_bytes`, aligned to
/// a UTF-8 character boundary and taken from the TAIL of `s` (so the most
/// recent output is preserved when truncation is required).
fn truncate_tail_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// If the very end of `s` contains a partial ANSI escape sequence, trim it
/// so downstream ANSI parsers (e.g. the frontend `ansi-to-react` renderer)
/// don't see a half-emitted escape.
///
/// Handles the three common ACP-stream cases:
/// - CSI (`ESC [ ... final`): terminator is a byte in 0x40..=0x7E after
///   the `[` introducer.
/// - OSC (`ESC ] ... ST|BEL`): terminator is BEL (0x07) or `ESC \`.
/// - Simple two-byte escape (`ESC <byte>`): complete as soon as the byte
///   following ESC is present.
///
/// ESC is ASCII (1 byte), always a valid UTF-8 char boundary, so slicing
/// at `esc_pos` cannot produce an invalid UTF-8 string.
fn trim_partial_ansi_tail(s: &str) -> &str {
    let bytes = s.as_bytes();
    let Some(esc_pos) = bytes.iter().rposition(|&b| b == 0x1B) else {
        return s;
    };
    let after = &bytes[esc_pos + 1..];
    if after.is_empty() {
        return &s[..esc_pos];
    }
    let terminated = match after[0] {
        b'[' => after[1..].iter().any(|&b| (0x40..=0x7E).contains(&b)),
        b']' => {
            after[1..].contains(&0x07)
                || after[1..].windows(2).any(|w| w[0] == 0x1B && w[1] == b'\\')
        }
        // Two-byte escape sequences (ESC M, ESC D, …) are complete as
        // soon as the second byte is present.
        _ => true,
    };
    if terminated {
        s
    } else {
        &s[..esc_pos]
    }
}

#[derive(Debug, Default)]
struct TrackedTerminalToolCall {
    terminal_ids: Vec<String>,
    status: Option<String>,
    terminal_offsets: HashMap<String, u64>,
    terminal_exit_reported: HashSet<String>,
    has_emitted_output: bool,
    missing_polls: u8,
}

#[derive(Debug, Default)]
struct TerminalPollResult {
    output: Option<String>,
    append: bool,
    any_found: bool,
    all_exited: bool,
}

fn is_final_tool_call_status(status: Option<&str>) -> bool {
    matches!(status, Some("completed" | "failed"))
}

fn merge_terminal_ids(existing: &mut Vec<String>, incoming: Vec<String>) -> bool {
    let mut changed = false;
    for terminal_id in incoming {
        if !existing.iter().any(|id| id == &terminal_id) {
            existing.push(terminal_id);
            changed = true;
        }
    }
    changed
}

fn extract_terminal_ids(content: &[ToolCallContent]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut terminal_ids = Vec::new();
    for item in content {
        if let ToolCallContent::Terminal(terminal) = item {
            let terminal_id = terminal.terminal_id.to_string();
            if seen.insert(terminal_id.clone()) {
                terminal_ids.push(terminal_id);
            }
        }
    }
    terminal_ids
}

fn track_terminal_tool_calls(
    update: &SessionUpdate,
    tracked: &mut HashMap<String, TrackedTerminalToolCall>,
) -> bool {
    match update {
        SessionUpdate::ToolCall(tc) => {
            let terminal_ids = extract_terminal_ids(&tc.content);
            if terminal_ids.is_empty() {
                return false;
            }

            let status = format!("{:?}", tc.status).to_lowercase();
            let entry = tracked.entry(tc.tool_call_id.to_string()).or_default();
            let changed = merge_terminal_ids(&mut entry.terminal_ids, terminal_ids);
            entry.status = Some(status);
            changed
        }
        SessionUpdate::ToolCallUpdate(tcu) => {
            let mut changed = false;
            let mut should_track = false;

            let terminal_ids = tcu
                .fields
                .content
                .as_ref()
                .map(|content| extract_terminal_ids(content))
                .unwrap_or_default();
            if !terminal_ids.is_empty() {
                should_track = true;
            }

            if tracked.contains_key(&tcu.tool_call_id.to_string()) {
                should_track = true;
            }

            if !should_track {
                return false;
            }

            let entry = tracked.entry(tcu.tool_call_id.to_string()).or_default();
            if !terminal_ids.is_empty() {
                changed = merge_terminal_ids(&mut entry.terminal_ids, terminal_ids);
            }

            if let Some(status) = tcu.fields.status {
                let status_str = format!("{:?}", status).to_lowercase();
                if entry.status.as_deref() != Some(status_str.as_str()) {
                    changed = true;
                }
                entry.status = Some(status_str);
            }

            changed
        }
        _ => false,
    }
}

/// Feed shell tool_call / tool_call_update signals into the Grok terminal
/// association fallback (no-op when the bridge is disabled).
fn observe_terminal_assoc_from_update(
    update: &SessionUpdate,
    session_id: &str,
    assoc: &std::sync::Mutex<TerminalAssocFallback>,
) {
    let hint = match update {
        SessionUpdate::ToolCall(tc) => ToolCallAssocHint {
            tool_call_id: tc.tool_call_id.to_string(),
            kind: Some(format!("{:?}", tc.kind).to_lowercase()),
            title: Some(tc.title.clone()),
            has_terminal_content: !extract_terminal_ids(&tc.content).is_empty(),
            status: Some(format!("{:?}", tc.status).to_lowercase()),
        },
        SessionUpdate::ToolCallUpdate(tcu) => {
            let terminal_ids = tcu
                .fields
                .content
                .as_ref()
                .map(|content| extract_terminal_ids(content))
                .unwrap_or_default();
            ToolCallAssocHint {
                tool_call_id: tcu.tool_call_id.to_string(),
                kind: tcu.fields.kind.map(|k| format!("{:?}", k).to_lowercase()),
                title: tcu.fields.title.clone(),
                has_terminal_content: !terminal_ids.is_empty(),
                status: tcu.fields.status.map(|s| format!("{:?}", s).to_lowercase()),
            }
        }
        _ => return,
    };

    if let Ok(mut bridge) = assoc.lock() {
        bridge.observe_tool(session_id, hint);
    }
}

/// Merge fallback `tool_call_id ↔ terminal_id` binds into the live poller map.
/// Returns the tool_call_ids whose terminal association changed (for watchdog
/// capability re-derivation) — empty when nothing new was attached.
fn merge_terminal_assoc_binds(
    session_id: &str,
    assoc: &std::sync::Mutex<TerminalAssocFallback>,
    tracked: &mut HashMap<String, TrackedTerminalToolCall>,
) -> Vec<String> {
    let binds = match assoc.lock() {
        Ok(mut bridge) => bridge.drain_pending_binds(session_id),
        Err(_) => return Vec::new(),
    };
    if binds.is_empty() {
        return Vec::new();
    }

    let mut changed_tools = Vec::new();
    for (tool_call_id, terminal_id) in binds {
        let entry = tracked.entry(tool_call_id.clone()).or_default();
        if merge_terminal_ids(&mut entry.terminal_ids, vec![terminal_id])
            && !changed_tools.iter().any(|id| id == &tool_call_id)
        {
            changed_tools.push(tool_call_id);
        }
        if entry.status.is_none() {
            // Keep the poller from treating an unbound entry as already final.
            entry.status = Some("inprogress".into());
        }
    }
    changed_tools
}

fn format_terminal_exit_status(exit_status: &TerminalExitStatus) -> String {
    let mut parts = Vec::new();
    if let Some(code) = exit_status.exit_code {
        parts.push(format!("exit code: {code}"));
    }
    if let Some(signal) = &exit_status.signal {
        parts.push(format!("signal: {signal}"));
    }
    if parts.is_empty() {
        "finished".to_string()
    } else {
        parts.join(", ")
    }
}

async fn poll_terminal_tool_call_output(
    terminal_runtime: &TerminalRuntime,
    session_id: &SessionId,
    tracked: &mut TrackedTerminalToolCall,
) -> Result<TerminalPollResult, TerminalRuntimeError> {
    let mut chunks: Vec<String> = Vec::new();
    let mut any_found = false;
    let mut all_exited = true;
    let include_headers = tracked.terminal_ids.len() > 1;

    for terminal_id in &tracked.terminal_ids {
        let from_offset = tracked.terminal_offsets.get(terminal_id).copied();
        let response = match terminal_runtime
            .terminal_output_delta(session_id.0.as_ref(), terminal_id, from_offset)
            .await
        {
            Ok(response) => response,
            Err(TerminalRuntimeError::InvalidParams(_)) => continue,
            Err(err) => return Err(err),
        };

        any_found = true;
        tracked
            .terminal_offsets
            .insert(terminal_id.clone(), response.next_offset);

        if response.exit_status.is_none() {
            all_exited = false;
        }

        let mut chunk = String::new();
        if include_headers {
            chunk.push_str(&format!("[Terminal: {terminal_id}]\n"));
        }

        if response.had_gap {
            chunk.push_str("[output truncated]\n");
        }

        if !response.output.is_empty() {
            chunk.push_str(&response.output);
            if !chunk.ends_with('\n') {
                chunk.push('\n');
            }
        }

        if response.truncated && from_offset.is_none() {
            chunk.push_str("[output truncated]\n");
        }

        if let Some(exit_status) = response.exit_status {
            if tracked.terminal_exit_reported.insert(terminal_id.clone()) {
                chunk.push_str(&format!(
                    "[terminal exited: {}]\n",
                    format_terminal_exit_status(&exit_status)
                ));
            }
        }

        if chunk.ends_with('\n') {
            chunk.pop();
        }
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
    }

    if !any_found {
        all_exited = false;
    }

    let append = tracked.has_emitted_output;
    if !chunks.is_empty() {
        tracked.has_emitted_output = true;
    }

    Ok(TerminalPollResult {
        output: if chunks.is_empty() {
            None
        } else {
            Some(chunks.join("\n\n"))
        },
        append,
        any_found,
        all_exited,
    })
}

async fn emit_terminal_output_update(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    tool_call_id: &str,
    output: String,
    append: bool,
) {
    // Safety cap: when a subprocess writes very fast between poll ticks,
    // the delta produced by `poll_terminal_tool_call_output` can still be
    // up to ~1 MB (the terminal buffer limit). Enforce the pipeline-wide
    // single-event cap (with ANSI-safe truncation) before emission so the
    // WS/IPC fanout never carries a multi-MB payload.
    let (payload, _append) = build_emit_payload(&output, append);
    emit_with_state(
        state,
        emitter,
        AcpEvent::ToolCallUpdate {
            tool_call_id: tool_call_id.to_string(),
            title: None,
            status: None,
            content: None,
            raw_input: None,
            raw_output: Some(payload),
            raw_output_append: Some(append),
            locations: None,
            meta: None,
            images: None,
        },
    )
    .await;
}

/// Publish `ToolWatchdogChanged` for a Cleared/TimedOut projection so
/// `SessionState`'s attach map drops the lease. No-op for other phases.
async fn emit_tool_watchdog_clear(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    projection: crate::acp::tool_watchdog::ToolWatchdogProjection,
) {
    use crate::acp::tool_watchdog::ToolWatchdogPhase;
    if matches!(
        projection.phase,
        ToolWatchdogPhase::Cleared | ToolWatchdogPhase::TimedOut
    ) {
        emit_with_state(state, emitter, AcpEvent::ToolWatchdogChanged { projection }).await;
    }
}

/// Publish Cleared/TimedOut projections returned by registry demotions.
async fn emit_tool_watchdog_clears(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    projections: impl IntoIterator<Item = crate::acp::tool_watchdog::ToolWatchdogProjection>,
) {
    for projection in projections {
        emit_tool_watchdog_clear(state, emitter, projection).await;
    }
}

/// Feed tool-watchdog leases from an authoritative tool_call / update fact.
///
/// Does **not** bind Terminal capability from this frame's terminal ids —
/// that races with multi-terminal accumulation. Capability is derived only
/// via [`tool_watchdog_sync_tracked_terminals`] from the accumulated
/// `TrackedTerminalToolCall` / fallback association map.
///
/// Returns the settle `error_code` when a cancel-owned lease was removed as
/// TimedOut/user_cancelled (so the caller can rewrite provider `completed` →
/// `failed` before SessionState applies a successful completion).
async fn tool_watchdog_on_tool_event(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    tool_call_id: &str,
    kind: &str,
    title: Option<&str>,
    status: Option<&str>,
    meta_marks_background: bool,
) -> Option<String> {
    use crate::acp::tool_watchdog::{
        classify_tool_category, CancellationCapability, WatchdogInstant,
    };

    let (attr, turn) = {
        let s = state.read().await;
        let turn = s.tool_watchdog_turn_stamp()?;
        (s.lease_attribution(), turn)
    };
    let at = WatchdogInstant::now();
    let category = classify_tool_category(kind, title);
    let final_status = is_final_tool_call_status(status);

    if meta_marks_background {
        // Acknowledged background handoff ends foreground ownership immediately.
        if let Some(outcome) = attr
            .register_or_touch_tool(&turn, tool_call_id, category, at)
            .await
        {
            // First-tool admission may retire a Grace fallback permanently.
            if let Some(cleared) = outcome.cleared {
                emit_tool_watchdog_clear(state, emitter, cleared).await;
            }
        }
        if let Some(projection) = attr.background_handoff(&turn, tool_call_id).await {
            // Drop actionable Grace/Warning/Cancelling from attach replay map.
            emit_tool_watchdog_clear(state, emitter, projection).await;
        }
        return None;
    }

    if final_status {
        // MCP terminal: deregister cancel token so registrations do not leak.
        if matches!(category, crate::acp::tool_watchdog::ToolCategory::Mcp) {
            let key = crate::acp::tool_watchdog::tool_lease_key(&turn, tool_call_id);
            if let Some(stamp) = attr.registry().tool_stamp(&key).await {
                if let Some(CancellationCapability::McpRequest { cancel_token }) =
                    attr.registry().lease_capability(&stamp.lease_id).await
                {
                    let mcp_reg = {
                        let s = state.read().await;
                        s.mcp_cancel_registry.clone()
                    };
                    let _ = mcp_reg.deregister(&stamp, cancel_token).await;
                }
            }
        }
        if let Some(status) = status {
            if let Some(outcome) = attr
                .register_or_touch_tool(&turn, tool_call_id, category, at)
                .await
            {
                if let Some(cleared) = outcome.cleared {
                    emit_tool_watchdog_clear(state, emitter, cleared).await;
                }
            }
            // Progress-before-complete may demote Grace→Running; still emit
            // Cleared so attach cannot replay a stale actionable projection.
            if let Some(apply) = attr.record_status(&turn, tool_call_id, status, at).await {
                if let Some(cleared) = apply.cleared {
                    emit_tool_watchdog_clear(state, emitter, cleared).await;
                }
            }
        }
        if let Some(projection) = attr.complete_tool(&turn, tool_call_id).await {
            // Emit Cleared on normal complete (no error_code) and TimedOut /
            // user_cancelled when a cancel claim already owned the outcome.
            // Return error_code so the provider ToolCallUpdate can be rewritten
            // from completed → failed (I2 late-final race).
            let settle_error = projection.error_code.clone();
            emit_tool_watchdog_clear(state, emitter, projection).await;
            return settle_error;
        }
        return None;
    }

    // Register first, then record status. Capability binding is deferred to
    // the accumulated association sync after tracking (never frame-only ids).
    let stamp = attr
        .register_or_touch_tool(&turn, tool_call_id, category, at)
        .await;
    if let Some(outcome) = stamp.as_ref() {
        // First tracked-tool admission may retire a Grace fallback permanently
        // (complete_turn cannot clear a lease already removed).
        if let Some(cleared) = outcome.cleared.clone() {
            emit_tool_watchdog_clear(state, emitter, cleared).await;
        }
    }
    if let Some(status) = status {
        // Semantic progress that demotes Warning/Grace → Running must publish
        // Cleared so attach/replay does not keep a stale Grace surface.
        if let Some(apply) = attr.record_status(&turn, tool_call_id, status, at).await {
            if let Some(cleared) = apply.cleared {
                emit_tool_watchdog_clear(state, emitter, cleared).await;
            }
        }
    }

    // MCP capability: only bind `McpRequest` when a real host cancel callback
    // is available. A always-true placeholder falsely reports specific cancel
    // success and (with status/bind version races) produces stale stamps that
    // force turn-escalation for unrelated work. Without a real cancel handle
    // the lease retains `Turn` capability and escalation uses session/cancel.
    //
    // When a real cancel is wired later:
    // 1. Re-fetch the current stamp after any status progress (status bumps version).
    // 2. `bind_capability` then re-register/store the **post-bind** stamp so
    //    cancel/deregister match the registry CAS stamp exactly.
    // 3. Deregister on terminal settle (see final_status path above).
    let _ = stamp;
    None
}

/// Sync Terminal/Turn capability from the **accumulated** host association
/// (TrackedTerminalToolCall / fallback binds), not a single frame's ids.
async fn tool_watchdog_sync_terminal_association(
    state: &Arc<RwLock<SessionState>>,
    tool_call_id: &str,
    terminal_ids: &[String],
) {
    if terminal_ids.is_empty() {
        return;
    }
    let (attr, turn, session_id) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        let session_id = s.external_id.clone().unwrap_or_default();
        (s.lease_attribution(), turn, session_id)
    };
    let _ = attr
        .sync_terminal_association(&turn, tool_call_id, &session_id, terminal_ids)
        .await;
}

/// Sync one tool from the live tracked map (post-register, pre-frontend emit).
async fn tool_watchdog_sync_tool_from_tracked(
    state: &Arc<RwLock<SessionState>>,
    tool_call_id: &str,
    tracked: Option<&HashMap<String, TrackedTerminalToolCall>>,
) {
    let Some(tracked) = tracked else {
        return;
    };
    let Some(entry) = tracked.get(tool_call_id) else {
        return;
    };
    if entry.terminal_ids.is_empty() {
        return;
    }
    tool_watchdog_sync_terminal_association(state, tool_call_id, &entry.terminal_ids).await;
}

/// After tracking/fallback merge, re-derive capability for every tool whose
/// association is non-empty. Singleton → Terminal; multi → Turn downgrade.
///
/// Must run **before** any frontend await when association just became multi
/// so a concurrent scan never snapshots a stale Terminal(A) capability.
async fn tool_watchdog_sync_tracked_terminals(
    state: &Arc<RwLock<SessionState>>,
    tracked: &HashMap<String, TrackedTerminalToolCall>,
) {
    for (tool_call_id, entry) in tracked {
        if entry.terminal_ids.is_empty() {
            continue;
        }
        tool_watchdog_sync_terminal_association(state, tool_call_id, &entry.terminal_ids).await;
    }
}

async fn tool_watchdog_record_agent_activity(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    text: &str,
) {
    use crate::acp::tool_watchdog::WatchdogInstant;
    let (attr, turn) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        (s.lease_attribution(), turn)
    };
    if let Some(cleared) = attr
        .record_agent_activity(&turn, text, WatchdogInstant::now())
        .await
    {
        emit_tool_watchdog_clear(state, emitter, cleared).await;
    }
}

async fn tool_watchdog_start_turn(state: &Arc<RwLock<SessionState>>) {
    use crate::acp::tool_watchdog::WatchdogInstant;
    let (attr, turn) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        (s.lease_attribution(), turn)
    };
    attr.start_turn(turn, WatchdogInstant::now()).await;
}

async fn tool_watchdog_complete_turn(state: &Arc<RwLock<SessionState>>, emitter: &EventEmitter) {
    let (attr, turn) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        (s.lease_attribution(), turn)
    };
    let projections = attr.complete_turn(&turn).await;
    // Emit timed_out / user_cancelled settle and cleared projections.
    emit_tool_watchdog_clears(state, emitter, projections).await;
}

async fn tool_watchdog_pause_permission(state: &Arc<RwLock<SessionState>>, emitter: &EventEmitter) {
    let (attr, turn) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        (s.lease_attribution(), turn)
    };
    let cleared = attr.pause_permission(&turn).await;
    emit_tool_watchdog_clears(state, emitter, cleared).await;
}

async fn tool_watchdog_resume(state: &Arc<RwLock<SessionState>>) {
    use crate::acp::tool_watchdog::WatchdogInstant;
    let (attr, turn) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        (s.lease_attribution(), turn)
    };
    attr.resume(&turn, WatchdogInstant::now()).await;
}

async fn tool_watchdog_terminal_offset_for(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    tool_call_id: &str,
    terminal_id: &str,
    next_offset: u64,
) {
    use crate::acp::tool_watchdog::WatchdogInstant;
    let (attr, turn) = {
        let s = state.read().await;
        let Some(turn) = s.tool_watchdog_turn_stamp() else {
            return;
        };
        (s.lease_attribution(), turn)
    };
    if let Some(apply) = attr
        .record_terminal_offset_for(
            &turn,
            tool_call_id,
            terminal_id,
            next_offset,
            WatchdogInstant::now(),
        )
        .await
    {
        if let Some(cleared) = apply.cleared {
            emit_tool_watchdog_clear(state, emitter, cleared).await;
        }
    }
}

async fn poll_tracked_terminal_tool_calls(
    terminal_runtime: &TerminalRuntime,
    session_id: &SessionId,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    tracked: &mut HashMap<String, TrackedTerminalToolCall>,
) {
    if tracked.is_empty() {
        return;
    }

    let tool_call_ids: Vec<String> = tracked.keys().cloned().collect();
    let mut remove_ids: Vec<String> = Vec::new();

    for tool_call_id in tool_call_ids {
        let Some(entry) = tracked.get_mut(&tool_call_id) else {
            continue;
        };
        if entry.terminal_ids.is_empty() {
            remove_ids.push(tool_call_id.clone());
            continue;
        }

        let poll_result =
            match poll_terminal_tool_call_output(terminal_runtime, session_id, entry).await {
                Ok(result) => result,
                Err(err) => {
                    tracing::error!(
                        "[ACP] Failed to poll terminal output for tool call {}: {:?}",
                        tool_call_id,
                        err
                    );
                    continue;
                }
            };

        if poll_result.any_found {
            entry.missing_polls = 0;
        } else {
            entry.missing_polls = entry.missing_polls.saturating_add(1);
        }

        // Authoritative terminal progress: renew when ANY associated terminal
        // advances its own offset (per-terminal fingerprint), not just max.
        for (terminal_id, offset) in entry.terminal_offsets.iter() {
            tool_watchdog_terminal_offset_for(state, emitter, &tool_call_id, terminal_id, *offset)
                .await;
        }
        if poll_result.all_exited && poll_result.any_found {
            let (attr, turn) = {
                let s = state.read().await;
                match s.tool_watchdog_turn_stamp() {
                    Some(turn) => (Some(s.lease_attribution()), Some(turn)),
                    None => (None, None),
                }
            };
            if let (Some(attr), Some(turn)) = (attr, turn) {
                if let Some(apply) = attr
                    .record_terminal_exit(
                        &turn,
                        &tool_call_id,
                        crate::acp::tool_watchdog::WatchdogInstant::now(),
                    )
                    .await
                {
                    if let Some(cleared) = apply.cleared {
                        emit_tool_watchdog_clear(state, emitter, cleared).await;
                    }
                }
            }
        }

        if let Some(output) = poll_result.output {
            emit_terminal_output_update(state, emitter, &tool_call_id, output, poll_result.append)
                .await;
        }

        if (is_final_tool_call_status(entry.status.as_deref())
            && (!poll_result.any_found || poll_result.all_exited))
            || entry.missing_polls >= TERMINAL_POLL_MISSING_LIMIT
        {
            remove_ids.push(tool_call_id.clone());
        }
    }

    for tool_call_id in remove_ids {
        tracked.remove(&tool_call_id);
    }
}

/// Append the just-ended turn's observed span to the timing journal (see
/// `crate::turn_timings`). `probe` is `Some((send_stamp, prompt_hash))` only
/// on agents codeg journals for (Cursor) and is consumed on the first
/// journaling terminal path, so a turn appends at most one line.
///
/// ONLY cleanly completed turns are journaled — callers gate on the
/// NORMALIZED stop reason (`reason_str == "end_turn"`, which a raw
/// `end_turn` with no agent output does NOT satisfy: it reclassifies to
/// `"empty"` and is excluded). A canceled or empty turn may never have been
/// persisted by Cursor at all, and journaling such a phantom re-opens the
/// misassignment the parser's guards exist to prevent: a later same-hash
/// store turn could pair with the phantom's line even across non-contiguous
/// positions (Codex review R4-2). An unjournaled-but-persisted turn
/// mid-session merely stops the reverse walk (older turns lose their
/// clocks); when such turns make up the session's TAIL, the second accepted
/// residual in `turn_timings`' module docs applies (a stale journal tail can
/// hash-collide with the store's newest turn).
///
/// The append is queued to the journal's single-writer thread and awaited
/// with a short timeout: the normal case lands in microseconds BEFORE the
/// TurnComplete emit (so the post-turn reparse deterministically sees it),
/// while a hung filesystem blocks neither the turn loop nor any Tokio pool —
/// the queued job just lands late (still in order; the FIFO queue is what
/// makes overtaking structurally impossible) or is dropped at the queue cap.
async fn journal_turn_span(
    probe: &mut Option<(u64, String, u64)>,
    connection_id: &str,
    session_id: &str,
) {
    let Some((started_at_ms, prompt_sha, ord)) = probe.take() else {
        return;
    };
    let ack = crate::turn_timings::enqueue_turn_timing(
        crate::paths::codeg_turn_timings_root(),
        crate::turn_timings::CURSOR_JOURNAL_AGENT.to_string(),
        session_id.to_string(),
        crate::turn_timings::TurnTiming {
            v: crate::turn_timings::TURN_TIMING_SCHEMA_VERSION,
            ord,
            conn: connection_id.to_string(),
            prompt_sha,
            started_at_ms,
            ended_at_ms: crate::turn_timings::now_epoch_ms(),
        },
    );
    // Determinism window only — a timeout (or a dropped job's closed channel)
    // means the entry lands late or not at all, degrading that turn to a
    // missing footer clock. (See `turn_timings`' module docs for the two
    // narrow accepted residuals where missing lines can shift alignment.)
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ack).await;
}

fn map_prompt_blocks(blocks: Vec<PromptInputBlock>) -> Vec<ContentBlock> {
    blocks
        .into_iter()
        .map(|block| match block {
            PromptInputBlock::Text { text } => ContentBlock::Text(TextContent::new(text)),
            PromptInputBlock::Image {
                data,
                mime_type,
                uri,
            } => ContentBlock::Image(ImageContent::new(data, mime_type).uri(uri)),
            PromptInputBlock::Resource {
                uri,
                mime_type,
                text,
                blob,
            } => {
                let resource = match (text, blob) {
                    (Some(text_value), _) => {
                        let content =
                            TextResourceContents::new(text_value, uri.clone()).mime_type(mime_type);
                        EmbeddedResourceResource::TextResourceContents(content)
                    }
                    (None, Some(blob_value)) => {
                        let content =
                            BlobResourceContents::new(blob_value, uri.clone()).mime_type(mime_type);
                        EmbeddedResourceResource::BlobResourceContents(content)
                    }
                    (None, None) => {
                        let content =
                            TextResourceContents::new("", uri.clone()).mime_type(mime_type);
                        EmbeddedResourceResource::TextResourceContents(content)
                    }
                };
                ContentBlock::Resource(EmbeddedResource::new(resource))
            }
            PromptInputBlock::ResourceLink {
                uri,
                name,
                mime_type,
                description,
            } => {
                let mut link = ResourceLink::new(name, uri);
                link.mime_type = mime_type;
                link.description = description;
                ContentBlock::ResourceLink(link)
            }
        })
        .collect()
}

/// Result when the conversation loop exits due to a fork request.
struct ForkExitInfo {
    fork_response: sacp::schema::ForkSessionResponse,
    /// Raw top-level `models` from the fork response (Grok per-model effort data),
    /// captured before the typed deserialize drops it. `None` when absent.
    fork_models_raw: Option<serde_json::Value>,
    original_session_id: String,
    reply: tokio::sync::oneshot::Sender<Result<crate::acp::types::ForkProtocolResult, AcpError>>,
    connection: ConnectionTo<Agent>,
}

/// After `run_conversation_loop` returns, handle normal exit or fork transition.
///
/// When fork is requested, the original session has already been dropped by the
/// caller.  We attach to the forked session (S2) directly using the
/// `ForkSessionResponse` — no separate `session/load` is needed because S2 was
/// just created in-memory by the agent on this connection.
#[allow(clippy::too_many_arguments)]
async fn handle_fork_or_exit(
    loop_result: Result<Option<ForkExitInfo>, sacp::Error>,
    conn_id: &str,
    emitter: &EventEmitter,
    state: &Arc<RwLock<SessionState>>,
    agent_type: AgentType,
    perms: &PendingPermissions,
    cmd_rx: &mut mpsc::Receiver<ConnectionCommand>,
    control_rx: &mut mpsc::Receiver<ConnectionControl>,
    cmd_liveness_rx: &mut watch::Receiver<bool>,
    control_liveness_rx: &mut watch::Receiver<bool>,
    terminal_runtime: Arc<TerminalRuntime>,
    terminal_assoc: Arc<std::sync::Mutex<TerminalAssocFallback>>,
    file_system_runtime: Arc<FileSystemRuntime>,
    _cwd: &std::path::Path,
    cwd_string: &str,
    // Immutable connection shell snapshot — never re-read from settings.
    shell_spec: &ResolvedShellSpec,
    // Same immutable launch route plan as the original session loop.
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
    // Threaded through from run_connection: the connection-scoped prompt
    // ledger (the forked session's loop keeps fingerprinting into the SAME
    // ledger the still-running watcher consumes from).
    prompt_ledger: &background_watch::PromptLedger,
    // Same process-lifetime one-shot as the original loop — fork must not
    // re-inject a superseding `<codeg_terminal_context>` block.
    terminal_prompt_context: &TerminalPromptContext,
    // Threaded through from run_connection so the forked session's
    // run_conversation_loop call has the same delegation cascade
    // capability as the original.
    delegation_injection: Option<&DelegationInjection>,
) -> Result<(), sacp::Error> {
    let fork_info = match loop_result {
        Ok(Some(info)) => info,
        Ok(None) => return Ok(()),
        Err(e) => return Err(e),
    };

    let cx = fork_info.connection;
    let fork_resp = fork_info.fork_response;
    let fork_models_raw = fork_info.fork_models_raw;
    let new_sid = fork_resp.session_id.0.to_string();

    tracing::info!(
        "[ACP] Fork transition: attaching to forked session {} (original: {})",
        new_sid,
        fork_info.original_session_id
    );

    // Reply protocol-level result to manager.fork_session, which will combine
    // it with the freshly-created sibling row id to produce the wire ForkResultInfo.
    let _ = fork_info
        .reply
        .send(Ok(crate::acp::types::ForkProtocolResult {
            forked_session_id: new_sid.clone(),
            original_session_id: fork_info.original_session_id,
        }));

    // Build a NewSessionResponse from the ForkSessionResponse so we can
    // attach directly — the forked session is already live on this process.
    let initial_config_options = fork_resp.config_options.clone();
    let new_resp = NewSessionResponse::new(fork_resp.session_id)
        .modes(fork_resp.modes)
        .config_options(fork_resp.config_options)
        .meta(fork_resp.meta);
    let grok_meta = if agent_type == AgentType::Grok {
        new_resp.meta.clone()
    } else {
        None
    };
    // Opportunistic: grok may carry per-model effort data on a fork response.
    let grok_effort_specs =
        (agent_type == AgentType::Grok).then(|| parse_grok_effort_specs(fork_models_raw.as_ref()));
    let mut session = cx.attach_session(new_resp, Default::default())?;

    // A fork is a new session id, hence a new transcript file. Its history
    // starts empty and accumulates from the fork point — the pre-fork turns
    // stay in the parent's transcript, which is what forking means.
    record_transcript_header(agent_type, &new_sid, cwd_string);
    emit_with_state(
        state,
        emitter,
        AcpEvent::SessionStarted {
            session_id: new_sid.clone(),
        },
    )
    .await;
    emit_session_modes(state, emitter, session.modes()).await;
    apply_and_emit_session_config_options(
        &cx,
        &mut session,
        state,
        emitter,
        agent_type,
        grok_meta.as_ref(),
        grok_effort_specs.as_ref(),
        None,
        &BTreeMap::new(),
        initial_config_options.unwrap_or_default(),
        file_system_runtime.as_ref(),
    )
    .await;
    emit_selectors_ready(state, emitter).await;

    let loop_result = run_conversation_loop(
        &mut session,
        conn_id,
        emitter,
        state,
        agent_type,
        perms,
        cmd_rx,
        control_rx,
        cmd_liveness_rx,
        control_liveness_rx,
        terminal_runtime.clone(),
        terminal_assoc.clone(),
        file_system_runtime.clone(),
        cwd_string,
        true, // fork already succeeded on this process
        shell_spec,
        route_plan,
        prompt_ledger,
        terminal_prompt_context,
        delegation_injection,
    )
    .await;
    terminal_runtime.release_all_for_session(&new_sid).await;
    if let Ok(mut bridge) = terminal_assoc.lock() {
        bridge.clear_session(&new_sid);
    }
    drop(session);

    // Recursively handle nested forks
    Box::pin(handle_fork_or_exit(
        loop_result,
        conn_id,
        emitter,
        state,
        agent_type,
        perms,
        cmd_rx,
        control_rx,
        cmd_liveness_rx,
        control_liveness_rx,
        terminal_runtime,
        terminal_assoc,
        file_system_runtime,
        _cwd,
        cwd_string,
        shell_spec,
        route_plan,
        prompt_ledger,
        terminal_prompt_context,
        delegation_injection,
    ))
    .await
}

/// Main conversation command loop: wait for frontend commands and process them.
///
/// Map ACP `StopReason` to a stable lowercase string carried in the
/// `TurnComplete` event. Covers all 5 spec variants so non-success reasons
/// (`Refusal`/`MaxTokens`/`MaxTurnRequests`) keep their semantics instead of
/// collapsing to `"unknown"` — the lifecycle subscriber and frontend rely on
/// this distinction. The wildcard arm exists because the upstream enum is
/// `#[non_exhaustive]`.
fn stop_reason_to_str(reason: StopReason) -> &'static str {
    match reason {
        StopReason::EndTurn => "end_turn",
        StopReason::Cancelled => "cancelled",
        StopReason::Refusal => "refusal",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        _ => "unknown",
    }
}

/// Map a parent turn's stop-reason string onto the join-only ownership cascade
/// reason. Clean `end_turn` still drains live Codeg children as
/// [`ParentTurnEndReason::JoinAbandoned`] (no-op when none remain).
fn parent_turn_end_reason(stop_reason: &str) -> crate::acp::delegation::types::ParentTurnEndReason {
    use crate::acp::delegation::types::ParentTurnEndReason;
    match stop_reason {
        "cancelled" => ParentTurnEndReason::ParentCanceled,
        "end_turn" => ParentTurnEndReason::JoinAbandoned,
        "refusal" | "max_tokens" | "max_turn_requests" | "empty" | "unknown" => {
            ParentTurnEndReason::ParentTurnFailed
        }
        _ => ParentTurnEndReason::ParentTurnFailed,
    }
}

async fn cleanup_delegation_parent(
    injection: &DelegationInjection,
    connection_id: &str,
    state: &Arc<RwLock<SessionState>>,
) {
    // First cleanup action: cancel every live continuation worker for this
    // parent connection before any await, state read, lease/token revocation,
    // or Broker drain. Preserve the Weak/no-cycle fallback when upgrade fails.
    let coordinator = injection.continuation_coordinator.upgrade();
    if let Some(ref coordinator) = coordinator {
        coordinator.cancel_workers_for_parent(connection_id);
    }

    let (token, conversation_id) = {
        let state = state.read().await;
        (state.delegation_token.clone(), state.conversation_id)
    };
    if let Some(token) = token {
        injection.leases.revoke(&token).await;
        injection.tokens.revoke(&token).await;
    }
    let cause = injection.parent_connection_exit_causes.take(connection_id);
    let termination = cause.termination().clone();
    let context = crate::acp::termination::ParentEndContext {
        reason: crate::acp::delegation::types::ParentTurnEndReason::ParentDisconnected,
        termination,
    };
    if let Some(coordinator) = coordinator {
        coordinator
            .handle_parent_connection_exit(connection_id, conversation_id, cause)
            .await;
    } else {
        injection
            .broker
            .cancel_by_parent_with_context(connection_id, context)
            .await;
    }
    // Reclaim a parked `ask_user_question` instead of waiting for the
    // companion's ask socket to close; dropping the sender declines it cleanly.
    injection
        .questions
        .cancel_questions_by_parent(connection_id)
        .await;
    injection
        .plan_approvals
        .cancel_plan_approvals_by_parent(connection_id)
        .await;
}

fn reject_suspension_lease(lease: &mut SuspensionLease, code: &'static str) {
    if let Some(reply) = lease.reply.take() {
        let _ = reply.send(Err(AcpError::protocol(code)));
    }
}

#[cfg(test)]
fn install_suspension_lease(
    state: &SessionState,
    active_prompt_generation: u64,
    slot: &mut Option<SuspensionLease>,
    lease: SuspensionLease,
) {
    install_suspension_lease_from_snapshot(
        state.turn_in_flight,
        state.active_turn_generation,
        active_prompt_generation,
        slot,
        lease,
    );
}

fn install_suspension_lease_from_snapshot(
    turn_in_flight: bool,
    active_turn_generation: Option<u64>,
    active_prompt_generation: u64,
    slot: &mut Option<SuspensionLease>,
    mut lease: SuspensionLease,
) {
    let rejection = if !turn_in_flight || active_turn_generation.is_none() {
        Some("suspend_no_active_turn")
    } else if active_turn_generation != Some(active_prompt_generation)
        || lease.parent_turn_generation != active_prompt_generation
    {
        Some("suspend_turn_generation_mismatch")
    } else if slot.is_some() {
        Some("suspend_already_pending")
    } else {
        None
    };

    if let Some(code) = rejection {
        reject_suspension_lease(&mut lease, code);
    } else {
        *slot = Some(lease);
    }
}

/// Extension and `SessionMessage::StopReason` terminals are diagnostic-only
/// while a suspension owns the turn. The bound prompt RPC remains authoritative.
fn record_suspension_terminal_diagnostic(
    lease: Option<&SuspensionLease>,
    diagnostic: &mut Option<String>,
    reason: &str,
) -> bool {
    if lease.is_none() {
        return false;
    }
    *diagnostic = Some(reason.to_string());
    true
}

#[allow(clippy::too_many_arguments)]
async fn emit_ordinary_turn_finalization(
    stop_reason: &str,
    reason: crate::acp::delegation::types::ParentTurnEndReason,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    connection_id: &str,
    session_id: &str,
    agent_type: AgentType,
    mark_awaiting_reply: bool,
    broker: Option<&crate::acp::delegation::broker::DelegationBroker>,
) {
    if let Some(err_event) = turn_failure_error_event(stop_reason, agent_type) {
        emit_with_state(state, emitter, err_event).await;
    }
    // Clear leases while the turn stamp is still active on SessionState.
    tool_watchdog_complete_turn(state, emitter).await;
    emit_with_state(
        state,
        emitter,
        AcpEvent::TurnComplete {
            session_id: session_id.to_string(),
            stop_reason: stop_reason.to_string(),
            agent_type: agent_type.to_string(),
            mark_awaiting_reply,
            termination_source: None,
            provider_turn_id: None,
        },
    )
    .await;
    if let Some(broker) = broker {
        broker.cancel_by_parent_turn(connection_id, reason).await;
    }
}

fn classify_turn_terminal(
    source: &TurnTerminalSource<'_>,
    lease: Option<&SuspensionLease>,
) -> TurnFinalizationDisposition {
    match source {
        // User intent is authoritative even after a lease was installed.
        TurnTerminalSource::UserCancel => TurnFinalizationDisposition::UserCancelled,
        TurnTerminalSource::SuspensionDrainTimeout => TurnFinalizationDisposition::SuspensionFailed,
        TurnTerminalSource::Upstream("cancelled") if lease.is_some() => {
            TurnFinalizationDisposition::DelegationSuspended
        }
        TurnTerminalSource::Upstream(_) if lease.is_some() => {
            TurnFinalizationDisposition::SuspensionFailed
        }
        TurnTerminalSource::Upstream(reason) => {
            TurnFinalizationDisposition::NaturalEnd(parent_turn_end_reason(reason))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize_turn_terminal(
    source: TurnTerminalSource<'_>,
    suspension: &mut Option<SuspensionLease>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    connection_id: &str,
    session_id: &str,
    agent_type: AgentType,
    mark_awaiting_reply: bool,
    broker: Option<&crate::acp::delegation::broker::DelegationBroker>,
) -> TurnFinalizationDisposition {
    let disposition = classify_turn_terminal(&source, suspension.as_ref());

    match disposition {
        TurnFinalizationDisposition::DelegationSuspended => {
            let mut lease = suspension.take().expect("classified with suspension lease");
            let identity_matches =
                lease.connection_id == connection_id && lease.session_id == session_id;
            // Complete old-generation foreground leases BEFORE clear_suspended_turn
            // drops active_turn_generation — after that, no code can reconstruct
            // the old turn stamp and leases would leak until disconnect.
            if identity_matches {
                tool_watchdog_complete_turn(state, emitter).await;
            }
            let cleared = if identity_matches {
                state
                    .write()
                    .await
                    .clear_suspended_turn(lease.parent_turn_generation)
            } else {
                false
            };
            if !cleared {
                reject_suspension_lease(&mut lease, "suspend_session_fence_mismatch");
                return TurnFinalizationDisposition::SuspensionFailed;
            }
            emit_with_state(
                state,
                emitter,
                AcpEvent::StatusChanged {
                    status: ConnectionStatus::Connected,
                },
            )
            .await;
            if let Some(reply) = lease.reply.take() {
                let _ = reply.send(Ok(SuspensionAck {
                    continuation_id: lease.continuation_id,
                    parent_turn_generation: lease.parent_turn_generation,
                }));
            }
            TurnFinalizationDisposition::DelegationSuspended
        }
        TurnFinalizationDisposition::SuspensionFailed
            if matches!(source, TurnTerminalSource::SuspensionDrainTimeout) =>
        {
            if let Some(mut lease) = suspension.take() {
                reject_suspension_lease(&mut lease, "suspend_drain_timeout");
            }
            // No TurnComplete on this path — clear fence id explicitly.
            state.write().await.active_provider_turn_id = None;
            TurnFinalizationDisposition::SuspensionFailed
        }
        TurnFinalizationDisposition::UserCancelled => {
            if let Some(mut lease) = suspension.take() {
                reject_suspension_lease(&mut lease, "suspend_cancelled_by_user");
            }
            tool_watchdog_complete_turn(state, emitter).await;
            // Snapshot provider turn id before TurnComplete apply clears it.
            let provider_turn_id = state.write().await.active_provider_turn_id.take();
            emit_with_state(
                state,
                emitter,
                AcpEvent::TurnComplete {
                    session_id: session_id.to_string(),
                    stop_reason: "cancelled".into(),
                    agent_type: agent_type.to_string(),
                    mark_awaiting_reply,
                    termination_source: Some(TurnTerminationSource::UserStop),
                    provider_turn_id,
                },
            )
            .await;
            if let Some(broker) = broker {
                broker
                    .cancel_by_parent_turn(
                        connection_id,
                        crate::acp::delegation::types::ParentTurnEndReason::ParentCanceled,
                    )
                    .await;
            }
            TurnFinalizationDisposition::UserCancelled
        }
        TurnFinalizationDisposition::NaturalEnd(reason) => {
            let stop_reason = match source {
                TurnTerminalSource::Upstream(reason) => reason,
                _ => unreachable!("natural/failure finalization requires upstream reason"),
            };
            emit_ordinary_turn_finalization(
                stop_reason,
                reason,
                state,
                emitter,
                connection_id,
                session_id,
                agent_type,
                mark_awaiting_reply,
                broker,
            )
            .await;
            TurnFinalizationDisposition::NaturalEnd(reason)
        }
        TurnFinalizationDisposition::SuspensionFailed => {
            let stop_reason = match source {
                TurnTerminalSource::Upstream(reason) => reason,
                _ => unreachable!("suspension failure finalization requires upstream reason"),
            };
            if let Some(mut lease) = suspension.take() {
                reject_suspension_lease(&mut lease, "suspend_turn_ended_before_cancel");
            }
            emit_ordinary_turn_finalization(
                stop_reason,
                parent_turn_end_reason(stop_reason),
                state,
                emitter,
                connection_id,
                session_id,
                agent_type,
                mark_awaiting_reply,
                broker,
            )
            .await;
            TurnFinalizationDisposition::SuspensionFailed
        }
    }
}

type AncillaryCommandFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

enum ConversationInput {
    Command(ConnectionCommand),
    Control(ConnectionControl),
    ChannelsClosed,
}

#[derive(Clone, Copy)]
struct SuspensionAdmissionSnapshot {
    turn_in_flight: bool,
    active_turn_generation: Option<u64>,
}

enum ActiveTerminalControl {
    UserCancel,
    /// Watchdog-driven turn cancel (timeout or user-stop claim that escalated).
    WatchdogCancel {
        cause: crate::acp::tool_watchdog::CancelCause,
    },
    Disconnect,
}

#[allow(clippy::too_many_arguments)]
fn apply_suspension_control(
    continuation_id: String,
    parent_turn_generation: u64,
    reply: oneshot::Sender<Result<SuspensionAck, AcpError>>,
    admission: SuspensionAdmissionSnapshot,
    turn_generation: u64,
    suspension: &mut Option<SuspensionLease>,
    suspension_deadline: &mut Pin<Box<tokio::time::Sleep>>,
    conn_id: &str,
    sid: &SessionId,
    cx: &ConnectionTo<Agent>,
) {
    let was_empty = suspension.is_none();
    install_suspension_lease_from_snapshot(
        admission.turn_in_flight,
        admission.active_turn_generation,
        turn_generation,
        suspension,
        SuspensionLease {
            continuation_id,
            parent_turn_generation,
            connection_id: conn_id.to_string(),
            session_id: sid.0.to_string(),
            reply: Some(reply),
        },
    );
    if was_empty && suspension.is_some() {
        suspension_deadline.as_mut().reset(
            tokio::time::Instant::now()
                + std::time::Duration::from_millis(SUSPENSION_DRAIN_TIMEOUT_MS),
        );
        let _ = cx.send_notification_to(Agent, CancelNotification::new(sid.clone()));
    }
}

#[allow(clippy::too_many_arguments)]
fn drain_ready_active_controls(
    control_rx: &mut mpsc::Receiver<ConnectionControl>,
    admission: SuspensionAdmissionSnapshot,
    turn_generation: u64,
    suspension: &mut Option<SuspensionLease>,
    suspension_deadline: &mut Pin<Box<tokio::time::Sleep>>,
    conn_id: &str,
    sid: &SessionId,
    cx: &ConnectionTo<Agent>,
    terminal_runtime: &std::sync::Arc<crate::acp::terminal_runtime::TerminalRuntime>,
) -> Option<ActiveTerminalControl> {
    let ready_controls = control_rx.len();
    for _ in 0..ready_controls {
        match control_rx.try_recv() {
            Ok(ConnectionControl::SuspendForDelegation {
                continuation_id,
                parent_turn_generation,
                reply,
            }) => apply_suspension_control(
                continuation_id,
                parent_turn_generation,
                reply,
                admission,
                turn_generation,
                suspension,
                suspension_deadline,
                conn_id,
                sid,
                cx,
            ),
            Ok(ConnectionControl::CancelTerminal {
                session_id,
                terminal_id,
                reply,
            }) => {
                admit_cancel_terminal_control(terminal_runtime, session_id, terminal_id, reply);
            }
            Ok(ConnectionControl::Cancel) => return Some(ActiveTerminalControl::UserCancel),
            Ok(ConnectionControl::CancelTurn {
                turn_generation: expected_gen,
                cause,
            }) => {
                // Generation-guarded: ignore stale claims for a prior turn.
                if Some(expected_gen) == admission.active_turn_generation {
                    return Some(ActiveTerminalControl::WatchdogCancel { cause });
                }
            }
            Ok(ConnectionControl::Disconnect) => return Some(ActiveTerminalControl::Disconnect),
            Err(_) => break,
        }
    }
    None
}

/// Admit a host `CancelTerminal` control message without blocking the select loop.
///
/// Acks immediately after spawning a detached process-tree kill under
/// [`TERMINAL_KILL_EXECUTOR_TIMEOUT`]. Never awaits kill completion here.
fn admit_cancel_terminal_control(
    terminal_runtime: &std::sync::Arc<crate::acp::terminal_runtime::TerminalRuntime>,
    session_id: String,
    terminal_id: String,
    reply: oneshot::Sender<Result<(), crate::acp::terminal_runtime::TerminalRuntimeError>>,
) {
    use crate::acp::tool_watchdog::TERMINAL_KILL_EXECUTOR_TIMEOUT;
    use sacp::schema::{KillTerminalRequest, SessionId, TerminalId};

    let runtime = std::sync::Arc::clone(terminal_runtime);
    // Admission ack first so the control lane never waits on process-tree exit.
    let _ = reply.send(Ok(()));
    tokio::spawn(async move {
        let req =
            KillTerminalRequest::new(SessionId::new(session_id), TerminalId::new(terminal_id));
        let kill_fut = runtime.kill_terminal(req);
        match tokio::time::timeout(TERMINAL_KILL_EXECUTOR_TIMEOUT, kill_fut).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                tracing::warn!("[ACP] detached CancelTerminal kill failed: {err:?}");
            }
            Err(_) => {
                tracing::warn!(
                    "[ACP] detached CancelTerminal kill exceeded {:?}",
                    TERMINAL_KILL_EXECUTOR_TIMEOUT
                );
            }
        }
    });
}

fn start_ancillary_command(
    command: ConnectionCommand,
    cx: ConnectionTo<Agent>,
    sid: SessionId,
    state: Arc<RwLock<SessionState>>,
    emitter: EventEmitter,
    file_system_runtime: Arc<FileSystemRuntime>,
    agent_type: AgentType,
) -> Result<AncillaryCommandFuture, ConnectionCommand> {
    match command {
        ConnectionCommand::SetMode { mode_id } => Ok(Box::pin(async move {
            let req = SetSessionModeRequest::new(sid, mode_id.clone());
            match cx.send_request_to(Agent, req).block_task().await {
                Ok(_) => {
                    sync_file_system_outside_access(
                        file_system_runtime.as_ref(),
                        agent_type,
                        Some(&mode_id),
                    );
                    emit_with_state(&state, &emitter, AcpEvent::ModeChanged { mode_id }).await;
                }
                Err(error) => {
                    emit_with_state(
                        &state,
                        &emitter,
                        AcpEvent::Error {
                            message: format!("Failed to set mode: {error}"),
                            agent_type: agent_type.to_string(),
                            code: None,
                            terminal: false,
                        },
                    )
                    .await;
                }
            }
        })),
        ConnectionCommand::SetConfigOption {
            config_id,
            value_id,
        } => Ok(Box::pin(async move {
            let set_result = if agent_type == AgentType::Grok {
                set_grok_config_option(&cx, &sid, &state, &emitter, config_id, value_id).await
            } else {
                let is_mode = config_id == "mode";
                let mode_value = value_id.clone();
                let result = set_session_config_option(
                    &cx, &sid, &state, &emitter, agent_type, config_id, value_id,
                )
                .await;
                if result.is_ok() && is_mode {
                    sync_file_system_outside_access(
                        file_system_runtime.as_ref(),
                        agent_type,
                        Some(&mode_value),
                    );
                }
                result
            };
            if let Err(error) = set_result {
                emit_with_state(
                    &state,
                    &emitter,
                    AcpEvent::Error {
                        message: format!("Failed to set config option: {error}"),
                        agent_type: agent_type.to_string(),
                        code: None,
                        terminal: false,
                    },
                )
                .await;
            }
        })),
        command => Err(command),
    }
}

struct BoundPromptFinalization {
    status_restored_by_suspension: bool,
    disconnect_requested: bool,
}

#[allow(clippy::too_many_arguments)]
async fn finalize_bound_prompt_response(
    prompt_result: Result<sacp::schema::PromptResponse, sacp::Error>,
    suspension: &mut Option<SuspensionLease>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    conn_id: &str,
    sid: &SessionId,
    agent_type: AgentType,
    mark_awaiting_reply: bool,
    terminal_assoc: &Arc<std::sync::Mutex<TerminalAssocFallback>>,
    tracked_terminal_tool_calls: &mut HashMap<String, TrackedTerminalToolCall>,
    terminal_runtime: &Arc<TerminalRuntime>,
    perms: &PendingPermissions,
    turn_had_agent_output: bool,
    turn_started_at_ms: u64,
    turn_timing_probe: &mut Option<(u64, String, u64)>,
    broker: Option<&crate::acp::delegation::broker::DelegationBroker>,
) -> Result<BoundPromptFinalization, sacp::Error> {
    let reason = match prompt_result {
        Ok(response) => response.stop_reason,
        Err(error) => {
            if let Some(mut lease) = suspension.take() {
                reject_suspension_lease(&mut lease, "suspend_prompt_response_failed");
            }
            return Err(error);
        }
    };
    let bound = merge_terminal_assoc_binds(
        sid.0.as_ref(),
        terminal_assoc.as_ref(),
        tracked_terminal_tool_calls,
    );
    if !bound.is_empty() || !tracked_terminal_tool_calls.is_empty() {
        tool_watchdog_sync_tracked_terminals(state, tracked_terminal_tool_calls).await;
    }
    if !tracked_terminal_tool_calls.is_empty() {
        poll_tracked_terminal_tool_calls(
            terminal_runtime.as_ref(),
            sid,
            state,
            emitter,
            tracked_terminal_tool_calls,
        )
        .await;
    }
    let raw_reason_str = stop_reason_to_str(reason);
    let reason_str = rewrite_end_turn_if_empty(raw_reason_str, turn_had_agent_output);
    if reason_str == "end_turn" {
        journal_turn_span(turn_timing_probe, conn_id, &sid.0).await;
    }
    record_turn_end(
        agent_type,
        &sid.0,
        reason_str,
        turn_started_at_ms,
        current_session_model_id(state).await,
    )
    .await;
    tracing::info!(
        connection_id = %conn_id,
        session_id = %sid.0,
        agent = %agent_type,
        stop_reason = %reason_str,
        turn_had_agent_output,
        source = "prompt_response",
        "[ACP] completing turn from session/prompt response"
    );
    if reason_str == "cancelled" && suspension.is_some() {
        tracked_terminal_tool_calls.clear();
        cancel_pending_permissions(state, emitter, perms).await;
        terminal_runtime
            .release_all_for_session(sid.0.as_ref())
            .await;
    }
    let disposition = finalize_turn_terminal(
        TurnTerminalSource::Upstream(reason_str),
        suspension,
        state,
        emitter,
        conn_id,
        sid.0.as_ref(),
        agent_type,
        mark_awaiting_reply,
        broker,
    )
    .await;
    let status_restored_by_suspension = matches!(
        disposition,
        TurnFinalizationDisposition::DelegationSuspended
    );
    if !status_restored_by_suspension {
        tracing::info!(
            connection_id = %conn_id,
            session_id = %sid.0,
            stop_reason = %reason_str,
            source = "prompt_response",
            "[ACP] TurnComplete emitted (state+bus+desktop path returned)"
        );
    }
    Ok(BoundPromptFinalization {
        status_restored_by_suspension,
        disconnect_requested: reason_str == "cancelled"
            && matches!(disposition, TurnFinalizationDisposition::SuspensionFailed),
    })
}

#[allow(clippy::too_many_arguments)]
async fn finalize_active_user_cancel(
    cx: &ConnectionTo<Agent>,
    sid: &SessionId,
    suspension: &mut Option<SuspensionLease>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    conn_id: &str,
    agent_type: AgentType,
    mark_awaiting_reply: bool,
    tracked_terminal_tool_calls: &mut HashMap<String, TrackedTerminalToolCall>,
    perms: &PendingPermissions,
    terminal_runtime: &Arc<TerminalRuntime>,
    delegation_injection: Option<&DelegationInjection>,
) {
    let _ = cx.send_notification_to(Agent, CancelNotification::new(sid.clone()));
    let _ = finalize_turn_terminal(
        TurnTerminalSource::UserCancel,
        suspension,
        state,
        emitter,
        conn_id,
        sid.0.as_ref(),
        agent_type,
        mark_awaiting_reply,
        None,
    )
    .await;
    tracked_terminal_tool_calls.clear();
    cancel_pending_permissions(state, emitter, perms).await;
    terminal_runtime
        .release_all_for_session(sid.0.as_ref())
        .await;
    if let Some(injection) = delegation_injection {
        injection
            .broker
            .cancel_by_parent_turn(
                conn_id,
                crate::acp::delegation::types::ParentTurnEndReason::ParentCanceled,
            )
            .await;
        injection
            .questions
            .cancel_questions_by_parent(conn_id)
            .await;
        injection
            .plan_approvals
            .cancel_plan_approvals_by_parent(conn_id)
            .await;
    }
}

/// Generation-guarded tool-watchdog turn cancel (session/cancel).
///
/// Distinct from [`finalize_active_user_cancel`]: automatic timeout must **not**
/// cascade-cancel acknowledged background children via `cancel_by_parent_turn`.
/// UserStop claims that escalate here also avoid the user-cancel tree cascade
/// when the initiating lease was narrow (supervisor owns settlement).
#[allow(clippy::too_many_arguments)]
async fn finalize_active_watchdog_cancel(
    cx: &ConnectionTo<Agent>,
    sid: &SessionId,
    suspension: &mut Option<SuspensionLease>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    conn_id: &str,
    agent_type: AgentType,
    mark_awaiting_reply: bool,
    tracked_terminal_tool_calls: &mut HashMap<String, TrackedTerminalToolCall>,
    perms: &PendingPermissions,
    terminal_runtime: &Arc<TerminalRuntime>,
    cause: crate::acp::tool_watchdog::CancelCause,
) {
    use crate::acp::session_state::ToolCallStatus;
    use crate::acp::tool_watchdog::error_code_for_cause;

    let error_code = error_code_for_cause(cause);
    // Leave failed tool transcript entries before TurnComplete clears
    // active_tool_calls / live_message, so promotion keeps a failed tool_result.
    let failed_tool_ids: Vec<String> = {
        let s = state.read().await;
        s.active_tool_calls
            .iter()
            .filter(|(_, t)| {
                !matches!(t.status, ToolCallStatus::Completed | ToolCallStatus::Failed)
            })
            .map(|(id, _)| id.clone())
            .collect()
    };
    for tool_call_id in failed_tool_ids {
        emit_with_state(
            state,
            emitter,
            AcpEvent::ToolCallUpdate {
                tool_call_id,
                title: None,
                status: Some("failed".into()),
                content: None,
                raw_input: None,
                raw_output: Some(error_code.to_string()),
                raw_output_append: None,
                locations: None,
                meta: None,
                images: None,
            },
        )
        .await;
    }
    let _ = cx.send_notification_to(Agent, CancelNotification::new(sid.clone()));
    // Drop any pending suspension without parent-tree cascade.
    if let Some(mut lease) = suspension.take() {
        reject_suspension_lease(&mut lease, "suspend_cancelled_by_watchdog");
    }
    // Settles Cancelling leases (timed_out / user_cancelled) and emits projections.
    tool_watchdog_complete_turn(state, emitter).await;
    // TurnComplete with cancelled stop_reason; do NOT cancel_by_parent_turn so
    // acknowledged background children survive multi-task wait timeout.
    // Watchdog is never user_stop — clear fence id and leave optional fields absent.
    state.write().await.active_provider_turn_id = None;
    emit_with_state(
        state,
        emitter,
        AcpEvent::TurnComplete {
            session_id: sid.0.to_string(),
            stop_reason: "cancelled".into(),
            agent_type: agent_type.to_string(),
            mark_awaiting_reply,
            termination_source: None,
            provider_turn_id: None,
        },
    )
    .await;
    tracing::info!(
        connection_id = %conn_id,
        session_id = %sid.0,
        error_code,
        ?cause,
        "[ACP] watchdog turn cancel finalized (no parent-tree cascade)"
    );
    tracked_terminal_tool_calls.clear();
    cancel_pending_permissions(state, emitter, perms).await;
    terminal_runtime
        .release_all_for_session(sid.0.as_ref())
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn finalize_active_disconnect(
    cx: &ConnectionTo<Agent>,
    sid: &SessionId,
    suspension: &mut Option<SuspensionLease>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
    terminal_runtime: &Arc<TerminalRuntime>,
    tracked_terminal_tool_calls: &mut HashMap<String, TrackedTerminalToolCall>,
) {
    let _ = cx.send_notification_to(Agent, CancelNotification::new(sid.clone()));
    if let Some(mut lease) = suspension.take() {
        reject_suspension_lease(&mut lease, "suspend_parent_disconnected");
    }
    tracked_terminal_tool_calls.clear();
    cancel_pending_permissions(state, emitter, perms).await;
    terminal_runtime
        .release_all_for_session(sid.0.as_ref())
        .await;
}

/// Classify a `session/load` failure into a stable frontend `code` when the
/// historical session cannot be restored — either the agent has no record of
/// it (`ResourceNotFound`, -32002) or the agent process/session died mid-load.
/// Claude 0.58.1 surfaces the latter as a -32603 Internal error whose message
/// contains "process exited with code N" (its `getOrCreateSession` only maps
/// "Query closed…"/"No conversation found…" to `ResourceNotFound`), so the
/// crash/ended family is matched on the wire message. Both codes route to the
/// same `SessionLoadFailed` banner (Reload / New conversation) instead of a raw
/// protocol error.
///
/// Returns `None` for failures that must keep the existing behavior:
/// "Method not found" (agent lacks resume → silent `session/new` fallback),
/// "Authentication required" (silent stop), and any other error (emit
/// "starting new" then fall through to `session/new`).
fn classify_session_load_failure(
    code: sacp::schema::ErrorCode,
    message: &str,
) -> Option<&'static str> {
    // Before the app-server switch, the bundled Codex adapter generated its
    // own ACP UUIDs and persisted a CLI-runtime mapping. Those IDs are not
    // Codex thread IDs, so the adapter explicitly asks the user to start a
    // new session rather than attempting an invalid resume or silent fallback.
    if message.contains("This Codex session was created by the legacy CLI runtime") {
        return Some("legacy_cli_session");
    }
    if matches!(code, sacp::schema::ErrorCode::ResourceNotFound) {
        return Some("resource_not_found");
    }
    // Upstream signals for an unrecoverable session (claude-agent-acp 0.58.1):
    //  - "process exited"    → "Claude Code process exited with code 1",
    //                          "The Claude Agent process exited unexpectedly…"
    //  - "session has ended" → SESSION_ENDED_MESSAGE
    //  - "Session not found" → a plain Error rethrown as an Internal error
    const UNRECOVERABLE: &[&str] = &["process exited", "session has ended", "Session not found"];
    if UNRECOVERABLE.iter().any(|s| message.contains(s)) {
        return Some("session_unavailable");
    }
    None
}

/// Disposition for a failed `session/load` RPC, shared by production bootstrap
/// and the ResumeExistingOnly contract harness.
///
/// **Order is load-bearing:** under [`SessionAttachMode::ResumeExistingOnly`],
/// *any* load RPC failure must refuse bootstrap (design §2) *before*
/// Default-only classification (`ResourceNotFound` → Reload/New UI). Classifying
/// first would skip durable `unresumable` settle on continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionLoadErrorAction {
    /// Continue / ResumeExistingOnly: call `refuse_unresumable_bootstrap`.
    RefuseUnresumableBootstrap,
    /// Default attach: surface `SessionLoadFailed` with a stable frontend code.
    SurfaceClassifiedLoadFailed { code: &'static str },
    /// Default attach: auth stop / method-not-found / session/new fallthrough.
    ContinueDefaultFallthrough,
}

/// Decide how a `session/load` RPC error must be handled for `attach_mode`.
///
/// Production load-error handling and the resume-contract harness both call
/// this so a harness cannot green while production short-circuits incorrectly.
fn session_load_error_action(
    attach_mode: crate::acp::session_attach::SessionAttachMode,
    code: sacp::schema::ErrorCode,
    message: &str,
) -> SessionLoadErrorAction {
    // ResumeExistingOnly first — includes classified ResourceNotFound (-32002).
    if !attach_mode.allows_session_new() {
        return SessionLoadErrorAction::RefuseUnresumableBootstrap;
    }
    if let Some(code) = classify_session_load_failure(code, message) {
        return SessionLoadErrorAction::SurfaceClassifiedLoadFailed { code };
    }
    SessionLoadErrorAction::ContinueDefaultFallthrough
}

/// Whether codeg can absorb a "the agent forgot this session" load failure by
/// itself, rather than stopping and asking the user to Reload or start over.
///
/// It can exactly when codeg — not the agent — owns the conversation's history:
/// custom ACP agents, whose turns are recorded to
/// [`crate::acp_transcript`]. There the failure costs nothing visible — the
/// history still renders, and a fresh agent session (linked by
/// `continues_from`) continues the same conversation. Agents whose history is
/// read back out of their own store keep the banner: for them the session
/// really is gone, and silently starting a new one would orphan it.
///
/// `classified` is [`classify_session_load_failure`]'s verdict; `None` (an
/// unexpected failure) is never recovered here — it keeps the existing
/// emit-then-fall-back-to-`session/new` behaviour.
fn recovers_load_failure_locally(agent_type: AgentType, classified: Option<&'static str>) -> bool {
    classified.is_some() && transcript_dir_for(agent_type).is_some()
}

/// True when a `SessionUpdate` represents actual agent-produced output for
/// the current turn. Used to detect "silent EndTurn" cases where an agent
/// (notably OpenCode) reports the turn ended successfully but never emitted
/// any reply or tool call — in practice this means the model-side request
/// was swallowed and the user would otherwise see a blank conversation
/// transition silently to `PendingReview`. Metadata-only updates
/// (`UserMessageChunk`, `Plan`, `*ModeUpdate`, `ConfigOptionUpdate`,
/// `SessionInfoUpdate`, `AvailableCommandsUpdate`, `UsageUpdate`) do not
/// count.
fn is_agent_output_update(update: &SessionUpdate) -> bool {
    matches!(
        update,
        SessionUpdate::AgentMessageChunk(_)
            | SessionUpdate::AgentThoughtChunk(_)
            | SessionUpdate::ToolCall(_)
            | SessionUpdate::ToolCallUpdate(_)
    )
}

/// Soft-watchdog activity classifier. True only for Agent transcript /
/// thinking chunks, tool start/update/progress, and plan activity.
/// User/frontend keepalive, commands, usage, status, and session-info noise
/// do **not** count. Distinct from [`is_agent_output_update`] (which excludes
/// `Plan` for silent-EndTurn detection).
pub(crate) fn advances_agent_activity(update: &SessionUpdate) -> bool {
    matches!(
        update,
        SessionUpdate::AgentMessageChunk(_)
            | SessionUpdate::AgentThoughtChunk(_)
            | SessionUpdate::ToolCall(_)
            | SessionUpdate::ToolCallUpdate(_)
            | SessionUpdate::Plan(_)
    )
}

/// Mark session health when `update` is semantic agent activity.
///
/// Extracted so inbound health updates can be unit-tested without an
/// `EventEmitter`, WebSocket attach, or frontend subscriber. Callers must
/// invoke this **before** `emit_conversation_update` so filters / missing
/// viewers cannot suppress the clock advance.
async fn mark_agent_activity_for_update(
    state: &Arc<tokio::sync::RwLock<SessionState>>,
    update: &SessionUpdate,
    at: chrono::DateTime<chrono::Utc>,
) -> bool {
    if !advances_agent_activity(update) {
        return false;
    }
    state.write().await.mark_agent_activity(at);
    true
}

/// Build an `AcpEvent::Error` for a non-success stop reason so the user gets a
/// toast instead of a silent transition to `PendingReview`. Returns `None` for
/// `end_turn` (success) and `cancelled` (already user-driven).
///
/// `Refusal` is included because OpenCode (and similar agents) map backend /
/// gateway errors to `Refusal` per the ACP spec gap.
/// `empty` is a synthesized reason emitted by `run_conversation_loop` when
/// the agent reports `EndTurn` without producing any agent output.
fn turn_failure_error_event(reason_str: &str, agent_type: AgentType) -> Option<AcpEvent> {
    let (code, message) = match reason_str {
        "refusal" => (
            "turn_failed_refusal",
            format!("{agent_type} refused to continue this turn."),
        ),
        "max_tokens" => (
            "turn_failed_max_tokens",
            format!("{agent_type} reached the maximum token limit for this turn."),
        ),
        "max_turn_requests" => (
            "turn_failed_max_turn_requests",
            format!("{agent_type} reached the maximum number of allowed requests for this turn."),
        ),
        "unknown" => (
            "turn_failed_unknown",
            format!("{agent_type} ended the turn with an unrecognized stop reason."),
        ),
        "empty" => (
            "turn_failed_empty",
            format!(
                "{agent_type} ended the turn without producing any response. \
                 Please check the agent's configuration."
            ),
        ),
        _ => return None,
    };
    Some(AcpEvent::Error {
        message,
        agent_type: agent_type.to_string(),
        code: Some(code.to_string()),
        // Non-terminal: this Error is paired with a `TurnComplete`
        // carrying the same stop reason. The connection stays alive and
        // the broker's pending entry is drained by `complete_call` with
        // the correct child-side mapping (`ChildRefusal` /
        // `ChildMaxTokens` / …). See F1 in the v0.14.3 sub-agent
        // delegation post-mortem.
        terminal: false,
    })
}

/// Returns `Ok(None)` on normal exit (disconnect / channel closed) or
/// `Ok(Some(ForkExitInfo))` when the loop should be restarted on a forked session.
#[allow(clippy::too_many_arguments)]
async fn run_conversation_loop<'a>(
    session: &mut sacp::ActiveSession<'a, Agent>,
    conn_id: &str,
    emitter: &EventEmitter,
    state: &Arc<RwLock<SessionState>>,
    agent_type: AgentType,
    perms: &PendingPermissions,
    cmd_rx: &mut mpsc::Receiver<ConnectionCommand>,
    control_rx: &mut mpsc::Receiver<ConnectionControl>,
    cmd_liveness_rx: &mut watch::Receiver<bool>,
    control_liveness_rx: &mut watch::Receiver<bool>,
    terminal_runtime: Arc<TerminalRuntime>,
    terminal_assoc: Arc<std::sync::Mutex<TerminalAssocFallback>>,
    file_system_runtime: Arc<FileSystemRuntime>,
    cwd: &str,
    supports_fork: bool,
    // Immutable connection shell snapshot for `session/fork` metadata.
    shell_spec: &ResolvedShellSpec,
    // Immutable launch route plan — fork reuses the same Claude deny merge as
    // new/load/resume; never re-resolves policy.
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
    // Connection-scoped (created once in `run_connection`, shared across fork
    // restarts of this loop): outgoing prompts are fingerprinted here so the
    // transcript watcher can classify their turns as wire-rendered foreground.
    prompt_ledger: &background_watch::PromptLedger,
    // Process-lifetime one-shot: first wire prompt gets terminal context;
    // UI `UserMessage` events still use the original user blocks only.
    terminal_prompt_context: &TerminalPromptContext,
    // Source of the broker reference used to cascade-cancel pending
    // delegations on parent prompt cancel / non-success TurnComplete.
    // `None` for test paths that don't wire delegation.
    delegation_injection: Option<&DelegationInjection>,
) -> Result<Option<ForkExitInfo>, sacp::Error> {
    // Session-scoped cache for diffing cumulative `raw_output` snapshots
    // into incremental deltas. Shared across the idle loop and the active
    // turn loop so tool calls that span turns stay consistent.
    let mut raw_output_cache = ToolCallOutputCache::default();
    // Session-scoped CodeBuddy live state: authoritative title rewrites
    // (tool_call_id → "agent" / inner `mcp__…` name) so a later status-only
    // update can't downgrade an Agent / delegation card mid-stream, plus the
    // open-sub-agent window used to suppress a sub-agent's interleaved
    // thought/message chunks. See `emit_conversation_update`. Shared across the
    // idle and turn loops.
    let mut cb_state = CodeBuddyLiveState::default();
    let mut ancillary_command: Option<AncillaryCommandFuture> = None;
    let mut normal_lane_closed = false;
    let mut control_lane_closed = false;
    let mut normal_liveness_observed = false;
    let mut control_liveness_observed = false;
    // 1-based per-connection turn counter for the timing journal's ordinal
    // (see `turn_timings::TurnTiming::ord`) — incremented for EVERY Cursor
    // prompt turn, journaled or not, so consecutive ordinals prove adjacent
    // turns to the reader.
    let mut cursor_turn_ord: u64 = 0;
    loop {
        // Wait for either a user command or a session update (e.g. available_commands_update)
        let input = loop {
            tokio::select! {
                biased;
                control = control_rx.recv(), if !control_lane_closed => {
                    match control {
                        Some(control) => break ConversationInput::Control(control),
                        None => {
                            control_lane_closed = true;
                            if normal_lane_closed || cmd_rx.is_closed() {
                                break ConversationInput::ChannelsClosed;
                            }
                        }
                    }
                }
                _ = control_liveness_rx.changed(), if !control_liveness_observed => {
                    control_liveness_observed = true;
                    if both_connection_lanes_closed(
                        normal_lane_closed,
                        control_lane_closed,
                        cmd_liveness_rx,
                        control_liveness_rx,
                    ) {
                        break ConversationInput::ChannelsClosed;
                    }
                }
                _ = cmd_liveness_rx.changed(), if !normal_liveness_observed => {
                    normal_liveness_observed = true;
                    if both_connection_lanes_closed(
                        normal_lane_closed,
                        control_lane_closed,
                        cmd_liveness_rx,
                        control_liveness_rx,
                    ) {
                        break ConversationInput::ChannelsClosed;
                    }
                }
                _ = async {
                    ancillary_command
                        .as_mut()
                        .expect("guarded ancillary command")
                        .await
                }, if ancillary_command.is_some() => {
                    ancillary_command = None;
                }
                cmd = cmd_rx.recv(), if ancillary_command.is_none() && !normal_lane_closed => {
                    match cmd {
                        Some(command) => {
                            let cx = session.connection();
                            let sid = session.session_id().clone();
                            match start_ancillary_command(
                                command,
                                cx,
                                sid,
                                Arc::clone(state),
                                emitter.clone(),
                                file_system_runtime.clone(),
                                agent_type,
                            ) {
                                Ok(future) => ancillary_command = Some(future),
                                Err(command) => break ConversationInput::Command(command),
                            }
                        }
                        None => {
                            normal_lane_closed = true;
                            if control_lane_closed || control_rx.is_closed() {
                                break ConversationInput::ChannelsClosed;
                            }
                        }
                    }
                }
                update = session.read_update() => {
                    match update {
                        Ok(SessionMessage::SessionMessage(dispatch)) => {
                            let h = emitter.clone();
                            let st = Arc::clone(state);
                            let cwd_opt = Some(cwd);
                            let dispatch = fix_usage_update_nulls(dispatch);
                            let _ = MatchDispatch::new(dispatch)
                                .if_notification(
                                    async |notif: SessionNotification| {
                                        // Soft-watchdog: mark agent activity at
                                        // the inbound boundary, before conversion.
                                        mark_agent_activity_for_update(
                                            &st,
                                            &notif.update,
                                            chrono::Utc::now(),
                                        )
                                        .await;
                                        maybe_emit_grok_total_tokens_usage(
                                            &st,
                                            &h,
                                            agent_type,
                                            notif.meta.as_ref(),
                                        )
                                        .await;
                                        emit_conversation_update(&st, &h, agent_type, notif.update, cwd_opt, &mut raw_output_cache, &mut cb_state, None).await;
                                        Ok(())
                                    },
                                )
                                .await
                                .otherwise(async |dispatch| {
                                    let mut compact_flag = false;
                                    let _ = maybe_emit_live_ext_notification(
                                        &st,
                                        &h,
                                        agent_type,
                                        dispatch,
                                        crate::acp::xai_session_notification::PrivateExtEmitMode::IdleUsageOnly,
                                        &mut compact_flag,
                                    )
                                    .await;
                                    Ok(())
                                })
                                .await;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            handle_idle_session_update_error(
                                delegation_injection.map(|injection| {
                                    injection.parent_connection_exit_causes.as_ref()
                                }),
                                conn_id,
                                &e,
                            );
                        }
                    }
                }
            }
        };
        match input {
            ConversationInput::Command(ConnectionCommand::Prompt {
                blocks,
                user_message,
                mark_awaiting_reply,
                turn_generation,
            }) => {
                // Fingerprint the outgoing prompt for the background watcher's
                // foreground/out-of-turn classifier BEFORE the blocks are
                // consumed: the transcript record this prompt becomes must
                // classify as wire-rendered foreground, not overlay.
                // Ledger + UserMessage use original user content only; the
                // terminal context block is appended to the wire payload below.
                prompt_ledger.record_prompt_blocks(&blocks);
                // Cursor's ACP store carries no per-turn timestamps at all
                // (see `crate::turn_timings`), so codeg journals its own
                // observation of the turn span: hash + ordinal here (before
                // the blocks are consumed), the send stamp after the
                // `UserMessage` broadcast below, the append at TurnComplete.
                // The hash of the outgoing text blocks is what the history
                // parser correlates its user turns against; the ordinal is
                // its contiguity anchor (every turn consumes one, journaled
                // or not).
                let turn_timing_prep = matches!(agent_type, AgentType::Cursor).then(|| {
                    cursor_turn_ord += 1;
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| match b {
                            PromptInputBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    (crate::turn_timings::prompt_hash(&text), cursor_turn_ord)
                });
                let mut prompt_blocks = map_prompt_blocks(blocks);
                if prompt_blocks.is_empty() {
                    // Defensive: the manager rejects empty prompts before the
                    // concurrency gate is set / the command is enqueued (see
                    // `send_prompt_inner`), and `map_prompt_blocks` is 1:1, so an
                    // empty prompt should never reach here. If one ever did, it
                    // would carry no turn-in-flight gate, so just surface the
                    // error and keep the idle loop alive.
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::Error {
                            message: "Prompt must contain at least one content block".into(),
                            agent_type: agent_type.to_string(),
                            code: None,
                            // Recoverable: idle loop continues, awaiting the
                            // next user command. Connection stays alive.
                            terminal: false,
                        },
                    )
                    .await;
                    continue;
                }

                // Wire-only mutation: first prompt on this process gets the
                // versioned shell context. Live UI / optimistic dedup / hidden
                // generation paths never see this block (they use
                // `user_message` below). Title/translate must send the exact
                // utility prompt without a silent terminal-instruction prefix.
                let skip_terminal_prefix = {
                    let s = state.read().await;
                    s.purpose.is_hidden_generation()
                };
                if !skip_terminal_prefix {
                    terminal_prompt_context.append_once(&mut prompt_blocks);
                }

                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Prompting,
                    },
                )
                .await;
                // Prompt admission starts the untracked fallback clock for this
                // generation (active_turn_generation already set by the manager).
                tool_watchdog_start_turn(state).await;

                // Broadcast the user's prompt to cross-client viewers BEFORE
                // issuing the agent request. Emitting here (rather than at the
                // manager enqueue site) guarantees its seq strictly precedes the
                // turn's assistant/status events — viewers apply events in seq
                // order, so otherwise the reply could render above the message.
                // It also means a prompt that is never processed (rejected /
                // dropped) broadcasts nothing. `apply_event` records it as
                // `pending_user_message` so a client attaching mid-turn still
                // renders the user turn from the snapshot.
                // IMPORTANT: uses original projected user blocks — never the
                // wire-only `<codeg_terminal_context>` append above.
                if let Some((message_id, blocks)) = user_message {
                    emit_with_state(state, emitter, AcpEvent::UserMessage { message_id, blocks })
                        .await;
                }

                // Stamp the journal's turn start AFTER the `UserMessage`
                // broadcast: `apply_in_flight_message_id`'s recency gate
                // compares parsed user-turn timestamps — which the journal
                // upgrade rewrites to this stamp — against the broadcast's
                // application instant (`pending_user_message_started_at`,
                // stored at millisecond precision for exactly this
                // comparison). `emit_with_state` applies the event before
                // returning, so this stamp is never earlier than the gate's
                // threshold and the in-flight user turn stays stampable in
                // the journal-written-but-turn-still-pending window.
                let mut turn_timing_probe = turn_timing_prep.map(|(prompt_sha, ord)| {
                    (crate::turn_timings::now_epoch_ms(), prompt_sha, ord)
                });

                // Clone connection and session ID before entering the
                // select loop so we can send CancelNotification without
                // conflicting with session.read_update()'s mutable borrow.
                let cx = session.connection();
                let sid = session.session_id().clone();
                // Record the prompt BEFORE sending, so the transcript's line
                // order matches the wire order even if the agent replies
                // instantly — and awaited, so the replay gate can never see
                // this conversation as transcript-less (see `record_prompt`).
                record_prompt(agent_type, &sid.0, &prompt_blocks).await;
                let turn_started_at_ms = crate::acp_transcript::now_epoch_ms();
                let prompt_request = PromptRequest::new(sid.clone(), prompt_blocks);
                // Use Box::pin (heap) instead of tokio::pin! (stack) so the
                // future can be moved into a background task on cancel.
                let mut prompt_response = Box::pin(
                    cx.clone()
                        .send_request_to(Agent, prompt_request)
                        .block_task(),
                );
                tracing::info!(
                    connection_id = %conn_id,
                    session_id = %sid.0,
                    agent = %agent_type,
                    mark_awaiting_reply,
                    "[ACP] session/prompt sent; awaiting turn completion \
                     (prompt_response | StopReason | extension turn_completed)"
                );
                let mut tracked_terminal_tool_calls: HashMap<String, TrackedTerminalToolCall> =
                    HashMap::new();
                let mut terminal_poll_interval = tokio::time::interval(
                    std::time::Duration::from_millis(TERMINAL_POLL_INTERVAL_MS),
                );
                terminal_poll_interval
                    .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut disconnect_requested = false;
                let mut suspension: Option<SuspensionLease> = None;
                let mut suspension_terminal_diagnostic: Option<String> = None;
                let mut suspension_deadline = Box::pin(tokio::time::sleep(
                    std::time::Duration::from_secs(365 * 24 * 60 * 60),
                ));
                let mut status_restored_by_suspension = false;
                let suspension_admission = {
                    let snapshot = state.read().await;
                    SuspensionAdmissionSnapshot {
                        turn_in_flight: snapshot.turn_in_flight,
                        active_turn_generation: snapshot.active_turn_generation,
                    }
                };
                // Tracks whether the agent produced any real output during
                // this turn (text reply, thinking chunk, or tool call). When
                // an agent reports `EndTurn` with this still false, we treat
                // it as a silent failure and synthesize an `"empty"` stop
                // reason so the user gets an error toast instead of a
                // confusing `PendingReview` on a blank conversation.
                let mut turn_had_agent_output = false;
                let mut grok_retry_reconciler = GrokRetryReconciler::default();
                // Tracks whether a Grok compact lifecycle ContentDelta was already
                // emitted this turn so subsequent lifecycle strings get a leading `\n`.
                let mut compact_text_emitted_this_turn = false;
                // A CodeBuddy native sub-agent's full lifecycle (Agent tool call
                // open → completed) happens within one turn, so reset the
                // suppression window at each turn start. This bounds the tracking
                // sets and guarantees a sub-agent that ended without a terminal
                // frame (cancel/abort) can never suppress the NEXT turn's
                // main-agent thinking. `title_overrides` intentionally persists
                // (a card's identity is session-stable).
                cb_state.open_subagents.clear();
                cb_state.closed_subagents.clear();

                // Read updates until turn completes.
                // We must also listen for commands (e.g. RespondPermission)
                // to avoid deadlocking when the agent awaits a permission response.
                loop {
                    tokio::select! {
                        biased;
                        prompt_result = &mut prompt_response => {
                            match drain_ready_active_controls(
                                control_rx,
                                suspension_admission,
                                turn_generation,
                                &mut suspension,
                                &mut suspension_deadline,
                                conn_id,
                                &sid,
                                &cx,
                                &terminal_runtime,
                            ) {
                                Some(ActiveTerminalControl::UserCancel) => {
                                    // Harvest ready activeTurnId before snapshot.
                                    drain_ready_in_prompt_updates(
                                        &mut ReadyUpdateSource::Live(session),
                                        state,
                                        emitter,
                                        agent_type,
                                        &sid,
                                        cwd,
                                        &terminal_runtime,
                                        &terminal_assoc,
                                        &mut tracked_terminal_tool_calls,
                                        &mut raw_output_cache,
                                        &mut cb_state,
                                        &mut grok_retry_reconciler,
                                        &mut turn_had_agent_output,
                                        &mut compact_text_emitted_this_turn,
                                    )
                                    .await;
                                    finalize_active_user_cancel(
                                        &cx,
                                        &sid,
                                        &mut suspension,
                                        state,
                                        emitter,
                                        conn_id,
                                        agent_type,
                                        mark_awaiting_reply,
                                        &mut tracked_terminal_tool_calls,
                                        perms,
                                        &terminal_runtime,
                                        delegation_injection,
                                    )
                                    .await;
                                    break;
                                }
                                Some(ActiveTerminalControl::WatchdogCancel { cause }) => {
                                    finalize_active_watchdog_cancel(
                                        &cx,
                                        &sid,
                                        &mut suspension,
                                        state,
                                        emitter,
                                        conn_id,
                                        agent_type,
                                        mark_awaiting_reply,
                                        &mut tracked_terminal_tool_calls,
                                        perms,
                                        &terminal_runtime,
                                        cause,
                                    )
                                    .await;
                                    break;
                                }
                                Some(ActiveTerminalControl::Disconnect) => {
                                    finalize_active_disconnect(
                                        &cx,
                                        &sid,
                                        &mut suspension,
                                        state,
                                        emitter,
                                        perms,
                                        &terminal_runtime,
                                        &mut tracked_terminal_tool_calls,
                                    )
                                    .await;
                                    disconnect_requested = true;
                                    break;
                                }
                                None => {}
                            }
                            if both_connection_lanes_closed(
                                normal_lane_closed,
                                control_lane_closed,
                                cmd_liveness_rx,
                                control_liveness_rx,
                            ) {
                                record_session_channel_loss(delegation_injection, conn_id);
                                if let Some(mut lease) = suspension.take() {
                                    reject_suspension_lease(
                                        &mut lease,
                                        "suspend_parent_disconnected",
                                    );
                                }
                                disconnect_requested = true;
                                break;
                            }
                            // Drain ready session updates so private compact cannot
                            // lose a race to biased prompt_response completion.
                            drain_ready_in_prompt_updates(
                                &mut ReadyUpdateSource::Live(session),
                                state,
                                emitter,
                                agent_type,
                                &sid,
                                cwd,
                                &terminal_runtime,
                                &terminal_assoc,
                                &mut tracked_terminal_tool_calls,
                                &mut raw_output_cache,
                                &mut cb_state,
                                &mut grok_retry_reconciler,
                                &mut turn_had_agent_output,
                                &mut compact_text_emitted_this_turn,
                            )
                            .await;
                            let outcome = finalize_bound_prompt_response(
                                prompt_result,
                                &mut suspension,
                                state,
                                emitter,
                                conn_id,
                                &sid,
                                agent_type,
                                mark_awaiting_reply,
                                &terminal_assoc,
                                &mut tracked_terminal_tool_calls,
                                &terminal_runtime,
                                perms,
                                turn_had_agent_output,
                                turn_started_at_ms,
                                &mut turn_timing_probe,
                                delegation_injection.map(|injection| injection.broker.as_ref()),
                            )
                            .await?;
                            status_restored_by_suspension =
                                outcome.status_restored_by_suspension;
                            disconnect_requested = outcome.disconnect_requested;
                            break;
                        }
                        _ = &mut suspension_deadline, if suspension.is_some() => {
                            match drain_ready_active_controls(
                                control_rx,
                                suspension_admission,
                                turn_generation,
                                &mut suspension,
                                &mut suspension_deadline,
                                conn_id,
                                &sid,
                                &cx,
                                &terminal_runtime,
                            ) {
                                Some(ActiveTerminalControl::UserCancel) => {
                                    // Harvest ready activeTurnId before snapshot.
                                    drain_ready_in_prompt_updates(
                                        &mut ReadyUpdateSource::Live(session),
                                        state,
                                        emitter,
                                        agent_type,
                                        &sid,
                                        cwd,
                                        &terminal_runtime,
                                        &terminal_assoc,
                                        &mut tracked_terminal_tool_calls,
                                        &mut raw_output_cache,
                                        &mut cb_state,
                                        &mut grok_retry_reconciler,
                                        &mut turn_had_agent_output,
                                        &mut compact_text_emitted_this_turn,
                                    )
                                    .await;
                                    finalize_active_user_cancel(
                                        &cx,
                                        &sid,
                                        &mut suspension,
                                        state,
                                        emitter,
                                        conn_id,
                                        agent_type,
                                        mark_awaiting_reply,
                                        &mut tracked_terminal_tool_calls,
                                        perms,
                                        &terminal_runtime,
                                    delegation_injection,
                                    )
                                    .await;
                                    tokio::spawn(async move {
                                        let _ = prompt_response.await;
                                    });
                                    break;
                                }
                                Some(ActiveTerminalControl::WatchdogCancel { cause }) => {
                                    finalize_active_watchdog_cancel(
                                        &cx,
                                        &sid,
                                        &mut suspension,
                                        state,
                                        emitter,
                                        conn_id,
                                        agent_type,
                                        mark_awaiting_reply,
                                        &mut tracked_terminal_tool_calls,
                                        perms,
                                        &terminal_runtime,
                                        cause,
                                    )
                                    .await;
                                    tokio::spawn(async move {
                                        let _ = prompt_response.await;
                                    });
                                    break;
                                }
                                Some(ActiveTerminalControl::Disconnect) => {
                                    finalize_active_disconnect(
                                        &cx,
                                        &sid,
                                        &mut suspension,
                                        state,
                                        emitter,
                                        perms,
                                        &terminal_runtime,
                                        &mut tracked_terminal_tool_calls,
                                    )
                                    .await;
                                    disconnect_requested = true;
                                    break;
                                }
                                None => {}
                            }
                            if both_connection_lanes_closed(
                                normal_lane_closed,
                                control_lane_closed,
                                cmd_liveness_rx,
                                control_liveness_rx,
                            ) {
                                record_session_channel_loss(delegation_injection, conn_id);
                                if let Some(mut lease) = suspension.take() {
                                    reject_suspension_lease(
                                        &mut lease,
                                        "suspend_parent_disconnected",
                                    );
                                }
                                disconnect_requested = true;
                                break;
                            }
                            if let Some(injection) = delegation_injection {
                                injection
                                    .parent_connection_exit_causes
                                    .record_suspension_drain_timeout(conn_id);
                            }
                            let _ = finalize_turn_terminal(
                                TurnTerminalSource::SuspensionDrainTimeout,
                                &mut suspension,
                                state,
                                emitter,
                                conn_id,
                                sid.0.as_ref(),
                                agent_type,
                                mark_awaiting_reply,
                                delegation_injection.map(|injection| injection.broker.as_ref()),
                            )
                            .await;
                            disconnect_requested = true;
                            break;
                        }
                        control = control_rx.recv(), if !control_lane_closed => {
                            match control {
                                Some(ConnectionControl::SuspendForDelegation {
                                    continuation_id,
                                    parent_turn_generation,
                                    reply,
                                }) => {
                                    apply_suspension_control(
                                        continuation_id,
                                        parent_turn_generation,
                                        reply,
                                        suspension_admission,
                                        turn_generation,
                                        &mut suspension,
                                        &mut suspension_deadline,
                                        conn_id,
                                        &sid,
                                        &cx,
                                    );
                                }
                                Some(ConnectionControl::CancelTerminal {
                                    session_id,
                                    terminal_id,
                                    reply,
                                }) => {
                                    // Non-turn-ending: admit/ack + detached kill only.
                                    admit_cancel_terminal_control(
                                        &terminal_runtime,
                                        session_id,
                                        terminal_id,
                                        reply,
                                    );
                                }
                                Some(ConnectionControl::Cancel) => {
                                    // Harvest ready activeTurnId before snapshot.
                                    drain_ready_in_prompt_updates(
                                        &mut ReadyUpdateSource::Live(session),
                                        state,
                                        emitter,
                                        agent_type,
                                        &sid,
                                        cwd,
                                        &terminal_runtime,
                                        &terminal_assoc,
                                        &mut tracked_terminal_tool_calls,
                                        &mut raw_output_cache,
                                        &mut cb_state,
                                        &mut grok_retry_reconciler,
                                        &mut turn_had_agent_output,
                                        &mut compact_text_emitted_this_turn,
                                    )
                                    .await;
                                    finalize_active_user_cancel(
                                        &cx,
                                        &sid,
                                        &mut suspension,
                                        state,
                                        emitter,
                                        conn_id,
                                        agent_type,
                                        mark_awaiting_reply,
                                        &mut tracked_terminal_tool_calls,
                                        perms,
                                        &terminal_runtime,
                                        delegation_injection,
                                    )
                                    .await;
                                    tokio::spawn(async move {
                                        let _ = prompt_response.await;
                                    });
                                    break;
                                }
                                Some(ConnectionControl::CancelTurn {
                                    turn_generation: expected_gen,
                                    cause,
                                }) => {
                                    if suspension_admission.active_turn_generation
                                        == Some(expected_gen)
                                    {
                                        finalize_active_watchdog_cancel(
                                            &cx,
                                            &sid,
                                            &mut suspension,
                                            state,
                                            emitter,
                                            conn_id,
                                            agent_type,
                                            mark_awaiting_reply,
                                            &mut tracked_terminal_tool_calls,
                                            perms,
                                            &terminal_runtime,
                                            cause,
                                        )
                                        .await;
                                        tokio::spawn(async move {
                                            let _ = prompt_response.await;
                                        });
                                        break;
                                    }
                                    // Stale generation: ignore and keep prompting.
                                }
                                Some(ConnectionControl::Disconnect) => {
                                    tracing::info!(
                                        "[ACP] disconnect requested during prompting; connection_id={conn_id}"
                                    );
                                    finalize_active_disconnect(
                                        &cx,
                                        &sid,
                                        &mut suspension,
                                        state,
                                        emitter,
                                        perms,
                                        &terminal_runtime,
                                        &mut tracked_terminal_tool_calls,
                                    )
                                    .await;
                                    disconnect_requested = true;
                                    break;
                                }
                                None => {
                                    control_lane_closed = true;
                                    if normal_lane_closed || cmd_rx.is_closed() {
                                        record_session_channel_loss(
                                            delegation_injection,
                                            conn_id,
                                        );
                                        if let Some(mut lease) = suspension.take() {
                                            reject_suspension_lease(
                                                &mut lease,
                                                "suspend_parent_disconnected",
                                            );
                                        }
                                        disconnect_requested = true;
                                        break;
                                    }
                                }
                            }
                        }
                        _ = control_liveness_rx.changed(), if !control_liveness_observed => {
                            control_liveness_observed = true;
                            if both_connection_lanes_closed(
                                normal_lane_closed,
                                control_lane_closed,
                                cmd_liveness_rx,
                                control_liveness_rx,
                            ) {
                                record_session_channel_loss(delegation_injection, conn_id);
                                if let Some(mut lease) = suspension.take() {
                                    reject_suspension_lease(
                                        &mut lease,
                                        "suspend_parent_disconnected",
                                    );
                                }
                                disconnect_requested = true;
                                break;
                            }
                        }
                        _ = cmd_liveness_rx.changed(), if !normal_liveness_observed => {
                            normal_liveness_observed = true;
                            if both_connection_lanes_closed(
                                normal_lane_closed,
                                control_lane_closed,
                                cmd_liveness_rx,
                                control_liveness_rx,
                            ) {
                                record_session_channel_loss(delegation_injection, conn_id);
                                if let Some(mut lease) = suspension.take() {
                                    reject_suspension_lease(
                                        &mut lease,
                                        "suspend_parent_disconnected",
                                    );
                                }
                                disconnect_requested = true;
                                break;
                            }
                        }
                        _ = async {
                            ancillary_command
                                .as_mut()
                                .expect("guarded ancillary command")
                                .await
                        }, if ancillary_command.is_some() => {
                            ancillary_command = None;
                        }
                        command = cmd_rx.recv(), if ancillary_command.is_none() && !normal_lane_closed => {
                            match command {
                                Some(command) => {
                                    match start_ancillary_command(
                                        command,
                                        cx.clone(),
                                        sid.clone(),
                                        Arc::clone(state),
                                        emitter.clone(),
                                        file_system_runtime.clone(),
                                        agent_type,
                                    ) {
                                        Ok(future) => ancillary_command = Some(future),
                                        Err(ConnectionCommand::RespondPermission {
                                            request_id,
                                            option_id,
                                        }) => {
                                            if let Some(responder) =
                                                perms.lock().await.remove(&request_id)
                                            {
                                                responder.respond_selected(option_id);
                                                emit_with_state(
                                                    state,
                                                    emitter,
                                                    AcpEvent::PermissionResolved { request_id },
                                                )
                                                .await;
                                                tool_watchdog_resume(state).await;
                                            }
                                        }
                                        Err(ConnectionCommand::Fork { reply }) => {
                                            let _ = reply.send(Err(AcpError::TurnInProgress));
                                        }
                                        Err(ConnectionCommand::GoalControl { action }) => {
                                            if let Err(error) =
                                                send_goal_control(&cx, &sid, action).await
                                            {
                                                emit_with_state(
                                                    state,
                                                    emitter,
                                                    AcpEvent::Error {
                                                        message: format!(
                                                            "Failed to control goal: {error}"
                                                        ),
                                                        agent_type: agent_type.to_string(),
                                                        code: None,
                                                        terminal: false,
                                                    },
                                                )
                                                .await;
                                            }
                                        }
                                        Err(ConnectionCommand::Prompt { .. }) => {
                                            emit_with_state(
                                                state,
                                                emitter,
                                                AcpEvent::Error {
                                                    message: "Prompt received while a turn is already active".into(),
                                                    agent_type: agent_type.to_string(),
                                                    code: Some("turn_in_progress".into()),
                                                    terminal: false,
                                                },
                                            )
                                            .await;
                                        }
                                        Err(ConnectionCommand::SetMode { .. })
                                        | Err(ConnectionCommand::SetConfigOption { .. }) => {
                                            unreachable!("ancillary commands always start")
                                        }
                                    }
                                }
                                None => {
                                    normal_lane_closed = true;
                                    if control_lane_closed || control_rx.is_closed() {
                                        record_session_channel_loss(
                                            delegation_injection,
                                            conn_id,
                                        );
                                        if let Some(mut lease) = suspension.take() {
                                            reject_suspension_lease(
                                                &mut lease,
                                                "suspend_parent_disconnected",
                                            );
                                        }
                                        disconnect_requested = true;
                                        break;
                                    }
                                }
                            }
                        }
                        update = session.read_update() => {
                            let update = match update {
                                Ok(u) => u,
                                Err(e) => {
                                    tracing::warn!("[ACP] Ignoring unrecognized session update: {e}");
                                    continue;
                                }
                            };
                            match update {
                                SessionMessage::SessionMessage(dispatch) => {
                                    let h = emitter.clone();
                                    let st = Arc::clone(state);
                                    let runtime = terminal_runtime.clone();
                                    let session_id = sid.clone();
                                    let cwd_opt = Some(cwd);
                                    let dispatch = fix_usage_update_nulls(dispatch);
                                    // Grok reports `/compact` results on ext methods
                                    // that bypass the typed pipeline below. Count a
                                    // visible compaction result before dispatch so a
                                    // compaction-only turn is never rewritten to empty.
                                    if grok_ext_notification_is_turn_output(
                                        &dispatch,
                                        agent_type,
                                    ) {
                                        turn_had_agent_output = true;
                                    }
                                    if let Dispatch::Notification(notification) = &dispatch {
                                        if reconcile_grok_retry_dispatch(
                                            agent_type,
                                            notification,
                                            &mut grok_retry_reconciler,
                                            state,
                                            emitter,
                                            &mut turn_had_agent_output,
                                        )
                                        .await
                                        {
                                            continue;
                                        }
                                    }
                                    // Grok `_x.ai/session/update` + turn_completed
                                    // is not a typed SessionNotification — handle
                                    // it before MatchDispatch so a stalled
                                    // session/prompt RPC cannot leave the row
                                    // stuck in `in_progress`.
                                    if let Some(ext_reason) =
                                        parse_extension_turn_completed(&dispatch)
                                    {
                                        drain_ready_in_prompt_updates(
                                            &mut ReadyUpdateSource::Live(session),
                                            state,
                                            emitter,
                                            agent_type,
                                            &sid,
                                            cwd,
                                            &terminal_runtime,
                                            &terminal_assoc,
                                            &mut tracked_terminal_tool_calls,
                                            &mut raw_output_cache,
                                            &mut cb_state,
                                            &mut grok_retry_reconciler,
                                            &mut turn_had_agent_output,
                                            &mut compact_text_emitted_this_turn,
                                        )
                                        .await;
                                        let _ = merge_terminal_assoc_binds(
                                            sid.0.as_ref(),
                                            terminal_assoc.as_ref(),
                                            &mut tracked_terminal_tool_calls,
                                        );
                                        if !tracked_terminal_tool_calls.is_empty() {
                                            poll_tracked_terminal_tool_calls(
                                                terminal_runtime.as_ref(),
                                                &sid,
                                                state,
                                                emitter,
                                                &mut tracked_terminal_tool_calls,
                                            )
                                            .await;
                                        }
                                        let reason_str = rewrite_end_turn_if_empty(
                                            &ext_reason,
                                            turn_had_agent_output,
                                        )
                                        .to_string();
                                        if record_suspension_terminal_diagnostic(
                                            suspension.as_ref(),
                                            &mut suspension_terminal_diagnostic,
                                            &reason_str,
                                        ) {
                                            tracing::info!(
                                                connection_id = %conn_id,
                                                session_id = %sid.0,
                                                stop_reason = %reason_str,
                                                "[ACP] recorded extension terminal while draining suspended turn"
                                            );
                                            continue;
                                        }
                                        tracing::info!(
                                            connection_id = %conn_id,
                                            session_id = %sid.0,
                                            agent = %agent_type,
                                            stop_reason = %reason_str,
                                            turn_had_agent_output,
                                            source = "extension_turn_completed",
                                            "[ACP] completing turn from extension \
                                             turn_completed (prompt_response may still \
                                             be pending; draining in background)"
                                        );
                                        if reason_str == "end_turn" {
                                            journal_turn_span(
                                                &mut turn_timing_probe,
                                                conn_id,
                                                &sid.0,
                                            )
                                            .await;
                                        }
                                        record_turn_end(
                                            agent_type,
                                            &sid.0,
                                            &reason_str,
                                            turn_started_at_ms,
                                            current_session_model_id(state).await,
                                        )
                                        .await;
                                        let _ = finalize_turn_terminal(
                                            TurnTerminalSource::Upstream(&reason_str),
                                            &mut suspension,
                                            state,
                                            emitter,
                                            conn_id,
                                            sid.0.as_ref(),
                                            agent_type,
                                            mark_awaiting_reply,
                                            delegation_injection.map(|injection| injection.broker.as_ref()),
                                        )
                                        .await;
                                        tracing::info!(
                                            connection_id = %conn_id,
                                            session_id = %sid.0,
                                            stop_reason = %reason_str,
                                            source = "extension_turn_completed",
                                            "[ACP] TurnComplete emitted (state+bus+desktop path returned)"
                                        );
                                        // Prompt RPC may still complete later;
                                        // drain so sacp does not warn about a
                                        // dropped receiver.
                                        tokio::spawn(async move {
                                            match prompt_response.await {
                                                Ok(resp) => {
                                                    tracing::info!(
                                                        stop_reason = ?resp.stop_reason,
                                                        "[ACP] drained late prompt_response \
                                                         after extension turn_completed"
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        error = %e,
                                                        "[ACP] late prompt_response after \
                                                         extension turn_completed failed \
                                                         (benign if agent already closed \
                                                         the turn)"
                                                    );
                                                }
                                            }
                                        });
                                        break;
                                    }
                                    if let Err(e) = MatchDispatch::new(dispatch)
                                        .if_notification(
                                            async |notif: SessionNotification| {
                                                observe_terminal_assoc_from_update(
                                                    &notif.update,
                                                    session_id.0.as_ref(),
                                                    terminal_assoc.as_ref(),
                                                );
                                                let should_poll_now = track_terminal_tool_calls(
                                                    &notif.update,
                                                    &mut tracked_terminal_tool_calls,
                                                );
                                                let bound = merge_terminal_assoc_binds(
                                                    session_id.0.as_ref(),
                                                    terminal_assoc.as_ref(),
                                                    &mut tracked_terminal_tool_calls,
                                                );
                                                // I2: sync accumulated association immediately
                                                // after track/merge and before any other await
                                                // (mark_agent_activity / frontend emit) so a
                                                // multi-terminal tool never stays Terminal(A)
                                                // across an observable await gap.
                                                if should_poll_now || !bound.is_empty() {
                                                    tool_watchdog_sync_tracked_terminals(
                                                        &st,
                                                        &tracked_terminal_tool_calls,
                                                    )
                                                    .await;
                                                }
                                                if is_agent_output_update(&notif.update) {
                                                    turn_had_agent_output = true;
                                                }
                                                // Custom agents have no store
                                                // of their own to parse later.
                                                record_transcript_update(
                                                    agent_type,
                                                    &session_id.0,
                                                    &notif.update,
                                                );
                                                // Soft-watchdog: mark at inbound
                                                // boundary before event conversion.
                                                mark_agent_activity_for_update(
                                                    &st,
                                                    &notif.update,
                                                    chrono::Utc::now(),
                                                )
                                                .await;
                                                maybe_emit_grok_total_tokens_usage(
                                                    &st,
                                                    &h,
                                                    agent_type,
                                                    notif.meta.as_ref(),
                                                )
                                                .await;
                                                emit_conversation_update(
                                                    &st,
                                                    &h,
                                                    agent_type,
                                                    notif.update,
                                                    cwd_opt,
                                                    &mut raw_output_cache,
                                                    &mut cb_state,
                                                    Some(&tracked_terminal_tool_calls),
                                                )
                                                .await;
                                                if should_poll_now || !bound.is_empty() {
                                                    poll_tracked_terminal_tool_calls(
                                                        runtime.as_ref(),
                                                        &session_id,
                                                        &st,
                                                        &h,
                                                        &mut tracked_terminal_tool_calls,
                                                    )
                                                    .await;
                                                }
                                                Ok(())
                                            },
                                        )
                                        .await
                                        .otherwise(async |dispatch| {
                                            if maybe_emit_live_ext_notification(
                                                &st,
                                                &h,
                                                agent_type,
                                                dispatch,
                                                crate::acp::xai_session_notification::PrivateExtEmitMode::InPrompt,
                                                &mut compact_text_emitted_this_turn,
                                            )
                                            .await
                                            {
                                                turn_had_agent_output = true;
                                                st.write()
                                                    .await
                                                    .mark_agent_activity(chrono::Utc::now());
                                            }
                                            Ok(())
                                        })
                                        .await
                                    {
                                        tracing::warn!("[ACP] Ignoring dispatch parse error: {e}");
                                    }
                                }
                                SessionMessage::StopReason(reason) => {
                                    drain_ready_in_prompt_updates(
                                        &mut ReadyUpdateSource::Live(session),
                                        state,
                                        emitter,
                                        agent_type,
                                        &sid,
                                        cwd,
                                        &terminal_runtime,
                                        &terminal_assoc,
                                        &mut tracked_terminal_tool_calls,
                                        &mut raw_output_cache,
                                        &mut cb_state,
                                        &mut grok_retry_reconciler,
                                        &mut turn_had_agent_output,
                                        &mut compact_text_emitted_this_turn,
                                    )
                                    .await;
                                    let _ = merge_terminal_assoc_binds(
                                        sid.0.as_ref(),
                                        terminal_assoc.as_ref(),
                                        &mut tracked_terminal_tool_calls,
                                    );
                                    if !tracked_terminal_tool_calls.is_empty() {
                                        poll_tracked_terminal_tool_calls(
                                            terminal_runtime.as_ref(),
                                            &sid,
                                            state,
                                            emitter,
                                            &mut tracked_terminal_tool_calls,
                                        )
                                        .await;
                                    }
                                    let raw_reason_str = stop_reason_to_str(reason);
                                    let reason_str = rewrite_end_turn_if_empty(
                                        raw_reason_str,
                                        turn_had_agent_output,
                                    );
                                    if record_suspension_terminal_diagnostic(
                                        suspension.as_ref(),
                                        &mut suspension_terminal_diagnostic,
                                        reason_str,
                                    ) {
                                        tracing::info!(
                                            connection_id = %conn_id,
                                            session_id = %sid.0,
                                            stop_reason = %reason_str,
                                            "[ACP] recorded StopReason while draining suspended turn"
                                        );
                                        continue;
                                    }
                                    // Clean completions only: cancelled/empty turns may not
                                    // have been persisted by Cursor.
                                    if reason_str == "end_turn" {
                                        journal_turn_span(
                                            &mut turn_timing_probe,
                                            conn_id,
                                            &sid.0,
                                        )
                                        .await;
                                    }
                                    record_turn_end(
                                        agent_type,
                                        &sid.0,
                                        reason_str,
                                        turn_started_at_ms,
                                        current_session_model_id(state).await,
                                    )
                                    .await;
                                    tracing::info!(
                                        connection_id = %conn_id,
                                        session_id = %sid.0,
                                        agent = %agent_type,
                                        stop_reason = %reason_str,
                                        turn_had_agent_output,
                                        source = "stop_reason_message",
                                        "[ACP] completing turn from SessionMessage::StopReason"
                                    );
                                    let _ = finalize_turn_terminal(
                                        TurnTerminalSource::Upstream(reason_str),
                                        &mut suspension,
                                        state,
                                        emitter,
                                        conn_id,
                                        sid.0.as_ref(),
                                        agent_type,
                                        mark_awaiting_reply,
                                        delegation_injection.map(|injection| injection.broker.as_ref()),
                                    )
                                    .await;
                                    tracing::info!(
                                        connection_id = %conn_id,
                                        session_id = %sid.0,
                                        stop_reason = %reason_str,
                                        source = "stop_reason_message",
                                        "[ACP] TurnComplete emitted (state+bus+desktop path returned)"
                                    );
                                    // Join-only ownership: every parent stop
                                    // reason (including clean `end_turn`) drains
                                    // live Codeg descendants under a stable root
                                    // code. `end_turn` → JoinAbandoned (no-op when
                                    // no live children remain); cancelled →
                                    // ParentCanceled; refusal/max_tokens/empty/
                                    // unknown → ParentTurnFailed. The connection
                                    // stays alive (only the turn ended), so use
                                    // the turn-scoped cancel that keeps the
                                    // parent's `consumed` tool_call memory — a
                                    // late re-emit must not re-register and
                                    // mis-bind the next same-key delegation.
                                    //
                                    // Await inline: the fast tracker + tree
                                    // drain MUST finish before the loop accepts
                                    // the next prompt so it stays scoped to the
                                    // just-ended turn. The broker backgrounds
                                    // the slow child teardown
                                    // (spawner.cancel/disconnect) internally, so
                                    // this won't block on slow agents; its
                                    // idempotent drain also lets the cleanup-
                                    // guard cascade at run_connection exit run
                                    // without race-double-drain.
                                    break;
                                }
                                _ => {}
                            }
                        }
                        // Always tick for Grok (fallback may attach a terminal
                        // without an official ToolCallContent::Terminal). Other
                        // agents only poll while something is already tracked.
                        _ = terminal_poll_interval.tick(),
                            if !tracked_terminal_tool_calls.is_empty()
                                || terminal_assoc
                                    .lock()
                                    .map(|b| b.enabled())
                                    .unwrap_or(false)
                        => {
                            // Merge create-time binds even when tracked was empty
                            // (Grok creates the terminal after tool_call but the
                            // association arrives only via this fallback).
                            let _ = merge_terminal_assoc_binds(
                                sid.0.as_ref(),
                                terminal_assoc.as_ref(),
                                &mut tracked_terminal_tool_calls,
                            );
                            if !tracked_terminal_tool_calls.is_empty() {
                                poll_tracked_terminal_tool_calls(
                                    terminal_runtime.as_ref(),
                                    &sid,
                                    state,
                                    emitter,
                                    &mut tracked_terminal_tool_calls,
                                )
                                .await;
                            }
                        }
                    }
                }

                if disconnect_requested {
                    tracing::info!(
                        "[ACP] closing connection loop after disconnect; connection_id={conn_id}"
                    );
                    break;
                }

                if !status_restored_by_suspension {
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::StatusChanged {
                            status: ConnectionStatus::Connected,
                        },
                    )
                    .await;
                }
            }
            ConversationInput::Command(ConnectionCommand::RespondPermission {
                request_id,
                option_id,
            }) => {
                if let Some(pending) = perms.lock().await.remove(&request_id) {
                    pending.respond_selected(option_id);
                    emit_with_state(state, emitter, AcpEvent::PermissionResolved { request_id })
                        .await;
                    tool_watchdog_resume(state).await;
                }
            }
            ConversationInput::Command(ConnectionCommand::SetMode { .. })
            | ConversationInput::Command(ConnectionCommand::SetConfigOption { .. }) => {
                unreachable!("ancillary commands are owned before dispatch")
            }
            ConversationInput::Control(ConnectionControl::SuspendForDelegation {
                reply, ..
            }) => {
                let _ = reply.send(Err(AcpError::protocol("suspend_no_active_turn")));
            }
            ConversationInput::Control(ConnectionControl::CancelTerminal {
                session_id,
                terminal_id,
                reply,
            }) => {
                // Idle outer loop: still admit terminal kill without ending a turn.
                admit_cancel_terminal_control(&terminal_runtime, session_id, terminal_id, reply);
            }
            ConversationInput::Control(ConnectionControl::CancelTurn { .. }) => {
                // No active turn in the outer loop — generation-guarded claim is stale.
            }
            ConversationInput::Command(ConnectionCommand::GoalControl { action }) => {
                let cx = session.connection();
                let sid = session.session_id().clone();
                if let Err(e) = send_goal_control(&cx, &sid, action).await {
                    emit_with_state(
                        state,
                        emitter,
                        AcpEvent::Error {
                            message: format!("Failed to control goal: {e}"),
                            agent_type: agent_type.to_string(),
                            code: None,
                            // Recoverable: an idle pause/clear failure leaves the
                            // connection alive.
                            terminal: false,
                        },
                    )
                    .await;
                }
            }
            ConversationInput::Control(ConnectionControl::Cancel) => {
                let cx = session.connection();
                let sid = session.session_id().clone();
                let _ = cx.send_notification_to(Agent, CancelNotification::new(sid.clone()));
                cancel_pending_permissions(state, emitter, perms).await;
                terminal_runtime
                    .release_all_for_session(sid.0.as_ref())
                    .await;
                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::StatusChanged {
                        status: ConnectionStatus::Connected,
                    },
                )
                .await;
                // Cascade-cancel any pending delegations owned by this parent.
                // Reached when Cancel arrives between prompts (idle path); the
                // inner Cancel handler covers mid-prompt. Both must trigger
                // because the per-prompt cancel path doesn't tear down the
                // parent connection, so the cleanup-guard cancel_by_parent
                // at run_connection's exit wouldn't fire. Turn-scoped for that
                // same reason: the connection stays alive, so keep the parent's
                // `consumed` tool_call memory (a re-emit must not mis-bind the
                // next same-key delegation).
                //
                // Awaited inline (fast drain before the next prompt; broker
                // backgrounds the slow child teardown): see inner Cancel
                // handler above for rationale.
                if let Some(inj) = delegation_injection {
                    inj.broker
                        .cancel_by_parent_turn(
                            conn_id,
                            crate::acp::delegation::types::ParentTurnEndReason::ParentCanceled,
                        )
                        .await;
                    inj.questions.cancel_questions_by_parent(conn_id).await;
                    inj.plan_approvals
                        .cancel_plan_approvals_by_parent(conn_id)
                        .await;
                }
            }
            ConversationInput::Command(ConnectionCommand::Fork { reply }) => {
                if !supports_fork {
                    let _ = reply.send(Err(AcpError::protocol(
                        "This agent does not support session/fork".to_string(),
                    )));
                    continue;
                }
                let cx = session.connection();
                let sid = session.session_id().clone();
                tracing::info!(
                    "[ACP] Sending session/fork for session_id={} cwd={}",
                    sid.0,
                    cwd
                );
                // Same immutable route plan + shell snapshot as new/load/resume
                // (Codeg Claude re-asserts Agent/Task deny; native unchanged).
                // Never re-read global terminal settings during fork.
                let purpose = state.read().await.purpose;
                let terminal_meta = match session_request_meta(
                    agent_type,
                    route_plan,
                    shell_spec,
                    adapter_for(agent_type),
                    purpose,
                ) {
                    Ok(meta) => meta,
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        continue;
                    }
                };
                let result = crate::acp::fork::fork_session(&cx, &sid, cwd, terminal_meta).await;
                match result {
                    Ok((fork_response, fork_models_raw)) => {
                        tracing::info!(
                            "[ACP] Fork succeeded: new_session_id={}",
                            fork_response.session_id.0
                        );
                        return Ok(Some(ForkExitInfo {
                            fork_response,
                            fork_models_raw,
                            original_session_id: sid.0.to_string(),
                            reply,
                            connection: cx,
                        }));
                    }
                    Err(e) => {
                        tracing::error!("[ACP] Fork failed: {e}");
                        let _ = reply.send(Err(e));
                    }
                }
            }
            ConversationInput::Control(ConnectionControl::Disconnect) => {
                break;
            }
            ConversationInput::ChannelsClosed => {
                record_session_channel_loss(delegation_injection, conn_id);
                break;
            }
        }
    }
    Ok(None)
}

/// Serialize tool-call `content` blocks into a single human-readable string.
///
/// `include_diffs = false` skips `Diff` blocks. Used when the edit has been
/// hoisted into a synthesized canonical `raw_input` (see
/// `synthesize_edit_input_from_diffs`): without this the same edit ships twice
/// (doubling the event) and the hunkless full-file `--- /+++` blob stays in the
/// tool `output`, where `extractEditLineChangeStats` mis-counts it as full-file
/// +/- totals in the card header even though the body shows the compact diff.
pub(crate) fn serialize_tool_call_content(
    content: &[ToolCallContent],
    include_diffs: bool,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for item in content {
        match item {
            ToolCallContent::Content(c) => {
                if let ContentBlock::Text(text) = &c.content {
                    parts.push(text.text.clone());
                }
            }
            ToolCallContent::Diff(diff) if include_diffs => {
                let path = diff.path.display();
                let mut diff_text = format!("--- {path}\n+++ {path}\n");
                if let Some(old) = &diff.old_text {
                    for line in old.lines() {
                        diff_text.push_str(&format!("-{line}\n"));
                    }
                }
                for line in diff.new_text.lines() {
                    diff_text.push_str(&format!("+{line}\n"));
                }
                parts.push(diff_text);
            }
            ToolCallContent::Terminal(t) => {
                parts.push(format!("[Terminal: {}]", t.terminal_id));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Synthesize a canonical edit `raw_input` from `ToolCallContent::Diff` block(s).
///
/// codex-acp reports file edits as ACP `Diff` content blocks and leaves
/// `raw_input` empty — the edit lives only in `content`, and the ACP `title` is
/// the diff header `--- <path>`. With no `raw_input` the frontend classifier
/// (`inferLiveToolName`) falls back to `normalizeToolName(title)`, which returns
/// unrecognized strings verbatim, so the tool call renders as a generic tool
/// literally *named* `--- <path>` (wrench icon, raw header as the title) instead
/// of an edit card. The historical path is unaffected because the JSONL parser
/// stores codex's native `*** Begin Patch` text.
///
/// Reconstructing from the already-serialized `--- /+++` string would be lossy
/// (content lines beginning with `-`/`+`/`---`/`+++`, the old/new boundary,
/// CRLF). Here the structured `Diff` is still intact, so map it losslessly:
/// - exactly one Diff  -> `{"file_path","old_string","new_string"}`
/// - multiple Diffs    -> `{"changes":{"<path>":{"old_text","new_text"},…}}`
///
/// Both shapes classify as `"edit"` (`inferFromInput`) and render through the
/// existing `EditToolInput` / `EditChangesToolInput` → `generateUnifiedDiff`
/// pipeline (a real hunk diff, minimal even for full-file old/new). Returns
/// `None` when `content` carries no `Diff`, so callers only fall back to it when
/// the agent supplied no `raw_input` of its own.
pub(crate) fn synthesize_edit_input_from_diffs(content: &[ToolCallContent]) -> Option<String> {
    // Keep `old_text` as `Option`: ACP reports `None` for a newly created file
    // (`Diff.old_text` semantics). That distinction is the whole point of this
    // function's fix — collapsing `None` to `""` and emitting an edit shape
    // makes the frontend build a `--- a/<path>` diff, which `isAddedFileDiff`
    // does NOT match, so a freshly created file mis-renders as a modification
    // (the historical apply_patch `*** Add File:` path classifies it correctly).
    let diffs: Vec<(String, Option<String>, String)> = content
        .iter()
        .filter_map(|item| match item {
            ToolCallContent::Diff(diff) => Some((
                diff.path.display().to_string(),
                diff.old_text.clone(),
                diff.new_text.clone(),
            )),
            _ => None,
        })
        .collect();

    match diffs.as_slice() {
        [] => None,
        // New file (old_text absent) → write shape. `inferFromInput` classifies
        // `{file_path, content}` as `write`, whose diff builder emits the
        // `--- /dev/null` header `isAddedFileDiff` keys on → renders as a new
        // file, matching the reloaded-from-DB path.
        [(path, None, new)] => Some(
            serde_json::json!({
                "file_path": path,
                "content": new,
            })
            .to_string(),
        ),
        // Edit → canonical `{old_string,new_string}` for the frontend's
        // `generateUnifiedDiff` (a real hunk diff, minimal even for full-file
        // old/new).
        [(path, Some(old), new)] => Some(
            serde_json::json!({
                "file_path": path,
                "old_string": old,
                "new_string": new,
            })
            .to_string(),
        ),
        many => {
            let mut changes = serde_json::Map::new();
            for (path, old, new) in many {
                // Per-entry, mirror the single-diff split: a new file gets a
                // ready-made creation diff (`buildChunkFromEditChange` returns
                // it verbatim → `--- /dev/null` → new file); an edit hands
                // old/new text to the frontend to diff.
                let entry = match old {
                    None => serde_json::json!({ "diff": build_new_file_diff(path, new) }),
                    Some(old) => serde_json::json!({ "old_text": old, "new_text": new }),
                };
                changes.insert(path.clone(), entry);
            }
            Some(serde_json::json!({ "changes": changes }).to_string())
        }
    }
}

/// Build a minimal unified diff for a newly created file: the `--- /dev/null`
/// header the frontend's `isAddedFileDiff` keys on, then every line of
/// `new_text` as an addition. Byte-for-byte identical to the frontend `write`
/// op's diff builder (`session-files.ts`), so a multi-file batch's new-file
/// entries render exactly like a single-file creation.
fn build_new_file_diff(path: &str, new_text: &str) -> String {
    // `split('\n')` (not `lines()`) mirrors the frontend `content.split("\n")`:
    // it keeps the trailing empty segment from a final newline, so the `+N`
    // count and the trailing `+` addition line match exactly.
    let lines: Vec<&str> = new_text.split('\n').collect();
    let mut out = format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@", lines.len());
    for line in lines {
        out.push('\n');
        out.push('+');
        out.push_str(line);
    }
    out
}

/// Extract `ContentBlock::Image` payloads from a `ToolCallContent` slice.
/// Returns `None` when no images are present so the upstream `images` field
/// on `AcpEvent::ToolCall(Update)` stays absent for non-image tool calls
/// (preserves replace-on-update semantics: an absent field means "keep
/// prior", a `Some(vec)` replaces).
pub(crate) fn extract_tool_call_images(
    content: &[ToolCallContent],
) -> Option<Vec<ToolCallImageInfo>> {
    let mut imgs: Vec<ToolCallImageInfo> = Vec::new();
    for item in content {
        if let ToolCallContent::Content(c) = item {
            if let ContentBlock::Image(img) = &c.content {
                imgs.push(ToolCallImageInfo {
                    data: img.data.clone(),
                    mime_type: img.mime_type.clone(),
                    uri: img.uri.clone(),
                });
            }
        }
    }
    if imgs.is_empty() {
        None
    } else {
        Some(imgs)
    }
}

/// If the output looks like numbered lines (`   115→content`), strip them
/// and return `{"start_line":N,"content":"..."}` — same as the historical path.
fn structurize_live_output(text: &str) -> String {
    if let Some(json) = crate::parsers::strip_numbered_lines(text) {
        return json;
    }
    text.to_string()
}

/// Resolve line numbers for live tool call input.
///
/// Resolve line numbers for live tool call input (string form).
///
/// - For apply_patch with bare `@@`: resolve line numbers in place.
/// - For canonical edit JSON: inject `_start_line`.
fn resolve_live_tool_input(text: &str, cwd: Option<&str>) -> String {
    if text.contains("@@\n") || text.contains("@@\r\n") {
        if let Some(resolved) = crate::parsers::resolve_patch_text(text, cwd) {
            return resolved;
        }
    }
    if let Ok(mut parsed) = serde_json::from_str::<serde_json::Value>(text) {
        if inject_start_line(&mut parsed, cwd) {
            return parsed.to_string();
        }
    }
    text.to_string()
}

/// Try to inject `_start_line` into a JSON object with `file_path` + `old_string`.
/// Returns true if injected.
fn inject_start_line(value: &mut serde_json::Value, cwd: Option<&str>) -> bool {
    let obj = match value.as_object_mut() {
        Some(o) => o,
        None => return false,
    };
    let fp = obj
        .get("file_path")
        .or_else(|| obj.get("path"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let old_str = obj
        .get("old_string")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    if let (Some(fp), Some(old_str)) = (fp, old_str) {
        if let Some(sl) = find_string_start_line(&fp, &old_str, cwd) {
            obj.insert("_start_line".to_string(), serde_json::json!(sl));
            return true;
        }
    }
    false
}

/// Find the 1-based start line of `needle` in the file at `path`.
fn find_string_start_line(path: &str, needle: &str, cwd: Option<&str>) -> Option<u64> {
    if needle.is_empty() {
        return None;
    }
    let file_lines = crate::parsers::load_file_lines(path, cwd)?;
    let file_content = file_lines.join("\n");
    let byte_offset = file_content.find(needle)?;
    Some(file_content[..byte_offset].matches('\n').count() as u64 + 1)
}

pub(crate) fn json_value_to_text(val: &Option<serde_json::Value>) -> Option<String> {
    match val {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(v) if !v.is_null() => Some(v.to_string()),
        _ => None,
    }
}

/// Resolve the live `raw_output` string for a Grok tool call.
///
/// Grok reports terminal output in the standard `content[]` channel (clean,
/// human-readable text) AND in a structured `rawOutput` object whose readable
/// text lives only in the string `output_for_prompt` (its `output` field is a
/// raw byte array, and the remaining keys — `command`, `exit_code`, … — are
/// metadata). Feeding that object through `json_value_to_text` stringifies the
/// whole thing into a JSON blob that (a) shadows the clean `content` — the live
/// renderer's `raw_output_chunks` win over `content`
/// (`conversation-runtime-store.ts`) — and (b) is then dropped by the terminal
/// renderer as a metadata-only "command envelope"
/// (`commandOutputFromJsonString` returns `""`), so a finished command shows no
/// result during live streaming even though the history parser renders it fine.
///
/// Mirror the history parser (`parsers/grok.rs::update_tool_output`): prefer the
/// already-serialized `content`, and only fall back — when `content` is empty —
/// to the object's string `output_for_prompt` (Bash/terminal), a background-task
/// `TaskOutput` envelope (see `parsers::grok::grok_task_output_envelope`, the
/// one exception to "never emit the object blob": the frontend parses it into a
/// background-task card), or, for an MCP `rawOutput`, the text under `output`
/// (see grok_mcp_output_text). Returning `None` lets the frontend render
/// `content`. Non-object / absent / unrecognized `rawOutput` → `None`.
///
/// Note: `content` here is `serialize_tool_call_content`, which for a Grok
/// terminal call is the plain text block (verified against real `~/.grok`
/// data). It could in principle also serialize `Diff`/`Terminal` blocks, in
/// which case a Grok tool carrying ONLY such a block plus `output_for_prompt`
/// would render the serialized block instead of the prompt text — but Grok's
/// `run_terminal_command` emits `content:text`, so this stays parity with
/// history for the shapes Grok actually produces.
fn grok_live_tool_output(
    content: &Option<String>,
    raw_output: &Option<serde_json::Value>,
) -> Option<String> {
    if content.as_deref().is_some_and(|c| !c.trim().is_empty()) {
        return None;
    }
    let raw = raw_output.as_ref()?;
    // Bash / terminal calls: the readable text lives only in `output_for_prompt`.
    if let Some(text) = raw
        .get("output_for_prompt")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Some(text.to_string());
    }
    // Background-task polls (`get_command_or_subagent_output`): the command,
    // exit code and shell text all live under the `TaskOutput` envelope, which
    // matches none of the paths around it — without this the card streams empty.
    // Shared with the history parser so both hand the frontend the same string.
    if let Some(envelope) = crate::parsers::grok::grok_task_output_envelope(raw) {
        return Some(envelope);
    }
    // MCP calls (Grok's `use_tool` envelope): the result text lives under
    // `output.<*Output>` instead (see grok_mcp_output_text). Without this a
    // finished MCP call — e.g. the `delegate_to_agent` ack carrying
    // `task_id=…` — would surface no output at all.
    grok_mcp_output_text(raw)
}

/// Grok wraps every MCP tool invocation in a generic `use_tool` envelope whose
/// `raw_input` is `{"tool_name": "<server>__<tool>", "tool_input": {..real args..}}`.
/// Peel it so the call is correlated (delegation `lifecycle.rs`), classified, and
/// parsed as a direct MCP call — identical to how hosts like Claude Code surface
/// MCP tools. Without this, Grok's `delegate_to_agent` (and the other codeg-mcp
/// companion tools) never resolve to their dedicated cards, and the delegation
/// broker can't correlate the parent tool call to bind the sub-session.
///
/// Returns `(inner_tool_name, inner_tool_input)` only for the envelope shape —
/// a non-empty string `tool_name` plus a `tool_input` value — so Grok's native
/// tools (`run_terminal_command`, `search_tool`, `spawn_subagent`, …), which
/// carry their args directly, pass through untouched.
fn unwrap_grok_use_tool(
    raw_input: Option<&serde_json::Value>,
) -> Option<(String, serde_json::Value)> {
    let obj = raw_input?.as_object()?;
    let tool_name = obj
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())?;
    let tool_input = obj.get("tool_input")?;
    Some((tool_name.to_string(), tool_input.clone()))
}

/// Extract the human-readable text from a Grok MCP `rawOutput`
/// (`{"type":"MCP","output":{"OkayOutput":"…"}}`, or an `*Output` error variant).
/// The MCP result is the first string value under `output` (`output` may itself
/// be a bare string on some tools). Returns `None` for a non-MCP `rawOutput` so
/// the caller can fall through to the Bash/`output_for_prompt` path.
fn grok_mcp_output_text(raw_output: &serde_json::Value) -> Option<String> {
    if raw_output.get("type").and_then(serde_json::Value::as_str) != Some("MCP") {
        return None;
    }
    let output = raw_output.get("output")?;
    if let Some(text) = output.as_str() {
        return (!text.is_empty()).then(|| text.to_string());
    }
    // First NON-EMPTY string value (the singleton `*Output` variant). Filtering
    // inside `find_map` — not after — so an earlier empty-string sibling can't
    // shadow a later populated one.
    output
        .as_object()?
        .values()
        .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
        .map(str::to_string)
}

/// Recover a codeg-mcp companion tool's identity from its RESULT text, for
/// Cursor sessions only.
///
/// Cursor's ACP layer announces every MCP call from the first streaming
/// partial — before `McpArgs` exists — so the announcement is the literal
/// title "MCP: tool" with an empty `raw_input`, and `sendToolCallUpdate`
/// (bundle-verified) never forwards `title`/`raw_input` again. The ONLY
/// wire signal that ever identifies the call is the MCP result text arriving
/// on the completion update, and for the codeg-mcp companion tools that text
/// is a codeg-owned contract:
///
/// * a `delegate_to_agent` ack opens with
///   `"Delegation successful. task_id="` (`broker.rs::running_ack`);
/// * `get_delegation_status` renders the compact `{"tasks":[..]}` JSON
///   (`companion.rs::render_batch_report`), whose items carry `task_id` +
///   a `status` from the fixed report vocabulary.
///
/// (`cancel_delegation` results are free-form report messages with no stable
/// prefix, so a canceled call keeps the generic title — a rare op, accepted.)
///
/// Matching those shapes lets the completion update rewrite the title to the
/// canonical `<server>__<tool>` form (the exact name the history parser
/// derives from `McpArgs`), so the frontend resolves the dedicated delegation
/// cards instead of a generic "MCP: tool" call. Returns `None` for everything
/// else — an unrecognized result keeps the wire title untouched. Callers gate
/// the sniff to calls ANNOUNCED with the identity-less "MCP: tool" title
/// (`CodeBuddyLiveState::cursor_generic_mcp_ids`), so a native tool whose
/// output merely echoes these shapes is never re-titled.
fn cursor_companion_title_from_content(content: Option<&str>) -> Option<&'static str> {
    let text = content?.trim_start();
    if text.starts_with("Delegation successful. task_id=") {
        return Some(crate::acp::delegation::DELEGATE_TOOL_REWRITE_TITLE);
    }
    // Cheap guards before the full JSON parse: the status report is a JSON
    // object whose first key is `tasks`.
    if !text.starts_with('{') || !text.contains("\"tasks\"") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let tasks = v.get("tasks")?.as_array()?;
    let is_report_item = |t: &serde_json::Value| {
        t.get("task_id").and_then(|x| x.as_str()).is_some()
            && t.get("status").and_then(|x| x.as_str()).is_some_and(|s| {
                matches!(
                    s,
                    "running" | "completed" | "failed" | "canceled" | "unknown"
                )
            })
    };
    if !tasks.is_empty() && tasks.iter().all(is_report_item) {
        return Some(crate::acp::delegation::STATUS_TOOL_REWRITE_TITLE);
    }
    None
}

/// Mirrors `parsers/opencode.rs:425-429` (and `parsers/codebuddy.rs`'s
/// `subagent_type → "Agent"` rewrite) so streaming and reload-from-DB render the
/// same Agent card. The SQLite-side condition is
/// `tool == "task" && state.input.subagent_type IS NOT NULL`, where `tool` is the
/// agent's **internal** tool name. ACP only exposes a user-facing `title` (e.g.
/// "Explore project structure") rather than the internal tool name, so we cannot
/// replicate the `tool == "task"` half of the AND here. We instead anchor on a
/// known sub-agent-capable `agent_type` (OpenCode and CodeBuddy — both surface a
/// description-style title and the standard `{…, subagent_type}` input, and never
/// emit a bare top-level `subagent_type` for anything but a sub-agent) plus the
/// non-empty `subagent_type` string in `raw_input` — together these uniquely
/// identify a sub-agent invocation in practice. Other agents stay excluded to
/// avoid any cross-agent collision a generic `subagent_type` field could cause.
fn is_subagent_invocation(agent_type: AgentType, raw_input: &Option<String>) -> bool {
    if !matches!(agent_type, AgentType::OpenCode | AgentType::CodeBuddy) {
        return false;
    }
    let Some(text) = raw_input.as_deref() else {
        return false;
    };
    // Cheap substring guard avoids parsing large `raw_input` payloads
    // (e.g. prompts with many KB of context) when the field is absent.
    if !text.contains("subagent_type") {
        return false;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return false;
    };
    value
        .get("subagent_type")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// CodeBuddy routes MCP tools through its `DeferExecuteTool` virtualization
/// layer, which surfaces over ACP as a tool call whose `raw_input` wraps the real
/// call as `{ "toolName": "mcp__…", "params": { … } }`. Return that inner
/// `toolName` so the caller can rewrite the live `title` to it — making the
/// frontend resolve the dedicated card (delegation / question / …), mirroring the
/// historical unwrap in `parsers/codebuddy.rs`. `raw_input` is left untouched
/// (the cards peel `params` themselves, and that keeps `inferFromInput` from
/// misclassifying `cancel_delegation`'s `{task_id}` as a generic task).
fn codebuddy_deferred_tool_name(
    agent_type: AgentType,
    raw_input: &Option<String>,
) -> Option<String> {
    if agent_type != AgentType::CodeBuddy {
        return None;
    }
    let text = raw_input.as_deref()?;
    // Cheap substring guard before parsing a potentially large payload.
    if !text.contains("toolName") {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    crate::parsers::codebuddy::deferred_tool_name(&value).map(|s| s.to_string())
}

/// CodeBuddy ships a deferred MCP tool's RESULT as a single re-serialized
/// `{ "type": "text", "text": <inner> }` content part (the OpenAI-Agents content
/// shape), where `<inner>` is the MCP `CallToolResult` content text — for the
/// delegation companion, the compact report / `{ "tasks": [...] }` JSON. The
/// dedicated cards (`parseStatusReport` / `parseToolOutput`) expect that bare
/// inner payload (the content-only host shape they already handle for Claude
/// Code), NOT this wrapper, so a live `get_delegation_status` / `cancel_delegation`
/// poll otherwise renders as raw JSON text. Peel the wrapper to its inner `text`,
/// mirroring the historical `deferred_result_envelope` normalization in
/// `parsers/codebuddy.rs`.
///
/// Gated on CodeBuddy + the exact wrapper shape (`type == "text"` with a string
/// `text`): a non-deferred result (Bash/Read/ToolSearch/…) is never a lone
/// `{type,text}` object, and no delegation report carries a top-level `type`, so
/// those pass through untouched. Unlike the title rewrite, this needs no
/// `raw_input`, so it also normalizes a result-only `ToolCallUpdate` that omits it.
fn unwrap_codebuddy_deferred_output(agent_type: AgentType, text: &str) -> Option<String> {
    if agent_type != AgentType::CodeBuddy {
        return None;
    }
    // Cheap substring guard before parsing a potentially large payload.
    if !text.contains("\"type\"") {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let obj = value.as_object()?;
    if obj.get("type").and_then(|t| t.as_str()) != Some("text") {
        return None;
    }
    obj.get("text").and_then(|t| t.as_str()).map(str::to_string)
}

/// True when a CodeBuddy tool call's ACP `_meta` identifies it as a native
/// sub-agent (`Agent`) invocation. CodeBuddy tags this in `_meta` from the FIRST
/// frame (`codebuddy.ai/toolName == "Agent"`) and later adds
/// `codebuddy.ai/isSubagent` / `subagentType` — whereas the `subagent_type`
/// field in `raw_input` (see `is_subagent_invocation`) only streams in dozens of
/// frames later. Reading the meta lets the title rewrite fire on frame 1, so the
/// Agent pill never spends an opening window classified as a generic tool (and
/// its child tool calls, which carry `codebuddy.ai/parentToolCallId` every frame,
/// nest from the start). Gated on CodeBuddy so the generic `codebuddy.ai/*` keys
/// can never affect another agent.
fn codebuddy_meta_marks_subagent(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::CodeBuddy {
        return false;
    }
    let Some(meta) = meta else {
        return false;
    };
    if meta.get("codebuddy.ai/toolName").and_then(|v| v.as_str()) == Some("Agent") {
        return true;
    }
    if meta
        .get("codebuddy.ai/isSubagent")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return true;
    }
    meta.get("codebuddy.ai/subagentType")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// True when a Codex live `tool_call` is a `subAgentActivity` mapping
/// (codex-acp #304, v1.1.3+). codex-acp maps codex `subAgentActivity`
/// notifications onto ACP `tool_call(kind:other)` carrying
/// `_meta.codex.subagent = {threadId, path, activity}`. codeg already renders
/// codex collaboration from the `collabAgentToolCall` path (spawnAgent/wait/
/// closeAgent — see `collab-tool.ts`) and reconstructs the full nested
/// transcript on history reload from `agent-<id>.jsonl` (see
/// `parsers/codex.rs`), so this new live signal is redundant with what codeg
/// already shows. Suppressed at the emit point (keeping live and DB-reload
/// consistent — a suppressed event is never persisted) to preserve the current
/// live behavior. Gated on Codex.
fn is_codex_subagent_activity(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::Codex {
        return false;
    }
    meta.and_then(|m| m.get("codex"))
        .and_then(|codex| codex.get("subagent"))
        .is_some()
}

/// Extract a retryable-turn-error indicator from a Codex `session_info_update`'s
/// `_meta` (codex-acp #289, v1.1.3+). codex ships a transient, auto-retried
/// error as `_meta.codex.error = {message, codexErrorInfo, additionalDetails,
/// turnId, willRetry}` and keeps the prompt alive; it emits this only when
/// `willRetry == true`. Returns `(message, http_status)` when a non-empty
/// message is present. `codexErrorInfo` may be a bare string enum, an object
/// variant carrying an inner `httpStatusCode`, or absent — only the object form
/// yields a status. Defensively refuses a `willRetry == false` payload so a
/// terminal error can never render as "retrying".
fn codex_retry_indicator(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<(String, Option<i64>)> {
    let err = meta?.get("codex")?.get("error")?;
    if err.get("willRetry").and_then(|v| v.as_bool()) == Some(false) {
        return None;
    }
    let message = err.get("message").and_then(|v| v.as_str())?.trim();
    if message.is_empty() {
        return None;
    }
    let http_status = err
        .get("codexErrorInfo")
        .and_then(|info| info.as_object())
        .and_then(|obj| obj.values().next())
        .and_then(|inner| inner.get("httpStatusCode"))
        .and_then(|v| v.as_i64());
    Some((message.to_string(), http_status))
}

/// True when an available command is really a config-option state toggle rather
/// than an invokable slash command (codex-acp #293, v1.1.4). codex advertises
/// e.g. `/plan` as an `AvailableCommand` tagged
/// `_meta.commandAction = {kind:"setConfigOption", configId:"collaboration_mode",
/// value:"plan", resetValue:"default", presentation:"state"}` — codex's signal
/// that the client should represent it as STATE. codeg already surfaces that
/// state as the `collaboration_mode` config-option selector (the generic
/// `SessionConfigOption` path), so also listing `/plan` as a slash command is
/// redundant and its static "Turn plan mode on" description is wrong once plan
/// mode is already on. Suppress these from the command list. Commands with any
/// other action kind (e.g. `/goal`'s `prefixPrompt`, which takes an objective
/// argument) are real commands and kept. Gated on Codex — `commandAction` is a
/// codex-private `_meta` extension (the ACP schema has no such type).
fn is_config_option_state_command(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::Codex {
        return false;
    }
    meta.and_then(|m| m.get("commandAction"))
        .and_then(|action| action.get("kind"))
        .and_then(|kind| kind.as_str())
        == Some("setConfigOption")
}

/// True when a CodeBuddy sub-agent tool call's `_meta` marks it as a BACKGROUND
/// sub-agent (`codebuddy.ai/isBackground == true`). A background sub-agent runs
/// concurrently with the main agent, so the suppression-window invariant (parent
/// blocked → only sub-agent chunks in the window) does NOT hold for it — see
/// `track_subagent_window`, which excludes it from the window. Gated on CodeBuddy.
fn codebuddy_meta_marks_background(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::CodeBuddy {
        return false;
    }
    meta.and_then(|m| m.get("codebuddy.ai/isBackground"))
        .and_then(|v| v.as_bool())
        == Some(true)
}

/// True when a CodeBuddy thought/message `ContentChunk`'s own `_meta` marks the
/// chunk as sub-agent output (`codebuddy.ai/isSubagent`, or a
/// `codebuddy.ai/parentToolCallId` link to the Agent call). This is a precision
/// supplement to the open-sub-agent window — CodeBuddy is not confirmed to
/// populate chunk `_meta`, so suppression never relies on it alone. Gated on
/// CodeBuddy.
fn codebuddy_chunk_marks_subagent(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::CodeBuddy {
        return false;
    }
    let Some(meta) = meta else {
        return false;
    };
    if meta
        .get("codebuddy.ai/isSubagent")
        .and_then(|v| v.as_bool())
        == Some(true)
    {
        return true;
    }
    meta.get("codebuddy.ai/parentToolCallId")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// Whether a live thought/message chunk should be dropped from the top-level
/// stream because it belongs to a CodeBuddy sub-agent (whose work is already
/// represented by the Agent pill + its nested tool calls).
///
/// Claude Code is NOT handled here: its sub-agent chunks (claude-agent-acp
/// ≥0.63 with the `subagent-transcript` capability) arrive with a precise
/// per-chunk `_meta.claudeCode.parentToolUseId` and are forwarded WITH that
/// attribution instead of suppressed — see `claude_chunk_parent_tool_use_id`.
///
/// Suppress while we're inside an open sub-agent window OR when the chunk's own
/// meta marks it. The window safety rests on a structural invariant: the window
/// only ever holds FOREGROUND (blocking) sub-agents — a synchronous `Agent` tool
/// call suspends the parent model until the tool returns its result, so between
/// that call's open frame and its terminal frame the main session carries ONLY
/// the sub-agent's chunks, never main-agent output. BACKGROUND sub-agents (which
/// run concurrently and could interleave main-agent output) are deliberately
/// excluded from the window by `track_subagent_window`, so `window_open` can
/// never cause a main-agent chunk to be dropped. Gated on CodeBuddy; every other
/// agent always emits.
fn should_suppress_subagent_chunk(
    agent_type: AgentType,
    window_open: bool,
    chunk_meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    if agent_type != AgentType::CodeBuddy {
        return false;
    }
    window_open || codebuddy_chunk_marks_subagent(agent_type, chunk_meta)
}

/// Extract the update-level `_meta.claudeCode.parentToolUseId` of a live
/// text/thought chunk — set by claude-agent-acp ≥0.63 on a subagent's
/// forwarded chunks once the client advertises the `subagent-transcript`
/// capability (see `build_client_capabilities`). The chunk is emitted WITH
/// this attribution (never suppressed): the frontend routes parented chunks
/// into the live Agent capsule instead of the main thread. Gated on
/// ClaudeCode so no other agent's namespaced meta can alias into parented
/// routing; empty strings are treated as absent.
fn claude_chunk_parent_tool_use_id(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    if agent_type != AgentType::ClaudeCode {
        return None;
    }
    meta?
        .get("claudeCode")?
        .get("parentToolUseId")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Maintain the set of OPEN CodeBuddy sub-agent tool calls (`open`). `is_agent`
/// is true once `resolve_rewritten_title` classified this `tool_call_id` as a
/// native sub-agent (`"agent"`). A non-final status opens the window; a final
/// status (`completed` / `failed`) closes it and records the id in `closed`, so a
/// stray late non-final frame can't re-open an already-finished sub-agent.
///
/// `is_background` (from `codebuddy_meta_marks_background`) EXCLUDES a sub-agent
/// from the window: a background sub-agent runs concurrently with the main agent,
/// so the "window holds only sub-agent chunks" invariant that makes
/// `should_suppress_subagent_chunk` safe would not hold. We treat a background
/// marker exactly like a terminal frame (remove + record closed) so it can never
/// suppress interleaved main-agent output. (`isBackground` can stream in a frame
/// or two after the call opens, so a background sub-agent's earliest chunks may be
/// briefly suppressed before the marker arrives — an accepted, rare imperfection;
/// the user-reported case is foreground, where the marker is `false`.)
///
/// Gated on CodeBuddy so a single-agent-type connection of any other agent stays
/// inert.
fn track_subagent_window(
    agent_type: AgentType,
    is_agent: bool,
    is_background: bool,
    status: Option<&str>,
    tool_call_id: &str,
    open: &mut HashSet<String>,
    closed: &mut HashSet<String>,
) {
    if agent_type != AgentType::CodeBuddy || !is_agent {
        return;
    }
    let is_final = matches!(status, Some("completed") | Some("failed"));
    if is_final || is_background {
        open.remove(tool_call_id);
        closed.insert(tool_call_id.to_string());
    } else if !closed.contains(tool_call_id) {
        open.insert(tool_call_id.to_string());
    }
}

/// Per-session CodeBuddy live-stream state threaded through
/// `emit_conversation_update`. Consolidates the authoritative title rewrites and
/// the open-sub-agent suppression window so CodeBuddy's sparse, multi-frame
/// sub-agent stream resolves to a stable Agent pill (whose children nest) with
/// its interleaved thought/message chunks suppressed. Created per connection,
/// shared across the idle and active-turn loops; the historical-replay path uses
/// a throwaway instance. Mirrors `ToolCallOutputCache`'s lifetime.
#[derive(Default)]
struct CodeBuddyLiveState {
    /// tool_call_id → authoritative title: `"agent"` for a native sub-agent, or
    /// the inner `mcp__…` name for a `DeferExecuteTool` MCP call. Re-asserted on
    /// every later frame so a status-only update can't downgrade the card.
    title_overrides: HashMap<String, String>,
    /// Sub-agent tool calls currently OPEN (classified, not yet completed/failed).
    /// While non-empty, interleaved thought/message chunks belong to a sub-agent
    /// and are suppressed from the top-level stream (matching Claude Code).
    open_subagents: HashSet<String>,
    /// Sub-agent tool calls that already reached a final status — guards against a
    /// stray late non-final frame re-opening a finished sub-agent.
    closed_subagents: HashSet<String>,
    /// Objective of the Codex `/goal` run currently open on this connection (set
    /// by the latest `active` `session_info_update` goal, cleared on any terminal
    /// status). Lets a later `goal:null` close the run by objective — and be a
    /// no-op when no run is open. See `crate::acp::codex_goal::next_goal_marker`.
    ///
    /// This lives here (not in `SessionState`) because `CodeBuddyLiveState` and
    /// `SessionState` share one lifetime: a browser refresh / reconnect re-attaches
    /// to the *running* connection (`find_connection_for_reuse`), keeping both; a
    /// brand-new connection resets both together (empty live blocks + fresh state).
    /// So this state never resets while goal blocks it would close still exist.
    codex_open_goal: Option<String>,
    /// Monotonic per-connection counter for synthetic goal tool-call ids. Occurrence
    /// (not content) addressing keeps two runs that share an objective from
    /// colliding in the reducer's id-keyed live block list.
    codex_goal_seq: u64,
    /// Cursor tool calls announced with the identity-less "MCP: tool" title.
    /// Only these are eligible for the completion-time result sniff
    /// (`cursor_companion_title_from_content`) — a `shell`/`read` call whose
    /// OUTPUT merely echoes a delegation ack must never be re-titled. Entries
    /// are dropped once the call reaches a terminal status (the sniff, if any,
    /// has recorded its override by then), so the set tracks only in-flight
    /// calls.
    cursor_generic_mcp_ids: HashSet<String>,
    /// Grok tool_call ids whose interactive question already renders via the
    /// `_x.ai/ask_user_question` ext bridge (`handle_grok_ask_user_question`). The
    /// redundant native `tool_call` / `tool_call_update` stream for these is
    /// dropped so the card doesn't double-render; tracked by id because a later
    /// status-only update may drop the `x.ai/tool` meta that first identified it.
    grok_ask_tool_ids: HashSet<String>,
}

/// True when a tool call's ACP `_meta` marks it as grok's native
/// `ask_user_question` (`x.ai/tool.kind == "ask_user"`). Codeg answers grok's
/// blocking `_x.ai/ask_user_question` ext request by rendering the interactive
/// `AskQuestionCard` (see `handle_grok_ask_user_question`), so the parallel
/// `tool_call` stream grok emits for the same call is redundant — it is dropped
/// live so the question doesn't render twice (once answerable, once inert).
fn grok_meta_marks_ask_user(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> bool {
    matches!(agent_type, AgentType::Grok)
        && meta
            .and_then(|m| m.get("x.ai/tool"))
            .and_then(|t| t.get("kind"))
            .and_then(|k| k.as_str())
            == Some("ask_user")
}

fn suppress_grok_ask_tool_frame(
    agent_type: AgentType,
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
    tool_call_id: &str,
    status: Option<&str>,
    tracked_ids: &mut HashSet<String>,
) -> bool {
    if grok_meta_marks_ask_user(agent_type, meta) {
        tracked_ids.insert(tool_call_id.to_string());
    }
    let suppress = tracked_ids.contains(tool_call_id);
    if suppress && matches!(status, Some("completed") | Some("failed")) {
        tracked_ids.remove(tool_call_id);
    }
    suppress
}

/// Resolve a tool call's title, honoring an authoritative rewrite recorded for
/// the session in `overrides` (tool_call_id → resolved title).
///
/// Returns `Some(name)` when this event identifies a CodeBuddy `DeferExecuteTool`
/// (the inner `mcp__…` name, from `raw_input`) or a sub-agent invocation
/// (`"agent"`) — recording it — OR when a PRIOR event already classified this
/// `tool_call_id` and this event lost the marker (the override is re-asserted).
/// Returns `None` only when the call was never classified, so the caller falls
/// back to the event's own title.
///
/// Sub-agent detection fires on EITHER `raw_input.subagent_type`
/// (`is_subagent_invocation`) OR `meta_marks_subagent` — the precomputed
/// `codebuddy_meta_marks_subagent` result. The meta signal is what makes the pill
/// stable: CodeBuddy carries `codebuddy.ai/toolName == "Agent"` from the very
/// first frame, whereas `subagent_type` only reaches `raw_input` dozens of frames
/// later, so meta-first detection records the override immediately and every
/// later (sparse) frame re-asserts it.
///
/// The re-assertion is the fix for CodeBuddy's status-only `ToolCallUpdate`s:
/// they arrive without the original `subagent_type`/`toolName` payload but WITH
/// the agent's raw (non-agent) title. Without it the frontend
/// (`inferLiveToolName` → `getToolName`) downgrades the Agent / delegation card
/// back to a generic tool call mid-stream — which also un-nests its children.
/// `on_update` only tunes the (PII-safe, id-only) trace wording.
fn resolve_rewritten_title(
    agent_type: AgentType,
    raw_input: &Option<String>,
    tool_call_id: &str,
    on_update: bool,
    meta_marks_subagent: bool,
    overrides: &mut HashMap<String, String>,
) -> Option<String> {
    if let Some(inner) = codebuddy_deferred_tool_name(agent_type, raw_input) {
        tracing::info!(
            "[ACP][{agent_type}] unwrapped DeferExecuteTool to its real MCP tool (tool_call_id={tool_call_id}, on_update={on_update})"
        );
        overrides.insert(tool_call_id.to_string(), inner.clone());
        return Some(inner);
    }
    if is_subagent_invocation(agent_type, raw_input) || meta_marks_subagent {
        tracing::info!(
            "[ACP][{agent_type}] subagent detected, rewrote tool title to 'agent' (tool_call_id={tool_call_id}, on_update={on_update})"
        );
        overrides.insert(tool_call_id.to_string(), "agent".to_string());
        return Some("agent".to_string());
    }
    overrides.get(tool_call_id).cloned()
}

fn map_plan_priority(priority: &PlanEntryPriority) -> String {
    match priority {
        PlanEntryPriority::High => "high",
        PlanEntryPriority::Medium => "medium",
        PlanEntryPriority::Low => "low",
        _ => "unknown",
    }
    .to_string()
}

fn map_plan_status(status: &PlanEntryStatus) -> String {
    match status {
        PlanEntryStatus::Pending => "pending",
        PlanEntryStatus::InProgress => "in_progress",
        PlanEntryStatus::Completed => "completed",
        _ => "unknown",
    }
    .to_string()
}

fn map_plan_entries(plan: &Plan) -> Vec<PlanEntryInfo> {
    plan.entries
        .iter()
        .map(|entry| PlanEntryInfo {
            content: entry.content.clone(),
            priority: map_plan_priority(&entry.priority),
            status: map_plan_status(&entry.status),
        })
        .collect()
}

fn parse_claude_sdk_message_params(
    params: &serde_json::Value,
) -> Option<(String, serde_json::Value)> {
    let obj = params.as_object()?;
    let session_id = obj.get("sessionId")?.as_str()?.to_string();
    let message = obj.get("message")?.clone();
    Some((session_id, message))
}

fn is_claude_api_retry_message(message: &serde_json::Value) -> bool {
    let obj = match message.as_object() {
        Some(obj) => obj,
        None => return false,
    };
    let message_type = obj.get("type").and_then(|v| v.as_str());
    let message_subtype = obj.get("subtype").and_then(|v| v.as_str());
    matches!(message_type, Some("system")) && matches!(message_subtype, Some("api_retry"))
}

fn map_claude_sdk_ext_notification(notification: &UntypedMessage) -> Option<AcpEvent> {
    if notification.method() != "_claude/sdkMessage" {
        return None;
    }

    let (session_id, message) = parse_claude_sdk_message_params(notification.params())?;
    if !is_claude_api_retry_message(&message) {
        return None;
    }
    Some(AcpEvent::ClaudeSdkMessage {
        session_id,
        message,
    })
}

/// Shared empty-rewrite used by all three turn-completion sites.
fn rewrite_end_turn_if_empty(raw_reason: &str, turn_had_agent_output: bool) -> &str {
    if raw_reason == "end_turn" && !turn_had_agent_output {
        "empty"
    } else {
        raw_reason
    }
}

fn resolve_context_window_size(state: &SessionState) -> u64 {
    use crate::acp::xai_session_notification::resolve_context_window_size_from_parts;
    let existing = state.usage.as_ref().map(|u| u.size);
    let model = current_grok_model_id_from_opts(state.config_options.as_deref().unwrap_or(&[]));
    let configured = crate::parsers::grok::read_grok_model_context_window(model.as_deref());
    resolve_context_window_size_from_parts(existing, model.as_deref(), configured)
}

/// Grok never emits standard ACP `usage_update`; it stamps cumulative
/// `params._meta.totalTokens` on ordinary session notifications instead.
/// Promote that into `AcpEvent::UsageUpdate` so the composer context ring
/// updates while the turn is still in flight (history re-derives from the
/// same field via the Grok parser).
async fn maybe_emit_grok_total_tokens_usage(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    meta: Option<&Meta>,
) {
    if agent_type != AgentType::Grok {
        return;
    }
    let Some(meta) = meta else {
        return;
    };
    let Some(used) = meta
        .get("totalTokens")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().filter(|&i| i >= 0).map(|i| i as u64))
        })
        .filter(|&u| u > 0)
    else {
        return;
    };
    let size = {
        let st = state.read().await;
        resolve_context_window_size(&st)
    };
    {
        let st = state.read().await;
        if let Some(u) = &st.usage {
            if u.used == used && u.size == size {
                return;
            }
        }
    }
    emit_with_state(state, emitter, AcpEvent::UsageUpdate { used, size }).await;
}

async fn map_and_emit_xai_session_notification(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    notification: &UntypedMessage,
    mode: crate::acp::xai_session_notification::PrivateExtEmitMode,
    compact_text_emitted_this_turn: &mut bool,
) -> bool {
    use crate::acp::xai_session_notification::{
        map_xai_session_notification, with_lifecycle_separator, PrivateExtEmitMode,
        XaiSessionAction,
    };

    if matches!(mode, PrivateExtEmitMode::LoadDrainNoop) {
        return false;
    }
    let Some(actions) = map_xai_session_notification(notification) else {
        return false;
    };
    let allow_text = matches!(mode, PrivateExtEmitMode::InPrompt);
    let mut emitted_text = false;
    for action in actions {
        match action {
            XaiSessionAction::AgentText(text) if allow_text => {
                let text = with_lifecycle_separator(text, *compact_text_emitted_this_turn);
                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::ContentDelta {
                        text,
                        parent_tool_use_id: None,
                    },
                )
                .await;
                *compact_text_emitted_this_turn = true;
                emitted_text = true;
            }
            XaiSessionAction::AgentText(_) => {}
            XaiSessionAction::Usage { used } if used > 0 => {
                let size = {
                    let st = state.read().await;
                    resolve_context_window_size(&st)
                };
                emit_with_state(state, emitter, AcpEvent::UsageUpdate { used, size }).await;
            }
            XaiSessionAction::Usage { .. } => {}
        }
    }
    if emitted_text {
        tracing::debug!("mapped x.ai private compact notification with ContentDelta");
    }
    emitted_text
}

/// Single otherwise entry for private extensions (Claude SDK + Grok compact).
/// Returns `true` iff at least one ContentDelta was emitted (agent-output equivalent).
async fn maybe_emit_private_ext_notification(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    dispatch: Dispatch,
    mode: crate::acp::xai_session_notification::PrivateExtEmitMode,
    compact_text_emitted_this_turn: &mut bool,
) -> bool {
    let Dispatch::Notification(notification) = dispatch else {
        return false;
    };

    if let Some(event) = map_claude_sdk_ext_notification(&notification) {
        // api_retry → ClaudeSdkMessage does not advance transcript/tool state.
        emit_with_state(state, emitter, event).await;
        return false;
    }
    map_and_emit_xai_session_notification(
        state,
        emitter,
        &notification,
        mode,
        compact_text_emitted_this_turn,
    )
    .await
}

/// The JSON-RPC methods grok uses for its private, namespaced session updates.
/// Both share the standard `session/update` envelope (`params.update.
/// sessionUpdate` + fields, verified live against grok 0.2.111) but carry
/// variants the typed ACP pipeline can't deserialize, so codeg drops them.
const GROK_EXT_UPDATE_METHODS: [&str; 2] = ["_x.ai/session_notification", "_x.ai/session/update"];

/// A stable id for a synthetic event derived from a grok ext notification —
/// grok stamps `params._meta.eventId`; fall back to a fresh uuid.
fn grok_ext_event_id(params: &serde_json::Value) -> String {
    params
        .get("_meta")
        .and_then(|m| m.get("eventId"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| format!("grok-ext-{}", uuid::Uuid::new_v4().simple()))
}

/// Map grok's private context-compaction ext notifications into `AcpEvent`s.
///
/// grok reports `/compact` (and auto-compaction) results on
/// `_x.ai/session_notification` / `_x.ai/session/update` rather than as normal
/// `agent_message_chunk`s. Those methods never match the typed `session/update`
/// pipeline, so without this the whole turn is blank and `/compact` looks like
/// it failed. Only grok emits these, so gate on the agent. Turn-level failures
/// are intentionally NOT handled here — the `session/prompt` response path
/// (`turn_failure_error_event`) already surfaces those, and duplicating them
/// would double-report.
fn map_grok_ext_notification(
    notification: &UntypedMessage,
    agent_type: AgentType,
) -> Option<AcpEvent> {
    if !matches!(agent_type, AgentType::Grok) {
        return None;
    }
    if !GROK_EXT_UPDATE_METHODS.contains(&notification.method()) {
        return None;
    }
    let params = notification.params();
    let update = params.get("update")?;
    match update.get("sessionUpdate").and_then(|v| v.as_str())? {
        // grok always emits `completed` (even a no-op where before == after).
        // Render the shared context-compaction card (recognized frontend-side by
        // `_meta.contextCompaction`, same as codex) carrying the token delta.
        "auto_compact_completed" => {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "contextCompaction".to_string(),
                serde_json::Value::Bool(true),
            );
            if let Some(before) = update.get("tokens_before").and_then(|v| v.as_u64()) {
                meta.insert("tokensBefore".to_string(), before.into());
            }
            if let Some(after) = update.get("tokens_after").and_then(|v| v.as_u64()) {
                meta.insert("tokensAfter".to_string(), after.into());
            }
            Some(AcpEvent::ToolCall {
                tool_call_id: grok_ext_event_id(params),
                title: "Context compaction".to_string(),
                kind: "other".to_string(),
                status: "completed".to_string(),
                content: None,
                raw_input: None,
                raw_output: None,
                locations: None,
                meta: Some(serde_json::Value::Object(meta)),
                images: None,
            })
        }
        // Compaction itself blew up (e.g. the summarizer model call failed) while
        // the turn still ended cleanly — surface a non-terminal error so the
        // result isn't a silent blank.
        "auto_compact_failed" => Some(AcpEvent::Error {
            message: format!(
                "Context compaction failed{}",
                update
                    .get("reason")
                    .or_else(|| update.get("message"))
                    .and_then(|v| v.as_str())
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default()
            ),
            agent_type: agent_type.to_string(),
            code: None,
            terminal: false,
        }),
        _ => None,
    }
}

/// Whether a dispatch is a grok ext notification that
/// `map_grok_ext_notification` renders as visible turn output (a compaction card
/// or a compaction error). The active-turn loop consults this BEFORE the typed
/// pipeline to mark the turn as non-empty: a `/compact` turn emits only these
/// ext notifications and no standard `agent_message_chunk`, so without this its
/// `end_turn` is reclassified as `"empty"` and re-surfaced as a spurious error —
/// the exact symptom this change removes. Reuses `map_grok_ext_notification` so
/// the handled-variant set can never drift from what actually emits.
fn grok_ext_notification_is_turn_output(dispatch: &Dispatch, agent_type: AgentType) -> bool {
    match dispatch {
        Dispatch::Notification(notification) => {
            map_grok_ext_notification(notification, agent_type).is_some()
        }
        _ => false,
    }
}

async fn maybe_emit_ext_notification(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    dispatch: Dispatch,
) {
    let Dispatch::Notification(notification) = dispatch else {
        return;
    };

    if let Some(event) = map_claude_sdk_ext_notification(&notification)
        .or_else(|| map_grok_ext_notification(&notification, agent_type))
    {
        emit_with_state(state, emitter, event).await;
    }
}

/// Real-time extension mapper. Grok compaction completion/failure uses the
/// upstream card/error projection; the fork mapper still supplies usage and
/// the started/cancelled lifecycle text. A notification emits one visible
/// projection, never both a card and a text delta.
async fn maybe_emit_live_ext_notification(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    dispatch: Dispatch,
    mode: crate::acp::xai_session_notification::PrivateExtEmitMode,
    compact_text_emitted_this_turn: &mut bool,
) -> bool {
    let generic_event = match &dispatch {
        Dispatch::Notification(notification) => map_claude_sdk_ext_notification(notification)
            .map(|event| (event, false))
            .or_else(|| {
                map_grok_ext_notification(notification, agent_type).map(|event| (event, true))
            }),
        _ => None,
    };

    if let Some((event, is_grok_output)) = generic_event {
        emit_with_state(state, emitter, event).await;
        if is_grok_output {
            if let Dispatch::Notification(notification) = &dispatch {
                let _ = map_and_emit_xai_session_notification(
                    state,
                    emitter,
                    notification,
                    crate::acp::xai_session_notification::PrivateExtEmitMode::IdleUsageOnly,
                    compact_text_emitted_this_turn,
                )
                .await;
            }
            *compact_text_emitted_this_turn = true;
        }
        return is_grok_output;
    }

    maybe_emit_private_ext_notification(
        state,
        emitter,
        dispatch,
        mode,
        compact_text_emitted_this_turn,
    )
    .await
}

/// Test seam for pre-finalization drain: live zero-timeout reads vs fake queue.
enum ReadyUpdateSource<'borrow, 'responder> {
    Live(&'borrow mut sacp::ActiveSession<'responder, Agent>),
    /// Used by unit tests that inject preloaded session messages.
    #[cfg(test)]
    #[allow(dead_code)]
    Fake(&'borrow mut std::collections::VecDeque<SessionMessage>),
}

impl<'borrow, 'responder> ReadyUpdateSource<'borrow, 'responder> {
    async fn try_next_ready(&mut self) -> Option<Result<SessionMessage, sacp::Error>> {
        match self {
            ReadyUpdateSource::Live(session) => {
                tokio::time::timeout(std::time::Duration::ZERO, session.read_update())
                    .await
                    .ok()
            }
            #[cfg(test)]
            ReadyUpdateSource::Fake(q) => q.pop_front().map(Ok),
        }
    }
}

async fn reconcile_grok_retry_dispatch(
    agent_type: AgentType,
    notification: &UntypedMessage,
    reconciler: &mut GrokRetryReconciler,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    turn_had_agent_output: &mut bool,
) -> bool {
    if agent_type != AgentType::Grok {
        return false;
    }

    match reconciler.observe(notification) {
        GrokRetryAction::Pass => false,
        GrokRetryAction::Consume => true,
        GrokRetryAction::Rollback { attempt } => {
            emit_with_state(state, emitter, AcpEvent::TurnAttemptRollback { attempt }).await;
            *turn_had_agent_output = state.read().await.has_live_agent_output();
            tracing::debug!(attempt, "rolled back speculative Grok retry output");
            true
        }
        GrokRetryAction::DropStale { update_kind } => {
            tracing::debug!(update_kind, "dropping stale Grok retry update");
            true
        }
    }
}

/// Drain already-ready session updates before empty-rewrite finalization.
/// Never finalizes a turn; secondary terminals are suppressed (primary T wins).
#[allow(clippy::too_many_arguments)]
async fn drain_ready_in_prompt_updates(
    source: &mut ReadyUpdateSource<'_, '_>,
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    sid: &SessionId,
    cwd: &str,
    terminal_runtime: &Arc<TerminalRuntime>,
    terminal_assoc: &Arc<std::sync::Mutex<TerminalAssocFallback>>,
    tracked_terminal_tool_calls: &mut HashMap<String, TrackedTerminalToolCall>,
    raw_output_cache: &mut ToolCallOutputCache,
    cb_state: &mut CodeBuddyLiveState,
    grok_retry_reconciler: &mut GrokRetryReconciler,
    turn_had_agent_output: &mut bool,
    compact_text_emitted_this_turn: &mut bool,
) {
    let cwd_opt = Some(cwd);
    let mut drained = 0u32;
    loop {
        if drained >= 64 {
            tracing::warn!("[ACP] pre-finalize drain hit 64-message cap");
            break;
        }
        let Some(msg_res) = source.try_next_ready().await else {
            break;
        };
        drained += 1;
        let Ok(msg) = msg_res else {
            tracing::warn!("[ACP] session update error during pre-finalize drain; stopping drain");
            break;
        };
        match msg {
            SessionMessage::StopReason(_) => {
                tracing::debug!(
                    secondary_terminal_suppressed = true,
                    kind = "stop_reason",
                    "pre-finalize drain"
                );
                continue;
            }
            SessionMessage::SessionMessage(dispatch) => {
                let dispatch = fix_usage_update_nulls(dispatch);
                if let Dispatch::Notification(notification) = &dispatch {
                    if reconcile_grok_retry_dispatch(
                        agent_type,
                        notification,
                        grok_retry_reconciler,
                        state,
                        emitter,
                        turn_had_agent_output,
                    )
                    .await
                    {
                        continue;
                    }
                }
                if parse_extension_turn_completed(&dispatch).is_some() {
                    tracing::debug!(
                        secondary_terminal_suppressed = true,
                        kind = "extension_turn_completed",
                        "pre-finalize drain"
                    );
                    continue;
                }
                if grok_ext_notification_is_turn_output(&dispatch, agent_type) {
                    *turn_had_agent_output = true;
                }
                let h = emitter.clone();
                let st = Arc::clone(state);
                let runtime = terminal_runtime.clone();
                let session_id = sid.clone();
                if let Err(e) = MatchDispatch::new(dispatch)
                    .if_notification(async |notif: SessionNotification| {
                        observe_terminal_assoc_from_update(
                            &notif.update,
                            session_id.0.as_ref(),
                            terminal_assoc.as_ref(),
                        );
                        let should_poll_now =
                            track_terminal_tool_calls(&notif.update, tracked_terminal_tool_calls);
                        let bound = merge_terminal_assoc_binds(
                            session_id.0.as_ref(),
                            terminal_assoc.as_ref(),
                            tracked_terminal_tool_calls,
                        );
                        // I2: sync accumulated association before frontend emit.
                        if should_poll_now || !bound.is_empty() {
                            tool_watchdog_sync_tracked_terminals(&st, tracked_terminal_tool_calls)
                                .await;
                        }
                        if is_agent_output_update(&notif.update) {
                            *turn_had_agent_output = true;
                        }
                        record_transcript_update(agent_type, &session_id.0, &notif.update);
                        mark_agent_activity_for_update(&st, &notif.update, chrono::Utc::now())
                            .await;
                        maybe_emit_grok_total_tokens_usage(
                            &st,
                            &h,
                            agent_type,
                            notif.meta.as_ref(),
                        )
                        .await;
                        emit_conversation_update(
                            &st,
                            &h,
                            agent_type,
                            notif.update,
                            cwd_opt,
                            raw_output_cache,
                            cb_state,
                            Some(tracked_terminal_tool_calls),
                        )
                        .await;
                        if should_poll_now || !bound.is_empty() {
                            poll_tracked_terminal_tool_calls(
                                runtime.as_ref(),
                                &session_id,
                                &st,
                                &h,
                                tracked_terminal_tool_calls,
                            )
                            .await;
                        }
                        Ok(())
                    })
                    .await
                    .otherwise(async |dispatch| {
                        if maybe_emit_live_ext_notification(
                            &st,
                            &h,
                            agent_type,
                            dispatch,
                            crate::acp::xai_session_notification::PrivateExtEmitMode::InPrompt,
                            compact_text_emitted_this_turn,
                        )
                        .await
                        {
                            *turn_had_agent_output = true;
                            st.write().await.mark_agent_activity(chrono::Utc::now());
                            tracing::debug!(
                                drain_hit_private_compact = true,
                                "pre-finalize drain mapped ContentDelta"
                            );
                        }
                        Ok(())
                    })
                    .await
                {
                    tracing::warn!(
                        "[ACP] Ignoring dispatch parse error during pre-finalize drain: {e}"
                    );
                }
            }
            _ => {}
        }
    }
}

/// Grok (and potentially other hosts) emit turn completion as an extension
/// session notification rather than only via the `session/prompt` response:
///
/// ```text
/// method: "_x.ai/session/update" | "session/update"
/// update.sessionUpdate: "turn_completed"
/// update.stop_reason: "end_turn" | ...
/// ```
///
/// Historically the live ACP loop ignored this and waited only on
/// `prompt_response` / [`SessionMessage::StopReason`]. When the RPC response
/// stalls after the agent has already finished, the conversation row stays
/// `in_progress` forever even though the full answer streamed. Treat the
/// extension notification as a first-class turn-completion signal.
fn parse_extension_turn_completed(dispatch: &Dispatch) -> Option<String> {
    let Dispatch::Notification(notification) = dispatch else {
        return None;
    };
    parse_extension_turn_completed_notification(notification)
}

fn parse_extension_turn_completed_notification(notification: &UntypedMessage) -> Option<String> {
    let method = notification.method();
    if method != "_x.ai/session/update" && method != "session/update" {
        return None;
    }
    let update = notification.params().get("update")?;
    if update.get("sessionUpdate").and_then(|v| v.as_str()) != Some("turn_completed") {
        return None;
    }
    let raw = update
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("end_turn");
    Some(normalize_extension_stop_reason(raw))
}

/// Normalize host-provided stop_reason strings to the stable lowercase set
/// consumed by lifecycle (`end_turn` / `cancelled` / …).
fn normalize_extension_stop_reason(raw: &str) -> String {
    match raw {
        "end_turn" | "cancelled" | "refusal" | "max_tokens" | "max_turn_requests" | "empty"
        | "unknown" => raw.to_string(),
        "EndTurn" => "end_turn".into(),
        "Cancelled" | "canceled" | "Canceled" => "cancelled".into(),
        "Refusal" => "refusal".into(),
        "MaxTokens" => "max_tokens".into(),
        "MaxTurnRequests" => "max_turn_requests".into(),
        other => {
            tracing::warn!(
                stop_reason = %other,
                "[ACP] unknown extension turn_completed stop_reason; treating as end_turn"
            );
            "end_turn".into()
        }
    }
}
/// Fix null fields in `usage_update` notifications that would otherwise fail deserialization.
///
/// Some ACP agents send `"used": null` in usage_update notifications, but the
/// upstream schema expects `u64`. This function patches the raw JSON params
/// so that `null` numeric fields default to `0`.
fn fix_usage_update_nulls(mut dispatch: Dispatch) -> Dispatch {
    if let Dispatch::Notification(ref mut msg) = dispatch {
        if let Some(update) = msg.params.get_mut("update") {
            if update.get("sessionUpdate").and_then(|v| v.as_str()) == Some("usage_update") {
                if update.get("used").map(|v| v.is_null()).unwrap_or(false) {
                    update["used"] = serde_json::Value::from(0u64);
                }
                if update.get("size").map(|v| v.is_null()).unwrap_or(false) {
                    update["size"] = serde_json::Value::from(0u64);
                }
            }
        }
    }
    dispatch
}

/// Convert a SessionUpdate into AcpEvent(s) and emit to frontend.
///
/// `raw_output_cache` is a per-session cache used to detect cumulative
/// snapshots from agents and convert them into incremental deltas so the
/// event pipeline never carries a full N-MB tool output more than once.
///
/// `cb_state` is the per-session `CodeBuddyLiveState`: the authoritative
/// title-rewrite map (so a status-only update can't downgrade an Agent /
/// delegation card and un-nest its children) plus the open-sub-agent window used
/// to suppress a CodeBuddy sub-agent's interleaved thought/message chunks.
/// Mirrors `raw_output_cache`'s lifetime.
///
/// `tracked_terminals` is the live accumulated terminal association map. When
/// present, capability is re-derived from it immediately after tool lease
/// register/progress and **before** frontend emission (Task 5 r3 I2).
#[allow(clippy::too_many_arguments)] // tracked map required for pre-emit capability sync
async fn emit_conversation_update(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    update: SessionUpdate,
    cwd: Option<&str>,
    raw_output_cache: &mut ToolCallOutputCache,
    cb_state: &mut CodeBuddyLiveState,
    tracked_terminals: Option<&HashMap<String, TrackedTerminalToolCall>>,
) {
    match update {
        SessionUpdate::UserMessageChunk(_) => {
            // User echo chunks are informational for transcript sync and
            // currently not rendered in live ACP UI.
        }
        SessionUpdate::AgentMessageChunk(ContentChunk {
            content: ContentBlock::Text(text),
            meta,
            ..
        }) => {
            // Drop a CodeBuddy sub-agent's interleaved message text — it belongs
            // to the Agent pill, not the main thread (see
            // `should_suppress_subagent_chunk`). No-op for every other agent.
            if !should_suppress_subagent_chunk(
                agent_type,
                !cb_state.open_subagents.is_empty(),
                meta.as_ref(),
            ) {
                tool_watchdog_record_agent_activity(state, emitter, &text.text).await;
                // Claude subagent chunks (claude-agent-acp ≥0.63 with the
                // `subagent-transcript` capability) are NOT suppressed: they
                // emit with their parent id so the frontend can route them
                // into the live Agent capsule.
                let parent_tool_use_id = claude_chunk_parent_tool_use_id(agent_type, meta.as_ref());
                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::ContentDelta {
                        text: text.text,
                        parent_tool_use_id,
                    },
                )
                .await;
            }
        }
        SessionUpdate::AgentMessageChunk(_) => {
            // Non-text chunks are currently not surfaced in live streaming UI.
        }
        SessionUpdate::AgentThoughtChunk(ContentChunk {
            content: ContentBlock::Text(text),
            meta,
            ..
        }) => {
            // Same suppression for a sub-agent's interleaved reasoning.
            if !should_suppress_subagent_chunk(
                agent_type,
                !cb_state.open_subagents.is_empty(),
                meta.as_ref(),
            ) {
                tool_watchdog_record_agent_activity(state, emitter, &text.text).await;
                let parent_tool_use_id = claude_chunk_parent_tool_use_id(agent_type, meta.as_ref());
                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::Thinking {
                        text: text.text,
                        parent_tool_use_id,
                    },
                )
                .await;
            }
        }
        SessionUpdate::AgentThoughtChunk(_) => {
            // Non-text thought chunks are currently ignored.
        }
        SessionUpdate::ToolCall(tc) => {
            // codex-acp #304 (v1.1.3+) surfaces codex `subAgentActivity` as a
            // live `tool_call`; suppress it — it is redundant with the collab
            // capsule and the history reconstruction (see
            // `is_codex_subagent_activity`).
            if is_codex_subagent_activity(agent_type, tc.meta.as_ref()) {
                return;
            }
            let tool_call_id = tc.tool_call_id.to_string();
            let status = format!("{:?}", tc.status).to_lowercase();
            // Grok emits a redundant `tool_call` for its native ask_user_question
            // alongside the blocking `_x.ai/ask_user_question` ext request codeg
            // answers with the interactive card; drop it here (remembering the id so
            // later status-only updates that lost the meta are dropped too).
            if suppress_grok_ask_tool_frame(
                agent_type,
                tc.meta.as_ref(),
                &tool_call_id,
                Some(status.as_str()),
                &mut cb_state.grok_ask_tool_ids,
            ) {
                return;
            }
            // CodeBuddy double-wraps a deferred MCP result as a `{type,text}`
            // content part; peel it (in both the content and raw_output channels)
            // so the dedicated delegation cards parse it instead of showing raw JSON.
            // codex-acp reports file edits as a `Diff` content block with no
            // `raw_input`; synthesize a canonical edit so the call classifies/
            // renders as an edit instead of a tool named after the raw diff
            // header (see synthesize_edit_input_from_diffs). When we do, drop the
            // `Diff` from `content` — it's the same edit re-serialized hunklessly,
            // which would otherwise double the event and skew the header +/- stats.
            // Blank raw_input is treated as absent (matches the frontend guard).
            // Grok wraps every MCP call in a `use_tool` envelope; peel it so the
            // call is correlated/classified/parsed as a direct MCP call — its
            // real `tool_input` becomes `raw_input`, its `tool_name` the title
            // below (see unwrap_grok_use_tool).
            let grok_use_tool = if matches!(agent_type, AgentType::Grok) {
                unwrap_grok_use_tool(tc.raw_input.as_ref())
            } else {
                None
            };
            let own_raw_input = match &grok_use_tool {
                Some((_, inner)) => {
                    json_value_to_text(&Some(inner.clone())).filter(|t| !t.trim().is_empty())
                }
                None => json_value_to_text(&tc.raw_input).filter(|t| !t.trim().is_empty()),
            };
            let synthesized_edit = if own_raw_input.is_none() {
                synthesize_edit_input_from_diffs(&tc.content)
            } else {
                None
            };
            let content = serialize_tool_call_content(&tc.content, synthesized_edit.is_none())
                .map(|c| unwrap_codebuddy_deferred_output(agent_type, &c).unwrap_or(c));
            let images = extract_tool_call_images(&tc.content);
            let raw_input = synthesized_edit
                .or(own_raw_input)
                .map(|text| resolve_live_tool_input(&text, cwd));
            // Initial tool_call notification — the frontend reducer
            // treats `raw_output` as a full replacement, so we bypass
            // the diff path and seed the cache with the current snapshot.
            let raw_output_text = if matches!(agent_type, AgentType::Grok) {
                // Grok's structured rawOutput would shadow `content` and render
                // empty; take the parity path (see grok_live_tool_output).
                grok_live_tool_output(&content, &tc.raw_output)
            } else {
                json_value_to_text(&tc.raw_output)
                    .map(|text| unwrap_codebuddy_deferred_output(agent_type, &text).unwrap_or(text))
                    .map(|text| structurize_live_output(&text))
            };
            let raw_output =
                raw_output_text.and_then(|text| raw_output_cache.seed(&tool_call_id, &text));
            let locations = if tc.locations.is_empty() {
                None
            } else {
                serde_json::to_value(&tc.locations).ok()
            };
            // Read the CodeBuddy sub-agent markers from `_meta` BEFORE it's moved
            // into the emitted `Value` below — `meta_marks_subagent` is the early,
            // reliable signal (frame 1) that keeps the Agent pill from flickering;
            // `meta_marks_background` keeps a concurrent sub-agent out of the
            // suppression window (see fn docs).
            let meta_marks_subagent = codebuddy_meta_marks_subagent(agent_type, tc.meta.as_ref());
            let meta_marks_background =
                codebuddy_meta_marks_background(agent_type, tc.meta.as_ref());
            let meta = tc.meta.map(serde_json::Value::Object);
            raw_output_cache.remove_if_final(&tool_call_id, Some(status.as_str()));
            // Avoid logging titles/payloads below — they can be model-generated
            // user task descriptions (PII-adjacent) and would create noise in
            // server-mode log sinks. The opaque tool_call_id is enough to
            // correlate these events with downstream traces.
            // Record the peeled Grok MCP name as an authoritative title override
            // so later sparse `use_tool` updates (which carry the generic wrapper
            // title and no raw_input) re-assert it via resolve_rewritten_title
            // instead of reverting the delegation card to a generic tool. Mirrors
            // the CodeBuddy DeferExecuteTool / sub-agent title persistence.
            if let Some((name, _)) = &grok_use_tool {
                cb_state
                    .title_overrides
                    .insert(tool_call_id.clone(), name.clone());
            }
            // Resolve (and record) any authoritative title rewrite so a later
            // status-only update can't downgrade this card (see fn doc).
            let title = resolve_rewritten_title(
                agent_type,
                &raw_input,
                &tool_call_id,
                false,
                meta_marks_subagent,
                &mut cb_state.title_overrides,
            )
            .unwrap_or(tc.title);
            // Mark Cursor's identity-less MCP announcements as eligible for the
            // completion-time result sniff. Scoping the sniff to ids announced
            // with this exact title keeps a `shell`/`read` call whose OUTPUT
            // echoes a delegation ack from being re-titled.
            if matches!(agent_type, AgentType::Cursor)
                && title == crate::acp::lifecycle::CURSOR_IDENTITYLESS_MCP_TITLE
            {
                cb_state.cursor_generic_mcp_ids.insert(tool_call_id.clone());
            }
            // Open/close the sub-agent suppression window for this call. `title ==
            // "agent"` iff this is a classified native sub-agent (DeferExecuteTool
            // rewrites to an `mcp__…` name, never "agent").
            track_subagent_window(
                agent_type,
                title == "agent",
                meta_marks_background,
                Some(status.as_str()),
                &tool_call_id,
                &mut cb_state.open_subagents,
                &mut cb_state.closed_subagents,
            );
            let kind = format!("{:?}", tc.kind).to_lowercase();
            // Register / progress / complete leases, then sync accumulated
            // terminal capability, then frontend emission (no await gap where
            // association is multi while capability is still Terminal(A)).
            let settle_error = tool_watchdog_on_tool_event(
                state,
                emitter,
                &tool_call_id,
                &kind,
                Some(title.as_str()),
                Some(status.as_str()),
                meta_marks_background,
            )
            .await;
            tool_watchdog_sync_tool_from_tracked(state, &tool_call_id, tracked_terminals).await;
            // I2: cancel-owned settle must not leave a successful completion.
            let (status, raw_output) = if let Some((failed_status, code)) =
                crate::acp::tool_watchdog::rewrite_completed_status_if_watchdog_settled(
                    Some(status.as_str()),
                    settle_error.as_deref(),
                ) {
                (failed_status.to_string(), Some(code))
            } else {
                (status, raw_output)
            };
            emit_with_state(
                state,
                emitter,
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
                },
            )
            .await;
        }
        SessionUpdate::ToolCallUpdate(tcu) => {
            // Symmetric with the `ToolCall` arm: a follow-up update for a codex
            // `subAgentActivity` still carries `_meta.codex.subagent`, so drop
            // it too (see `is_codex_subagent_activity`).
            if is_codex_subagent_activity(agent_type, tcu.meta.as_ref()) {
                return;
            }
            let tool_call_id = tcu.tool_call_id.to_string();
            let status = tcu
                .fields
                .status
                .as_ref()
                .map(|s| format!("{s:?}").to_lowercase());
            // Suppress the redundant update stream for grok's ask_user_question
            // (see the ToolCall arm): match the tracked id, or the meta on a late
            // update that still carries it.
            if suppress_grok_ask_tool_frame(
                agent_type,
                tcu.meta.as_ref(),
                &tool_call_id,
                status.as_deref(),
                &mut cb_state.grok_ask_tool_ids,
            ) {
                return;
            }
            // Peel CodeBuddy's `{type,text}` deferred-MCP wrapper here too — the
            // result often arrives on an update (see raw_output below).
            // Same Diff→canonical-edit hoist as the initial ToolCall path: the
            // edit may first arrive on an update. Drop the redundant Diff from
            // `content` when hoisted. The reducer preserves a prior raw_input on
            // status-only updates (`action.raw_input ?? block.info.raw_input`).
            // Grok `use_tool` unwrap, symmetric with the ToolCall arm — a rare
            // update that re-sends the envelope is peeled the same way (most
            // updates carry no raw_input, so this resolves to None and the
            // reducer keeps the prior unwrapped input).
            let grok_use_tool = if matches!(agent_type, AgentType::Grok) {
                unwrap_grok_use_tool(tcu.fields.raw_input.as_ref())
            } else {
                None
            };
            let own_raw_input = match &grok_use_tool {
                Some((_, inner)) => {
                    json_value_to_text(&Some(inner.clone())).filter(|t| !t.trim().is_empty())
                }
                None => json_value_to_text(&tcu.fields.raw_input).filter(|t| !t.trim().is_empty()),
            };
            let synthesized_edit = if own_raw_input.is_none() {
                tcu.fields
                    .content
                    .as_deref()
                    .and_then(synthesize_edit_input_from_diffs)
            } else {
                None
            };
            let content = tcu
                .fields
                .content
                .as_deref()
                .and_then(|c| serialize_tool_call_content(c, synthesized_edit.is_none()))
                .map(|c| unwrap_codebuddy_deferred_output(agent_type, &c).unwrap_or(c));
            let images = tcu
                .fields
                .content
                .as_deref()
                .and_then(extract_tool_call_images);
            let raw_input = synthesized_edit
                .or(own_raw_input)
                .map(|text| resolve_live_tool_input(&text, cwd));
            // Diff the incoming raw_output against the last snapshot we
            // emitted for this tool call. This turns cumulative snapshots
            // from agents (Claude Code, Codex, …) into incremental deltas
            // with `raw_output_append=true`, collapsing the O(N²) transfer
            // problem to O(N) while capping any single emitted chunk to
            // MAX_SINGLE_EMIT_BYTES.
            let raw_output_text = if matches!(agent_type, AgentType::Grok) {
                // Grok's structured rawOutput would shadow `content` and render
                // empty; take the parity path (see grok_live_tool_output).
                grok_live_tool_output(&content, &tcu.fields.raw_output)
            } else {
                json_value_to_text(&tcu.fields.raw_output)
                    .map(|text| unwrap_codebuddy_deferred_output(agent_type, &text).unwrap_or(text))
                    .map(|text| structurize_live_output(&text))
            };
            let (raw_output, raw_output_append) = match raw_output_text {
                Some(text) => match raw_output_cache.consume(&tool_call_id, &text) {
                    Some((payload, append)) => (Some(payload), Some(append)),
                    None => (None, None),
                },
                None => (None, None),
            };
            let locations = tcu
                .fields
                .locations
                .as_ref()
                .filter(|l| !l.is_empty())
                .and_then(|l| serde_json::to_value(l).ok());
            let meta_marks_subagent = codebuddy_meta_marks_subagent(agent_type, tcu.meta.as_ref());
            let meta_marks_background =
                codebuddy_meta_marks_background(agent_type, tcu.meta.as_ref());
            let meta = tcu.meta.clone().map(serde_json::Value::Object);
            raw_output_cache.remove_if_final(&tool_call_id, status.as_deref());
            // Re-assert any authoritative title rewrite (see fn doc): an update
            // that carries the subagent/deferred marker classifies (and records)
            // the card, and — the key fix — a later status-only update that LOST
            // the marker but carries the agent's raw (non-agent) title still
            // resolves to the recorded override, so the Agent/delegation card and
            // its child nesting (`getToolName === "agent"`) don't revert to a
            // generic tool call mid-stream. Falls back to the event's own title
            // for never-classified tool calls.
            // Symmetric with the ToolCall arm: a (rare) update that re-sends the
            // envelope records the peeled name so it survives later sparse updates.
            if let Some((name, _)) = &grok_use_tool {
                cb_state
                    .title_overrides
                    .insert(tool_call_id.clone(), name.clone());
            }
            // Cursor loses MCP tool identity on the wire entirely (announced as
            // "MCP: tool" before McpArgs exists; updates never resend title or
            // raw_input). The completion update's result text is the one signal
            // left — recover the codeg-mcp companion identity from it and record
            // it as an authoritative override so the delegation / status cards
            // resolve instead of a generic tool. Gated to ids this connection
            // announced with the identity-less title (see the
            // `cursor_generic_mcp_ids` field doc); the entry is dropped once
            // the call goes terminal.
            if matches!(agent_type, AgentType::Cursor)
                && cb_state.cursor_generic_mcp_ids.contains(&tool_call_id)
            {
                if let Some(name) = cursor_companion_title_from_content(content.as_deref()) {
                    cb_state
                        .title_overrides
                        .insert(tool_call_id.clone(), name.to_string());
                }
                if matches!(status.as_deref(), Some("completed") | Some("failed")) {
                    cb_state.cursor_generic_mcp_ids.remove(&tool_call_id);
                }
            }
            let title = resolve_rewritten_title(
                agent_type,
                &raw_input,
                &tool_call_id,
                true,
                meta_marks_subagent,
                &mut cb_state.title_overrides,
            )
            .or(tcu.fields.title);
            // Keep/close the sub-agent suppression window by status (an update
            // resolving to "agent" is a classified native sub-agent).
            track_subagent_window(
                agent_type,
                title.as_deref() == Some("agent"),
                meta_marks_background,
                status.as_deref(),
                &tool_call_id,
                &mut cb_state.open_subagents,
                &mut cb_state.closed_subagents,
            );
            let kind = tcu
                .fields
                .kind
                .map(|k| format!("{k:?}").to_lowercase())
                .unwrap_or_default();
            let settle_error = tool_watchdog_on_tool_event(
                state,
                emitter,
                &tool_call_id,
                &kind,
                title.as_deref(),
                status.as_deref(),
                meta_marks_background,
            )
            .await;
            // Sync accumulated association after lease admission, before
            // frontend await (Task 5 r3 I2).
            tool_watchdog_sync_tool_from_tracked(state, &tool_call_id, tracked_terminals).await;
            // I2 late-final race: claim settled TimedOut then provider still
            // emits completed — rewrite so SessionState / transcript get failed.
            let (status, raw_output) = if let Some((failed_status, code)) =
                crate::acp::tool_watchdog::rewrite_completed_status_if_watchdog_settled(
                    status.as_deref(),
                    settle_error.as_deref(),
                ) {
                (Some(failed_status.to_string()), Some(code))
            } else {
                (status, raw_output)
            };
            emit_with_state(
                state,
                emitter,
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
                },
            )
            .await;
        }
        SessionUpdate::CurrentModeUpdate(update) => {
            emit_with_state(
                state,
                emitter,
                AcpEvent::ModeChanged {
                    mode_id: update.current_mode_id.to_string(),
                },
            )
            .await;
        }
        SessionUpdate::Plan(plan) => {
            emit_with_state(
                state,
                emitter,
                AcpEvent::PlanUpdate {
                    entries: map_plan_entries(&plan),
                },
            )
            .await;
        }
        SessionUpdate::ConfigOptionUpdate(update) => {
            emit_session_config_options_values(state, emitter, agent_type, update.config_options)
                .await;
        }
        SessionUpdate::AvailableCommandsUpdate(update) => {
            // Drop config-option state toggles (codex `/plan` — see
            // `is_config_option_state_command`): they're already the
            // `collaboration_mode` selector, not invokable commands. Then dedup:
            // some agents (e.g. Claude Code with overlapping user/project slash
            // commands) emit duplicate entries sharing the same name. Keep the
            // first occurrence so downstream consumers don't render duplicates;
            // the frontend reducer also dedupes as a defensive measure.
            let mut seen = HashSet::new();
            let commands: Vec<AvailableCommandInfo> = update
                .available_commands
                .iter()
                .filter(|cmd| !is_config_option_state_command(agent_type, cmd.meta.as_ref()))
                .filter(|cmd| seen.insert(cmd.name.clone()))
                .map(|cmd| {
                    let input_hint = cmd.input.as_ref().map(|input| match input {
                        sacp::schema::AvailableCommandInput::Unstructured(u) => u.hint.clone(),
                        _ => String::new(),
                    });
                    AvailableCommandInfo {
                        name: cmd.name.clone(),
                        description: cmd.description.clone(),
                        input_hint,
                    }
                })
                .collect();
            emit_with_state(state, emitter, AcpEvent::AvailableCommands { commands }).await;
        }
        SessionUpdate::UsageUpdate(update) => {
            emit_with_state(
                state,
                emitter,
                AcpEvent::UsageUpdate {
                    used: update.used,
                    size: update.size,
                },
            )
            .await;
        }
        SessionUpdate::SessionInfoUpdate(info) => {
            // codex-acp v1.1.0 (#263) reports `/goal` transitions as structured
            // session metadata instead of live "Goal updated (…)" agent text:
            // the goal object rides under `_meta.codex.goal`. Map it onto codeg's
            // canonical create_goal/update_goal synthetic tool call so the
            // existing goal-card pipeline (groupGoalRuns/GoalCard) renders it —
            // byte-identical to the history path (parsers/codex.rs). Non-Codex
            // agents don't populate the `codex` key, so this is a no-op for them.
            // (`info.title` is Codex's native thread name; it is adopted via the
            // parser auto-title path on the next conversation fetch, not here, to
            // keep this DB-agnostic emit path unchanged — see parsers/codex.rs.)
            //
            // Patched codex-acp also emits `_meta.codex.activeTurnId` on
            // turn/started for user-stop reconciliation (Task 3). Accept only
            // while this connection's turn is in flight so late metadata after
            // terminal finalization cannot leak into the next prompt.
            if let Some(codex_meta) = info.meta.as_ref().and_then(|m| m.get("codex")) {
                if let Some(turn_id) = codex_meta
                    .get("activeTurnId")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    let mut s = state.write().await;
                    if s.turn_in_flight {
                        s.active_provider_turn_id = Some(turn_id.to_string());
                    }
                }
                if let Some(goal) = codex_meta.get("goal") {
                    if let Some(marker) = crate::acp::codex_goal::next_goal_marker(
                        &mut cb_state.codex_open_goal,
                        goal,
                    ) {
                        cb_state.codex_goal_seq += 1;
                        let tool_call_id =
                            crate::acp::codex_goal::goal_tool_call_id(cb_state.codex_goal_seq);
                        emit_with_state(
                            state,
                            emitter,
                            AcpEvent::ToolCall {
                                tool_call_id,
                                title: marker.title,
                                kind: "other".to_string(),
                                status: "completed".to_string(),
                                content: None,
                                raw_input: Some(marker.input_json),
                                raw_output: Some(marker.output_json),
                                locations: None,
                                meta: None,
                                images: None,
                            },
                        )
                        .await;
                    }
                }
            }
            // codex-acp #289 (v1.1.3+): a retryable turn error rides under
            // `_meta.codex.error` (only when `willRetry == true`) and the turn
            // stays alive. Surface a transient retry indicator (the frontend
            // reuses the Claude API-retry banner); it is NOT a turn failure.
            if let Some((message, error_status)) = codex_retry_indicator(info.meta.as_ref()) {
                emit_with_state(
                    state,
                    emitter,
                    AcpEvent::TurnRetrying {
                        message,
                        error_status,
                    },
                )
                .await;
            }
        }
        other => {
            // Log unhandled update types for debugging
            tracing::info!("[ACP] Unhandled SessionUpdate: {:?}", other);
        }
    }
}

#[cfg(test)]
mod disconnect_origin {
    use super::*;
    use crate::acp::delegation::continuation::coordinator::ParentConnectionExitCause;
    use crate::acp::termination::{
        AcpDisconnectOrigin, AcpTerminationClassification, AcpTerminationReason,
        AcpTerminationSource, AcpTerminationSummaryV1,
    };

    fn at(second: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(&format!("2026-07-31T01:02:{second:02}Z"))
            .expect("timestamp")
            .with_timezone(&chrono::Utc)
    }

    fn legacy_unspecified_summary() -> AcpTerminationSummaryV1 {
        AcpTerminationSummaryV1::legacy_unspecified(true, at(3))
    }

    fn unexpected_transport_summary() -> AcpTerminationSummaryV1 {
        AcpTerminationSummaryV1::new(
            AcpTerminationSource::Transport,
            AcpTerminationReason::TransportDisconnected,
            AcpTerminationClassification::Unexpected,
            true,
            at(4),
        )
    }

    #[test]
    fn observed_transport_loss_outranks_legacy_unspecified_but_not_recorded_user_intent() {
        let evidence = ParentConnectionExitEvidence::default();
        evidence.record_observation("c1", legacy_unspecified_summary());
        evidence.record_observation("c1", unexpected_transport_summary());
        assert_eq!(
            evidence
                .peek("c1")
                .expect("transport evidence")
                .classification,
            AcpTerminationClassification::Unexpected
        );

        evidence.record_intent("c1", AcpDisconnectOrigin::LegacyUnspecified, at(5));
        assert_eq!(
            evidence
                .peek("c1")
                .expect("transport evidence must outrank legacy intent")
                .classification,
            AcpTerminationClassification::Unexpected
        );

        evidence.record_intent("c1", AcpDisconnectOrigin::ExplicitUser, at(6));
        let intent = evidence.peek("c1").expect("intent evidence");
        assert_eq!(
            intent.frontend_origin,
            Some(AcpDisconnectOrigin::ExplicitUser)
        );

        evidence.record_observation("c1", unexpected_transport_summary());
        assert_eq!(
            evidence.peek("c1").expect("preserved intent"),
            intent,
            "a later transport observation must not overwrite recorded user intent"
        );
    }

    #[test]
    fn disconnect_origin_suspension_timeout_replaces_prior_unexpected_observation() {
        let evidence = ParentConnectionExitEvidence::default();
        evidence.record_observation("c1", unexpected_transport_summary());

        evidence.record_suspension_drain_timeout_at("c1", at(8));

        let ParentConnectionExitCause::SuspensionDrainTimeout { termination } = evidence.take("c1")
        else {
            panic!("suspension timeout must remain the classified exit cause")
        };
        assert_eq!(
            termination.reason,
            AcpTerminationReason::SuspensionDrainTimeout
        );
        assert_eq!(
            termination.classification,
            AcpTerminationClassification::AutomatedAmbiguous
        );
        assert_eq!(termination.observed_at, at(8));
    }

    #[test]
    fn disconnect_origin_explicit_intent_outranks_later_suspension_timeout() {
        let evidence = ParentConnectionExitEvidence::default();
        evidence.record_intent("c1", AcpDisconnectOrigin::ExplicitUser, at(6));

        evidence.record_suspension_drain_timeout_at("c1", at(9));

        let ParentConnectionExitCause::Disconnected { termination } = evidence.take("c1") else {
            panic!("explicit intent must not be relabeled as a suspension timeout")
        };
        assert_eq!(
            termination.frontend_origin,
            Some(AcpDisconnectOrigin::ExplicitUser)
        );
        assert_eq!(termination.requested_at, Some(at(6)));
        assert_eq!(termination.observed_at, at(6));
    }

    #[test]
    fn cleanup_without_evidence_writes_legacy_unspecified_not_transport_loss() {
        let evidence = ParentConnectionExitEvidence::default();
        let ParentConnectionExitCause::Disconnected { termination } = evidence.take("parent")
        else {
            panic!("missing evidence must use the disconnected legacy fallback")
        };
        assert_eq!(
            termination.classification,
            AcpTerminationClassification::LegacyUnknown
        );
        assert_eq!(termination.reason, AcpTerminationReason::LegacyUnspecified);
    }

    #[test]
    fn disconnect_origin_session_channel_loss_records_typed_observation_timestamp() {
        let evidence = ParentConnectionExitEvidence::default();
        evidence.record_session_lost("c1", at(7));

        let observed = evidence.peek("c1").expect("session-loss evidence");
        assert_eq!(observed.source, AcpTerminationSource::Session);
        assert_eq!(observed.reason, AcpTerminationReason::SessionLost);
        assert_eq!(
            observed.classification,
            AcpTerminationClassification::Unexpected
        );
        assert_eq!(observed.observed_at, at(7));
    }

    #[test]
    fn disconnect_origin_recoverable_idle_update_error_records_no_terminal_evidence() {
        let evidence = ParentConnectionExitEvidence::default();
        let error = sacp::util::internal_error("unrecognized session update");

        handle_idle_session_update_error(Some(&evidence), "c1", &error);

        assert_eq!(
            evidence.peek("c1"),
            None,
            "a recoverable idle update error must leave terminal evidence empty"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::terminal_context::TerminalPromptContext;
    use crate::terminal::shell::test_support::{
        posix_spec as test_posix_spec, pwsh_spec as test_pwsh_spec,
    };
    use sacp::schema::Diff;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    struct SuspensionLoopMockAgent {
        prompts: Arc<std::sync::Mutex<Vec<sacp::Responder<sacp::schema::PromptResponse>>>>,
        modes: Arc<std::sync::Mutex<Vec<sacp::Responder<sacp::schema::SetSessionModeResponse>>>>,
        agent_connection: Arc<std::sync::Mutex<Option<ConnectionTo<Client>>>>,
        cancel_count: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl sacp::ConnectTo<Client> for SuspensionLoopMockAgent {
        async fn connect_to(self, client: impl sacp::ConnectTo<Agent>) -> Result<(), sacp::Error> {
            use std::sync::atomic::Ordering;

            let prompt_responders = self.prompts;
            let prompt_connection = self.agent_connection.clone();
            let mode_responders = self.modes;
            let mode_connection = self.agent_connection;
            let cancel_count = self.cancel_count;
            Agent
                .builder()
                .on_receive_request(
                    async move |_request: PromptRequest,
                                responder: sacp::Responder<sacp::schema::PromptResponse>,
                                connection: ConnectionTo<Client>| {
                        *prompt_connection.lock().unwrap() = Some(connection);
                        prompt_responders.lock().unwrap().push(responder);
                        Ok(())
                    },
                    sacp::on_receive_request!(),
                )
                .on_receive_request(
                    async move |_request: SetSessionModeRequest,
                                responder: sacp::Responder<
                        sacp::schema::SetSessionModeResponse,
                    >,
                                connection: ConnectionTo<Client>| {
                        *mode_connection.lock().unwrap() = Some(connection);
                        mode_responders.lock().unwrap().push(responder);
                        Ok(())
                    },
                    sacp::on_receive_request!(),
                )
                .on_receive_notification(
                    async move |_notification: CancelNotification,
                                _connection: ConnectionTo<Client>| {
                        cancel_count.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    },
                    sacp::on_receive_notification!(),
                )
                .connect_to(client)
                .await
        }
    }

    struct SuspensionNoQuestions;

    #[async_trait::async_trait]
    impl crate::acp::question::SessionQuestionAccess for SuspensionNoQuestions {
        async fn register_question(
            &self,
            _parent_connection_id: &str,
            _questions: Vec<crate::acp::question::QuestionSpec>,
        ) -> Option<crate::acp::question::RegisteredQuestion> {
            None
        }

        async fn cancel_question(&self, _parent_connection_id: &str, _question_id: &str) {}

        async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {}
    }

    struct SuspensionNoPlanApprovals;

    #[async_trait::async_trait]
    impl crate::acp::plan_approval::SessionPlanApprovalAccess for SuspensionNoPlanApprovals {
        async fn register_plan_approval(
            &self,
            _parent_connection_id: &str,
            _tool_call_id: String,
            _plan_markdown: String,
        ) -> Option<crate::acp::plan_approval::RegisteredPlanApproval> {
            None
        }

        async fn cancel_plan_approvals_by_parent(&self, _parent_connection_id: &str) {}
    }

    struct SuspensionAllAgentsEnabled;

    #[async_trait::async_trait]
    impl AgentAvailabilityLookup for SuspensionAllAgentsEnabled {
        async fn disabled_agent_wire_slugs(&self) -> Vec<String> {
            Vec::new()
        }
    }

    fn delegation_suspend_injection(
        broker: Arc<crate::acp::delegation::broker::DelegationBroker>,
    ) -> DelegationInjection {
        DelegationInjection {
            broker,
            continuation_coordinator: std::sync::Weak::new(),
            parent_connection_exit_causes: Arc::new(ParentConnectionExitCauses::default()),
            tokens: Arc::new(crate::acp::delegation::listener::TokenRegistry::default()),
            leases: Arc::new(crate::acp::delegation::lease::CompanionLeaseRegistry::default()),
            socket_path: PathBuf::from("/tmp/codeg-suspension-test.sock"),
            agent_availability: Arc::new(SuspensionAllAgentsEnabled),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            questions: Arc::new(SuspensionNoQuestions),
            plan_approvals: Arc::new(SuspensionNoPlanApprovals),
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics: Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        }
    }

    async fn wait_for_suspension_loop_condition(
        description: &str,
        mut condition: impl FnMut() -> bool,
    ) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("timed out waiting for {description}");
    }

    async fn wait_for_suspension_mode_event(
        state: &Arc<RwLock<SessionState>>,
        expected_mode_id: &str,
    ) {
        for _ in 0..200 {
            let found = state
                .read()
                .await
                .recent_events_after(0)
                .expect("contiguous events")
                .iter()
                .any(|event| {
                    matches!(
                        &event.payload,
                        AcpEvent::ModeChanged { mode_id } if mode_id == expected_mode_id
                    )
                });
            if found {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("timed out waiting for ModeChanged({expected_mode_id})");
    }

    fn send_suspension_content_barrier(
        agent_connection: &Arc<std::sync::Mutex<Option<ConnectionTo<Client>>>>,
        text: &str,
    ) {
        let update = SessionNotification::new(
            SessionId::new("session-1".to_string()),
            SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                TextContent::new(text),
            ))),
        );
        agent_connection
            .lock()
            .unwrap()
            .as_ref()
            .expect("mock agent connection")
            .send_notification(update)
            .unwrap();
    }

    async fn wait_for_suspension_content_event(
        state: &Arc<RwLock<SessionState>>,
        expected_text: &str,
    ) {
        for _ in 0..200 {
            let found = state
                .read()
                .await
                .recent_events_after(0)
                .unwrap_or_default()
                .iter()
                .any(|event| {
                    matches!(
                        &event.payload,
                        AcpEvent::ContentDelta { text, .. } if text == expected_text
                    )
                });
            if found {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("timed out waiting for ContentDelta({expected_text})");
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_suspension_test_loop(
        mock_agent: SuspensionLoopMockAgent,
        state: Arc<RwLock<SessionState>>,
        mut cmd_rx: mpsc::Receiver<ConnectionCommand>,
        mut control_rx: mpsc::Receiver<ConnectionControl>,
        mut cmd_liveness_rx: watch::Receiver<bool>,
        mut control_liveness_rx: watch::Receiver<bool>,
        injection: DelegationInjection,
        seed_aux_stop_reason: bool,
    ) -> Result<(), sacp::Error> {
        Client
            .builder()
            .connect_with(mock_agent, async move |cx| {
                let session_id = SessionId::new("session-1".to_string());
                let mut session =
                    cx.attach_session(NewSessionResponse::new(session_id), Default::default())?;
                if seed_aux_stop_reason {
                    session.send_prompt("auxiliary terminal producer")?;
                }
                let shell = test_placeholder_terminal_shell();
                let terminal_runtime = Arc::new(TerminalRuntime::new(
                    BTreeMap::new(),
                    shell.spec.clone(),
                    adapter_for(AgentType::Codex),
                ));
                let terminal_assoc =
                    Arc::new(std::sync::Mutex::new(TerminalAssocFallback::new(false)));
                let file_system_runtime = Arc::new(FileSystemRuntime::new(PathBuf::from(".")));
                let prompt_ledger = background_watch::PromptLedger::shared();
                let terminal_prompt_context = TerminalPromptContext::new(shell.spec.clone());
                let pending_perms: PendingPermissions =
                    Arc::new(tokio::sync::Mutex::new(HashMap::new()));
                let route_plan = native_plan(AgentType::Codex);

                let loop_result = run_conversation_loop(
                    &mut session,
                    "parent-conn",
                    &EventEmitter::Noop,
                    &state,
                    AgentType::Codex,
                    &pending_perms,
                    &mut cmd_rx,
                    &mut control_rx,
                    &mut cmd_liveness_rx,
                    &mut control_liveness_rx,
                    terminal_runtime,
                    terminal_assoc,
                    file_system_runtime,
                    ".",
                    false,
                    &shell.spec,
                    &route_plan,
                    prompt_ledger.as_ref(),
                    &terminal_prompt_context,
                    Some(&injection),
                )
                .await;

                if matches!(loop_result, Ok(None)) {
                    cleanup_delegation_parent(&injection, "parent-conn", &state).await;
                }
                loop_result.map(|_| ())
            })
            .await
    }

    fn delegation_suspend_state(generation: u64) -> Arc<RwLock<SessionState>> {
        let mut state = SessionState::new(
            "parent-conn".into(),
            AgentType::Codex,
            None,
            "test".into(),
            None,
        );
        state.status = ConnectionStatus::Prompting;
        state.turn_in_flight = true;
        state.active_turn_generation = Some(generation);
        state.active_turn = Some(crate::acp::session_state::ActiveTurnContext {
            token: "turn-token".into(),
            locale: AppLocale::En,
        });
        Arc::new(RwLock::new(state))
    }

    fn delegation_suspend_lease(
        generation: u64,
    ) -> (
        SuspensionLease,
        tokio::sync::oneshot::Receiver<Result<SuspensionAck, AcpError>>,
    ) {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        (
            SuspensionLease {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: generation,
                connection_id: "parent-conn".into(),
                session_id: "session-1".into(),
                reply: Some(reply),
            },
            receiver,
        )
    }

    async fn delegation_suspend_broker_with_running_child() -> (
        Arc<crate::acp::delegation::broker::DelegationBroker>,
        Arc<crate::acp::delegation::spawner::mock::MockSpawner>,
        String,
    ) {
        use crate::acp::delegation::broker::{
            ConversationDepthLookup, DelegationBroker, DelegationConfig,
        };
        use crate::acp::delegation::spawner::{accepted, mock::MockSpawner, ConnectionSpawner};
        use crate::acp::delegation::types::{DelegationError, DelegationRequest, TaskStatus};

        struct EmptyLookup;
        #[async_trait::async_trait]
        impl ConversationDepthLookup for EmptyLookup {
            async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
                Ok(None)
            }
        }

        let spawner = Arc::new(MockSpawner::new());
        spawner.queue_spawn(Ok("child-conn".into())).await;
        spawner
            .queue_send(Ok(accepted(42, chrono::Utc::now())))
            .await;
        let broker = Arc::new(DelegationBroker::new(
            spawner.clone() as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;
        let report = broker
            .start_delegation(DelegationRequest {
                parent_connection_id: "parent-conn".into(),
                parent_conversation_id: 1,
                parent_tool_use_id: "tool-1".into(),
                agent_type: AgentType::Codex,
                profile_id: None,
                task: "child task".into(),
                working_dir: None,
                requested_working_dir: None,
                external_handle: None,
                work_unit_key: None,
                replaces_task_id: None,
                replacement_reason: None,
                correlation_id: None,
                recovery_authorization_id: None,
            })
            .await;
        assert_eq!(report.status, TaskStatus::Running);
        (broker, spawner, report.task_id.expect("running task id"))
    }

    async fn delegation_suspend_task_status(
        broker: &crate::acp::delegation::broker::DelegationBroker,
        task_id: &str,
    ) -> crate::acp::delegation::types::TaskStatus {
        use crate::acp::delegation::broker::StatusWait;
        broker
            .get_task_status("parent-conn", Some(1), task_id, StatusWait::Snapshot)
            .await
            .status
    }

    #[tokio::test]
    async fn continuation_cleanup_connection_teardown_projects_recorded_timeout_cause() {
        use crate::acp::delegation::continuation::store::{
            ContinuationStore, InMemoryContinuationStore, NewContinuation,
        };
        use crate::acp::delegation::continuation::types::{
            ContinuationFailureCode, ContinuationState, ContinuationTaskIds,
        };

        let (broker, _spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let store = Arc::new(InMemoryContinuationStore::default());
        let now = chrono::Utc::now();
        let row = store
            .insert_arming(NewContinuation {
                continuation_id: "connection-exit".to_string(),
                parent_conversation_id: 1,
                parent_session_id: "parent-session".to_string(),
                parent_connection_id: "parent-conn".to_string(),
                parent_turn_generation: 1,
                task_ids: ContinuationTaskIds(vec!["task-1".to_string()]),
                armed_at: now,
                wake_at: now,
                internal_prompt_id: "prompt-1".to_string(),
                internal_prompt_marker: "marker-1".to_string(),
            })
            .await
            .unwrap();
        let manager = Arc::new(crate::acp::manager::ConnectionManager::new());
        let coordinator = Arc::new(
            crate::acp::delegation::continuation::coordinator::DelegationContinuationCoordinator::new(
                store.clone(),
                broker.clone(),
                Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
                Arc::new(crate::acp::delegation::continuation::coordinator::ManagerContinuationPort::new(manager)),
                Arc::new(crate::acp::delegation::continuation::coordinator::SystemContinuationClock::new()),
            ),
        );
        let mut injection = delegation_suspend_injection(broker);
        injection.continuation_coordinator = Arc::downgrade(&coordinator);
        injection
            .parent_connection_exit_causes
            .record_suspension_drain_timeout("parent-conn");
        let state = delegation_suspend_state(1);
        state.write().await.conversation_id = Some(1);

        cleanup_delegation_parent(&injection, "parent-conn", &state).await;

        let failed = store.load(&row.continuation_id).await.unwrap().unwrap();
        assert_eq!(failed.state, ContinuationState::Failed);
        assert_eq!(
            failed.failure_code,
            Some(ContinuationFailureCode::SuspendDrainTimeout)
        );
    }

    #[tokio::test]
    async fn continuation_cleanup_connection_cancels_workers_before_state_read() {
        use crate::acp::connection::SuspensionAck;
        use crate::acp::delegation::continuation::coordinator::{
            JoinArmOutcome, JoinArmRequest, ParentContinuationPort, ParentTurnSnapshot,
            PromptAdmissionResult, SuspendRequest, SystemContinuationClock,
        };
        use crate::acp::delegation::continuation::store::{
            ContinuationStore, InMemoryContinuationStore,
        };
        use crate::acp::delegation::continuation::types::{
            ContinuationFailureCode, ContinuationState, ContinuationWaitingProjection,
        };
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Mutex;
        use tokio_util::sync::CancellationToken;

        struct LiveSuspendPort {
            entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            release: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
            admit_calls: AtomicUsize,
            fail_calls: AtomicUsize,
        }

        #[async_trait]
        impl ParentContinuationPort for LiveSuspendPort {
            async fn snapshot_parent(
                &self,
                connection_id: &str,
            ) -> Result<
                ParentTurnSnapshot,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                Ok(ParentTurnSnapshot {
                    connection_id: connection_id.into(),
                    conversation_id: 1,
                    session_id: "parent-session".into(),
                    turn_generation: 1,
                    turn_in_flight: true,
                })
            }

            async fn suspend_parent(
                &self,
                request: SuspendRequest,
            ) -> Result<
                SuspensionAck,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                if let Some(tx) = self
                    .entered
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take()
                {
                    let _ = tx.send(());
                }
                if let Some(release) = self.release.lock().await.take() {
                    let _ = release.await;
                }
                Ok(SuspensionAck {
                    continuation_id: request.continuation_id,
                    parent_turn_generation: request.parent_turn_generation,
                })
            }

            async fn admit_continuation(
                &self,
                _request: crate::acp::delegation::continuation::coordinator::ContinuationPromptRequest,
            ) -> Result<
                PromptAdmissionResult,
                crate::acp::delegation::continuation::coordinator::ContinuationError,
            > {
                self.admit_calls.fetch_add(1, Ordering::SeqCst);
                Ok(PromptAdmissionResult::Admitted)
            }

            async fn publish_waiting(
                &self,
                _connection_id: &str,
                _waiting: Option<ContinuationWaitingProjection>,
            ) -> Result<(), crate::acp::delegation::continuation::coordinator::ContinuationError>
            {
                Ok(())
            }

            async fn publish_failure(
                &self,
                _connection_id: &str,
                _code: ContinuationFailureCode,
            ) -> Result<(), crate::acp::delegation::continuation::coordinator::ContinuationError>
            {
                self.fail_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        }

        struct CleanupEmptyDepth;
        #[async_trait::async_trait]
        impl crate::acp::delegation::broker::ConversationDepthLookup for CleanupEmptyDepth {
            async fn parent_of(
                &self,
                _id: i32,
            ) -> Result<Option<i32>, crate::acp::delegation::types::DelegationError> {
                Ok(None)
            }
        }
        let broker = Arc::new(crate::acp::delegation::broker::DelegationBroker::new(
            Arc::new(crate::acp::delegation::spawner::mock::MockSpawner::default())
                as Arc<dyn crate::acp::delegation::spawner::ConnectionSpawner>,
            Arc::new(CleanupEmptyDepth)
                as Arc<dyn crate::acp::delegation::broker::ConversationDepthLookup>,
        ));
        broker
            .seed_live_task_for_test("parent-conn", "task-1")
            .await;
        let store = Arc::new(InMemoryContinuationStore::default());
        store.seed_parent_status(1, "in_progress").await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let port = Arc::new(LiveSuspendPort {
            entered: Mutex::new(Some(entered_tx)),
            release: tokio::sync::Mutex::new(Some(release_rx)),
            admit_calls: AtomicUsize::new(0),
            fail_calls: AtomicUsize::new(0),
        });
        let coordinator = Arc::new(
            crate::acp::delegation::continuation::coordinator::DelegationContinuationCoordinator::new(
                store.clone() as Arc<dyn ContinuationStore>,
                broker.clone(),
                Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
                port.clone() as Arc<dyn ParentContinuationPort>,
                Arc::new(SystemContinuationClock::new()),
            ),
        );
        let outcome = coordinator
            .begin_arm_from_join(JoinArmRequest {
                parent_connection_id: "parent-conn".into(),
                parent_conversation_id: 1,
                task_ids: vec!["task-1".into()],
                waiter_closed: CancellationToken::new(),
                transferred_wait_rx: None,
                foreground_release: {
                    let (owner, waiter) =
                        crate::acp::delegation::continuation::foreground_mcp_release_fence();
                    owner.frame_flushed();
                    waiter
                },
            })
            .await
            .unwrap();
        let JoinArmOutcome::Arming {
            continuation_id,
            completion,
        } = outcome
        else {
            panic!("expected arming worker");
        };
        entered_rx
            .await
            .expect("live worker must reach suspend gate");

        let mut injection = delegation_suspend_injection(broker);
        injection.continuation_coordinator = Arc::downgrade(&coordinator);
        injection
            .parent_connection_exit_causes
            .record_suspension_drain_timeout("parent-conn");
        let state = delegation_suspend_state(1);
        state.write().await.conversation_id = Some(1);
        // Hold the session write lock so any pre-cancel state.read() blocks.
        // Cancellation must still become visible first.
        let write_guard = state.write().await;
        let mut cleanup = tokio::spawn({
            let injection = injection.clone();
            let state = state.clone();
            async move { cleanup_delegation_parent(&injection, "parent-conn", &state).await }
        });
        let arm_result = tokio::select! {
            result = completion => result.expect("worker join"),
            _ = &mut cleanup => panic!("cleanup finished before cancel observed by worker"),
        };
        assert!(matches!(
            arm_result,
            Err(crate::acp::delegation::continuation::coordinator::ContinuationError::ArmWorkerDropped)
        ));
        assert!(
            !cleanup.is_finished(),
            "cleanup must still be blocked on state.read after cancel"
        );
        let mid = store.load(&continuation_id).await.unwrap().unwrap();
        assert!(
            !matches!(
                mid.state,
                ContinuationState::Failed
                    | ContinuationState::Completed
                    | ContinuationState::Cancelled
            ),
            "worker must not terminalize while cleanup is blocked before drain: {mid:?}"
        );
        assert_eq!(port.admit_calls.load(Ordering::SeqCst), 0);
        assert_eq!(port.fail_calls.load(Ordering::SeqCst), 0);

        drop(write_guard);
        let _ = release_tx.send(());
        cleanup.await.unwrap();
        for _ in 0..30 {
            if coordinator.worker_count() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        let failed = store.load(&continuation_id).await.unwrap().unwrap();
        assert_eq!(failed.state, ContinuationState::Failed);
        assert_eq!(
            failed.failure_code,
            Some(ContinuationFailureCode::SuspendDrainTimeout)
        );
        assert_eq!(
            store.parent_status(1).await.as_deref(),
            Some("cancelled"),
            "parent status must flip atomically with terminal persistence"
        );
        assert_eq!(coordinator.worker_count(), 0);
    }

    #[test]
    fn active_terminal_arbitration_observes_last_sender_closure() {
        let (cmd_tx, _cmd_rx, cmd_liveness_rx) = connection_channel::<ConnectionCommand>(1);
        let (control_tx, _control_rx, control_liveness_rx) =
            connection_channel::<ConnectionControl>(1);
        let cmd_clone = cmd_tx.clone();

        drop(cmd_clone);
        assert!(!*cmd_liveness_rx.borrow());
        drop(cmd_tx);
        drop(control_tx);

        assert!(both_connection_lanes_closed(
            false,
            false,
            &cmd_liveness_rx,
            &control_liveness_rx,
        ));
    }

    #[tokio::test]
    async fn delegation_suspend_rejects_wrong_generation() {
        let state = delegation_suspend_state(2);
        let (lease, mut receiver) = delegation_suspend_lease(1);
        let mut slot = None;

        install_suspension_lease(&*state.read().await, 2, &mut slot, lease);

        assert!(slot.is_none());
        let error = receiver.try_recv().expect("wrong generation must reject");
        assert!(error.unwrap_err().to_string().contains("generation"));
    }

    #[tokio::test]
    async fn delegation_suspend_waits_for_bound_prompt_response() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = delegation_suspend_state(1);
        let (broker, spawner, task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker.clone());
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_connection = Arc::new(std::sync::Mutex::new(None));
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes,
            agent_connection: agent_connection.clone(),
            cancel_count: cancel_count.clone(),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "bound parent prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            true,
        ));

        wait_for_suspension_loop_condition("auxiliary and bound prompt requests", || {
            prompts.lock().unwrap().len() == 2
        })
        .await;
        let (reply, mut receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("suspension CancelNotification", || {
            cancel_count.load(Ordering::SeqCst) == 1
        })
        .await;

        let extension = UntypedMessage::new(
            "_x.ai/session/update",
            serde_json::json!({
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "turn_completed",
                    "stopReason": "end_turn"
                }
            }),
        )
        .unwrap();
        agent_connection
            .lock()
            .unwrap()
            .as_ref()
            .expect("mock agent connection")
            .send_notification(extension)
            .unwrap();
        let auxiliary = prompts.lock().unwrap().remove(0);
        auxiliary
            .respond(sacp::schema::PromptResponse::new(StopReason::EndTurn))
            .unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        let bound = prompts.lock().unwrap().remove(0);
        bound
            .respond(sacp::schema::PromptResponse::new(StopReason::Cancelled))
            .unwrap();
        let ack = tokio::time::timeout(std::time::Duration::from_secs(1), receiver)
            .await
            .expect("bound prompt suspension ack timeout")
            .expect("bound prompt suspension reply")
            .expect("bound prompt suspension success");
        assert_eq!(ack.parent_turn_generation, 1);
        assert_eq!(
            delegation_suspend_task_status(&broker, &task_id).await,
            crate::acp::delegation::types::TaskStatus::Running
        );
        assert!(spawner.cancels.lock().await.is_empty());

        control_tx
            .send(ConnectionControl::Disconnect)
            .await
            .unwrap();
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_bound_response_wins_hung_mode_rpc() {
        use std::sync::atomic::AtomicUsize;

        let state = delegation_suspend_state(1);
        let (broker, spawner, task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker.clone());
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes: modes.clone(),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "bound response prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        wait_for_suspension_loop_condition("bound prompt request", || {
            prompts.lock().unwrap().len() == 1
        })
        .await;
        let (reply, mut receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "durable-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("hung session/set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        let bound = prompts.lock().unwrap().remove(0);
        bound
            .respond(sacp::schema::PromptResponse::new(StopReason::Cancelled))
            .unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        let ack = receiver
            .try_recv()
            .expect("bound prompt response must immediately resolve suspension")
            .expect("bound prompt suspension success");
        assert_eq!(ack.parent_turn_generation, 1);
        assert_eq!(
            delegation_suspend_task_status(&broker, &task_id).await,
            crate::acp::delegation::types::TaskStatus::Running
        );
        assert!(spawner.cancels.lock().await.is_empty());

        modes
            .lock()
            .unwrap()
            .remove(0)
            .respond(sacp::schema::SetSessionModeResponse::new())
            .expect("started mode RPC remains owned after suspension");
        wait_for_suspension_mode_event(&state, "durable-mode").await;
        control_tx
            .send(ConnectionControl::Disconnect)
            .await
            .unwrap();
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn delegation_suspend_duplicate_control_cannot_block_bound_response() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let state = delegation_suspend_state(1);
        let (broker, _spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes: Arc::new(std::sync::Mutex::new(Vec::new())),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: cancel_count.clone(),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "duplicate control stream prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while prompts.lock().unwrap().len() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bound prompt request timeout");
        let (reply, receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while cancel_count.load(Ordering::SeqCst) != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("suspension CancelNotification timeout");
        let stop_producer = Arc::new(AtomicBool::new(false));
        let producer_stop = stop_producer.clone();
        let producer_tx = control_tx.clone();
        let duplicate_producer = std::thread::spawn(move || {
            while !producer_stop.load(Ordering::SeqCst) {
                let (reply, _receiver) = oneshot::channel();
                let _ = producer_tx.try_send(ConnectionControl::SuspendForDelegation {
                    continuation_id: "duplicate".into(),
                    parent_turn_generation: 1,
                    reply,
                });
                std::thread::yield_now();
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while control_tx.capacity() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("duplicate control full-queue barrier timeout");
        prompts
            .lock()
            .unwrap()
            .remove(0)
            .respond(sacp::schema::PromptResponse::new(StopReason::Cancelled))
            .unwrap();
        // Bounded non-starvation under a native duplicate-control producer, not a
        // 1s latency SLA. Match the 5s causal barriers above so loaded CI can
        // schedule the biased prompt arm without false failure.
        let ack_result = tokio::time::timeout(std::time::Duration::from_secs(5), receiver).await;
        stop_producer.store(true, Ordering::SeqCst);
        duplicate_producer.join().expect("duplicate producer join");
        ack_result
            .expect("ready bound response must not be starved by duplicate controls")
            .expect("bound prompt suspension reply")
            .expect("bound prompt suspension success");
        control_tx
            .send(ConnectionControl::Disconnect)
            .await
            .unwrap();
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_reverse_closed_lanes_abort_hung_ancillary() {
        use std::sync::atomic::AtomicUsize;

        let state = delegation_suspend_state(1);
        let (broker, _spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_connection = Arc::new(std::sync::Mutex::new(None));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes: modes.clone(),
            agent_connection: agent_connection.clone(),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "closed lanes prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        wait_for_suspension_loop_condition("bound prompt request", || {
            prompts.lock().unwrap().len() == 1
        })
        .await;
        let (reply, mut receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "hung-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("hung session/set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        drop(control_tx);
        send_suspension_content_barrier(&agent_connection, "control-close-observed");
        wait_for_suspension_content_event(&state, "control-close-observed").await;
        drop(cmd_tx);
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }

        let error = receiver
            .try_recv()
            .expect("closed lanes must resolve installed lease")
            .unwrap_err();
        assert!(error.to_string().contains("suspend_parent_disconnected"));
        assert!(loop_task.is_finished());
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_closed_lanes_beat_ready_bound_response() {
        use std::sync::atomic::AtomicUsize;

        let state = delegation_suspend_state(1);
        let (broker, _spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_connection = Arc::new(std::sync::Mutex::new(None));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes: modes.clone(),
            agent_connection: agent_connection.clone(),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "ready response and closed lanes prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        wait_for_suspension_loop_condition("bound prompt request", || {
            prompts.lock().unwrap().len() == 1
        })
        .await;
        let (reply, mut receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "hung-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("hung session/set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        drop(control_tx);
        send_suspension_content_barrier(&agent_connection, "control-close-observed");
        wait_for_suspension_content_event(&state, "control-close-observed").await;

        prompts
            .lock()
            .unwrap()
            .remove(0)
            .respond(sacp::schema::PromptResponse::new(StopReason::Cancelled))
            .unwrap();
        drop(cmd_tx);
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }

        let error = receiver
            .try_recv()
            .expect("closed lanes must override ready bound response")
            .unwrap_err();
        assert!(error.to_string().contains("suspend_parent_disconnected"));
        assert!(loop_task.is_finished());
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_idle_reverse_closed_lanes_exit_hung_ancillary() {
        use std::sync::atomic::AtomicUsize;

        let mut idle_state = SessionState::new(
            "parent-conn".into(),
            AgentType::Codex,
            None,
            "test".into(),
            None,
        );
        idle_state.status = ConnectionStatus::Connected;
        let state = Arc::new(RwLock::new(idle_state));
        let (broker, spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_connection = Arc::new(std::sync::Mutex::new(None));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: Arc::new(std::sync::Mutex::new(Vec::new())),
            modes: modes.clone(),
            agent_connection: agent_connection.clone(),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "idle-hung-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("idle hung session/set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        drop(control_tx);
        send_suspension_content_barrier(&agent_connection, "idle-control-close-observed");
        wait_for_suspension_content_event(&state, "idle-control-close-observed").await;
        drop(cmd_tx);
        for _ in 0..200 {
            tokio::task::yield_now().await;
        }

        assert!(loop_task.is_finished());
        loop_task.await.unwrap().unwrap();
        wait_for_suspension_loop_condition("idle ParentDisconnected child cancel", || {
            spawner
                .cancels
                .try_lock()
                .map(|cancels| cancels.as_slice() == ["child-conn"])
                .unwrap_or(false)
        })
        .await;
    }

    #[tokio::test]
    async fn idle_user_cancel_reasserts_connected_without_turn_complete() {
        use std::sync::atomic::AtomicUsize;

        let mut idle_state = SessionState::new(
            "parent-conn".into(),
            AgentType::Codex,
            None,
            "test".into(),
            None,
        );
        idle_state.status = ConnectionStatus::Connected;
        let state = Arc::new(RwLock::new(idle_state));
        let (broker, _spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: Arc::new(std::sync::Mutex::new(Vec::new())),
            modes: modes.clone(),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        let event_seq_before_cancel = state.read().await.event_seq;
        control_tx.send(ConnectionControl::Cancel).await.unwrap();

        for _ in 0..200 {
            let has_connected = state
                .read()
                .await
                .recent_events_after(event_seq_before_cancel)
                .unwrap_or_default()
                .iter()
                .any(|event| {
                    matches!(
                        &event.payload,
                        AcpEvent::StatusChanged {
                            status: ConnectionStatus::Connected
                        }
                    )
                });
            if has_connected {
                break;
            }
            tokio::task::yield_now().await;
        }

        let events = state
            .read()
            .await
            .recent_events_after(event_seq_before_cancel)
            .unwrap_or_default();
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        &event.payload,
                        AcpEvent::StatusChanged {
                            status: ConnectionStatus::Connected
                        }
                    )
                })
                .count(),
            1,
            "idle Cancel must publish one authoritative Connected assertion"
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(&event.payload, AcpEvent::TurnComplete { .. })),
            "idle Cancel must not synthesize a TurnComplete"
        );
        assert_eq!(state.read().await.status, ConnectionStatus::Connected);

        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "idle-cancel-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("idle Cancel set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        modes
            .lock()
            .unwrap()
            .remove(0)
            .respond(sacp::schema::SetSessionModeResponse::new())
            .expect("idle Cancel must leave the command loop usable");
        wait_for_suspension_mode_event(&state, "idle-cancel-mode").await;

        control_tx
            .send(ConnectionControl::Disconnect)
            .await
            .unwrap();
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_disconnect_before_cancel_remains_fifo() {
        use std::sync::atomic::AtomicUsize;

        let state = delegation_suspend_state(1);
        let (broker, spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes: modes.clone(),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "disconnect ordering prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        wait_for_suspension_loop_condition("bound prompt request", || {
            prompts.lock().unwrap().len() == 1
        })
        .await;
        let (reply, mut receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "hung-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("hung session/set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        control_tx
            .send(ConnectionControl::Disconnect)
            .await
            .unwrap();
        control_tx.send(ConnectionControl::Cancel).await.unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        let error = receiver
            .try_recv()
            .expect("FIFO disconnect must resolve installed lease")
            .unwrap_err();
        assert!(error.to_string().contains("suspend_parent_disconnected"));
        wait_for_suspension_loop_condition("connection loop exit", || loop_task.is_finished())
            .await;
        loop_task.await.unwrap().unwrap();
        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        assert!(events
            .iter()
            .all(|event| !matches!(event.payload, AcpEvent::TurnComplete { .. })));
        wait_for_suspension_loop_condition("ParentDisconnected child cancel", || {
            spawner
                .cancels
                .try_lock()
                .map(|cancels| cancels.as_slice() == ["child-conn"])
                .unwrap_or(false)
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_user_cancel_retains_started_mode_rpc() {
        use std::sync::atomic::AtomicUsize;

        let state = delegation_suspend_state(1);
        let (broker, _spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes: modes.clone(),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "durable mode prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        wait_for_suspension_loop_condition("bound prompt request", || {
            prompts.lock().unwrap().len() == 1
        })
        .await;
        let (reply, mut receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "post-cancel-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("hung session/set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        control_tx.send(ConnectionControl::Cancel).await.unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let error = receiver
            .try_recv()
            .expect("user cancel must resolve installed lease")
            .unwrap_err();
        assert!(error.to_string().contains("suspend_cancelled_by_user"));

        modes
            .lock()
            .unwrap()
            .remove(0)
            .respond(sacp::schema::SetSessionModeResponse::new())
            .expect("started mode RPC must remain owned across user cancel");
        wait_for_suspension_mode_event(&state, "post-cancel-mode").await;

        control_tx
            .send(ConnectionControl::Disconnect)
            .await
            .unwrap();
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn delegation_suspend_cancelled_response_clears_turn_without_tree_cancel() {
        let state = delegation_suspend_state(1);
        let (broker, spawner, task_id) = delegation_suspend_broker_with_running_child().await;
        let (lease, receiver) = delegation_suspend_lease(1);
        let mut slot = Some(lease);

        let disposition = finalize_turn_terminal(
            TurnTerminalSource::Upstream("cancelled"),
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "parent-conn",
            "session-1",
            AgentType::Codex,
            false,
            Some(broker.as_ref()),
        )
        .await;

        assert!(matches!(
            disposition,
            TurnFinalizationDisposition::DelegationSuspended
        ));
        assert_eq!(
            receiver
                .await
                .expect("ack channel")
                .expect("suspension ack"),
            SuspensionAck {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
            }
        );
        let state = state.read().await;
        assert_eq!(state.last_suspended_turn_generation, Some(1));
        assert_eq!(state.active_turn_generation, None);
        assert!(!state.turn_in_flight);
        let events = state.recent_events_after(0).expect("contiguous events");
        assert!(events
            .iter()
            .all(|event| !matches!(event.payload, AcpEvent::TurnComplete { .. })));
        drop(state);
        assert_eq!(
            delegation_suspend_task_status(&broker, &task_id).await,
            crate::acp::delegation::types::TaskStatus::Running
        );
        assert!(spawner.cancels.lock().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_user_cancel_wins_installed_lease() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = delegation_suspend_state(1);
        let (broker, spawner, task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker.clone());
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes: modes.clone(),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: cancel_count.clone(),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "bound cancel prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state,
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        wait_for_suspension_loop_condition("bound prompt request", || {
            prompts.lock().unwrap().len() == 1
        })
        .await;
        let (reply, mut receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("suspension CancelNotification", || {
            cancel_count.load(Ordering::SeqCst) == 1
        })
        .await;
        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "hung-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("hung session/set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;
        for index in 0..8 {
            cmd_tx
                .send(ConnectionCommand::SetMode {
                    mode_id: format!("queued-mode-{index}"),
                })
                .await
                .unwrap();
        }
        control_tx.send(ConnectionControl::Cancel).await.unwrap();
        tokio::time::advance(std::time::Duration::from_millis(
            SUSPENSION_DRAIN_TIMEOUT_MS,
        ))
        .await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        let error = receiver
            .try_recv()
            .expect("queued user cancel must resolve the installed lease")
            .unwrap_err();
        assert!(error.to_string().contains("suspend_cancelled_by_user"));
        wait_for_suspension_loop_condition("ParentCanceled child cancel", || {
            spawner
                .cancels
                .try_lock()
                .map(|cancels| cancels.as_slice() == ["child-conn"])
                .unwrap_or(false)
        })
        .await;
        assert_eq!(
            delegation_suspend_task_status(&broker, &task_id).await,
            crate::acp::delegation::types::TaskStatus::Canceled
        );

        control_tx
            .send(ConnectionControl::Disconnect)
            .await
            .unwrap();
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn delegation_suspend_natural_end_rejects_lease_and_drains_tree() {
        let state = delegation_suspend_state(1);
        let (broker, _spawner, task_id) = delegation_suspend_broker_with_running_child().await;
        let (lease, receiver) = delegation_suspend_lease(1);
        let mut slot = Some(lease);

        let disposition = finalize_turn_terminal(
            TurnTerminalSource::Upstream("end_turn"),
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "parent-conn",
            "session-1",
            AgentType::Codex,
            true,
            Some(broker.as_ref()),
        )
        .await;

        assert!(matches!(
            disposition,
            TurnFinalizationDisposition::SuspensionFailed
        ));
        assert!(receiver.await.expect("reply").is_err());
        for _ in 0..100 {
            if delegation_suspend_task_status(&broker, &task_id).await
                == crate::acp::delegation::types::TaskStatus::Canceled
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            delegation_suspend_task_status(&broker, &task_id).await,
            crate::acp::delegation::types::TaskStatus::Canceled
        );
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_timeout_disconnects_and_never_acks_reuse() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = delegation_suspend_state(1);
        let (broker, spawner, _task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker);
        let prompts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_connection = Arc::new(std::sync::Mutex::new(None));
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: prompts.clone(),
            modes: modes.clone(),
            agent_connection,
            cancel_count: cancel_count.clone(),
        };
        let (cmd_tx, cmd_rx, cmd_liveness_rx) = connection_channel(8);
        let (control_tx, control_rx, control_liveness_rx) = connection_channel(8);
        cmd_tx
            .send(ConnectionCommand::Prompt {
                blocks: vec![PromptInputBlock::Text {
                    text: "bound timeout prompt".into(),
                }],
                user_message: None,
                mark_awaiting_reply: false,
                turn_generation: 1,
            })
            .await
            .unwrap();
        let loop_task = tokio::spawn(run_suspension_test_loop(
            mock_agent,
            state.clone(),
            cmd_rx,
            control_rx,
            cmd_liveness_rx,
            control_liveness_rx,
            injection,
            false,
        ));

        wait_for_suspension_loop_condition("bound prompt request", || {
            prompts.lock().unwrap().len() == 1
        })
        .await;
        let (reply, mut receiver) = oneshot::channel();
        control_tx
            .send(ConnectionControl::SuspendForDelegation {
                continuation_id: "continuation-1".into(),
                parent_turn_generation: 1,
                reply,
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("suspension CancelNotification", || {
            cancel_count.load(Ordering::SeqCst) == 1
        })
        .await;
        cmd_tx
            .send(ConnectionCommand::SetMode {
                mode_id: "hung-mode".into(),
            })
            .await
            .unwrap();
        wait_for_suspension_loop_condition("hung session/set_mode request", || {
            modes.lock().unwrap().len() == 1
        })
        .await;

        tokio::time::advance(std::time::Duration::from_millis(
            SUSPENSION_DRAIN_TIMEOUT_MS,
        ))
        .await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let error = receiver
            .try_recv()
            .expect("production loop must reject at the absolute drain deadline")
            .unwrap_err();
        assert!(error.to_string().contains("suspend_drain_timeout"));
        wait_for_suspension_loop_condition("connection loop exit", || loop_task.is_finished())
            .await;
        loop_task.await.unwrap().unwrap();
        let state = state.read().await;
        assert_eq!(state.active_turn_generation, Some(1));
        assert_eq!(state.last_suspended_turn_generation, None);
        drop(state);
        wait_for_suspension_loop_condition("ParentDisconnected child cancel", || {
            spawner
                .cancels
                .try_lock()
                .map(|cancels| cancels.as_slice() == ["child-conn"])
                .unwrap_or(false)
        })
        .await;
    }

    #[tokio::test]
    async fn delegation_suspend_late_extension_terminal_before_prompt_response_is_deduplicated() {
        let state = delegation_suspend_state(1);
        let (lease, receiver) = delegation_suspend_lease(1);
        let mut slot = Some(lease);
        let mut diagnostic = None;

        assert!(record_suspension_terminal_diagnostic(
            slot.as_ref(),
            &mut diagnostic,
            "end_turn",
        ));
        let disposition = finalize_turn_terminal(
            TurnTerminalSource::Upstream("cancelled"),
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "parent-conn",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;

        assert!(matches!(
            disposition,
            TurnFinalizationDisposition::DelegationSuspended
        ));
        assert!(receiver.await.expect("reply").is_ok());
        let state = state.read().await;
        let events = state.recent_events_after(0).expect("contiguous events");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.payload, AcpEvent::TurnComplete { .. }))
                .count(),
            0
        );
    }

    fn agent_text_update(text: &str) -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text}
        }))
        .expect("agent_message_chunk")
    }

    fn agent_thought_update(text: &str) -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "agent_thought_chunk",
            "content": {"type": "text", "text": text}
        }))
        .expect("agent_thought_chunk")
    }

    fn tool_start_update(tool_id: &str) -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": tool_id,
            "title": "run",
            "kind": "execute",
            "status": "pending"
        }))
        .expect("tool_call")
    }

    fn tool_progress_update(tool_id: &str) -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": tool_id,
            "status": "in_progress"
        }))
        .expect("tool_call_update")
    }

    fn plan_update() -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "plan",
            "entries": [{
                "content": "step",
                "priority": "medium",
                "status": "pending"
            }]
        }))
        .expect("plan")
    }

    fn available_commands_update() -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "available_commands_update",
            "availableCommands": []
        }))
        .expect("available_commands_update")
    }

    fn usage_update() -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "usage_update",
            "used": 1,
            "size": 100
        }))
        .expect("usage_update")
    }

    fn user_message_update(text: &str) -> SessionUpdate {
        serde_json::from_value(serde_json::json!({
            "sessionUpdate": "user_message_chunk",
            "content": {"type": "text", "text": text}
        }))
        .expect("user_message_chunk")
    }

    #[test]
    fn agent_activity_classifier_excludes_ui_and_status_noise() {
        assert!(advances_agent_activity(&agent_text_update("x")));
        assert!(advances_agent_activity(&agent_thought_update("x")));
        assert!(advances_agent_activity(&tool_start_update("tool-1")));
        assert!(advances_agent_activity(&tool_progress_update("tool-1")));
        assert!(advances_agent_activity(&plan_update()));
        assert!(!advances_agent_activity(&available_commands_update()));
        assert!(!advances_agent_activity(&usage_update()));
        assert!(!advances_agent_activity(&user_message_update("keepalive")));
    }

    #[tokio::test]
    async fn semantic_updates_advance_session_activity_without_frontend_delivery() {
        use crate::acp::delegation::supervisor::derive_observation;
        use crate::acp::delegation::types::TaskObservation;
        use tokio::sync::RwLock;

        let state = Arc::new(RwLock::new(SessionState::new(
            "activity-test".into(),
            AgentType::ClaudeCode,
            None,
            "test-window".into(),
            None,
        )));
        let base = chrono::DateTime::parse_from_rfc3339("2026-07-25T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        state.write().await.last_agent_activity_at = base;

        let semantic = [
            agent_text_update("token"),
            agent_thought_update("reasoning"),
            plan_update(),
            tool_start_update("tool-1"),
            tool_progress_update("tool-1"),
        ];
        for (index, update) in semantic.iter().enumerate() {
            let at = base + chrono::Duration::seconds(index as i64 + 1);
            assert!(mark_agent_activity_for_update(&state, update, at).await);
            assert_eq!(state.read().await.last_agent_activity_at, at);
        }

        let last = state.read().await.last_agent_activity_at;
        assert_eq!(
            derive_observation(last + chrono::Duration::seconds(299), last, false, 300,)
                .observation,
            TaskObservation::Active,
        );
        let noise = [
            available_commands_update(),
            usage_update(),
            user_message_update("keepalive"),
        ];
        for update in &noise {
            assert!(
                !mark_agent_activity_for_update(&state, update, last + chrono::Duration::hours(1),)
                    .await
            );
            assert_eq!(state.read().await.last_agent_activity_at, last);
        }
        assert_eq!(
            derive_observation(
                last + chrono::Duration::seconds(300),
                state.read().await.last_agent_activity_at,
                false,
                300,
            )
            .observation,
            TaskObservation::Stalled,
        );
    }

    #[test]
    fn parent_turn_end_reason_maps_stop_strings() {
        use crate::acp::delegation::types::ParentTurnEndReason;
        assert_eq!(
            parent_turn_end_reason("cancelled"),
            ParentTurnEndReason::ParentCanceled
        );
        assert_eq!(
            parent_turn_end_reason("end_turn"),
            ParentTurnEndReason::JoinAbandoned
        );
        for failure in [
            "refusal",
            "max_tokens",
            "max_turn_requests",
            "empty",
            "unknown",
            "something_new",
        ] {
            assert_eq!(
                parent_turn_end_reason(failure),
                ParentTurnEndReason::ParentTurnFailed,
                "stop_reason={failure}"
            );
        }
    }

    #[test]
    fn grok_ask_ext_request_routes_and_parses_captured_wire_shape() {
        use sacp::JsonRpcMessage;
        // Routing: the derive matches ONLY the underscore-prefixed ext method
        // (sacp routes typed handlers on the raw wire method — verified against
        // grok 0.2.101, where the missing underscore made codeg answer "unhandled"
        // and grok fall back to inert rendering).
        assert!(GrokAskUserQuestionRequest::matches_method(
            "_x.ai/ask_user_question"
        ));
        assert!(!GrokAskUserQuestionRequest::matches_method(
            "x.ai/ask_user_question"
        ));
        assert!(!GrokAskUserQuestionRequest::matches_method(
            "session/prompt"
        ));

        // The exact params grok sends (captured from a real 0.2.101 run): the
        // transparent newtype must deserialize them and the raw object must parse
        // into register-valid specs.
        let params = serde_json::json!({
            "sessionId": "019f70eb-32e5-7692-ae92-86fb6cb916a5",
            "toolCallId": "call-1af86ae7-ed54-440e-a983-2c5d22aa6682-0",
            "questions": [{
                "question": "What is your favorite color?",
                "options": [
                    { "label": "Red", "description": "Red" },
                    { "label": "Green", "description": "Green" },
                    { "label": "Blue", "description": "Blue" }
                ],
                "multiSelect": false
            }],
            "mode": "default"
        });
        let req: GrokAskUserQuestionRequest = serde_json::from_value(params).unwrap();
        let specs = crate::acp::question::parse_grok_ext_questions(&req.0).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].question, "What is your favorite color?");
        assert_eq!(specs[0].options.len(), 3);
        assert!(!specs[0].multi_select);
        crate::acp::question::validate_specs(&specs).unwrap();
    }

    fn diff_content(path: &str, old: Option<&str>, new: &str) -> ToolCallContent {
        let mut d = Diff::new(path, new);
        if let Some(o) = old {
            d = d.old_text(o.to_string());
        }
        ToolCallContent::Diff(d)
    }

    use crate::acp::delegation::route::{
        DelegationRoutePlan, DelegationRoutePolicy, DelegationRouteSource, NativeSuppressionPlan,
        RouteDegradedReason, ROUTE_ADAPTER_CONTRACT_VERSION,
    };

    fn codeg_plan(agent_type: AgentType) -> DelegationRoutePlan {
        let native_suppression = match agent_type {
            AgentType::Codex => NativeSuppressionPlan::CodexMultiAgentFalse,
            AgentType::Grok => NativeSuppressionPlan::GrokNoSubagents,
            AgentType::CodeBuddy => NativeSuppressionPlan::CodeBuddyDisallowedTools {
                tools: vec!["Agent".into(), "Task".into()],
            },
            AgentType::ClaudeCode => NativeSuppressionPlan::ClaudeDisallowedTools {
                tools: vec!["Agent".into(), "Task".into()],
            },
            _ => NativeSuppressionPlan::None,
        };
        DelegationRoutePlan {
            managed: true,
            requested: DelegationRoutePolicy::Codeg,
            effective: DelegationRoutePolicy::Codeg,
            source: DelegationRouteSource::GlobalDefault,
            native_suppression,
            expose_codeg_delegation: true,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: format!("test-codeg-{agent_type:?}"),
        }
    }

    fn native_plan(agent_type: AgentType) -> DelegationRoutePlan {
        let _ = agent_type;
        DelegationRoutePlan {
            managed: true,
            requested: DelegationRoutePolicy::Native,
            effective: DelegationRoutePolicy::Native,
            source: DelegationRouteSource::SessionOverride,
            native_suppression: NativeSuppressionPlan::None,
            expose_codeg_delegation: false,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: format!("test-native-{agent_type:?}"),
        }
    }

    #[test]
    fn continuation_capability_defaults_on_for_codex_codeg() {
        assert!(continuation_enabled_for_launch(
            &codeg_plan(AgentType::Codex),
            AgentType::Codex,
            None,
        ));
    }

    #[test]
    fn continuation_capability_kill_switch_and_scope() {
        use std::ffi::OsStr;

        let codex_codeg = codeg_plan(AgentType::Codex);
        for enabled in [
            OsStr::new("1"),
            OsStr::new("true"),
            OsStr::new("TRUE"),
            OsStr::new("yes"),
        ] {
            assert!(continuation_enabled_for_launch(
                &codex_codeg,
                AgentType::Codex,
                Some(enabled),
            ));
        }
        for disabled in [OsStr::new("0"), OsStr::new("false"), OsStr::new("FALSE")] {
            assert!(!continuation_enabled_for_launch(
                &codex_codeg,
                AgentType::Codex,
                Some(disabled),
            ));
        }
        // Scope: never on native routes or non-Codex agents, even with env on.
        assert!(!continuation_enabled_for_launch(
            &native_plan(AgentType::Codex),
            AgentType::Codex,
            None,
        ));
        assert!(!continuation_enabled_for_launch(
            &native_plan(AgentType::Codex),
            AgentType::Codex,
            Some(OsStr::new("1")),
        ));
        assert!(!continuation_enabled_for_launch(
            &codeg_plan(AgentType::ClaudeCode),
            AgentType::ClaudeCode,
            None,
        ));
        assert!(!continuation_enabled_for_launch(
            &codeg_plan(AgentType::ClaudeCode),
            AgentType::ClaudeCode,
            Some(OsStr::new("1")),
        ));
    }

    /// Build base Npx argv then apply the immutable route plan once — mirrors
    /// production `build_agent` for Npx agents.
    fn apply_base_npx_then_route(
        parts: &mut Vec<String>,
        agent_type: AgentType,
        args: &[&str],
        grok_permission_mode: Option<&str>,
        plan: &DelegationRoutePlan,
    ) {
        append_npx_launch_args(parts, agent_type, args, grok_permission_mode);
        apply_process_route(plan, agent_type, &mut BTreeMap::new(), parts).unwrap();
    }

    #[test]
    fn grok_npx_launch_args_put_permission_mode_before_subcommand() {
        let mut default_mode = vec!["grok".to_string()];
        append_npx_launch_args(
            &mut default_mode,
            AgentType::Grok,
            &["agent", "stdio"],
            None,
        );
        assert_eq!(
            default_mode,
            vec!["grok", "--no-auto-update", "agent", "stdio"]
        );

        let mut bypass = vec!["grok".to_string()];
        append_npx_launch_args(
            &mut bypass,
            AgentType::Grok,
            &["agent", "stdio"],
            Some("bypassPermissions"),
        );
        assert_eq!(
            bypass,
            vec![
                "grok",
                "--no-auto-update",
                "--permission-mode",
                "bypassPermissions",
                "agent",
                "stdio",
            ]
        );
    }

    #[test]
    fn non_grok_npx_launch_args_remain_unchanged() {
        let mut parts = vec!["codex-acp".to_string()];
        append_npx_launch_args(
            &mut parts,
            AgentType::Codex,
            &["serve"],
            Some("bypassPermissions"),
        );
        assert_eq!(parts, vec!["codex-acp", "serve"]);
    }

    #[test]
    fn managed_process_adapters_suppress_only_on_codeg_route() {
        let codeg_grok = codeg_plan(AgentType::Grok);
        let mut grok = vec!["grok".to_string()];
        apply_base_npx_then_route(
            &mut grok,
            AgentType::Grok,
            &["agent", "stdio"],
            None,
            &codeg_grok,
        );
        assert_eq!(
            grok,
            vec![
                "grok",
                "--no-auto-update",
                "--no-subagents",
                "agent",
                "stdio",
            ]
        );

        // The permission flag remains ahead of route suppression and subcommand.
        let mut grok_approve = vec!["grok".to_string()];
        apply_base_npx_then_route(
            &mut grok_approve,
            AgentType::Grok,
            &["agent", "stdio"],
            Some("bypassPermissions"),
            &codeg_plan(AgentType::Grok),
        );
        assert_eq!(
            grok_approve,
            vec![
                "grok",
                "--no-auto-update",
                "--permission-mode",
                "bypassPermissions",
                "--no-subagents",
                "agent",
                "stdio",
            ]
        );

        let mut codebuddy = vec!["codebuddy".to_string()];
        apply_base_npx_then_route(
            &mut codebuddy,
            AgentType::CodeBuddy,
            &["--acp"],
            None,
            &codeg_plan(AgentType::CodeBuddy),
        );
        assert_eq!(
            codebuddy,
            vec!["codebuddy", "--disallowedTools", "Agent", "Task", "--acp"]
        );

        // Stable de-duplicated union preserves user denies (TaskOutput/TaskStop)
        // and does not double-add Agent/Task.
        let mut codebuddy_union = vec![
            "codebuddy".to_string(),
            "--disallowedTools".to_string(),
            "Bash".to_string(),
            "TaskOutput".to_string(),
            "Task".to_string(),
            "TaskStop".to_string(),
        ];
        apply_base_npx_then_route(
            &mut codebuddy_union,
            AgentType::CodeBuddy,
            &["--acp"],
            None,
            &codeg_plan(AgentType::CodeBuddy),
        );
        assert_eq!(
            codebuddy_union,
            vec![
                "codebuddy",
                "--disallowedTools",
                "Bash",
                "TaskOutput",
                "Task",
                "TaskStop",
                "Agent",
                "--acp",
            ]
        );

        let mut native_grok = vec!["grok".to_string()];
        apply_base_npx_then_route(
            &mut native_grok,
            AgentType::Grok,
            &["agent", "stdio"],
            None,
            &native_plan(AgentType::Grok),
        );
        assert!(!native_grok.contains(&"--no-subagents".to_string()));
        assert!(!native_grok.contains(&"--disallowed-tools".to_string()));

        let mut native_cb = vec!["codebuddy".to_string()];
        apply_base_npx_then_route(
            &mut native_cb,
            AgentType::CodeBuddy,
            &["--acp"],
            None,
            &native_plan(AgentType::CodeBuddy),
        );
        assert_eq!(native_cb, vec!["codebuddy", "--acp"]);
        assert!(!native_cb.iter().any(|a| a == "--disallowedTools"));
    }

    #[test]
    fn codex_codeg_route_sets_official_multi_agent_config() {
        let mut env = BTreeMap::from([("KEEP".into(), "yes".into())]);
        apply_route_environment_with_inherited(
            AgentType::Codex,
            &codeg_plan(AgentType::Codex),
            &mut env,
            false,
            || None,
        )
        .unwrap();

        let config: serde_json::Value =
            serde_json::from_str(env.get("CODEX_CONFIG").unwrap()).unwrap();
        assert_eq!(config["features"]["multi_agent"], false);
        assert_eq!(env.get("KEEP").map(String::as_str), Some("yes"));
        assert!(!env.contains_key("CODEX_ACP_MULTI_AGENT"));
    }

    #[test]
    fn codex_codeg_route_merges_existing_official_config() {
        let original = serde_json::json!({
            "model": "gpt-5.4",
            "features": { "fast_mode": true, "multi_agent": true },
            "nested": { "keep": [1, 2, 3] }
        });
        let mut env = BTreeMap::from([
            (
                "CODEX_CONFIG".into(),
                serde_json::to_string(&original).unwrap(),
            ),
            ("CODEX_ACP_MULTI_AGENT".into(), "user-value".into()),
        ]);
        apply_route_environment(AgentType::Codex, &codeg_plan(AgentType::Codex), &mut env).unwrap();

        let merged: serde_json::Value =
            serde_json::from_str(env.get("CODEX_CONFIG").unwrap()).unwrap();
        assert_eq!(merged["model"], "gpt-5.4");
        assert_eq!(merged["features"]["fast_mode"], true);
        assert_eq!(merged["features"]["multi_agent"], false);
        assert_eq!(merged["nested"], original["nested"]);
        assert_eq!(
            env.get("CODEX_ACP_MULTI_AGENT").map(String::as_str),
            Some("user-value")
        );
    }

    #[test]
    fn codex_codeg_route_rejects_malformed_official_config() {
        for raw in ["not-json", "[]", r#"{"features":[]}"#] {
            let mut env = BTreeMap::from([("CODEX_CONFIG".into(), raw.into())]);
            let err =
                apply_route_environment(AgentType::Codex, &codeg_plan(AgentType::Codex), &mut env)
                    .unwrap_err();
            assert!(matches!(
                err,
                AcpError::RouteUnavailable {
                    reason: RouteDegradedReason::NativeSuppressionInvalid
                }
            ));
            assert_eq!(env.get("CODEX_CONFIG").map(String::as_str), Some(raw));
        }
    }

    #[test]
    fn codex_native_route_preserves_official_config_byte_for_byte() {
        let raw = " { \"features\" : { \"multi_agent\" : true } } ";
        let mut env = BTreeMap::from([("CODEX_CONFIG".into(), raw.into())]);
        apply_route_environment(AgentType::Codex, &native_plan(AgentType::Codex), &mut env)
            .unwrap();
        assert_eq!(env.get("CODEX_CONFIG").map(String::as_str), Some(raw));
    }

    #[test]
    fn codex_inherited_config_merges_valid_parent_value() {
        let inherited = serde_json::json!({
            "model": "gpt-5.4",
            "features": { "fast_mode": true, "multi_agent": true },
            "nested": { "keep": [1, 2, 3] }
        });
        let inherited = serde_json::to_string(&inherited).unwrap();
        let mut env = BTreeMap::from([("KEEP".into(), "yes".into())]);

        apply_route_environment_with_inherited(
            AgentType::Codex,
            &codeg_plan(AgentType::Codex),
            &mut env,
            false,
            || Some(inherited.into()),
        )
        .unwrap();

        let merged: serde_json::Value =
            serde_json::from_str(env.get("CODEX_CONFIG").unwrap()).unwrap();
        assert_eq!(merged["model"], "gpt-5.4");
        assert_eq!(merged["features"]["fast_mode"], true);
        assert_eq!(merged["features"]["multi_agent"], false);
        assert_eq!(merged["nested"], serde_json::json!({ "keep": [1, 2, 3] }));
        assert_eq!(env.get("KEEP").map(String::as_str), Some("yes"));
    }

    #[test]
    fn codex_inherited_config_rejects_malformed_parent_value_atomically() {
        for raw in ["not-json", "[]", r#"{"features":[]}"#] {
            let mut env = BTreeMap::from([("KEEP".into(), "yes".into())]);
            let original = env.clone();

            let err = apply_route_environment_with_inherited(
                AgentType::Codex,
                &codeg_plan(AgentType::Codex),
                &mut env,
                false,
                || Some(raw.into()),
            )
            .unwrap_err();

            assert!(matches!(
                err,
                AcpError::RouteUnavailable {
                    reason: RouteDegradedReason::NativeSuppressionInvalid
                }
            ));
            assert_eq!(env, original);
        }
    }

    #[test]
    fn codex_inherited_config_defers_to_explicit_launch_value() {
        let explicit = serde_json::json!({
            "model": "explicit",
            "features": { "multi_agent": true }
        });
        let mut env = BTreeMap::from([(
            "CODEX_CONFIG".into(),
            serde_json::to_string(&explicit).unwrap(),
        )]);

        apply_route_environment_with_inherited(
            AgentType::Codex,
            &codeg_plan(AgentType::Codex),
            &mut env,
            false,
            || panic!("explicit CODEX_CONFIG must avoid inherited lookup"),
        )
        .unwrap();

        let merged: serde_json::Value =
            serde_json::from_str(env.get("CODEX_CONFIG").unwrap()).unwrap();
        assert_eq!(merged["model"], "explicit");
        assert_eq!(merged["features"]["multi_agent"], false);
    }

    #[test]
    fn codex_inherited_config_uses_windows_case_insensitive_explicit_key() {
        let effective = serde_json::json!({
            "model": "effective-explicit",
            "features": { "multi_agent": true }
        });
        let mut env = BTreeMap::from([
            ("CODEX_CONFIG".into(), "not-json".into()),
            (
                "codex_config".into(),
                serde_json::to_string(&effective).unwrap(),
            ),
        ]);

        apply_route_environment_with_inherited(
            AgentType::Codex,
            &codeg_plan(AgentType::Codex),
            &mut env,
            true,
            || panic!("case-insensitive explicit key must avoid inherited lookup"),
        )
        .unwrap();

        let matching: Vec<_> = env
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("CODEX_CONFIG"))
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].0, "codex_config");
        let merged: serde_json::Value = serde_json::from_str(matching[0].1).unwrap();
        assert_eq!(merged["model"], "effective-explicit");
        assert_eq!(merged["features"]["multi_agent"], false);
    }

    #[test]
    fn codex_inherited_config_preserves_unix_case_sensitive_explicit_key() {
        let lower_raw = r#"{"model":"lowercase-unrelated"}"#;
        let inherited = r#"{"model":"inherited"}"#;
        let mut env = BTreeMap::from([("codex_config".into(), lower_raw.into())]);

        apply_route_environment_with_inherited(
            AgentType::Codex,
            &codeg_plan(AgentType::Codex),
            &mut env,
            false,
            || Some(inherited.into()),
        )
        .unwrap();

        assert_eq!(env.get("codex_config").map(String::as_str), Some(lower_raw));
        let merged: serde_json::Value =
            serde_json::from_str(env.get("CODEX_CONFIG").unwrap()).unwrap();
        assert_eq!(merged["model"], "inherited");
        assert_eq!(merged["features"]["multi_agent"], false);
    }

    #[test]
    fn codex_inherited_config_is_not_looked_up_for_native_or_unrelated_routes() {
        let original = BTreeMap::from([("KEEP".into(), "yes".into())]);

        let mut native_env = original.clone();
        apply_route_environment_with_inherited(
            AgentType::Codex,
            &native_plan(AgentType::Codex),
            &mut native_env,
            false,
            || panic!("native route must not look up inherited CODEX_CONFIG"),
        )
        .unwrap();
        assert_eq!(native_env, original);

        let mut unrelated_env = original.clone();
        apply_route_environment_with_inherited(
            AgentType::ClaudeCode,
            &codeg_plan(AgentType::Codex),
            &mut unrelated_env,
            false,
            || panic!("unrelated route must not look up inherited CODEX_CONFIG"),
        )
        .unwrap();
        assert_eq!(unrelated_env, original);
    }

    #[test]
    fn grok_env_and_claude_meta_are_additive_and_route_scoped() {
        // Grok Codeg sets GROK_SUBAGENTS=0 and never touches CODEX_ACP_MULTI_AGENT.
        let mut grok_env = BTreeMap::from([
            ("CODEX_ACP_MULTI_AGENT".into(), "1".into()),
            ("GROK_SUBAGENTS".into(), "1".into()),
        ]);
        apply_route_environment(AgentType::Grok, &codeg_plan(AgentType::Grok), &mut grok_env)
            .unwrap();
        assert_eq!(
            grok_env.get("CODEX_ACP_MULTI_AGENT").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            grok_env.get("GROK_SUBAGENTS").map(String::as_str),
            Some("0")
        );
        // Native Grok leaves GROK_SUBAGENTS untouched.
        let mut grok_native_env = BTreeMap::from([("GROK_SUBAGENTS".into(), "1".into())]);
        apply_route_environment(
            AgentType::Grok,
            &native_plan(AgentType::Grok),
            &mut grok_native_env,
        )
        .unwrap();
        assert_eq!(
            grok_native_env.get("GROK_SUBAGENTS").map(String::as_str),
            Some("1")
        );

        let existing = serde_json::json!({
            "claudeCode": {
                "emitRawSDKMessages": true,
                "options": { "disallowedTools": ["Bash"] }
            },
            "adapter": { "keep": true }
        });
        let merged = merge_claude_route_meta(
            existing.as_object().unwrap().clone(),
            &codeg_plan(AgentType::ClaudeCode),
        )
        .unwrap();
        assert_eq!(
            merged["claudeCode"]["options"]["disallowedTools"],
            serde_json::json!(["Bash", "Agent", "Task"])
        );
        assert_eq!(merged["claudeCode"]["emitRawSDKMessages"], true);
        assert_eq!(merged["adapter"]["keep"], true);

        // Existing Agent/Task are not duplicated; TaskOutput/TaskStop preserved.
        let with_partial = serde_json::json!({
            "claudeCode": {
                "options": {
                    "disallowedTools": ["Agent", "TaskOutput", "TaskStop"]
                }
            }
        });
        let merged_partial = merge_claude_route_meta(
            with_partial.as_object().unwrap().clone(),
            &codeg_plan(AgentType::ClaudeCode),
        )
        .unwrap();
        assert_eq!(
            merged_partial["claudeCode"]["options"]["disallowedTools"],
            serde_json::json!(["Agent", "TaskOutput", "TaskStop", "Task"])
        );

        // Native: serde-value equivalent to input (no Codeg deny injection).
        let native_input = serde_json::json!({
            "claudeCode": {
                "emitRawSDKMessages": true,
                "options": { "disallowedTools": ["Bash"] }
            },
            "adapter": { "keep": true }
        });
        let native_merged = merge_claude_route_meta(
            native_input.as_object().unwrap().clone(),
            &native_plan(AgentType::ClaudeCode),
        )
        .unwrap();
        assert_eq!(serde_json::Value::Object(native_merged), native_input);

        // Malformed shapes → RouteUnavailable NativeSuppressionInvalid.
        let bad_claude = serde_json::json!({ "claudeCode": "not-an-object" });
        let err = merge_claude_route_meta(
            bad_claude.as_object().unwrap().clone(),
            &codeg_plan(AgentType::ClaudeCode),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AcpError::RouteUnavailable {
                reason: RouteDegradedReason::NativeSuppressionInvalid
            }
        ));

        let bad_options = serde_json::json!({
            "claudeCode": { "options": [] }
        });
        let err = merge_claude_route_meta(
            bad_options.as_object().unwrap().clone(),
            &codeg_plan(AgentType::ClaudeCode),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AcpError::RouteUnavailable {
                reason: RouteDegradedReason::NativeSuppressionInvalid
            }
        ));

        let bad_tools = serde_json::json!({
            "claudeCode": {
                "options": { "disallowedTools": "Agent" }
            }
        });
        let err = merge_claude_route_meta(
            bad_tools.as_object().unwrap().clone(),
            &codeg_plan(AgentType::ClaudeCode),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            AcpError::RouteUnavailable {
                reason: RouteDegradedReason::NativeSuppressionInvalid
            }
        ));
    }

    /// Typed side channel only: display-string recovery must not mint RouteSpecific.
    #[test]
    fn bootstrap_outcome_typed_only_no_substring_fallback() {
        use crate::acp::delegation::route::RouteDegradedReason;

        // Exact typed mapping for the two allowed RouteSpecific reasons.
        assert!(matches!(
            bootstrap_outcome_from_acp_error(&AcpError::RouteUnavailable {
                reason: RouteDegradedReason::NativeSuppressionInvalid,
            }),
            RouteBootstrapOutcome::RouteSpecific(RouteDegradedReason::NativeSuppressionInvalid)
        ));
        assert!(matches!(
            bootstrap_outcome_from_acp_error(&AcpError::RouteUnavailable {
                reason: RouteDegradedReason::CompanionInitializationFailed,
            }),
            RouteBootstrapOutcome::RouteSpecific(
                RouteDegradedReason::CompanionInitializationFailed
            )
        ));

        // Auth/provider/SDK/process/generic stay Fatal.
        assert!(matches!(
            bootstrap_outcome_from_acp_error(&AcpError::SdkNotInstalled("missing".into())),
            RouteBootstrapOutcome::Fatal(AcpError::SdkNotInstalled(_))
        ));
        assert!(matches!(
            bootstrap_outcome_from_acp_error(&AcpError::InitializeTimeout),
            RouteBootstrapOutcome::Fatal(AcpError::InitializeTimeout)
        ));
        assert!(matches!(
            bootstrap_outcome_from_acp_error(&AcpError::ProcessExited),
            RouteBootstrapOutcome::Fatal(AcpError::Protocol(_))
        ));
        assert!(matches!(
            bootstrap_outcome_from_acp_error(&AcpError::protocol("auth failed")),
            RouteBootstrapOutcome::Fatal(AcpError::Protocol(_))
        ));

        // Residual classifier: init-timeout sentinel only — never parse
        // "delegation route unavailable: NativeSuppressionInvalid" display text.
        assert!(matches!(
            classify_connect_error_residual(INIT_TIMEOUT_SENTINEL),
            AcpError::InitializeTimeout
        ));
        let spoof = "delegation route unavailable: NativeSuppressionInvalid";
        assert!(
            matches!(
                classify_connect_error_residual(spoof),
                AcpError::Protocol(_)
            ),
            "substring fallback must not recover NativeSuppressionInvalid"
        );
        let spoof2 = "codeg delegation ready lease failed";
        assert!(
            matches!(
                classify_connect_error_residual(spoof2),
                AcpError::Protocol(_)
            ),
            "substring fallback must not recover CompanionInitializationFailed"
        );
        // Spoofed residual → Fatal, not RouteSpecific.
        let residual = classify_connect_error_residual(spoof);
        assert!(matches!(
            bootstrap_outcome_from_acp_error(&residual),
            RouteBootstrapOutcome::Fatal(_)
        ));
    }

    #[tokio::test]
    async fn bridge_acp_err_sends_typed_native_suppression_once() {
        use crate::acp::delegation::route::RouteDegradedReason;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let slot = Arc::new(tokio::sync::Mutex::new(Some(tx)));
        let sacp_err = bridge_acp_err_for_bootstrap(
            AcpError::RouteUnavailable {
                reason: RouteDegradedReason::NativeSuppressionInvalid,
            },
            &slot,
        )
        .await;
        // Display may still include the error text (sacp boundary), but bootstrap
        // outcome is typed and already consumed.
        assert!(!sacp_err.to_string().is_empty());
        assert!(slot.lock().await.is_none(), "sender must be taken");
        match rx.await.unwrap() {
            RouteBootstrapOutcome::RouteSpecific(RouteDegradedReason::NativeSuppressionInvalid) => {
            }
            other => panic!("expected typed RouteSpecific, got {other:?}"),
        }
        // Second bridge does not panic / double-send.
        let _ = bridge_acp_err_for_bootstrap(
            AcpError::RouteUnavailable {
                reason: RouteDegradedReason::NativeSuppressionInvalid,
            },
            &slot,
        )
        .await;
    }

    #[test]
    fn session_request_meta_claude_deny_list_matches_new_load_resume() {
        let plan = codeg_plan(AgentType::ClaudeCode);
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let spec = test_posix_spec();
        let adapter = adapter_for(AgentType::ClaudeCode);

        let new_req = build_new_session_request(
            AgentType::ClaudeCode,
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        let load_req = build_load_session_request(
            AgentType::ClaudeCode,
            SessionId::new("sess-load".to_string()),
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        let resume_req = build_resume_session_request(
            AgentType::ClaudeCode,
            SessionId::new("sess-resume".to_string()),
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();

        let expected = serde_json::json!(["Agent", "Task"]);
        for (label, meta) in [
            ("new", new_req.meta.as_ref()),
            ("load", load_req.meta.as_ref()),
            ("resume", resume_req.meta.as_ref()),
        ] {
            let tools = meta
                .expect(label)
                .get("claudeCode")
                .and_then(|c| c.get("options"))
                .and_then(|o| o.get("disallowedTools"))
                .cloned()
                .expect("disallowedTools present");
            assert_eq!(tools, expected, "{label} deny list");
            assert_eq!(
                meta.unwrap()
                    .get("claudeCode")
                    .and_then(|c| c.get("emitRawSDKMessages"))
                    .and_then(|v| v.as_bool()),
                Some(true),
                "{label} emitRawSDKMessages"
            );
            assert!(
                meta.unwrap().contains_key("codeg.dev/terminal"),
                "{label} terminal meta"
            );
        }
    }

    /// Grok session/new|load|resume must stamp `_meta.mcpConfig.codeg-mcp`
    /// with the classified timeout map (design 2026-07-30).
    #[test]
    fn session_request_meta_grok_codeg_mcp_timeouts_on_new_load_resume() {
        let plan = codeg_plan(AgentType::Grok);
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let spec = test_posix_spec();
        let adapter = adapter_for(AgentType::Grok);

        let new_req = build_new_session_request(
            AgentType::Grok,
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        let load_req = build_load_session_request(
            AgentType::Grok,
            SessionId::new("sess-load".to_string()),
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        let resume_req = build_resume_session_request(
            AgentType::Grok,
            SessionId::new("sess-resume".to_string()),
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();

        let expected_timeouts = grok_codeg_mcp_timeout_config();
        for (label, meta) in [
            ("new", new_req.meta.as_ref()),
            ("load", load_req.meta.as_ref()),
            ("resume", resume_req.meta.as_ref()),
        ] {
            let meta = meta.expect(label);
            let codeg = meta
                .get("mcpConfig")
                .and_then(|c| c.get("codeg-mcp"))
                .cloned()
                .unwrap_or_else(|| panic!("{label}: missing mcpConfig.codeg-mcp"));
            assert_eq!(codeg, expected_timeouts, "{label} timeout config");
            assert_eq!(codeg["startupTimeoutMs"], 30_000);
            assert_eq!(codeg["toolTimeoutMs"], 30_000);
            let map = codeg["toolTimeoutsMs"].as_object().expect("toolTimeoutsMs");
            assert_eq!(map["get_workflow_capabilities"], 5_000);
            assert_eq!(map["check_user_feedback"], 10_000);
            assert_eq!(map["get_session_info"], 15_000);
            assert_eq!(map["get_workflow_state"], 15_000);
            assert_eq!(map["cancel_delegation"], 15_000);
            assert_eq!(map["reply_to_delegation"], 15_000);
            assert_eq!(map["publish_workflow_manifest"], 30_000);
            assert_eq!(map["settle_workflow_gate"], 30_000);
            assert_eq!(map["delegate_to_agent"], 180_000); // 3 min
            assert_eq!(map["continue_delegation"], 300_000); // 5 min
            assert_eq!(map["ask_user_question"], 1_800_000); // 30 min
            assert_eq!(map["request_parent_decision"], 1_800_000); // 30 min
            assert_eq!(map["get_delegation_status"], 5_400_000); // 90 min
                                                                 // Terminal + route profiles must survive the merge.
            assert!(
                meta.contains_key("codeg.dev/terminal"),
                "{label} terminal meta preserved"
            );
            assert!(
                meta.get("agentProfile").is_some(),
                "{label} Grok Codeg route agentProfile preserved"
            );
        }
    }

    #[test]
    fn session_request_meta_non_grok_omits_codeg_mcp_timeouts() {
        let spec = test_posix_spec();
        for agent in [
            AgentType::ClaudeCode,
            AgentType::Codex,
            AgentType::CodeBuddy,
        ] {
            let meta = session_request_meta(
                agent,
                &codeg_plan(agent),
                &spec,
                adapter_for(agent),
                ConnectionPurpose::User,
            )
            .unwrap();
            assert!(
                meta.get("mcpConfig").is_none(),
                "{agent:?} must not emit Grok-only mcpConfig timeouts"
            );
        }
    }

    #[test]
    fn merge_grok_codeg_mcp_timeout_preserves_other_server_entries() {
        let mut existing = serde_json::Map::new();
        existing.insert(
            "mcpConfig".to_string(),
            serde_json::json!({
                "github": { "toolTimeoutMs": 60_000 },
                "codeg-mcp": { "toolTimeoutMs": 1 } // should be replaced
            }),
        );
        existing.insert(
            "agentProfile".to_string(),
            serde_json::json!({"name": "keep-me"}),
        );
        let merged = merge_grok_codeg_mcp_timeout_config(existing, AgentType::Grok);
        assert_eq!(merged["agentProfile"]["name"], "keep-me");
        assert_eq!(merged["mcpConfig"]["github"]["toolTimeoutMs"], 60_000);
        assert_eq!(
            merged["mcpConfig"]["codeg-mcp"],
            grok_codeg_mcp_timeout_config()
        );
    }

    #[test]
    fn merge_grok_codeg_mcp_timeout_is_noop_for_non_grok() {
        let mut existing = serde_json::Map::new();
        existing.insert("keep".to_string(), serde_json::json!(true));
        let merged = merge_grok_codeg_mcp_timeout_config(existing, AgentType::Codex);
        assert_eq!(merged.get("mcpConfig"), None);
        assert_eq!(merged["keep"], true);
    }

    /// Complete base argv (as production builds before `AcpAgent::from_args`)
    /// must receive structured route insertion via `apply_process_route` only:
    /// Grok `--no-subagents` before `agent stdio`, CodeBuddy deny union before
    /// `--acp`. Second application is a no-op (idempotent).
    #[test]
    fn apply_process_route_on_complete_argv_is_ordered_and_idempotent() {
        // Grok: base root flags + subcommand already present (single application point).
        let mut grok_argv = vec![
            "grok".to_string(),
            "--no-auto-update".to_string(),
            "agent".to_string(),
            "stdio".to_string(),
        ];
        let mut env = BTreeMap::new();
        apply_process_route(
            &codeg_plan(AgentType::Grok),
            AgentType::Grok,
            &mut env,
            &mut grok_argv,
        )
        .unwrap();
        assert_eq!(
            grok_argv,
            vec![
                "grok",
                "--no-auto-update",
                "--no-subagents",
                "agent",
                "stdio",
            ]
        );
        let grok_once = grok_argv.clone();
        apply_process_route(
            &codeg_plan(AgentType::Grok),
            AgentType::Grok,
            &mut env,
            &mut grok_argv,
        )
        .unwrap();
        assert_eq!(
            grok_argv, grok_once,
            "Grok apply_process_route is idempotent"
        );

        // Permission mode stays before route suppression and the subcommand.
        let mut grok_approve = vec![
            "grok".to_string(),
            "--no-auto-update".to_string(),
            "--permission-mode".to_string(),
            "bypassPermissions".to_string(),
            "agent".to_string(),
            "stdio".to_string(),
        ];
        apply_process_route(
            &codeg_plan(AgentType::Grok),
            AgentType::Grok,
            &mut BTreeMap::new(),
            &mut grok_approve,
        )
        .unwrap();
        assert_eq!(
            grok_approve,
            vec![
                "grok",
                "--no-auto-update",
                "--permission-mode",
                "bypassPermissions",
                "--no-subagents",
                "agent",
                "stdio",
            ]
        );

        // CodeBuddy: complete argv already includes --acp; union before it.
        let mut cb_argv = vec!["codebuddy".to_string(), "--acp".to_string()];
        apply_process_route(
            &codeg_plan(AgentType::CodeBuddy),
            AgentType::CodeBuddy,
            &mut BTreeMap::new(),
            &mut cb_argv,
        )
        .unwrap();
        assert_eq!(
            cb_argv,
            vec!["codebuddy", "--disallowedTools", "Agent", "Task", "--acp"]
        );
        let cb_once = cb_argv.clone();
        apply_process_route(
            &codeg_plan(AgentType::CodeBuddy),
            AgentType::CodeBuddy,
            &mut BTreeMap::new(),
            &mut cb_argv,
        )
        .unwrap();
        assert_eq!(
            cb_argv, cb_once,
            "CodeBuddy apply_process_route is idempotent"
        );

        // Pre-existing denies union without reordering unrelated tokens.
        let mut cb_union = vec![
            "codebuddy".to_string(),
            "--disallowedTools".to_string(),
            "Bash".to_string(),
            "TaskOutput".to_string(),
            "Task".to_string(),
            "TaskStop".to_string(),
            "--acp".to_string(),
        ];
        apply_process_route(
            &codeg_plan(AgentType::CodeBuddy),
            AgentType::CodeBuddy,
            &mut BTreeMap::new(),
            &mut cb_union,
        )
        .unwrap();
        assert_eq!(
            cb_union,
            vec![
                "codebuddy",
                "--disallowedTools",
                "Bash",
                "TaskOutput",
                "Task",
                "TaskStop",
                "Agent",
                "--acp",
            ]
        );
        let cb_union_once = cb_union.clone();
        apply_process_route(
            &codeg_plan(AgentType::CodeBuddy),
            AgentType::CodeBuddy,
            &mut BTreeMap::new(),
            &mut cb_union,
        )
        .unwrap();
        assert_eq!(cb_union, cb_union_once);
    }

    /// Native CodeBuddy must be a strict no-op on the complete argv, including
    /// pre-seeded `--disallowedTools` in any supported position.
    #[test]
    fn native_codebuddy_preserves_preseeded_disallowed_tools() {
        let cases = [
            vec![
                "codebuddy".to_string(),
                "--disallowedTools".to_string(),
                "Bash".to_string(),
                "TaskOutput".to_string(),
                "TaskStop".to_string(),
                "--acp".to_string(),
            ],
            vec![
                "codebuddy".to_string(),
                "--acp".to_string(),
                "--disallowedTools".to_string(),
                "Bash".to_string(),
                "TaskOutput".to_string(),
                "TaskStop".to_string(),
            ],
            vec![
                "codebuddy".to_string(),
                "--some-other-flag".to_string(),
                "--disallowedTools".to_string(),
                "Bash".to_string(),
                "TaskOutput".to_string(),
                "TaskStop".to_string(),
                "--acp".to_string(),
            ],
        ];
        for original in cases {
            let mut argv = original.clone();
            apply_process_route(
                &native_plan(AgentType::CodeBuddy),
                AgentType::CodeBuddy,
                &mut BTreeMap::new(),
                &mut argv,
            )
            .unwrap();
            assert_eq!(
                argv, original,
                "native CodeBuddy must not mutate preseeded denies"
            );
        }
    }

    /// Fork must re-assert the same Claude Codeg Agent/Task deny list as
    /// new/load/resume (via `session_request_meta` / deep merge). Native is
    /// unchanged (no Codeg deny injection).
    #[test]
    fn claude_fork_meta_reasserts_codeg_agent_task_deny() {
        let spec = test_posix_spec();
        let adapter = adapter_for(AgentType::ClaudeCode);

        let codeg_meta = session_request_meta(
            AgentType::ClaudeCode,
            &codeg_plan(AgentType::ClaudeCode),
            &spec,
            adapter,
            ConnectionPurpose::User,
        )
        .unwrap();
        assert_eq!(
            codeg_meta
                .get("claudeCode")
                .and_then(|c| c.get("options"))
                .and_then(|o| o.get("disallowedTools"))
                .cloned()
                .expect("Codeg fork meta must include disallowedTools"),
            serde_json::json!(["Agent", "Task"])
        );
        assert_eq!(
            codeg_meta
                .get("claudeCode")
                .and_then(|c| c.get("emitRawSDKMessages"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(
            codeg_meta.contains_key("codeg.dev/terminal"),
            "fork meta preserves terminal snapshot"
        );
        // Adapter keys from terminal_metadata path stay intact (no clobber).
        let fork_req = crate::acp::fork::build_fork_session_request(
            SessionId::new("s-fork-route"),
            "/tmp/codeg",
            codeg_meta.clone(),
        );
        let fork_val = serde_json::to_value(fork_req).unwrap();
        assert_eq!(
            fork_val["_meta"]["claudeCode"]["options"]["disallowedTools"],
            serde_json::json!(["Agent", "Task"])
        );
        assert!(fork_val["_meta"].get("codeg.dev/terminal").is_some());

        let native_meta = session_request_meta(
            AgentType::ClaudeCode,
            &native_plan(AgentType::ClaudeCode),
            &spec,
            adapter,
            ConnectionPurpose::User,
        )
        .unwrap();
        assert!(
            native_meta
                .get("claudeCode")
                .and_then(|c| c.get("options"))
                .and_then(|o| o.get("disallowedTools"))
                .is_none(),
            "native Claude fork meta must not inject Codeg denies"
        );
        assert_eq!(
            native_meta
                .get("claudeCode")
                .and_then(|c| c.get("emitRawSDKMessages"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(native_meta.contains_key("codeg.dev/terminal"));
    }

    #[tokio::test]
    async fn cancelled_permission_ids_emit_resolution_events() {
        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-permissions".to_string(),
            AgentType::ClaudeCode,
            None,
            "win".to_string(),
            None,
        )));
        let emitter = EventEmitter::Noop;

        emit_cancelled_permission_events(
            &state,
            &emitter,
            vec!["p-1".to_string(), "p-2".to_string()],
        )
        .await;

        let guard = state.read().await;
        let resolved = guard
            .recent_events_after(0)
            .expect("events recorded")
            .iter()
            .filter_map(|event| match &event.payload {
                AcpEvent::PermissionResolved { request_id } => Some(request_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(resolved, vec!["p-1", "p-2"]);
    }

    /// Mirrors the connection-loop prompt path: ledger/UI use original user
    /// blocks; only the wire `ContentBlock` list receives `append_once`.
    #[test]
    fn wire_prompt_gets_context_user_message_does_not() {
        let injector = TerminalPromptContext::new(test_pwsh_spec());
        let user_blocks = vec![PromptInputBlock::Text {
            text: "hello".into(),
        }];
        let ui = crate::acp::user_blocks_from_prompt(&user_blocks);
        let mut wire = map_prompt_blocks(user_blocks);
        injector.append_once(&mut wire);

        assert_eq!(ui.len(), 1);
        match &ui[0] {
            UserMessageBlock::Text { text } => {
                assert_eq!(text, "hello");
                assert!(!text.contains("codeg_terminal_context"));
            }
            other => panic!("expected text user block, got {other:?}"),
        }
        assert_eq!(wire.len(), 2);
        match &wire[0] {
            ContentBlock::Text(t) => assert_eq!(t.text, "hello"),
            other => panic!("expected user text first, got {other:?}"),
        }
        match &wire[1] {
            ContentBlock::Text(t) => {
                assert!(t.text.contains("<codeg_terminal_context version=\"1\">"));
            }
            other => panic!("expected context text second, got {other:?}"),
        }
    }

    /// Clone a `_meta` map out of a JSON object literal, mirroring how codex-acp
    /// ships tool-call / session-info `_meta`.
    fn meta_map(v: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        v.as_object().expect("object").clone()
    }

    #[test]
    fn codex_subagent_activity_detected_only_for_codex_subagent_meta() {
        // codex-acp #304: `_meta.codex.subagent` marks the suppressed activity.
        let sub = meta_map(serde_json::json!({
            "codex": { "subagent": { "threadId": "t1", "path": "/root/x", "activity": "started" } }
        }));
        assert!(is_codex_subagent_activity(AgentType::Codex, Some(&sub)));
        // Only Codex is gated — the same meta never suppresses another agent.
        assert!(!is_codex_subagent_activity(
            AgentType::ClaudeCode,
            Some(&sub)
        ));
        // Absent meta and sibling codex meta keys (goal / collaboration) are not
        // subagent activity and must render normally.
        assert!(!is_codex_subagent_activity(AgentType::Codex, None));
        let goal = meta_map(serde_json::json!({ "codex": { "goal": { "objective": "x" } } }));
        assert!(!is_codex_subagent_activity(AgentType::Codex, Some(&goal)));
        let collab = meta_map(serde_json::json!({
            "codex": { "collaboration": { "tool": "spawnAgent" } }
        }));
        assert!(!is_codex_subagent_activity(AgentType::Codex, Some(&collab)));
    }

    #[test]
    fn config_option_state_command_suppressed_only_for_codex_set_config_action() {
        // codex-acp #293: `/plan` is a config-option state toggle (rendered as the
        // `collaboration_mode` selector), not an invokable slash command.
        let plan = meta_map(serde_json::json!({
            "commandAction": {
                "kind": "setConfigOption",
                "configId": "collaboration_mode",
                "value": "plan",
                "resetValue": "default",
                "presentation": "state"
            }
        }));
        assert!(is_config_option_state_command(
            AgentType::Codex,
            Some(&plan)
        ));
        // Gated on Codex — the same meta never suppresses another agent's command.
        assert!(!is_config_option_state_command(
            AgentType::ClaudeCode,
            Some(&plan)
        ));
        // `/goal` uses a `prefixPrompt` action (takes an objective argument) → a
        // real command, kept.
        let goal = meta_map(serde_json::json!({
            "commandAction": { "kind": "prefixPrompt", "presentation": "state" }
        }));
        assert!(!is_config_option_state_command(
            AgentType::Codex,
            Some(&goal)
        ));
        // Ordinary commands (no `commandAction`) and absent meta are kept.
        assert!(!is_config_option_state_command(AgentType::Codex, None));
        let plain = meta_map(serde_json::json!({ "somethingElse": true }));
        assert!(!is_config_option_state_command(
            AgentType::Codex,
            Some(&plain)
        ));
    }

    #[test]
    fn goal_control_action_roundtrips_codex_wire_values() {
        // codex-acp #293: `_codex/session/goal_control` expects lowercase
        // "pause" / "clear" on the wire, and the same strings arrive from the
        // tauri command / HTTP endpoint — both directions must match exactly.
        assert_eq!(
            serde_json::to_value(GoalControlAction::Pause).unwrap(),
            serde_json::json!("pause")
        );
        assert_eq!(
            serde_json::to_value(GoalControlAction::Clear).unwrap(),
            serde_json::json!("clear")
        );
        assert_eq!(
            serde_json::from_value::<GoalControlAction>(serde_json::json!("pause")).unwrap(),
            GoalControlAction::Pause
        );
        assert_eq!(
            serde_json::from_value::<GoalControlAction>(serde_json::json!("clear")).unwrap(),
            GoalControlAction::Clear
        );
    }

    #[test]
    fn codex_retry_indicator_extracts_message_and_object_http_status() {
        // codex-acp #289: object-variant `codexErrorInfo` carries an inner
        // `httpStatusCode`; the message + status are surfaced.
        let m = meta_map(serde_json::json!({
            "codex": { "error": {
                "message": "Reconnecting after provider returned 401",
                "codexErrorInfo": { "responseStreamDisconnected": { "httpStatusCode": 401 } },
                "additionalDetails": "HTTP status 401",
                "turnId": "turn-id",
                "willRetry": true
            } }
        }));
        assert_eq!(
            codex_retry_indicator(Some(&m)),
            Some((
                "Reconnecting after provider returned 401".to_string(),
                Some(401)
            ))
        );
    }

    #[test]
    fn codex_retry_indicator_string_enum_yields_no_status() {
        // A bare string `codexErrorInfo` yields the message but no http status.
        let m = meta_map(serde_json::json!({
            "codex": { "error": {
                "message": "Server overloaded",
                "codexErrorInfo": "serverOverloaded",
                "willRetry": true
            } }
        }));
        assert_eq!(
            codex_retry_indicator(Some(&m)),
            Some(("Server overloaded".to_string(), None))
        );
    }

    #[test]
    fn codex_retry_indicator_refuses_terminal_empty_and_absent() {
        // `willRetry: false` (e.g. 401 auth) must never render a retry banner.
        let terminal = meta_map(serde_json::json!({
            "codex": { "error": { "message": "unauthorized", "willRetry": false } }
        }));
        assert_eq!(codex_retry_indicator(Some(&terminal)), None);
        // Blank/whitespace message → nothing to show.
        let blank = meta_map(serde_json::json!({
            "codex": { "error": { "message": "   ", "willRetry": true } }
        }));
        assert_eq!(codex_retry_indicator(Some(&blank)), None);
        // No `codex.error` at all (a goal-only or empty session_info_update).
        let goal_only = meta_map(serde_json::json!({ "codex": { "goal": null } }));
        assert_eq!(codex_retry_indicator(Some(&goal_only)), None);
        assert_eq!(codex_retry_indicator(None), None);
    }

    #[test]
    fn classify_load_failure_resource_not_found_maps_to_code() {
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::ResourceNotFound,
                "session abc not found",
            ),
            Some("resource_not_found"),
        );
        // The structured -32002 code takes precedence even when the message
        // would otherwise match the crash/ended family.
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::ResourceNotFound,
                "process exited with code 1",
            ),
            Some("resource_not_found"),
        );
    }

    #[test]
    fn session_load_error_action_resume_existing_refuses_before_resource_not_found() {
        use crate::acp::session_attach::SessionAttachMode;

        // Regression probe: ResourceNotFound classifies on Default, but under
        // ResumeExistingOnly the shared helper must still refuse bootstrap.
        // If order is inverted (classify first), this fails — and so would the
        // dual-error harness that calls the same helper.
        assert_eq!(
            classify_session_load_failure(sacp::schema::ErrorCode::ResourceNotFound, "load failed",),
            Some("resource_not_found"),
        );
        assert_eq!(
            session_load_error_action(
                SessionAttachMode::ResumeExistingOnly,
                sacp::schema::ErrorCode::ResourceNotFound,
                "load failed",
            ),
            SessionLoadErrorAction::RefuseUnresumableBootstrap,
        );
        assert_eq!(
            session_load_error_action(
                SessionAttachMode::ResumeExistingOnly,
                sacp::schema::ErrorCode::InternalError,
                "session/load blew up",
            ),
            SessionLoadErrorAction::RefuseUnresumableBootstrap,
        );
        assert_eq!(
            session_load_error_action(
                SessionAttachMode::Default,
                sacp::schema::ErrorCode::ResourceNotFound,
                "load failed",
            ),
            SessionLoadErrorAction::SurfaceClassifiedLoadFailed {
                code: "resource_not_found",
            },
        );
        assert_eq!(
            session_load_error_action(
                SessionAttachMode::Default,
                sacp::schema::ErrorCode::MethodNotFound,
                "Method not found",
            ),
            SessionLoadErrorAction::ContinueDefaultFallthrough,
        );
    }

    #[test]
    fn classify_load_failure_legacy_codex_cli_session_requires_new_session() {
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::ResourceNotFound,
                "This Codex session was created by the legacy CLI runtime and cannot be resumed. Create a new session.",
            ),
            Some("legacy_cli_session"),
        );
    }

    #[test]
    fn classify_load_failure_crash_and_ended_map_to_unavailable() {
        // The reported Claude 0.58.1 case: native CLI exits 1, wrapped as -32603.
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::InternalError,
                "Internal error: { \"details\": \"Claude Code process exited with code 1\" }",
            ),
            Some("session_unavailable"),
        );
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::InternalError,
                "The Claude Agent session has ended. Please start a new session.",
            ),
            Some("session_unavailable"),
        );
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::InternalError,
                "Session not found",
            ),
            Some("session_unavailable"),
        );
    }

    #[test]
    fn classify_load_failure_keeps_existing_behavior_for_recoverable_errors() {
        // "Method not found" (agent lacks resume) and "Authentication required"
        // must fall through to the existing session/new + silent-stop paths.
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::MethodNotFound,
                "Method not found",
            ),
            None,
        );
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::AuthRequired,
                "Authentication required",
            ),
            None,
        );
        // Any other internal error without a crash/ended signature stays a
        // session/new fallback.
        assert_eq!(
            classify_session_load_failure(
                sacp::schema::ErrorCode::InternalError,
                "some unrelated transient failure",
            ),
            None,
        );
    }

    #[test]
    fn agents_codeg_records_itself_absorb_a_forgotten_session() {
        // The reported case: a custom agent keeps sessions in memory, so every
        // restart makes session/load fail with "Session not found". codeg has
        // the turns in its own transcript, so it must recover silently instead
        // of blanking the conversation behind a load-failed banner.
        let custom = AgentType::custom("glm-acp-agent").expect("valid id");
        assert!(recovers_load_failure_locally(
            custom,
            Some("session_unavailable")
        ));
        assert!(recovers_load_failure_locally(
            custom,
            Some("resource_not_found")
        ));
        // An unexpected failure is not a "forgotten session" — keep the
        // existing emit-then-fall-back behaviour even for custom agents.
        assert!(!recovers_load_failure_locally(custom, None));

        // Built-ins read history back out of the agent's own store, so a
        // forgotten session really is gone and must still stop with the banner.
        for builtin in [
            AgentType::ClaudeCode,
            AgentType::Codex,
            AgentType::Gemini,
            AgentType::Cursor,
        ] {
            assert!(
                !recovers_load_failure_locally(builtin, Some("session_unavailable")),
                "{builtin:?} has no codeg-side transcript to fall back on"
            );
        }
    }

    #[test]
    fn cursor_env_policy_clears_inherited_creds_only_in_subscription() {
        let sub: BTreeMap<String, String> =
            [("CURSOR_AUTH_MODE".to_string(), "subscription".to_string())].into();

        // No configured creds → both injected empty (⇒ spawn strips inherited).
        let mut merged = vec![("PATH".to_string(), "/usr/bin".to_string())];
        apply_cursor_env_policy(&mut merged, &sub);
        assert!(merged
            .iter()
            .any(|(k, v)| k == "CURSOR_API_KEY" && v.is_empty()));
        assert!(merged
            .iter()
            .any(|(k, v)| k == "CURSOR_API_BASE_URL" && v.is_empty()));

        // Stale configured/inherited creds are replaced with spawn-time removal
        // markers so cursor-agent can fall back to the browser login.
        let mut with_creds = vec![
            ("CURSOR_API_KEY".to_string(), "sk-x".to_string()),
            (
                "CURSOR_API_BASE_URL".to_string(),
                "https://cursor.example.test".to_string(),
            ),
        ];
        apply_cursor_env_policy(&mut with_creds, &sub);
        for key in ["CURSOR_API_KEY", "CURSOR_API_BASE_URL"] {
            assert_eq!(
                with_creds
                    .iter()
                    .filter(|(candidate, _)| candidate == key)
                    .map(|(_, value)| value.as_str())
                    .collect::<Vec<_>>(),
                vec![""]
            );
        }

        // Custom mode and legacy/no-mode rows are left untouched.
        for mode in [Some("custom"), None] {
            let rt: BTreeMap<String, String> = mode
                .map(|m| [("CURSOR_AUTH_MODE".to_string(), m.to_string())].into())
                .unwrap_or_default();
            let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
            apply_cursor_env_policy(&mut env, &rt);
            assert!(!env.iter().any(|(k, _)| k == "CURSOR_API_KEY"));
            assert!(!env.iter().any(|(k, _)| k == "CURSOR_API_BASE_URL"));
        }
    }

    #[test]
    fn grok_ask_tool_tracking_is_released_on_terminal_frame() {
        let meta_value = serde_json::json!({"x.ai/tool": {"kind": "ask_user"}});
        let meta = meta_value.as_object().expect("object meta");
        let mut tracked = HashSet::new();

        assert!(suppress_grok_ask_tool_frame(
            AgentType::Grok,
            Some(meta),
            "ask-1",
            Some("in_progress"),
            &mut tracked,
        ));
        assert!(tracked.contains("ask-1"));
        assert!(suppress_grok_ask_tool_frame(
            AgentType::Grok,
            None,
            "ask-1",
            Some("completed"),
            &mut tracked,
        ));
        assert!(
            !tracked.contains("ask-1"),
            "terminal ask ids must not accumulate for the connection lifetime"
        );
    }

    #[test]
    fn grok_env_policy_clears_inherited_key_only_in_subscription() {
        let sub: BTreeMap<String, String> =
            [("GROK_AUTH_MODE".to_string(), "subscription".to_string())].into();

        // Subscription with no configured key → inject empty (⇒ spawn strips the
        // inherited XAI_API_KEY so `grok login` is used).
        let mut merged = vec![("PATH".to_string(), "/usr/bin".to_string())];
        apply_grok_env_policy(&mut merged, &sub);
        assert!(merged
            .iter()
            .any(|(k, v)| k == "XAI_API_KEY" && v.is_empty()));

        // Subscription mode always wins over a stale configured key.
        let mut with_key = vec![("XAI_API_KEY".to_string(), "xai-abc".to_string())];
        apply_grok_env_policy(&mut with_key, &sub);
        assert!(with_key
            .iter()
            .any(|(k, v)| k == "XAI_API_KEY" && v.is_empty()));

        // api_key mode and legacy/no-mode rows are left untouched.
        for mode in [Some("api_key"), None] {
            let rt: BTreeMap<String, String> = mode
                .map(|m| [("GROK_AUTH_MODE".to_string(), m.to_string())].into())
                .unwrap_or_default();
            let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
            apply_grok_env_policy(&mut env, &rt);
            assert!(!env.iter().any(|(k, _)| k == "XAI_API_KEY"));
        }
    }

    #[test]
    fn grok_npx_launch_env_policy_clears_inherited_key_for_subscription() {
        let runtime_env: BTreeMap<String, String> =
            [("GROK_AUTH_MODE".to_string(), "subscription".to_string())].into();
        let mut merged_env = merge_agent_env(&[], &runtime_env);

        apply_npx_launch_env_policy(AgentType::Grok, &mut merged_env, &runtime_env);

        assert!(merged_env
            .iter()
            .any(|(key, value)| key == "XAI_API_KEY" && value.is_empty()));
    }

    #[test]
    fn grok_npx_launch_env_policy_removes_explicit_key_for_subscription() {
        let runtime_env: BTreeMap<String, String> = [
            ("GROK_AUTH_MODE".to_string(), "subscription".to_string()),
            ("XAI_API_KEY".to_string(), "xai-stale-key".to_string()),
        ]
        .into();
        let mut merged_env = merge_agent_env(&[], &runtime_env);

        apply_npx_launch_env_policy(AgentType::Grok, &mut merged_env, &runtime_env);

        assert_eq!(
            merged_env
                .iter()
                .find(|(key, _)| key == "XAI_API_KEY")
                .map(|(_, value)| value.as_str()),
            Some(""),
            "subscription mode must remove even a stale configured API key"
        );
    }

    #[test]
    fn grok_env_policy_windows_removes_case_variant_keys() {
        let runtime_env: BTreeMap<String, String> = [
            ("grok_auth_mode".to_string(), "subscription".to_string()),
            ("xai_api_key".to_string(), "xai-stale-key".to_string()),
        ]
        .into();
        let mut merged_env = merge_agent_env(&[], &runtime_env);

        apply_grok_env_policy_with_platform(&mut merged_env, &runtime_env, true);

        let matching: Vec<_> = merged_env
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case("XAI_API_KEY"))
            .collect();
        assert_eq!(matching.len(), 1, "Windows launch env must have one key");
        assert_eq!(matching[0].0, "XAI_API_KEY");
        assert!(matching[0].1.is_empty());
    }

    #[test]
    fn codex_env_policy_forces_mcp_filter_off_and_overrides_user_twin() {
        // Codex gets the flag injected so codex-acp never drops the injected
        // `codeg-mcp` server on a config.toml name collision.
        let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
        apply_codex_env_policy(AgentType::Codex, &mut env);
        assert!(env
            .iter()
            .any(|(k, v)| k == "DISABLE_MCP_CONFIG_FILTERING" && v == "true"));

        // A user-supplied twin is replaced (not duplicated) so the override wins.
        let mut with_twin = vec![(
            "DISABLE_MCP_CONFIG_FILTERING".to_string(),
            "false".to_string(),
        )];
        apply_codex_env_policy(AgentType::Codex, &mut with_twin);
        let hits: Vec<_> = with_twin
            .iter()
            .filter(|(k, _)| k == "DISABLE_MCP_CONFIG_FILTERING")
            .collect();
        assert_eq!(hits.len(), 1, "no duplicate key");
        assert_eq!(hits[0].1, "true", "codeg override wins over user twin");
    }

    #[test]
    fn codex_env_policy_is_noop_for_other_agents() {
        for agent in [AgentType::Grok, AgentType::ClaudeCode, AgentType::Gemini] {
            let mut env = vec![("PATH".to_string(), "/usr/bin".to_string())];
            apply_codex_env_policy(agent, &mut env);
            assert!(
                !env.iter().any(|(k, _)| k == "DISABLE_MCP_CONFIG_FILTERING"),
                "{agent:?} must not receive the codex-only flag"
            );
        }
    }

    #[test]
    fn synthesize_edit_single_diff_makes_canonical_edit() {
        let content = vec![diff_content("/a.rs", Some("old line\n"), "new line\n")];
        let json = synthesize_edit_input_from_diffs(&content).expect("one diff -> canonical edit");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["file_path"], "/a.rs");
        assert_eq!(v["old_string"], "old line\n");
        assert_eq!(v["new_string"], "new line\n");
        // Classifies as "edit" on the frontend via old_string/new_string.
        assert!(v.get("changes").is_none());
    }

    #[test]
    fn synthesize_edit_new_file_uses_write_shape() {
        // codex-acp sends old_text=None for new files. Encode that as a write-
        // shaped input (`{file_path, content}`) so the frontend classifies it as
        // a creation (`inferFromInput` → "write" → `--- /dev/null` diff), not a
        // modification. Edit-shaped keys must be absent, or `inferFromInput`
        // would route it back to "edit".
        let content = vec![diff_content("/new.rs", None, "fn main() {}\n")];
        let json = synthesize_edit_input_from_diffs(&content).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["file_path"], "/new.rs");
        assert_eq!(v["content"], "fn main() {}\n");
        assert!(v.get("old_string").is_none());
        assert!(v.get("new_string").is_none());
    }

    #[test]
    fn build_new_file_diff_matches_frontend_write_builder() {
        // Format parity with session-files.ts's `write` diff builder: a
        // `--- /dev/null` header (so `isAddedFileDiff` fires) then every
        // `split("\n")` segment — including the trailing empty one — as a `+`
        // line, with `+1,N` counting those segments.
        assert_eq!(
            build_new_file_diff("src/x.rs", "a\nb\n"),
            "--- /dev/null\n+++ b/src/x.rs\n@@ -0,0 +1,3 @@\n+a\n+b\n+"
        );
    }

    #[test]
    fn synthesize_edit_multi_diff_makes_changes_map() {
        let content = vec![
            diff_content("/a.rs", Some("a-old"), "a-new"),
            diff_content("/b.rs", None, "b-new"),
        ];
        let json = synthesize_edit_input_from_diffs(&content).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // Object map keyed by path — the shape extractEditChangesPayload reads.
        // /a.rs is an edit → old/new text for the frontend's generateUnifiedDiff.
        assert_eq!(v["changes"]["/a.rs"]["old_text"], "a-old");
        assert_eq!(v["changes"]["/a.rs"]["new_text"], "a-new");
        // /b.rs is a new file (old_text=None) → a ready-made creation diff whose
        // `--- /dev/null` header makes `isAddedFileDiff` classify it as new;
        // it must NOT carry old_text/new_text (that path builds a `--- a/…`
        // modification diff instead).
        let b_diff = v["changes"]["/b.rs"]["diff"]
            .as_str()
            .expect("new-file entry carries a prebuilt diff");
        assert!(b_diff.contains("--- /dev/null"));
        assert!(b_diff.contains("+b-new"));
        assert!(v["changes"]["/b.rs"].get("old_text").is_none());
        assert!(v["changes"]["/b.rs"].get("new_text").is_none());
    }

    #[test]
    fn synthesize_edit_returns_none_without_diff() {
        // No Diff block -> None, so callers keep the agent's own raw_input.
        assert!(synthesize_edit_input_from_diffs(&[]).is_none());
    }

    #[test]
    fn serialize_excludes_diffs_when_hoisted_to_raw_input() {
        let content = vec![diff_content("/a.rs", Some("old"), "new")];
        // Default keeps the diff (unchanged behavior for non-hoisted content).
        assert!(serialize_tool_call_content(&content, true)
            .unwrap()
            .contains("--- /a.rs"));
        // When the edit is hoisted into raw_input, the diff is dropped so it
        // isn't shipped twice and the header stats don't read the full-file blob.
        assert!(serialize_tool_call_content(&content, false).is_none());
    }

    #[test]
    fn pi_preflight_flags_missing_custom_command() {
        let mut env = BTreeMap::new();
        env.insert(
            "PI_ACP_PI_COMMAND".to_string(),
            "/nonexistent/definitely-not-pi-xyz".to_string(),
        );
        let msg =
            pi_launch_preflight(&env).expect("an unresolvable custom pi command must be flagged");
        // Frontend invariant: routes to the localized SDK-missing install prompt.
        assert!(msg.contains("is not installed"), "got: {msg}");
        assert!(msg.contains("definitely-not-pi-xyz"), "got: {msg}");
    }

    #[test]
    fn pi_preflight_accepts_resolvable_custom_command() {
        // A binary we know exists and is executable on this platform — proves the
        // preflight clears (returns None) for a resolvable PI_ACP_PI_COMMAND.
        let existing = if cfg!(windows) {
            "C:\\Windows\\System32\\cmd.exe"
        } else {
            "/bin/sh"
        };
        let mut env = BTreeMap::new();
        env.insert("PI_ACP_PI_COMMAND".to_string(), existing.to_string());
        assert!(pi_launch_preflight(&env).is_none());
    }

    #[test]
    fn prepend_path_unix_prepends_and_keeps_single_key() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "/usr/bin:/bin".to_string());
        prepend_dir_to_path_env(&mut env, "/home/u/.local/bin", "/fallback", false);
        assert_eq!(env.get("PATH").unwrap(), "/home/u/.local/bin:/usr/bin:/bin");
        assert_eq!(env.keys().filter(|k| k.as_str() == "PATH").count(), 1);
    }

    #[test]
    fn prepend_path_unix_seeds_from_fallback_when_absent() {
        let mut env = BTreeMap::new();
        prepend_dir_to_path_env(&mut env, "/x/bin", "/usr/bin:/bin", false);
        assert_eq!(env.get("PATH").unwrap(), "/x/bin:/usr/bin:/bin");
    }

    #[test]
    fn prepend_path_windows_is_case_insensitive_and_no_clobber() {
        // Regression for the `Path` vs `PATH` clobber: a pre-existing `Path`
        // must be reused (not joined by a second `PATH` key that a later
        // case-insensitive `Command::env` could overwrite).
        let mut env = BTreeMap::new();
        env.insert("Path".to_string(), r"C:\Windows".to_string());
        prepend_dir_to_path_env(
            &mut env,
            r"C:\Users\u\AppData\Local\OfficeCLI",
            "ignored-fallback",
            true,
        );
        // Exactly one PATH-ish key, the original casing preserved, value prepended.
        let path_keys: Vec<&String> = env
            .keys()
            .filter(|k| k.eq_ignore_ascii_case("PATH"))
            .collect();
        assert_eq!(path_keys.len(), 1, "{env:?}");
        assert_eq!(
            env.get("Path").unwrap(),
            r"C:\Users\u\AppData\Local\OfficeCLI;C:\Windows"
        );
    }

    #[test]
    fn prepend_path_windows_seeds_from_fallback_with_semicolon() {
        let mut env = BTreeMap::new();
        prepend_dir_to_path_env(
            &mut env,
            r"C:\OfficeCLI",
            r"C:\Windows;C:\Windows\System32",
            true,
        );
        // No prior key → default `Path` casing on Windows.
        assert_eq!(
            env.get("Path").unwrap(),
            r"C:\OfficeCLI;C:\Windows;C:\Windows\System32"
        );
    }

    #[test]
    fn prepend_path_windows_collapses_duplicate_casings() {
        // Pathological but possible: both `PATH` and `Path` present. All
        // PATH-ish keys must collapse to exactly one, prepended onto the
        // effective (last-applied → `Path`) value, so no stale duplicate can
        // overwrite the injected dir when the child Command applies env.
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), r"C:\a".to_string());
        env.insert("Path".to_string(), r"C:\b".to_string());
        prepend_dir_to_path_env(&mut env, r"C:\OfficeCLI", "ignored-fallback", true);
        let path_keys: Vec<&String> = env
            .keys()
            .filter(|k| k.eq_ignore_ascii_case("PATH"))
            .collect();
        assert_eq!(
            path_keys.len(),
            1,
            "exactly one PATH-ish key must remain: {env:?}"
        );
        assert_eq!(env.get("Path").unwrap(), r"C:\OfficeCLI;C:\b");
    }

    #[test]
    fn client_capabilities_gate_per_agent() {
        // Serialize to inspect the wire shape — `_meta` is the serde rename
        // and the exact key path the adapters read.
        let caps_of = |agent: AgentType| {
            serde_json::to_value(build_client_capabilities(agent)).expect("caps serialize")
        };

        // Claude Code: subagent-transcript opt-in (strict boolean true), and
        // no elicitation (which would un-gate AskUserQuestion duplication).
        let claude = caps_of(AgentType::ClaudeCode);
        assert_eq!(
            claude["_meta"]["subagent-transcript"],
            serde_json::Value::Bool(true)
        );
        assert!(claude.get("elicitation").is_none());

        // Codex: form elicitation, no subagent-transcript meta.
        let codex = caps_of(AgentType::Codex);
        assert!(codex.get("elicitation").is_some());
        assert!(codex.get("_meta").is_none());

        // Everyone else: neither gate; fs + terminal always advertised.
        let other = caps_of(AgentType::Gemini);
        assert!(other.get("_meta").is_none());
        assert!(other.get("elicitation").is_none());
        assert_eq!(other["terminal"], serde_json::Value::Bool(true));
        assert_eq!(other["fs"]["readTextFile"], serde_json::Value::Bool(true));
    }

    #[test]
    fn claude_raw_sdk_meta_enabled_only_for_claude() {
        let claude_meta = claude_raw_sdk_session_meta(AgentType::ClaudeCode)
            .expect("Claude must have raw SDK meta");
        assert_eq!(
            claude_meta
                .get("claudeCode")
                .and_then(|v| v.get("emitRawSDKMessages"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        assert!(claude_raw_sdk_session_meta(AgentType::Codex).is_none());
    }

    #[test]
    fn map_claude_sdk_ext_notification_maps_valid_payload() {
        let raw = UntypedMessage::new(
            "_claude/sdkMessage",
            serde_json::json!({
                "sessionId": "session-123",
                "message": {
                    "type": "system",
                    "subtype": "api_retry",
                    "attempt": 3,
                    "max_retries": 10
                }
            }),
        )
        .unwrap();

        let event = map_claude_sdk_ext_notification(&raw).expect("valid sdk payload should map");

        match event {
            AcpEvent::ClaudeSdkMessage {
                session_id,
                message,
            } => {
                // connection_id 不再属于 AcpEvent，envelope 上提到顶层
                assert_eq!(session_id, "session-123");
                assert_eq!(message.get("type").and_then(|v| v.as_str()), Some("system"));
            }
            _ => panic!("expected ClaudeSdkMessage"),
        }
    }

    #[test]
    fn rewrite_end_turn_if_empty_is_production_fn() {
        use crate::acp::delegation::types::ParentTurnEndReason;
        assert_eq!(rewrite_end_turn_if_empty("end_turn", false), "empty");
        assert_eq!(rewrite_end_turn_if_empty("end_turn", true), "end_turn");
        assert_eq!(
            parent_turn_end_reason(rewrite_end_turn_if_empty("end_turn", true)),
            ParentTurnEndReason::JoinAbandoned
        );
        assert_eq!(
            parent_turn_end_reason(rewrite_end_turn_if_empty("end_turn", false)),
            ParentTurnEndReason::ParentTurnFailed
        );
    }

    #[tokio::test]
    async fn maybe_emit_private_in_prompt_completed_returns_true_and_sets_usage() {
        use crate::acp::xai_session_notification::PrivateExtEmitMode;
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-test".into(),
            AgentType::Grok,
            None,
            "win-test".into(),
            None,
        )));
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::test_web_only(broadcaster);
        let raw = include_str!("fixtures/grok_auto_compact_completed.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let notif =
            UntypedMessage::new(v["method"].as_str().unwrap(), v["params"].clone()).unwrap();
        let dispatch = Dispatch::Notification(notif);
        let mut compact_flag = false;
        let agent_out = maybe_emit_private_ext_notification(
            &state,
            &emitter,
            dispatch,
            PrivateExtEmitMode::InPrompt,
            &mut compact_flag,
        )
        .await;
        assert!(agent_out, "ContentDelta must mark agent output");
        assert!(compact_flag);
        let used = state.read().await.usage.as_ref().map(|u| u.used);
        assert_eq!(used, Some(18060));
    }

    #[tokio::test]
    async fn maybe_emit_grok_total_tokens_usage_from_session_meta() {
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-test".into(),
            AgentType::Grok,
            None,
            "win-test".into(),
            None,
        )));
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::test_web_only(broadcaster);

        let mut meta = Meta::new();
        meta.insert("totalTokens".into(), serde_json::json!(12345));
        maybe_emit_grok_total_tokens_usage(&state, &emitter, AgentType::Grok, Some(&meta)).await;
        let usage = state.read().await.usage.clone().expect("usage set");
        assert_eq!(usage.used, 12_345);
        assert!(usage.size > 0, "size must resolve from model/config");

        // Non-Grok agents ignore totalTokens meta.
        let state_cc = Arc::new(RwLock::new(SessionState::new(
            "conn-cc".into(),
            AgentType::ClaudeCode,
            None,
            "win-test".into(),
            None,
        )));
        maybe_emit_grok_total_tokens_usage(&state_cc, &emitter, AgentType::ClaudeCode, Some(&meta))
            .await;
        assert!(state_cc.read().await.usage.is_none());

        // Zero totalTokens is a no-op (keeps prior usage).
        let mut zero = Meta::new();
        zero.insert("totalTokens".into(), serde_json::json!(0));
        maybe_emit_grok_total_tokens_usage(&state, &emitter, AgentType::Grok, Some(&zero)).await;
        assert_eq!(state.read().await.usage.as_ref().unwrap().used, 12_345);
    }

    #[tokio::test]
    async fn maybe_emit_private_load_drain_noop_no_usage() {
        use crate::acp::xai_session_notification::PrivateExtEmitMode;
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-test".into(),
            AgentType::Grok,
            None,
            "win-test".into(),
            None,
        )));
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::test_web_only(broadcaster);
        let raw = include_str!("fixtures/grok_auto_compact_completed.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let notif =
            UntypedMessage::new(v["method"].as_str().unwrap(), v["params"].clone()).unwrap();
        let mut compact_flag = false;
        let agent_out = maybe_emit_private_ext_notification(
            &state,
            &emitter,
            Dispatch::Notification(notif),
            PrivateExtEmitMode::LoadDrainNoop,
            &mut compact_flag,
        )
        .await;
        assert!(!agent_out);
        assert!(!compact_flag);
        assert!(state.read().await.usage.is_none());
    }

    #[tokio::test]
    async fn maybe_emit_private_idle_usage_only_returns_false() {
        use crate::acp::xai_session_notification::PrivateExtEmitMode;
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-test".into(),
            AgentType::Grok,
            None,
            "win-test".into(),
            None,
        )));
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let emitter = EventEmitter::test_web_only(broadcaster);
        let raw = include_str!("fixtures/grok_auto_compact_completed.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let notif =
            UntypedMessage::new(v["method"].as_str().unwrap(), v["params"].clone()).unwrap();
        let mut compact_flag = false;
        let agent_out = maybe_emit_private_ext_notification(
            &state,
            &emitter,
            Dispatch::Notification(notif),
            PrivateExtEmitMode::IdleUsageOnly,
            &mut compact_flag,
        )
        .await;
        assert!(!agent_out);
        assert!(!compact_flag);
        assert_eq!(
            state.read().await.usage.as_ref().map(|u| u.used),
            Some(18060)
        );
    }

    fn compact_completed_session_message() -> SessionMessage {
        let raw = include_str!("fixtures/grok_auto_compact_completed.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let notif =
            UntypedMessage::new(v["method"].as_str().unwrap(), v["params"].clone()).unwrap();
        SessionMessage::SessionMessage(Dispatch::Notification(notif))
    }

    fn grok_standard_dispatch(
        kind: &str,
        event_seq: u64,
        agent_timestamp_ms: u64,
        prompt_id: &str,
        stream_start_ms: u64,
        text: &str,
    ) -> Dispatch {
        let notif = UntypedMessage::new(
            "session/update",
            serde_json::json!({
                "sessionId": "s-test",
                "update": {
                    "sessionUpdate": kind,
                    "content": { "type": "text", "text": text }
                },
                "_meta": {
                    "eventId": format!("s-test-{event_seq}"),
                    "agentTimestampMs": agent_timestamp_ms,
                    "promptId": prompt_id,
                    "streamStartMs": stream_start_ms
                }
            }),
        )
        .expect("standard Grok notification");
        Dispatch::Notification(notif)
    }

    fn grok_retry_dispatch(event_seq: u64, agent_timestamp_ms: u64, attempt: u32) -> Dispatch {
        let notif = UntypedMessage::new(
            "_x.ai/session/update",
            serde_json::json!({
                "sessionId": "s-test",
                "update": {
                    "sessionUpdate": "retry_state",
                    "type": "retrying",
                    "attempt": attempt,
                    "max_retries": 15,
                    "reason": "provider unavailable"
                },
                "_meta": {
                    "eventId": format!("s-test-{event_seq}"),
                    "agentTimestampMs": agent_timestamp_ms
                }
            }),
        )
        .expect("Grok retry notification");
        Dispatch::Notification(notif)
    }

    fn raw_notification(dispatch: &Dispatch) -> &UntypedMessage {
        match dispatch {
            Dispatch::Notification(notification) => notification,
            _ => panic!("expected notification dispatch"),
        }
    }

    #[tokio::test]
    async fn grok_retry_main_path_rolls_back_and_drops_late_failed_output() {
        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-test".into(),
            AgentType::Grok,
            None,
            "win-test".into(),
            None,
        )));
        let mut reconciler = crate::acp::grok_retry::GrokRetryReconciler::default();
        let mut turn_had_output = false;

        let old_thought =
            grok_standard_dispatch("agent_thought_chunk", 21, 1_000, "prompt-1", 100, "old");
        assert!(
            !reconcile_grok_retry_dispatch(
                AgentType::Grok,
                raw_notification(&old_thought),
                &mut reconciler,
                &state,
                &EventEmitter::Noop,
                &mut turn_had_output,
            )
            .await
        );
        emit_with_state(
            &state,
            &EventEmitter::Noop,
            AcpEvent::Thinking {
                text: "old".into(),
                parent_tool_use_id: None,
            },
        )
        .await;
        turn_had_output = true;

        let retry = grok_retry_dispatch(32, 1_100, 1);
        assert!(
            reconcile_grok_retry_dispatch(
                AgentType::Grok,
                raw_notification(&retry),
                &mut reconciler,
                &state,
                &EventEmitter::Noop,
                &mut turn_had_output,
            )
            .await
        );
        assert!(!turn_had_output);

        let late_failed =
            grok_standard_dispatch("agent_message_chunk", 31, 1_100, "prompt-1", 100, "stale");
        assert!(
            reconcile_grok_retry_dispatch(
                AgentType::Grok,
                raw_notification(&late_failed),
                &mut reconciler,
                &state,
                &EventEmitter::Noop,
                &mut turn_had_output,
            )
            .await
        );

        let accepted = grok_standard_dispatch(
            "agent_message_chunk",
            61,
            2_100,
            "prompt-1",
            200,
            "accepted",
        );
        assert!(
            !reconcile_grok_retry_dispatch(
                AgentType::Grok,
                raw_notification(&accepted),
                &mut reconciler,
                &state,
                &EventEmitter::Noop,
                &mut turn_had_output,
            )
            .await
        );
        emit_with_state(
            &state,
            &EventEmitter::Noop,
            AcpEvent::ContentDelta {
                text: "accepted".into(),
                parent_tool_use_id: None,
            },
        )
        .await;
        emit_with_state(
            &state,
            &EventEmitter::Noop,
            AcpEvent::TurnComplete {
                session_id: "s-test".into(),
                stop_reason: "end_turn".into(),
                agent_type: "grok".into(),
                mark_awaiting_reply: false,

                termination_source: None,
                provider_turn_id: None,
            },
        )
        .await;

        let state = state.read().await;
        let events = state.recent_events_after(0).expect("contiguous events");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.payload, AcpEvent::TurnAttemptRollback { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.payload, AcpEvent::TurnComplete { .. }))
                .count(),
            1
        );
        let content: Vec<&str> = events
            .iter()
            .filter_map(|event| match &event.payload {
                AcpEvent::ContentDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(content, ["accepted"]);
    }

    #[tokio::test]
    async fn grok_retry_drain_reuses_reconciler_and_accepts_replacement_stream() {
        use crate::acp::terminal_adapter::adapter_for;
        use crate::acp::terminal_assoc::TerminalAssocFallback;
        use crate::acp::terminal_runtime::TerminalRuntime;
        use sacp::schema::SessionId;

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-test".into(),
            AgentType::Grok,
            None,
            "win-test".into(),
            None,
        )));
        let mut q = std::collections::VecDeque::from([
            SessionMessage::SessionMessage(grok_standard_dispatch(
                "agent_thought_chunk",
                21,
                1_000,
                "prompt-1",
                100,
                "old thought",
            )),
            SessionMessage::SessionMessage(grok_retry_dispatch(32, 1_100, 1)),
            SessionMessage::SessionMessage(grok_standard_dispatch(
                "agent_message_chunk",
                31,
                1_100,
                "prompt-1",
                100,
                "stale answer",
            )),
            SessionMessage::SessionMessage(grok_standard_dispatch(
                "agent_thought_chunk",
                51,
                2_000,
                "prompt-1",
                200,
                "accepted thought",
            )),
            SessionMessage::SessionMessage(grok_standard_dispatch(
                "agent_message_chunk",
                61,
                2_100,
                "prompt-1",
                200,
                "accepted answer",
            )),
        ]);
        let mut source = ReadyUpdateSource::Fake(&mut q);
        let mut reconciler = crate::acp::grok_retry::GrokRetryReconciler::default();
        let mut turn_had = false;
        let mut compact_flag = false;
        let mut tracked = HashMap::new();
        let mut raw_cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let terminal_runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Grok),
        ));
        let terminal_assoc = Arc::new(std::sync::Mutex::new(TerminalAssocFallback::new(false)));
        let sid = SessionId::new("s-test");

        drain_ready_in_prompt_updates(
            &mut source,
            &state,
            &EventEmitter::Noop,
            AgentType::Grok,
            &sid,
            ".",
            &terminal_runtime,
            &terminal_assoc,
            &mut tracked,
            &mut raw_cache,
            &mut cb,
            &mut reconciler,
            &mut turn_had,
            &mut compact_flag,
        )
        .await;

        assert!(q.is_empty());
        assert!(turn_had);
        let state = state.read().await;
        let blocks = &state.live_message.as_ref().expect("live message").content;
        assert_eq!(blocks.len(), 2);
        assert!(matches!(
            &blocks[0],
            crate::acp::session_state::LiveContentBlock::Thinking { text, .. }
                if text == "accepted thought"
        ));
        assert!(matches!(
            &blocks[1],
            crate::acp::session_state::LiveContentBlock::Text { text, .. }
                if text == "accepted answer"
        ));
    }

    #[tokio::test]
    async fn drain_with_fake_queue_sets_flag_before_rewrite() {
        use crate::acp::terminal_adapter::adapter_for;
        use crate::acp::terminal_assoc::TerminalAssocFallback;
        use crate::acp::terminal_runtime::TerminalRuntime;
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};
        use sacp::schema::SessionId;

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-test".into(),
            AgentType::Grok,
            None,
            "win-test".into(),
            None,
        )));
        let emitter = EventEmitter::test_web_only(Arc::new(WebEventBroadcaster::new()));
        let mut q = std::collections::VecDeque::new();
        q.push_back(compact_completed_session_message());
        let mut source = ReadyUpdateSource::Fake(&mut q);
        let mut turn_had = false;
        let mut compact_flag = false;
        let mut tracked = HashMap::new();
        let mut raw_cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let mut reconciler = GrokRetryReconciler::default();
        let terminal_runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Grok),
        ));
        let terminal_assoc = Arc::new(std::sync::Mutex::new(TerminalAssocFallback::new(false)));
        let sid = SessionId::new("s-test");

        drain_ready_in_prompt_updates(
            &mut source,
            &state,
            &emitter,
            AgentType::Grok,
            &sid,
            ".",
            &terminal_runtime,
            &terminal_assoc,
            &mut tracked,
            &mut raw_cache,
            &mut cb,
            &mut reconciler,
            &mut turn_had,
            &mut compact_flag,
        )
        .await;

        assert!(
            turn_had,
            "drain must set agent-output flag via private emit"
        );
        assert!(compact_flag);
        assert_eq!(rewrite_end_turn_if_empty("end_turn", turn_had), "end_turn");
        assert_eq!(
            state.read().await.usage.as_ref().map(|u| u.used),
            Some(18060)
        );
    }

    #[tokio::test]
    async fn drain_suppresses_secondary_terminal_continues_for_compact() {
        use crate::acp::terminal_adapter::adapter_for;
        use crate::acp::terminal_assoc::TerminalAssocFallback;
        use crate::acp::terminal_runtime::TerminalRuntime;
        use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};
        use sacp::schema::{SessionId, StopReason};

        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-test".into(),
            AgentType::Grok,
            None,
            "win-test".into(),
            None,
        )));
        let emitter = EventEmitter::test_web_only(Arc::new(WebEventBroadcaster::new()));
        let mut q = std::collections::VecDeque::new();
        // Secondary terminal first, then compact — drain must not finalize and
        // must still process compact.
        q.push_back(SessionMessage::StopReason(StopReason::EndTurn));
        q.push_back(compact_completed_session_message());
        let mut source = ReadyUpdateSource::Fake(&mut q);
        let mut turn_had = false;
        let mut compact_flag = false;
        let mut tracked = HashMap::new();
        let mut raw_cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let mut reconciler = GrokRetryReconciler::default();
        let terminal_runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Grok),
        ));
        let terminal_assoc = Arc::new(std::sync::Mutex::new(TerminalAssocFallback::new(false)));
        let sid = SessionId::new("s-test");

        drain_ready_in_prompt_updates(
            &mut source,
            &state,
            &emitter,
            AgentType::Grok,
            &sid,
            ".",
            &terminal_runtime,
            &terminal_assoc,
            &mut tracked,
            &mut raw_cache,
            &mut cb,
            &mut reconciler,
            &mut turn_had,
            &mut compact_flag,
        )
        .await;

        assert!(q.is_empty(), "drain must consume full fake queue");
        assert!(turn_had);
        assert_eq!(rewrite_end_turn_if_empty("end_turn", turn_had), "end_turn");
    }

    // --- Task 3: user_stop TurnComplete + active provider turn id lifecycle ---

    fn active_turn_id_session_message(turn_id: &str) -> SessionMessage {
        let notif = UntypedMessage::new(
            "session/update",
            serde_json::json!({
                "sessionId": "s-test",
                "update": {
                    "sessionUpdate": "session_info_update",
                    "_meta": {
                        "codex": {
                            "activeTurnId": turn_id
                        }
                    }
                }
            }),
        )
        .expect("activeTurnId session_info_update");
        SessionMessage::SessionMessage(Dispatch::Notification(notif))
    }

    fn user_stop_test_state(turn_in_flight: bool) -> Arc<RwLock<SessionState>> {
        let state = Arc::new(RwLock::new(SessionState::new(
            "conn-user-stop".into(),
            AgentType::Codex,
            None,
            "win-test".into(),
            None,
        )));
        if turn_in_flight {
            let mut s = state.try_write().expect("state lock");
            s.turn_in_flight = true;
            s.active_turn_generation = Some(1);
            s.parent_turn_generation = 1;
        }
        state
    }

    fn last_turn_complete(
        events: &[std::sync::Arc<crate::acp::types::EventEnvelope>],
    ) -> &AcpEvent {
        events
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                ev @ AcpEvent::TurnComplete { .. } => Some(ev),
                _ => None,
            })
            .expect("expected TurnComplete")
    }

    #[tokio::test]
    async fn user_cancel_sets_user_stop_and_forwards_provider_turn_id() {
        use crate::models::message::TurnTerminationSource;

        let state = user_stop_test_state(true);
        {
            let mut s = state.write().await;
            s.active_provider_turn_id = Some("turn-abc".into());
        }
        let mut slot = None;
        let disposition = finalize_turn_terminal(
            TurnTerminalSource::UserCancel,
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "conn-user-stop",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        assert!(matches!(
            disposition,
            TurnFinalizationDisposition::UserCancelled
        ));
        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        match last_turn_complete(&events) {
            AcpEvent::TurnComplete {
                stop_reason,
                termination_source,
                provider_turn_id,
                ..
            } => {
                assert_eq!(stop_reason, "cancelled");
                assert_eq!(*termination_source, Some(TurnTerminationSource::UserStop));
                assert_eq!(provider_turn_id.as_deref(), Some("turn-abc"));
            }
            _ => unreachable!(),
        }
        assert!(
            state.read().await.active_provider_turn_id.is_none(),
            "provider id must be cleared after UserCancelled snapshot"
        );
    }

    #[tokio::test]
    async fn user_cancel_without_provider_id_still_sets_user_stop() {
        use crate::models::message::TurnTerminationSource;

        let state = user_stop_test_state(true);
        let mut slot = None;
        let _ = finalize_turn_terminal(
            TurnTerminalSource::UserCancel,
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "conn-user-stop",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        match last_turn_complete(&events) {
            AcpEvent::TurnComplete {
                termination_source,
                provider_turn_id,
                ..
            } => {
                assert_eq!(*termination_source, Some(TurnTerminationSource::UserStop));
                assert!(provider_turn_id.is_none());
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn active_user_cancel_emits_turn_complete_before_terminal_release() {
        use crate::acp::terminal_adapter::adapter_for;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let state = user_stop_test_state(true);
        let (broker, spawner, task_id) = delegation_suspend_broker_with_running_child().await;
        let injection = delegation_suspend_injection(broker.clone());
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let mock_agent = SuspensionLoopMockAgent {
            prompts: Arc::new(std::sync::Mutex::new(Vec::new())),
            modes: Arc::new(std::sync::Mutex::new(Vec::new())),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: cancel_count.clone(),
        };
        let runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Codex),
        ));
        let state_for_cancel = state.clone();
        let runtime_for_cancel = runtime.clone();
        let cancel_count_for_cancel = cancel_count.clone();
        Client
            .builder()
            .connect_with(mock_agent, async move |cx| {
                let sid = SessionId::new("session-1".to_string());
                let mut suspension = None;
                let mut tracked = HashMap::new();
                let perms: PendingPermissions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
                finalize_active_user_cancel(
                    &cx,
                    &sid,
                    &mut suspension,
                    &state_for_cancel,
                    &EventEmitter::Noop,
                    "parent-conn",
                    AgentType::Codex,
                    false,
                    &mut tracked,
                    &perms,
                    &runtime_for_cancel,
                    Some(&injection),
                )
                .await;
                wait_for_suspension_loop_condition("active user cancel notification", || {
                    cancel_count_for_cancel.load(Ordering::SeqCst) == 1
                })
                .await;
                Ok(())
            })
            .await
            .expect("active user cancel mock connect");

        assert_eq!(cancel_count.load(Ordering::SeqCst), 1);
        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        assert!(matches!(
            last_turn_complete(&events),
            AcpEvent::TurnComplete { stop_reason, .. } if stop_reason == "cancelled"
        ));
        wait_for_suspension_loop_condition("user cancel delegation cascade", || {
            spawner
                .cancels
                .try_lock()
                .map(|cancels| cancels.as_slice() == ["child-conn"])
                .unwrap_or(false)
        })
        .await;
        assert!(matches!(
            delegation_suspend_task_status(&broker, &task_id).await,
            crate::acp::delegation::types::TaskStatus::Canceled
        ));
    }

    #[tokio::test]
    async fn active_cancel_clears_pending_permissions_before_release() {
        use crate::acp::terminal_adapter::adapter_for;

        let state = user_stop_test_state(true);
        emit_with_state(
            &state,
            &EventEmitter::Noop,
            AcpEvent::ToolCall {
                tool_call_id: "tool-1".into(),
                title: "long command".into(),
                kind: "execute".into(),
                status: "in_progress".into(),
                content: None,
                raw_input: None,
                raw_output: None,
                locations: None,
                meta: None,
                images: None,
            },
        )
        .await;
        emit_with_state(
            &state,
            &EventEmitter::Noop,
            AcpEvent::PermissionRequest {
                request_id: "permission-1".into(),
                tool_call: serde_json::json!({"toolCallId": "tool-1"}),
                options: Vec::new(),
            },
        )
        .await;

        let event_stream = state.read().await.event_stream();
        let mut events = event_stream.subscribe();
        let state_at_turn_complete = state.clone();
        let turn_complete_observer = tokio::spawn(async move {
            loop {
                let event = events.recv().await.expect("cancel event");
                if matches!(event.payload, AcpEvent::TurnComplete { .. }) {
                    let state = state_at_turn_complete.read().await;
                    return (
                        state.pending_permission.is_none(),
                        state.active_tool_calls.is_empty(),
                    );
                }
            }
        });

        let mock_agent = SuspensionLoopMockAgent {
            prompts: Arc::new(std::sync::Mutex::new(Vec::new())),
            modes: Arc::new(std::sync::Mutex::new(Vec::new())),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Codex),
        ));
        let state_for_cancel = state.clone();
        Client
            .builder()
            .connect_with(mock_agent, async move |cx| {
                let sid = SessionId::new("session-1".to_string());
                let mut suspension = None;
                let mut tracked =
                    HashMap::from([("tool-1".to_string(), TrackedTerminalToolCall::default())]);
                let perms: PendingPermissions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
                finalize_active_user_cancel(
                    &cx,
                    &sid,
                    &mut suspension,
                    &state_for_cancel,
                    &EventEmitter::Noop,
                    "conn-user-stop",
                    AgentType::Codex,
                    false,
                    &mut tracked,
                    &perms,
                    &runtime,
                    None,
                )
                .await;
                assert!(tracked.is_empty(), "tracked tools must clear on cancel");
                Ok(())
            })
            .await
            .expect("active cancel mock connect");

        assert_eq!(
            turn_complete_observer.await.expect("observer task"),
            (true, true),
            "TurnComplete must expose cleared permission/tool state"
        );
    }

    #[tokio::test]
    async fn turn_complete_precedes_release_with_outstanding_wait_for_exit() {
        use crate::acp::terminal_adapter::adapter_for;

        let state = user_stop_test_state(true);
        let event_stream = state.read().await.event_stream();
        let mut events = event_stream.subscribe();
        let runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Codex),
        ));
        let sid = SessionId::new("session-1".to_string());
        let command = if cfg!(windows) {
            "ping -t 127.0.0.1"
        } else {
            "sleep 30"
        };
        let created = runtime
            .create_terminal(CreateTerminalRequest::new(sid.clone(), command.to_string()))
            .await
            .expect("create long-running terminal");
        let waiter_runtime = runtime.clone();
        let waiter_sid = sid.clone();
        let waiter_terminal_id = created.terminal_id.clone();
        let mut waiter = tokio::spawn(async move {
            waiter_runtime
                .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                    waiter_sid,
                    waiter_terminal_id,
                ))
                .await
        });

        let ordering = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    event = events.recv() => {
                        if matches!(event.expect("cancel event").payload, AcpEvent::TurnComplete { .. }) {
                            return true;
                        }
                    }
                    result = &mut waiter => {
                        panic!("terminal release completed before TurnComplete: {result:?}");
                    }
                }
            }
        });

        let mock_agent = SuspensionLoopMockAgent {
            prompts: Arc::new(std::sync::Mutex::new(Vec::new())),
            modes: Arc::new(std::sync::Mutex::new(Vec::new())),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let state_for_cancel = state.clone();
        let runtime_for_cancel = runtime.clone();
        Client
            .builder()
            .connect_with(mock_agent, async move |cx| {
                let mut suspension = None;
                let mut tracked = HashMap::new();
                let perms: PendingPermissions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
                finalize_active_user_cancel(
                    &cx,
                    &sid,
                    &mut suspension,
                    &state_for_cancel,
                    &EventEmitter::Noop,
                    "conn-user-stop",
                    AgentType::Codex,
                    false,
                    &mut tracked,
                    &perms,
                    &runtime_for_cancel,
                    None,
                )
                .await;
                Ok(())
            })
            .await
            .expect("active cancel mock connect");

        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), ordering)
                .await
                .expect("TurnComplete observer timed out")
                .expect("TurnComplete observer failed")
        );
    }

    #[tokio::test]
    async fn ordinary_cancelled_stop_reason_does_not_set_user_stop() {
        let state = user_stop_test_state(true);
        {
            let mut s = state.write().await;
            s.active_provider_turn_id = Some("turn-should-clear".into());
        }
        let mut slot = None;
        let disposition = finalize_turn_terminal(
            TurnTerminalSource::Upstream("cancelled"),
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "conn-user-stop",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        assert!(matches!(
            disposition,
            TurnFinalizationDisposition::NaturalEnd(_)
        ));
        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        match last_turn_complete(&events) {
            AcpEvent::TurnComplete {
                stop_reason,
                termination_source,
                provider_turn_id,
                ..
            } => {
                assert_eq!(stop_reason, "cancelled");
                assert!(termination_source.is_none());
                assert!(provider_turn_id.is_none());
            }
            _ => unreachable!(),
        }
        assert!(state.read().await.active_provider_turn_id.is_none());
    }

    /// Task 3 Step 1: watchdog cancel shares `stop_reason=cancelled` with user
    /// Stop but must never set `termination_source` / `provider_turn_id`, and
    /// must clear any stored fence id. Exercises
    /// [`finalize_active_watchdog_cancel`] directly (not ordinary finalization).
    #[tokio::test]
    async fn watchdog_cancel_does_not_set_user_stop_and_clears_provider_id() {
        use crate::acp::terminal_adapter::adapter_for;
        use crate::acp::terminal_runtime::TerminalRuntime;
        use crate::acp::tool_watchdog::CancelCause;
        use sacp::schema::SessionId;
        use std::sync::atomic::AtomicUsize;

        let state = user_stop_test_state(true);
        {
            let mut s = state.write().await;
            s.active_provider_turn_id = Some("watchdog-turn-id".into());
        }

        let mock_agent = SuspensionLoopMockAgent {
            prompts: Arc::new(std::sync::Mutex::new(Vec::new())),
            modes: Arc::new(std::sync::Mutex::new(Vec::new())),
            agent_connection: Arc::new(std::sync::Mutex::new(None)),
            cancel_count: Arc::new(AtomicUsize::new(0)),
        };
        let state_for_loop = state.clone();
        Client
            .builder()
            .connect_with(mock_agent, async move |cx| {
                let sid = SessionId::new("session-1".to_string());
                let mut suspension = None;
                let mut tracked = HashMap::new();
                let perms: PendingPermissions = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
                let terminal_runtime = Arc::new(TerminalRuntime::new(
                    BTreeMap::new(),
                    test_placeholder_terminal_shell().spec,
                    adapter_for(AgentType::Codex),
                ));
                finalize_active_watchdog_cancel(
                    &cx,
                    &sid,
                    &mut suspension,
                    &state_for_loop,
                    &EventEmitter::Noop,
                    "conn-user-stop",
                    AgentType::Codex,
                    false,
                    &mut tracked,
                    &perms,
                    &terminal_runtime,
                    CancelCause::AutoTimeout,
                )
                .await;
                Ok(())
            })
            .await
            .expect("watchdog cancel mock connect");

        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        match last_turn_complete(&events) {
            AcpEvent::TurnComplete {
                stop_reason,
                termination_source,
                provider_turn_id,
                ..
            } => {
                assert_eq!(stop_reason, "cancelled");
                assert!(
                    termination_source.is_none(),
                    "watchdog cancel must not set termination_source=user_stop"
                );
                assert!(
                    provider_turn_id.is_none(),
                    "watchdog cancel must not forward provider_turn_id"
                );
            }
            _ => unreachable!(),
        }
        assert!(
            state.read().await.active_provider_turn_id.is_none(),
            "watchdog cancel must clear stored provider turn id"
        );
    }

    #[tokio::test]
    async fn suspension_failed_does_not_set_user_stop() {
        let state = user_stop_test_state(true);
        {
            let mut s = state.write().await;
            s.active_provider_turn_id = Some("turn-suspend-fail".into());
        }
        let (lease, _receiver) = delegation_suspend_lease(1);
        let mut slot = Some(lease);
        let disposition = finalize_turn_terminal(
            TurnTerminalSource::Upstream("end_turn"),
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "parent-conn",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        assert!(matches!(
            disposition,
            TurnFinalizationDisposition::SuspensionFailed
        ));
        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        match last_turn_complete(&events) {
            AcpEvent::TurnComplete {
                termination_source,
                provider_turn_id,
                ..
            } => {
                assert!(termination_source.is_none());
                assert!(provider_turn_id.is_none());
            }
            _ => unreachable!(),
        }
        assert!(state.read().await.active_provider_turn_id.is_none());
    }

    #[tokio::test]
    async fn delegation_suspended_retains_provider_turn_id() {
        let state = user_stop_test_state(true);
        {
            let mut s = state.write().await;
            s.active_provider_turn_id = Some("turn-retain".into());
        }
        let (lease, receiver) = delegation_suspend_lease(1);
        let mut slot = Some(lease);
        let disposition = finalize_turn_terminal(
            TurnTerminalSource::Upstream("cancelled"),
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "parent-conn",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        assert!(matches!(
            disposition,
            TurnFinalizationDisposition::DelegationSuspended
        ));
        let _ = receiver.await;
        let state_guard = state.read().await;
        assert_eq!(
            state_guard.active_provider_turn_id.as_deref(),
            Some("turn-retain"),
            "DelegationSuspended must retain stored provider turn id"
        );
        assert!(state_guard
            .recent_events_after(0)
            .expect("events")
            .iter()
            .all(|e| !matches!(e.payload, AcpEvent::TurnComplete { .. })));
    }

    #[tokio::test]
    async fn late_active_turn_id_after_finalization_is_ignored() {
        use crate::acp::terminal_adapter::adapter_for;
        use crate::acp::terminal_assoc::TerminalAssocFallback;
        use crate::acp::terminal_runtime::TerminalRuntime;
        use sacp::schema::SessionId;

        let state = user_stop_test_state(true);
        let mut slot = None;
        let _ = finalize_turn_terminal(
            TurnTerminalSource::Upstream("end_turn"),
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "conn-user-stop",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        assert!(!state.read().await.turn_in_flight);
        assert!(state.read().await.active_provider_turn_id.is_none());

        let mut q =
            std::collections::VecDeque::from([active_turn_id_session_message("late-turn-id")]);
        let mut source = ReadyUpdateSource::Fake(&mut q);
        let mut reconciler = GrokRetryReconciler::default();
        let mut turn_had = false;
        let mut compact_flag = false;
        let mut tracked = HashMap::new();
        let mut raw_cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let terminal_runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Codex),
        ));
        let terminal_assoc = Arc::new(std::sync::Mutex::new(TerminalAssocFallback::new(false)));
        let sid = SessionId::new("s-test");

        drain_ready_in_prompt_updates(
            &mut source,
            &state,
            &EventEmitter::Noop,
            AgentType::Codex,
            &sid,
            ".",
            &terminal_runtime,
            &terminal_assoc,
            &mut tracked,
            &mut raw_cache,
            &mut cb,
            &mut reconciler,
            &mut turn_had,
            &mut compact_flag,
        )
        .await;

        assert!(
            state.read().await.active_provider_turn_id.is_none(),
            "late activeTurnId after terminal finalization must not stick"
        );
    }

    #[tokio::test]
    async fn ready_drain_preserves_active_turn_id_for_user_cancel() {
        use crate::acp::terminal_adapter::adapter_for;
        use crate::acp::terminal_assoc::TerminalAssocFallback;
        use crate::acp::terminal_runtime::TerminalRuntime;
        use crate::models::message::TurnTerminationSource;
        use sacp::schema::SessionId;

        let state = user_stop_test_state(true);
        let mut q =
            std::collections::VecDeque::from([active_turn_id_session_message("drained-turn-id")]);
        let mut source = ReadyUpdateSource::Fake(&mut q);
        let mut reconciler = GrokRetryReconciler::default();
        let mut turn_had = false;
        let mut compact_flag = false;
        let mut tracked = HashMap::new();
        let mut raw_cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let terminal_runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Codex),
        ));
        let terminal_assoc = Arc::new(std::sync::Mutex::new(TerminalAssocFallback::new(false)));
        let sid = SessionId::new("s-test");

        // Bounded ready drain (same path as pre-user-cancel) harvests id
        // already on the wire before snapshot.
        drain_ready_in_prompt_updates(
            &mut source,
            &state,
            &EventEmitter::Noop,
            AgentType::Codex,
            &sid,
            ".",
            &terminal_runtime,
            &terminal_assoc,
            &mut tracked,
            &mut raw_cache,
            &mut cb,
            &mut reconciler,
            &mut turn_had,
            &mut compact_flag,
        )
        .await;

        assert_eq!(
            state.read().await.active_provider_turn_id.as_deref(),
            Some("drained-turn-id")
        );

        let mut slot = None;
        let _ = finalize_turn_terminal(
            TurnTerminalSource::UserCancel,
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "conn-user-stop",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        match last_turn_complete(&events) {
            AcpEvent::TurnComplete {
                termination_source,
                provider_turn_id,
                ..
            } => {
                assert_eq!(*termination_source, Some(TurnTerminationSource::UserStop));
                assert_eq!(provider_turn_id.as_deref(), Some("drained-turn-id"));
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn end_turn_clears_id_so_later_user_stop_does_not_reuse() {
        use crate::models::message::TurnTerminationSource;

        let state = user_stop_test_state(true);
        {
            let mut s = state.write().await;
            s.active_provider_turn_id = Some("old-turn".into());
        }
        let mut slot = None;
        let _ = finalize_turn_terminal(
            TurnTerminalSource::Upstream("end_turn"),
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "conn-user-stop",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        assert!(state.read().await.active_provider_turn_id.is_none());

        // Simulate a later prompt without a fresh activeTurnId, then user Stop.
        {
            let mut s = state.write().await;
            s.turn_in_flight = true;
            s.active_turn_generation = Some(2);
            s.parent_turn_generation = 2;
            // Intentionally leave active_provider_turn_id as None (no new id).
        }
        let mut slot = None;
        let _ = finalize_turn_terminal(
            TurnTerminalSource::UserCancel,
            &mut slot,
            &state,
            &EventEmitter::Noop,
            "conn-user-stop",
            "session-1",
            AgentType::Codex,
            false,
            None,
        )
        .await;
        let events = state
            .read()
            .await
            .recent_events_after(0)
            .expect("contiguous events");
        // Last TurnComplete is the user stop.
        match last_turn_complete(&events) {
            AcpEvent::TurnComplete {
                termination_source,
                provider_turn_id,
                ..
            } => {
                assert_eq!(*termination_source, Some(TurnTerminationSource::UserStop));
                assert!(
                    provider_turn_id.is_none(),
                    "must not reuse old-turn after end_turn cleared it"
                );
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn accept_active_turn_id_only_while_turn_in_flight() {
        use crate::acp::terminal_adapter::adapter_for;
        use crate::acp::terminal_assoc::TerminalAssocFallback;
        use crate::acp::terminal_runtime::TerminalRuntime;
        use sacp::schema::SessionId;

        // In-flight: accept.
        let state = user_stop_test_state(true);
        let mut q =
            std::collections::VecDeque::from([active_turn_id_session_message("in-flight-id")]);
        let mut source = ReadyUpdateSource::Fake(&mut q);
        let mut reconciler = GrokRetryReconciler::default();
        let mut turn_had = false;
        let mut compact_flag = false;
        let mut tracked = HashMap::new();
        let mut raw_cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();
        let terminal_runtime = Arc::new(TerminalRuntime::new(
            BTreeMap::new(),
            test_placeholder_terminal_shell().spec,
            adapter_for(AgentType::Codex),
        ));
        let terminal_assoc = Arc::new(std::sync::Mutex::new(TerminalAssocFallback::new(false)));
        let sid = SessionId::new("s-test");
        drain_ready_in_prompt_updates(
            &mut source,
            &state,
            &EventEmitter::Noop,
            AgentType::Codex,
            &sid,
            ".",
            &terminal_runtime,
            &terminal_assoc,
            &mut tracked,
            &mut raw_cache,
            &mut cb,
            &mut reconciler,
            &mut turn_had,
            &mut compact_flag,
        )
        .await;
        assert_eq!(
            state.read().await.active_provider_turn_id.as_deref(),
            Some("in-flight-id")
        );

        // Not in flight: ignore.
        let idle = user_stop_test_state(false);
        let mut q2 = std::collections::VecDeque::from([active_turn_id_session_message("idle-id")]);
        let mut source2 = ReadyUpdateSource::Fake(&mut q2);
        drain_ready_in_prompt_updates(
            &mut source2,
            &idle,
            &EventEmitter::Noop,
            AgentType::Codex,
            &sid,
            ".",
            &terminal_runtime,
            &terminal_assoc,
            &mut tracked,
            &mut raw_cache,
            &mut cb,
            &mut reconciler,
            &mut turn_had,
            &mut compact_flag,
        )
        .await;
        assert!(idle.read().await.active_provider_turn_id.is_none());
    }

    #[test]
    fn parse_extension_turn_completed_accepts_grok_xai_method() {
        let notif = UntypedMessage::new(
            "_x.ai/session/update",
            serde_json::json!({
                "sessionId": "019f74ba-af84-7a11-afd4-d7685a9a599d",
                "update": {
                    "sessionUpdate": "turn_completed",
                    "prompt_id": "b1d064c3-39df-4885-8d0a-cc52fa26d75a",
                    "stop_reason": "end_turn"
                }
            }),
        )
        .unwrap();
        assert_eq!(
            parse_extension_turn_completed_notification(&notif).as_deref(),
            Some("end_turn")
        );
        let dispatch = Dispatch::Notification(notif);
        assert_eq!(
            parse_extension_turn_completed(&dispatch).as_deref(),
            Some("end_turn")
        );
    }

    #[test]
    fn parse_extension_turn_completed_accepts_session_update_method() {
        let notif = UntypedMessage::new(
            "session/update",
            serde_json::json!({
                "sessionId": "s",
                "update": {
                    "sessionUpdate": "turn_completed",
                    "stop_reason": "max_tokens"
                }
            }),
        )
        .unwrap();
        assert_eq!(
            parse_extension_turn_completed_notification(&notif).as_deref(),
            Some("max_tokens")
        );
    }

    #[test]
    fn parse_extension_turn_completed_ignores_other_updates() {
        let notif = UntypedMessage::new(
            "_x.ai/session/update",
            serde_json::json!({
                "sessionId": "s",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "hi"}
                }
            }),
        )
        .unwrap();
        assert!(parse_extension_turn_completed_notification(&notif).is_none());
        assert!(parse_extension_turn_completed_notification(
            &UntypedMessage::new("_x.ai/sessions/changed", serde_json::json!({})).unwrap()
        )
        .is_none());
    }

    #[test]
    fn normalize_extension_stop_reason_maps_pascal_case() {
        assert_eq!(normalize_extension_stop_reason("EndTurn"), "end_turn");
        assert_eq!(normalize_extension_stop_reason("Cancelled"), "cancelled");
        assert_eq!(normalize_extension_stop_reason("canceled"), "cancelled");
        assert_eq!(
            normalize_extension_stop_reason("totally_unknown_reason"),
            "end_turn"
        );
    }

    #[test]
    fn map_claude_sdk_ext_notification_rejects_non_api_retry() {
        let non_retry = UntypedMessage::new(
            "_claude/sdkMessage",
            serde_json::json!({
                "sessionId": "session-123",
                "message": {"type": "system", "subtype": "status"}
            }),
        )
        .unwrap();
        assert!(map_claude_sdk_ext_notification(&non_retry).is_none());
    }

    #[test]
    fn map_claude_sdk_ext_notification_rejects_invalid_payload() {
        let wrong_method = UntypedMessage::new(
            "_other/method",
            serde_json::json!({"sessionId": "s", "message": {}}),
        )
        .unwrap();
        assert!(map_claude_sdk_ext_notification(&wrong_method).is_none());

        let missing_fields =
            UntypedMessage::new("_claude/sdkMessage", serde_json::json!({"sessionId": 1})).unwrap();
        assert!(map_claude_sdk_ext_notification(&missing_fields).is_none());
    }

    /// The exact `_x.ai/session_notification` envelope captured from grok 0.2.111
    /// running `/compact` — `auto_compact_completed` under `params.update`, with
    /// the token delta and an `_meta.eventId`.
    #[test]
    fn map_grok_ext_notification_maps_auto_compact_completed() {
        let raw = UntypedMessage::new(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionId": "019f9475-c67f-7390-9ee5-a09d29986a6c",
                "update": {
                    "sessionUpdate": "auto_compact_completed",
                    "tokens_before": 45389,
                    "tokens_after": 16486,
                    "summary_preview": null
                },
                "_meta": {
                    "eventId": "019f9475-c67f-7390-9ee5-a09d29986a6c-4",
                    "agentTimestampMs": 1784902203750u64
                }
            }),
        )
        .unwrap();

        let event = map_grok_ext_notification(&raw, AgentType::Grok)
            .expect("auto_compact_completed should map to a compaction card");
        match event {
            AcpEvent::ToolCall {
                tool_call_id,
                status,
                meta,
                ..
            } => {
                assert_eq!(tool_call_id, "019f9475-c67f-7390-9ee5-a09d29986a6c-4");
                assert_eq!(status, "completed");
                let meta = meta.expect("compaction card needs meta");
                assert_eq!(
                    meta.get("contextCompaction").and_then(|v| v.as_bool()),
                    Some(true)
                );
                assert_eq!(
                    meta.get("tokensBefore").and_then(|v| v.as_u64()),
                    Some(45389)
                );
                assert_eq!(
                    meta.get("tokensAfter").and_then(|v| v.as_u64()),
                    Some(16486)
                );
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    /// The same variant may arrive on the sibling `_x.ai/session/update` method.
    #[test]
    fn map_grok_ext_notification_handles_session_update_method() {
        let raw = UntypedMessage::new(
            "_x.ai/session/update",
            serde_json::json!({
                "sessionId": "s",
                "update": { "sessionUpdate": "auto_compact_completed", "tokens_before": 100, "tokens_after": 100 }
            }),
        )
        .unwrap();
        // No `_meta.eventId` → a generated id, but it must still map.
        assert!(matches!(
            map_grok_ext_notification(&raw, AgentType::Grok),
            Some(AcpEvent::ToolCall { .. })
        ));
    }

    #[test]
    fn map_grok_ext_notification_auto_compact_failed_surfaces_error() {
        let raw = UntypedMessage::new(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionId": "s",
                "update": { "sessionUpdate": "auto_compact_failed", "reason": "API error (status 503)" }
            }),
        )
        .unwrap();
        match map_grok_ext_notification(&raw, AgentType::Grok) {
            Some(AcpEvent::Error {
                message, terminal, ..
            }) => {
                assert!(
                    message.contains("503"),
                    "error should carry the reason; got: {message}"
                );
                assert!(!terminal, "compaction failure must not kill the connection");
            }
            other => panic!("expected non-terminal Error, got {other:?}"),
        }
    }

    #[test]
    fn map_grok_ext_notification_is_grok_gated_and_scoped() {
        let compact = serde_json::json!({
            "sessionId": "s",
            "update": { "sessionUpdate": "auto_compact_completed", "tokens_before": 1, "tokens_after": 1 }
        });
        // Non-grok agent: ignored even for the same payload.
        let raw = UntypedMessage::new("_x.ai/session_notification", compact.clone()).unwrap();
        assert!(map_grok_ext_notification(&raw, AgentType::Codex).is_none());

        // Turn-level state is intentionally left to the prompt-response path.
        let turn = UntypedMessage::new(
            "_x.ai/session_notification",
            serde_json::json!({
                "sessionId": "s",
                "update": { "sessionUpdate": "turn_completed", "stop_reason": "error", "agent_result": "boom" }
            }),
        )
        .unwrap();
        assert!(map_grok_ext_notification(&turn, AgentType::Grok).is_none());

        // Unrelated method: ignored.
        let other = UntypedMessage::new("session/update", compact).unwrap();
        assert!(map_grok_ext_notification(&other, AgentType::Grok).is_none());
    }

    /// The turn-loop consults this to keep a compaction-only `/compact` turn
    /// from being reclassified as `"empty"` (which re-surfaces a spurious error).
    /// It must count exactly the compaction outcomes that emit a card/error.
    #[test]
    fn grok_ext_notification_is_turn_output_marks_compaction_outcomes() {
        let notif = |variant: &str| {
            Dispatch::Notification(
                UntypedMessage::new(
                    "_x.ai/session_notification",
                    serde_json::json!({
                        "sessionId": "s",
                        "update": {
                            "sessionUpdate": variant,
                            "tokens_before": 9, "tokens_after": 8, "reason": "x"
                        }
                    }),
                )
                .unwrap(),
            )
        };
        // Both compaction outcomes are visible turn output.
        assert!(grok_ext_notification_is_turn_output(
            &notif("auto_compact_completed"),
            AgentType::Grok
        ));
        assert!(grok_ext_notification_is_turn_output(
            &notif("auto_compact_failed"),
            AgentType::Grok
        ));
        // turn_completed is deliberately left to the prompt-response path — it is
        // NOT counted here (otherwise a genuinely empty turn would be masked).
        assert!(!grok_ext_notification_is_turn_output(
            &notif("turn_completed"),
            AgentType::Grok
        ));
        // Never fires for a non-grok agent.
        assert!(!grok_ext_notification_is_turn_output(
            &notif("auto_compact_completed"),
            AgentType::Codex
        ));
    }

    #[test]
    fn build_new_session_request_sets_claude_raw_meta() {
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let req = build_new_session_request(
            AgentType::ClaudeCode,
            &cwd,
            Vec::new(),
            &test_posix_spec(),
            adapter_for(AgentType::ClaudeCode),
            &native_plan(AgentType::ClaudeCode),
            ConnectionPurpose::User,
        )
        .unwrap();

        assert_eq!(
            req.meta
                .as_ref()
                .and_then(|m| m.get("claudeCode"))
                .and_then(|v| v.get("emitRawSDKMessages"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        // Native plan: no Codeg-injected deny list.
        assert!(req
            .meta
            .as_ref()
            .and_then(|m| m.get("claudeCode"))
            .and_then(|c| c.get("options"))
            .is_none());
    }

    /// The `loadSession` capability gate hands the failure ladder a synthetic
    /// error instead of sending an unsupported RPC. That error must classify as
    /// "just open a new session": anything else would put a "session could not
    /// be loaded" banner in front of every user whose agent simply does not
    /// implement `session/load`.
    #[test]
    fn a_session_load_never_sent_falls_back_without_alarming_the_user() {
        let e = sacp::Error::method_not_found()
            .data("agent does not advertise the loadSession capability");
        let text = e.to_string();
        assert_eq!(classify_session_load_failure(e.code, &text), None);
        assert!(text.contains("Method not found"), "{text}");
        assert!(!text.contains("Authentication required"), "{text}");
    }

    #[test]
    fn the_model_selector_is_the_model_recorded_on_a_turn() {
        let select = |id: &str, category: &str, current: &str| SessionConfigOptionInfo {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            category: Some(category.to_string()),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: current.to_string(),
                options: Vec::new(),
                groups: Vec::new(),
            }),
        };

        // The model comes from the `model` selector, not from whichever
        // selector happens to be first — agents publish several.
        assert_eq!(
            current_model_id_from_opts(&[
                select("effort", "mode", "high"),
                select("model", "model", "grok-4"),
            ]),
            Some("grok-4".to_string())
        );
        // No model selector (the common case for custom agents) and an empty
        // current value both mean "unknown", never a placeholder.
        assert_eq!(
            current_model_id_from_opts(&[select("effort", "mode", "high")]),
            None
        );
        assert_eq!(
            current_model_id_from_opts(&[select("m", "model", "")]),
            None
        );
        assert_eq!(current_model_id_from_opts(&[]), None);
    }

    #[test]
    fn build_load_session_request_skips_claude_meta_for_non_claude() {
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let req = build_load_session_request(
            AgentType::Codex,
            SessionId::new("abc".to_string()),
            &cwd,
            Vec::new(),
            &test_posix_spec(),
            adapter_for(AgentType::Codex),
            &native_plan(AgentType::Codex),
            ConnectionPurpose::User,
        )
        .unwrap();

        // Terminal metadata is always present; Claude raw-SDK meta is not.
        let meta = req.meta.as_ref().expect("terminal meta required");
        assert!(!meta.contains_key("claudeCode"));
        assert!(meta.contains_key("codeg.dev/terminal"));
    }

    #[test]
    fn build_resume_session_request_skips_claude_meta_for_non_claude() {
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let req = build_resume_session_request(
            AgentType::Codex,
            SessionId::new("abc".to_string()),
            &cwd,
            Vec::new(),
            &test_posix_spec(),
            adapter_for(AgentType::Codex),
            &native_plan(AgentType::Codex),
            ConnectionPurpose::User,
        )
        .unwrap();

        let meta = req.meta.as_ref().expect("terminal meta required");
        assert!(!meta.contains_key("claudeCode"));
        assert!(meta.contains_key("codeg.dev/terminal"));
    }

    /// Grok's native ask tool remains enabled on every session-open path so its
    /// blocking extension request can be bridged into Codeg's question cards.
    #[test]
    fn grok_session_meta_keeps_native_ask_user_enabled_on_new_load_resume() {
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let spec = test_posix_spec();
        let adapter = adapter_for(AgentType::Grok);
        let plan = native_plan(AgentType::Grok);

        let new_req = build_new_session_request(
            AgentType::Grok,
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        let load_req = build_load_session_request(
            AgentType::Grok,
            SessionId::new("sess-load".to_string()),
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        let resume_req = build_resume_session_request(
            AgentType::Grok,
            SessionId::new("sess-resume".to_string()),
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();

        for (label, meta) in [
            ("new", new_req.meta.as_ref()),
            ("load", load_req.meta.as_ref()),
            ("resume", resume_req.meta.as_ref()),
        ] {
            let meta = meta.unwrap_or_else(|| panic!("{label}: session meta required"));
            assert!(
                !meta.contains_key("askUserQuestion"),
                "{label}: askUserQuestion must remain at Grok's enabled default"
            );
            assert!(
                meta.contains_key("codeg.dev/terminal"),
                "{label}: terminal meta must remain"
            );
        }
    }

    /// Hidden generation (title/translate) must stamp a restrictive
    /// `_meta.agentProfile` so ACP strips shell/MCP tools; ordinary User
    /// purpose must not.
    #[test]
    fn grok_hidden_generation_meta_stamps_agent_profile_denylist() {
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let spec = test_posix_spec();
        let adapter = adapter_for(AgentType::Grok);
        let plan = native_plan(AgentType::Grok);

        for purpose in [
            ConnectionPurpose::InternalTitle,
            ConnectionPurpose::InternalTranslate,
        ] {
            let req = build_new_session_request(
                AgentType::Grok,
                &cwd,
                Vec::new(),
                &spec,
                adapter,
                &plan,
                purpose,
            )
            .unwrap();
            let meta = req.meta.as_ref().expect("meta");
            let profile = meta
                .get("agentProfile")
                .and_then(|v| v.as_object())
                .unwrap_or_else(|| panic!("{purpose:?}: agentProfile required"));
            assert_eq!(
                profile.get("name").and_then(|v| v.as_str()),
                Some("codeg-hidden-generation"),
                "{purpose:?}"
            );
            let denied = profile
                .get("disallowedTools")
                .and_then(|v| v.as_array())
                .expect("disallowedTools");
            let as_str: Vec<&str> = denied.iter().filter_map(|v| v.as_str()).collect();
            assert!(
                as_str.contains(&"run_terminal_cmd"),
                "{purpose:?}: must deny shell tool, got {as_str:?}"
            );
            assert!(
                as_str.contains(&"search_tool") && as_str.contains(&"use_tool"),
                "{purpose:?}: must deny MCP meta tools"
            );
            assert!(
                !meta.contains_key("askUserQuestion"),
                "{purpose:?}: native ask bridge remains available"
            );
        }

        let user_req = build_new_session_request(
            AgentType::Grok,
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        assert!(
            user_req
                .meta
                .as_ref()
                .map(|m| !m.contains_key("agentProfile"))
                .unwrap_or(false),
            "User purpose on Native route must not stamp agentProfile"
        );
    }

    /// Codeg-route Grok user sessions must stamp a **narrow** agentProfile that
    /// denylists native subagent tools on new/load/resume (ACP-effective path).
    /// Does not strip shell/read or set maxTurns/permissionMode.
    #[test]
    fn grok_codeg_route_meta_stamps_narrow_subagent_denylist_on_new_load_resume() {
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let spec = test_posix_spec();
        let adapter = adapter_for(AgentType::Grok);
        let plan = codeg_plan(AgentType::Grok);

        let new_req = build_new_session_request(
            AgentType::Grok,
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        let load_req = build_load_session_request(
            AgentType::Grok,
            SessionId::new("sess-load".to_string()),
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();
        let resume_req = build_resume_session_request(
            AgentType::Grok,
            SessionId::new("sess-resume".to_string()),
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::User,
        )
        .unwrap();

        for (label, meta) in [
            ("new", new_req.meta.as_ref()),
            ("load", load_req.meta.as_ref()),
            ("resume", resume_req.meta.as_ref()),
        ] {
            let meta = meta.unwrap_or_else(|| panic!("{label}: session meta required"));
            let profile = meta
                .get("agentProfile")
                .and_then(|v| v.as_object())
                .unwrap_or_else(|| panic!("{label}: agentProfile required"));
            assert_eq!(
                profile.get("name").and_then(|v| v.as_str()),
                Some("codeg-route-no-native-subagents"),
                "{label}"
            );
            let denied = profile
                .get("disallowedTools")
                .and_then(|v| v.as_array())
                .expect("disallowedTools");
            let as_str: Vec<&str> = denied.iter().filter_map(|v| v.as_str()).collect();
            for tool in [
                "spawn_subagent",
                "get_command_or_subagent_output",
                "kill_command_or_subagent",
            ] {
                assert!(
                    as_str.contains(&tool),
                    "{label}: must deny {tool}, got {as_str:?}"
                );
            }
            // Narrow profile: do not disable ordinary coding tools.
            assert!(
                !as_str.contains(&"read_file") && !as_str.contains(&"run_terminal_command"),
                "{label}: must not strip shell/read, got {as_str:?}"
            );
            assert!(
                profile.get("maxTurns").is_none(),
                "{label}: must not set maxTurns"
            );
            assert!(
                profile.get("permissionMode").is_none(),
                "{label}: must not set permissionMode"
            );
            assert!(
                !meta.contains_key("askUserQuestion"),
                "{label}: native ask bridge remains available"
            );
            assert!(
                meta.contains_key("codeg.dev/terminal"),
                "{label}: terminal meta must remain"
            );
        }

        // Hidden generation keeps the full denylist profile, not the narrow route one.
        let hidden_req = build_new_session_request(
            AgentType::Grok,
            &cwd,
            Vec::new(),
            &spec,
            adapter,
            &plan,
            ConnectionPurpose::InternalTitle,
        )
        .unwrap();
        let hidden_profile = hidden_req
            .meta
            .as_ref()
            .and_then(|m| m.get("agentProfile"))
            .and_then(|v| v.as_object())
            .expect("hidden agentProfile");
        assert_eq!(
            hidden_profile.get("name").and_then(|v| v.as_str()),
            Some("codeg-hidden-generation")
        );
    }

    #[test]
    fn non_grok_session_meta_omits_ask_user_question_flag() {
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        for agent in [AgentType::Codex, AgentType::ClaudeCode] {
            let req = build_new_session_request(
                agent,
                &cwd,
                Vec::new(),
                &test_posix_spec(),
                adapter_for(agent),
                &native_plan(agent),
                ConnectionPurpose::User,
            )
            .unwrap();
            let meta = req.meta.as_ref().expect("session meta required");
            assert!(
                !meta.contains_key("askUserQuestion"),
                "{agent:?}: askUserQuestion is Grok-only"
            );
        }
    }

    fn assert_codeg_terminal_meta(value: &serde_json::Value, dialect: &str, shell: &str) {
        let term = &value["_meta"]["codeg.dev/terminal"];
        assert_eq!(term["dialect"], dialect);
        assert_eq!(term["shell"], shell);
        assert_eq!(term["platform"], std::env::consts::OS);
        assert_eq!(term["commandMode"], "selected-shell-for-command-lines");
    }

    #[test]
    fn initialize_contains_terminal_metadata() {
        let spec = test_pwsh_spec();
        let request =
            build_initialize_request(AgentType::Codex, &spec, adapter_for(AgentType::Codex))
                .unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_codeg_terminal_meta(&value, "powershell", &spec.executable.to_string_lossy());
    }

    #[test]
    fn initialize_disables_client_terminal_only_for_grok() {
        let spec = test_pwsh_spec();
        let request =
            build_initialize_request(AgentType::Grok, &spec, adapter_for(AgentType::Grok)).unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["clientCapabilities"]["terminal"], false);

        let request =
            build_initialize_request(AgentType::Codex, &spec, adapter_for(AgentType::Codex))
                .unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["clientCapabilities"]["terminal"], true);
    }

    #[test]
    fn canonical_spec_to_mcp_server_stdio() {
        // Use an absolute path so the test is portable across machines that
        // may or may not have `npx` on PATH.
        let spec = serde_json::json!({
            "type": "stdio",
            "command": "/usr/local/bin/npx",
            "args": ["-y", "@mcp_hub_org/cli@latest", "run", "figma-developer-mcp"],
            "env": {"FIGMA_API_KEY": "secret"},
        });
        let server = canonical_spec_to_mcp_server("figma", &spec).expect("stdio spec should map");
        match server {
            McpServer::Stdio(s) => {
                assert_eq!(s.name, "figma");
                assert_eq!(s.command, std::path::PathBuf::from("/usr/local/bin/npx"));
                assert_eq!(s.args.len(), 4);
                assert_eq!(s.env.len(), 1);
                assert_eq!(s.env[0].name, "FIGMA_API_KEY");
            }
            other => panic!("expected Stdio variant, got {other:?}"),
        }
    }

    #[test]
    fn canonical_spec_resolves_bare_command_to_absolute() {
        // Bare command names get resolved via PATH so the resulting payload
        // satisfies the ACP "command MUST be absolute" requirement. We use
        // `cargo` because the test process must have it on PATH.
        let spec = serde_json::json!({
            "type": "stdio",
            "command": "cargo",
        });
        let server = canonical_spec_to_mcp_server("x", &spec).expect("bare command should resolve");
        match server {
            McpServer::Stdio(s) => assert!(
                s.command.is_absolute(),
                "expected absolute path, got {}",
                s.command.display()
            ),
            other => panic!("expected Stdio variant, got {other:?}"),
        }
    }

    #[test]
    fn grok_incompatible_agent_switch_detects_stable_code() {
        // Exact shape Grok returns when switching to a model whose agentType
        // differs from the established conversation's (captured from a live
        // `session/set_model` probe against grok 0.2.98).
        let err = sacp::Error::new(-32600, "Cannot switch to model ...").data(serde_json::json!({
            "code": "MODEL_SWITCH_INCOMPATIBLE_AGENT",
            "activeAgentType": "grok-build-plan",
            "requiredAgentType": "cursor",
            "modelId": "grok-composer-2.5-fast",
            "suggestion": "start_new_session"
        }));
        assert!(is_grok_incompatible_agent_switch(&err));

        // A different data.code, or no data at all, must NOT be swallowed —
        // those fall through to the generic error path.
        let other =
            sacp::Error::new(-32603, "boom").data(serde_json::json!({ "code": "SOMETHING_ELSE" }));
        assert!(!is_grok_incompatible_agent_switch(&other));
        assert!(!is_grok_incompatible_agent_switch(
            &sacp::Error::internal_error()
        ));
    }

    #[test]
    fn synthesize_grok_config_options_yields_model_and_effort_selectors() {
        // `_meta["x.ai/sessionConfig"].options` as delivered by `session/new`
        // (captured live): both model choices and the "mode" effort choices.
        let meta: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": true},
                        {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer 2.5", "selected": false},
                        {"id": "high", "category": "mode", "label": "High Effort", "selected": true},
                        {"id": "low", "category": "mode", "label": "Low Effort", "selected": false}
                    ]
                }
            }),
        )
        .unwrap();

        // Empty specs → the effort selector comes from the flat `x.ai/sessionConfig`
        // "mode" list (the no-`models` fallback path).
        let opts = synthesize_grok_config_options(Some(&meta), &HashMap::new())
            .expect("should synthesize");
        assert_eq!(opts.len(), 2, "model + effort selectors");

        let model = &opts[0];
        assert_eq!(model.id, GROK_MODEL_OPTION_ID);
        assert_eq!(model.category.as_deref(), Some("model"));
        let SessionConfigKindInfo::Select(model_sel) = &model.kind;
        // Both models appear (agent-type filtering is deliberately NOT applied —
        // cross-type switches are handled gracefully at set time instead).
        assert_eq!(model_sel.options.len(), 2);
        assert_eq!(
            model_sel.current_value, "grok-4.5",
            "the `selected` model is current"
        );
        assert!(model_sel
            .options
            .iter()
            .any(|o| o.value == "grok-composer-2.5-fast"));

        let effort = &opts[1];
        assert_eq!(effort.id, GROK_EFFORT_OPTION_ID);
        assert_eq!(effort.category.as_deref(), Some("mode"));
        let SessionConfigKindInfo::Select(effort_sel) = &effort.kind;
        assert_eq!(effort_sel.options.len(), 2);
        assert_eq!(
            effort_sel.current_value, "high",
            "the `selected` effort is current"
        );
        assert!(effort_sel.options.iter().any(|o| o.value == "low"));
    }

    #[test]
    fn synthesize_grok_config_options_model_only_when_no_effort_offered() {
        // A model that doesn't advertise `supportsReasoningEffort` yields no
        // `category:"mode"` entries → only the model selector is surfaced.
        let meta: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer 2.5", "selected": true}
                    ]
                }
            }),
        )
        .unwrap();
        // Empty specs → the effort selector comes from the flat `x.ai/sessionConfig`
        // "mode" list (the no-`models` fallback path).
        let opts = synthesize_grok_config_options(Some(&meta), &HashMap::new())
            .expect("should synthesize");
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].id, GROK_MODEL_OPTION_ID);
    }

    #[test]
    fn grok_set_model_params_carry_effort_override() {
        // Pure model switch → no `_meta`, so grok keeps the current effort.
        let p = build_grok_set_model_params("s1", "grok-4.5", None);
        assert_eq!(p["sessionId"], "s1");
        assert_eq!(p["modelId"], "grok-4.5");
        assert!(p.get("_meta").is_none());
        // Effort override rides in `_meta.reasoningEffort` (the key grok parses).
        let p = build_grok_set_model_params("s1", "grok-4.5", Some("high"));
        assert_eq!(p["modelId"], "grok-4.5");
        assert_eq!(p["_meta"]["reasoningEffort"], "high");
    }

    #[test]
    fn synthesize_grok_config_options_none_without_sessionconfig() {
        let empty: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        assert!(synthesize_grok_config_options(Some(&empty), &HashMap::new()).is_none());
        assert!(synthesize_grok_config_options(None, &HashMap::new()).is_none());
    }

    /// Raw top-level `models` mirroring grok 0.2.99's `session/new`: grok-4.5
    /// supports effort (default `xhigh`, switchable high/medium/low),
    /// grok-composer-2.5-fast supports none.
    fn grok_models_fixture() -> serde_json::Value {
        serde_json::json!({
            "currentModelId": "grok-4.5",
            "availableModels": [
                {
                    "modelId": "grok-4.5",
                    "name": "Grok 4.5",
                    "_meta": {
                        "supportsReasoningEffort": true,
                        "reasoningEffort": "xhigh",
                        "reasoningEfforts": [
                            {"id": "high", "label": "High Effort", "description": "Highest quality", "default": true},
                            {"id": "medium", "label": "Medium Effort", "description": "Balanced"},
                            {"id": "low", "label": "Low Effort", "description": "Fast"}
                        ]
                    }
                },
                {
                    "modelId": "grok-composer-2.5-fast",
                    "name": "Composer 2.5",
                    "_meta": {"supportsReasoningEffort": false}
                }
            ]
        })
    }

    #[test]
    fn parse_grok_effort_specs_reads_per_model_meta() {
        let specs = parse_grok_effort_specs(Some(&grok_models_fixture()));
        let g45 = specs.get("grok-4.5").expect("grok-4.5 present");
        assert!(g45.supports);
        assert_eq!(g45.default.as_deref(), Some("xhigh"));
        assert_eq!(g45.options.len(), 3);
        assert_eq!(g45.options[0].0, "high");
        let fast = specs
            .get("grok-composer-2.5-fast")
            .expect("composer present");
        assert!(!fast.supports);
        assert!(fast.default.is_none());
        assert!(fast.options.is_empty());
    }

    #[test]
    fn parse_grok_effort_specs_absent_models_is_empty() {
        assert!(parse_grok_effort_specs(None).is_empty());
        assert!(parse_grok_effort_specs(Some(&serde_json::json!({}))).is_empty());
        // Missing `_meta` degrades to supports=false / default=None / options=[].
        let bare = serde_json::json!({ "availableModels": [{"modelId": "m1", "name": "M1"}] });
        let specs = parse_grok_effort_specs(Some(&bare));
        let m1 = specs.get("m1").expect("m1 present");
        assert!(!m1.supports);
        assert!(m1.default.is_none());
        assert!(m1.options.is_empty());
    }

    #[test]
    fn build_grok_effort_option_injects_default_and_gates_supports() {
        let specs = parse_grok_effort_specs(Some(&grok_models_fixture()));
        // grok-4.5: `xhigh` default is injected at the FRONT (not in the
        // switchable list), current = xhigh, with canonical labels.
        let effort = build_grok_effort_option("grok-4.5", &specs).expect("has effort");
        assert_eq!(effort.id, GROK_EFFORT_OPTION_ID);
        let SessionConfigKindInfo::Select(sel) = &effort.kind;
        assert_eq!(sel.current_value, "xhigh");
        assert_eq!(sel.options.len(), 4, "high/medium/low + injected xhigh");
        assert_eq!(sel.options[0].value, "xhigh");
        assert_eq!(sel.options[0].name, "Max");
        // The injected default has no grok description, so it gets our canonical
        // one — every tier must have sub-text, not just high/medium/low.
        assert_eq!(
            sel.options[0].description.as_deref(),
            Some("Maximum reasoning for the most complex tasks")
        );
        assert!(sel.options.iter().all(|o| o.description.is_some()));
        // Grok's own per-tier text is preserved for the switchable tiers.
        assert!(sel.options.iter().any(|o| o.value == "high"
            && o.name == "High"
            && o.description.as_deref() == Some("Highest quality")));
        // Unsupported model → no selector; unknown model → None.
        assert!(build_grok_effort_option("grok-composer-2.5-fast", &specs).is_none());
        assert!(build_grok_effort_option("nope", &specs).is_none());
    }

    #[test]
    fn synthesize_grok_config_options_model_reactive_effort_for_4_5() {
        // Flat sessionConfig marks grok-4.5 current; per-model specs drive effort.
        let meta: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": true},
                        {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer 2.5", "selected": false}
                    ]
                }
            }),
        )
        .unwrap();
        let specs = parse_grok_effort_specs(Some(&grok_models_fixture()));
        let opts = synthesize_grok_config_options(Some(&meta), &specs).expect("synthesize");
        assert_eq!(opts.len(), 2, "model + effort");
        let effort = opts
            .iter()
            .find(|o| o.id == GROK_EFFORT_OPTION_ID)
            .expect("effort selector");
        let SessionConfigKindInfo::Select(sel) = &effort.kind;
        assert_eq!(sel.current_value, "xhigh", "grok-4.5's real default");
        assert!(sel
            .options
            .iter()
            .any(|o| o.value == "xhigh" && o.name == "Max"));
    }

    #[test]
    fn synthesize_grok_config_options_no_effort_for_composer_fast() {
        // Current model is the no-effort composer model → only the model selector.
        let meta: serde_json::Map<String, serde_json::Value> = serde_json::from_value(
            serde_json::json!({
                "x.ai/sessionConfig": {
                    "options": [
                        {"id": "grok-4.5", "category": "model", "label": "Grok 4.5", "selected": false},
                        {"id": "grok-composer-2.5-fast", "category": "model", "label": "Composer 2.5", "selected": true}
                    ]
                }
            }),
        )
        .unwrap();
        let specs = parse_grok_effort_specs(Some(&grok_models_fixture()));
        let opts = synthesize_grok_config_options(Some(&meta), &specs).expect("synthesize");
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].id, GROK_MODEL_OPTION_ID);
    }

    #[test]
    fn set_grok_effort_selector_for_model_drops_and_adds() {
        let specs = parse_grok_effort_specs(Some(&grok_models_fixture()));
        // Model + grok-4.5 effort → switching to the no-effort model DROPS effort.
        let mut opts = grok_model_options("grok-4.5");
        opts.push(build_grok_effort_option("grok-4.5", &specs).unwrap());
        assert_eq!(opts.len(), 2);
        set_grok_effort_selector_for_model(&mut opts, "grok-composer-2.5-fast", &specs);
        assert_eq!(opts.len(), 1);
        assert!(opts.iter().all(|o| o.id != GROK_EFFORT_OPTION_ID));
        // Switching back to grok-4.5 RE-ADDS it, current = xhigh.
        set_grok_effort_selector_for_model(&mut opts, "grok-4.5", &specs);
        let effort = opts
            .iter()
            .find(|o| o.id == GROK_EFFORT_OPTION_ID)
            .expect("re-added");
        let SessionConfigKindInfo::Select(sel) = &effort.kind;
        assert_eq!(sel.current_value, "xhigh");
    }

    fn grok_model_options(current: &str) -> Vec<SessionConfigOptionInfo> {
        vec![SessionConfigOptionInfo {
            id: GROK_MODEL_OPTION_ID.to_string(),
            name: "Model".to_string(),
            description: None,
            category: Some("model".to_string()),
            kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
                current_value: current.to_string(),
                options: vec![
                    SessionConfigSelectOptionInfo {
                        value: "grok-4.5".to_string(),
                        name: "Grok 4.5".to_string(),
                        description: None,
                    },
                    SessionConfigSelectOptionInfo {
                        value: "grok-composer-2.5-fast".to_string(),
                        name: "Composer 2.5".to_string(),
                        description: None,
                    },
                ],
                groups: Vec::new(),
            }),
        }]
    }

    #[tokio::test]
    async fn grok_incompatible_agent_switch_reverts_and_reports_without_deadlock() {
        use std::time::Duration;

        let mut st = SessionState::new(
            "conn-test".to_string(),
            AgentType::Grok,
            None,
            "win".to_string(),
            None,
        );
        // The conversation is on grok-4.5; the user optimistically picked the
        // cross-agent-type Composer model, which Grok rejected mid-conversation.
        st.config_options = Some(grok_model_options("grok-4.5"));
        let state = Arc::new(RwLock::new(st));
        let emitter = EventEmitter::Noop;

        // Regression guard: the recovery previously read `config_options` inline
        // in an `if let`, holding the read guard across `emit_*` (which take the
        // write lock) → deadlock. A timeout turns that hang into a failure.
        tokio::time::timeout(
            Duration::from_secs(5),
            emit_grok_incompatible_agent_switch(&state, &emitter),
        )
        .await
        .expect("recovery must complete, not deadlock on the state lock");

        let guard = state.read().await;

        // The optimistic pick is reverted: the authoritative model is unchanged.
        let opts = guard.config_options.as_ref().expect("options preserved");
        let SessionConfigKindInfo::Select(sel) = &opts[0].kind;
        assert_eq!(sel.current_value, "grok-4.5");

        // Event ordering: the authoritative options (revert) precede the coded
        // error so the composer snaps back before the toast appears.
        let events = guard.recent_events_after(0).expect("events recorded");
        let cfg_idx = events
            .iter()
            .position(|e| matches!(&e.payload, AcpEvent::SessionConfigOptions { .. }))
            .expect("a session_config_options revert is emitted");
        let err_idx = events
            .iter()
            .position(|e| matches!(&e.payload, AcpEvent::Error { .. }))
            .expect("a coded error is emitted");
        assert!(cfg_idx < err_idx, "revert must precede the error");

        // The reverted options carry the original model.
        if let AcpEvent::SessionConfigOptions { config_options } = &events[cfg_idx].payload {
            let SessionConfigKindInfo::Select(sel) = &config_options[0].kind;
            assert_eq!(sel.current_value, "grok-4.5");
        }

        // Exactly one error, carrying the localizable code (not a raw message)
        // and recoverable — no generic double-emit.
        let errors: Vec<(Option<String>, bool)> = events
            .iter()
            .filter_map(|e| match &e.payload {
                AcpEvent::Error { code, terminal, .. } => Some((code.clone(), *terminal)),
                _ => None,
            })
            .collect();
        assert_eq!(errors.len(), 1, "no double error emit");
        assert_eq!(
            errors[0].0.as_deref(),
            Some(GROK_INCOMPATIBLE_AGENT_ERROR_CODE)
        );
        assert!(!errors[0].1, "recoverable, not terminal");
    }

    #[test]
    fn grok_live_tool_output_prefers_content() {
        // The clean content channel carries the output → don't ship raw_output
        // at all (frontend renders `content`, matching the parser's precedence).
        let content = Some("build ok\n".to_string());
        let raw = Some(serde_json::json!({
            "output_for_prompt": "exit: 0\n\nbuild ok",
            "exit_code": 0,
            "command": "pnpm build",
        }));
        assert_eq!(grok_live_tool_output(&content, &raw), None);
    }

    #[test]
    fn grok_live_tool_output_falls_back_to_output_for_prompt_when_content_empty() {
        // With no content, recover the readable text from the string
        // `output_for_prompt` (NOT the byte-array `output`, NOT the whole blob).
        let raw = Some(serde_json::json!({
            "output": [10, 62, 32],
            "output_for_prompt": "exit: 0\n\nok",
            "exit_code": 0,
            "command": "pnpm build",
        }));
        assert_eq!(
            grok_live_tool_output(&None, &raw).as_deref(),
            Some("exit: 0\n\nok")
        );
        // Whitespace-only content is treated as empty.
        let ws = Some("  \n".to_string());
        assert_eq!(
            grok_live_tool_output(&ws, &raw).as_deref(),
            Some("exit: 0\n\nok")
        );
    }

    /// A `get_command_or_subagent_output` poll has no `content[]` and no
    /// `output_for_prompt` — its whole result sits under the `TaskOutput`
    /// envelope, which used to be dropped, streaming an empty card. Live must
    /// emit the SAME string the history parser stores so the background-task
    /// card renders identically before and after a reload.
    #[test]
    fn grok_live_tool_output_emits_task_output_envelope() {
        let raw = serde_json::json!({
            "type": "TaskOutput",
            "Result": {
                "task_id": "term_b0d",
                "command": "/bin/bash -lc 'pnpm dev'",
                "status": "failed",
                "exit_code": 1,
                "output": "boom",
            },
        });
        let live = grok_live_tool_output(&None, &Some(raw.clone())).expect("envelope emitted");
        assert_eq!(
            live,
            crate::parsers::grok::grok_task_output_envelope(&raw).unwrap(),
            "live and history must hand the frontend the same string"
        );
        let parsed: serde_json::Value = serde_json::from_str(&live).unwrap();
        assert_eq!(parsed["Result"]["exit_code"], 1);
        // A poll that DOES carry clean content keeps content's precedence.
        assert_eq!(
            grok_live_tool_output(&Some("已完成".to_string()), &Some(raw)),
            None
        );
    }

    #[test]
    fn grok_live_tool_output_none_without_usable_string() {
        // Object without `output_for_prompt` (only the byte-array `output`).
        let no_prompt = Some(serde_json::json!({
            "output": [10, 62],
            "exit_code": 0,
            "command": "x",
        }));
        assert_eq!(grok_live_tool_output(&None, &no_prompt), None);
        // Non-object rawOutput.
        assert_eq!(
            grok_live_tool_output(&None, &Some(serde_json::json!("a string"))),
            None
        );
        // Absent rawOutput.
        assert_eq!(grok_live_tool_output(&None, &None), None);
    }

    /// A finished Grok terminal `tool_call_update` carries the readable output in
    /// BOTH the `content[]` channel and a structured `rawOutput` object (its
    /// `output` field a byte array, text only under `output_for_prompt`).
    /// Regression: the live path must NOT ship the stringified object as
    /// `raw_output` (which shadows `content` and renders empty) — it emits `None`
    /// so the frontend renders the clean `content`.
    #[tokio::test]
    async fn grok_terminal_update_emits_content_not_raw_output_blob() {
        let st = SessionState::new(
            "conn-grok".to_string(),
            AgentType::Grok,
            None,
            "win".to_string(),
            None,
        );
        let state = Arc::new(RwLock::new(st));
        let emitter = EventEmitter::Noop;
        let mut cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();

        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "status": "completed",
            "content": [{"type": "content", "content": {"type": "text", "text": "\n> build\nbuild ok\n"}}],
            "rawOutput": {
                "output": [10, 62, 32],
                "output_for_prompt": "exit: 0\n\nbuild ok",
                "exit_code": 0,
                "command": "pnpm build",
            },
        }))
        .expect("valid tool_call_update wire shape");

        emit_conversation_update(
            &state,
            &emitter,
            AgentType::Grok,
            update,
            None,
            &mut cache,
            &mut cb,
            None,
        )
        .await;

        let guard = state.read().await;
        let events = guard.recent_events_after(0).expect("events recorded");
        let (raw_output, content) = events
            .iter()
            .find_map(|e| match &e.payload {
                AcpEvent::ToolCallUpdate {
                    raw_output,
                    content,
                    ..
                } => Some((raw_output.clone(), content.clone())),
                _ => None,
            })
            .expect("a tool_call_update event is emitted");

        assert!(
            raw_output.is_none(),
            "Grok must not ship the rawOutput object blob (it shadows content \
             and the terminal renderer drops it): {raw_output:?}"
        );
        assert!(
            content.as_deref().is_some_and(|c| c.contains("build ok")),
            "the clean content channel carries the executed command's output: {content:?}"
        );
    }

    /// Contrast guard: the Grok-only extraction must not change other agents.
    /// A non-Grok agent that sends the same object-shaped `rawOutput` still gets
    /// it stringified into `raw_output` (existing `json_value_to_text` behavior).
    #[tokio::test]
    async fn non_grok_object_raw_output_is_stringified_unchanged() {
        let st = SessionState::new(
            "conn-claude".to_string(),
            AgentType::ClaudeCode,
            None,
            "win".to_string(),
            None,
        );
        let state = Arc::new(RwLock::new(st));
        let emitter = EventEmitter::Noop;
        let mut cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();

        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-1",
            "status": "completed",
            "rawOutput": {"output_for_prompt": "exit: 0\n\nok", "command": "x"},
        }))
        .expect("valid tool_call_update wire shape");

        emit_conversation_update(
            &state,
            &emitter,
            AgentType::ClaudeCode,
            update,
            None,
            &mut cache,
            &mut cb,
            None,
        )
        .await;

        let guard = state.read().await;
        let events = guard.recent_events_after(0).expect("events recorded");
        let raw_output = events
            .iter()
            .find_map(|e| match &e.payload {
                AcpEvent::ToolCallUpdate { raw_output, .. } => Some(raw_output.clone()),
                _ => None,
            })
            .expect("a tool_call_update event is emitted");
        assert!(
            raw_output.is_some(),
            "non-Grok agents keep the existing json_value_to_text behavior"
        );
    }

    #[test]
    fn unwrap_grok_use_tool_peels_mcp_envelope() {
        // Grok's `use_tool` envelope nests the real MCP tool name + args.
        let raw = serde_json::json!({
            "tool_name": "codeg-mcp__delegate_to_agent",
            "tool_input": {"agent_type": "codex", "task": "build", "working_dir": "/w"},
        });
        let (name, input) = unwrap_grok_use_tool(Some(&raw)).expect("envelope peels");
        assert_eq!(name, "codeg-mcp__delegate_to_agent");
        assert_eq!(input.get("task").and_then(|v| v.as_str()), Some("build"));
        assert_eq!(
            input.get("agent_type").and_then(|v| v.as_str()),
            Some("codex")
        );
    }

    #[test]
    fn unwrap_grok_use_tool_ignores_native_tools() {
        // Native Grok tools carry args directly (no tool_name/tool_input shape) —
        // they must pass through untouched.
        let terminal = serde_json::json!({"command": "pnpm build"});
        assert!(unwrap_grok_use_tool(Some(&terminal)).is_none());
        // Missing tool_input.
        assert!(unwrap_grok_use_tool(Some(&serde_json::json!({"tool_name": "x"}))).is_none());
        // Empty tool_name.
        assert!(unwrap_grok_use_tool(Some(
            &serde_json::json!({"tool_name": "", "tool_input": {}})
        ))
        .is_none());
        // Absent / non-object.
        assert!(unwrap_grok_use_tool(None).is_none());
        assert!(unwrap_grok_use_tool(Some(&serde_json::json!("s"))).is_none());
    }

    #[test]
    fn grok_mcp_output_text_extracts_result() {
        // `{type:MCP, output:{OkayOutput:"…"}}` — text is the first string value.
        let ok = serde_json::json!({
            "type": "MCP",
            "tool_name": "delegate_to_agent",
            "output": {"OkayOutput": "Delegation successful. task_id=abc-123."},
        });
        assert_eq!(
            grok_mcp_output_text(&ok).as_deref(),
            Some("Delegation successful. task_id=abc-123.")
        );
        // `output` may be a bare string.
        let bare = serde_json::json!({"type": "MCP", "output": "done"});
        assert_eq!(grok_mcp_output_text(&bare).as_deref(), Some("done"));
        // An empty-string sibling (sorted before the real key) must not shadow
        // the populated result.
        let empty_first = serde_json::json!({
            "type": "MCP",
            "output": {"AErr": "", "OkayOutput": "real result"},
        });
        assert_eq!(
            grok_mcp_output_text(&empty_first).as_deref(),
            Some("real result")
        );
        // A pure error variant (any `*Output` key) is surfaced too.
        let err = serde_json::json!({"type": "MCP", "output": {"ErrOutput": "boom"}});
        assert_eq!(grok_mcp_output_text(&err).as_deref(), Some("boom"));
        // Non-MCP rawOutput → None (caller falls through to output_for_prompt).
        let bash = serde_json::json!({"type": "Bash", "output_for_prompt": "ok"});
        assert_eq!(grok_mcp_output_text(&bash), None);
    }

    #[test]
    fn cursor_companion_title_resolves_delegate_ack() {
        // The broker's running ack (broker.rs::running_ack) — leading
        // whitespace tolerated, the prefix is the contract.
        let ack = "Delegation successful. task_id=799467c7-0188-4e7a-b5ef-241d4b141a83. \
                   Call get_delegation_status with this id in the task_ids array.";
        assert_eq!(
            cursor_companion_title_from_content(Some(ack)),
            Some("codeg-mcp__delegate_to_agent")
        );
        assert_eq!(
            cursor_companion_title_from_content(Some(&format!("  {ack}"))),
            Some("codeg-mcp__delegate_to_agent")
        );
    }

    #[test]
    fn cursor_companion_title_resolves_status_report() {
        // Real-device shape: companion.rs::render_batch_report's compact JSON.
        let report = r#"{"tasks":[{"agent_type":"claude_code","child_conversation_id":1576,"duration_ms":27288,"status":"completed","task_id":"799467c7-0188-4e7a-b5ef-241d4b141a83","text":"done"}]}"#;
        assert_eq!(
            cursor_companion_title_from_content(Some(report)),
            Some("codeg-mcp__get_delegation_status")
        );
        // Mixed batch with a running item still resolves.
        let mixed =
            r#"{"tasks":[{"task_id":"a","status":"running"},{"task_id":"b","status":"unknown"}]}"#;
        assert_eq!(
            cursor_companion_title_from_content(Some(mixed)),
            Some("codeg-mcp__get_delegation_status")
        );
    }

    #[test]
    fn cursor_companion_title_rejects_lookalikes() {
        // Foreign task-manager output: status outside the report vocabulary.
        let foreign =
            r#"{"tasks":[{"task_id":"T-1","status":"todo"},{"task_id":"T-2","status":"done"}]}"#;
        assert_eq!(cursor_companion_title_from_content(Some(foreign)), None);
        // Item missing task_id.
        let missing = r#"{"tasks":[{"status":"completed"}]}"#;
        assert_eq!(cursor_companion_title_from_content(Some(missing)), None);
        // Empty batch carries nothing to verify — leave the title alone.
        assert_eq!(
            cursor_companion_title_from_content(Some(r#"{"tasks":[]}"#)),
            None
        );
        // Plain text / absent / non-JSON.
        assert_eq!(cursor_companion_title_from_content(Some("ls -la ok")), None);
        assert_eq!(cursor_companion_title_from_content(None), None);
        // Ack prefix must match from the start, not mid-string.
        assert_eq!(
            cursor_companion_title_from_content(Some("Note: Delegation successful. task_id=x.")),
            None
        );
    }

    #[test]
    fn grok_live_tool_output_recovers_mcp_result() {
        // An MCP call (delegate ack) has empty content and no output_for_prompt;
        // the readable text lives under `output.OkayOutput`.
        let raw = Some(serde_json::json!({
            "type": "MCP",
            "tool_name": "delegate_to_agent",
            "server_name": "codeg-mcp",
            "output": {"OkayOutput": "Delegation successful. task_id=2dc85849-5426."},
        }));
        assert_eq!(
            grok_live_tool_output(&None, &raw).as_deref(),
            Some("Delegation successful. task_id=2dc85849-5426.")
        );
    }

    /// Grok wraps `delegate_to_agent` in a `use_tool` envelope. The live path must
    /// peel it so the emitted event carries the MCP tool name as its title and the
    /// real `{agent_type, task}` as raw_input — the exact shape the delegation
    /// broker (`lifecycle.rs`) correlates on and the frontend classifies into the
    /// delegation card — and must surface the MCP ack (carrying `task_id`) as
    /// output instead of dropping it.
    #[tokio::test]
    async fn grok_use_tool_delegate_unwraps_to_direct_mcp_call() {
        let st = SessionState::new(
            "conn-grok".to_string(),
            AgentType::Grok,
            None,
            "win".to_string(),
            None,
        );
        let state = Arc::new(RwLock::new(st));
        let emitter = EventEmitter::Noop;
        let mut cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();

        // Initial tool_call carries the use_tool envelope (real Grok wire shape —
        // no kind/status on the update object; they default).
        let call: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-d",
            "title": "use_tool",
            "rawInput": {
                "tool_name": "codeg-mcp__delegate_to_agent",
                "tool_input": {"agent_type": "codex", "working_dir": "/w", "task": "run build"},
            },
        }))
        .expect("valid tool_call wire shape");
        emit_conversation_update(
            &state,
            &emitter,
            AgentType::Grok,
            call,
            None,
            &mut cache,
            &mut cb,
            None,
        )
        .await;

        // The ack arrives on the completed update as an MCP rawOutput. Real Grok
        // updates re-send the generic `use_tool` wrapper title and carry NO
        // raw_input — the recorded override must re-assert the peeled name so the
        // frontend reducer doesn't revert the delegation card to a generic tool.
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-d",
            "title": "use_tool",
            "status": "completed",
            "rawOutput": {
                "type": "MCP",
                "tool_name": "delegate_to_agent",
                "server_name": "codeg-mcp",
                "output": {"OkayOutput": "Delegation successful. task_id=2dc85849-5426-44f7."},
            },
        }))
        .expect("valid tool_call_update wire shape");
        emit_conversation_update(
            &state,
            &emitter,
            AgentType::Grok,
            update,
            None,
            &mut cache,
            &mut cb,
            None,
        )
        .await;

        let guard = state.read().await;
        let events = guard.recent_events_after(0).expect("events recorded");

        // Initial ToolCall: title unwrapped to the MCP tool name; raw_input the
        // real delegation args (the `use_tool` wrapper gone).
        let (title, raw_input) = events
            .iter()
            .find_map(|e| match &e.payload {
                AcpEvent::ToolCall {
                    title, raw_input, ..
                } => Some((title.clone(), raw_input.clone())),
                _ => None,
            })
            .expect("a tool_call event is emitted");
        assert_eq!(title, "codeg-mcp__delegate_to_agent");
        let raw_input = raw_input.expect("raw_input present after unwrap");
        assert!(
            raw_input.contains("\"agent_type\":\"codex\""),
            "raw_input carries agent_type: {raw_input}"
        );
        assert!(
            raw_input.contains("\"task\":\"run build\""),
            "raw_input carries task: {raw_input}"
        );
        assert!(
            !raw_input.contains("tool_input"),
            "the use_tool wrapper is peeled: {raw_input}"
        );

        // Update: the MCP ack (with task_id) surfaces as output, AND the emitted
        // title re-asserts the peeled name — the sparse `use_tool` wrapper title
        // must not win.
        let (upd_title, raw_output) = events
            .iter()
            .find_map(|e| match &e.payload {
                AcpEvent::ToolCallUpdate {
                    title, raw_output, ..
                } => raw_output.clone().map(|o| (title.clone(), o)),
                _ => None,
            })
            .expect("a tool_call_update with output is emitted");
        assert!(
            raw_output.contains("task_id=2dc85849"),
            "the delegate ack (with task_id) surfaces as output: {raw_output}"
        );
        assert_eq!(
            upd_title.as_deref(),
            Some("codeg-mcp__delegate_to_agent"),
            "the sparse-update wrapper title is overridden by the recorded name"
        );
        // No emitted event ever ships the generic `use_tool` wrapper title.
        assert!(
            events.iter().all(|e| !matches!(
                &e.payload,
                AcpEvent::ToolCall { title, .. } if title == "use_tool"
            ) && !matches!(
                &e.payload,
                AcpEvent::ToolCallUpdate { title: Some(t), .. } if t == "use_tool"
            )),
            "no event ships the generic use_tool wrapper title"
        );
    }

    /// The unwrap is symmetric on the ToolCallUpdate arm: an update that itself
    /// carries the `use_tool` envelope (rawInput) is peeled the same way — title →
    /// MCP name, raw_input → the inner args.
    #[tokio::test]
    async fn grok_use_tool_envelope_on_update_is_unwrapped() {
        let st = SessionState::new(
            "conn-grok".to_string(),
            AgentType::Grok,
            None,
            "win".to_string(),
            None,
        );
        let state = Arc::new(RwLock::new(st));
        let emitter = EventEmitter::Noop;
        let mut cache = ToolCallOutputCache::default();
        let mut cb = CodeBuddyLiveState::default();

        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call-u",
            "title": "use_tool",
            "status": "in_progress",
            "rawInput": {
                "tool_name": "codeg-mcp__cancel_delegation",
                "tool_input": {"task_id": "abc-123"},
            },
        }))
        .expect("valid tool_call_update wire shape");
        emit_conversation_update(
            &state,
            &emitter,
            AgentType::Grok,
            update,
            None,
            &mut cache,
            &mut cb,
            None,
        )
        .await;

        let guard = state.read().await;
        let events = guard.recent_events_after(0).expect("events recorded");
        let (title, raw_input) = events
            .iter()
            .find_map(|e| match &e.payload {
                AcpEvent::ToolCallUpdate {
                    title, raw_input, ..
                } => Some((title.clone(), raw_input.clone())),
                _ => None,
            })
            .expect("a tool_call_update event is emitted");
        assert_eq!(title.as_deref(), Some("codeg-mcp__cancel_delegation"));
        let raw_input = raw_input.expect("raw_input present after unwrap");
        assert!(
            raw_input.contains("\"task_id\":\"abc-123\""),
            "inner args surface as raw_input: {raw_input}"
        );
        assert!(
            !raw_input.contains("tool_input"),
            "the use_tool wrapper is peeled: {raw_input}"
        );
    }

    #[test]
    fn canonical_spec_to_mcp_server_http_with_headers() {
        let spec = serde_json::json!({
            "type": "http",
            "url": "https://example.com/mcp",
            "headers": {"Authorization": "Bearer token"},
        });
        let server = canonical_spec_to_mcp_server("remote", &spec).expect("http spec should map");
        match server {
            McpServer::Http(s) => {
                assert_eq!(s.url, "https://example.com/mcp");
                assert_eq!(s.headers.len(), 1);
                assert_eq!(s.headers[0].name, "Authorization");
            }
            other => panic!("expected Http variant, got {other:?}"),
        }
    }

    #[test]
    fn canonical_spec_to_mcp_server_rejects_unknown_type() {
        let spec = serde_json::json!({"type": "websocket", "url": "wss://x"});
        assert!(canonical_spec_to_mcp_server("x", &spec).is_err());
    }

    #[test]
    fn stdio_server_serializes_to_acp_wire_format() {
        // Replicates the Figma MCP entry shipped to the agent and asserts the
        // exact JSON shape claude-agent-acp expects (no `type` tag for stdio,
        // env as [{name, value}] array, command as a string path).
        let spec = serde_json::json!({
            "type": "stdio",
            "command": "/usr/local/bin/npx",
            "args": ["-y", "@mcp_hub_org/cli@latest", "run", "figma-developer-mcp"],
        });
        let server = canonical_spec_to_mcp_server("figma", &spec).expect("stdio spec should map");
        let json = serde_json::to_value(&server).expect("server should serialize");
        assert_eq!(json["name"], "figma");
        assert_eq!(json["command"], "/usr/local/bin/npx");
        assert_eq!(json["args"][0], "-y");
        assert_eq!(json["args"][1], "@mcp_hub_org/cli@latest");
        assert!(
            json.get("type").is_none(),
            "stdio variant must serialize without a `type` tag (claude-agent-acp \
             treats absence-of-type as stdio); got {json:#?}"
        );
    }

    // ─── ToolCallOutputCache ────────────────────────────────────────────

    #[test]
    fn cache_first_update_emits_full_replace() {
        let mut cache = ToolCallOutputCache::default();
        let (payload, append) = cache.consume("t1", "hello world").expect("should emit");
        assert_eq!(payload, "hello world");
        assert!(!append, "first emit must be replacement");
    }

    #[test]
    fn cache_repeated_identical_snapshot_is_noop() {
        let mut cache = ToolCallOutputCache::default();
        cache.consume("t1", "same").unwrap();
        assert!(
            cache.consume("t1", "same").is_none(),
            "identical snapshot must not emit"
        );
    }

    #[test]
    fn cache_prefix_extension_emits_suffix_with_append() {
        let mut cache = ToolCallOutputCache::default();
        cache.consume("t1", "line-1\n").unwrap();
        let (payload, append) = cache
            .consume("t1", "line-1\nline-2\n")
            .expect("should emit");
        assert_eq!(payload, "line-2\n");
        assert!(append, "prefix extension must emit with append=true");
    }

    #[test]
    fn cache_divergent_snapshot_falls_back_to_replace() {
        let mut cache = ToolCallOutputCache::default();
        cache.consume("t1", "hello world").unwrap();
        let (payload, append) = cache.consume("t1", "foo bar baz").expect("should emit");
        assert_eq!(payload, "foo bar baz");
        assert!(!append, "non-extension snapshot must replace");
    }

    #[test]
    fn cache_tracks_extensions_past_cached_tail_boundary() {
        // Regression test for the original bug: when cumulative raw_output
        // exceeds MAX_CACHED_TAIL_BYTES, subsequent extensions must still be
        // detectable by comparing the cached tail against the expected
        // offset in the incoming snapshot.
        let mut cache = ToolCallOutputCache::default();
        // First snapshot: 10 KB of 'a' + unique 4 KB marker at the end.
        let prefix = "a".repeat(10 * 1024);
        let marker = "M".repeat(4 * 1024);
        let first = format!("{prefix}{marker}");
        cache.consume("t1", &first).unwrap();

        // Second snapshot extends first by 16 KB of 'Z'.
        let delta = "Z".repeat(16 * 1024);
        let second = format!("{first}{delta}");
        let (payload, append) = cache.consume("t1", &second).expect("should emit");
        assert!(
            append,
            "extension beyond cached tail must still be detected"
        );
        // The emitted payload should carry the delta (or its tail when
        // truncated at MAX_SINGLE_EMIT_BYTES). For a 16 KB delta that's
        // well below the 64 KB cap, we expect it verbatim.
        assert_eq!(payload, delta);
    }

    #[test]
    fn cache_extension_larger_than_emit_cap_gets_truncated() {
        let mut cache = ToolCallOutputCache::default();
        cache.consume("t1", "seed").unwrap();
        // Build a delta much larger than MAX_SINGLE_EMIT_BYTES.
        let big_delta = "X".repeat(MAX_SINGLE_EMIT_BYTES * 2);
        let second = format!("seed{big_delta}");
        let (payload, append) = cache.consume("t1", &second).expect("should emit");
        assert!(append);
        assert!(
            payload.starts_with(TRUNCATION_MARKER),
            "oversized delta must be prefixed with truncation marker"
        );
        // Payload length: marker + at most MAX_SINGLE_EMIT_BYTES of tail.
        assert!(payload.len() <= TRUNCATION_MARKER.len() + MAX_SINGLE_EMIT_BYTES);
    }

    #[test]
    fn cache_respects_utf8_char_boundary_on_truncation() {
        let mut cache = ToolCallOutputCache::default();
        // Single first-update whose byte length forces truncation at a
        // position that would otherwise fall mid-codepoint. 中 is 3 bytes
        // (E4 B8 AD) and MAX_SINGLE_EMIT_BYTES (65536) is not a multiple
        // of 3, so naïve byte slicing would land mid-char.
        let chinese_block = "中".repeat((MAX_SINGLE_EMIT_BYTES / 3) + 100);
        let (payload, _append) = cache.consume("t1", &chinese_block).expect("should emit");
        // Payload must start with the truncation marker (since size > cap).
        assert!(
            payload.starts_with(TRUNCATION_MARKER),
            "oversized snapshot must be truncated"
        );
        // Body after the marker must be valid UTF-8 consisting only of 中.
        let body = &payload[TRUNCATION_MARKER.len()..];
        assert!(!body.is_empty());
        assert!(
            body.chars().all(|c| c == '中'),
            "truncation boundary must land on a UTF-8 codepoint edge"
        );
    }

    #[test]
    fn cache_final_status_clears_entry() {
        let mut cache = ToolCallOutputCache::default();
        cache.consume("t1", "hello").unwrap();
        assert!(cache.entries.contains_key("t1"));
        cache.remove_if_final("t1", Some("completed"));
        assert!(!cache.entries.contains_key("t1"));

        cache.consume("t2", "x").unwrap();
        cache.remove_if_final("t2", Some("cancelled"));
        assert!(!cache.entries.contains_key("t2"));

        cache.consume("t3", "x").unwrap();
        cache.remove_if_final("t3", Some("in_progress"));
        assert!(
            cache.entries.contains_key("t3"),
            "in-progress status must not clear cache"
        );
    }

    #[test]
    fn cache_enforces_entry_cap_via_fifo_eviction() {
        let mut cache = ToolCallOutputCache::default();
        for i in 0..(MAX_CACHE_ENTRIES + 50) {
            cache.consume(&format!("tool-{i}"), "body").unwrap();
        }
        assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
        // Oldest entries should have been evicted; newest must still exist.
        assert!(!cache.entries.contains_key("tool-0"));
        assert!(cache
            .entries
            .contains_key(&format!("tool-{}", MAX_CACHE_ENTRIES + 49)));
    }

    #[test]
    fn cache_seed_always_replaces_and_caches() {
        let mut cache = ToolCallOutputCache::default();
        cache.consume("t1", "stale").unwrap();
        // A hypothetical replay would send another ToolCall for the same
        // id — seed() must install the new snapshot without trying to
        // diff against the stale prior entry.
        let payload = cache.seed("t1", "fresh").expect("seed emits");
        assert_eq!(payload, "fresh");
        // Next consume should diff against "fresh", not "stale".
        let (p2, append) = cache.consume("t1", "fresh+more").expect("emit");
        assert!(append, "should detect extension of freshly seeded entry");
        assert_eq!(p2, "+more");
    }

    // ─── trim_partial_ansi_tail ─────────────────────────────────────────

    #[test]
    fn ansi_trim_leaves_pure_text_unchanged() {
        assert_eq!(trim_partial_ansi_tail("plain text"), "plain text");
    }

    #[test]
    fn ansi_trim_keeps_completed_sequences() {
        let s = "\x1b[31mRED\x1b[0m done";
        assert_eq!(trim_partial_ansi_tail(s), s);
    }

    #[test]
    fn ansi_trim_cuts_unterminated_trailing_sequence() {
        let s = "hello \x1b[31";
        assert_eq!(trim_partial_ansi_tail(s), "hello ");
    }

    #[test]
    fn ansi_trim_handles_bare_escape_at_end() {
        let s = "hello\x1b";
        assert_eq!(trim_partial_ansi_tail(s), "hello");
    }

    // ─── truncate_tail_at_char_boundary ─────────────────────────────────

    #[test]
    fn truncate_under_cap_returns_as_is() {
        assert_eq!(truncate_tail_at_char_boundary("abc", 10), "abc");
    }

    #[test]
    fn truncate_returns_tail_on_overflow() {
        assert_eq!(truncate_tail_at_char_boundary("abcdef", 3), "def");
    }

    #[test]
    fn truncate_respects_multibyte_utf8_boundary() {
        // "中中中" is 9 bytes; asking for 4 bytes would land mid-char.
        let s = "中中中";
        let out = truncate_tail_at_char_boundary(s, 4);
        // Must be valid UTF-8 (indexing an invalid boundary would have
        // panicked at slicing time).
        assert!(out.chars().all(|c| c == '中'));
        assert!(out.len() <= 6); // at most 2 chars (6 bytes)
    }

    // ─── is_subagent_invocation ─────────────────────────────────

    #[test]
    fn subagent_detects_opencode_with_subagent_type_regardless_of_title() {
        // OpenCode's ACP title is the user-facing description (e.g. the
        // task's `description` field), NOT the internal tool name. The
        // historical-parser equivalent at parsers/opencode.rs:425-429
        // anchors on `tool == "task"`, which we can't replicate here
        // because ACP doesn't expose the internal tool name — so we rely
        // solely on agent_type + subagent_type. Verify the detection
        // triggers regardless of the title shape.
        let input = Some(r#"{"subagent_type":"researcher","prompt":"x"}"#.to_string());
        assert!(is_subagent_invocation(AgentType::OpenCode, &input));
    }

    #[test]
    fn subagent_gates_on_supported_agent_types() {
        // OpenCode and CodeBuddy both rewrite a `subagent_type`-bearing call to
        // the Agent card; other agents stay excluded so a generic `subagent_type`
        // field never triggers a cross-agent collision.
        let input = Some(r#"{"subagent_type":"x"}"#.to_string());
        assert!(is_subagent_invocation(AgentType::OpenCode, &input));
        assert!(is_subagent_invocation(AgentType::CodeBuddy, &input));
        assert!(!is_subagent_invocation(AgentType::ClaudeCode, &input));
        assert!(!is_subagent_invocation(AgentType::Codex, &input));
    }

    #[test]
    fn subagent_rejects_empty_or_non_string_subagent_type() {
        for raw in [
            r#"{"subagent_type":""}"#,
            r#"{"subagent_type":null}"#,
            r#"{"subagent_type":42}"#,
            r#"{"subagent_type":["a"]}"#,
        ] {
            assert!(
                !is_subagent_invocation(AgentType::OpenCode, &Some(raw.to_string())),
                "expected false for raw_input={raw}"
            );
        }
    }

    #[test]
    fn subagent_rejects_none_malformed_or_non_object_root() {
        assert!(!is_subagent_invocation(AgentType::OpenCode, &None));
        for raw in [
            "not json",
            "{}",
            r#""string""#,
            "[1,2,3]",
            // Substring guard short-circuits this before JSON parsing;
            // verify both code paths agree on the result.
            "12345",
            // Field name present as substring but not as object key — the
            // substring guard lets this through but JSON parsing rejects
            // it (the value is a number, not an object with that key).
            r#"{"note":"contains the word subagent_type as text"}"#,
        ] {
            assert!(
                !is_subagent_invocation(AgentType::OpenCode, &Some(raw.to_string())),
                "expected false for raw_input={raw}"
            );
        }
    }

    #[test]
    fn subagent_rejects_when_subagent_type_appears_only_as_value() {
        // The cheap substring guard lets this through (the bytes
        // "subagent_type" appear in the JSON text), but JSON parsing
        // correctly finds no top-level `subagent_type` key, so the helper
        // returns false. Regression guard against any future "optimisation"
        // that conflates the substring check with the field check.
        let input = Some(r#"{"description":"use subagent_type=foo"}"#.to_string());
        assert!(!is_subagent_invocation(AgentType::OpenCode, &input));
    }

    #[test]
    fn subagent_detects_when_raw_input_has_other_fields_ahead_of_subagent_type() {
        // Mirrors the OpenCode wire shape `{description, prompt, subagent_type}`
        // — the field order in JSON doesn't matter, but exercise a realistic
        // payload (with non-trivial sizes) end-to-end.
        let input = Some(
            r#"{"description":"Explore project structure","prompt":"Look at the repo layout and summarise the stack.","subagent_type":"general-purpose"}"#
                .to_string(),
        );
        assert!(is_subagent_invocation(AgentType::OpenCode, &input));
    }

    // ─── codebuddy_deferred_tool_name ────────────────────────────────────

    #[test]
    fn deferred_unwraps_codebuddy_mcp_tool_name() {
        // CodeBuddy wraps MCP calls as `{toolName, params}` via DeferExecuteTool.
        let input = Some(
            r#"{"params":{"agent_type":"codex","task":"build"},"toolName":"mcp__codeg-mcp__delegate_to_agent"}"#
                .to_string(),
        );
        assert_eq!(
            codebuddy_deferred_tool_name(AgentType::CodeBuddy, &input).as_deref(),
            Some("mcp__codeg-mcp__delegate_to_agent")
        );
    }

    #[test]
    fn deferred_gates_on_codebuddy_and_shape() {
        let wrapped = Some(
            r#"{"params":{"task_id":"a"},"toolName":"mcp__codeg-mcp__cancel_delegation"}"#
                .to_string(),
        );
        // Only CodeBuddy is unwrapped.
        assert!(codebuddy_deferred_tool_name(AgentType::OpenCode, &wrapped).is_none());
        // Missing `params`, missing/blank `toolName`, or non-wrapper shapes → None.
        for raw in [
            r#"{"toolName":"mcp__codeg-mcp__delegate_to_agent"}"#, // no params
            r#"{"params":{"x":1},"toolName":""}"#,                 // blank toolName
            r#"{"params":{"x":1}}"#,                               // no toolName
            r#"{"command":"ls"}"#,                                 // plain tool
            "not json",
        ] {
            assert!(
                codebuddy_deferred_tool_name(AgentType::CodeBuddy, &Some(raw.to_string()))
                    .is_none(),
                "expected None for raw_input={raw}"
            );
        }
        assert!(codebuddy_deferred_tool_name(AgentType::CodeBuddy, &None).is_none());
    }

    // ─── unwrap_codebuddy_deferred_output ────────────────────────────────

    #[test]
    fn deferred_output_peels_codebuddy_content_wrapper() {
        // The exact live shape from the bug report: a `get_delegation_status`
        // batch result double-wrapped as a `{text,type}` content part, whose
        // inner `text` is the compact `{tasks:[...]}` JSON. Peeling it yields the
        // bare report JSON the frontend `parseStatusReports` already understands.
        let inner = r#"{"tasks":[{"status":"completed","task_id":"666da381","child_conversation_id":18,"text":"ok"}]}"#;
        let wrapped = serde_json::json!({ "text": inner, "type": "text" }).to_string();
        assert_eq!(
            unwrap_codebuddy_deferred_output(AgentType::CodeBuddy, &wrapped).as_deref(),
            Some(inner)
        );
    }

    #[test]
    fn deferred_output_gates_on_codebuddy_and_wrapper_shape() {
        let wrapped =
            serde_json::json!({ "text": "{\"status\":\"running\"}", "type": "text" }).to_string();
        // Only CodeBuddy is unwrapped — the wrapper is a CodeBuddy quirk.
        assert!(unwrap_codebuddy_deferred_output(AgentType::OpenCode, &wrapped).is_none());
        assert!(unwrap_codebuddy_deferred_output(AgentType::ClaudeCode, &wrapped).is_none());
        for raw in [
            // Plain (non-deferred) tool output passes through untouched.
            "build succeeded",
            // A delegation report has no top-level `type` discriminator.
            r#"{"status":"completed","task_id":"x","text":"done"}"#,
            // A batch envelope is already in the bare shape — no `type` either.
            r#"{"tasks":[{"status":"completed","task_id":"x"}]}"#,
            // Wrong discriminator value.
            r#"{"type":"image","text":"x"}"#,
            // Missing inner `text`.
            r#"{"type":"text"}"#,
            "not json",
        ] {
            assert!(
                unwrap_codebuddy_deferred_output(AgentType::CodeBuddy, raw).is_none(),
                "expected pass-through (None) for output={raw}"
            );
        }
    }

    // ─── resolve_rewritten_title (title state across updates) ────────────

    #[test]
    fn rewritten_title_persists_across_status_only_updates() {
        let mut overrides: HashMap<String, String> = HashMap::new();
        let subagent = Some(
            r#"{"description":"Run pnpm build","subagent_type":"general-purpose"}"#.to_string(),
        );
        // Initial event carrying the subagent marker → "agent", recorded.
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &subagent,
                "tc1",
                false,
                false,
                &mut overrides
            )
            .as_deref(),
            Some("agent")
        );
        // The bug: a later status-only update lost the marker (raw_input None).
        // The override must be RE-ASSERTED, not downgraded to the event's title.
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &None,
                "tc1",
                true,
                false,
                &mut overrides
            )
            .as_deref(),
            Some("agent"),
            "a status-only update must not downgrade the Agent card mid-stream"
        );
        // Even an update whose raw_input looks like a different tool keeps it.
        let bash = Some(r#"{"command":"ls"}"#.to_string());
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &bash,
                "tc1",
                true,
                false,
                &mut overrides
            )
            .as_deref(),
            Some("agent")
        );
        // A never-classified tool call returns None → caller uses its own title.
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &None,
                "tc2",
                true,
                false,
                &mut overrides
            ),
            None
        );
        // Deferred MCP tool: inner name recorded, then re-asserted on a bare update.
        let deferred = Some(
            r#"{"params":{"agent_type":"codex","task":"x"},"toolName":"mcp__codeg-mcp__delegate_to_agent"}"#
                .to_string(),
        );
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &deferred,
                "tc3",
                false,
                false,
                &mut overrides
            )
            .as_deref(),
            Some("mcp__codeg-mcp__delegate_to_agent")
        );
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &None,
                "tc3",
                true,
                false,
                &mut overrides
            )
            .as_deref(),
            Some("mcp__codeg-mcp__delegate_to_agent")
        );
        // Non-CodeBuddy agent with no prior classification: never rewritten.
        assert_eq!(
            resolve_rewritten_title(
                AgentType::OpenCode,
                &None,
                "tc9",
                true,
                false,
                &mut overrides
            ),
            None
        );
    }

    // ─── codebuddy_meta_marks_subagent ───────────────────────────────────

    #[test]
    fn meta_marks_subagent_reads_codebuddy_keys() {
        // Any one of the three CodeBuddy sub-agent markers is sufficient.
        let tool_name = serde_json::json!({ "codebuddy.ai/toolName": "Agent" });
        let is_sub = serde_json::json!({ "codebuddy.ai/isSubagent": true });
        let sub_type = serde_json::json!({ "codebuddy.ai/subagentType": "general-purpose" });
        for meta in [&tool_name, &is_sub, &sub_type] {
            assert!(codebuddy_meta_marks_subagent(
                AgentType::CodeBuddy,
                meta.as_object()
            ));
        }
        // Gated on CodeBuddy: the generic `codebuddy.ai/*` keys can't classify
        // another agent.
        assert!(!codebuddy_meta_marks_subagent(
            AgentType::OpenCode,
            tool_name.as_object()
        ));
        // Negative shapes: non-Agent toolName, empty subagentType, absent meta.
        let other = serde_json::json!({
            "codebuddy.ai/toolName": "Bash",
            "codebuddy.ai/subagentType": "",
            "codebuddy.ai/isSubagent": false,
        });
        assert!(!codebuddy_meta_marks_subagent(
            AgentType::CodeBuddy,
            other.as_object()
        ));
        assert!(!codebuddy_meta_marks_subagent(AgentType::CodeBuddy, None));
    }

    #[test]
    fn rewritten_title_fires_on_meta_before_raw_input() {
        let mut overrides: HashMap<String, String> = HashMap::new();
        // Frame 1: `raw_input` has NO `subagent_type` yet, but `_meta` already
        // marks it (the early, reliable signal). Title must already be "agent".
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &None,
                "tc1",
                false,
                true,
                &mut overrides
            )
            .as_deref(),
            Some("agent")
        );
        // Later sparse frames carry NEITHER signal — the override is re-asserted,
        // so the pill never flickers back to a generic tool mid-stream.
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &None,
                "tc1",
                true,
                false,
                &mut overrides
            )
            .as_deref(),
            Some("agent"),
            "meta-classified Agent pill must stay 'agent' across signal-less frames"
        );
        // DeferExecuteTool still wins over the meta path (distinct mechanism).
        let deferred = Some(
            r#"{"params":{"agent_type":"codex","task":"x"},"toolName":"mcp__codeg-mcp__delegate_to_agent"}"#
                .to_string(),
        );
        assert_eq!(
            resolve_rewritten_title(
                AgentType::CodeBuddy,
                &deferred,
                "tc2",
                false,
                false,
                &mut overrides
            )
            .as_deref(),
            Some("mcp__codeg-mcp__delegate_to_agent")
        );
    }

    // ─── track_subagent_window / should_suppress_subagent_chunk ──────────

    #[test]
    fn subagent_window_opens_and_closes_by_status() {
        let mut open: HashSet<String> = HashSet::new();
        let mut closed: HashSet<String> = HashSet::new();
        let fg = false; // foreground (not background)
                        // A non-final foreground agent frame opens the window.
        track_subagent_window(
            AgentType::CodeBuddy,
            true,
            fg,
            Some("in_progress"),
            "a",
            &mut open,
            &mut closed,
        );
        assert!(open.contains("a"));
        // A final frame closes it.
        track_subagent_window(
            AgentType::CodeBuddy,
            true,
            fg,
            Some("completed"),
            "a",
            &mut open,
            &mut closed,
        );
        assert!(!open.contains("a"));
        // A stray late non-final frame must NOT re-open a finished sub-agent.
        track_subagent_window(
            AgentType::CodeBuddy,
            true,
            fg,
            Some("in_progress"),
            "a",
            &mut open,
            &mut closed,
        );
        assert!(!open.contains("a"), "completed sub-agent must not re-open");
        // Non-agent tool calls never enter the window.
        track_subagent_window(
            AgentType::CodeBuddy,
            false,
            fg,
            Some("in_progress"),
            "b",
            &mut open,
            &mut closed,
        );
        assert!(!open.contains("b"));
        // Other agents are inert.
        track_subagent_window(
            AgentType::OpenCode,
            true,
            fg,
            Some("in_progress"),
            "c",
            &mut open,
            &mut closed,
        );
        assert!(!open.contains("c"));
    }

    #[test]
    fn subagent_window_excludes_background_subagents() {
        // A BACKGROUND sub-agent runs concurrently with the main agent, so it must
        // never open the suppression window — otherwise interleaved MAIN-agent
        // chunks would be wrongly dropped (the case the reviewer flagged).
        let mut open: HashSet<String> = HashSet::new();
        let mut closed: HashSet<String> = HashSet::new();
        track_subagent_window(
            AgentType::CodeBuddy,
            true,
            true, // is_background
            Some("in_progress"),
            "bg",
            &mut open,
            &mut closed,
        );
        assert!(
            !open.contains("bg"),
            "a background sub-agent must not open the window"
        );
        // And once known-background, a later (still non-final, no-longer-marked)
        // frame must not re-open it either.
        track_subagent_window(
            AgentType::CodeBuddy,
            true,
            false,
            Some("in_progress"),
            "bg",
            &mut open,
            &mut closed,
        );
        assert!(
            !open.contains("bg"),
            "a sub-agent seen as background must stay excluded"
        );
    }

    #[test]
    fn suppress_subagent_chunk_by_window_or_chunk_meta() {
        // Inside an open FOREGROUND window → suppress. This is safe because the
        // window only ever holds foreground (blocking) sub-agents, during which
        // the parent model is suspended — so every chunk in the window is the
        // sub-agent's, never main-agent output (background sub-agents, which could
        // interleave main output, are excluded from the window upstream).
        assert!(should_suppress_subagent_chunk(
            AgentType::CodeBuddy,
            true,
            None
        ));
        // Window closed and no chunk meta → emit (e.g. main-agent text before the
        // sub-agent opens or after it closes).
        assert!(!should_suppress_subagent_chunk(
            AgentType::CodeBuddy,
            false,
            None
        ));
        // Window closed but the chunk's own meta marks it → suppress (precision
        // supplement; never relied on alone).
        let sub = serde_json::json!({ "codebuddy.ai/isSubagent": true });
        let parented = serde_json::json!({ "codebuddy.ai/parentToolCallId": "call_x" });
        for meta in [&sub, &parented] {
            assert!(should_suppress_subagent_chunk(
                AgentType::CodeBuddy,
                false,
                meta.as_object()
            ));
        }
        // Other agents never suppress, even inside a (spurious) open window.
        assert!(!should_suppress_subagent_chunk(
            AgentType::OpenCode,
            true,
            None
        ));
    }

    #[test]
    fn claude_chunk_parent_reads_only_wellformed_claude_meta() {
        let valid = serde_json::json!({ "claudeCode": { "parentToolUseId": "toolu_01A" } });
        assert_eq!(
            claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, valid.as_object()),
            Some("toolu_01A".to_string())
        );
        // Gated on ClaudeCode — the same meta on another agent must not alias
        // into parented routing.
        assert_eq!(
            claude_chunk_parent_tool_use_id(AgentType::CodeBuddy, valid.as_object()),
            None
        );
        // Absent meta / absent key / wrong type / empty string → None.
        assert_eq!(
            claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, None),
            None
        );
        let no_key = serde_json::json!({ "claudeCode": { "toolName": "Agent" } });
        assert_eq!(
            claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, no_key.as_object()),
            None
        );
        let wrong_type = serde_json::json!({ "claudeCode": { "parentToolUseId": 42 } });
        assert_eq!(
            claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, wrong_type.as_object()),
            None
        );
        let empty = serde_json::json!({ "claudeCode": { "parentToolUseId": "" } });
        assert_eq!(
            claude_chunk_parent_tool_use_id(AgentType::ClaudeCode, empty.as_object()),
            None
        );
    }

    #[test]
    fn meta_marks_background_reads_codebuddy_flag() {
        let bg = serde_json::json!({ "codebuddy.ai/isBackground": true });
        let fg = serde_json::json!({ "codebuddy.ai/isBackground": false });
        assert!(codebuddy_meta_marks_background(
            AgentType::CodeBuddy,
            bg.as_object()
        ));
        // Foreground (the user-reported case), absent flag, and other agents → false.
        assert!(!codebuddy_meta_marks_background(
            AgentType::CodeBuddy,
            fg.as_object()
        ));
        assert!(!codebuddy_meta_marks_background(AgentType::CodeBuddy, None));
        assert!(!codebuddy_meta_marks_background(
            AgentType::OpenCode,
            bg.as_object()
        ));
    }

    // ─── inject_codeg_mcp: enabled=false short-circuit ──────────
    //
    // Guards the "default off" product contract: when the broker config has
    // `enabled: false` (the new production default for fresh installs), the
    // delegate-MCP injection must not push a server entry and must not
    // register a per-launch token. The early return at the top of
    // `inject_codeg_mcp` is the single chokepoint that keeps a
    // codeg-mcp stdio MCP out of every ACP session until the user
    // opts in via the settings panel.
    #[tokio::test]
    async fn complete_work_injection_skips_disabled_root_and_binds_v2_child() {
        use crate::acp::delegation::broker::{ConversationDepthLookup, DelegationBroker};
        use crate::acp::delegation::listener::TokenRegistry;
        use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
        use crate::acp::delegation::types::DelegationError;

        struct EmptyLookup;
        #[async_trait::async_trait]
        impl ConversationDepthLookup for EmptyLookup {
            async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
                Ok(None)
            }
        }

        let broker = Arc::new(DelegationBroker::new(
            Arc::new(MockSpawner::default()) as Arc<dyn ConnectionSpawner>,
            Arc::new(EmptyLookup) as Arc<dyn ConversationDepthLookup>,
        ));
        // No set_config call: broker carries its default config, which is
        // `enabled: false` after the product-default flip. This is the
        // exact state a fresh install reaches before the user touches the
        // settings panel. Feedback is likewise disabled by default, so with
        // BOTH features off the companion isn't injected at all.
        struct NoQuestions;
        #[async_trait::async_trait]
        impl crate::acp::question::SessionQuestionAccess for NoQuestions {
            async fn register_question(
                &self,
                _parent_connection_id: &str,
                _questions: Vec<crate::acp::question::QuestionSpec>,
            ) -> Option<crate::acp::question::RegisteredQuestion> {
                None
            }
            async fn cancel_question(&self, _parent_connection_id: &str, _question_id: &str) {}
            async fn cancel_questions_by_parent(&self, _parent_connection_id: &str) {}
        }
        struct NoPlanApprovals;
        #[async_trait::async_trait]
        impl crate::acp::plan_approval::SessionPlanApprovalAccess for NoPlanApprovals {
            async fn register_plan_approval(
                &self,
                _parent_connection_id: &str,
                _tool_call_id: String,
                _plan_markdown: String,
            ) -> Option<crate::acp::plan_approval::RegisteredPlanApproval> {
                None
            }
            async fn cancel_plan_approvals_by_parent(&self, _parent_connection_id: &str) {}
        }
        struct AllEnabled;
        #[async_trait::async_trait]
        impl AgentAvailabilityLookup for AllEnabled {
            async fn disabled_agent_wire_slugs(&self) -> Vec<String> {
                Vec::new()
            }
        }
        let injection = DelegationInjection {
            broker,
            continuation_coordinator: std::sync::Weak::new(),
            parent_connection_exit_causes: Arc::new(ParentConnectionExitCauses::default()),
            tokens: Arc::new(TokenRegistry::default()),
            leases: Arc::new(crate::acp::delegation::lease::CompanionLeaseRegistry::default()),
            socket_path: std::path::PathBuf::from("/tmp/codeg-mcp.sock"),
            agent_availability: Arc::new(AllEnabled) as Arc<dyn AgentAvailabilityLookup>,
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            questions: Arc::new(NoQuestions)
                as Arc<dyn crate::acp::question::SessionQuestionAccess>,
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics: Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
            plan_approvals: Arc::new(NoPlanApprovals)
                as Arc<dyn crate::acp::plan_approval::SessionPlanApprovalAccess>,
        };

        let mut servers: Vec<McpServer> = Vec::new();
        // Plan does not expose delegation; feedback/ask/sessions off → skip.
        let plan = native_plan(AgentType::Codex);
        let result = inject_codeg_mcp(
            &mut servers,
            &injection,
            "parent-conn",
            std::path::Path::new("/tmp"),
            AgentType::Codex,
            &plan,
            "test-incarnation",
            None,
        )
        .await;

        assert!(result.is_none(), "disabled broker must return None");
        assert!(
            servers.is_empty(),
            "disabled broker must not push any MCP server entry; got {servers:?}"
        );
        // Token registry stays untouched — no lookup should resolve to a
        // valid entry because nothing was registered.
        assert!(
            injection.tokens.lookup("any-token").await.is_none(),
            "disabled broker must not register a delegate token"
        );

        let prior_binary = std::env::var_os("CODEG_MCP_BIN");
        unsafe {
            std::env::set_var("CODEG_MCP_BIN", std::env::current_exe().unwrap());
        }
        let mut forced_child_plan = codeg_plan(AgentType::Codex);
        forced_child_plan.source = DelegationRouteSource::ForcedChild;
        let binding = crate::acp::delegation::workflow::WorkflowChildMcpBinding {
            task_id: "bound-task".into(),
            workflow_id: "bound-workflow".into(),
            protocol_version: 2,
            node_id: "bound-node".into(),
        };
        let injected = inject_codeg_mcp(
            &mut servers,
            &injection,
            "child-connection",
            std::path::Path::new("/tmp"),
            AgentType::Codex,
            &forced_child_plan,
            "child-incarnation",
            Some(&binding),
        )
        .await;
        unsafe {
            match prior_binary {
                Some(path) => std::env::set_var("CODEG_MCP_BIN", path),
                None => std::env::remove_var("CODEG_MCP_BIN"),
            }
        }
        let injected = injected.expect("forced child injection");

        let McpServer::Stdio(server) = &servers[0] else {
            panic!("expected stdio companion");
        };
        let argument_after = |name: &str| {
            let index = server.args.iter().position(|value| value == name).unwrap();
            server.args[index + 1].as_str()
        };
        assert!(!argument_after("--features")
            .split(',')
            .any(|feature| feature == "completion_v2"));
        assert_eq!(argument_after("--role"), "delegation_child");
        assert_eq!(
            argument_after("--connection-incarnation-id"),
            "child-incarnation"
        );
        let token = injection.tokens.lookup(&injected.token).await.unwrap();
        assert_eq!(token.parent_connection_id, "child-connection");
        assert_eq!(
            token.role,
            crate::acp::delegation::transport::CompanionRole::DelegationChild
        );
        assert!(!token.completion_v2);
        assert_eq!(token.bound_task_id.as_deref(), Some("bound-task"));
    }

    // Disabled custom slugs never become companion arguments. Disabled
    // built-ins are sorted for deterministic subtraction from the closed enum.
    #[test]
    fn disabled_builtin_target_args_excludes_custom_agents() {
        let disabled = vec![
            "grok".to_string(),
            "codex".to_string(),
            "custom:delegate-off".to_string(),
        ];
        let disabled_builtins = disabled_builtin_target_args(&disabled);
        assert_eq!(
            disabled_builtins,
            vec!["codex".to_string(), "grok".to_string()],
            "builtins only, sorted for a deterministic arg string"
        );

        assert!(disabled_builtin_target_args(&[]).is_empty());
    }

    // ─── companion_features_arg: inject/skip decision + --features value ──
    //
    // The companion now carries two independently-toggled tool groups. It is
    // injected when EITHER is on, and the `--features` arg names exactly the
    // enabled groups so the companion hides the rest. Crucially, feedback alone
    // must still inject the companion (the historical delegation-only gate would
    // have skipped it).

    /// Post-ready unavailability helper carries the stable audit code without
    /// inventing a second metrics Arc or mutating route plan fields.
    #[tokio::test]
    async fn post_ready_unavailable_audit_stable_code_no_route_mutation() {
        use crate::acp::delegation::metrics::{DelegationAuditRecord, DELEGATION_UNAVAILABLE_CODE};
        use crate::acp::delegation::route::{
            DelegationRoutePlan, DelegationRoutePolicy, DelegationRouteSource,
            NativeSuppressionPlan, ROUTE_ADAPTER_CONTRACT_VERSION,
        };
        use crate::acp::session_state::SessionState;
        use crate::web::event_bridge::EventEmitter;

        let plan = DelegationRoutePlan {
            managed: true,
            requested: DelegationRoutePolicy::Codeg,
            effective: DelegationRoutePolicy::Codeg,
            source: DelegationRouteSource::GlobalDefault,
            native_suppression: NativeSuppressionPlan::CodexMultiAgentFalse,
            expose_codeg_delegation: true,
            degraded_reason: None,
            adapter_contract_version: ROUTE_ADAPTER_CONTRACT_VERSION.to_string(),
            fingerprint: "test-ready-avail".into(),
        };
        let state = Arc::new(tokio::sync::RwLock::new(SessionState::new(
            "conn-avail-test".into(),
            AgentType::Codex,
            None,
            "win".into(),
            None,
        )));
        {
            let mut s = state.write().await;
            s.set_route_plan_snapshot(&plan);
            s.set_delegation_available(true);
            s.conversation_id = Some(7);
        }
        let route_before = state.read().await.delegation_route.clone();
        assert!(route_before.delegation_available);
        assert_eq!(route_before.effective, DelegationRoutePolicy::Codeg);
        assert!(route_before.degraded_reason.is_none());

        // Reuse audit constructor (same as finish_route_ready monitor path).
        let audit =
            DelegationAuditRecord::availability("conn-avail-test", Some(7), AgentType::Codex);
        assert_eq!(audit.stable_code(), Some(DELEGATION_UNAVAILABLE_CODE));
        emit_post_ready_unavailable(
            &state,
            &EventEmitter::Noop,
            "conn-avail-test",
            Some(7),
            AgentType::Codex,
        )
        .await;

        let after = state.read().await.delegation_route.clone();
        assert!(!after.delegation_available, "availability must flip false");
        assert_eq!(
            after.effective, route_before.effective,
            "immutable route fields must not change"
        );
        assert_eq!(after.requested, route_before.requested);
        assert_eq!(after.source, route_before.source);
        assert_eq!(after.managed, route_before.managed);
        assert_eq!(after.degraded_reason, route_before.degraded_reason);
    }

    #[test]
    fn companion_features_follow_plan_not_live_broker_route_setting() {
        // Plan-driven feature helper: expose_codeg_delegation maps to
        // delegation + coordination_v1 — never a live Broker re-read.
        assert_eq!(
            companion_features_arg(true, true, true, false, false, false),
            Some("delegation,coordination_v1,feedback".into())
        );
        assert_eq!(
            companion_features_arg(false, false, true, false, false, false),
            Some("feedback".into())
        );
        assert_eq!(
            companion_features_arg(false, false, false, false, false, false),
            None
        );
    }

    #[test]
    fn companion_features_arg_inject_skip_decision() {
        // All off → no companion at all.
        assert_eq!(
            companion_features_arg(false, false, false, false, false, false),
            None
        );
        // Delegation only without coordination → no Join token.
        assert_eq!(
            companion_features_arg(true, false, false, false, false, false),
            Some("delegation".to_string())
        );
        // Delegation + coordination_v1 (production Codeg-delegation plan).
        assert_eq!(
            companion_features_arg(true, true, false, false, false, false),
            Some("delegation,coordination_v1".to_string())
        );
        // Root + delegation enables workflow_v2 injection.
        assert_eq!(
            companion_features_arg(true, true, false, false, false, true),
            Some("delegation,coordination_v1,workflow_v2".to_string())
        );
        // Feedback only — the decoupling: companion injected for feedback even
        // when delegation is off.
        assert_eq!(
            companion_features_arg(false, false, true, false, false, false),
            Some("feedback".to_string())
        );
        // Ask only — likewise injects the companion on its own.
        assert_eq!(
            companion_features_arg(false, false, false, true, false, false),
            Some("ask".to_string())
        );
        // Sessions only — likewise injects the companion on its own.
        assert_eq!(
            companion_features_arg(false, false, false, false, true, false),
            Some("sessions".to_string())
        );
        // Workflow alone still injects (feature bit on without other groups).
        assert_eq!(
            companion_features_arg(false, false, false, false, false, true),
            Some("workflow_v2".to_string())
        );
        // All on → comma-joined, in declaration order.
        assert_eq!(
            companion_features_arg(true, true, true, true, true, true),
            Some("delegation,coordination_v1,feedback,ask,sessions,workflow_v2".to_string())
        );
    }

    #[test]
    fn companion_features_arg_uses_native_ask_for_grok_only() {
        assert_eq!(
            companion_features_arg_for_agent(AgentType::Grok, true, true, true, true, true, true,),
            Some("delegation,coordination_v1,feedback,sessions,workflow_v2".to_string())
        );
        assert_eq!(
            companion_features_arg_for_agent(AgentType::Codex, true, true, true, true, true, true,),
            Some("delegation,coordination_v1,feedback,ask,sessions,workflow_v2".to_string())
        );
    }

    #[test]
    fn workflow_v2_launch_feature_uses_only_the_canonical_token() {
        let root = companion_features_arg(true, true, false, false, false, true)
            .expect("root workflow launch features");
        assert!(root.split(',').any(|token| token == "workflow_v2"));
        assert!(!root.split(',').any(|token| token == "workflow_v1"));
    }

    // ── ResumeExistingOnly connection contract harness ──────────────────────
    //
    // Exercises production wire helpers (`send_resume_session` /
    // `send_load_session_capturing_id`) + `gate_session_started_for_attach`
    // under ResumeExistingOnly with a mock agent that answers resume/load/new
    // and counts each method. Refuse paths call production
    // `refuse_unresumable_bootstrap` (real SessionLoadFailed events / settle).
    // `reused_session` is broker-level after continue admission — proven by
    // `resume_existing_accepts_standard_omit_id_continue_sets_reused_session`
    // in broker tests.

    #[derive(Clone)]
    enum ResumeContractRpcOutcome {
        Ok(serde_json::Value),
        Err { code: i32, message: String },
    }

    /// Shared method counters for ResumeExistingOnly contract harness.
    struct ResumeContractCounters {
        resume_count: Arc<std::sync::atomic::AtomicUsize>,
        load_count: Arc<std::sync::atomic::AtomicUsize>,
        session_new_count: Arc<std::sync::atomic::AtomicUsize>,
        prompt_count: Arc<std::sync::atomic::AtomicUsize>,
        prompt_seen: Arc<std::sync::atomic::AtomicBool>,
        prompt_notify: Arc<tokio::sync::Notify>,
    }

    /// Mock agent for ResumeExistingOnly contract tests: initialize +
    /// session/resume|load|new|prompt with method counters.
    struct ResumeContractMockAgent {
        resume_count: Arc<std::sync::atomic::AtomicUsize>,
        load_count: Arc<std::sync::atomic::AtomicUsize>,
        session_new_count: Arc<std::sync::atomic::AtomicUsize>,
        prompt_count: Arc<std::sync::atomic::AtomicUsize>,
        prompt_seen: Arc<std::sync::atomic::AtomicBool>,
        prompt_notify: Arc<tokio::sync::Notify>,
        advertise_resume: bool,
        advertise_load: bool,
        resume_outcome: ResumeContractRpcOutcome,
        load_outcome: ResumeContractRpcOutcome,
    }

    impl ResumeContractMockAgent {
        fn with_counters(
            advertise_resume: bool,
            advertise_load: bool,
            resume_outcome: ResumeContractRpcOutcome,
            load_outcome: ResumeContractRpcOutcome,
        ) -> (Self, ResumeContractCounters) {
            let resume_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let load_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let session_new_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let prompt_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let prompt_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let prompt_notify = Arc::new(tokio::sync::Notify::new());
            let agent = Self {
                resume_count: resume_count.clone(),
                load_count: load_count.clone(),
                session_new_count: session_new_count.clone(),
                prompt_count: prompt_count.clone(),
                prompt_seen: prompt_seen.clone(),
                prompt_notify: prompt_notify.clone(),
                advertise_resume,
                advertise_load,
                resume_outcome,
                load_outcome,
            };
            (
                agent,
                ResumeContractCounters {
                    resume_count,
                    load_count,
                    session_new_count,
                    prompt_count,
                    prompt_seen,
                    prompt_notify,
                },
            )
        }
    }

    impl sacp::ConnectTo<Client> for ResumeContractMockAgent {
        async fn connect_to(self, client: impl sacp::ConnectTo<Agent>) -> Result<(), sacp::Error> {
            use sacp::schema::{
                AgentCapabilities, InitializeRequest, InitializeResponse, PromptRequest,
                PromptResponse, SessionCapabilities, SessionResumeCapabilities,
            };
            use std::sync::atomic::Ordering;

            let advertise_resume = self.advertise_resume;
            let advertise_load = self.advertise_load;
            let resume_outcome = self.resume_outcome.clone();
            let load_outcome = self.load_outcome.clone();
            let resume_count = self.resume_count;
            let load_count = self.load_count;
            let session_new_count = self.session_new_count;
            let prompt_count = self.prompt_count;
            let prompt_seen = self.prompt_seen;
            let prompt_notify = self.prompt_notify;

            Agent
                .builder()
                .on_receive_request(
                    async move |req: InitializeRequest, responder, _connection| {
                        let mut caps = AgentCapabilities::new().load_session(advertise_load);
                        if advertise_resume {
                            caps = caps.session_capabilities(
                                SessionCapabilities::new().resume(SessionResumeCapabilities::new()),
                            );
                        }
                        responder.respond(
                            InitializeResponse::new(req.protocol_version).agent_capabilities(caps),
                        )
                    },
                    sacp::on_receive_request!(),
                )
                .on_receive_request(
                    async move |_req: PromptRequest, responder, _connection| {
                        prompt_count.fetch_add(1, Ordering::SeqCst);
                        prompt_seen.store(true, Ordering::SeqCst);
                        prompt_notify.notify_waiters();
                        responder.respond(PromptResponse::new(StopReason::EndTurn))
                    },
                    sacp::on_receive_request!(),
                )
                .on_receive_request(
                    async move |req: UntypedMessage, responder, _connection| match req.method() {
                        "session/resume" => {
                            resume_count.fetch_add(1, Ordering::SeqCst);
                            match &resume_outcome {
                                ResumeContractRpcOutcome::Ok(body) => {
                                    responder.respond(body.clone())
                                }
                                ResumeContractRpcOutcome::Err { code, message } => responder
                                    .respond_with_error(sacp::Error::new(*code, message.clone())),
                            }
                        }
                        "session/load" => {
                            load_count.fetch_add(1, Ordering::SeqCst);
                            match &load_outcome {
                                ResumeContractRpcOutcome::Ok(body) => {
                                    responder.respond(body.clone())
                                }
                                ResumeContractRpcOutcome::Err { code, message } => responder
                                    .respond_with_error(sacp::Error::new(*code, message.clone())),
                            }
                        }
                        "session/new" => {
                            session_new_count.fetch_add(1, Ordering::SeqCst);
                            responder.respond(serde_json::json!({
                                "sessionId": "should-not-be-called"
                            }))
                        }
                        other => responder.respond_with_error(sacp::util::internal_error(format!(
                            "unexpected method in resume contract mock: {other}"
                        ))),
                    },
                    sacp::on_receive_request!(),
                )
                .connect_to(client)
                .await
        }
    }

    #[derive(Debug, Clone)]
    struct ResumeContractObservation {
        emit_session_id: Option<String>,
        refused_reason: Option<String>,
        prompt_admitted: bool,
        production_refuse_called: bool,
        session_load_failed_code: Option<String>,
        settled_error_code: Option<String>,
        resume_count: usize,
        load_count: usize,
        session_new_count: usize,
        prompt_count: usize,
    }

    /// Optional broker handoff so dual-error / mismatch refusals exercise
    /// production `refuse_unresumable_bootstrap` → durable unresumable settle.
    struct ResumeContractSettleFixture {
        broker: Arc<crate::acp::delegation::broker::DelegationBroker>,
        runs: Arc<crate::acp::delegation::run_store::RunStore>,
        task_id: String,
        connection_id: String,
    }

    /// Standard ACP resume/load body with modes/config and **no** sessionId
    /// (Codex ACP shape).
    fn codex_shaped_no_session_id_body() -> serde_json::Value {
        serde_json::json!({
            "modes": {
                "currentModeId": "default",
                "availableModes": [
                    {"id": "default", "name": "Default"},
                    {"id": "full-access", "name": "Full Access"}
                ]
            },
            "configOptions": [
                {
                    "id": "model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "gpt-5.1-codex",
                    "options": [
                        {"value": "gpt-5.1-codex", "name": "GPT-5.1 Codex"}
                    ]
                }
            ]
        })
    }

    fn empty_no_session_id_body() -> serde_json::Value {
        serde_json::json!({})
    }

    fn body_with_session_id(session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "sessionId": session_id,
            "modes": {
                "currentModeId": "default",
                "availableModes": [{"id": "default", "name": "Default"}]
            }
        })
    }

    async fn wait_for_mock_prompt(
        prompt_seen: &std::sync::atomic::AtomicBool,
        prompt_notify: &tokio::sync::Notify,
    ) {
        use std::sync::atomic::Ordering;
        if prompt_seen.load(Ordering::SeqCst) {
            return;
        }
        let notified = prompt_notify.notified();
        tokio::pin!(notified);
        if prompt_seen.load(Ordering::SeqCst) {
            return;
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), notified)
            .await
            .expect("mock must receive session/prompt");
        assert!(
            prompt_seen.load(Ordering::SeqCst),
            "prompt_seen must be set after notify"
        );
    }

    async fn admit_prompt_after_emit(
        cx: &ConnectionTo<Agent>,
        session_id: String,
        obs: &mut ResumeContractObservation,
        prompt_seen: &std::sync::atomic::AtomicBool,
        prompt_notify: &tokio::sync::Notify,
    ) -> Result<(), sacp::Error> {
        obs.emit_session_id = Some(session_id.clone());
        let new_resp = NewSessionResponse::new(SessionId::new(session_id));
        let mut session = cx.attach_session(new_resp, Default::default())?;
        session.send_prompt("continue after resume/load")?;
        wait_for_mock_prompt(prompt_seen, prompt_notify).await;
        obs.prompt_admitted = true;
        Ok(())
    }

    /// Production refuse path used by ResumeExistingOnly gate/load failure.
    async fn apply_production_refuse(
        state: &Arc<RwLock<SessionState>>,
        requested_session_id: &str,
        message: String,
        broker: Option<&crate::acp::delegation::broker::DelegationBroker>,
        connection_id: &str,
        event_rx: &mut tokio::sync::broadcast::Receiver<
            std::sync::Arc<crate::acp::types::EventEnvelope>,
        >,
        obs: &mut ResumeContractObservation,
    ) {
        refuse_unresumable_bootstrap(
            state,
            &EventEmitter::Noop,
            requested_session_id,
            message.clone(),
            broker,
            connection_id,
        )
        .await;
        obs.production_refuse_called = true;
        obs.refused_reason = Some(message);
        while let Ok(env) = event_rx.try_recv() {
            if let AcpEvent::SessionLoadFailed { code, .. } = &env.payload {
                obs.session_load_failed_code = Some(code.clone());
            }
        }
    }

    async fn setup_resume_contract_settle_fixture(label: &str) -> ResumeContractSettleFixture {
        use crate::acp::delegation::broker::{
            AdmissionHandoff, ConversationDepthLookup, DelegationBroker, DelegationConfig,
        };
        use crate::acp::delegation::run_store::{ReservingRunInsert, RunStore};
        use crate::acp::delegation::spawner::{mock::MockSpawner, ConnectionSpawner};
        use crate::acp::delegation::store::{DbDelegationTaskStore, DelegationTaskStore};
        use crate::acp::delegation::types::DelegationError;
        use crate::db::entities::delegation_task_run::AdmissionClass;
        use crate::db::service::conversation_service;
        use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
        use chrono::Utc;

        struct EmptyLookup;
        #[async_trait::async_trait]
        impl ConversationDepthLookup for EmptyLookup {
            async fn parent_of(&self, _id: i32) -> Result<Option<i32>, DelegationError> {
                Ok(None)
            }
        }

        let db = Arc::new(fresh_in_memory_db().await);
        let folder = seed_folder(&db, &format!("/tmp/codeg-resume-contract-{label}")).await;
        let parent = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some(format!("parent-{label}")),
            None,
        )
        .await
        .expect("parent");
        let child = conversation_service::create(
            &db.conn,
            folder,
            AgentType::ClaudeCode,
            Some(format!("child-{label}")),
            None,
        )
        .await
        .expect("child");
        let runs = Arc::new(RunStore::new(db.clone()));
        let task_id = format!("task-resume-contract-{label}");
        runs.insert_reserving(ReservingRunInsert {
            task_id: task_id.clone(),
            root_task_id: task_id.clone(),
            previous_task_id: Some("task-gen1".into()),
            generation: 2,
            parent_conversation_id: parent.id,
            parent_tool_use_id: Some(format!("pt-{label}")),
            child_conversation_id: child.id,
            agent_type: AgentType::ClaudeCode.to_string(),
            profile_id: None,
            workspace_path: Some(format!("/tmp/codeg-resume-contract-{label}")),
            route_fingerprint: None,
            launch_snapshot_version: None,
            mode_id: None,
            config_values_json: None,
            task_preview: Some("continue".into()),
            request_fingerprint: Some(format!("fp-{label}")),
            admission_class: AdmissionClass::NormalRevision,
            lineage_root_task_id: task_id.clone(),
            work_unit_key: Some(format!("unit-{label}")),
            history_only: false,
            replaced_task_id: None,
            replacement_reason: None,
            started_at: Some(Utc::now()),
        })
        .await
        .expect("insert reserving");

        let mock = Arc::new(MockSpawner::new());
        let task_store = Arc::new(DbDelegationTaskStore::from_run_store(runs.clone()))
            as Arc<dyn DelegationTaskStore>;
        let broker = Arc::new(
            DelegationBroker::new(
                mock as Arc<dyn ConnectionSpawner>,
                Arc::new(EmptyLookup) as Arc<dyn ConversationDepthLookup>,
            )
            .with_task_store(task_store)
            .with_run_store(runs.clone()),
        );
        broker
            .set_config(DelegationConfig {
                enabled: true,
                ..DelegationConfig::default()
            })
            .await;

        let reg = broker
            .begin_run_admission(AdmissionHandoff {
                task_id: task_id.clone(),
                generation: 2,
                child_conversation_id: child.id,
                parent_connection_id: format!("parent-conn-{label}"),
                parent_conversation_id: parent.id,
                parent_tool_use_id: format!("pt-{label}"),
                task_preview: "continue".into(),
                child_connection_id: None,
            })
            .await
            .expect("begin_run_admission");

        ResumeContractSettleFixture {
            broker,
            runs,
            task_id,
            connection_id: reg.child_connection_id,
        }
    }

    /// Client-side ResumeExistingOnly chain using production wire helpers + gate
    /// + production `refuse_unresumable_bootstrap` on refuse.
    async fn run_resume_existing_contract(
        mock: ResumeContractMockAgent,
        requested_session_id: &str,
        counters: ResumeContractCounters,
        settle: Option<ResumeContractSettleFixture>,
    ) -> ResumeContractObservation {
        use std::sync::atomic::Ordering;

        let ResumeContractCounters {
            resume_count,
            load_count,
            session_new_count,
            prompt_count,
            prompt_seen,
            prompt_notify,
        } = counters;
        let requested = requested_session_id.to_string();
        let outcome: Arc<std::sync::Mutex<Option<ResumeContractObservation>>> =
            Arc::new(std::sync::Mutex::new(None));
        let outcome_slot = outcome.clone();
        let connection_id = settle
            .as_ref()
            .map(|s| s.connection_id.clone())
            .unwrap_or_else(|| "conn-resume-contract".into());
        let broker_for_refuse = settle.as_ref().map(|s| s.broker.clone());
        let state = Arc::new(RwLock::new(SessionState::new(
            connection_id.clone(),
            AgentType::Codex,
            Some(PathBuf::from(".")),
            "main".into(),
            None,
        )));

        Client
            .builder()
            .connect_with(mock, async move |cx| {
                let shell = test_placeholder_terminal_shell();
                let init_req = build_initialize_request(
                    AgentType::Codex,
                    &shell.spec,
                    adapter_for(AgentType::Codex),
                )
                .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                let init_resp = cx
                    .send_request_to(Agent, init_req)
                    .block_task()
                    .await?;
                let supports_resume = init_resp
                    .agent_capabilities
                    .session_capabilities
                    .resume
                    .is_some();

                let route_plan = native_plan(AgentType::Codex);
                let cwd = PathBuf::from(".");
                let mut obs = ResumeContractObservation {
                    emit_session_id: None,
                    refused_reason: None,
                    prompt_admitted: false,
                    production_refuse_called: false,
                    session_load_failed_code: None,
                    settled_error_code: None,
                    resume_count: 0,
                    load_count: 0,
                    session_new_count: 0,
                    prompt_count: 0,
                };
                let mut event_rx = state.read().await.event_stream().subscribe();

                if supports_resume {
                    let resume_req = build_resume_session_request(
                        AgentType::Codex,
                        SessionId::new(requested.clone()),
                        &cwd,
                        Vec::new(),
                        &shell.spec,
                        adapter_for(AgentType::Codex),
                        &route_plan,
                        ConnectionPurpose::User,
                    )
                    .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                    match send_resume_session(&cx, resume_req).await {
                        Ok((_resp, _models, returned_id)) => {
                            match crate::acp::session_attach::gate_session_started_for_attach(
                                crate::acp::session_attach::SessionAttachMode::ResumeExistingOnly,
                                &requested,
                                returned_id.as_deref(),
                            ) {
                                crate::acp::session_attach::SessionStartedDecision::Emit {
                                    session_id,
                                } => {
                                    admit_prompt_after_emit(
                                        &cx,
                                        session_id,
                                        &mut obs,
                                        &prompt_seen,
                                        &prompt_notify,
                                    )
                                    .await?;
                                    *outcome_slot.lock().unwrap() = Some(obs);
                                    return Ok(());
                                }
                                crate::acp::session_attach::SessionStartedDecision::RefuseUnresumable {
                                    reason,
                                } => {
                                    apply_production_refuse(
                                        &state,
                                        &requested,
                                        format!("resume_existing_only: {reason}"),
                                        broker_for_refuse.as_deref(),
                                        &connection_id,
                                        &mut event_rx,
                                        &mut obs,
                                    )
                                    .await;
                                    *outcome_slot.lock().unwrap() = Some(obs);
                                    return Ok(());
                                }
                            }
                        }
                        Err(_e) => {
                            // Production: every resume error falls through to load.
                        }
                    }
                }

                // session/load (resume → load only; never session/new).
                let load_req = build_load_session_request(
                    AgentType::Codex,
                    SessionId::new(requested.clone()),
                    &cwd,
                    Vec::new(),
                    &shell.spec,
                    adapter_for(AgentType::Codex),
                    &route_plan,
                    ConnectionPurpose::User,
                )
                .map_err(|e| sacp::util::internal_error(e.to_string()))?;
                match send_load_session_capturing_id(&cx, load_req).await {
                    Ok((_resp, returned_id)) => {
                        match crate::acp::session_attach::gate_session_started_for_attach(
                            crate::acp::session_attach::SessionAttachMode::ResumeExistingOnly,
                            &requested,
                            returned_id.as_deref(),
                        ) {
                            crate::acp::session_attach::SessionStartedDecision::Emit {
                                session_id,
                            } => {
                                admit_prompt_after_emit(
                                    &cx,
                                    session_id,
                                    &mut obs,
                                    &prompt_seen,
                                    &prompt_notify,
                                )
                                .await?;
                            }
                            crate::acp::session_attach::SessionStartedDecision::RefuseUnresumable {
                                reason,
                            } => {
                                apply_production_refuse(
                                    &state,
                                    &requested,
                                    format!("resume_existing_only: {reason}"),
                                    broker_for_refuse.as_deref(),
                                    &connection_id,
                                    &mut event_rx,
                                    &mut obs,
                                )
                                .await;
                            }
                        }
                    }
                    Err(e) => {
                        // Same decision helper as production load-error path —
                        // do not reimplement order here. If the helper ever
                        // classifies ResourceNotFound under ResumeExistingOnly
                        // instead of refusing, dual-error asserts fail.
                        let err_str = e.to_string();
                        match session_load_error_action(
                            crate::acp::session_attach::SessionAttachMode::ResumeExistingOnly,
                            e.code,
                            &err_str,
                        ) {
                            SessionLoadErrorAction::RefuseUnresumableBootstrap => {
                                apply_production_refuse(
                                    &state,
                                    &requested,
                                    format!(
                                        "resume_existing_only: session/load failed: {err_str}"
                                    ),
                                    broker_for_refuse.as_deref(),
                                    &connection_id,
                                    &mut event_rx,
                                    &mut obs,
                                )
                                .await;
                            }
                            SessionLoadErrorAction::SurfaceClassifiedLoadFailed {
                                code,
                            } => {
                                // Production would only take this under Default
                                // attach. Record without refuse so dual-error
                                // cannot green on a diverged decision.
                                obs.session_load_failed_code = Some(code.to_string());
                            }
                            SessionLoadErrorAction::ContinueDefaultFallthrough => {}
                        }
                    }
                }

                *outcome_slot.lock().unwrap() = Some(obs);
                Ok(())
            })
            .await
            .expect("resume contract connect_with");

        let mut obs = outcome
            .lock()
            .unwrap()
            .take()
            .expect("resume contract observation");
        obs.resume_count = resume_count.load(Ordering::SeqCst);
        obs.load_count = load_count.load(Ordering::SeqCst);
        obs.session_new_count = session_new_count.load(Ordering::SeqCst);
        obs.prompt_count = prompt_count.load(Ordering::SeqCst);
        if let Some(fixture) = settle {
            if let Ok(Some(run)) = fixture.runs.load_by_task_id(&fixture.task_id).await {
                obs.settled_error_code = run.error_code;
            }
        }
        obs
    }

    #[tokio::test]
    async fn resume_existing_accepts_standard_no_id_resume_admits_prompt() {
        let body = empty_no_session_id_body();
        let (mock, counters) = ResumeContractMockAgent::with_counters(
            true,
            true,
            ResumeContractRpcOutcome::Ok(body),
            ResumeContractRpcOutcome::Err {
                code: -32601,
                message: "load should not run after resume success".into(),
            },
        );
        let obs = run_resume_existing_contract(mock, "sess-requested", counters, None).await;

        assert_eq!(obs.emit_session_id.as_deref(), Some("sess-requested"));
        assert!(obs.refused_reason.is_none(), "unexpected refuse: {obs:?}");
        assert!(obs.prompt_admitted);
        assert_eq!(obs.prompt_count, 1);
        assert_eq!(obs.resume_count, 1);
        assert_eq!(
            obs.load_count, 0,
            "successful resume must not fall into load"
        );
        assert_eq!(
            obs.session_new_count, 0,
            "ResumeExistingOnly never session/new"
        );
        // reused_session is broker-level after continue admission; see
        // resume_existing_accepts_standard_omit_id_continue_sets_reused_session.
    }

    #[tokio::test]
    async fn resume_existing_accepts_standard_no_id_load_admits_prompt() {
        let body = empty_no_session_id_body();
        let (mock, counters) = ResumeContractMockAgent::with_counters(
            false, // no resume capability → load only
            true,
            ResumeContractRpcOutcome::Err {
                code: -32601,
                message: "resume not advertised".into(),
            },
            ResumeContractRpcOutcome::Ok(body),
        );
        let obs = run_resume_existing_contract(mock, "sess-load-only", counters, None).await;

        assert_eq!(obs.emit_session_id.as_deref(), Some("sess-load-only"));
        assert!(obs.refused_reason.is_none(), "unexpected refuse: {obs:?}");
        assert!(obs.prompt_admitted);
        assert_eq!(obs.prompt_count, 1);
        assert_eq!(obs.resume_count, 0);
        assert_eq!(obs.load_count, 1);
        assert_eq!(obs.session_new_count, 0);
    }

    #[tokio::test]
    async fn resume_existing_accepts_standard_resume_error_then_load_no_id() {
        let body = empty_no_session_id_body();
        let (mock, counters) = ResumeContractMockAgent::with_counters(
            true,
            true,
            ResumeContractRpcOutcome::Err {
                code: -32002,
                message: "resume unavailable".into(),
            },
            ResumeContractRpcOutcome::Ok(body),
        );
        let obs = run_resume_existing_contract(mock, "sess-fallback", counters, None).await;

        assert_eq!(obs.emit_session_id.as_deref(), Some("sess-fallback"));
        assert!(obs.refused_reason.is_none(), "unexpected refuse: {obs:?}");
        assert!(obs.prompt_admitted);
        assert_eq!(obs.prompt_count, 1, "exactly one prompt after resume→load");
        assert_eq!(obs.resume_count, 1);
        assert_eq!(obs.load_count, 1);
        assert_eq!(obs.session_new_count, 0);
    }

    #[tokio::test]
    async fn resume_existing_accepts_standard_mismatch_refuses_no_prompt() {
        let body = body_with_session_id("sess-other");
        let settle = setup_resume_contract_settle_fixture("mismatch").await;
        let (mock, counters) = ResumeContractMockAgent::with_counters(
            true,
            true,
            ResumeContractRpcOutcome::Ok(body),
            ResumeContractRpcOutcome::Err {
                code: -32601,
                message: "load should not run after resume mismatch refuse".into(),
            },
        );
        let obs = run_resume_existing_contract(mock, "sess-expected", counters, Some(settle)).await;

        assert!(
            obs.emit_session_id.is_none(),
            "must not emit SessionStarted"
        );
        assert!(!obs.prompt_admitted);
        assert_eq!(obs.prompt_count, 0);
        assert_eq!(obs.session_new_count, 0);
        assert!(
            obs.production_refuse_called,
            "must call production refuse_unresumable_bootstrap"
        );
        assert_eq!(
            obs.session_load_failed_code.as_deref(),
            Some("unresumable"),
            "production refuse must emit SessionLoadFailed(unresumable)"
        );
        assert_eq!(
            obs.settled_error_code.as_deref(),
            Some("unresumable"),
            "production refuse must durable-settle unresumable"
        );
        let reason = obs.refused_reason.expect("expected refuse");
        assert!(
            reason.contains("mismatch"),
            "refuse reason should mention mismatch: {reason}"
        );
        assert_eq!(obs.resume_count, 1);
        assert_eq!(obs.load_count, 0);
    }

    #[tokio::test]
    async fn resume_existing_accepts_standard_both_error_no_prompt_no_new() {
        // Use ResourceNotFound (-32002) deliberately: under Default attach this
        // classifies to SessionLoadFailed(resource_not_found) and returns
        // without refuse settle. Production ResumeExistingOnly must still
        // refuse_unresumable_bootstrap first (design §2 resume/load RPC failure).
        let settle = setup_resume_contract_settle_fixture("dual-error").await;
        let (mock, counters) = ResumeContractMockAgent::with_counters(
            true,
            true,
            ResumeContractRpcOutcome::Err {
                code: -32002,
                message: "resume failed".into(),
            },
            ResumeContractRpcOutcome::Err {
                code: -32002,
                message: "load failed".into(),
            },
        );
        // Sanity: this code *would* short-circuit on Default attach.
        assert_eq!(
            classify_session_load_failure(sacp::schema::ErrorCode::ResourceNotFound, "load failed",),
            Some("resource_not_found"),
        );
        let obs = run_resume_existing_contract(mock, "sess-dead", counters, Some(settle)).await;

        assert!(obs.emit_session_id.is_none());
        assert!(!obs.prompt_admitted);
        assert_eq!(obs.prompt_count, 0);
        assert_eq!(
            obs.session_new_count, 0,
            "never session/new under ResumeExistingOnly"
        );
        assert_eq!(obs.resume_count, 1);
        assert_eq!(obs.load_count, 1);
        assert!(
            obs.production_refuse_called,
            "dual-error ResourceNotFound must still call production refuse_unresumable_bootstrap"
        );
        assert_eq!(
            obs.session_load_failed_code.as_deref(),
            Some("unresumable"),
            "ResumeExistingOnly must emit SessionLoadFailed(unresumable), not resource_not_found"
        );
        assert_eq!(
            obs.settled_error_code.as_deref(),
            Some("unresumable"),
            "production refuse must durable-settle unresumable (not harness-only flag)"
        );
        let reason = obs.refused_reason.expect("unresumable settle path");
        assert!(
            reason.contains("resume_existing_only"),
            "expected unresumable path: {reason}"
        );
        assert!(
            reason.contains("session/load failed"),
            "expected load-failed refuse: {reason}"
        );
    }

    #[tokio::test]
    async fn resume_existing_accepts_standard_codex_shaped_body_no_session_id() {
        let body = codex_shaped_no_session_id_body();
        // Ensure body has no sessionId (Codex contract).
        assert!(body.get("sessionId").is_none());
        assert!(body.get("session_id").is_none());
        assert!(body.get("modes").is_some());
        assert!(body.get("configOptions").is_some());

        let (mock, counters) = ResumeContractMockAgent::with_counters(
            true,
            true,
            ResumeContractRpcOutcome::Ok(body),
            ResumeContractRpcOutcome::Err {
                code: -32601,
                message: "unused".into(),
            },
        );
        let obs = run_resume_existing_contract(mock, "codex-sess-1", counters, None).await;

        assert_eq!(obs.emit_session_id.as_deref(), Some("codex-sess-1"));
        assert!(obs.prompt_admitted);
        assert_eq!(obs.prompt_count, 1);
        assert_eq!(obs.session_new_count, 0);
        assert_eq!(obs.resume_count, 1);
    }

    #[test]
    fn resume_existing_accepts_standard_extract_camel_and_snake_case() {
        // Re-assert opportunistic extraction still works (kept from Task 1 matrix).
        use crate::acp::session_attach::extract_session_id_from_raw_response;
        assert_eq!(
            extract_session_id_from_raw_response(&serde_json::json!({
                "sessionId": "sid-camel",
                "modes": null
            }))
            .as_deref(),
            Some("sid-camel")
        );
        assert_eq!(
            extract_session_id_from_raw_response(&serde_json::json!({
                "session_id": "sid-snake"
            }))
            .as_deref(),
            Some("sid-snake")
        );
        assert_eq!(
            extract_session_id_from_raw_response(&codex_shaped_no_session_id_body()),
            None
        );
    }
}
