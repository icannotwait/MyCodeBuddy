use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};
use std::time::Duration;

use sacp::schema::{
    CreateTerminalRequest, CreateTerminalResponse, KillTerminalRequest, KillTerminalResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, TerminalExitStatus, TerminalOutputRequest,
    TerminalOutputResponse, WaitForTerminalExitRequest, WaitForTerminalExitResponse,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{watch, Mutex, Notify, RwLock};
use tokio::task::JoinHandle;

use crate::acp::terminal_adapter::{adapter_for, AcpTerminalAdapter};
use crate::models::agent::AgentType;
use crate::terminal::shell::{
    build_command_line, ResolvedShellSpec, ShellCommandStrategy, ShellDialect, ShellSource,
};
use crate::terminal::shell_flavor::{classify_shell_family, ShellFamily};

type TerminalMap = HashMap<String, Arc<TerminalInstance>>;
const DEFAULT_OUTPUT_BYTE_LIMIT: u64 = 1_000_000;

fn acp_terminal_runtimes() -> &'static StdMutex<Vec<Weak<TerminalRuntime>>> {
    static REGISTRY: OnceLock<StdMutex<Vec<Weak<TerminalRuntime>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| StdMutex::new(Vec::new()))
}

/// Register a connection's ACP terminal runtime so process quit can kill
/// `terminal/create` children that live only on the connection thread.
pub fn register_acp_terminal_runtime(runtime: &Arc<TerminalRuntime>) {
    let mut list = acp_terminal_runtimes()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    list.retain(|weak| weak.strong_count() > 0);
    list.push(Arc::downgrade(runtime));
}

/// Kill every live ACP `terminal/create` process in this host.
pub async fn kill_all_registered_acp_terminals() {
    let runtimes: Vec<Arc<TerminalRuntime>> = {
        let mut list = acp_terminal_runtimes()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        list.retain(|weak| weak.strong_count() > 0);
        list.iter().filter_map(Weak::upgrade).collect()
    };
    for runtime in runtimes {
        runtime.kill_all().await;
    }
}
/// After the child process exits, wait up to this long for the stdout/stderr
/// reader tasks to drain naturally before aborting them. Needed because a
/// grandchild process (e.g. Node spawned from a `.cmd` shim on Windows) can
/// inherit the pipe handle and keep it open long after the direct child
/// exits, turning `wait_for_exit` into a silent hang.
const READER_DRAIN_GRACE: Duration = Duration::from_millis(200);
/// On POSIX, how long a killed command gets to honor `SIGTERM` before the owner
/// task escalates to `SIGKILL`. Windows uses `TerminateProcess` for the first
/// tree-kill pass, so it does not depend on this grace period.
const KILL_ESCALATE_GRACE: Duration = Duration::from_secs(2);
/// How long `kill_command` waits to be able to *report* that the process is
/// gone. Bounding the report keeps session teardown and the cancel path
/// responsive; it does NOT abandon the process — the owner task holds the
/// `Child` and keeps reaping past this deadline.
const KILL_REPORT_BUDGET: Duration = Duration::from_secs(5);
/// Backoff cap between retries when `Child::wait` itself fails.
const WAIT_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(1);
/// How long the owner task retries a failing `Child::wait` before publishing a
/// terminal state anyway. Without this, a persistently failing wait would pin
/// `TerminalCompletion::Running` forever and any agent that polls
/// `terminal/output` (rather than calling `terminal/wait_for_exit`) would hang —
/// the very bug this module was rewritten to remove. The owner task keeps
/// owning and reaping the child afterwards; only the *reporting* is unblocked.
const WAIT_ERROR_BUDGET: Duration = Duration::from_secs(30);
/// Retry cadence after `WAIT_ERROR_BUDGET` is spent and completion has already
/// been published. Purely janitorial at that point, so it can be slow.
const WAIT_ERROR_IDLE_RETRY: Duration = Duration::from_secs(5);

/// How a `terminal/create` request will be executed.
///
/// Classification is deterministic and never retries a failed spawn under a
/// different mode or shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Spawn `program` directly with the request's exact `args` boundaries.
    DirectProgram(PathBuf),
    /// Run `request.command` as a single line through the connection's
    /// snapshotted shell via [`build_command_line`].
    ShellCommandLine,
}

#[derive(Debug)]
pub enum TerminalRuntimeError {
    InvalidParams(String),
    Spawn {
        code: &'static str,
        executable: String,
        display_name: String,
        mode: &'static str,
        os_error: String,
    },
    Internal(String),
}

impl TerminalRuntimeError {
    pub fn into_rpc_error(self) -> sacp::Error {
        match self {
            Self::InvalidParams(message) => sacp::Error::invalid_params().data(message),
            Self::Spawn {
                code,
                executable,
                display_name,
                mode,
                os_error,
            } => {
                // Stable structured diagnostics only: base env, request env,
                // and command text can contain credentials.
                sacp::Error::new(
                    -32603,
                    format!("failed to spawn terminal ({mode}): {os_error}"),
                )
                .data(serde_json::json!({
                    "code": code,
                    "executable": executable,
                    "displayName": display_name,
                    "mode": mode,
                    "osError": os_error,
                }))
            }
            Self::Internal(message) => sacp::util::internal_error(message),
        }
    }
}

#[derive(Debug, Default, Clone)]
struct TerminalSnapshot {
    output: String,
    output_base_offset: u64,
    truncated: bool,
    exit_status: Option<TerminalExitStatus>,
}

/// What the owner task has published about the child process. Two states only:
/// either it is still running, or it is **known** to be gone.
///
/// There is deliberately no "failed" state. An unknown-but-maybe-finished state
/// would be indistinguishable from `Running` to `terminal/output` consumers
/// (`exit_status: None` reads as "still going"), which is exactly how an agent
/// turn hangs forever.
#[derive(Debug, Clone)]
enum TerminalCompletion {
    Running,
    /// The process is gone. Carries the real status when `Child::wait`
    /// reported one; carries an empty [`TerminalExitStatus`] (no code, no
    /// signal — ACP's "finished, details unknown") when the status is
    /// unknowable, e.g. something else reaped the child out from under us.
    Exited(TerminalExitStatus),
}

struct TerminalInstance {
    session_id: String,
    output_limit: Option<usize>,
    snapshot: Mutex<TerminalSnapshot>,
    reader_handles: Mutex<Vec<JoinHandle<()>>>,
    /// Asks the owner task to kill the process tree.
    ///
    /// Callers signal rather than holding a pid of their own, so every kill
    /// runs while the owner still holds the **un-reaped** `Child`. That is what
    /// makes the pid safe to signal: an un-reaped child keeps its pid reserved,
    /// so it cannot have been recycled onto some unrelated process.
    kill: Notify,
    /// Terminal state, published by the owner task.
    ///
    /// Always written with `send_replace`, never `send`: a `watch` **discards**
    /// a value sent while no receiver exists (see the `Sender::new` doc in
    /// tokio's `sync/watch.rs`, which asserts `send(..).is_err()`), so a
    /// short-lived command that exits before anyone subscribes would strand
    /// every later waiter.
    completion: watch::Sender<TerminalCompletion>,
}

impl TerminalInstance {
    fn new(session_id: String, output_limit: Option<u64>) -> Self {
        Self {
            session_id,
            output_limit: output_limit.and_then(|v| usize::try_from(v).ok()),
            snapshot: Mutex::new(TerminalSnapshot::default()),
            reader_handles: Mutex::new(Vec::new()),
            kill: Notify::new(),
            completion: watch::Sender::new(TerminalCompletion::Running),
        }
    }

    /// Wait briefly for stdout/stderr reader tasks to finish; abort any that
    /// remain. Must be called after the direct child has already exited —
    /// otherwise we would abort readers that are still making progress.
    async fn drain_readers(&self) {
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *self.reader_handles.lock().await);
        for handle in handles {
            let abort = handle.abort_handle();
            if tokio::time::timeout(READER_DRAIN_GRACE, handle)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
    }

    async fn append_output(&self, text: &str) {
        let mut snapshot = self.snapshot.lock().await;
        snapshot.output.push_str(text);
        if let Some(limit) = self.output_limit {
            let removed = enforce_output_limit(&mut snapshot.output, limit);
            if removed > 0 {
                snapshot.truncated = true;
                snapshot.output_base_offset = snapshot
                    .output_base_offset
                    .saturating_add(u64::try_from(removed).unwrap_or(u64::MAX));
            }
        }
    }

    /// Block until the owner task publishes a terminal state.
    ///
    /// Holds no lock for the duration, so `terminal/output`, `terminal/kill`
    /// and the session cancel path stay responsive while a never-exiting
    /// command (a dev server, `tail -f`, a watcher) is still running.
    async fn wait_for_exit(&self) -> Result<TerminalExitStatus, TerminalRuntimeError> {
        let mut completion = self.completion.subscribe();
        loop {
            let current = completion.borrow_and_update().clone();
            if let TerminalCompletion::Exited(exit_status) = current {
                return Ok(exit_status);
            }
            if completion.changed().await.is_err() {
                return Err(TerminalRuntimeError::Internal(
                    "terminal owner task ended without publishing an exit status".to_string(),
                ));
            }
        }
    }

    async fn kill_command(&self) -> Result<(), TerminalRuntimeError> {
        if matches!(*self.completion.borrow(), TerminalCompletion::Exited(_)) {
            return Ok(());
        }

        // The owner task performs the kill; see `TerminalInstance::kill`.
        self.kill.notify_one();

        // Bound only how long we wait to *report*. The owner task keeps the
        // `Child` and keeps escalating and reaping regardless, so returning
        // early here never abandons the process — even once `release_terminal`
        // has already dropped this terminal from the map.
        if tokio::time::timeout(KILL_REPORT_BUDGET, self.wait_for_exit())
            .await
            .is_err()
        {
            tracing::warn!(
                "[ACP] terminal for session {} did not reap within {:?} of being killed; \
                 the owner task keeps trying",
                self.session_id,
                KILL_REPORT_BUDGET
            );
        }
        Ok(())
    }

    async fn snapshot(&self) -> TerminalSnapshot {
        self.snapshot.lock().await.clone()
    }
}

/// Signal a whole process tree, returning the pids the pass actually signalled.
///
/// `kill_tree` snapshots the tree, signals it children-first, and returns
/// immediately without waiting for anything to die. A descendant that ignores
/// the signal therefore outlives its parent and gets reparented away from
/// `pid`, at which point no snapshot rooted at `pid` can find it again. The
/// returned pids are the only remaining handle on such a survivor.
async fn signal_tree(pid: Option<u32>, signal: &str) -> Vec<u32> {
    let Some(pid) = pid else {
        return Vec::new();
    };

    // Windows has no POSIX signal escalation: the patched vendored backend
    // snapshots the tree and uses TerminateProcess. Call that path directly so
    // correctness never depends on a signal string the backend ignores.
    #[cfg(windows)]
    let kill_result = kill_tree::tokio::kill_tree(pid).await;
    #[cfg(not(windows))]
    let kill_result = {
        let config = kill_tree::Config {
            signal: signal.to_string(),
            include_target: true,
        };
        kill_tree::tokio::kill_tree_with_config(pid, &config).await
    };

    match kill_result {
        Ok(outputs) => outputs
            .into_iter()
            .filter_map(|output| match output {
                kill_tree::Output::Killed { process_id, .. } => Some(process_id),
                kill_tree::Output::MaybeAlreadyTerminated { .. } => None,
            })
            .collect(),
        Err(err) => {
            tracing::error!("[ACP] kill_tree({signal}) failed for pid {pid}: {err}");
            Vec::new()
        }
    }
}

/// Hard-kill any recorded descendant that outlived the first tree-kill pass.
/// This is `SIGKILL` on POSIX and `TerminateProcess` on Windows.
///
/// Best-effort by construction: survivors are identified by pid alone, so a pid
/// recycled inside the escalation window could in principle be signalled by
/// mistake. That is the same trade-off `pkill -P`-style cleanup makes, and it
/// is the price of reaching a descendant that has been reparented away. `root`
/// is skipped — it is the direct child, whose pid is already protected by the
/// un-reaped `Child` handle the owner task holds.
async fn sweep_survivors(signalled: &HashSet<u32>, root: Option<u32>) {
    for &pid in signalled {
        if Some(pid) == root {
            continue;
        }
        signal_tree(Some(pid), "SIGKILL").await;
    }
}

/// True when `Child::wait` failed because the child was already reaped by
/// something else, which means the process **is** gone but its status is lost
/// forever — and, critically, that its pid is now free for reuse and must not
/// be signalled.
///
/// Unix-only on purpose: on Windows `raw_os_error` carries Win32 codes, a
/// different namespace from the CRT errno `libc::ECHILD` belongs to. Mirrors
/// the `cfg`-guarded errno check in `crate::process::is_exec_busy`.
#[cfg(unix)]
fn is_already_reaped(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(libc::ECHILD)
}

#[cfg(not(unix))]
fn is_already_reaped(_err: &std::io::Error) -> bool {
    false
}

/// Sole owner of a spawned terminal's `Child` for the process's whole life.
///
/// Everything else in this module reads a snapshot or signals this task, which
/// is what keeps `terminal/output`, `terminal/kill` and session teardown off
/// the unbounded `Child::wait`. The task ends only once the process is **known**
/// to be gone. Spawn uses `kill_on_drop(true)` so aborting this owner (or
/// dropping `Child` on task cancel) does not leak the OS process; quit still
/// also runs [`kill_all_registered_acp_terminals`].
async fn own_terminal_process(terminal: Arc<TerminalInstance>, mut child: tokio::process::Child) {
    let pid = child.id();
    // Cumulative across every pass, never overwritten: a later `SIGKILL` pass
    // re-snapshots the (by then smaller) tree, so only the union still holds a
    // descendant that was recorded earlier and has since been reparented.
    let mut signalled: HashSet<u32> = HashSet::new();
    let mut escalate_at: Option<tokio::time::Instant> = None;
    let mut kill_requested = false;
    let mut escalated = false;
    // State for the `Child::wait`-keeps-failing path.
    let mut backoff = Duration::from_millis(10);
    let mut wait_error_deadline: Option<tokio::time::Instant> = None;
    let mut published_without_reaping = false;

    let exit_status = loop {
        let escalate = async {
            match escalate_at {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            reaped = child.wait() => match reaped {
                Ok(status) => break map_exit_status(status),
                Err(err) if is_already_reaped(&err) => {
                    // Gone, but the status died with whoever reaped it. Do NOT
                    // signal `pid` from here on: it is reusable now.
                    tracing::error!(
                        "[ACP] terminal child was reaped elsewhere; exit status unavailable: {err}"
                    );
                    terminal
                        .append_output("\n[terminal exit status unavailable: child reaped elsewhere]\n")
                        .await;
                    break TerminalExitStatus::new();
                }
                Err(err) => {
                    // Not "already reaped", so nothing has collected this child
                    // and its pid is still ours. It may well still be running:
                    // kill it hard and keep trying to reap it.
                    tracing::error!("[ACP] failed waiting for terminal process to exit: {err}");
                    signalled.extend(signal_tree(pid, "SIGKILL").await);

                    let deadline = *wait_error_deadline
                        .get_or_insert_with(|| tokio::time::Instant::now() + WAIT_ERROR_BUDGET);
                    if !published_without_reaping && tokio::time::Instant::now() >= deadline {
                        // Budget spent. Unblock every reader of the completion
                        // state so no agent turn is left hanging, but keep
                        // owning the child so cleanup still has a real handle.
                        published_without_reaping = true;
                        terminal
                            .append_output(
                                "\n[terminal exit status unavailable: could not reap the process]\n",
                            )
                            .await;
                        terminal.drain_readers().await;
                        let unknown = TerminalExitStatus::new();
                        terminal.snapshot.lock().await.exit_status = Some(unknown.clone());
                        terminal
                            .completion
                            .send_replace(TerminalCompletion::Exited(unknown));
                    }

                    let pause = if published_without_reaping {
                        WAIT_ERROR_IDLE_RETRY
                    } else {
                        backoff = (backoff * 2).min(WAIT_RETRY_MAX_BACKOFF);
                        backoff
                    };
                    tokio::time::sleep(pause).await;
                }
            },
            () = escalate => {
                escalate_at = None;
                escalated = true;
                signalled.extend(signal_tree(pid, "SIGKILL").await);
            }
            () = terminal.kill.notified() => {
                let signal = if escalated { "SIGKILL" } else { "SIGTERM" };
                signalled.extend(signal_tree(pid, signal).await);
                // Only the first request arms the escalation deadline. Clearing
                // `escalate_at` when the timer fires means a plain
                // `get_or_insert` would let a later request arm a fresh one and
                // push `SIGKILL` back indefinitely.
                if !kill_requested {
                    kill_requested = true;
                    escalate_at = Some(tokio::time::Instant::now() + KILL_ESCALATE_GRACE);
                }
            }
        }
    };

    if kill_requested {
        sweep_survivors(&signalled, pid).await;
    }

    if published_without_reaping {
        // Completion was already published on the budget-exhausted path; the
        // real status arrived late and nobody is waiting for it any more.
        return;
    }

    // Drain readers BEFORE publishing. Otherwise a caller polling
    // `terminal/output` can see `exit_status = Some(...)` while a grandchild
    // process (e.g. Node spawned from a `.cmd` shim on Windows) still holds the
    // stdout/stderr pipe and is flushing tail output. If the agent treats
    // exit_status as "terminal done", the trailing bytes never reach the UI.
    // Draining first upholds the invariant: whenever an external observer sees
    // exit_status, the snapshot already contains (or has explicitly given up
    // on) all reader output.
    terminal.drain_readers().await;
    terminal.snapshot.lock().await.exit_status = Some(exit_status.clone());
    terminal
        .completion
        .send_replace(TerminalCompletion::Exited(exit_status));
}

pub struct TerminalRuntime {
    terminals: Mutex<TerminalMap>,
    /// Base environment merged into every spawned terminal command before
    /// the agent's per-request `env` is applied. This is where the codeg
    /// git credential helper (`GIT_CONFIG_*`) lives so an agent that runs
    /// `git push` via the ACP `terminal/create` tool inherits the same
    /// auth path the agent process itself does. Per-request env from the
    /// agent overrides on key collision so an agent can still scrub or
    /// override anything explicitly.
    base_env: BTreeMap<String, String>,
    /// Fallback working directory applied to spawned terminals when the
    /// agent's `terminal/create` request omits `cwd`. The connection layer
    /// sets this to the session's resolved working directory so terminals
    /// default to the folder the conversation runs in instead of codeg's own
    /// process cwd (often "/" on desktop, the dev crate dir in development).
    /// `None` leaves the process cwd inherited (legacy behavior).
    default_cwd: Option<PathBuf>,
    /// Immutable launch-time shell snapshot for this connection.
    terminal_shell: ResolvedShellSpec,
    /// Per-agent request normalization hooks.
    adapter: &'static dyn AcpTerminalAdapter,
}

/// Shared, hot-swappable default shell for ACP terminal requests.
///
/// Owned by [`crate::acp::manager::ConnectionManager`] and read when applying
/// persisted General Settings.
#[derive(Clone, Default)]
pub struct TerminalShellRuntimeConfig {
    inner: Arc<RwLock<Option<String>>>,
}

impl TerminalShellRuntimeConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn snapshot(&self) -> Option<String> {
        self.inner.read().await.clone()
    }

    pub async fn set(&self, default_shell: Option<String>) {
        *self.inner.write().await = default_shell;
    }
}

#[derive(Debug, Clone)]
pub struct TerminalOutputDelta {
    pub output: String,
    pub next_offset: u64,
    pub had_gap: bool,
    pub truncated: bool,
    pub exit_status: Option<TerminalExitStatus>,
}

impl TerminalRuntime {
    /// Construct a runtime with the connection's base env, immutable shell
    /// snapshot, and static terminal adapter.
    pub fn new(
        base_env: BTreeMap<String, String>,
        terminal_shell: ResolvedShellSpec,
        adapter: &'static dyn AcpTerminalAdapter,
    ) -> Self {
        Self {
            terminals: Mutex::new(HashMap::new()),
            base_env,
            default_cwd: None,
            terminal_shell,
            adapter,
        }
    }

    /// Compatibility constructor for callers that predate connection-scoped
    /// shell snapshots. New connection code should use [`Self::new`].
    pub fn with_base_env(base_env: BTreeMap<String, String>) -> Self {
        #[cfg(windows)]
        let terminal_shell = ResolvedShellSpec {
            executable: PathBuf::from(
                std::env::var_os("COMSPEC").unwrap_or_else(|| OsString::from("cmd.exe")),
            ),
            dialect: ShellDialect::Cmd,
            display_name: "Command Prompt".into(),
            source: ShellSource::System,
            command_strategy: ShellCommandStrategy::Cmd,
        };
        #[cfg(not(windows))]
        let terminal_shell = ResolvedShellSpec {
            executable: PathBuf::from("/bin/sh"),
            dialect: ShellDialect::Posix,
            display_name: "sh".into(),
            source: ShellSource::System,
            command_strategy: ShellCommandStrategy::Posix,
        };

        Self::new(base_env, terminal_shell, adapter_for(AgentType::Codex))
    }

    /// Set the fallback working directory used when a `terminal/create` request
    /// does not specify its own `cwd`. Chainable after [`Self::new`].
    pub fn with_default_cwd(mut self, default_cwd: Option<PathBuf>) -> Self {
        self.default_cwd = default_cwd;
        self
    }

    /// Effective working directory used for executable resolution and as the
    /// spawn fallback when the request omits `cwd`.
    fn effective_cwd(&self, request: &CreateTerminalRequest) -> PathBuf {
        request
            .cwd
            .clone()
            .or_else(|| self.default_cwd.clone().filter(|path| path.is_dir()))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    /// PATH used for classification, layered like the eventual child process:
    /// request env (last duplicate wins) -> runtime base env -> process PATH.
    fn effective_path(&self, request: &CreateTerminalRequest) -> Option<OsString> {
        request_env_value(request, "PATH")
            .map(OsString::from)
            .or_else(|| map_env_value(&self.base_env, "PATH").map(OsString::from))
            .or_else(|| std::env::var_os("PATH"))
    }

    /// Deterministically classify a request before spawning it.
    pub fn classify_request(
        &self,
        request: &CreateTerminalRequest,
    ) -> Result<ExecutionMode, TerminalRuntimeError> {
        if !request.args.is_empty() {
            return Ok(ExecutionMode::DirectProgram(PathBuf::from(
                &request.command,
            )));
        }

        let cwd = self.effective_cwd(request);
        if let Ok(path) = which::which_in(&request.command, self.effective_path(request), &cwd) {
            return Ok(ExecutionMode::DirectProgram(path));
        }

        // Unresolved names go through the snapshotted shell. PowerShell/cmd
        // resolve builtins the OS cannot exec; POSIX still wraps via `-c`.
        let family = classify_shell_family(&self.terminal_shell.executable.to_string_lossy());
        let _via_shell = family.resolves_bare_builtins() || matches!(family, ShellFamily::Posix);
        Ok(ExecutionMode::ShellCommandLine)
    }

    /// Apply stdio, working directory, and environment to a freshly built
    /// terminal command. Shared by the direct-exec and shell-fallback spawn
    /// paths in `create_terminal` so both honor the same cwd precedence and
    /// env layering.
    fn configure_command(
        &self,
        command: &mut tokio::process::Command,
        request: &CreateTerminalRequest,
    ) {
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        // Working directory. An explicit `cwd` from the agent (validated
        // absolute in `create_terminal`) is honored as-is, so a non-existent
        // directory surfaces as a loud spawn failure rather than silently
        // running somewhere else. Only when the agent omits `cwd` do we fall
        // back to the connection's session working directory — agents like
        // CodeBuddy omit it, which would otherwise inherit codeg's own process
        // cwd instead of the folder the conversation runs in. The fallback is
        // guarded on `is_dir` so a not-yet-created session dir never turns into
        // a spawn failure (mirrors the cwd guard in `build_agent`).
        if let Some(cwd) = request.cwd.as_deref() {
            command.current_dir(cwd);
        } else if let Some(default_cwd) = self.default_cwd.as_deref() {
            if default_cwd.is_dir() {
                command.current_dir(default_cwd);
            }
        }

        // Apply the runtime's base env first (e.g. `GIT_CONFIG_*` for the
        // codeg credential helper), then layer the agent's request env on top
        // so agents can still override or scrub specific keys.
        for (key, value) in &self.base_env {
            command.env(key, value);
        }
        for env_var in &request.env {
            command.env(&env_var.name, &env_var.value);
        }
    }

    pub async fn create_terminal(
        &self,
        request: CreateTerminalRequest,
    ) -> Result<CreateTerminalResponse, TerminalRuntimeError> {
        let request = self.adapter.normalize_terminal_request(request)?;

        if let Some(cwd) = request.cwd.as_ref() {
            if !cwd.is_absolute() {
                return Err(TerminalRuntimeError::InvalidParams(
                    "terminal/create requires an absolute cwd when provided".to_string(),
                ));
            }
        }

        if request.command.trim().is_empty() {
            return Err(TerminalRuntimeError::InvalidParams(
                "terminal/create requires a non-empty command".to_string(),
            ));
        }

        let output_byte_limit = request
            .output_byte_limit
            .unwrap_or(DEFAULT_OUTPUT_BYTE_LIMIT);
        if output_byte_limit == 0 {
            return Err(TerminalRuntimeError::InvalidParams(
                "terminal/create outputByteLimit must be greater than 0".to_string(),
            ));
        }

        let mode = self.classify_request(&request)?;
        let mut command = match &mode {
            ExecutionMode::DirectProgram(program) => {
                let mut command = crate::process::tokio_command(program);
                command.args(&request.args);
                command
            }
            ExecutionMode::ShellCommandLine => {
                build_command_line(&self.terminal_shell, &request.command)
            }
        };
        self.configure_command(&mut command, &request);
        command.kill_on_drop(true);

        let mut child = crate::process::spawn_retrying_exec_busy(|| command.spawn())
            .await
            .map_err(|err| {
                let (code, executable, mode_label) = match &mode {
                    ExecutionMode::DirectProgram(program) => (
                        "terminal_program_spawn_failed",
                        program.display().to_string(),
                        "direct_program",
                    ),
                    ExecutionMode::ShellCommandLine => (
                        "terminal_shell_spawn_failed",
                        self.terminal_shell.executable.display().to_string(),
                        "shell_command_line",
                    ),
                };
                TerminalRuntimeError::Spawn {
                    code,
                    executable,
                    display_name: self.terminal_shell.display_name.clone(),
                    mode: mode_label,
                    os_error: err.to_string(),
                }
            })?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let terminal_id = format!("term_{}", uuid::Uuid::new_v4().simple());
        let terminal = Arc::new(TerminalInstance::new(
            request.session_id.to_string(),
            Some(output_byte_limit),
        ));

        let mut handles: Vec<JoinHandle<()>> = Vec::new();
        if let Some(reader) = stdout {
            let terminal_ref = terminal.clone();
            handles.push(tokio::spawn(async move {
                read_stream(reader, terminal_ref).await;
            }));
        }

        if let Some(reader) = stderr {
            let terminal_ref = terminal.clone();
            handles.push(tokio::spawn(async move {
                read_stream(reader, terminal_ref).await;
            }));
        }

        if !handles.is_empty() {
            terminal.reader_handles.lock().await.extend(handles);
        }

        // Hand the child to its owner task only AFTER the reader handles are
        // registered: the owner drains them before publishing the exit status,
        // and a command that exits instantly would otherwise drain an empty
        // list and leave the readers running past the published completion.
        tokio::spawn(own_terminal_process(terminal.clone(), child));

        self.terminals
            .lock()
            .await
            .insert(terminal_id.clone(), terminal);

        Ok(CreateTerminalResponse::new(terminal_id))
    }

    pub async fn terminal_output(
        &self,
        request: TerminalOutputRequest,
    ) -> Result<TerminalOutputResponse, TerminalRuntimeError> {
        let terminal = self
            .find_terminal(
                &request.terminal_id.to_string(),
                &request.session_id.to_string(),
            )
            .await?;

        let snapshot = terminal.snapshot().await;

        Ok(
            TerminalOutputResponse::new(snapshot.output, snapshot.truncated)
                .exit_status(snapshot.exit_status),
        )
    }

    pub async fn terminal_output_delta(
        &self,
        session_id: &str,
        terminal_id: &str,
        from_offset: Option<u64>,
    ) -> Result<TerminalOutputDelta, TerminalRuntimeError> {
        let terminal = self.find_terminal(terminal_id, session_id).await?;
        let snapshot = terminal.snapshot().await;

        let output_len = u64::try_from(snapshot.output.len()).unwrap_or(u64::MAX);
        let base_offset = snapshot.output_base_offset;
        let end_offset = base_offset.saturating_add(output_len);
        let requested_offset = from_offset.unwrap_or(base_offset);
        let had_gap = from_offset
            .map(|offset| offset < base_offset)
            .unwrap_or(false);
        let start_offset = requested_offset.clamp(base_offset, end_offset);
        let start_index = usize::try_from(start_offset.saturating_sub(base_offset)).unwrap_or(0);
        let output = snapshot.output[start_index..].to_string();

        Ok(TerminalOutputDelta {
            output,
            next_offset: end_offset,
            had_gap,
            truncated: snapshot.truncated,
            exit_status: snapshot.exit_status,
        })
    }

    pub async fn wait_for_terminal_exit(
        &self,
        request: WaitForTerminalExitRequest,
    ) -> Result<WaitForTerminalExitResponse, TerminalRuntimeError> {
        let terminal = self
            .find_terminal(
                &request.terminal_id.to_string(),
                &request.session_id.to_string(),
            )
            .await?;
        let exit_status = terminal.wait_for_exit().await?;
        Ok(WaitForTerminalExitResponse::new(exit_status))
    }

    pub async fn kill_terminal(
        &self,
        request: KillTerminalRequest,
    ) -> Result<KillTerminalResponse, TerminalRuntimeError> {
        let terminal = self
            .find_terminal(
                &request.terminal_id.to_string(),
                &request.session_id.to_string(),
            )
            .await?;
        terminal.kill_command().await?;
        Ok(KillTerminalResponse::new())
    }

    pub async fn release_terminal(
        &self,
        request: ReleaseTerminalRequest,
    ) -> Result<ReleaseTerminalResponse, TerminalRuntimeError> {
        let terminal_id = request.terminal_id.to_string();
        let session_id = request.session_id.to_string();
        let terminal = {
            let mut terminals = self.terminals.lock().await;
            let Some(existing) = terminals.get(&terminal_id) else {
                return Err(TerminalRuntimeError::InvalidParams(format!(
                    "terminal {terminal_id} not found"
                )));
            };
            if existing.session_id != session_id {
                return Err(TerminalRuntimeError::InvalidParams(format!(
                    "terminal {terminal_id} does not belong to session {session_id}"
                )));
            }
            terminals.remove(&terminal_id).expect("terminal exists")
        };

        terminal.kill_command().await?;
        Ok(ReleaseTerminalResponse::new())
    }

    pub async fn release_all_for_session(&self, session_id: &str) {
        let removed = {
            let mut terminals = self.terminals.lock().await;
            let ids: Vec<String> = terminals
                .iter()
                .filter(|(_, term)| term.session_id == session_id)
                .map(|(id, _)| id.clone())
                .collect();

            let mut removed = Vec::with_capacity(ids.len());
            for id in ids {
                if let Some(term) = terminals.remove(&id) {
                    removed.push(term);
                }
            }
            removed
        };

        // Kill concurrently. Sequentially, a session holding N terminals would
        // cost up to N * KILL_REPORT_BUDGET to tear down, and this runs on the
        // cancel path where the user is waiting.
        futures::future::join_all(removed.into_iter().map(|terminal| async move {
            if let Err(err) = terminal.kill_command().await {
                tracing::error!("[ACP] Failed to release terminal during cleanup: {err:?}");
            }
        }))
        .await;
    }

    pub async fn kill_all(&self) {
        let removed: Vec<Arc<TerminalInstance>> = {
            let mut terminals = self.terminals.lock().await;
            terminals.drain().map(|(_, term)| term).collect()
        };
        futures::future::join_all(removed.into_iter().map(|terminal| async move {
            if let Err(err) = terminal.kill_command().await {
                tracing::error!("[ACP] Failed to kill terminal during shutdown: {err:?}");
            }
        }))
        .await;
    }

    async fn find_terminal(
        &self,
        terminal_id: &str,
        session_id: &str,
    ) -> Result<Arc<TerminalInstance>, TerminalRuntimeError> {
        let terminal = {
            let terminals = self.terminals.lock().await;
            terminals.get(terminal_id).cloned()
        }
        .ok_or_else(|| {
            TerminalRuntimeError::InvalidParams(format!("terminal {terminal_id} not found"))
        })?;

        if terminal.session_id != session_id {
            return Err(TerminalRuntimeError::InvalidParams(format!(
                "terminal {terminal_id} does not belong to session {session_id}"
            )));
        }

        Ok(terminal)
    }
}

fn request_env_value(request: &CreateTerminalRequest, key: &str) -> Option<String> {
    request
        .env
        .iter()
        .rev()
        .find(|entry| env_keys_equal(&entry.name, key))
        .map(|entry| entry.value.clone())
}

fn map_env_value(map: &BTreeMap<String, String>, key: &str) -> Option<String> {
    map.iter()
        .find(|(candidate, _)| env_keys_equal(candidate, key))
        .map(|(_, value)| value.clone())
}

fn env_keys_equal(a: &str, b: &str) -> bool {
    #[cfg(windows)]
    {
        a.eq_ignore_ascii_case(b)
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

async fn read_stream<R>(mut reader: R, terminal: Arc<TerminalInstance>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 4096];
    let mut pending = Vec::<u8>::new();
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => {
                if !pending.is_empty() {
                    let text = String::from_utf8_lossy(&pending).to_string();
                    terminal.append_output(&text).await;
                    pending.clear();
                }
                break;
            }
            Ok(size) => {
                pending.extend_from_slice(&buffer[..size]);
                let decoded = decode_available_utf8(&mut pending);
                if !decoded.is_empty() {
                    terminal.append_output(&decoded).await;
                }
            }
            Err(_) => break,
        }
    }
}

fn map_exit_status(status: std::process::ExitStatus) -> TerminalExitStatus {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status).map(|s| s.to_string());
    #[cfg(not(unix))]
    let signal: Option<String> = None;

    let exit_code = status.code().and_then(|code| u32::try_from(code).ok());
    TerminalExitStatus::new()
        .exit_code(exit_code)
        .signal(signal)
}

fn enforce_output_limit(output: &mut String, limit: usize) -> usize {
    if output.len() <= limit {
        return 0;
    }

    let mut start = output.len().saturating_sub(limit);
    while start < output.len() && !output.is_char_boundary(start) {
        start += 1;
    }

    output.drain(..start);
    start
}

fn decode_available_utf8(pending: &mut Vec<u8>) -> String {
    let mut output = String::new();
    let mut consumed = 0usize;
    let mut remaining = pending.as_slice();

    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(text) => {
                output.push_str(text);
                consumed = consumed.saturating_add(remaining.len());
                break;
            }
            Err(err) => {
                let valid_up_to = err.valid_up_to();
                if valid_up_to > 0 {
                    if let Ok(text) = std::str::from_utf8(&remaining[..valid_up_to]) {
                        output.push_str(text);
                    }
                    consumed = consumed.saturating_add(valid_up_to);
                    remaining = &remaining[valid_up_to..];
                }

                match err.error_len() {
                    Some(invalid_len) => {
                        output.push_str(&String::from_utf8_lossy(&remaining[..invalid_len]));
                        consumed = consumed.saturating_add(invalid_len);
                        remaining = &remaining[invalid_len..];
                    }
                    None => break, // keep partial UTF-8 sequence for next chunk
                }
            }
        }
    }

    if consumed > 0 {
        pending.drain(..consumed);
    }
    output
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::acp::terminal_adapter::adapter_for;
    use crate::models::agent::AgentType;
    use crate::terminal::shell::{
        ResolvedShellSpec, ShellCommandStrategy, ShellDialect, ShellSource,
    };
    use sacp::schema::{EnvVariable, SessionId, TerminalId, WaitForTerminalExitRequest};

    fn test_runtime(base_env: BTreeMap<String, String>) -> TerminalRuntime {
        TerminalRuntime::new(
            base_env,
            ResolvedShellSpec {
                executable: PathBuf::from("/bin/sh"),
                dialect: ShellDialect::Posix,
                display_name: "sh".into(),
                source: ShellSource::System,
                command_strategy: ShellCommandStrategy::Posix,
            },
            adapter_for(AgentType::Codex),
        )
    }

    /// Regression: when an ACP agent calls `terminal/create` (e.g. to run
    /// `git push`), the runtime's base env — populated by the connection
    /// layer with the codeg credential helper's `GIT_CONFIG_*` keys —
    /// must reach the spawned process. Per-request `env` from the agent
    /// still wins on key collision so the agent can scrub or override
    /// specific keys for individual commands.
    #[tokio::test]
    async fn base_env_propagates_and_request_env_overrides() {
        let mut base_env = BTreeMap::new();
        base_env.insert("CODEG_TEST_BASE_VAR".to_string(), "from_base".to_string());
        base_env.insert("CODEG_TEST_OVERRIDE".to_string(), "loses".to_string());
        let runtime = test_runtime(base_env);

        let session_id = SessionId::new("test-session".to_string());
        let mut request = CreateTerminalRequest::new(session_id.clone(), "/bin/sh".to_string());
        request.args = vec![
            "-c".into(),
            // Print both vars on separate lines so we can match each
            // independently regardless of shell quoting.
            "printf '%s\\n' \"$CODEG_TEST_BASE_VAR\" \"$CODEG_TEST_OVERRIDE\"".into(),
        ];
        request.env = vec![EnvVariable::new("CODEG_TEST_OVERRIDE", "request_wins")];

        let response = runtime
            .create_terminal(request)
            .await
            .expect("create terminal");
        let terminal_id = response.terminal_id.clone();

        // Wait for the child to exit so the captured output is final.
        runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
            .expect("wait for exit");

        let out = runtime
            .terminal_output(TerminalOutputRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
            .expect("get output");

        assert!(
            out.output.contains("from_base"),
            "base env did not reach the spawned process; got:\n{}",
            out.output
        );
        assert!(
            out.output.contains("request_wins"),
            "per-request env did not override base on key collision; got:\n{}",
            out.output
        );
        assert!(
            !out.output.contains("loses"),
            "base value leaked through despite the request override; got:\n{}",
            out.output
        );

        // Drop terminal handle so the runtime drops its writer ends.
        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    /// Spawn `request`, wait for it to exit, return its captured output, and
    /// release the session's terminals.
    async fn run_and_capture(
        runtime: &TerminalRuntime,
        session_id: &SessionId,
        request: CreateTerminalRequest,
    ) -> String {
        let response = runtime
            .create_terminal(request)
            .await
            .expect("create terminal");
        let terminal_id = response.terminal_id.clone();
        runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
            .expect("wait for exit");
        let out = runtime
            .terminal_output(TerminalOutputRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
            .expect("get output");
        runtime.release_all_for_session(session_id.0.as_ref()).await;
        out.output
    }

    /// A `terminal/create` that omits `cwd` defaults to the runtime's
    /// configured working directory rather than codeg's own process cwd.
    #[tokio::test]
    async fn falls_back_to_default_cwd_when_request_omits_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");
        let canonical = dir.path().canonicalize().expect("canonicalize");
        let runtime =
            test_runtime(BTreeMap::new()).with_default_cwd(Some(dir.path().to_path_buf()));

        let session_id = SessionId::new("cwd-default".to_string());
        // Bare `pwd` (no whitespace) → direct exec; current_dir still applies.
        let request = CreateTerminalRequest::new(session_id.clone(), "pwd".to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;

        assert!(
            output.contains(canonical.to_string_lossy().as_ref()),
            "terminal did not run in the default cwd; got:\n{output}"
        );
    }

    /// An explicit absolute `cwd` in the request takes precedence over the
    /// runtime default.
    #[tokio::test]
    async fn request_cwd_overrides_default_cwd() {
        let default_dir = tempfile::tempdir().expect("default dir");
        let request_dir = tempfile::tempdir().expect("request dir");
        let request_canonical = request_dir.path().canonicalize().expect("canonicalize");
        let runtime =
            test_runtime(BTreeMap::new()).with_default_cwd(Some(default_dir.path().to_path_buf()));

        let session_id = SessionId::new("cwd-override".to_string());
        let mut request = CreateTerminalRequest::new(session_id.clone(), "pwd".to_string());
        request.cwd = Some(request_dir.path().to_path_buf());
        let output = run_and_capture(&runtime, &session_id, request).await;

        assert!(
            output.contains(request_canonical.to_string_lossy().as_ref()),
            "request cwd did not take precedence over the default; got:\n{output}"
        );
    }

    /// A whitespace-bearing command with empty args runs through the shell. A
    /// direct exec would ENOENT trying to run a program literally named with
    /// spaces — this is the shape CodeBuddy sends.
    #[tokio::test]
    async fn whitespace_command_runs_through_shell() {
        let runtime = test_runtime(BTreeMap::new());

        let session_id = SessionId::new("shell-wrap".to_string());
        let request =
            CreateTerminalRequest::new(session_id.clone(), "echo hello world".to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("hello world"),
            "shell did not run the whitespace command; got:\n{output}"
        );

        // Genuine shell operators must evaluate, not be passed as literal args.
        let session_id = SessionId::new("shell-ops".to_string());
        let request = CreateTerminalRequest::new(session_id.clone(), "true && echo OK".to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("OK"),
            "shell operators did not evaluate; got:\n{output}"
        );
    }

    /// A whole shell line longer than the OS path limit still runs through the
    /// shell. grok crams `<shell> -lc "<script>"` into `command` with empty
    /// args; a multi-KB heredoc makes the direct exec fail with ENAMETOOLONG →
    /// `ErrorKind::InvalidFilename` (not `NotFound`), which the fallback guard
    /// must also catch. The marker exceeds Linux's PATH_MAX (4096) so the direct
    /// exec fails with ENAMETOOLONG on both macOS (1024) and Linux — a sub-limit
    /// line would return `NotFound` and silently exercise the other branch.
    #[tokio::test]
    async fn overlong_command_line_falls_back_to_shell() {
        let runtime = test_runtime(BTreeMap::new());

        let session_id = SessionId::new("overlong-cmd".to_string());
        let marker = "x".repeat(5000);
        let request = CreateTerminalRequest::new(session_id.clone(), format!("echo {marker}"));
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains(&marker),
            "overlong command line did not run via the shell fallback; got {} bytes",
            output.len()
        );
    }

    /// The shell-wrapped path still honors the working directory, so a
    /// CodeBuddy-style `pnpm build` runs in the session folder.
    #[tokio::test]
    async fn shell_wrapped_command_respects_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");
        let canonical = dir.path().canonicalize().expect("canonicalize");
        let runtime =
            test_runtime(BTreeMap::new()).with_default_cwd(Some(dir.path().to_path_buf()));

        let session_id = SessionId::new("shell-cwd".to_string());
        // Whitespace → shell-wrapped; must run in the default cwd.
        let request =
            CreateTerminalRequest::new(session_id.clone(), "pwd && echo done".to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains(canonical.to_string_lossy().as_ref()) && output.contains("done"),
            "shell-wrapped command ignored the default cwd; got:\n{output}"
        );
    }

    /// When the agent supplies explicit `args`, the command is exec'd directly
    /// (no shell), so an argument containing spaces stays a single argument.
    #[tokio::test]
    async fn explicit_args_bypass_shell_wrap() {
        let runtime = test_runtime(BTreeMap::new());

        let session_id = SessionId::new("direct-exec".to_string());
        let mut request = CreateTerminalRequest::new(session_id.clone(), "/bin/echo".to_string());
        request.args = vec!["hello world".into()];
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("hello world"),
            "direct exec did not pass the single arg through; got:\n{output}"
        );
    }

    /// An explicit but non-existent absolute `cwd` is honored as-is and
    /// surfaces as a spawn failure — never silently downgraded to the default
    /// fallback or the inherited process cwd.
    #[tokio::test]
    async fn explicit_missing_cwd_surfaces_as_spawn_failure() {
        let default_dir = tempfile::tempdir().expect("default dir");
        let runtime =
            test_runtime(BTreeMap::new()).with_default_cwd(Some(default_dir.path().to_path_buf()));

        let session_id = SessionId::new("missing-cwd".to_string());
        let mut request = CreateTerminalRequest::new(session_id, "pwd".to_string());
        request.cwd = Some(PathBuf::from("/codeg-nonexistent-cwd/does/not/exist"));

        let result = runtime.create_terminal(request).await;
        // Missing explicit cwd fails at OS spawn and is reported as
        // `TerminalRuntimeError::Spawn` (not Internal / not silent fallback).
        assert!(
            matches!(result, Err(TerminalRuntimeError::Spawn { .. })),
            "expected a spawn failure for a missing explicit cwd, got {result:?}"
        );
    }

    /// A real executable whose path contains spaces is exec'd directly: the
    /// direct spawn succeeds, so the shell fallback never fires (shell-wrapping
    /// would split the path at the space).
    ///
    /// Note: writing an executable and exec'ing it from a multi-threaded test
    /// binary can hit a spurious `ETXTBSY` when a sibling test forks in the
    /// window where the write descriptor is still open. The spawn's
    /// `spawn_retrying_exec_busy` wrapper is what keeps this deterministic —
    /// see `spawn_rides_out_a_transient_text_file_busy`.
    #[tokio::test]
    async fn executable_path_with_spaces_is_not_shell_wrapped() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("my tool"); // space in the file name
        std::fs::write(&exe, "#!/bin/sh\necho ran-directly\n").expect("write script");
        let mut perms = std::fs::metadata(&exe).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&exe, perms).expect("chmod");

        let runtime = test_runtime(BTreeMap::new());
        let session_id = SessionId::new("space-exe".to_string());
        // Empty args + whitespace in command, but it resolves to a real
        // executable, so it must run directly rather than via the shell.
        let request =
            CreateTerminalRequest::new(session_id.clone(), exe.to_string_lossy().to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("ran-directly"),
            "space-containing executable was not exec'd directly; got:\n{output}"
        );
    }

    /// A whitespace-only command is rejected up front rather than spawning a
    /// shell no-op.
    #[tokio::test]
    async fn whitespace_only_command_is_rejected() {
        let runtime = test_runtime(BTreeMap::new());
        let session_id = SessionId::new("blank-command".to_string());
        let request = CreateTerminalRequest::new(session_id, "   ".to_string());

        let result = runtime.create_terminal(request).await;
        assert!(
            matches!(result, Err(TerminalRuntimeError::InvalidParams(_))),
            "expected InvalidParams for a whitespace-only command"
        );
    }

    /// A relative, space-containing executable resolves against the terminal's
    /// effective cwd and runs directly — the shell fallback fires only on a
    /// genuine NotFound. This guards the regression where a pre-spawn `which`
    /// check, run in codeg's own cwd, would shell-wrap and split the path.
    #[tokio::test]
    async fn relative_executable_with_spaces_runs_in_effective_cwd() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("my tool"); // space in the file name
        std::fs::write(&exe, "#!/bin/sh\necho ran-relative\n").expect("write script");
        let mut perms = std::fs::metadata(&exe).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&exe, perms).expect("chmod");

        let runtime =
            test_runtime(BTreeMap::new()).with_default_cwd(Some(dir.path().to_path_buf()));
        let session_id = SessionId::new("rel-space-exe".to_string());
        // "./my tool": empty args + whitespace, resolvable only against the
        // terminal's cwd — must run directly, not via the shell.
        let request = CreateTerminalRequest::new(session_id.clone(), "./my tool".to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("ran-relative"),
            "relative space-containing exe was not run in the effective cwd; got:\n{output}"
        );
    }

    /// Spawn `command` and return `(runtime, session_id, terminal_id)` without
    /// waiting for it to finish. For the long-running cases below, where the
    /// point is what happens *while* the command is still alive.
    async fn start(command: &str, session: &str) -> (Arc<TerminalRuntime>, SessionId, TerminalId) {
        let runtime = Arc::new(test_runtime(BTreeMap::new()));
        let session_id = SessionId::new(session.to_string());
        let request = CreateTerminalRequest::new(session_id.clone(), command.to_string());
        let response = runtime
            .create_terminal(request)
            .await
            .expect("create terminal");
        let terminal_id = response.terminal_id.clone();
        (runtime, session_id, terminal_id)
    }

    /// True while `pid` names a live process.
    fn pid_is_alive(pid: u32) -> bool {
        // Signal 0 performs the permission/existence check without delivering.
        unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
    }

    /// Current parent pid of `pid`, or `None` if it is gone / unreadable.
    fn parent_pid_of(pid: u32) -> Option<u32> {
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    }

    /// Poll until `pid` is gone, or the deadline passes.
    async fn wait_until_pid_gone(pid: u32, budget: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + budget;
        while tokio::time::Instant::now() < deadline {
            if !pid_is_alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        !pid_is_alive(pid)
    }

    /// Regression: a waiter that subscribes AFTER a fast command already exited
    /// must still get its exit status.
    ///
    /// This is the `watch::Sender::send` trap — `send` DISCARDS the value when
    /// no receiver exists yet, so publishing completion with it would strand
    /// every later waiter forever. Observing the exit through `terminal/output`
    /// first is what guarantees the publication already happened; only then is
    /// `wait_for_terminal_exit` called.
    #[tokio::test]
    async fn wait_for_exit_resolves_when_subscribing_after_a_fast_exit() {
        let (runtime, session_id, terminal_id) = start("true", "fast-exit").await;

        // Wait for the owner task to publish, observed via the output path.
        let mut published = false;
        for _ in 0..200 {
            let out = runtime
                .terminal_output(TerminalOutputRequest::new(
                    session_id.clone(),
                    terminal_id.clone(),
                ))
                .await
                .expect("get output");
            if out.exit_status.is_some() {
                published = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(published, "owner task never published an exit status");

        // Subscribing only now must not hang.
        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            )),
        )
        .await
        .expect("wait_for_exit hung after a fast exit — completion was lost")
        .expect("wait for exit");
        assert_eq!(exit.exit_status.exit_code, Some(0));

        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    /// The bug this module was rewritten for: an outstanding `wait_for_exit` on
    /// a command that never exits must not block `terminal/output` or
    /// `terminal/kill`. Previously `wait_for_exit` held the child mutex for the
    /// whole wait, so both of those (and the session cancel path) deadlocked.
    #[tokio::test]
    async fn output_and_kill_work_while_wait_for_exit_is_outstanding() {
        let (runtime, session_id, terminal_id) = start("sleep 30", "wait-outstanding").await;

        let waiter = tokio::spawn({
            let runtime = runtime.clone();
            let session_id = session_id.clone();
            let terminal_id = terminal_id.clone();
            async move {
                runtime
                    .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                        session_id,
                        terminal_id,
                    ))
                    .await
            }
        });
        // Let the waiter actually park inside wait_for_exit.
        tokio::time::sleep(Duration::from_millis(100)).await;

        tokio::time::timeout(
            Duration::from_secs(5),
            runtime.terminal_output(TerminalOutputRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            )),
        )
        .await
        .expect("terminal_output blocked behind an outstanding wait_for_exit")
        .expect("get output");

        tokio::time::timeout(
            Duration::from_secs(10),
            runtime.kill_terminal(KillTerminalRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            )),
        )
        .await
        .expect("kill_terminal blocked behind an outstanding wait_for_exit")
        .expect("kill terminal");

        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("the outstanding wait never resolved after the kill")
            .expect("waiter task")
            .expect("wait for exit");

        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    /// Every concurrent waiter is released, not just the first one.
    #[tokio::test]
    async fn multiple_waiters_all_resolve() {
        let (runtime, session_id, terminal_id) = start("sleep 30", "many-waiters").await;

        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let runtime = runtime.clone();
                let session_id = session_id.clone();
                let terminal_id = terminal_id.clone();
                tokio::spawn(async move {
                    runtime
                        .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                            session_id,
                            terminal_id,
                        ))
                        .await
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(100)).await;

        runtime
            .kill_terminal(KillTerminalRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
            .expect("kill terminal");

        for waiter in waiters {
            tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .expect("a concurrent waiter was never released")
                .expect("waiter task")
                .expect("wait for exit");
        }

        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    /// `kill_tree` only sends `SIGTERM`, which a process that traps it ignores
    /// forever. The owner task must escalate to `SIGKILL`.
    #[tokio::test]
    async fn kill_escalates_to_sigkill_for_a_sigterm_ignoring_process() {
        let (runtime, session_id, terminal_id) = start(
            "sh -c 'trap \"\" TERM; while :; do sleep 0.1; done'",
            "sigterm-immune",
        )
        .await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        tokio::time::timeout(
            Duration::from_secs(15),
            runtime.kill_terminal(KillTerminalRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            )),
        )
        .await
        .expect("kill_terminal never returned for a SIGTERM-immune process")
        .expect("kill terminal");

        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            runtime.wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            )),
        )
        .await
        .expect("the SIGTERM-immune process was never reaped")
        .expect("wait for exit");
        assert!(
            exit.exit_status.signal.is_some() || exit.exit_status.exit_code.is_some(),
            "expected a terminal status after escalation, got {:?}",
            exit.exit_status
        );

        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    /// A descendant that outlives a SIGTERM-obedient parent must still be
    /// killed.
    ///
    /// Deliberately THREE levels deep: root traps TERM, the middle level dies
    /// on TERM, the leaf traps TERM. When the middle level dies the leaf is
    /// reparented, so no fresh snapshot rooted at the direct child can find it
    /// again — only the cumulative record of everything the SIGTERM pass
    /// signalled still reaches it. A two-level version of this test passes even
    /// with a non-cumulative record, so it would not catch the regression.
    #[tokio::test]
    async fn kill_sweeps_a_descendant_that_outlives_a_sigterm_obedient_parent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let pid_file = dir.path().join("leaf.pid");
        let leaf = dir.path().join("leaf.sh");
        let middle = dir.path().join("middle.sh");
        let root = dir.path().join("root.sh");

        // Three files rather than one nested one-liner: the quoting of nested
        // `sh -c "sh -c '...'"` is unreadable and easy to get subtly wrong.
        //
        // Note the ORDER in root.sh. `trap '' TERM` sets SIG_IGN, and SIG_IGN is
        // inherited across both fork AND exec — and a shell that starts with a
        // signal already ignored cannot restore its default (`trap - TERM` is a
        // no-op there). Installing the trap before spawning would therefore make
        // the ENTIRE subtree TERM-immune, the middle level would never die, the
        // leaf would never be reparented, and this test would pass even with a
        // non-cumulative record. Spawn first, then go immune.
        std::fs::write(
            &leaf,
            format!(
                "trap '' TERM\necho $$ > {}\nwhile :; do sleep 0.2; done\n",
                pid_file.display()
            ),
        )
        .expect("write leaf.sh");
        std::fs::write(
            &middle,
            format!("sh {} &\nwhile :; do sleep 0.2; done\n", leaf.display()),
        )
        .expect("write middle.sh");
        std::fs::write(
            &root,
            format!(
                "sh {} &\ntrap '' TERM\nwhile :; do sleep 0.2; done\n",
                middle.display()
            ),
        )
        .expect("write root.sh");

        let (runtime, session_id, terminal_id) =
            start(&format!("sh {}", root.display()), "deep-tree").await;

        // Wait for the leaf to announce itself.
        let mut leaf_pid = None;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    if pid > 0 {
                        leaf_pid = Some(pid);
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let leaf_pid = leaf_pid.expect("leaf process never recorded its pid");
        assert!(pid_is_alive(leaf_pid), "leaf should be running");

        tokio::time::timeout(
            Duration::from_secs(20),
            runtime.kill_terminal(KillTerminalRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            )),
        )
        .await
        .expect("kill_terminal never returned")
        .expect("kill terminal");

        assert!(
            wait_until_pid_gone(leaf_pid, Duration::from_secs(10)).await,
            "reparented leaf {leaf_pid} survived the kill"
        );

        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    /// Sanity check for the test above: the middle level must actually die on
    /// `SIGTERM` while the leaf survives and is reparented. If this stops
    /// holding, `kill_sweeps_a_descendant_that_outlives_a_sigterm_obedient_parent`
    /// silently stops testing the reparenting path it exists for.
    #[tokio::test]
    async fn deep_tree_fixture_reparents_the_leaf_on_sigterm() {
        let dir = tempfile::tempdir().expect("temp dir");
        let pid_file = dir.path().join("leaf.pid");
        let leaf = dir.path().join("leaf.sh");
        let middle = dir.path().join("middle.sh");
        let root = dir.path().join("root.sh");
        std::fs::write(
            &leaf,
            format!(
                "trap '' TERM\necho $$ > {}\nwhile :; do sleep 0.2; done\n",
                pid_file.display()
            ),
        )
        .expect("write leaf.sh");
        std::fs::write(
            &middle,
            format!("sh {} &\nwhile :; do sleep 0.2; done\n", leaf.display()),
        )
        .expect("write middle.sh");
        std::fs::write(
            &root,
            format!(
                "sh {} &\ntrap '' TERM\nwhile :; do sleep 0.2; done\n",
                middle.display()
            ),
        )
        .expect("write root.sh");

        let (runtime, session_id, terminal_id) =
            start(&format!("sh {}", root.display()), "deep-tree-fixture").await;

        let mut leaf_pid = None;
        for _ in 0..200 {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    if pid > 0 {
                        leaf_pid = Some(pid);
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let leaf_pid = leaf_pid.expect("leaf process never recorded its pid");
        let middle_pid = parent_pid_of(leaf_pid).expect("leaf should have a parent");

        // The graceful pass only: no escalation, no sweep.
        signal_tree(Some(middle_pid), "SIGTERM").await;
        let middle_died = wait_until_pid_gone(middle_pid, Duration::from_secs(5)).await;
        let leaf_survived = pid_is_alive(leaf_pid);
        let leaf_parent_now = parent_pid_of(leaf_pid);

        // Clean up BEFORE asserting: a failing assertion would otherwise unwind
        // past the cleanup and leave a TERM-immune spinner running forever on
        // the machine (or the CI worker) that ran the test.
        unsafe { libc::kill(leaf_pid as libc::pid_t, libc::SIGKILL) };
        runtime.release_all_for_session(session_id.0.as_ref()).await;
        let _ = terminal_id;

        assert!(
            middle_died,
            "middle level ignored SIGTERM — the fixture no longer exercises reparenting"
        );
        assert!(
            leaf_survived,
            "leaf died on SIGTERM — the fixture no longer exercises reparenting"
        );
        // Deliberately NOT `== Some(1)`: under a child subreaper (containers,
        // some init systems) an orphan is reparented to the subreaper rather
        // than to pid 1. All this test needs is that the leaf moved off its
        // dead parent, which is what makes it unreachable from the root.
        assert!(
            leaf_parent_now.is_some() && leaf_parent_now != Some(middle_pid),
            "leaf was not reparented after its parent died (parent is still {leaf_parent_now:?})"
        );
    }

    /// Session teardown kills terminals concurrently. Sequentially, N stubborn
    /// terminals would each burn their own escalation grace and the cancel path
    /// (which awaits this) would stall for N times as long.
    ///
    /// The commands deliberately ignore SIGTERM: with TERM-responsive ones each
    /// kill returns in milliseconds, so a sequential implementation would finish
    /// just as fast and the test would prove nothing. Ignoring TERM forces every
    /// terminal to cost a full `KILL_ESCALATE_GRACE`, making the difference
    /// between 4 * grace (sequential) and ~1 * grace (concurrent) unmistakable.
    #[tokio::test]
    async fn release_all_for_session_kills_many_terminals_concurrently() {
        const TERMINALS: u32 = 4;
        let runtime = Arc::new(test_runtime(BTreeMap::new()));
        let session_id = SessionId::new("bulk-release".to_string());
        for _ in 0..TERMINALS {
            let request = CreateTerminalRequest::new(
                session_id.clone(),
                "sh -c 'trap \"\" TERM; while :; do sleep 0.1; done'".to_string(),
            );
            runtime
                .create_terminal(request)
                .await
                .expect("create terminal");
        }
        // Let every shell install its trap before we start killing.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            Duration::from_secs(30),
            runtime.release_all_for_session(session_id.0.as_ref()),
        )
        .await
        .expect("release_all_for_session never returned");

        let elapsed = started.elapsed();
        assert!(
            elapsed < KILL_ESCALATE_GRACE * 2,
            "teardown of {TERMINALS} SIGTERM-immune terminals took {elapsed:?}; \
             concurrent kills should cost roughly one {KILL_ESCALATE_GRACE:?} grace, \
             so this suggests they ran sequentially"
        );
    }

    /// Output is readable while the command is still running — the snapshot is
    /// not gated on the process finishing.
    #[tokio::test]
    async fn output_is_readable_before_the_command_exits() {
        let (runtime, session_id, terminal_id) =
            start("sh -c 'echo early-marker; sleep 30'", "live-output").await;

        let mut saw_marker = false;
        for _ in 0..200 {
            let out = runtime
                .terminal_output(TerminalOutputRequest::new(
                    session_id.clone(),
                    terminal_id.clone(),
                ))
                .await
                .expect("get output");
            if out.output.contains("early-marker") {
                assert!(
                    out.exit_status.is_none(),
                    "command should still be running while we read its output"
                );
                saw_marker = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(saw_marker, "never saw output from a still-running command");

        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    /// Once `wait_for_exit` returns, ALL output is already in the snapshot.
    /// Guards the drain-before-publish ordering in the owner task: publishing
    /// the exit status first would let a caller treat the terminal as done and
    /// miss the tail.
    #[tokio::test]
    async fn all_output_is_present_after_wait_returns() {
        let runtime = Arc::new(test_runtime(BTreeMap::new()));
        let session_id = SessionId::new("drain-order".to_string());
        let request = CreateTerminalRequest::new(
            session_id.clone(),
            "sh -c 'i=0; while [ $i -lt 400 ]; do echo line-$i; i=$((i+1)); done'".to_string(),
        );
        let response = runtime
            .create_terminal(request)
            .await
            .expect("create terminal");
        let terminal_id = response.terminal_id.clone();

        runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
            .expect("wait for exit");

        let out = runtime
            .terminal_output(TerminalOutputRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
            .expect("get output");
        assert!(
            out.output.contains("line-399"),
            "tail output was missing after wait_for_exit returned"
        );

        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    /// A brand-new executable that something still holds open for writing is
    /// retried, not rejected. exec returns `ETXTBSY` while any write descriptor
    /// to the file is open anywhere, and codeg forks constantly (git, agents,
    /// other terminals), so a descriptor inherited across a `fork()` can make an
    /// agent's just-written script briefly unrunnable — including in this test
    /// binary, which is where the flake was first observed.
    ///
    /// Linux enforces `ETXTBSY` here, so this exercises the retry loop; macOS
    /// does not enforce it for script exec, where the first attempt simply
    /// succeeds and the assertion still holds.
    #[tokio::test]
    async fn spawn_rides_out_a_transient_text_file_busy() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("busy-tool");
        std::fs::write(&exe, "#!/bin/sh\necho ran-after-busy\n").expect("write script");
        let mut perms = std::fs::metadata(&exe).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&exe, perms).expect("chmod");

        // Hold a write descriptor open, then release it while the spawn is still
        // retrying — the shape of the real race, with the timing made explicit.
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&exe)
            .expect("hold write handle");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(writer);
        });

        let runtime = test_runtime(BTreeMap::new());
        let session_id = SessionId::new("busy-exe".to_string());
        // No whitespace in the command, so there is no shell fallback to mask a
        // failed direct exec: if the retry does not ride out ETXTBSY,
        // `create_terminal` errors and this test panics.
        let request =
            CreateTerminalRequest::new(session_id.clone(), exe.to_string_lossy().to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("ran-after-busy"),
            "transient ETXTBSY was not retried; got:\n{output}"
        );
    }
}

#[cfg(test)]
mod fork_contract_tests {
    use super::*;
    use crate::acp::terminal_adapter::adapter_for;
    use crate::models::agent::AgentType;
    use crate::terminal::shell::{
        ResolvedShellSpec, ShellCommandStrategy, ShellDialect, ShellSource,
    };
    use sacp::schema::{EnvVariable, SessionId, WaitForTerminalExitRequest};
    use std::path::Path;

    #[cfg(windows)]
    fn platform_test_shell() -> ResolvedShellSpec {
        let (executable, display_name) = [
            ("pwsh.exe", "PowerShell 7"),
            ("powershell.exe", "Windows PowerShell"),
        ]
        .into_iter()
        .find_map(|(candidate, display_name)| {
            which::which(candidate)
                .ok()
                .map(|path| (path, display_name.to_string()))
        })
        .expect("PowerShell is required for Windows ACP terminal runtime tests");

        ResolvedShellSpec {
            executable,
            dialect: ShellDialect::PowerShell,
            display_name,
            source: ShellSource::System,
            command_strategy: ShellCommandStrategy::PowerShell,
        }
    }

    #[cfg(unix)]
    fn platform_test_shell() -> ResolvedShellSpec {
        ResolvedShellSpec {
            executable: PathBuf::from("/bin/sh"),
            dialect: ShellDialect::Posix,
            display_name: "sh".into(),
            source: ShellSource::System,
            command_strategy: ShellCommandStrategy::Posix,
        }
    }

    #[cfg(windows)]
    fn windows_cmd_test_shell() -> ResolvedShellSpec {
        ResolvedShellSpec {
            executable: which::which("cmd.exe").expect("cmd.exe is required on Windows"),
            dialect: ShellDialect::Cmd,
            display_name: "Command Prompt".into(),
            source: ShellSource::System,
            command_strategy: ShellCommandStrategy::Cmd,
        }
    }

    #[cfg(windows)]
    fn test_executable_name(stem: &str) -> String {
        format!("{stem}.cmd")
    }

    #[cfg(unix)]
    fn test_executable_name(stem: &str) -> String {
        stem.to_string()
    }

    #[cfg(windows)]
    fn create_test_executable(path: PathBuf) -> PathBuf {
        std::fs::write(&path, "@echo off\r\nexit /b 0\r\n").unwrap();
        path
    }

    #[cfg(unix)]
    fn create_test_executable(path: PathBuf) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(windows)]
    fn platform_print_working_directory_line() -> String {
        "Get-Location".into()
    }

    #[cfg(unix)]
    fn platform_print_working_directory_line() -> String {
        "pwd; true".into()
    }

    #[cfg(windows)]
    fn platform_marker_then_exit_line(marker: &Path, code: i32) -> String {
        let marker = marker.to_string_lossy().replace('\'', "''");
        format!("Add-Content -LiteralPath '{marker}' -NoNewline -Value x; exit {code}")
    }

    #[cfg(unix)]
    fn platform_marker_then_exit_line(marker: &Path, code: i32) -> String {
        let marker = marker.to_string_lossy().replace('\'', "'\"'\"'");
        format!("printf x >> '{marker}'; exit {code}")
    }

    #[cfg(windows)]
    fn platform_long_running_command() -> &'static str {
        "Start-Sleep -Seconds 30"
    }

    #[cfg(unix)]
    fn platform_long_running_command() -> &'static str {
        "sleep 30"
    }

    fn test_runtime(agent_type: AgentType) -> TerminalRuntime {
        TerminalRuntime::new(
            BTreeMap::new(),
            platform_test_shell(),
            adapter_for(agent_type),
        )
    }

    #[allow(dead_code)] // helper kept for terminal capture fixtures
    async fn run_and_capture(
        runtime: &TerminalRuntime,
        session_id: &SessionId,
        request: CreateTerminalRequest,
    ) -> String {
        let response = runtime
            .create_terminal(request)
            .await
            .expect("create terminal");
        let terminal_id = response.terminal_id.clone();
        runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                terminal_id.clone(),
            ))
            .await
            .expect("wait for exit");
        let output = runtime
            .terminal_output(TerminalOutputRequest::new(session_id.clone(), terminal_id))
            .await
            .expect("get output");
        runtime.release_all_for_session(session_id.0.as_ref()).await;
        output.output
    }

    async fn start_long_running(
        session: &str,
    ) -> (Arc<TerminalRuntime>, SessionId, sacp::schema::TerminalId) {
        let runtime = Arc::new(test_runtime(AgentType::Grok));
        let session_id = SessionId::new(session);
        let response = runtime
            .create_terminal(CreateTerminalRequest::new(
                session_id.clone(),
                platform_long_running_command(),
            ))
            .await
            .expect("create long-running terminal");
        (runtime, session_id, response.terminal_id)
    }

    #[tokio::test]
    async fn legacy_with_base_env_constructor_preserves_shell_execution() {
        let runtime = TerminalRuntime::with_base_env(BTreeMap::new());
        let session_id = SessionId::new("legacy-constructor");
        let response = runtime
            .create_terminal(CreateTerminalRequest::new(
                session_id.clone(),
                "echo compatibility",
            ))
            .await
            .expect("legacy constructor creates terminal");
        runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                response.terminal_id.clone(),
            ))
            .await
            .expect("legacy terminal exits");
        let output = runtime
            .terminal_output(TerminalOutputRequest::new(
                session_id.clone(),
                response.terminal_id,
            ))
            .await
            .expect("legacy terminal output");
        assert!(output.output.contains("compatibility"));
        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    #[test]
    fn grok_codebuddy_and_claude_share_deterministic_shell_classification() {
        for agent_type in [AgentType::Grok, AgentType::CodeBuddy, AgentType::ClaudeCode] {
            let runtime = test_runtime(agent_type);
            let shell_line = CreateTerminalRequest::new(SessionId::new("s"), "echo terminal");
            assert_eq!(
                runtime.classify_request(&shell_line).unwrap(),
                ExecutionMode::ShellCommandLine
            );

            let mut direct = CreateTerminalRequest::new(SessionId::new("s"), "cargo");
            direct.args = vec!["--version".into()];
            assert!(matches!(
                runtime.classify_request(&direct).unwrap(),
                ExecutionMode::DirectProgram(_)
            ));
        }
    }

    #[test]
    fn request_path_env_overrides_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let bin_dir = temp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let executable = create_test_executable(bin_dir.join(test_executable_name("special-tool")));

        let runtime = test_runtime(AgentType::CodeBuddy);
        let mut request =
            CreateTerminalRequest::new(SessionId::new("path-override"), "special-tool");
        request.env = vec![EnvVariable::new(
            "PATH",
            bin_dir.to_string_lossy().into_owned(),
        )];

        match runtime.classify_request(&request).unwrap() {
            ExecutionMode::DirectProgram(path) => assert_eq!(
                path.file_name(),
                executable.file_name(),
                "resolved path {path:?} should match test executable"
            ),
            ExecutionMode::ShellCommandLine => {
                panic!("expected DirectProgram when PATH points at the test binary")
            }
        }
    }

    #[tokio::test]
    async fn complete_line_runs_through_selected_shell() {
        let runtime = test_runtime(AgentType::Grok);
        let session_id = SessionId::new("selected-shell");
        let request =
            CreateTerminalRequest::new(session_id.clone(), platform_print_working_directory_line());
        assert_eq!(
            runtime.classify_request(&request).unwrap(),
            ExecutionMode::ShellCommandLine
        );
        let response = runtime.create_terminal(request).await.unwrap();
        let result = runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                response.terminal_id,
            ))
            .await
            .unwrap();
        assert_eq!(result.exit_status.exit_code, Some(0));
        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    #[tokio::test]
    async fn nonzero_marker_command_executes_once_without_retry() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("marker.txt");
        let runtime =
            test_runtime(AgentType::ClaudeCode).with_default_cwd(Some(temp.path().to_path_buf()));
        let session_id = SessionId::new("nonzero-executes-once");
        let request = CreateTerminalRequest::new(
            session_id.clone(),
            platform_marker_then_exit_line(&marker, 7),
        );
        let response = runtime.create_terminal(request).await.unwrap();
        let result = runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                response.terminal_id,
            ))
            .await
            .unwrap();
        assert_eq!(result.exit_status.exit_code, Some(7));
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_absolute_complete_line_is_not_spawned_as_a_program() {
        let temp = tempfile::tempdir().unwrap();
        let quoted = temp.path().to_string_lossy().replace('\'', "''");
        let line = format!("Set-Location -LiteralPath '{quoted}'; Get-Location");
        let runtime = test_runtime(AgentType::CodeBuddy);
        let session_id = SessionId::new("powershell-absolute-line");
        let response = runtime
            .create_terminal(CreateTerminalRequest::new(session_id.clone(), line))
            .await
            .unwrap();
        let result = runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                response.terminal_id,
            ))
            .await
            .unwrap();
        assert_eq!(result.exit_status.exit_code, Some(0));
        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_drive_switch_and_where_run_through_cmd() {
        let temp = tempfile::tempdir().unwrap();
        let line = format!("cd /d {} && where cmd.exe", temp.path().to_string_lossy());
        let runtime = TerminalRuntime::new(
            BTreeMap::new(),
            windows_cmd_test_shell(),
            adapter_for(AgentType::Grok),
        );
        let session_id = SessionId::new("cmd-builtins");
        let request = CreateTerminalRequest::new(session_id.clone(), line);
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(output.to_ascii_lowercase().contains("cmd.exe"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_quotes_pipeline_and_redirect_execute_as_one_line() {
        let temp = tempfile::tempdir().unwrap();
        let output_path = temp.path().join("quoted output.txt");
        let quoted_path = output_path.to_string_lossy().replace('\'', "''");
        let line = format!(
            "'quoted value' | ForEach-Object {{ $_ }} > '{quoted_path}'; Get-Content -LiteralPath '{quoted_path}'"
        );
        let runtime = test_runtime(AgentType::ClaudeCode);
        let session_id = SessionId::new("powershell-operators");
        let request = CreateTerminalRequest::new(session_id.clone(), line);
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("quoted value"),
            "unexpected output: {output}"
        );
        assert!(output_path.is_file());
    }

    #[tokio::test]
    async fn spawn_diagnostics_are_stable_and_secret_free() {
        let mut shell_runtime = test_runtime(AgentType::Grok);
        shell_runtime.terminal_shell.executable = PathBuf::from("missing-shell-for-test");
        shell_runtime
            .base_env
            .insert("OPENAI_API_KEY".into(), "test-secret-value".into());
        let shell_error = shell_runtime
            .create_terminal(CreateTerminalRequest::new(
                SessionId::new("shell-error"),
                "echo test-command-secret",
            ))
            .await
            .unwrap_err();
        let shell_rpc = serde_json::to_value(shell_error.into_rpc_error()).unwrap();
        assert_eq!(shell_rpc["data"]["code"], "terminal_shell_spawn_failed");
        assert_eq!(shell_rpc["data"]["mode"], "shell_command_line");
        assert!(!shell_rpc.to_string().contains("test-secret-value"));
        assert!(!shell_rpc.to_string().contains("test-command-secret"));

        let program_runtime = test_runtime(AgentType::ClaudeCode);
        let mut direct =
            CreateTerminalRequest::new(SessionId::new("program-error"), "missing-program-for-test");
        direct.args = vec!["--version".into()];
        let program_error = program_runtime.create_terminal(direct).await.unwrap_err();
        let program_rpc = serde_json::to_value(program_error.into_rpc_error()).unwrap();
        assert_eq!(program_rpc["data"]["code"], "terminal_program_spawn_failed");
        assert_eq!(program_rpc["data"]["mode"], "direct_program");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn direct_program_uses_windows_program_normalization() {
        let runtime = test_runtime(AgentType::CodeBuddy);
        let session_id = SessionId::new("windows-normalized-program");
        let mut request = CreateTerminalRequest::new(session_id.clone(), "cmd");
        request.args = vec![
            "/D".into(),
            "/S".into(),
            "/C".into(),
            "echo normalized".into(),
        ];
        let response = runtime
            .create_terminal(request)
            .await
            .expect("spawn cmd.exe");
        runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id.clone(),
                response.terminal_id.clone(),
            ))
            .await
            .expect("wait for cmd.exe");
        let output = runtime
            .terminal_output(TerminalOutputRequest::new(
                session_id.clone(),
                response.terminal_id,
            ))
            .await
            .expect("read cmd.exe output");
        assert!(output.output.contains("normalized"));
        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    #[cfg(windows)]
    fn windows_process_is_alive(pid: u32) -> std::io::Result<bool> {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "tasklist failed for pid {pid}: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_release_kills_terminal_process_tree() {
        let runtime = test_runtime(AgentType::Grok);
        let session_id = SessionId::new("windows-process-tree");
        let command = "$exe = (Get-Process -Id $PID).Path; \
            $child = Start-Process -FilePath $exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -PassThru; \
            Write-Output \"parent-pid=$PID child-pid=$($child.Id)\"; Start-Sleep -Seconds 30";
        let response = runtime
            .create_terminal(CreateTerminalRequest::new(session_id.clone(), command))
            .await
            .expect("create process tree");

        let mut pids = None;
        for _ in 0..200 {
            let output = runtime
                .terminal_output(TerminalOutputRequest::new(
                    session_id.clone(),
                    response.terminal_id.clone(),
                ))
                .await
                .expect("read process tree pids");
            let line = output
                .output
                .lines()
                .find(|line| line.contains("parent-pid=") && line.contains("child-pid="));
            if let Some(line) = line {
                let mut fields = line.split_whitespace();
                let parent = fields
                    .next()
                    .and_then(|field| field.strip_prefix("parent-pid="))
                    .and_then(|pid| pid.parse::<u32>().ok());
                let child = fields
                    .next()
                    .and_then(|field| field.strip_prefix("child-pid="))
                    .and_then(|pid| pid.parse::<u32>().ok());
                if let (Some(parent), Some(child)) = (parent, child) {
                    pids = Some((parent, child));
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        runtime.release_all_for_session(session_id.0.as_ref()).await;
        let (parent_pid, child_pid) = pids.expect("terminal reported parent and child pids");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let parent_alive =
                windows_process_is_alive(parent_pid).expect("probe terminal parent pid");
            let child_alive =
                windows_process_is_alive(child_pid).expect("probe terminal child pid");
            if !parent_alive && !child_alive {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "terminal tree survived release: parent={parent_alive}, child={child_alive}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn concurrent_wait_and_session_release_completes_promptly() {
        let (runtime, session_id, terminal_id) = start_long_running("wait-release-race").await;
        let waiter = tokio::spawn({
            let runtime = runtime.clone();
            let session_id = session_id.clone();
            let terminal_id = terminal_id.clone();
            async move {
                runtime
                    .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                        session_id,
                        terminal_id,
                    ))
                    .await
            }
        });
        tokio::task::yield_now().await;

        tokio::time::timeout(
            Duration::from_secs(10),
            runtime.release_all_for_session(session_id.0.as_ref()),
        )
        .await
        .expect("session release blocked behind wait_for_exit");
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("waiter did not resolve after release")
            .expect("waiter task")
            .expect("wait for exit");
    }

    #[tokio::test]
    async fn terminal_output_delta_does_not_block_behind_wait_for_exit() {
        let (runtime, session_id, terminal_id) = start_long_running("wait-output-poll-race").await;
        let waiter = tokio::spawn({
            let runtime = runtime.clone();
            let session_id = session_id.clone();
            let terminal_id = terminal_id.clone();
            async move {
                runtime
                    .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                        session_id,
                        terminal_id,
                    ))
                    .await
            }
        });
        tokio::task::yield_now().await;

        let delta = tokio::time::timeout(
            Duration::from_millis(500),
            runtime.terminal_output_delta(session_id.0.as_ref(), terminal_id.0.as_ref(), None),
        )
        .await
        .expect("terminal output delta blocked behind wait_for_exit")
        .expect("terminal output delta");
        assert!(delta.exit_status.is_none());

        runtime.release_all_for_session(session_id.0.as_ref()).await;
        tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("waiter did not resolve after release")
            .expect("waiter task")
            .expect("wait for exit");
    }

    #[tokio::test]
    async fn release_timeout_does_not_cancel_exit_publication() {
        let (runtime, session_id, terminal_id) =
            start_long_running("release-timeout-publication").await;
        let terminal = runtime
            .find_terminal(terminal_id.0.as_ref(), session_id.0.as_ref())
            .await
            .expect("terminal exists");

        // Once the child exits, the owner must acquire this lock to drain
        // readers before publishing completion. Holding it forces release to
        // spend its report budget without cancelling the owner task.
        let reader_handles = terminal.reader_handles.lock().await;
        let release = tokio::spawn({
            let runtime = runtime.clone();
            let session_id = session_id.clone();
            async move {
                runtime.release_all_for_session(session_id.0.as_ref()).await;
            }
        });
        tokio::time::timeout(KILL_REPORT_BUDGET + Duration::from_secs(2), release)
            .await
            .expect("release exceeded its report budget")
            .expect("release task");
        assert!(
            matches!(*terminal.completion.borrow(), TerminalCompletion::Running),
            "reader lock must still prevent completion publication"
        );

        drop(reader_handles);
        tokio::time::timeout(Duration::from_secs(5), terminal.wait_for_exit())
            .await
            .expect("owner stopped publishing after release timeout")
            .expect("owner exit publication");
    }
}
