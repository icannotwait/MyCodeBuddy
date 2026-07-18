use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use sacp::schema::{
    CreateTerminalRequest, CreateTerminalResponse, KillTerminalRequest, KillTerminalResponse,
    ReleaseTerminalRequest, ReleaseTerminalResponse, TerminalExitStatus, TerminalOutputRequest,
    TerminalOutputResponse, WaitForTerminalExitRequest, WaitForTerminalExitResponse,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::acp::terminal_adapter::AcpTerminalAdapter;
use crate::terminal::shell::{build_command_line, ResolvedShellSpec};

type TerminalMap = HashMap<String, Arc<TerminalInstance>>;
const DEFAULT_OUTPUT_BYTE_LIMIT: u64 = 1_000_000;
/// After the child process exits, wait up to this long for the stdout/stderr
/// reader tasks to drain naturally before aborting them. Needed because a
/// grandchild process (e.g. Node spawned from a `.cmd` shim on Windows) can
/// inherit the pipe handle and keep it open long after the direct child
/// exits, turning `wait_for_exit` into a silent hang.
const READER_DRAIN_GRACE: Duration = Duration::from_millis(200);
/// Session-wide bound for waiting on terminal cleanup tasks. The owned tasks
/// continue in the background after this deadline so timeout cannot interrupt
/// kill/wait/drain/exit-status publication midway through its state change.
const RELEASE_KILL_BOUND: Duration = Duration::from_secs(3);
/// Upper bound for a waiter that has observed cancel (or a missing child) and
/// is waiting for the kill path to publish `exit_status`. Prevents infinite
/// hangs if kill fails to publish.
const EXIT_STATUS_WAIT_BOUND: Duration = Duration::from_secs(10);

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
                // Stable structured diagnostics only — never attach base_env,
                // request env, or command text (command lines may hold secrets).
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

struct TerminalInstance {
    session_id: String,
    output_limit: Option<usize>,
    child: Mutex<Option<tokio::process::Child>>,
    snapshot: Mutex<TerminalSnapshot>,
    reader_handles: Mutex<Vec<JoinHandle<()>>>,
    /// Signalled by [`Self::kill_command`] so a concurrent
    /// [`Self::wait_for_exit`] can drop the child mutex and let kill proceed.
    cancel: CancellationToken,
    /// Retains the published exit status so waiters cannot miss a notification
    /// between checking the snapshot and registering for the next change.
    exit_status_tx: watch::Sender<Option<TerminalExitStatus>>,
}

impl TerminalInstance {
    fn new(session_id: String, output_limit: Option<u64>, child: tokio::process::Child) -> Self {
        let (exit_status_tx, _) = watch::channel(None);
        Self {
            session_id,
            output_limit: output_limit.and_then(|v| usize::try_from(v).ok()),
            child: Mutex::new(Some(child)),
            snapshot: Mutex::new(TerminalSnapshot::default()),
            reader_handles: Mutex::new(Vec::new()),
            cancel: CancellationToken::new(),
            exit_status_tx,
        }
    }

    async fn publish_exit_status(&self, exit_status: TerminalExitStatus) {
        let published = {
            let mut snapshot = self.snapshot.lock().await;
            if snapshot.exit_status.is_none() {
                snapshot.exit_status = Some(exit_status.clone());
                true
            } else {
                false
            }
        };
        if published {
            self.exit_status_tx.send_replace(Some(exit_status));
        }
    }

    /// Wait until `exit_status` is published (or a hard bound elapses).
    async fn await_published_exit_status(
        &self,
    ) -> Result<TerminalExitStatus, TerminalRuntimeError> {
        let deadline = tokio::time::Instant::now() + EXIT_STATUS_WAIT_BOUND;
        let mut exit_status_rx = self.exit_status_tx.subscribe();
        loop {
            if let Some(exit_status) = exit_status_rx.borrow().clone() {
                return Ok(exit_status);
            }

            // Opportunistically observe a natural exit if the kill path has
            // not published yet (e.g. race where the process died first).
            self.refresh_exit_status().await?;
            if let Some(exit_status) = exit_status_rx.borrow().clone() {
                return Ok(exit_status);
            }

            match tokio::time::timeout_at(deadline, exit_status_rx.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    return Err(TerminalRuntimeError::Internal(
                        "terminal exit status publisher closed unexpectedly".to_string(),
                    ))
                }
                Err(_) => {
                    return Err(TerminalRuntimeError::Internal(
                        "timed out waiting for terminal exit status after cancel/kill".to_string(),
                    ))
                }
            }
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

    async fn refresh_exit_status(&self) -> Result<(), TerminalRuntimeError> {
        {
            let snapshot = self.snapshot.lock().await;
            if snapshot.exit_status.is_some() {
                return Ok(());
            }
        }

        let maybe_status = {
            let mut child_guard = self.child.lock().await;
            if let Some(child) = child_guard.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        *child_guard = None;
                        Some(status)
                    }
                    Ok(None) => None,
                    Err(err) => {
                        return Err(TerminalRuntimeError::Internal(format!(
                            "failed to query terminal exit status: {err}"
                        )))
                    }
                }
            } else {
                None
            }
        };

        if let Some(status) = maybe_status {
            // Drain readers BEFORE exposing exit_status. Otherwise a caller
            // polling `terminal/output` can see `exit_status = Some(...)` while
            // a grandchild process (e.g. Node spawned from a `.cmd` shim on
            // Windows) still holds the stdout/stderr pipe and is flushing
            // tail output. If the agent treats exit_status as "terminal done",
            // the trailing bytes never reach the UI. Draining here upholds the
            // invariant: whenever an external observer sees exit_status, the
            // snapshot already contains (or has explicitly given up on) all
            // reader output.
            self.drain_readers().await;
            self.publish_exit_status(map_exit_status(status)).await;
        }

        Ok(())
    }

    async fn wait_for_exit(&self) -> Result<TerminalExitStatus, TerminalRuntimeError> {
        self.refresh_exit_status().await?;
        let cached_exit = self.snapshot.lock().await.exit_status.clone();
        if let Some(exit_status) = cached_exit {
            self.drain_readers().await;
            return Ok(exit_status);
        }

        // Hold the child mutex only while racing `child.wait()` against cancel.
        // On cancel we MUST drop the mutex so `kill_command` can acquire it —
        // previously both paths held the same lock across `wait()`, so cancel
        // deadlocked behind a long-running agent `waitForExit`.
        let wait_result = {
            let mut child_guard = self.child.lock().await;
            let Some(child) = child_guard.as_mut() else {
                // Kill path (or another waiter) already took ownership / finished.
                drop(child_guard);
                let exit_status = self.await_published_exit_status().await?;
                self.drain_readers().await;
                return Ok(exit_status);
            };

            tokio::select! {
                status = child.wait() => {
                    *child_guard = None;
                    Some(status)
                }
                _ = self.cancel.cancelled() => {
                    // Release the child lock for kill_command before awaiting
                    // the published exit status.
                    None
                }
            }
        };

        match wait_result {
            Some(Ok(status)) => {
                self.drain_readers().await;
                let exit_status = map_exit_status(status);
                self.publish_exit_status(exit_status.clone()).await;
                Ok(exit_status)
            }
            Some(Err(err)) => Err(TerminalRuntimeError::Internal(format!(
                "failed waiting for terminal process to exit: {err}"
            ))),
            None => {
                let exit_status = self.await_published_exit_status().await?;
                self.drain_readers().await;
                Ok(exit_status)
            }
        }
    }

    async fn kill_command(&self) -> Result<(), TerminalRuntimeError> {
        // Signal waiters first so any concurrent `wait_for_exit` drops the
        // child mutex via its cancel branch. Do not acquire `child` before this.
        self.cancel.cancel();

        self.refresh_exit_status().await?;
        let already_exited = self.snapshot.lock().await.exit_status.is_some();
        if already_exited {
            self.drain_readers().await;
            return Ok(());
        }

        let exit_status = {
            let mut child_guard = self.child.lock().await;
            let Some(child) = child_guard.as_mut() else {
                // Waiter may still be finishing a natural exit, or another kill
                // already cleared the child. Wait for the published status.
                drop(child_guard);
                let _ = self.await_published_exit_status().await?;
                self.drain_readers().await;
                return Ok(());
            };

            if let Some(pid) = child.id() {
                if let Err(err) = kill_tree::tokio::kill_tree(pid).await {
                    tracing::error!("[ACP] kill_tree failed for pid {pid}: {err}");
                }
            }

            let status = child.wait().await.map_err(|err| {
                TerminalRuntimeError::Internal(format!(
                    "failed to wait for killed terminal process: {err}"
                ))
            })?;
            *child_guard = None;
            map_exit_status(status)
        };

        self.drain_readers().await;
        self.publish_exit_status(exit_status).await;
        Ok(())
    }

    async fn snapshot(&self) -> TerminalSnapshot {
        self.snapshot.lock().await.clone()
    }
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
    /// Immutable launch-time shell snapshot for this connection. Used for
    /// `ShellCommandLine` execution and spawn diagnostics; never re-resolved
    /// from live settings after the connection starts.
    terminal_shell: ResolvedShellSpec,
    /// Per-agent request shaping hooks (normalization, validation).
    adapter: &'static dyn AcpTerminalAdapter,
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
    /// request env (last duplicate wins) → runtime base env → process PATH.
    fn effective_path(&self, request: &CreateTerminalRequest) -> Option<OsString> {
        request_env_value(request, "PATH")
            .map(OsString::from)
            .or_else(|| map_env_value(&self.base_env, "PATH").map(OsString::from))
            .or_else(|| std::env::var_os("PATH"))
    }

    /// Deterministically classify how `request` will be spawned.
    ///
    /// - Non-empty `args` → [`ExecutionMode::DirectProgram`] (exact argv)
    /// - Empty `args` + command resolves as an existing executable → DirectProgram
    /// - Otherwise → [`ExecutionMode::ShellCommandLine`] through the snapshotted shell
    ///
    /// Does not strip quotes or parse shell syntax. Never retries under another
    /// mode after a failed spawn.
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

        Ok(ExecutionMode::ShellCommandLine)
    }

    /// Apply stdio, working directory, and environment to a freshly built
    /// terminal command. Shared by direct-exec and shell-line spawn paths so
    /// both honor the same cwd precedence and env layering.
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

        // Classify once, construct one command, spawn once. No OS-error-driven
        // fallback onto another shell or dialect translation.
        let mode = self.classify_request(&request)?;
        let mut command = match &mode {
            ExecutionMode::DirectProgram(program) => {
                let mut cmd = crate::process::tokio_command(program);
                cmd.args(&request.args);
                cmd
            }
            ExecutionMode::ShellCommandLine => {
                build_command_line(&self.terminal_shell, &request.command)
            }
        };
        self.configure_command(&mut command, &request);

        let mut child = command.spawn().map_err(|err| {
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
            child,
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

        terminal.refresh_exit_status().await?;
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
        terminal.refresh_exit_status().await?;
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

        let cleanup_tasks = removed
            .into_iter()
            .map(|terminal| {
                tokio::spawn(async move {
                    if let Err(err) = terminal.kill_command().await {
                        tracing::error!("[ACP] Failed to release terminal during cleanup: {err:?}");
                    }
                })
            })
            .collect::<Vec<_>>();

        let deadline = tokio::time::Instant::now() + RELEASE_KILL_BOUND;
        let mut timed_out = 0usize;
        for task in cleanup_tasks {
            match tokio::time::timeout_at(deadline, task).await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    tracing::error!("[ACP] terminal cleanup task failed: {err}");
                }
                Err(_) => timed_out += 1,
            }
        }
        if timed_out > 0 {
            tracing::error!(
                "[ACP] {timed_out} terminal cleanup task(s) exceeded {RELEASE_KILL_BOUND:?}; continuing in background"
            );
        }
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

/// Look up `key` in the request env list. Scans in reverse so the last
/// duplicate wins, matching the order used by [`TerminalRuntime::configure_command`].
/// Key comparison is case-insensitive on Windows.
fn request_env_value(request: &CreateTerminalRequest, key: &str) -> Option<String> {
    request
        .env
        .iter()
        .rev()
        .find(|env_var| env_keys_equal(&env_var.name, key))
        .map(|env_var| env_var.value.clone())
}

/// Look up `key` in a base env map. Returns the last case-insensitive match
/// under BTreeMap iteration order so classification PATH matches what the
/// child eventually sees when multiple spellings (e.g. `Path` / `PATH`) exist.
fn map_env_value(map: &BTreeMap<String, String>, key: &str) -> Option<String> {
    map.iter()
        .filter(|(k, _)| env_keys_equal(k, key))
        .map(|(_, v)| v.clone())
        .next_back()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::terminal_adapter::adapter_for;
    use crate::models::agent::AgentType;
    use crate::terminal::shell::{ShellCommandStrategy, ShellDialect, ShellSource};
    use sacp::schema::{EnvVariable, SessionId, WaitForTerminalExitRequest};
    use std::path::Path;

    fn test_runtime(shell: ResolvedShellSpec) -> TerminalRuntime {
        TerminalRuntime::new(BTreeMap::new(), shell, adapter_for(AgentType::Codex))
    }

    fn test_shell_spec() -> ResolvedShellSpec {
        platform_test_shell()
    }

    #[cfg(windows)]
    fn platform_long_running_command() -> String {
        "Start-Sleep -Seconds 3600".to_string()
    }

    #[cfg(unix)]
    fn platform_long_running_command() -> String {
        "sleep 3600".to_string()
    }

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
        "pwd".into()
    }

    #[cfg(windows)]
    fn platform_marker_then_exit_line(marker: &Path, code: i32) -> String {
        let marker = marker.to_string_lossy().replace('\'', "''");
        format!("Set-Content -LiteralPath '{marker}' -NoNewline -Value x; exit {code}")
    }

    #[cfg(unix)]
    fn platform_marker_then_exit_line(marker: &Path, code: i32) -> String {
        let marker = marker.to_string_lossy().replace('\'', "'\"'\"'");
        format!("printf x > '{marker}'; exit {code}")
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

    #[test]
    fn non_empty_args_are_always_direct() {
        let runtime = test_runtime(test_shell_spec());
        let mut request = CreateTerminalRequest::new(SessionId::new("s"), "cargo");
        request.args = vec![
            "check".into(),
            "--manifest-path".into(),
            r"D:\repo with space\Cargo.toml".into(),
        ];
        assert!(matches!(
            runtime.classify_request(&request).unwrap(),
            ExecutionMode::DirectProgram(_)
        ));
    }

    #[test]
    fn empty_args_existing_executable_with_spaces_is_direct() {
        let temp = tempfile::tempdir().unwrap();
        let executable = create_test_executable(temp.path().join(test_executable_name("my tool")));
        let runtime = test_runtime(test_shell_spec()).with_default_cwd(Some(temp.path().into()));
        let request = CreateTerminalRequest::new(
            SessionId::new("s"),
            executable.to_string_lossy().into_owned(),
        );
        assert_eq!(
            runtime.classify_request(&request).unwrap(),
            ExecutionMode::DirectProgram(executable)
        );
    }

    #[test]
    fn builtins_and_complete_lines_use_selected_shell() {
        let runtime = test_runtime(test_shell_spec());
        for line in [
            "cd",
            "Get-Location",
            "Set-Location 'D:\\repo'; Get-Location",
            "cd /d D:\\repo && where cargo",
            "cargo check --manifest-path D:\\repo\\Cargo.toml",
            "echo x | findstr x",
            "echo x > output.txt",
        ] {
            let request = CreateTerminalRequest::new(SessionId::new("s"), line);
            assert_eq!(
                runtime.classify_request(&request).unwrap(),
                ExecutionMode::ShellCommandLine,
                "expected shell line for {line:?}"
            );
        }
    }

    #[test]
    fn request_path_env_overrides_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let bin_dir = temp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let executable = create_test_executable(bin_dir.join(test_executable_name("special-tool")));

        let runtime = test_runtime(test_shell_spec());
        let mut request = CreateTerminalRequest::new(SessionId::new("s"), "special-tool");
        // Prefer the Windows-style `Path` key so classification still matches
        // case-insensitive env layering on Windows (and exact `PATH` on Unix
        // would also work — here we exercise the override path with PATH).
        request.env = vec![EnvVariable::new(
            "PATH",
            bin_dir.to_string_lossy().into_owned(),
        )];

        let mode = runtime.classify_request(&request).unwrap();
        match mode {
            ExecutionMode::DirectProgram(path) => {
                assert_eq!(
                    path.file_name(),
                    executable.file_name(),
                    "resolved path {path:?} should match test executable"
                );
            }
            ExecutionMode::ShellCommandLine => {
                panic!("expected DirectProgram when PATH points at the test binary")
            }
        }
    }

    #[test]
    fn request_cwd_is_used_for_relative_executable_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let executable = create_test_executable(temp.path().join(test_executable_name("rel-tool")));
        let runtime = test_runtime(test_shell_spec());

        #[cfg(windows)]
        let command = format!(r".\{}", test_executable_name("rel-tool"));
        #[cfg(unix)]
        let command = format!("./{}", test_executable_name("rel-tool"));

        let mut request = CreateTerminalRequest::new(SessionId::new("s"), command);
        request.cwd = Some(temp.path().to_path_buf());

        assert_eq!(
            runtime.classify_request(&request).unwrap(),
            ExecutionMode::DirectProgram(executable)
        );
    }

    #[test]
    fn whitespace_only_command_is_rejected() {
        let runtime = test_runtime(test_shell_spec());
        let session_id = SessionId::new("blank-command".to_string());
        let request = CreateTerminalRequest::new(session_id, "   ".to_string());

        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(runtime.create_terminal(request));
        assert!(
            matches!(result, Err(TerminalRuntimeError::InvalidParams(_))),
            "expected InvalidParams for a whitespace-only command"
        );
    }

    #[tokio::test]
    async fn complete_line_runs_through_selected_shell() {
        let runtime = test_runtime(platform_test_shell());
        let line = platform_print_working_directory_line();
        let request = CreateTerminalRequest::new(SessionId::new("s"), line);
        let response = runtime.create_terminal(request).await.unwrap();
        let result = runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                SessionId::new("s"),
                response.terminal_id,
            ))
            .await
            .unwrap();
        assert_eq!(result.exit_status.exit_code, Some(0));
    }

    #[tokio::test]
    async fn nonzero_marker_command_executes_once_without_retry() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("marker.txt");
        let runtime =
            test_runtime(platform_test_shell()).with_default_cwd(Some(temp.path().into()));
        let request = CreateTerminalRequest::new(
            SessionId::new("s"),
            platform_marker_then_exit_line(&marker, 7),
        );
        let response = runtime.create_terminal(request).await.unwrap();
        runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                SessionId::new("s"),
                response.terminal_id,
            ))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "x");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_absolute_complete_line_is_not_spawned_as_a_program() {
        let temp = tempfile::tempdir().unwrap();
        let quoted = temp.path().to_string_lossy().replace('\'', "''");
        let line = format!("Set-Location -LiteralPath '{quoted}'; Get-Location");
        let runtime = test_runtime(platform_test_shell());
        let session_id = SessionId::new("powershell-absolute-line");
        let response = runtime
            .create_terminal(CreateTerminalRequest::new(session_id.clone(), line))
            .await
            .unwrap();
        let result = runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id,
                response.terminal_id,
            ))
            .await
            .unwrap();
        assert_eq!(result.exit_status.exit_code, Some(0));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cmd_drive_switch_and_where_run_through_cmd() {
        let temp = tempfile::tempdir().unwrap();
        // Avoid nested `"..."` around the path: Rust's Command quotes the whole
        // `/C` argument, which breaks cmd's nested-quote parsing for
        // `cd /d "path"`. Temp paths have no spaces, so unquoted `cd /d` is
        // still a real CMD builtin line under the snapshotted Cmd strategy.
        let line = format!("cd /d {} && where cmd.exe", temp.path().to_string_lossy());
        let runtime = test_runtime(windows_cmd_test_shell());
        let session_id = SessionId::new("cmd-builtins");
        let response = runtime
            .create_terminal(CreateTerminalRequest::new(session_id.clone(), line))
            .await
            .unwrap();
        let result = runtime
            .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                session_id,
                response.terminal_id,
            ))
            .await
            .unwrap();
        assert_eq!(result.exit_status.exit_code, Some(0));
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
        let runtime = test_runtime(platform_test_shell());
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
        let mut shell_runtime = test_runtime(test_shell_spec());
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

        let program_runtime = test_runtime(test_shell_spec());
        let mut direct =
            CreateTerminalRequest::new(SessionId::new("program-error"), "missing-program-for-test");
        direct.args = vec!["--version".into()];
        let program_error = program_runtime.create_terminal(direct).await.unwrap_err();
        let program_rpc = serde_json::to_value(program_error.into_rpc_error()).unwrap();
        assert_eq!(program_rpc["data"]["code"], "terminal_program_spawn_failed");
        assert_eq!(program_rpc["data"]["mode"], "direct_program");
    }

    /// Regression: when an ACP agent calls `terminal/create` (e.g. to run
    /// `git push`), the runtime's base env — populated by the connection
    /// layer with the codeg credential helper's `GIT_CONFIG_*` keys —
    /// must reach the spawned process. Per-request `env` from the agent
    /// still wins on key collision so the agent can scrub or override
    /// specific keys for individual commands.
    #[cfg(unix)]
    #[tokio::test]
    async fn base_env_propagates_and_request_env_overrides() {
        let mut base_env = BTreeMap::new();
        base_env.insert("CODEG_TEST_BASE_VAR".to_string(), "from_base".to_string());
        base_env.insert("CODEG_TEST_OVERRIDE".to_string(), "loses".to_string());
        let runtime = TerminalRuntime::new(
            base_env,
            platform_test_shell(),
            adapter_for(AgentType::Codex),
        );

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

    /// A `terminal/create` that omits `cwd` defaults to the runtime's
    /// configured working directory rather than codeg's own process cwd.
    #[cfg(unix)]
    #[tokio::test]
    async fn falls_back_to_default_cwd_when_request_omits_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");
        let canonical = dir.path().canonicalize().expect("canonicalize");
        let runtime =
            test_runtime(platform_test_shell()).with_default_cwd(Some(dir.path().to_path_buf()));

        let session_id = SessionId::new("cwd-default".to_string());
        // Bare `pwd` (no whitespace) → may be DirectProgram if on PATH;
        // current_dir still applies either way.
        let request = CreateTerminalRequest::new(session_id.clone(), "pwd".to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;

        assert!(
            output.contains(canonical.to_string_lossy().as_ref()),
            "terminal did not run in the default cwd; got:\n{output}"
        );
    }

    /// An explicit absolute `cwd` in the request takes precedence over the
    /// runtime default.
    #[cfg(unix)]
    #[tokio::test]
    async fn request_cwd_overrides_default_cwd() {
        let default_dir = tempfile::tempdir().expect("default dir");
        let request_dir = tempfile::tempdir().expect("request dir");
        let request_canonical = request_dir.path().canonicalize().expect("canonicalize");
        let runtime = test_runtime(platform_test_shell())
            .with_default_cwd(Some(default_dir.path().to_path_buf()));

        let session_id = SessionId::new("cwd-override".to_string());
        let mut request = CreateTerminalRequest::new(session_id.clone(), "pwd".to_string());
        request.cwd = Some(request_dir.path().to_path_buf());
        let output = run_and_capture(&runtime, &session_id, request).await;

        assert!(
            output.contains(request_canonical.to_string_lossy().as_ref()),
            "request cwd did not take precedence over the default; got:\n{output}"
        );
    }

    /// A whitespace-bearing command with empty args runs through the shell.
    #[cfg(unix)]
    #[tokio::test]
    async fn whitespace_command_runs_through_shell() {
        let runtime = test_runtime(platform_test_shell());

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

    /// The shell-wrapped path still honors the working directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn shell_wrapped_command_respects_cwd() {
        let dir = tempfile::tempdir().expect("temp dir");
        let canonical = dir.path().canonicalize().expect("canonicalize");
        let runtime =
            test_runtime(platform_test_shell()).with_default_cwd(Some(dir.path().to_path_buf()));

        let session_id = SessionId::new("shell-cwd".to_string());
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
    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_args_bypass_shell_wrap() {
        let runtime = test_runtime(platform_test_shell());

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
    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_missing_cwd_surfaces_as_spawn_failure() {
        let default_dir = tempfile::tempdir().expect("default dir");
        let runtime = test_runtime(platform_test_shell())
            .with_default_cwd(Some(default_dir.path().to_path_buf()));

        let session_id = SessionId::new("missing-cwd".to_string());
        let mut request = CreateTerminalRequest::new(session_id, "pwd".to_string());
        request.cwd = Some(PathBuf::from("/codeg-nonexistent-cwd/does/not/exist"));

        let result = runtime.create_terminal(request).await;
        assert!(
            matches!(result, Err(TerminalRuntimeError::Spawn { .. })),
            "expected a spawn failure for a missing explicit cwd, got {result:?}"
        );
    }

    /// A real executable whose path contains spaces is exec'd directly.
    #[cfg(unix)]
    #[tokio::test]
    async fn executable_path_with_spaces_is_not_shell_wrapped() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let exe = dir.path().join("my tool"); // space in the file name
        std::fs::write(&exe, "#!/bin/sh\necho ran-directly\n").expect("write script");
        let mut perms = std::fs::metadata(&exe).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&exe, perms).expect("chmod");

        let runtime = test_runtime(platform_test_shell());
        let session_id = SessionId::new("space-exe".to_string());
        let request =
            CreateTerminalRequest::new(session_id.clone(), exe.to_string_lossy().to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("ran-directly"),
            "space-containing executable was not exec'd directly; got:\n{output}"
        );
    }

    /// A relative, space-containing executable resolves against the terminal's
    /// effective cwd and runs directly.
    #[cfg(unix)]
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
            test_runtime(platform_test_shell()).with_default_cwd(Some(dir.path().to_path_buf()));
        let session_id = SessionId::new("rel-space-exe".to_string());
        let request = CreateTerminalRequest::new(session_id.clone(), "./my tool".to_string());
        let output = run_and_capture(&runtime, &session_id, request).await;
        assert!(
            output.contains("ran-relative"),
            "relative space-containing exe was not run in the effective cwd; got:\n{output}"
        );
    }

    /// Regression: a concurrent `wait_for_terminal_exit` must not hold the
    /// child mutex across the whole process lifetime in a way that blocks
    /// `release_all_for_session` / kill. Without CancellationToken, this
    /// deadlocks for the full sleep duration (and cancel UI hangs forever).
    #[tokio::test]
    async fn concurrent_wait_and_session_release_completes_promptly() {
        let runtime = Arc::new(test_runtime(platform_test_shell()));
        let session_id = SessionId::new("wait-release-race");

        let response = runtime
            .create_terminal(CreateTerminalRequest::new(
                session_id.clone(),
                platform_long_running_command(),
            ))
            .await
            .expect("create long-running terminal");
        let terminal_id = response.terminal_id.clone();
        let terminal = runtime
            .find_terminal(terminal_id.0.as_ref(), session_id.0.as_ref())
            .await
            .expect("terminal exists");

        // Capture the child PID before concurrent wait/release so we can assert
        // the process tree was actually terminated (not just that futures
        // unblocked).
        let child_pid = {
            let guard = terminal.child.lock().await;
            guard.as_ref().and_then(|child| child.id())
        };
        assert!(
            child_pid.is_some(),
            "expected a live child pid before release"
        );

        let wait_runtime = Arc::clone(&runtime);
        let wait_session = session_id.clone();
        let wait_terminal = terminal_id.clone();
        let wait_handle = tokio::spawn(async move {
            wait_runtime
                .wait_for_terminal_exit(WaitForTerminalExitRequest::new(
                    wait_session,
                    wait_terminal,
                ))
                .await
        });

        // On this current-thread runtime, try_lock can only fail here if the
        // waiter yielded from child.wait() while retaining the mutex. The
        // refresh path never yields while it owns this lock.
        let wall_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while let Ok(guard) = terminal.child.try_lock() {
            drop(guard);
            assert!(
                std::time::Instant::now() < wall_deadline,
                "waiter never acquired the child mutex"
            );
            tokio::task::yield_now().await;
        }

        let release_runtime = Arc::clone(&runtime);
        let release_session = session_id.0.to_string();
        let release_handle = tokio::spawn(async move {
            release_runtime
                .release_all_for_session(&release_session)
                .await;
        });

        let joined = tokio::time::timeout(Duration::from_secs(5), async {
            let wait_result = wait_handle.await.expect("wait task join");
            release_handle.await.expect("release task join");
            wait_result
        })
        .await
        .expect("wait+release must complete within 5s (cancel/kill deadlock regression)");

        let wait_response = joined.expect("wait_for_terminal_exit after kill");
        // Killed processes may report signal/non-zero code depending on OS;
        // the critical property is that wait returned at all with a status.
        assert!(
            wait_response.exit_status.exit_code.is_some()
                || wait_response.exit_status.signal.is_some(),
            "expected an exit code or signal after kill; got {:?}",
            wait_response.exit_status
        );

        if let Some(pid) = child_pid {
            // Give the OS a brief moment to reap; kill_tree should have
            // terminated the process already by the time wait returned.
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(
                !process_is_alive(pid),
                "process tree root pid {pid} still alive after session release"
            );
        }
    }

    /// Regression: the release bound must stop waiting for cleanup without
    /// cancelling the task that owns child-exit publication. The old direct
    /// `timeout(kill_command())` could drop that future after clearing `child`
    /// and strand every waiter with no remaining status publisher.
    #[tokio::test]
    async fn release_timeout_does_not_cancel_exit_publication() {
        let runtime = Arc::new(test_runtime(platform_test_shell()));
        let session_id = SessionId::new("release-timeout-publication");
        let response = runtime
            .create_terminal(CreateTerminalRequest::new(
                session_id.clone(),
                platform_long_running_command(),
            ))
            .await
            .expect("create terminal");
        let terminal = runtime
            .find_terminal(response.terminal_id.0.as_ref(), session_id.0.as_ref())
            .await
            .expect("terminal exists");

        // Keep drain_readers blocked at its mutex after the process exits so
        // the test can pin kill_command between clearing child and publishing
        // exit_status without relying on timer scheduling.
        let reader_handles_guard = terminal.reader_handles.lock().await;

        let release_runtime = Arc::clone(&runtime);
        let release_session = session_id.0.to_string();
        let release_started = std::time::Instant::now();
        let release = tokio::spawn(async move {
            release_runtime
                .release_all_for_session(&release_session)
                .await;
        });

        let wall_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while terminal.child.lock().await.is_some() {
            assert!(
                std::time::Instant::now() < wall_deadline,
                "kill did not clear child"
            );
            tokio::task::yield_now().await;
        }
        assert!(
            release_started.elapsed() < RELEASE_KILL_BOUND,
            "release timed out before the child was cleared"
        );

        tokio::time::pause();
        tokio::time::advance(RELEASE_KILL_BOUND).await;
        release.await.expect("release task join");
        assert!(
            terminal.snapshot().await.exit_status.is_none(),
            "test precondition failed: exit status published while reader lock was held"
        );
        drop(reader_handles_guard);

        for _ in 0..100 {
            if terminal.snapshot().await.exit_status.is_some() {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("detached kill task did not publish exit status");
    }

    /// A waiter that subscribes after publication must still observe the exit
    /// status immediately. An edge-triggered Notify cannot provide this
    /// contract without a check/register race; a retained signal can.
    #[tokio::test]
    async fn exit_status_signal_retains_publication_for_late_subscriber() {
        let runtime = test_runtime(platform_test_shell());
        let session_id = SessionId::new("retained-exit-status");
        let response = runtime
            .create_terminal(CreateTerminalRequest::new(session_id.clone(), "exit 0"))
            .await
            .expect("create terminal");
        let terminal = runtime
            .find_terminal(response.terminal_id.0.as_ref(), session_id.0.as_ref())
            .await
            .expect("terminal exists");
        let expected = terminal.wait_for_exit().await.expect("wait for exit");

        let receiver = terminal.exit_status_tx.subscribe();
        let observed = receiver.borrow().clone();
        assert_eq!(observed, Some(expected));

        runtime.release_all_for_session(session_id.0.as_ref()).await;
    }

    #[cfg(windows)]
    fn process_is_alive(pid: u32) -> bool {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|output| {
                let text = String::from_utf8_lossy(&output.stdout);
                text.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }

    #[cfg(unix)]
    fn process_is_alive(pid: u32) -> bool {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}
