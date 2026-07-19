use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use sacp::schema::{
    BlobResourceContents, CancelNotification, ClientCapabilities, ContentBlock, ContentChunk,
    CreateTerminalRequest, CreateTerminalResponse, EmbeddedResource, EmbeddedResourceResource,
    FileSystemCapabilities, ImageContent, InitializeRequest, KillTerminalRequest,
    KillTerminalResponse, LoadSessionRequest, Meta, NewSessionRequest, NewSessionResponse,
    PermissionOptionKind, Plan, PlanEntryPriority, PlanEntryStatus, PromptRequest, ProtocolVersion,
    ReadTextFileRequest, ReadTextFileResponse, ReleaseTerminalRequest, ReleaseTerminalResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, ResourceLink,
    ResumeSessionRequest, ResumeSessionResponse, SelectedPermissionOutcome, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectGroup,
    SessionConfigSelectOption, SessionConfigSelectOptions, SessionId, SessionModeState,
    SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, SetSessionModeRequest, StopReason, TerminalExitStatus,
    TerminalOutputRequest, TerminalOutputResponse, TextContent, TextResourceContents,
    ToolCallContent, WaitForTerminalExitRequest, WaitForTerminalExitResponse, WriteTextFileRequest,
    WriteTextFileResponse,
};
use sacp::schema::{HttpHeader, McpServer, McpServerHttp, McpServerSse, McpServerStdio};
use sacp::util::MatchDispatch;
use sacp::{
    on_receive_request, Agent, Client, ConnectionTo, Dispatch, Responder, SessionMessage,
    UntypedMessage,
};
use sacp_tokio::AcpAgent;
use tokio::sync::{mpsc, oneshot, RwLock};

use crate::acp::background_watch;
use crate::acp::error::AcpError;
use crate::acp::file_system_runtime::{
    mode_allows_outside_workspace, FileSystemRuntime, FileSystemRuntimeError,
};
use crate::acp::registry::{self, AgentDistribution};
use crate::acp::session_state::SessionState;
use crate::acp::terminal_adapter::{adapter_for, AcpTerminalAdapter};
use crate::acp::terminal_assoc::{TerminalAssocFallback, ToolCallAssocHint};
use crate::acp::terminal_context::{terminal_metadata, TerminalPromptContext};
use crate::acp::terminal_runtime::{TerminalRuntime, TerminalRuntimeError};
use crate::acp::types::{
    AcpEvent, AvailableCommandInfo, ConfigStaleKind, ConnectionInfo, ConnectionStatus,
    PermissionOptionInfo, PlanEntryInfo, PromptCapabilitiesInfo, PromptInputBlock,
    SessionConfigKindInfo, SessionConfigOptionInfo, SessionConfigSelectGroupInfo,
    SessionConfigSelectInfo, SessionConfigSelectOptionInfo, SessionModeInfo, SessionModeStateInfo,
    ToolCallImageInfo, UserMessageBlock,
};
use crate::auto_title::ConnectionLaunchContext;
use crate::models::agent::AgentType;
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
    Cancel,
    Disconnect,
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
}

impl Drop for ConnectionCleanupGuard {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.connections.try_lock() {
            guard.remove(&self.connection_id);
            return;
        }
        let connections = self.connections.clone();
        let connection_id = std::mem::take(&mut self.connection_id);
        tokio::spawn(async move {
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
    /// Bounded FIFO for prompts, settings, permissions, and forks.
    pub cmd_tx: mpsc::Sender<ConnectionCommand>,
    /// Bounded FIFO for suspension, user cancellation, and disconnect.
    pub control_tx: mpsc::Sender<ConnectionControl>,
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

/// Codex Codeg only: set/override `CODEX_ACP_MULTI_AGENT=0`.
/// Native, unmanaged, and non-Codex plans leave the key byte-for-byte untouched
/// (including user values `0`/`1` and absence).
fn apply_route_environment(
    agent_type: AgentType,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
    env: &mut BTreeMap<String, String>,
) -> Result<(), AcpError> {
    use crate::acp::delegation::route::NativeSuppressionPlan;

    if agent_type == AgentType::Codex
        && matches!(
            plan.native_suppression,
            NativeSuppressionPlan::CodexMultiAgentFalse
        )
    {
        env.insert("CODEX_ACP_MULTI_AGENT".into(), "0".into());
    }
    Ok(())
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
    grok_always_approve: bool,
) {
    if agent_type == AgentType::Grok {
        // Grok's native ask_user_question waits outside Codeg's QuestionRequest
        // flow. Remove it so structured questions use codeg-mcp instead.
        // Independent of route: resolves a different Codeg question-tool collision.
        for arg in [
            "--no-auto-update",
            "--disallowed-tools",
            "ask_user_question",
        ] {
            parts.push(arg.into());
        }
        if grok_always_approve {
            parts.push("--always-approve".into());
        }
    }
    for arg in args {
        parts.push((*arg).into());
    }
}

async fn build_agent(
    agent_type: AgentType,
    runtime_env: &BTreeMap<String, String>,
    cwd: &Path,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
) -> Result<AcpAgent, AcpError> {
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
            //  - `--disallowed-tools ask_user_question`: best-effort only —
            //    Grok treats this flag as **headless-only** (`-p`); on
            //    `agent stdio` ACP it is ignored. The effective disable is
            //    `_meta.askUserQuestion: false` on session new/load/resume
            //    (see `merge_grok_ask_user_question_meta`).
            //  - `--always-approve`: auto-approve tool executions, but ONLY when the
            //    user selected that permission mode in the Grok panel. "ask"/unset
            //    leaves it off so ACP permission requests still reach codeg's UI.
            // Route tokens (`--no-subagents`, CodeBuddy `--disallowedTools`) are
            // applied once by `apply_process_route` on this complete argv.
            let grok_always_approve =
                agent_type == AgentType::Grok && crate::commands::acp::grok_launch_always_approve();
            append_npx_launch_args(&mut argv, agent_type, args, grok_always_approve);
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
            // INVARIANT: the substring "is not installed" is matched
            // verbatim by the frontend catch block in
            // `src/contexts/acp-connections-context.tsx` to surface a
            // localized install prompt. Do not change the wording.
            let (binary_path, cached_version) =
                crate::acp::binary_cache::find_best_cached_binary_for_agent(agent_type, cmd)?
                    .ok_or_else(|| {
                        AcpError::SdkNotInstalled(format!(
                            "{} is not installed. Please install it in Agent Settings.",
                            meta.name
                        ))
                    })?;
            if cached_version == registry_version {
                tracing::info!("[ACP][{}] Using cached binary {cached_version}", meta.name);
            } else {
                tracing::info!(
                    "[ACP][{}] Using cached binary {cached_version} (registry recommends {registry_version})",
                    meta.name
                );
            }

            let binary_str = binary_path.to_string_lossy().to_string();
            let binary_size = std::fs::metadata(&binary_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let mut server = McpServerStdio::new(meta.name, &binary_str);
            let cmd_args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
            let cmd_args_for_log = cmd_args.clone();
            if !cmd_args.is_empty() {
                server = server.args(cmd_args);
            }
            let merged_env = merge_agent_env(env, runtime_env);
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
    emitter: EventEmitter,
    connections: Arc<tokio::sync::Mutex<HashMap<String, AgentConnection>>>,
    preferred_mode_id: Option<String>,
    preferred_config_values: BTreeMap<String, String>,
    delegation_injection: Option<DelegationInjection>,
    launch_context: ConnectionLaunchContext,
) -> Result<SpawnHandshake, AcpError> {
    // Create the authoritative session state up front. Subsequent emit_with_state
    // calls write through this state and increment its seq counter so the first
    // event the frontend sees has seq=1, not the placeholder 0 from Phase 0.
    let mut initial_state = SessionState::new(
        connection_id.clone(),
        agent_type,
        working_dir.clone().map(PathBuf::from),
        owner_window_label.clone(),
        None, // folder_id 由后续 prompt handler 在首次 send 时绑定 (Phase 2)
    );
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
    let agent = build_agent(agent_type, &runtime_env, &launch_cwd, &route_plan).await?;

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

    let (cmd_tx, cmd_rx) = mpsc::channel::<ConnectionCommand>(32);
    let (control_tx, control_rx) = mpsc::channel::<ConnectionControl>(32);
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
    connections.lock().await.insert(
        connection_id.clone(),
        AgentConnection {
            id: connection_id.clone(),
            agent_type,
            status: ConnectionStatus::Connecting,
            owner_window_label,
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
        },
    );

    let join_handle = tokio::spawn(async move {
        // RAII guard: runs on normal exit AND on panic unwinding, so a
        // panic inside `run_connection` can't leak a stale map entry.
        let _cleanup = ConnectionCleanupGuard {
            connections: cleanup_connections,
            connection_id: cleanup_connection_id,
        };

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
            emitter_clone.clone(),
            Arc::clone(&state_clone),
            terminal_base_env,
            terminal_shell,
            preferred_mode_id,
            preferred_config_values,
            delegation_injection,
            route_plan,
            route_bootstrap_tx,
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
        // `_cleanup` is dropped here — removes the connection entry from
        // the manager map. Same drop semantics apply on panic unwinding.
    });

    // Install abort handle so unexposed teardown can terminate before the
    // conversation loop (Disconnect would never drain there).
    {
        let mut map = connections.lock().await;
        if let Some(conn) = map.get_mut(&connection_id) {
            conn.task_abort = Some(join_handle.abort_handle());
        }
    }

    Ok(SpawnHandshake {
        session_started_rx,
        route_bootstrap_rx,
    })
}

/// Shared state for pending permission responders.
type PendingPermissions =
    Arc<tokio::sync::Mutex<HashMap<String, Responder<RequestPermissionResponse>>>>;

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
) -> Option<Vec<SessionConfigOptionInfo>> {
    let options = meta?
        .get("x.ai/sessionConfig")?
        .get("options")?
        .as_array()?;

    let mut model_opts: Vec<SessionConfigSelectOptionInfo> = Vec::new();
    let mut model_current: Option<String> = None;

    for opt in options {
        let Some(id) = opt.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        // Only the MODEL selector is surfaced in the composer. Grok exposes a
        // live `session/set_model`, but has NO live reasoning-effort setter, and
        // effort is a GLOBAL `~/.grok/config.toml` setting shared by every Grok
        // session — so it belongs in the Grok settings panel (which routes
        // through the manager's fingerprint staleness), not a per-session
        // composer control. Grok's effort choices arrive here as category
        // "mode"; we deliberately skip them.
        if opt.get("category").and_then(|v| v.as_str()) != Some("model") {
            continue;
        }
        let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or(id);
        if opt
            .get("selected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            model_current = Some(id.to_string());
        }
        model_opts.push(SessionConfigSelectOptionInfo {
            value: id.to_string(),
            name: label.to_string(),
            description: opt
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        });
    }

    if model_opts.is_empty() {
        return None;
    }
    let current = model_current.unwrap_or_else(|| model_opts[0].value.clone());
    Some(vec![SessionConfigOptionInfo {
        id: GROK_MODEL_OPTION_ID.to_string(),
        name: "Model".to_string(),
        description: None,
        category: Some("model".to_string()),
        kind: SessionConfigKindInfo::Select(SessionConfigSelectInfo {
            current_value: current,
            options: model_opts,
            groups: Vec::new(),
        }),
    }])
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

/// Switch Grok's active model via the standard ACP `session/set_model`. Sent as
/// an `UntypedMessage` for the same reason as `session/resume` /
/// `session/set_config_option`: sacp 11.0.0's typed request is gated behind the
/// `unstable_session_model` feature (not enabled), and the orphan rule blocks a
/// local `JsonRpcRequest` impl. Grok has no ACP setter for reasoning effort
/// (verified: `set_model` ignores it and `set_mode` is a no-op) — that is
/// handled out-of-band by writing `~/.grok/config.toml`.
async fn set_grok_model(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    model_id: String,
) -> Result<(), sacp::Error> {
    let params = serde_json::json!({
        "sessionId": session_id.0.to_string(),
        "modelId": model_id,
    });
    let untyped_req = UntypedMessage::new("session/set_model", params).map_err(|e| {
        sacp::util::internal_error(format!("Failed to build set_model request: {e}"))
    })?;
    cx.send_request_to(Agent, untyped_req).block_task().await?;
    Ok(())
}

/// On reconnect, re-apply the user's last-picked Grok model (saved per agent by
/// the frontend and shipped back as a preferred config value) via `set_model`,
/// reflecting it in `current_value`. Effort preferences are omitted: Grok has no
/// live effort setter, so effort follows `~/.grok/config.toml` at session birth.
async fn apply_grok_preferred_model(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    opts: &mut [SessionConfigOptionInfo],
    preferred_config_values: &BTreeMap<String, String>,
) {
    let Some(pref) = preferred_config_values.get(GROK_MODEL_OPTION_ID) else {
        return;
    };
    let Some(model_opt) = opts.iter_mut().find(|o| o.id == GROK_MODEL_OPTION_ID) else {
        return;
    };
    let SessionConfigKindInfo::Select(sel) = &mut model_opt.kind;
    // Skip if already current, or the saved model is no longer offered (stale).
    if &sel.current_value == pref || !sel.options.iter().any(|o| &o.value == pref) {
        return;
    }
    match set_grok_model(cx, session_id, pref.clone()).await {
        Ok(()) => sel.current_value = pref.clone(),
        Err(e) => {
            tracing::error!("[ACP] failed to apply preferred grok model '{pref}' on connect: {e}")
        }
    }
}

/// Route a composer config-option change for Grok. Only the model selector is
/// live: model → `session/set_model`. (Reasoning effort is not a composer
/// control — it's a global `~/.grok/config.toml` setting owned by the Grok
/// settings panel.) Re-emits the options with the new `current_value` so the
/// backend snapshot stays authoritative.
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
    if config_id != GROK_MODEL_OPTION_ID {
        return Ok(());
    }
    // Model switching is live over ACP.
    match set_grok_model(cx, session_id, value_id.clone()).await {
        Ok(()) => {
            let current = state.read().await.config_options.clone();
            if let Some(mut opts) = current {
                if let Some(o) = opts.iter_mut().find(|o| o.id == config_id) {
                    let SessionConfigKindInfo::Select(sel) = &mut o.kind;
                    sel.current_value = value_id;
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
    preferred_mode_id: Option<&str>,
    preferred_config_values: &BTreeMap<String, String>,
    initial_config_options: Vec<SessionConfigOption>,
    file_system_runtime: &FileSystemRuntime,
) {
    if agent_type == AgentType::Grok {
        // Grok has no session mode selector; full-access follows
        // `~/.grok/config.toml` `[ui].permission_mode = always-approve`.
        sync_file_system_outside_access(file_system_runtime, agent_type, None);
        if let Some(mut opts) = synthesize_grok_config_options(grok_meta) {
            let session_id = session.session_id().clone();
            apply_grok_preferred_model(cx, &session_id, &mut opts, preferred_config_values).await;
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
/// Grok's "always-approve" permission mode (from `~/.grok/config.toml`) is the
/// full-access equivalent for that agent. Other agents use session mode /
/// config-option ids recognized by [`mode_allows_outside_workspace`].
fn initial_allow_outside_workspace(
    agent_type: AgentType,
    preferred_mode_id: Option<&str>,
    preferred_config_values: &BTreeMap<String, String>,
) -> bool {
    if agent_type == AgentType::Grok && crate::commands::acp::grok_launch_always_approve() {
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
/// with the agent's current full-access mode (and Grok always-approve).
fn sync_file_system_outside_access(
    file_system_runtime: &FileSystemRuntime,
    agent_type: AgentType,
    mode_id: Option<&str>,
) {
    let allow =
        if agent_type == AgentType::Grok && crate::commands::acp::grok_launch_always_approve() {
            true
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

/// Stamp Grok pager `--no-ask-user` parity into session `_meta`.
///
/// Grok shell reads `_meta.askUserQuestion == false` (via
/// `parse_ask_user_question_from_meta`) and strips native
/// `GrokBuild:ask_user_question` from the toolset so structured questions route
/// through `codeg-mcp__ask_user_question`. CLI `--disallowed-tools` is
/// **headless-only** and is ignored on `agent stdio` ACP launches — this meta
/// stamp is the ACP-effective gate.
fn merge_grok_ask_user_question_meta(
    mut meta: serde_json::Map<String, serde_json::Value>,
    agent_type: AgentType,
) -> serde_json::Map<String, serde_json::Value> {
    if agent_type == AgentType::Grok {
        meta.insert(
            "askUserQuestion".to_string(),
            serde_json::Value::Bool(false),
        );
    }
    meta
}

/// Merge Claude raw-SDK meta, Grok ask-user gate, route suppression, terminal
/// snapshot, and adapter contributions. Consumes the immutable `route_plan`
/// only for `native_suppression` (Claude deny list).
fn session_request_meta(
    agent_type: AgentType,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
) -> Result<Meta, AcpError> {
    let existing = claude_raw_sdk_session_meta(agent_type).unwrap_or_default();
    let with_grok = merge_grok_ask_user_question_meta(existing, agent_type);
    let with_route = merge_claude_route_meta(with_grok, route_plan)?;
    terminal_metadata(with_route, spec, adapter)
}

/// Build the ACP `initialize` request, declaring client capabilities and the
/// connection's terminal shell dialect under `_meta["codeg.dev/terminal"]`.
fn build_initialize_request(
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
) -> Result<InitializeRequest, AcpError> {
    let meta = terminal_metadata(Meta::default(), spec, adapter)?;
    Ok(InitializeRequest::new(ProtocolVersion::LATEST)
        .client_capabilities(
            ClientCapabilities::new()
                .terminal(true)
                .fs(FileSystemCapabilities::new()
                    .read_text_file(true)
                    .write_text_file(true)),
        )
        .meta(meta))
}

fn build_new_session_request(
    agent_type: AgentType,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
) -> Result<NewSessionRequest, AcpError> {
    let meta = session_request_meta(agent_type, route_plan, spec, adapter)?;
    let mut req = NewSessionRequest::new(cwd.to_path_buf()).meta(meta);
    if !mcp_servers.is_empty() {
        req = req.mcp_servers(mcp_servers);
    }
    Ok(req)
}

fn build_load_session_request(
    agent_type: AgentType,
    session_id: SessionId,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
) -> Result<LoadSessionRequest, AcpError> {
    let meta = session_request_meta(agent_type, route_plan, spec, adapter)?;
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
fn build_resume_session_request(
    agent_type: AgentType,
    session_id: SessionId,
    cwd: &Path,
    mcp_servers: Vec<McpServer>,
    spec: &ResolvedShellSpec,
    adapter: &dyn AcpTerminalAdapter,
    route_plan: &crate::acp::delegation::route::DelegationRoutePlan,
) -> Result<ResumeSessionRequest, AcpError> {
    let meta = session_request_meta(agent_type, route_plan, spec, adapter)?;
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
async fn send_resume_session(
    cx: &ConnectionTo<Agent>,
    req: ResumeSessionRequest,
) -> Result<ResumeSessionResponse, sacp::Error> {
    let untyped_req = UntypedMessage::new("session/resume", req)
        .map_err(|e| sacp::util::internal_error(format!("Failed to build resume request: {e}")))?;

    let raw_response = cx.send_request_to(Agent, untyped_req).block_task().await?;
    serde_json::from_value(raw_response)
        .map_err(|e| sacp::util::internal_error(format!("Failed to parse resume response: {e}")))
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
fn load_mcp_servers_for_agent(agent_type: AgentType) -> Vec<McpServer> {
    // Hermes, Kimi Code, and Grok each read their own native MCP config at
    // launch — Hermes from `~/.hermes/config.yaml` (`mcp_servers`, registered as
    // `mcp-<name>` toolsets), Kimi Code from `~/.kimi-code/mcp.json`
    // (`mcpServers`), Grok from `~/.grok/config.toml` (`[mcp_servers.<name>]`).
    // codeg manages those files directly via the MCP settings UI, so forwarding
    // the same servers over the ACP wire here would double-register them — skip
    // it. (The built-in `codeg-mcp` companion is injected separately by
    // `inject_codeg_mcp`, so it still reaches them.)
    if matches!(
        agent_type,
        AgentType::Hermes | AgentType::KimiCode | AgentType::Grok
    ) {
        return Vec::new();
    }
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

    let mut out = Vec::with_capacity(entries.len());
    for (name, spec) in entries {
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
#[derive(Clone)]
pub struct DelegationInjection {
    pub broker: Arc<crate::acp::delegation::broker::DelegationBroker>,
    pub tokens: Arc<crate::acp::delegation::listener::TokenRegistry>,
    /// Authenticated ready-lease registry. Lease waiters are registered at
    /// MCP injection when the immutable plan exposes Codeg delegation.
    pub leases: Arc<crate::acp::delegation::lease::CompanionLeaseRegistry>,
    pub socket_path: PathBuf,
    /// Hot-swappable "is live-feedback enabled?" flag. Read at injection time
    /// so `codeg-mcp` is injected when feedback (or ask/sessions) is on even
    /// when the plan omits Codeg delegation. Shares the same `tokens` registry
    /// and UDS socket as the ready lease.
    pub feedback: crate::acp::feedback::FeedbackRuntimeConfig,
    /// Hot-swappable "is ask-user-question enabled?" flag. Read at injection
    /// time alongside feedback/sessions so `codeg-mcp` is injected when ANY
    /// of the three is on, and the companion's `--features` lists `ask` to expose
    /// the `ask_user_question` tool.
    pub ask: crate::acp::question::QuestionRuntimeConfig,
    /// Hot-swappable "is get-session-info enabled?" flag. Read at injection time
    /// alongside the other three so `codeg-mcp` is injected when ANY of the four
    /// is on, and the companion's `--features` lists `sessions` to expose the
    /// `get_session_info` tool. No teardown handle (the lookup is stateless).
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

/// Append the built-in `codeg-mcp` MCP entry if delegation is enabled
/// AND the companion binary is present on disk. Returns the per-launch token
/// that was registered, or `None` when injection was skipped (disabled by
/// config, or binary missing).
///
/// When the binary is missing we log a single-line warning and skip
/// injection rather than register the token + emit a phantom McpServerStdio
/// pointing at a non-existent path. Phantom injection would have made every
/// new ACP session ship a guaranteed-to-fail MCP server entry: stricter
/// agents (Claude Code) refuse the whole session; lax agents lose the
/// delegate tool silently. Skipping leaves the agent fully functional minus
/// `delegate_to_agent`, which is the right degradation when codeg-mcp didn't
/// make it into the install.
/// The `--features` value for a companion launch given the feature flags,
/// or `None` when none is enabled (the companion isn't injected at all).
/// Pulled out as a pure function so the inject/skip decision is unit-testable
/// without a real binary on disk or a live broker. `coordination_v1` is only
/// advertised when delegation is on (Join requires the delegation tools).
fn companion_features_arg(
    delegation_enabled: bool,
    coordination_v1: bool,
    feedback_enabled: bool,
    ask_enabled: bool,
    sessions_enabled: bool,
) -> Option<String> {
    if !delegation_enabled && !feedback_enabled && !ask_enabled && !sessions_enabled {
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
    Some(features.join(","))
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

async fn inject_codeg_mcp(
    servers: &mut Vec<McpServer>,
    injection: &DelegationInjection,
    parent_connection_id: &str,
    working_dir: &Path,
    plan: &crate::acp::delegation::route::DelegationRoutePlan,
) -> Option<CompanionInjection> {
    // Feature list follows the immutable launch plan for Codeg delegation —
    // never the live Broker settings toggle. Feedback/ask/sessions remain
    // independent launch-time snapshots of their runtime configs.
    let delegation_enabled = plan.expose_codeg_delegation;
    // Join capability is connection-bound and follows Codeg delegation exposure.
    let coordination_v1 = plan.expose_codeg_delegation;
    let role = if plan.source == crate::acp::delegation::route::DelegationRouteSource::ForcedChild {
        crate::acp::delegation::transport::CompanionRole::DelegationChild
    } else {
        crate::acp::delegation::transport::CompanionRole::Root
    };
    let feedback_enabled = injection.feedback.is_enabled().await;
    let ask_enabled = injection.ask.is_enabled().await;
    let sessions_enabled = injection.sessions.is_enabled().await;
    // `None` (no feature enabled) short-circuits the whole injection.
    let features_arg = companion_features_arg(
        delegation_enabled,
        coordination_v1,
        feedback_enabled,
        ask_enabled,
        sessions_enabled,
    )?;
    let Some(binary_path) = locate_codeg_mcp_binary() else {
        tracing::warn!(
            "[delegation][WARN] codeg-mcp companion binary not found (checked CODEG_MCP_BIN, \
             exe sibling, and PATH); skipping delegate_to_agent / check_user_feedback / \
             ask_user_question / get_session_info tool injection for connection \
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
                role,
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
    server = server.args(vec![
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
        // feedback / ask / sessions).
        "--features".to_string(),
        features_arg,
        "--role".to_string(),
        role_arg.to_string(),
    ]);
    servers.push(McpServer::Stdio(server));
    Some(CompanionInjection {
        token,
        feedback_available: feedback_enabled,
        delegation_lease,
    })
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
    emitter: EventEmitter,
    state: Arc<RwLock<SessionState>>,
    terminal_base_env: BTreeMap<String, String>,
    terminal_shell: crate::terminal::shell::ResolvedShellSnapshot,
    preferred_mode_id: Option<String>,
    preferred_config_values: BTreeMap<String, String>,
    delegation_injection: Option<DelegationInjection>,
    route_plan: crate::acp::delegation::route::DelegationRoutePlan,
    route_bootstrap_tx: tokio::sync::oneshot::Sender<RouteBootstrapOutcome>,
) -> Result<(), AcpError> {
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
    let file_system_runtime = Arc::new(
        FileSystemRuntime::new(cwd.clone()).with_allow_outside_workspace(
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
    // Internal title runs must not start background transcript watchers.
    let is_internal_title = {
        let s = state.read().await;
        s.purpose == crate::auto_title::ConnectionPurpose::InternalTitle
    };
    let _bg_watch = if is_internal_title {
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
                            _cx: ConnectionTo<Agent>| {
                    respond_terminal_request(responder, runtime.wait_for_terminal_exit(req).await)?;
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
        .connect_with(agent, {
            let route_bootstrap_tx = Arc::clone(&route_bootstrap_tx);
            async move |cx| -> Result<(), sacp::Error> {
            let state = state_outer;
            let agent_name_for_log = registry::get_agent_meta(agent_type).name;

            // Advertise filesystem + terminal capabilities for ACP tool execution,
            // and declare the connection's shell snapshot under `_meta`.
            let init_request = build_initialize_request(
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
                        inject_codeg_mcp(&mut mcp_servers, inj, &conn_id, &cwd, &route_plan)
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

            if let Some(sid) = session_id {
                // Prefer session/resume when the agent advertises the
                // capability: it restores session context WITHOUT replaying
                // history (which session/load does only for us to drain and
                // discard — the transcript the user sees comes from the disk
                // parser, not the ACP wire). On any non-terminal resume failure
                // we fall through to the session/load block below, so the
                // effective chain is resume → load → new.
                if supports_resume {
                    let resume_req = match build_resume_session_request(
                        agent_type,
                        SessionId::new(sid.clone()),
                        &cwd,
                        mcp_servers.clone(),
                        &terminal_shell.spec,
                        adapter_for(agent_type),
                        &route_plan,
                    ) {
                        Ok(r) => r,
                        Err(e) => {
                            return Err(
                                bridge_acp_err_for_bootstrap(e, &route_bootstrap_tx).await
                            );
                        }
                    };
                    match send_resume_session(&cx, resume_req).await {
                        Ok(resume_resp) => {
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
                            let mut session = cx.attach_session(new_resp, Default::default())?;

                            // No drain: session/resume does not replay history,
                            // so there is nothing to discard. Any buffered
                            // notification (e.g. an early AvailableCommandsUpdate)
                            // is consumed and forwarded by run_conversation_loop.

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
                            // which already owns all terminal decisions
                            // (SessionLoadFailed for not-found, silent stop for
                            // auth, fallback to session/new otherwise). No
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

                // Load existing session via session/load
                let load_req = match build_load_session_request(
                    agent_type,
                    SessionId::new(sid.clone()),
                    &cwd,
                    mcp_servers.clone(),
                    &terminal_shell.spec,
                    adapter_for(agent_type),
                    &route_plan,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        return Err(bridge_acp_err_for_bootstrap(e, &route_bootstrap_tx).await);
                    }
                };
                let load_result = cx.send_request_to(Agent, load_req).block_task().await;

                match load_result {
                    Ok(load_resp) => {
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
                        // but forward AvailableCommandsUpdate to the frontend
                        let mut drained = 0u32;
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
                                            )
                                            .await;
                                        }
                                        Ok(())
                                    })
                                    .await
                                    .otherwise(async |dispatch| {
                                        // Historical replay: never counts as
                                        // agent activity for the soft watchdog.
                                        let _ = maybe_emit_claude_sdk_ext_notification(
                                            &st, &h, dispatch,
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
                        // session/load failed. Classify it: an unrecoverable
                        // historical session — the agent has no record of it
                        // (ResourceNotFound, -32002) or the agent process/session
                        // died mid-load (Claude 0.58.1 reports this as a -32603
                        // Internal error, not -32002) — is surfaced to the
                        // frontend as SessionLoadFailed so the user can choose
                        // Reload vs New conversation. It is NOT auto-fallen-back
                        // to session/new, which would silently orphan the
                        // historical context (and, on a dead process, fail anyway
                        // and leak a raw protocol error). Every other failure
                        // keeps the session/new fallback below.
                        let err_str = e.to_string();
                        if let Some(code) = classify_session_load_failure(e.code, &err_str) {
                            tracing::warn!(
                                "[ACP] session/load failed ({err_str}); surfacing as session_load_failed={code}"
                            );
                            emit_with_state(
                                &state,
                                &emitter_clone,
                                AcpEvent::SessionLoadFailed {
                                    session_id: sid.clone(),
                                    message: err_str,
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
                        tracing::warn!(
                            "[ACP] session/load failed ({err_str}), falling back to session/new"
                        );
                        // Only emit a visible error for unexpected failures;
                        // "Method not found" is expected for agents that don't
                        // support session resume (e.g. Cline).
                        // "Authentication required" is expected for agents whose
                        // credentials have expired (e.g. Gemini CLI) — skip
                        // session/new too since it will also fail.
                        if err_str.contains("Authentication required") {
                            return Ok(());
                        }
                        if !err_str.contains("Method not found") {
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
                        ) {
                            Ok(r) => r,
                            Err(e) => {
                                return Err(
                                    bridge_acp_err_for_bootstrap(e, &route_bootstrap_tx).await
                                );
                            }
                        };
                        let new_resp = cx
                            .send_request_to(Agent, new_session_req)
                            .block_task()
                            .await?;
                        let fallback_sid = new_resp.session_id.0.to_string();
                        let initial_config_options = new_resp.config_options.clone();
                        let grok_meta = if agent_type == AgentType::Grok {
                            new_resp.meta.clone()
                        } else {
                            None
                        };
                        let mut session = cx.attach_session(new_resp, Default::default())?;
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
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        return Err(bridge_acp_err_for_bootstrap(e, &route_bootstrap_tx).await);
                    }
                };
                let new_resp = cx
                    .send_request_to(Agent, new_session_req)
                    .block_task()
                    .await?;
                let sid = new_resp.session_id.0.to_string();
                let initial_config_options = new_resp.config_options.clone();
                let grok_meta = if agent_type == AgentType::Grok {
                    new_resp.meta.clone()
                } else {
                    None
                };
                let mut session = cx.attach_session(new_resp, Default::default())?;
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
async fn handle_permission_request(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
    cwd: &str,
    req: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
) {
    // Internal title runs have no interactive UI path: decline immediately and
    // still emit so the private-stream runner observes Interactive failure.
    let is_internal_title = {
        let s = state.read().await;
        s.purpose == crate::auto_title::ConnectionPurpose::InternalTitle
    };
    if is_internal_title {
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

    perms.lock().await.insert(request_id.clone(), responder);

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
}

async fn emit_cancelled_permission_events(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    request_ids: impl IntoIterator<Item = String>,
) {
    for request_id in request_ids {
        emit_with_state(state, emitter, AcpEvent::PermissionResolved { request_id }).await;
    }
}

async fn cancel_pending_permissions(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    perms: &PendingPermissions,
) {
    let drained = perms.lock().await.drain().collect::<Vec<_>>();
    let mut request_ids = Vec::with_capacity(drained.len());
    for (request_id, responder) in drained {
        let _ = responder.respond(RequestPermissionResponse::new(
            RequestPermissionOutcome::Cancelled,
        ));
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
                kind: tcu
                    .fields
                    .kind
                    .map(|k| format!("{:?}", k).to_lowercase()),
                title: tcu.fields.title.clone(),
                has_terminal_content: !terminal_ids.is_empty(),
                status: tcu
                    .fields
                    .status
                    .map(|s| format!("{:?}", s).to_lowercase()),
            }
        }
        _ => return,
    };

    if let Ok(mut bridge) = assoc.lock() {
        bridge.observe_tool(session_id, hint);
    }
}

/// Merge fallback `tool_call_id ↔ terminal_id` binds into the live poller map.
/// Returns true when at least one new terminal id was attached.
fn merge_terminal_assoc_binds(
    session_id: &str,
    assoc: &std::sync::Mutex<TerminalAssocFallback>,
    tracked: &mut HashMap<String, TrackedTerminalToolCall>,
) -> bool {
    let binds = match assoc.lock() {
        Ok(mut bridge) => bridge.drain_pending_binds(session_id),
        Err(_) => return false,
    };
    if binds.is_empty() {
        return false;
    }

    let mut changed = false;
    for (tool_call_id, terminal_id) in binds {
        let entry = tracked.entry(tool_call_id).or_default();
        if merge_terminal_ids(&mut entry.terminal_ids, vec![terminal_id]) {
            changed = true;
        }
        if entry.status.is_none() {
            // Keep the poller from treating an unbound entry as already final.
            entry.status = Some("inprogress".into());
        }
    }
    changed
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
    let mut session = cx.attach_session(new_resp, Default::default())?;

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
    let token = state.read().await.delegation_token.clone();
    if let Some(token) = token {
        injection.leases.revoke(&token).await;
        injection.tokens.revoke(&token).await;
    }
    injection.broker.cancel_by_parent(connection_id).await;
    // Reclaim a parked `ask_user_question` instead of waiting for the
    // companion's ask socket to close; dropping the sender declines it cleanly.
    injection
        .questions
        .cancel_questions_by_parent(connection_id)
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
    emit_with_state(
        state,
        emitter,
        AcpEvent::TurnComplete {
            session_id: session_id.to_string(),
            stop_reason: stop_reason.to_string(),
            agent_type: agent_type.to_string(),
            mark_awaiting_reply,
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
            TurnFinalizationDisposition::SuspensionFailed
        }
        TurnFinalizationDisposition::UserCancelled => {
            if let Some(mut lease) = suspension.take() {
                reject_suspension_lease(&mut lease, "suspend_cancelled_by_user");
            }
            emit_with_state(
                state,
                emitter,
                AcpEvent::TurnComplete {
                    session_id: session_id.to_string(),
                    stop_reason: "cancelled".into(),
                    agent_type: agent_type.to_string(),
                    mark_awaiting_reply,
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
            Ok(ConnectionControl::Cancel) => {
                return Some(ActiveTerminalControl::UserCancel)
            }
            Ok(ConnectionControl::Disconnect) => {
                return Some(ActiveTerminalControl::Disconnect)
            }
            Err(_) => break,
        }
    }
    None
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
                set_grok_config_option(
                    &cx,
                    &sid,
                    &state,
                    &emitter,
                    config_id,
                    value_id,
                )
                .await
            } else {
                let is_mode = config_id == "mode";
                let mode_value = value_id.clone();
                let result = set_session_config_option(
                    &cx,
                    &sid,
                    &state,
                    &emitter,
                    agent_type,
                    config_id,
                    value_id,
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
    let _ = merge_terminal_assoc_binds(
        sid.0.as_ref(),
        terminal_assoc.as_ref(),
        tracked_terminal_tool_calls,
    );
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
    let reason_str = if raw_reason_str == "end_turn" && !turn_had_agent_output {
        "empty"
    } else {
        raw_reason_str
    };
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
            && matches!(
                disposition,
                TurnFinalizationDisposition::SuspensionFailed
            ),
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
    broker: Option<&crate::acp::delegation::broker::DelegationBroker>,
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
        broker,
    )
    .await;
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
                                        if advances_agent_activity(&notif.update) {
                                            st.write().await.mark_agent_activity(chrono::Utc::now());
                                        }
                                        emit_conversation_update(&st, &h, agent_type, notif.update, cwd_opt, &mut raw_output_cache, &mut cb_state).await;
                                        Ok(())
                                    },
                                )
                                .await
                                .otherwise(async |dispatch| {
                                    if maybe_emit_claude_sdk_ext_notification(&st, &h, dispatch)
                                        .await
                                    {
                                        // Only when a normalizer advanced
                                        // transcript/tool state (currently never
                                        // for api_retry-only envelopes).
                                        st.write().await.mark_agent_activity(chrono::Utc::now());
                                    }
                                    Ok(())
                                })
                                .await;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!("[ACP] Ignoring unrecognized session update in idle loop: {e}");
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
                // versioned shell context. Live UI / optimistic dedup / title
                // paths never see this block (they use `user_message` below).
                // InternalTitle must send the exact title prompt without a
                // silent terminal-instruction prefix.
                let skip_terminal_prefix = {
                    let s = state.read().await;
                    s.purpose == crate::auto_title::ConnectionPurpose::InternalTitle
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

                // Clone connection and session ID before entering the
                // select loop so we can send CancelNotification without
                // conflicting with session.read_update()'s mutable borrow.
                let cx = session.connection();
                let sid = session.session_id().clone();
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
                            ) {
                                Some(ActiveTerminalControl::UserCancel) => {
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
                                        delegation_injection
                                            .map(|injection| injection.broker.as_ref()),
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
                            ) {
                                Some(ActiveTerminalControl::UserCancel) => {
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
                                        delegation_injection
                                            .map(|injection| injection.broker.as_ref()),
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
                                Some(ConnectionControl::Cancel) => {
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
                                        delegation_injection
                                            .map(|injection| injection.broker.as_ref()),
                                    )
                                    .await;
                                    tokio::spawn(async move {
                                        let _ = prompt_response.await;
                                    });
                                    break;
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
                                                let outcome = RequestPermissionOutcome::Selected(
                                                    SelectedPermissionOutcome::new(option_id),
                                                );
                                                let _ = responder.respond(
                                                    RequestPermissionResponse::new(outcome),
                                                );
                                                emit_with_state(
                                                    state,
                                                    emitter,
                                                    AcpEvent::PermissionResolved { request_id },
                                                )
                                                .await;
                                            }
                                        }
                                        Err(ConnectionCommand::Fork { reply }) => {
                                            let _ = reply.send(Err(AcpError::TurnInProgress));
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
                                    // Grok `_x.ai/session/update` + turn_completed
                                    // is not a typed SessionNotification — handle
                                    // it before MatchDispatch so a stalled
                                    // session/prompt RPC cannot leave the row
                                    // stuck in `in_progress`.
                                    if let Some(ext_reason) =
                                        parse_extension_turn_completed(&dispatch)
                                    {
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
                                        let mut reason_str = ext_reason;
                                        if reason_str == "end_turn" && !turn_had_agent_output {
                                            reason_str = "empty".into();
                                        }
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
                                                if is_agent_output_update(&notif.update) {
                                                    turn_had_agent_output = true;
                                                }
                                                // Soft-watchdog: mark at inbound
                                                // boundary before event conversion.
                                                if advances_agent_activity(&notif.update) {
                                                    st.write()
                                                        .await
                                                        .mark_agent_activity(chrono::Utc::now());
                                                }
                                                emit_conversation_update(&st, &h, agent_type, notif.update, cwd_opt, &mut raw_output_cache, &mut cb_state).await;
                                                if should_poll_now || bound {
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
                                            if maybe_emit_claude_sdk_ext_notification(
                                                &st, &h, dispatch,
                                            )
                                            .await
                                            {
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
                                    let reason_str = if raw_reason_str == "end_turn"
                                        && !turn_had_agent_output
                                    {
                                        "empty"
                                    } else {
                                        raw_reason_str
                                    };
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
                if let Some(responder) = perms.lock().await.remove(&request_id) {
                    let outcome = RequestPermissionOutcome::Selected(
                        SelectedPermissionOutcome::new(option_id),
                    );
                    let _ = responder.respond(RequestPermissionResponse::new(outcome));
                    emit_with_state(state, emitter, AcpEvent::PermissionResolved { request_id })
                        .await;
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
            ConversationInput::Control(ConnectionControl::Cancel) => {
                let cx = session.connection();
                let sid = session.session_id().clone();
                let _ = cx.send_notification_to(Agent, CancelNotification::new(sid.clone()));
                cancel_pending_permissions(state, emitter, perms).await;
                terminal_runtime
                    .release_all_for_session(sid.0.as_ref())
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
                let terminal_meta = match session_request_meta(
                    agent_type,
                    route_plan,
                    shell_spec,
                    adapter_for(agent_type),
                ) {
                    Ok(meta) => meta,
                    Err(e) => {
                        let _ = reply.send(Err(e));
                        continue;
                    }
                };
                let result = crate::acp::fork::fork_session(&cx, &sid, cwd, terminal_meta).await;
                match result {
                    Ok(fork_response) => {
                        tracing::info!(
                            "[ACP] Fork succeeded: new_session_id={}",
                            fork_response.session_id.0
                        );
                        return Ok(Some(ForkExitInfo {
                            fork_response,
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
            ConversationInput::Control(ConnectionControl::Disconnect)
            | ConversationInput::ChannelsClosed => {
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
fn serialize_tool_call_content(content: &[ToolCallContent], include_diffs: bool) -> Option<String> {
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
fn synthesize_edit_input_from_diffs(content: &[ToolCallContent]) -> Option<String> {
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
fn extract_tool_call_images(content: &[ToolCallContent]) -> Option<Vec<ToolCallImageInfo>> {
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

fn json_value_to_text(val: &Option<serde_json::Value>) -> Option<String> {
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
/// to the object's string `output_for_prompt` (Bash/terminal) or, for an MCP
/// `rawOutput`, the text under `output` (see grok_mcp_output_text). Returning
/// `None` lets the frontend render `content`. Never emit the object blob.
/// Non-object / absent / unrecognized `rawOutput` → `None`.
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
/// represented by the Agent pill + its nested tool calls). Matches Claude Code,
/// which never streams a sub-agent's internal reasoning onto the main session.
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

/// Emit adapter-specific raw SDK notifications. Returns `true` only when a
/// normalizer advanced the same live transcript / tool state that the soft
/// watchdog treats as agent activity. Claude `api_retry` system messages are
/// UI status only and return `false`.
async fn maybe_emit_claude_sdk_ext_notification(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    dispatch: Dispatch,
) -> bool {
    let Dispatch::Notification(notification) = dispatch else {
        return false;
    };

    if let Some(event) = map_claude_sdk_ext_notification(&notification) {
        // api_retry → ClaudeSdkMessage does not advance transcript/tool state.
        emit_with_state(state, emitter, event).await;
        return false;
    }
    false
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

fn parse_extension_turn_completed_notification(
    notification: &UntypedMessage,
) -> Option<String> {
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
        "end_turn" | "cancelled" | "refusal" | "max_tokens" | "max_turn_requests"
        | "empty" | "unknown" => raw.to_string(),
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
async fn emit_conversation_update(
    state: &Arc<RwLock<SessionState>>,
    emitter: &EventEmitter,
    agent_type: AgentType,
    update: SessionUpdate,
    cwd: Option<&str>,
    raw_output_cache: &mut ToolCallOutputCache,
    cb_state: &mut CodeBuddyLiveState,
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
                emit_with_state(state, emitter, AcpEvent::ContentDelta { text: text.text }).await;
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
                emit_with_state(state, emitter, AcpEvent::Thinking { text: text.text }).await;
            }
        }
        SessionUpdate::AgentThoughtChunk(_) => {
            // Non-text thought chunks are currently ignored.
        }
        SessionUpdate::ToolCall(tc) => {
            let tool_call_id = tc.tool_call_id.to_string();
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
            let status = format!("{:?}", tc.status).to_lowercase();
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
            emit_with_state(
                state,
                emitter,
                AcpEvent::ToolCall {
                    tool_call_id,
                    title,
                    kind: format!("{:?}", tc.kind).to_lowercase(),
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
            let tool_call_id = tcu.tool_call_id.to_string();
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
            let status = tcu.fields.status.map(|s| format!("{:?}", s).to_lowercase());
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
            // Some agents (e.g. Claude Code with overlapping user/project slash
            // commands) emit duplicate entries sharing the same name. Keep the
            // first occurrence so downstream consumers don't render duplicates;
            // the frontend reducer also dedupes as a defensive measure.
            let mut seen = HashSet::new();
            let commands: Vec<AvailableCommandInfo> = update
                .available_commands
                .iter()
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
            if let Some(goal) = info
                .meta
                .as_ref()
                .and_then(|m| m.get("codex"))
                .and_then(|codex| codex.get("goal"))
            {
                if let Some(marker) =
                    crate::acp::codex_goal::next_goal_marker(&mut cb_state.codex_open_goal, goal)
                {
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
        other => {
            // Log unhandled update types for debugging
            tracing::info!("[ACP] Unhandled SessionUpdate: {:?}", other);
        }
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
    use std::sync::Arc;

    struct SuspensionLoopMockAgent {
        prompts: Arc<std::sync::Mutex<Vec<sacp::Responder<sacp::schema::PromptResponse>>>>,
        modes: Arc<std::sync::Mutex<Vec<sacp::Responder<sacp::schema::SetSessionModeResponse>>>>,
        agent_connection: Arc<std::sync::Mutex<Option<ConnectionTo<Client>>>>,
        cancel_count: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl sacp::ConnectTo<Client> for SuspensionLoopMockAgent {
        async fn connect_to(
            self,
            client: impl sacp::ConnectTo<Agent>,
        ) -> Result<(), sacp::Error> {
            use std::sync::atomic::Ordering;

            let prompt_responders = self.prompts;
            let prompt_connection = self.agent_connection.clone();
            let mode_responders = self.modes;
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
                                _connection: ConnectionTo<Client>| {
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

    fn delegation_suspend_injection(
        broker: Arc<crate::acp::delegation::broker::DelegationBroker>,
    ) -> DelegationInjection {
        DelegationInjection {
            broker,
            tokens: Arc::new(crate::acp::delegation::listener::TokenRegistry::default()),
            leases: Arc::new(
                crate::acp::delegation::lease::CompanionLeaseRegistry::default(),
            ),
            socket_path: PathBuf::from("/tmp/codeg-suspension-test.sock"),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            questions: Arc::new(SuspensionNoQuestions),
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

    #[allow(clippy::too_many_arguments)]
    async fn run_suspension_test_loop(
        mock_agent: SuspensionLoopMockAgent,
        state: Arc<RwLock<SessionState>>,
        mut cmd_rx: mpsc::Receiver<ConnectionCommand>,
        mut control_rx: mpsc::Receiver<ConnectionControl>,
        injection: DelegationInjection,
        seed_aux_stop_reason: bool,
    ) -> Result<(), sacp::Error> {
        Client
            .builder()
            .connect_with(mock_agent, async move |cx| {
                let session_id = SessionId::new("session-1".to_string());
                let mut session = cx.attach_session(
                    NewSessionResponse::new(session_id),
                    Default::default(),
                )?;
                if seed_aux_stop_reason {
                    session.send_prompt("auxiliary terminal producer")?;
                }
                let shell = test_placeholder_terminal_shell();
                let terminal_runtime = Arc::new(TerminalRuntime::new(
                    BTreeMap::new(),
                    shell.spec.clone(),
                    adapter_for(AgentType::Codex),
                ));
                let terminal_assoc = Arc::new(std::sync::Mutex::new(
                    TerminalAssocFallback::new(false),
                ));
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (control_tx, control_rx) = mpsc::channel(8);
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
            state,
            cmd_rx,
            control_rx,
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

        control_tx.send(ConnectionControl::Disconnect).await.unwrap();
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (control_tx, control_rx) = mpsc::channel(8);
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
        control_tx.send(ConnectionControl::Disconnect).await.unwrap();
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (control_tx, control_rx) = mpsc::channel(8);
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
        let ack_result = tokio::time::timeout(std::time::Duration::from_secs(1), receiver).await;
        stop_producer.store(true, Ordering::SeqCst);
        duplicate_producer.join().expect("duplicate producer join");
        ack_result
            .expect("ready bound response must not be starved by duplicate controls")
            .expect("bound prompt suspension reply")
            .expect("bound prompt suspension success");
        control_tx.send(ConnectionControl::Disconnect).await.unwrap();
        loop_task.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn delegation_suspend_closed_lanes_abort_hung_ancillary() {
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (control_tx, control_rx) = mpsc::channel(8);
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
            state,
            cmd_rx,
            control_rx,
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
        drop(cmd_tx);
        drop(control_tx);
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (control_tx, control_rx) = mpsc::channel(8);
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
        control_tx.send(ConnectionControl::Disconnect).await.unwrap();
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (control_tx, control_rx) = mpsc::channel(8);
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

        control_tx.send(ConnectionControl::Disconnect).await.unwrap();
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (control_tx, control_rx) = mpsc::channel(8);
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

        control_tx.send(ConnectionControl::Disconnect).await.unwrap();
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
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (control_tx, control_rx) = mpsc::channel(8);
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

    /// Build base Npx argv then apply the immutable route plan once — mirrors
    /// production `build_agent` for Npx agents.
    fn apply_base_npx_then_route(
        parts: &mut Vec<String>,
        agent_type: AgentType,
        args: &[&str],
        grok_always_approve: bool,
        plan: &DelegationRoutePlan,
    ) {
        append_npx_launch_args(parts, agent_type, args, grok_always_approve);
        apply_process_route(plan, agent_type, &mut BTreeMap::new(), parts).unwrap();
    }

    #[test]
    fn grok_npx_launch_args_disable_native_question_before_subcommand() {
        // Base args only (append_npx is route-independent): question deny remains.
        let mut without_auto_approve = vec!["grok".to_string()];
        append_npx_launch_args(
            &mut without_auto_approve,
            AgentType::Grok,
            &["agent", "stdio"],
            false,
        );
        assert_eq!(
            without_auto_approve,
            vec![
                "grok",
                "--no-auto-update",
                "--disallowed-tools",
                "ask_user_question",
                "agent",
                "stdio",
            ]
        );

        let mut with_auto_approve = vec!["grok".to_string()];
        append_npx_launch_args(
            &mut with_auto_approve,
            AgentType::Grok,
            &["agent", "stdio"],
            true,
        );
        assert_eq!(
            with_auto_approve,
            vec![
                "grok",
                "--no-auto-update",
                "--disallowed-tools",
                "ask_user_question",
                "--always-approve",
                "agent",
                "stdio",
            ]
        );
    }

    #[test]
    fn non_grok_npx_launch_args_remain_unchanged() {
        let mut parts = vec!["codex-acp".to_string()];
        append_npx_launch_args(&mut parts, AgentType::Codex, &["serve"], true);
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
            false,
            &codeg_grok,
        );
        assert_eq!(
            grok,
            vec![
                "grok",
                "--no-auto-update",
                "--disallowed-tools",
                "ask_user_question",
                "--no-subagents",
                "agent",
                "stdio",
            ]
        );

        // always-approve stays between question deny and --no-subagents.
        let mut grok_approve = vec!["grok".to_string()];
        apply_base_npx_then_route(
            &mut grok_approve,
            AgentType::Grok,
            &["agent", "stdio"],
            true,
            &codeg_plan(AgentType::Grok),
        );
        assert_eq!(
            grok_approve,
            vec![
                "grok",
                "--no-auto-update",
                "--disallowed-tools",
                "ask_user_question",
                "--always-approve",
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
            false,
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
            false,
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
            false,
            &native_plan(AgentType::Grok),
        );
        assert!(!native_grok.contains(&"--no-subagents".to_string()));
        // ask_user_question deny is independent of route.
        assert!(native_grok.contains(&"ask_user_question".to_string()));

        let mut native_cb = vec!["codebuddy".to_string()];
        apply_base_npx_then_route(
            &mut native_cb,
            AgentType::CodeBuddy,
            &["--acp"],
            false,
            &native_plan(AgentType::CodeBuddy),
        );
        assert_eq!(native_cb, vec!["codebuddy", "--acp"]);
        assert!(!native_cb.iter().any(|a| a == "--disallowedTools"));
    }

    #[test]
    fn codex_env_and_claude_meta_are_additive_and_route_scoped() {
        // Codex Codeg: set/override CODEX_ACP_MULTI_AGENT=0; leave unrelated keys.
        // Exercise `apply_process_route` (env + argv) as the process-level entry.
        let mut codeg_env = BTreeMap::from([
            ("KEEP".into(), "yes".into()),
            ("CODEX_ACP_MULTI_AGENT".into(), "1".into()),
        ]);
        let mut codeg_argv = Vec::new();
        apply_process_route(
            &codeg_plan(AgentType::Codex),
            AgentType::Codex,
            &mut codeg_env,
            &mut codeg_argv,
        )
        .unwrap();
        assert_eq!(
            codeg_env.get("CODEX_ACP_MULTI_AGENT").map(String::as_str),
            Some("0")
        );
        assert_eq!(codeg_env.get("KEEP").map(String::as_str), Some("yes"));
        assert!(codeg_argv.is_empty());

        // Native preserves user env byte-for-byte (fresh maps; no cross-route reuse).
        for user_val in ["1", "0"] {
            let mut native_env = BTreeMap::from([
                ("KEEP".into(), "yes".into()),
                ("CODEX_ACP_MULTI_AGENT".into(), user_val.into()),
            ]);
            apply_route_environment(
                AgentType::Codex,
                &native_plan(AgentType::Codex),
                &mut native_env,
            )
            .unwrap();
            assert_eq!(
                native_env.get("CODEX_ACP_MULTI_AGENT").map(String::as_str),
                Some(user_val)
            );
            assert_eq!(native_env.get("KEEP").map(String::as_str), Some("yes"));
        }

        // Non-Codex plan never touches CODEX_ACP_MULTI_AGENT.
        let mut grok_env = BTreeMap::from([("CODEX_ACP_MULTI_AGENT".into(), "1".into())]);
        apply_route_environment(AgentType::Grok, &codeg_plan(AgentType::Grok), &mut grok_env)
            .unwrap();
        assert_eq!(
            grok_env.get("CODEX_ACP_MULTI_AGENT").map(String::as_str),
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
            "--disallowed-tools".to_string(),
            "ask_user_question".to_string(),
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
                "--disallowed-tools",
                "ask_user_question",
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

        // always-approve stays between question deny and --no-subagents.
        let mut grok_approve = vec![
            "grok".to_string(),
            "--no-auto-update".to_string(),
            "--disallowed-tools".to_string(),
            "ask_user_question".to_string(),
            "--always-approve".to_string(),
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
                "--disallowed-tools",
                "ask_user_question",
                "--always-approve",
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

    /// Two prompts on one process-scoped injector: only the first is enriched.
    #[test]
    fn terminal_prompt_context_one_shot_across_two_prompts() {
        let injector = TerminalPromptContext::new(test_pwsh_spec());
        let mut first = map_prompt_blocks(vec![PromptInputBlock::Text {
            text: "first".into(),
        }]);
        injector.append_once(&mut first);
        let mut second = map_prompt_blocks(vec![PromptInputBlock::Text {
            text: "second".into(),
        }]);
        injector.append_once(&mut second);
        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 1);
    }

    /// Fork reuses the same Arc; reattachment never re-enters `run_connection`
    /// (injector lives only there), so mid-process reconnect cannot append a
    /// superseding context.
    #[test]
    fn terminal_prompt_context_survives_fork_and_reattachment_semantics() {
        let injector = Arc::new(TerminalPromptContext::new(test_pwsh_spec()));
        let mut first = map_prompt_blocks(vec![PromptInputBlock::Text {
            text: "pre-fork".into(),
        }]);
        injector.append_once(&mut first);
        // Simulate fork: same Arc into the restarted conversation loop.
        let forked = Arc::clone(&injector);
        let mut post_fork = map_prompt_blocks(vec![PromptInputBlock::Text {
            text: "post-fork".into(),
        }]);
        forked.append_once(&mut post_fork);
        assert_eq!(first.len(), 2);
        assert_eq!(post_fork.len(), 1);
        // "Reattachment" is just another hold on the same process injector —
        // AgentConnection reuse does not call TerminalPromptContext::new.
        let reattached = Arc::clone(&injector);
        let mut mid = map_prompt_blocks(vec![PromptInputBlock::Text {
            text: "reattach".into(),
        }]);
        reattached.append_once(&mut mid);
        assert_eq!(mid.len(), 1);
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
        )
        .unwrap();

        // Terminal metadata is always present; Claude raw-SDK meta is not.
        let meta = req.meta.as_ref().expect("terminal meta required");
        assert!(!meta.contains_key("claudeCode"));
        assert!(meta.contains_key("codeg.dev/terminal"));
    }

    #[test]
    fn build_resume_session_request_sets_claude_raw_meta() {
        let cwd = std::path::PathBuf::from("/tmp/codeg");
        let req = build_resume_session_request(
            AgentType::ClaudeCode,
            SessionId::new("abc".to_string()),
            &cwd,
            Vec::new(),
            &test_posix_spec(),
            adapter_for(AgentType::ClaudeCode),
            &native_plan(AgentType::ClaudeCode),
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
        )
        .unwrap();

        let meta = req.meta.as_ref().expect("terminal meta required");
        assert!(!meta.contains_key("claudeCode"));
        assert!(meta.contains_key("codeg.dev/terminal"));
    }

    /// Grok pager `--no-ask-user` stamps `_meta.askUserQuestion = false` so the
    /// shell strips native `GrokBuild:ask_user_question`. Codeg must do the same
    /// on every session open path (new / load / resume); ACP ignores the
    /// headless-only `--disallowed-tools` flag.
    #[test]
    fn grok_session_meta_disables_native_ask_user_on_new_load_resume() {
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
        )
        .unwrap();

        for (label, meta) in [
            ("new", new_req.meta.as_ref()),
            ("load", load_req.meta.as_ref()),
            ("resume", resume_req.meta.as_ref()),
        ] {
            let meta = meta.unwrap_or_else(|| panic!("{label}: session meta required"));
            assert_eq!(
                meta.get("askUserQuestion").and_then(|v| v.as_bool()),
                Some(false),
                "{label}: askUserQuestion must be false (Grok --no-ask-user parity)"
            );
            // Terminal snapshot still merges; the ask-user stamp is additive.
            assert!(
                meta.contains_key("codeg.dev/terminal"),
                "{label}: terminal meta must remain"
            );
        }
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
        let request = build_initialize_request(&spec, adapter_for(AgentType::Codex)).unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_codeg_terminal_meta(&value, "powershell", &spec.executable.to_string_lossy());
    }

    #[test]
    fn claude_and_terminal_metadata_are_merged() {
        let request = build_new_session_request(
            AgentType::ClaudeCode,
            Path::new("/tmp/project"),
            Vec::new(),
            &test_posix_spec(),
            adapter_for(AgentType::ClaudeCode),
            &native_plan(AgentType::ClaudeCode),
        )
        .unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["_meta"]["claudeCode"]["emitRawSDKMessages"], true);
        assert_codeg_terminal_meta(&value, "posix", "/bin/sh");
    }

    #[test]
    fn load_session_contains_terminal_metadata() {
        let spec = test_pwsh_spec();
        let request = build_load_session_request(
            AgentType::Codex,
            SessionId::new("sess-load"),
            Path::new("/tmp/project"),
            Vec::new(),
            &spec,
            adapter_for(AgentType::Codex),
            &native_plan(AgentType::Codex),
        )
        .unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_codeg_terminal_meta(&value, "powershell", &spec.executable.to_string_lossy());
    }

    #[test]
    fn resume_session_contains_terminal_metadata() {
        let spec = test_posix_spec();
        let request = build_resume_session_request(
            AgentType::ClaudeCode,
            SessionId::new("sess-resume"),
            Path::new("/tmp/project"),
            Vec::new(),
            &spec,
            adapter_for(AgentType::ClaudeCode),
            &native_plan(AgentType::ClaudeCode),
        )
        .unwrap();
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["_meta"]["claudeCode"]["emitRawSDKMessages"], true);
        assert_codeg_terminal_meta(&value, "posix", "/bin/sh");
    }

    /// Fake adapter that tries to inject a conflicting `codeg.dev/terminal`
    /// value. Codeg's authoritative snapshot must win after merge.
    struct ConflictingTerminalAdapter;

    impl AcpTerminalAdapter for ConflictingTerminalAdapter {
        fn agent_metadata(&self, _shell: &ResolvedShellSpec) -> Result<Meta, AcpError> {
            let mut meta = Meta::default();
            meta.insert(
                "codeg.dev/terminal".into(),
                serde_json::json!({
                    "shell": "/bogus/from-adapter",
                    "dialect": "cmd",
                    "platform": "bogus",
                    "commandMode": "adapter-lies",
                }),
            );
            meta.insert("adapterExtra".into(), serde_json::json!({"ok": true}));
            Ok(meta)
        }
    }

    #[test]
    fn terminal_metadata_overwrites_adapter_collision() {
        let spec = test_pwsh_spec();
        let request = build_initialize_request(&spec, &ConflictingTerminalAdapter).unwrap();
        let value = serde_json::to_value(request).unwrap();
        // Codeg snapshot wins on the reserved namespace.
        assert_codeg_terminal_meta(&value, "powershell", &spec.executable.to_string_lossy());
        // Non-colliding adapter keys are still merged in.
        assert_eq!(value["_meta"]["adapterExtra"]["ok"], true);
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
    fn synthesize_grok_config_options_yields_model_only_selector() {
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

        let opts = synthesize_grok_config_options(Some(&meta)).expect("should synthesize");
        assert_eq!(opts.len(), 1, "only the model selector is surfaced");
        let opt = &opts[0];
        assert_eq!(opt.id, GROK_MODEL_OPTION_ID);
        assert_eq!(opt.category.as_deref(), Some("model"));
        let SessionConfigKindInfo::Select(sel) = &opt.kind;
        // Both models appear (agent-type filtering is deliberately NOT applied —
        // cross-type switches are handled gracefully at set time instead).
        assert_eq!(sel.options.len(), 2);
        assert_eq!(
            sel.current_value, "grok-4.5",
            "the `selected` model is current"
        );
        assert!(sel
            .options
            .iter()
            .any(|o| o.value == "grok-composer-2.5-fast"));
        // The "mode" (effort) entries are excluded from the composer.
        assert!(sel
            .options
            .iter()
            .all(|o| o.value != "high" && o.value != "low"));
    }

    #[test]
    fn synthesize_grok_config_options_none_without_sessionconfig() {
        let empty: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
        assert!(synthesize_grok_config_options(Some(&empty)).is_none());
        assert!(synthesize_grok_config_options(None).is_none());
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
    async fn inject_codeg_delegate_skipped_when_broker_disabled() {
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
        let injection = DelegationInjection {
            broker,
            tokens: Arc::new(TokenRegistry::default()),
            leases: Arc::new(crate::acp::delegation::lease::CompanionLeaseRegistry::default()),
            socket_path: std::path::PathBuf::from("/tmp/codeg-mcp.sock"),
            feedback: crate::acp::feedback::FeedbackRuntimeConfig::new(),
            ask: crate::acp::question::QuestionRuntimeConfig::new(),
            sessions: crate::acp::session_info::SessionInfoRuntimeConfig::new(),
            questions: Arc::new(NoQuestions)
                as Arc<dyn crate::acp::question::SessionQuestionAccess>,
            supervisor_wake: crate::acp::delegation::supervisor::SupervisorWake::noop(),
            metrics: Arc::new(crate::acp::delegation::metrics::DelegationMetrics::default()),
        };

        let mut servers: Vec<McpServer> = Vec::new();
        // Plan does not expose delegation; feedback/ask/sessions off → skip.
        let plan = native_plan(AgentType::Codex);
        let result = inject_codeg_mcp(
            &mut servers,
            &injection,
            "parent-conn",
            std::path::Path::new("/tmp"),
            &plan,
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
            companion_features_arg(true, true, true, false, false),
            Some("delegation,coordination_v1,feedback".into())
        );
        assert_eq!(
            companion_features_arg(false, false, true, false, false),
            Some("feedback".into())
        );
        assert_eq!(
            companion_features_arg(false, false, false, false, false),
            None
        );
    }

    #[test]
    fn companion_features_arg_inject_skip_decision() {
        // All off → no companion at all.
        assert_eq!(
            companion_features_arg(false, false, false, false, false),
            None
        );
        // Delegation only without coordination → no Join token.
        assert_eq!(
            companion_features_arg(true, false, false, false, false),
            Some("delegation".to_string())
        );
        // Delegation + coordination_v1 (production Codeg-delegation plan).
        assert_eq!(
            companion_features_arg(true, true, false, false, false),
            Some("delegation,coordination_v1".to_string())
        );
        // Feedback only — the decoupling: companion injected for feedback even
        // when delegation is off.
        assert_eq!(
            companion_features_arg(false, false, true, false, false),
            Some("feedback".to_string())
        );
        // Ask only — likewise injects the companion on its own.
        assert_eq!(
            companion_features_arg(false, false, false, true, false),
            Some("ask".to_string())
        );
        // Sessions only — likewise injects the companion on its own.
        assert_eq!(
            companion_features_arg(false, false, false, false, true),
            Some("sessions".to_string())
        );
        // All on → comma-joined, in declaration order.
        assert_eq!(
            companion_features_arg(true, true, true, true, true),
            Some("delegation,coordination_v1,feedback,ask,sessions".to_string())
        );
    }
}
