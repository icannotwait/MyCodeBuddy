#[cfg(any(windows, test))]
use std::fs::OpenOptions;
#[cfg(any(windows, test))]
use std::io;
#[cfg(any(windows, test))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

#[cfg(any(windows, test))]
use chrono::{DateTime, Utc};
use regex::Regex;
use tauri::Manager;

pub(crate) const REGISTERED_WINDOW_LABELS_MAX: usize = 16;
#[cfg(windows)]
const WEBVIEW2_ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
#[cfg(windows)]
const WEBVIEW_DEBUG_ENV: &str = "CODEG_WEBVIEW_DEBUG";
#[cfg(windows)]
const INTERNAL_LOG_DIR: &str = "webview2-internal";
#[cfg(any(windows, test))]
const LOG_RESERVATION_ATTEMPTS: u32 = 32;
#[cfg(any(windows, test))]
const RETAINED_INTERNAL_LOGS_MAX: usize = 5;
const DIAGNOSTIC_TEXT_MAX_CHARS: usize = 240;

pub(crate) struct ProcessStart {
    instant: Instant,
    #[cfg(windows)]
    utc: DateTime<Utc>,
}

impl ProcessStart {
    pub(crate) fn now() -> Self {
        Self {
            instant: Instant::now(),
            #[cfg(windows)]
            utc: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SafeSwitchState {
    pub(crate) disable_gpu: bool,
    pub(crate) enable_logging: bool,
    pub(crate) verbosity: Option<u32>,
    pub(crate) log_file_present: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeSnapshot {
    pub(crate) app_version: &'static str,
    pub(crate) app_pid: u32,
    pub(crate) webview_version: Option<String>,
    pub(crate) webview_version_error: Option<String>,
    pub(crate) disable_hardware_acceleration: bool,
    pub(crate) webview_debug_enabled: bool,
    pub(crate) safe_switches: SafeSwitchState,
    pub(crate) browser_executable_override_present: bool,
    pub(crate) user_data_override_present: bool,
    pub(crate) release_channel_override_present: bool,
    pub(crate) webview_log_path: Option<PathBuf>,
}

pub(crate) struct ProcessState {
    started_at: Instant,
    pub(crate) snapshot: RuntimeSnapshot,
}

static PROCESS_STATE: OnceLock<ProcessState> = OnceLock::new();
static DIAGNOSTICS_WARNING_EMITTED: AtomicBool = AtomicBool::new(false);
static WINDOW_ATTEMPT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn current_process_state() -> &'static ProcessState {
    PROCESS_STATE
        .get()
        .expect("window diagnostics must be initialized before window creation")
}

#[cfg(any(windows, test))]
fn debug_requested(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        let value = value.trim();
        value == "1" || value.eq_ignore_ascii_case("true")
    })
}

fn tokenize_chromium_args(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut token_started = false;
    let mut in_quotes = false;
    let mut index = 0;

    while index < chars.len() {
        let current = chars[index];
        if current.is_ascii_whitespace() && !in_quotes {
            if token_started {
                tokens.push(std::mem::take(&mut token));
                token_started = false;
            }
            index += 1;
            continue;
        }

        if current == '\\' {
            let run_start = index;
            while index < chars.len() && chars[index] == '\\' {
                index += 1;
            }
            let slash_count = index - run_start;
            token_started = true;
            if index < chars.len() && chars[index] == '"' {
                token.extend(std::iter::repeat_n('\\', slash_count / 2));
                if slash_count % 2 == 0 {
                    in_quotes = !in_quotes;
                } else {
                    token.push('"');
                }
                index += 1;
            } else {
                token.extend(std::iter::repeat_n('\\', slash_count));
            }
            continue;
        }

        token_started = true;
        if current == '"' {
            in_quotes = !in_quotes;
        } else {
            token.push(current);
        }
        index += 1;
    }

    if token_started {
        tokens.push(token);
    }
    tokens
}

#[cfg(any(windows, test))]
fn serialize_chromium_args(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| serialize_chromium_token(token))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(any(windows, test))]
fn serialize_chromium_token(token: &str) -> String {
    if !token.is_empty()
        && !token
            .chars()
            .any(|value| value.is_ascii_whitespace() || value == '"')
    {
        return token.to_string();
    }

    let chars: Vec<char> = token.chars().collect();
    let mut serialized = String::from("\"");
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '\\' {
            if chars[index] == '"' {
                serialized.push('\\');
            }
            serialized.push(chars[index]);
            index += 1;
            continue;
        }

        let run_start = index;
        while index < chars.len() && chars[index] == '\\' {
            index += 1;
        }
        let slash_count = index - run_start;
        let before_quote = index < chars.len() && chars[index] == '"';
        let at_end = index == chars.len();
        let output_count = if before_quote {
            slash_count * 2 + 1
        } else if at_end {
            slash_count * 2
        } else {
            slash_count
        };
        serialized.extend(std::iter::repeat_n('\\', output_count));
        if before_quote {
            serialized.push('"');
            index += 1;
        }
    }
    serialized.push('"');
    serialized
}

#[cfg(any(windows, test))]
fn merge_browser_args(
    existing: &str,
    disable_hardware_acceleration: bool,
    debug_log_path: Option<&Path>,
) -> String {
    let parsed = tokenize_chromium_args(existing);
    let mut merged = Vec::with_capacity(parsed.len() + 4);

    if debug_log_path.is_some() {
        let mut index = 0;
        while index < parsed.len() {
            let token = &parsed[index];
            if token == "--enable-logging"
                || token.starts_with("--v=")
                || token.starts_with("--log-file=")
            {
                index += 1;
            } else if token == "--v" || token == "--log-file" {
                index = (index + 2).min(parsed.len());
            } else {
                merged.push(token.clone());
                index += 1;
            }
        }
    } else {
        merged = parsed;
    }

    if disable_hardware_acceleration && !merged.iter().any(|token| token == "--disable-gpu") {
        merged.push("--disable-gpu".to_string());
    }
    if let Some(log_path) = debug_log_path {
        merged.push("--enable-logging".to_string());
        merged.push("--v=1".to_string());
        merged.push(format!("--log-file={}", log_path.display()));
    }

    serialize_chromium_args(&merged)
}

fn summarize_switches(tokens: &[String]) -> SafeSwitchState {
    let mut state = SafeSwitchState::default();
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        match token.as_str() {
            "--disable-gpu" => state.disable_gpu = true,
            "--enable-logging" => state.enable_logging = true,
            "--v" => {
                if let Some(value) = tokens.get(index + 1).and_then(|value| parse_decimal(value)) {
                    state.verbosity = Some(value);
                }
                index += usize::from(index + 1 < tokens.len());
            }
            "--log-file" => {
                state.log_file_present = true;
                index += usize::from(index + 1 < tokens.len());
            }
            _ => {
                if let Some(value) = token.strip_prefix("--v=").and_then(parse_decimal) {
                    state.verbosity = Some(value);
                } else if token.starts_with("--log-file=") {
                    state.log_file_present = true;
                }
            }
        }
        index += 1;
    }
    state
}

fn parse_decimal(value: &str) -> Option<u32> {
    (!value.is_empty() && value.bytes().all(|value| value.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

pub(crate) fn sanitize_diagnostic_text(value: &str) -> String {
    let without_controls: String = value
        .chars()
        .map(|value| if value.is_control() { ' ' } else { value })
        .collect();

    let url_pattern =
        Regex::new(r#"(?i)\b(?:https?|file)://[^\s\"'<>]+"#).expect("valid URL regex");
    let without_urls = url_pattern.replace_all(&without_controls, "<url>");

    let quoted_path_pattern = Regex::new(
        r#"(?x)(?:\"(?:[A-Za-z]:[\\/]|\\\\|/)[^\"]*\"|'(?:[A-Za-z]:[\\/]|\\\\|/)[^']*')"#,
    )
    .expect("valid quoted path regex");
    let without_quoted_paths = quoted_path_pattern.replace_all(&without_urls, "<path>");

    let windows_path_pattern =
        Regex::new(r#"(?i)(?:^|([^A-Za-z0-9]))(?:[A-Z]:[\\/]|\\\\)[^\s\"'<>]+"#)
            .expect("valid Windows path regex");
    let without_windows_paths =
        windows_path_pattern.replace_all(&without_quoted_paths, "${1}<path>");
    let posix_path_pattern = Regex::new(r#"/[^\s\"'<>]+"#).expect("valid POSIX path regex");
    let without_paths = posix_path_pattern.replace_all(&without_windows_paths, "<path>");

    let query_pattern =
        Regex::new(r#"\?[^\s\"'<>]*=[^\s\"'<>]*"#).expect("valid query-fragment regex");
    query_pattern
        .replace_all(&without_paths, "<query>")
        .chars()
        .take(DIAGNOSTIC_TEXT_MAX_CHARS)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ErrorProjection {
    failure_kind: &'static str,
    error_hresult: Option<String>,
    error_message: String,
}

fn project_error(error: &(dyn std::error::Error + 'static)) -> ErrorProjection {
    let hresult = extract_hresult(error);
    ErrorProjection {
        failure_kind: classify_hresult(hresult),
        error_hresult: hresult.map(|value| format!("0x{value:08x}")),
        error_message: sanitize_diagnostic_text(&error.to_string()),
    }
}

fn extract_hresult(error: &(dyn std::error::Error + 'static)) -> Option<u32> {
    static HRESULT_PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = HRESULT_PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\bHRESULT(?:\s*\(\s*|\s+)0x([0-9a-f]{8})\b").expect("valid HRESULT regex")
    });
    let mut current = Some(error);

    while let Some(source) = current {
        let message = source.to_string();
        if let Some(value) = pattern
            .captures(&message)
            .and_then(|captures| captures.get(1))
            .and_then(|value| u32::from_str_radix(value.as_str(), 16).ok())
        {
            return Some(value);
        }
        current = source.source();
    }
    None
}

fn classify_hresult(hresult: Option<u32>) -> &'static str {
    match hresult {
        Some(0x80010108) => "rpc_disconnected",
        _ => "unknown",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowKind {
    Main,
    ConversationPopout,
    Settings,
    ImportSessions,
    Commit,
    Merge,
    Stash,
    Push,
    ProjectBoot,
    Pet,
    PetPanel,
    RemoteWorkspace,
}

impl WindowKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::ConversationPopout => "conversation_popout",
            Self::Settings => "settings",
            Self::ImportSessions => "import_sessions",
            Self::Commit => "commit",
            Self::Merge => "merge",
            Self::Stash => "stash",
            Self::Push => "push",
            Self::ProjectBoot => "project_boot",
            Self::Pet => "pet",
            Self::PetPanel => "pet_panel",
            Self::RemoteWorkspace => "remote_workspace",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowIdentity {
    attempt_id: String,
    window_kind: WindowKind,
    window_label: String,
    caller_label: Option<String>,
    operation_id: Option<String>,
    app_pid: u32,
    app_uptime_ms: u128,
    registered_window_count: usize,
    registered_window_labels: Vec<String>,
    registered_window_labels_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiagnosticEvent {
    Started(WindowIdentity),
    Succeeded {
        attempt_id: String,
        window_kind: WindowKind,
        window_label: String,
        elapsed_ms: u128,
    },
    Failed {
        identity: WindowIdentity,
        elapsed_ms: u128,
        error: ErrorProjection,
        webview_version: Option<String>,
        safe_switches: SafeSwitchState,
    },
    Abandoned {
        identity: WindowIdentity,
        elapsed_ms: u128,
    },
}

impl DiagnosticEvent {
    #[cfg(test)]
    fn event_name(&self) -> &'static str {
        match self {
            Self::Started(_) => "window_create_started",
            Self::Succeeded { .. } => "window_create_succeeded",
            Self::Failed { .. } => "window_create_failed",
            Self::Abandoned { .. } => "window_create_abandoned",
        }
    }

    #[cfg(test)]
    fn is_terminal(&self) -> bool {
        !matches!(self, Self::Started(_))
    }
}

trait DiagnosticSink: Send + Sync {
    fn emit(&self, event: DiagnosticEvent) -> Result<(), String>;
}

struct TracingDiagnosticSink;

impl DiagnosticSink for TracingDiagnosticSink {
    fn emit(&self, event: DiagnosticEvent) -> Result<(), String> {
        match event {
            DiagnosticEvent::Started(identity) => {
                tracing::info!(
                    target: "codeg::window",
                    event = "window_create_started",
                    attempt_id = identity.attempt_id,
                    window_kind = identity.window_kind.as_str(),
                    window_label = identity.window_label,
                    caller_label = identity.caller_label.as_deref(),
                    operation_id = identity.operation_id.as_deref(),
                    app_pid = identity.app_pid,
                    app_uptime_ms = identity.app_uptime_ms,
                    registered_window_count = identity.registered_window_count,
                    registered_window_labels = ?identity.registered_window_labels,
                    registered_window_labels_truncated = identity.registered_window_labels_truncated,
                );
            }
            DiagnosticEvent::Succeeded {
                attempt_id,
                window_kind,
                window_label,
                elapsed_ms,
            } => {
                tracing::info!(
                    target: "codeg::window",
                    event = "window_create_succeeded",
                    attempt_id,
                    window_kind = window_kind.as_str(),
                    window_label,
                    elapsed_ms,
                );
            }
            DiagnosticEvent::Failed {
                identity,
                elapsed_ms,
                error,
                webview_version,
                safe_switches,
            } => {
                tracing::error!(
                    target: "codeg::window",
                    event = "window_create_failed",
                    attempt_id = identity.attempt_id,
                    window_kind = identity.window_kind.as_str(),
                    window_label = identity.window_label,
                    caller_label = identity.caller_label.as_deref(),
                    operation_id = identity.operation_id.as_deref(),
                    app_pid = identity.app_pid,
                    app_uptime_ms = identity.app_uptime_ms,
                    registered_window_count = identity.registered_window_count,
                    registered_window_labels = ?identity.registered_window_labels,
                    registered_window_labels_truncated = identity.registered_window_labels_truncated,
                    elapsed_ms,
                    failure_kind = error.failure_kind,
                    error_hresult = error.error_hresult.as_deref(),
                    error_message = error.error_message,
                    webview_version = webview_version.as_deref(),
                    disable_gpu = safe_switches.disable_gpu,
                    enable_logging = safe_switches.enable_logging,
                    verbosity = safe_switches.verbosity,
                    log_file_present = safe_switches.log_file_present,
                );
            }
            DiagnosticEvent::Abandoned {
                identity,
                elapsed_ms,
            } => {
                tracing::warn!(
                    target: "codeg::window",
                    event = "window_create_abandoned",
                    attempt_id = identity.attempt_id,
                    window_kind = identity.window_kind.as_str(),
                    window_label = identity.window_label,
                    caller_label = identity.caller_label.as_deref(),
                    operation_id = identity.operation_id.as_deref(),
                    app_pid = identity.app_pid,
                    app_uptime_ms = identity.app_uptime_ms,
                    registered_window_count = identity.registered_window_count,
                    registered_window_labels = ?identity.registered_window_labels,
                    registered_window_labels_truncated = identity.registered_window_labels_truncated,
                    elapsed_ms,
                );
            }
        }
        Ok(())
    }
}

static TRACING_DIAGNOSTIC_SINK: TracingDiagnosticSink = TracingDiagnosticSink;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisteredWindowSnapshot {
    count: usize,
    labels: Vec<String>,
    truncated: bool,
}

fn snapshot_registered_window_labels(
    labels: impl IntoIterator<Item = String>,
) -> RegisteredWindowSnapshot {
    let mut labels: Vec<_> = labels.into_iter().collect();
    labels.sort();
    let count = labels.len();
    labels.truncate(REGISTERED_WINDOW_LABELS_MAX);
    RegisteredWindowSnapshot {
        count,
        labels,
        truncated: count > REGISTERED_WINDOW_LABELS_MAX,
    }
}

fn format_attempt_id(pid: u32, sequence: u64) -> String {
    format!("window-{pid}-{sequence}")
}

fn next_attempt_id(pid: u32) -> String {
    let sequence = WINDOW_ATTEMPT_SEQUENCE.fetch_add(1, Ordering::Relaxed) + 1;
    format_attempt_id(pid, sequence)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FailureRuntimeContext {
    webview_version: Option<String>,
    safe_switches: SafeSwitchState,
}

fn failure_runtime_context(snapshot: &RuntimeSnapshot) -> FailureRuntimeContext {
    FailureRuntimeContext {
        webview_version: snapshot.webview_version.clone(),
        safe_switches: snapshot.safe_switches.clone(),
    }
}

#[derive(Clone)]
struct AttemptContext<'a> {
    sink: &'a dyn DiagnosticSink,
    window_kind: WindowKind,
    window_label: String,
    caller_label: Option<String>,
    operation_id: Option<String>,
    app_pid: u32,
    app_uptime_ms: u128,
    registered_window_labels: Vec<String>,
    failure_runtime: FailureRuntimeContext,
}

struct WindowCreationAttempt<'a> {
    identity: WindowIdentity,
    started_at: Instant,
    sink: &'a dyn DiagnosticSink,
    terminal_emitted: bool,
    failure_runtime: FailureRuntimeContext,
}

impl WindowCreationAttempt<'static> {
    fn begin(
        app: &tauri::AppHandle,
        kind: WindowKind,
        window_label: &str,
        caller_label: Option<&str>,
        operation_id: Option<&str>,
    ) -> Self {
        let process = current_process_state();
        let registered_window_labels = app.webview_windows().into_keys().collect();
        Self::begin_with_context(AttemptContext {
            sink: &TRACING_DIAGNOSTIC_SINK,
            window_kind: kind,
            window_label: window_label.to_string(),
            caller_label: caller_label.map(str::to_string),
            operation_id: operation_id.map(str::to_string),
            app_pid: process.snapshot.app_pid,
            app_uptime_ms: process.started_at.elapsed().as_millis(),
            registered_window_labels,
            failure_runtime: failure_runtime_context(&process.snapshot),
        })
    }
}

impl<'a> WindowCreationAttempt<'a> {
    fn begin_with_context(context: AttemptContext<'a>) -> Self {
        let registered = snapshot_registered_window_labels(context.registered_window_labels);
        let identity = WindowIdentity {
            attempt_id: next_attempt_id(context.app_pid),
            window_kind: context.window_kind,
            window_label: context.window_label,
            caller_label: context.caller_label,
            operation_id: context.operation_id,
            app_pid: context.app_pid,
            app_uptime_ms: context.app_uptime_ms,
            registered_window_count: registered.count,
            registered_window_labels: registered.labels,
            registered_window_labels_truncated: registered.truncated,
        };
        let attempt = Self {
            identity,
            started_at: Instant::now(),
            sink: context.sink,
            terminal_emitted: false,
            failure_runtime: context.failure_runtime,
        };
        attempt.emit_nonfatal(
            "window_create_started_sink",
            DiagnosticEvent::Started(attempt.identity.clone()),
        );
        attempt
    }

    #[cfg(test)]
    fn begin_for_test(context: AttemptContext<'a>) -> Self {
        Self::begin_with_context(context)
    }

    fn finish_success(&mut self) {
        if self.terminal_emitted {
            return;
        }
        self.terminal_emitted = true;
        self.emit_nonfatal(
            "window_create_succeeded_sink",
            DiagnosticEvent::Succeeded {
                attempt_id: self.identity.attempt_id.clone(),
                window_kind: self.identity.window_kind,
                window_label: self.identity.window_label.clone(),
                elapsed_ms: self.started_at.elapsed().as_millis(),
            },
        );
    }

    fn finish_failure(&mut self, error: &(dyn std::error::Error + 'static)) {
        if self.terminal_emitted {
            return;
        }
        let event = DiagnosticEvent::Failed {
            identity: self.identity.clone(),
            elapsed_ms: self.started_at.elapsed().as_millis(),
            error: project_error(error),
            webview_version: self.failure_runtime.webview_version.clone(),
            safe_switches: self.failure_runtime.safe_switches.clone(),
        };
        self.terminal_emitted = true;
        self.emit_nonfatal("window_create_failed_sink", event);
    }

    fn emit_nonfatal(&self, stage: &'static str, event: DiagnosticEvent) {
        if let Err(error) = self.sink.emit(event) {
            diagnostics_warn_once(stage, &error);
        }
    }
}

impl Drop for WindowCreationAttempt<'_> {
    fn drop(&mut self) {
        if self.terminal_emitted {
            return;
        }
        self.terminal_emitted = true;
        self.emit_nonfatal(
            "window_create_abandoned_sink",
            DiagnosticEvent::Abandoned {
                identity: self.identity.clone(),
                elapsed_ms: self.started_at.elapsed().as_millis(),
            },
        );
    }
}

fn run_build_with_attempt<T, E, F>(mut attempt: WindowCreationAttempt<'_>, build: F) -> Result<T, E>
where
    E: std::error::Error + 'static,
    F: FnOnce() -> Result<T, E>,
{
    match build() {
        Ok(value) => {
            attempt.finish_success();
            Ok(value)
        }
        Err(error) => {
            attempt.finish_failure(&error);
            Err(error)
        }
    }
}

pub(crate) fn build_with_diagnostics<T, E, F>(
    app: &tauri::AppHandle,
    kind: WindowKind,
    window_label: &str,
    caller_label: Option<&str>,
    operation_id: Option<&str>,
    build: F,
) -> Result<T, E>
where
    E: std::error::Error + 'static,
    F: FnOnce() -> Result<T, E>,
{
    let attempt = WindowCreationAttempt::begin(app, kind, window_label, caller_label, operation_id);
    run_build_with_attempt(attempt, build)
}

#[cfg(test)]
fn run_build_for_test<T, E, F>(context: AttemptContext<'_>, build: F) -> Result<T, E>
where
    E: std::error::Error + 'static,
    F: FnOnce() -> Result<T, E>,
{
    run_build_with_attempt(WindowCreationAttempt::begin_for_test(context), build)
}

struct RuntimeSnapshotInputs<'a> {
    app_version: &'static str,
    app_pid: u32,
    webview_version: Result<String, String>,
    disable_hardware_acceleration: bool,
    webview_debug_enabled: bool,
    browser_args: &'a str,
    browser_executable_override: Option<&'a str>,
    user_data_override: Option<&'a str>,
    release_channel_override: Option<&'a str>,
    webview_log_path: Option<PathBuf>,
}

fn runtime_snapshot_from_inputs(inputs: RuntimeSnapshotInputs<'_>) -> RuntimeSnapshot {
    let (webview_version, webview_version_error) = match inputs.webview_version {
        Ok(version) => (Some(version), None),
        Err(error) => (None, Some(sanitize_diagnostic_text(&error))),
    };
    RuntimeSnapshot {
        app_version: inputs.app_version,
        app_pid: inputs.app_pid,
        webview_version,
        webview_version_error,
        disable_hardware_acceleration: inputs.disable_hardware_acceleration,
        webview_debug_enabled: inputs.webview_debug_enabled,
        safe_switches: summarize_switches(&tokenize_chromium_args(inputs.browser_args)),
        browser_executable_override_present: inputs.browser_executable_override.is_some(),
        user_data_override_present: inputs.user_data_override.is_some(),
        release_channel_override_present: inputs.release_channel_override.is_some(),
        webview_log_path: inputs.webview_log_path,
    }
}

pub(crate) fn initialize(start: ProcessStart, prefs: &crate::preferences::AppPreferences) {
    #[cfg(windows)]
    let (browser_args, webview_log_path) = {
        let requested = debug_requested(std::env::var(WEBVIEW_DEBUG_ENV).ok().as_deref());
        let internal_log_dir = crate::paths::codeg_logs_root().join(INTERNAL_LOG_DIR);
        let reserved_path = if requested {
            match reserve_internal_log(&internal_log_dir, std::process::id(), start.utc) {
                Ok(path) => Some(path),
                Err(error) => {
                    diagnostics_warn_once("reserve", &error.to_string());
                    None
                }
            }
        } else {
            None
        };

        let existing_args = std::env::var(WEBVIEW2_ENV).unwrap_or_default();
        let merged_args = merge_browser_args(
            &existing_args,
            prefs.disable_hardware_acceleration,
            reserved_path.as_deref(),
        );
        // SAFETY: Task 3 calls `initialize` before plugins, async workers, or
        // any other process helper can concurrently read the environment.
        unsafe {
            std::env::set_var(WEBVIEW2_ENV, &merged_args);
        }

        if let Some(path) = reserved_path.as_ref() {
            if let Err(error) = prune_internal_logs(&internal_log_dir, path) {
                diagnostics_warn_once("prune", &error.to_string());
            }
        }
        (merged_args, reserved_path)
    };

    #[cfg(not(windows))]
    let (browser_args, webview_log_path) = (String::new(), None);

    let webview_version = tauri::webview_version().map_err(|error| error.to_string());
    let snapshot = runtime_snapshot_from_inputs(RuntimeSnapshotInputs {
        app_version: env!("CARGO_PKG_VERSION"),
        app_pid: std::process::id(),
        webview_version,
        disable_hardware_acceleration: prefs.disable_hardware_acceleration,
        webview_debug_enabled: webview_log_path.is_some(),
        browser_args: &browser_args,
        browser_executable_override: std::env::var_os("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER")
            .as_ref()
            .map(|_| "present"),
        user_data_override: std::env::var_os("WEBVIEW2_USER_DATA_FOLDER")
            .as_ref()
            .map(|_| "present"),
        release_channel_override: std::env::var_os("WEBVIEW2_RELEASE_CHANNEL_PREFERENCE")
            .as_ref()
            .map(|_| "present"),
        webview_log_path,
    });
    let state = ProcessState {
        started_at: start.instant,
        snapshot,
    };
    if PROCESS_STATE.set(state).is_err() {
        diagnostics_warn_once("initialize", "window diagnostics already initialized");
        return;
    }

    emit_runtime_snapshot(&current_process_state().snapshot);
}

fn emit_runtime_snapshot(snapshot: &RuntimeSnapshot) {
    let webview_log_path = snapshot
        .webview_log_path
        .as_ref()
        .map(|path| path.display().to_string());
    tracing::info!(
        target: "codeg::window",
        event = "webview_runtime_snapshot",
        app_version = snapshot.app_version,
        app_pid = snapshot.app_pid,
        webview_version = snapshot.webview_version.as_deref(),
        webview_version_error = snapshot.webview_version_error.as_deref(),
        disable_hardware_acceleration = snapshot.disable_hardware_acceleration,
        webview_debug_enabled = snapshot.webview_debug_enabled,
        disable_gpu = snapshot.safe_switches.disable_gpu,
        enable_logging = snapshot.safe_switches.enable_logging,
        verbosity = snapshot.safe_switches.verbosity,
        log_file_present = snapshot.safe_switches.log_file_present,
        browser_executable_override_present = snapshot.browser_executable_override_present,
        user_data_override_present = snapshot.user_data_override_present,
        release_channel_override_present = snapshot.release_channel_override_present,
        webview_log_path = webview_log_path.as_deref(),
    );
}

fn diagnostics_warn_once(stage: &'static str, error: &str) {
    if DIAGNOSTICS_WARNING_EMITTED.swap(true, Ordering::Relaxed) {
        return;
    }
    let error = sanitize_diagnostic_text(error);
    tracing::warn!(
        target: "codeg::window",
        event = "webview_diagnostics_warning",
        stage,
        error,
    );
}

#[cfg(any(windows, test))]
fn reserve_internal_log(dir: &Path, pid: u32, stamp: DateTime<Utc>) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let timestamp = stamp.format("%Y%m%dT%H%M%SZ");
    let mut last_collision = None;

    for attempt in 0..LOG_RESERVATION_ATTEMPTS {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let path = dir.join(format!("webview2-{pid}-{timestamp}{suffix}.log"));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => {
                drop(file);
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "WebView2 internal log reservation exhausted",
        )
    }))
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalLogName {
    name: String,
    pid: u32,
}

#[cfg(any(windows, test))]
#[derive(Debug, Clone)]
struct RetentionCandidate {
    name: InternalLogName,
    modified_ms: u128,
}

#[cfg(any(windows, test))]
fn parse_internal_log_name(name: &str) -> Option<InternalLogName> {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        Regex::new(r"^webview2-([0-9]+)-[0-9]{8}T[0-9]{6}Z(?:-[0-9]+)?\.log$")
            .expect("valid internal log filename regex")
    });
    let captures = pattern.captures(name)?;
    Some(InternalLogName {
        name: name.to_string(),
        pid: captures.get(1)?.as_str().parse().ok()?,
    })
}

#[cfg(any(windows, test))]
fn retention_deletions(
    candidates: Vec<RetentionCandidate>,
    current_name: &str,
    pid_is_live: &dyn Fn(u32) -> Option<bool>,
) -> Vec<String> {
    let mut protected_count = 1usize;
    let mut dead = Vec::new();

    for candidate in candidates {
        if candidate.name.name == current_name {
            continue;
        }
        match pid_is_live(candidate.name.pid) {
            Some(false) => dead.push(candidate),
            Some(true) | None => protected_count += 1,
        }
    }

    dead.sort_by(|left, right| {
        right
            .modified_ms
            .cmp(&left.modified_ms)
            .then_with(|| left.name.name.cmp(&right.name.name))
    });
    let keep_dead = RETAINED_INTERNAL_LOGS_MAX.saturating_sub(protected_count);
    dead.into_iter()
        .skip(keep_dead)
        .map(|candidate| candidate.name.name)
        .collect()
}

#[cfg(windows)]
fn prune_internal_logs(dir: &Path, current_path: &Path) -> io::Result<()> {
    let current_name = current_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid current log name"))?;
    let mut candidates = Vec::new();

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(parsed) = parse_internal_log_name(&name) else {
            continue;
        };
        let modified_ms = entry
            .metadata()?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        candidates.push(RetentionCandidate {
            name: parsed,
            modified_ms,
        });
    }

    for name in retention_deletions(candidates, current_name, &pid_is_live) {
        std::fs::remove_file(dir.join(name))?;
    }
    Ok(())
}

#[cfg(windows)]
fn pid_is_live(pid: u32) -> Option<bool> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return Some(false);
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        let error = unsafe { GetLastError() };
        return (error == ERROR_INVALID_PARAMETER).then_some(false);
    }

    let mut exit_code = 0;
    let read_ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    if read_ok == 0 {
        None
    } else {
        Some(exit_code == STILL_ACTIVE as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::fmt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct SyntheticError {
        message: String,
        source: Option<Box<SyntheticError>>,
    }

    impl fmt::Display for SyntheticError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.message)
        }
    }

    impl Error for SyntheticError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.source
                .as_deref()
                .map(|source| source as &(dyn Error + 'static))
        }
    }

    fn synthetic_error_with_source(
        message: &str,
        source_message: &str,
        suffix: String,
    ) -> SyntheticError {
        SyntheticError {
            message: format!("{message} {suffix}"),
            source: Some(Box::new(SyntheticError {
                message: source_message.to_string(),
                source: None,
            })),
        }
    }

    #[test]
    fn error_projection_is_bounded_redacted_and_classified() {
        let error = synthetic_error_with_source(
            "open \"C:\\Users\\Alice\\Secret Folder\\token.txt\" and /home/alice/token.txt\nhttps://example.test/private?token=secret ?credential=hunter2\t",
            "WebView2 error: WindowsError(Error { code: HRESULT(0x80010108) })",
            "payload".repeat(100),
        );
        let projected = project_error(&error);

        assert_eq!(projected.failure_kind, "rpc_disconnected");
        assert_eq!(projected.error_hresult.as_deref(), Some("0x80010108"));
        assert!(projected.error_message.chars().count() <= 240);
        assert!(!projected.error_message.chars().any(char::is_control));
        for secret in ["Alice", "token.txt", "example.test", "secret", "hunter2"] {
            assert!(!projected.error_message.contains(secret), "leaked {secret}");
        }
        assert!(projected.error_message.contains("<path>"));
        assert!(projected.error_message.contains("<url>"));
        assert!(projected.error_message.contains("<query>"));
    }

    #[test]
    fn error_projection_extracts_labeled_hresult_forms_from_source_chain() {
        let cases = [
            "HRESULT(0X80010108)",
            "HRESULT 0x80010108",
            "HRESULT ( 0x80010108 )",
        ];

        for source_message in cases {
            let error = synthetic_error_with_source("outer", source_message, String::new());
            assert_eq!(extract_hresult(&error), Some(0x80010108));
            let projected = project_error(&error);
            assert_eq!(projected.failure_kind, "rpc_disconnected");
            assert_eq!(projected.error_hresult.as_deref(), Some("0x80010108"));
        }
    }

    #[test]
    fn error_projection_normalizes_unknown_hresult_and_ignores_unlabeled_hex() {
        for labeled in ["HRESULT(0xDEADBEEF)", "HRESULT 0xdeadbeef"] {
            let error = SyntheticError {
                message: labeled.to_string(),
                source: None,
            };
            let projected = project_error(&error);
            assert_eq!(projected.failure_kind, "unknown");
            assert_eq!(projected.error_hresult.as_deref(), Some("0xdeadbeef"));
        }

        for message in ["raw 0x80010108", "no HRESULT here"] {
            let error = SyntheticError {
                message: message.to_string(),
                source: None,
            };
            let projected = project_error(&error);
            assert_eq!(projected.failure_kind, "unknown");
            assert_eq!(projected.error_hresult, None);
        }
        assert_eq!(classify_hresult(None), "unknown");
        assert_eq!(classify_hresult(Some(0xdeadbeef)), "unknown");
    }

    #[test]
    fn error_projection_redacts_path_and_url_variants() {
        let message = "\"C:\\Users\\Alice\\Secret Folder\\quoted.txt\" C:\\Users\\Bob\\plain.txt '\\\\server\\private share\\quoted.txt' \\\\server\\share\\plain.txt /home/carol/private.txt file:///C:/Users/Dan/token.txt http://example.test/private?key=value ?credential=hunter2";
        let error = SyntheticError {
            message: message.to_string(),
            source: None,
        };
        let projected = project_error(&error);

        assert_eq!(projected.failure_kind, "unknown");
        assert_eq!(projected.error_hresult, None);
        assert!(projected.error_message.chars().count() <= 240);
        assert!(!projected.error_message.chars().any(char::is_control));
        assert!(projected.error_message.contains("<path>"));
        assert!(projected.error_message.contains("<url>"));
        assert!(projected.error_message.contains("<query>"));
        for secret in [
            "Alice",
            "Bob",
            "server",
            "carol",
            "Dan",
            "example.test",
            "hunter2",
            "quoted.txt",
            "plain.txt",
            "private.txt",
            "token.txt",
        ] {
            assert!(!projected.error_message.contains(secret), "leaked {secret}");
        }
    }

    #[test]
    fn error_projection_replaces_adjacent_controls_and_caps_multibyte_unicode() {
        let error = SyntheticError {
            message: format!("before\u{0001}\u{0002}after {}", "界".repeat(300)),
            source: None,
        };
        let projected = project_error(&error);

        assert_eq!(projected.error_message.chars().count(), 240);
        assert!(projected.error_message.contains("before  after"));
        assert!(!projected.error_message.chars().any(char::is_control));
    }

    #[derive(Debug)]
    struct MarkedSyntheticError {
        marker: Arc<str>,
    }

    impl MarkedSyntheticError {
        fn new(marker: Arc<str>) -> Self {
            Self { marker }
        }

        fn marker(&self) -> &Arc<str> {
            &self.marker
        }
    }

    impl fmt::Display for MarkedSyntheticError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("marked error HRESULT(0x80010108)")
        }
    }

    impl Error for MarkedSyntheticError {}

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<DiagnosticEvent>>,
    }

    impl RecordingSink {
        fn events(&self) -> Vec<DiagnosticEvent> {
            self.events.lock().unwrap().clone()
        }

        fn terminal_names(&self) -> Vec<&'static str> {
            self.events()
                .into_iter()
                .filter(|event| event.is_terminal())
                .map(|event| event.event_name())
                .collect()
        }

        fn clear(&self) {
            self.events.lock().unwrap().clear();
        }
    }

    impl DiagnosticSink for RecordingSink {
        fn emit(&self, event: DiagnosticEvent) -> Result<(), String> {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailingSink {
        event_names: Mutex<Vec<&'static str>>,
    }

    impl FailingSink {
        fn terminal_names(&self) -> Vec<&'static str> {
            self.event_names
                .lock()
                .unwrap()
                .iter()
                .copied()
                .filter(|name| *name != "window_create_started")
                .collect()
        }
    }

    impl DiagnosticSink for FailingSink {
        fn emit(&self, event: DiagnosticEvent) -> Result<(), String> {
            self.event_names.lock().unwrap().push(event.event_name());
            Err("synthetic sink failure".to_string())
        }
    }

    fn test_attempt_context<'a>(sink: &'a dyn DiagnosticSink) -> AttemptContext<'a> {
        AttemptContext {
            sink,
            window_kind: WindowKind::Settings,
            window_label: "settings".to_string(),
            caller_label: Some("main".to_string()),
            operation_id: Some("operation-42".to_string()),
            app_pid: 4242,
            app_uptime_ms: 9_001,
            registered_window_labels: vec!["settings".to_string(), "main".to_string()],
            failure_runtime: FailureRuntimeContext {
                webview_version: Some("151.0.4129.59".to_string()),
                safe_switches: SafeSwitchState {
                    disable_gpu: true,
                    enable_logging: true,
                    verbosity: Some(1),
                    log_file_present: true,
                },
            },
        }
    }

    #[test]
    fn window_kind_uses_the_exact_allowlisted_serialized_values() {
        let cases = [
            (WindowKind::Main, "main"),
            (WindowKind::ConversationPopout, "conversation_popout"),
            (WindowKind::Settings, "settings"),
            (WindowKind::ImportSessions, "import_sessions"),
            (WindowKind::Commit, "commit"),
            (WindowKind::Merge, "merge"),
            (WindowKind::Stash, "stash"),
            (WindowKind::Push, "push"),
            (WindowKind::ProjectBoot, "project_boot"),
            (WindowKind::Pet, "pet"),
            (WindowKind::PetPanel, "pet_panel"),
            (WindowKind::RemoteWorkspace, "remote_workspace"),
        ];

        for (kind, expected) in cases {
            assert_eq!(kind.as_str(), expected);
        }
    }

    #[test]
    fn attempt_ids_are_formatted_and_increase_monotonically() {
        assert_eq!(format_attempt_id(4242, 1), "window-4242-1");
        let first = next_attempt_id(4242);
        let second = next_attempt_id(4242);
        let sequence = |attempt_id: &str| {
            attempt_id
                .rsplit('-')
                .next()
                .unwrap()
                .parse::<u64>()
                .unwrap()
        };
        assert!(sequence(&second) > sequence(&first));
    }

    #[test]
    fn registered_window_labels_are_sorted_counted_and_capped() {
        let labels = (0..20)
            .rev()
            .map(|index| format!("window-{index:02}"))
            .collect::<Vec<_>>();
        let snapshot = snapshot_registered_window_labels(labels);

        assert_eq!(snapshot.count, 20);
        assert_eq!(
            snapshot.labels,
            (0..16)
                .map(|index| format!("window-{index:02}"))
                .collect::<Vec<_>>()
        );
        assert!(snapshot.truncated);

        let exactly_sixteen = snapshot_registered_window_labels(
            (0..16).rev().map(|index| format!("window-{index:02}")),
        );
        assert_eq!(exactly_sixteen.count, 16);
        assert_eq!(exactly_sixteen.labels.len(), 16);
        assert!(!exactly_sixteen.truncated);
    }

    #[test]
    fn attempt_returns_original_results_and_one_terminal_event() {
        let sink = RecordingSink::default();
        let context = test_attempt_context(&sink);
        let value =
            run_build_for_test(context.clone(), || Ok::<_, MarkedSyntheticError>(37)).unwrap();
        assert_eq!(value, 37);
        assert_eq!(sink.terminal_names(), vec!["window_create_succeeded"]);

        sink.clear();
        let marker: Arc<str> = Arc::from("original-error");
        let returned = run_build_for_test(context, || {
            Err::<(), _>(MarkedSyntheticError::new(marker.clone()))
        })
        .unwrap_err();
        assert!(Arc::ptr_eq(returned.marker(), &marker));
        assert_eq!(sink.terminal_names(), vec!["window_create_failed"]);
    }

    #[test]
    fn dropping_unfinished_attempt_emits_abandoned_once() {
        let sink = RecordingSink::default();
        {
            let _attempt = WindowCreationAttempt::begin_for_test(test_attempt_context(&sink));
        }
        assert_eq!(sink.terminal_names(), vec!["window_create_abandoned"]);
    }

    #[test]
    fn explicitly_completed_attempt_does_not_emit_abandoned_on_drop() {
        let sink = RecordingSink::default();
        {
            let mut attempt = WindowCreationAttempt::begin_for_test(test_attempt_context(&sink));
            attempt.finish_success();
            attempt.finish_success();
        }
        assert_eq!(sink.terminal_names(), vec!["window_create_succeeded"]);
    }

    #[test]
    fn failing_sink_cannot_change_results_or_add_an_abandoned_terminal() {
        let sink = FailingSink::default();
        let context = test_attempt_context(&sink);
        let value =
            run_build_for_test(context.clone(), || Ok::<_, MarkedSyntheticError>(37)).unwrap();
        assert_eq!(value, 37);

        let marker: Arc<str> = Arc::from("original-error");
        let returned = run_build_for_test(context, || {
            Err::<(), _>(MarkedSyntheticError::new(marker.clone()))
        })
        .unwrap_err();
        assert!(Arc::ptr_eq(returned.marker(), &marker));
        assert_eq!(
            sink.terminal_names(),
            vec!["window_create_succeeded", "window_create_failed"]
        );
    }

    #[test]
    fn started_and_failed_events_share_identity_and_safe_runtime_context() {
        let sink = RecordingSink::default();
        let snapshot = runtime_snapshot_from_inputs(RuntimeSnapshotInputs {
            app_version: "0.22.2-test",
            app_pid: 4242,
            webview_version: Ok("151.0.4129.59".to_string()),
            disable_hardware_acceleration: true,
            webview_debug_enabled: true,
            browser_args: r#"--disable-gpu --enable-logging --v=1 --log-file="C:\Secret Logs\owned.log" --unrelated=browser-secret"#,
            browser_executable_override: None,
            user_data_override: None,
            release_channel_override: None,
            webview_log_path: Some(PathBuf::from(r"C:\Secret Logs\webview2-internal\owned.log")),
        });
        let mut context = test_attempt_context(&sink);
        context.failure_runtime = failure_runtime_context(&snapshot);

        let marker: Arc<str> = Arc::from("original-error");
        let returned = run_build_for_test(context, || {
            Err::<(), _>(MarkedSyntheticError::new(marker.clone()))
        })
        .unwrap_err();
        assert!(Arc::ptr_eq(returned.marker(), &marker));

        let events = sink.events();
        assert_eq!(events.len(), 2);
        let DiagnosticEvent::Started(started) = &events[0] else {
            panic!("expected started event")
        };
        let DiagnosticEvent::Failed {
            identity,
            error,
            webview_version,
            safe_switches,
            ..
        } = &events[1]
        else {
            panic!("expected failed event")
        };
        assert_eq!(identity, started);
        assert_eq!(error.failure_kind, "rpc_disconnected");
        assert_eq!(webview_version.as_deref(), Some("151.0.4129.59"));
        assert_eq!(
            safe_switches,
            &SafeSwitchState {
                disable_gpu: true,
                enable_logging: true,
                verbosity: Some(1),
                log_file_present: true,
            }
        );
        let rendered = format!("{:?}", events[1]);
        for secret in ["Secret Logs", "webview2-internal", "browser-secret"] {
            assert!(!rendered.contains(secret), "leaked {secret}");
        }
    }

    #[test]
    fn success_event_contains_only_the_minimal_correlation_fields() {
        let sink = RecordingSink::default();
        run_build_for_test(test_attempt_context(&sink), || {
            Ok::<_, MarkedSyntheticError>(())
        })
        .unwrap();

        let events = sink.events();
        let DiagnosticEvent::Started(started) = &events[0] else {
            panic!("expected started event")
        };
        let DiagnosticEvent::Succeeded {
            attempt_id,
            window_kind,
            window_label,
            elapsed_ms,
        } = &events[1]
        else {
            panic!("expected succeeded event")
        };
        assert_eq!(attempt_id, &started.attempt_id);
        assert_eq!(*window_kind, started.window_kind);
        assert_eq!(window_label, &started.window_label);
        assert!(*elapsed_ms < 60_000);
    }

    #[test]
    fn debug_env_accepts_only_one_and_true() {
        assert!(debug_requested(Some("1")));
        assert!(debug_requested(Some(" TRUE ")));
        assert!(!debug_requested(Some("yes")));
        assert!(!debug_requested(Some("0")));
        assert!(!debug_requested(None));
    }

    #[test]
    fn merge_browser_args_owns_logging_switches_only_when_effective() {
        let log_path = Path::new(r"C:\Program Data\DrawCode\webview2.log");
        let input = r#"--foo "value with spaces" --v 2 --log-file="C:\old path\old.log" --enable-logging --bar=tail\"#;
        let merged = merge_browser_args(input, true, Some(log_path));
        let tokens = tokenize_chromium_args(&merged);

        assert_owned_debug_switches(&tokens, log_path);
        assert_eq!(
            tokens
                .iter()
                .filter(|value| *value == "--disable-gpu")
                .count(),
            1
        );
        assert_eq!(
            unrelated_tokens(&tokens),
            vec!["--foo", "value with spaces", "--bar=tail\\"]
        );
    }

    #[test]
    fn disabled_debug_preserves_caller_logging_tokens() {
        let input = r#"--v 2 --log-file "C:\caller path\caller.log" --enable-logging"#;
        let merged = merge_browser_args(input, false, None);
        assert_eq!(
            tokenize_chromium_args(&merged),
            tokenize_chromium_args(input)
        );
    }

    #[test]
    fn chromium_args_round_trip_required_quoting_cases() {
        let cases = [
            ("quoted value", r#"--foo "value with spaces""#),
            ("embedded quote", r#"--foo "value \"inside\" tail""#),
            ("empty quoted token", r#"--foo "" --bar"#),
            ("quoted one trailing slash", r#"--foo "one slash\\""#),
            ("quoted two trailing slashes", r#"--foo "two slashes\\\\""#),
        ];

        for (name, input) in cases {
            let parsed = tokenize_chromium_args(input);
            let serialized = serialize_chromium_args(&parsed);
            assert_eq!(
                tokenize_chromium_args(&serialized),
                parsed,
                "round trip failed for {name}"
            );
        }
    }

    #[test]
    fn debug_merge_table_preserves_unrelated_order_and_owns_switches() {
        let log_path = Path::new(r"C:\Program Data\DrawCode\webview2.log");
        let cases = [
            ("equals verbosity", r#"--before --v=2 --after"#),
            ("separate verbosity", r#"--before --v 2 --after"#),
            ("equals log file", r#"--before --log-file=old.log --after"#),
            (
                "separate log file",
                r#"--before --log-file old.log --after"#,
            ),
            (
                "duplicates",
                r#"--before --enable-logging --enable-logging --v=2 --v 3 --log-file=old.log --log-file older.log --after"#,
            ),
            (
                "quoted value",
                r#"--before --foo "value with spaces" --after"#,
            ),
            (
                "embedded quote",
                r#"--before --foo "value \"inside\" tail" --after"#,
            ),
            ("empty quoted token", r#"--before "" --after"#),
            (
                "quoted one trailing slash",
                r#"--before "one slash\\" --after"#,
            ),
            (
                "quoted two trailing slashes",
                r#"--before "two slashes\\\\" --after"#,
            ),
        ];

        for (name, input) in cases {
            let original = tokenize_chromium_args(input);
            let expected_unrelated = remove_logging_switches(&original);
            let merged = tokenize_chromium_args(&merge_browser_args(input, false, Some(log_path)));

            assert_owned_debug_switches(&merged, log_path);
            assert_eq!(
                unrelated_tokens(&merged),
                expected_unrelated,
                "unrelated tokens changed for {name}"
            );
        }
    }

    #[test]
    fn safe_switch_summary_ignores_arbitrary_argument_values() {
        let tokens = tokenize_chromium_args(
            r#"--disable-gpu --enable-logging --v 7 --log-file="C:\owned.log" --secret=hunter2"#,
        );

        assert_eq!(
            summarize_switches(&tokens),
            SafeSwitchState {
                disable_gpu: true,
                enable_logging: true,
                verbosity: Some(7),
                log_file_present: true,
            }
        );
    }

    #[test]
    fn startup_error_sanitizer_redacts_before_bounding() {
        let input = format!(
            "open \"C:\\Users\\Alice\\Secret Folder\\token.txt\" and /home/alice/token.txt\nhttps://example.test/private?token=secret ?credential=hunter2 {}",
            "界".repeat(300)
        );
        let sanitized = sanitize_diagnostic_text(&input);

        assert!(sanitized.chars().count() <= 240);
        assert!(!sanitized.chars().any(char::is_control));
        for secret in ["Alice", "token.txt", "example.test", "secret", "hunter2"] {
            assert!(!sanitized.contains(secret), "leaked {secret}");
        }
        assert!(sanitized.contains("<path>"));
        assert!(sanitized.contains("<url>"));
        assert!(sanitized.contains("<query>"));
    }

    #[test]
    fn startup_error_sanitizer_redacts_punctuation_prefixed_posix_paths() {
        let sanitized = sanitize_diagnostic_text(
            r"path=/home/alice/private-repository/token.txt runtime:/srv/codeg/private/runtime.db windows=C:\Users\Bob\Secret\token.txt",
        );

        assert!(sanitized.contains("path=<path>"), "sanitized={sanitized}");
        assert!(
            sanitized.contains("runtime:<path>"),
            "sanitized={sanitized}"
        );
        assert!(
            sanitized.contains("windows=<path>"),
            "sanitized={sanitized}"
        );
        for secret in ["alice", "private-repository", "codeg", "runtime.db", "Bob"] {
            assert!(!sanitized.contains(secret), "leaked {secret}");
        }
    }

    #[test]
    fn runtime_snapshot_projects_only_safe_startup_fields() {
        let log_path = PathBuf::from(r"C:\DrawCode Logs\webview2-internal\owned.log");
        let snapshot = runtime_snapshot_from_inputs(RuntimeSnapshotInputs {
            app_version: "0.22.2-test",
            app_pid: 42,
            webview_version: Ok("151.0.4129.59".to_string()),
            disable_hardware_acceleration: true,
            webview_debug_enabled: true,
            browser_args: r#"--disable-gpu --enable-logging --v=1 --log-file="C:\DrawCode Logs\webview2-internal\owned.log" --unrelated=browser-secret"#,
            browser_executable_override: Some(r"C:\Users\Alice\Secret Runtime"),
            user_data_override: Some(r"C:\Users\Alice\Secret Profile"),
            release_channel_override: Some("secret-channel"),
            webview_log_path: Some(log_path.clone()),
        });

        assert_eq!(snapshot.app_version, "0.22.2-test");
        assert_eq!(snapshot.app_pid, 42);
        assert_eq!(snapshot.webview_version.as_deref(), Some("151.0.4129.59"));
        assert_eq!(snapshot.webview_version_error, None);
        assert!(snapshot.disable_hardware_acceleration);
        assert!(snapshot.webview_debug_enabled);
        assert_eq!(
            snapshot.safe_switches,
            SafeSwitchState {
                disable_gpu: true,
                enable_logging: true,
                verbosity: Some(1),
                log_file_present: true,
            }
        );
        assert!(snapshot.browser_executable_override_present);
        assert!(snapshot.user_data_override_present);
        assert!(snapshot.release_channel_override_present);
        assert_eq!(snapshot.webview_log_path, Some(log_path));

        let rendered = format!("{snapshot:?}");
        for secret in [
            "Alice",
            "browser-secret",
            "secret-channel",
            "Secret Profile",
        ] {
            assert!(!rendered.contains(secret), "leaked {secret}");
        }
    }

    #[test]
    fn runtime_snapshot_sanitizes_webview_version_errors() {
        let snapshot = runtime_snapshot_from_inputs(RuntimeSnapshotInputs {
            app_version: "0.22.2-test",
            app_pid: 42,
            webview_version: Err(
                r#"failed at "C:\Users\Alice\runtime" https://example.test/?token=secret"#
                    .to_string(),
            ),
            disable_hardware_acceleration: false,
            webview_debug_enabled: false,
            browser_args: "",
            browser_executable_override: None,
            user_data_override: None,
            release_channel_override: None,
            webview_log_path: None,
        });

        assert_eq!(snapshot.webview_version, None);
        let error = snapshot.webview_version_error.unwrap();
        assert!(error.contains("<path>"));
        assert!(error.contains("<url>"));
        assert!(!error.contains("Alice"));
        assert!(!error.contains("secret"));
    }

    #[test]
    fn reserve_internal_log_uses_collision_suffix_without_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = DateTime::parse_from_rfc3339("2026-08-08T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc);
        std::fs::write(dir.path().join("webview2-42-20260808T010203Z.log"), b"old").unwrap();

        let path = reserve_internal_log(dir.path(), 42, stamp).unwrap();
        assert_eq!(
            path.file_name().unwrap(),
            "webview2-42-20260808T010203Z-1.log"
        );
        assert_eq!(
            std::fs::read(dir.path().join("webview2-42-20260808T010203Z.log")).unwrap(),
            b"old"
        );
    }

    #[test]
    fn reserve_internal_log_stops_after_32_create_new_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let stamp = DateTime::parse_from_rfc3339("2026-08-08T01:02:03Z")
            .unwrap()
            .with_timezone(&Utc);
        for suffix in 0..LOG_RESERVATION_ATTEMPTS {
            let suffix = if suffix == 0 {
                String::new()
            } else {
                format!("-{suffix}")
            };
            std::fs::write(
                dir.path()
                    .join(format!("webview2-42-20260808T010203Z{suffix}.log")),
                b"old",
            )
            .unwrap();
        }

        let error = reserve_internal_log(dir.path(), 42, stamp).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(!dir
            .path()
            .join("webview2-42-20260808T010203Z-32.log")
            .exists());
    }

    #[test]
    fn strict_internal_name_parser_rejects_near_matches() {
        assert!(parse_internal_log_name("webview2-42-20260808T010203Z.log").is_some());
        assert!(parse_internal_log_name("webview2-42-20260808T010203Z-7.log").is_some());
        assert!(parse_internal_log_name("prefix-webview2-42-20260808T010203Z.log").is_none());
        assert!(parse_internal_log_name("webview2-x-20260808T010203Z.log").is_none());
        assert!(parse_internal_log_name("webview2-42-2026-08-08.log").is_none());
        assert!(parse_internal_log_name("webview2-42-20260808T010203Z.log.bak").is_none());
    }

    #[test]
    fn retention_protects_current_live_and_unknown_and_keeps_newest_dead() {
        let current = "webview2-99-20260808T010203Z.log";
        let candidates = vec![
            retention_candidate(current, 99, 100),
            retention_candidate("webview2-1-20260808T010201Z.log", 1, 90),
            retention_candidate("webview2-2-20260808T010200Z.log", 2, 80),
            retention_candidate("webview2-3-20260808T010159Z.log", 3, 70),
            retention_candidate("webview2-4-20260808T010158Z.log", 4, 60),
            retention_candidate("webview2-5-20260808T010157Z.log", 5, 50),
        ];

        let deleted = retention_deletions(candidates, current, &|pid| match pid {
            1 => Some(true),
            2 => None,
            3..=5 => Some(false),
            99 => panic!("current PID must not be probed"),
            _ => unreachable!(),
        });

        assert_eq!(deleted, vec!["webview2-5-20260808T010157Z.log".to_string()]);
    }

    #[test]
    fn retention_allows_live_files_to_exceed_cap_and_deletes_only_dead() {
        let current = "webview2-99-20260808T010203Z.log";
        let mut candidates = vec![retention_candidate(current, 99, 100)];
        for pid in 1..=6 {
            candidates.push(retention_candidate(
                &format!("webview2-{pid}-20260808T01020{pid}Z.log"),
                pid,
                pid as u128,
            ));
        }
        candidates.push(retention_candidate(
            "webview2-7-20260808T010159Z.log",
            7,
            200,
        ));

        let deleted = retention_deletions(candidates, current, &|pid| match pid {
            1..=6 => Some(true),
            7 => Some(false),
            99 => panic!("current PID must not be probed"),
            _ => unreachable!(),
        });

        assert_eq!(deleted, vec!["webview2-7-20260808T010159Z.log".to_string()]);
    }

    #[test]
    fn retention_filters_nonmatches_and_reused_live_pid_protects_every_file() {
        let current = "webview2-99-20260808T010203Z.log";
        let raw = [
            current,
            "webview2-42-20260808T010201Z.log",
            "webview2-42-20260808T010202Z-1.log",
            "prefix-webview2-7-20260808T010159Z.log",
            "webview2-7-20260808T010159Z.log.bak",
        ];
        let candidates: Vec<_> = raw
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                parse_internal_log_name(name).map(|parsed| RetentionCandidate {
                    name: parsed,
                    modified_ms: index as u128,
                })
            })
            .collect();

        assert_eq!(candidates.len(), 3);
        assert!(retention_deletions(candidates, current, &|pid| match pid {
            42 => Some(true),
            99 => panic!("current PID must not be probed"),
            _ => Some(false),
        })
        .is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn windows_pid_probe_reports_definitive_live_and_dead_cases() {
        assert_eq!(pid_is_live(std::process::id()), Some(true));
        assert_eq!(pid_is_live(0), Some(false));
    }

    fn retention_candidate(name: &str, pid: u32, modified_ms: u128) -> RetentionCandidate {
        RetentionCandidate {
            name: InternalLogName {
                name: name.to_string(),
                pid,
            },
            modified_ms,
        }
    }

    fn assert_owned_debug_switches(tokens: &[String], log_path: &Path) {
        assert_eq!(
            tokens
                .iter()
                .filter(|value| *value == "--enable-logging")
                .count(),
            1
        );
        assert_eq!(
            tokens
                .iter()
                .filter(|value| value.starts_with("--v="))
                .count(),
            1
        );
        assert_eq!(
            tokens
                .iter()
                .filter(|value| value.starts_with("--log-file="))
                .count(),
            1
        );
        assert!(tokens.contains(&"--v=1".to_string()));
        assert!(tokens.contains(&format!("--log-file={}", log_path.display())));
    }

    fn unrelated_tokens(tokens: &[String]) -> Vec<String> {
        tokens
            .iter()
            .filter(|value| {
                !value.starts_with("--v=")
                    && !value.starts_with("--log-file=")
                    && *value != "--enable-logging"
                    && *value != "--disable-gpu"
            })
            .cloned()
            .collect()
    }

    fn remove_logging_switches(tokens: &[String]) -> Vec<String> {
        let mut unrelated = Vec::new();
        let mut index = 0;
        while index < tokens.len() {
            let token = &tokens[index];
            if token == "--enable-logging"
                || token.starts_with("--v=")
                || token.starts_with("--log-file=")
            {
                index += 1;
            } else if token == "--v" || token == "--log-file" {
                index += 2;
            } else {
                unrelated.push(token.clone());
                index += 1;
            }
        }
        unrelated
    }

    #[test]
    fn durable_json_omits_absent_optionals_and_records_typed_present_values() {
        use std::io::{self, Write};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct Buf(Arc<Mutex<Vec<u8>>>);

        impl Write for Buf {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for Buf {
            type Writer = Buf;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        fn parse_event_fields(line: &str) -> serde_json::Map<String, serde_json::Value> {
            let root: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("expected JSON log line, got {line:?}: {error}"));
            root.get("fields")
                .and_then(|value| value.as_object())
                .cloned()
                .unwrap_or_else(|| panic!("expected fields object in {line}"))
        }

        fn assert_absent(fields: &serde_json::Map<String, serde_json::Value>, key: &str) {
            assert!(
                !fields.contains_key(key),
                "expected {key} absent, got {fields:?}"
            );
        }

        fn assert_string(
            fields: &serde_json::Map<String, serde_json::Value>,
            key: &str,
            expected: &str,
        ) {
            match fields.get(key) {
                Some(serde_json::Value::String(value)) => {
                    assert_eq!(value, expected, "field {key}");
                }
                other => panic!("expected string {key}={expected:?}, got {other:?}"),
            }
        }

        fn assert_u64(
            fields: &serde_json::Map<String, serde_json::Value>,
            key: &str,
            expected: u64,
        ) {
            match fields.get(key) {
                Some(serde_json::Value::Number(value)) => {
                    assert_eq!(value.as_u64(), Some(expected), "field {key}");
                }
                other => panic!("expected number {key}={expected}, got {other:?}"),
            }
        }

        let identity_absent = WindowIdentity {
            attempt_id: "wca-1".to_string(),
            window_kind: WindowKind::Main,
            window_label: "main".to_string(),
            caller_label: None,
            operation_id: None,
            app_pid: 11,
            app_uptime_ms: 22,
            registered_window_count: 0,
            registered_window_labels: Vec::new(),
            registered_window_labels_truncated: false,
        };
        let identity_present = WindowIdentity {
            attempt_id: "wca-2".to_string(),
            window_kind: WindowKind::ConversationPopout,
            window_label: "conversation-popout-7".to_string(),
            caller_label: Some("main".to_string()),
            operation_id: Some("operation-7".to_string()),
            app_pid: 11,
            app_uptime_ms: 33,
            registered_window_count: 1,
            registered_window_labels: vec!["main".to_string()],
            registered_window_labels_truncated: false,
        };

        let buf = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_writer(Buf(buf.clone()))
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        TracingDiagnosticSink
            .emit(DiagnosticEvent::Started(identity_absent.clone()))
            .unwrap();
        TracingDiagnosticSink
            .emit(DiagnosticEvent::Started(identity_present.clone()))
            .unwrap();
        TracingDiagnosticSink
            .emit(DiagnosticEvent::Failed {
                identity: identity_present.clone(),
                elapsed_ms: 12,
                error: ErrorProjection {
                    failure_kind: "rpc_disconnected",
                    error_hresult: Some("0x80010108".to_string()),
                    error_message: "rpc disconnected".to_string(),
                },
                webview_version: Some("151.0.4129.59".to_string()),
                safe_switches: SafeSwitchState {
                    disable_gpu: true,
                    enable_logging: true,
                    verbosity: Some(1),
                    log_file_present: true,
                },
            })
            .unwrap();
        TracingDiagnosticSink
            .emit(DiagnosticEvent::Failed {
                identity: identity_absent.clone(),
                elapsed_ms: 8,
                error: ErrorProjection {
                    failure_kind: "unknown",
                    error_hresult: None,
                    error_message: "generic failure".to_string(),
                },
                webview_version: None,
                safe_switches: SafeSwitchState {
                    disable_gpu: false,
                    enable_logging: false,
                    verbosity: None,
                    log_file_present: false,
                },
            })
            .unwrap();
        emit_runtime_snapshot(&runtime_snapshot_from_inputs(RuntimeSnapshotInputs {
            app_version: "0.22.2-test",
            app_pid: 42,
            webview_version: Ok("151.0.4129.59".to_string()),
            disable_hardware_acceleration: true,
            webview_debug_enabled: true,
            browser_args: "--disable-gpu --enable-logging --v=1",
            browser_executable_override: None,
            user_data_override: None,
            release_channel_override: None,
            webview_log_path: Some(PathBuf::from(
                r"C:\DrawCode Logs\webview2-internal\owned.log",
            )),
        }));
        emit_runtime_snapshot(&runtime_snapshot_from_inputs(RuntimeSnapshotInputs {
            app_version: "0.22.2-test",
            app_pid: 42,
            webview_version: Err("unavailable".to_string()),
            disable_hardware_acceleration: false,
            webview_debug_enabled: false,
            browser_args: "",
            browser_executable_override: None,
            user_data_override: None,
            release_channel_override: None,
            webview_log_path: None,
        }));
        drop(_guard);

        let output = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = output
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(lines.len(), 6, "unexpected capture: {output}");

        let started_absent = parse_event_fields(lines[0]);
        assert_eq!(
            started_absent.get("event").and_then(|v| v.as_str()),
            Some("window_create_started")
        );
        assert_absent(&started_absent, "caller_label");
        assert_absent(&started_absent, "operation_id");

        let started_present = parse_event_fields(lines[1]);
        assert_string(&started_present, "caller_label", "main");
        assert_string(&started_present, "operation_id", "operation-7");
        assert!(
            !matches!(
                started_present.get("caller_label"),
                Some(serde_json::Value::String(value)) if value.starts_with("Some(")
            ),
            "caller_label must not be Debug-wrapped: {started_present:?}"
        );

        let failed_present = parse_event_fields(lines[2]);
        assert_eq!(
            failed_present.get("event").and_then(|v| v.as_str()),
            Some("window_create_failed")
        );
        assert_string(&failed_present, "caller_label", "main");
        assert_string(&failed_present, "operation_id", "operation-7");
        assert_string(&failed_present, "error_hresult", "0x80010108");
        assert_string(&failed_present, "webview_version", "151.0.4129.59");
        assert_u64(&failed_present, "verbosity", 1);

        let failed_absent = parse_event_fields(lines[3]);
        assert_absent(&failed_absent, "caller_label");
        assert_absent(&failed_absent, "operation_id");
        assert_absent(&failed_absent, "error_hresult");
        assert_absent(&failed_absent, "webview_version");
        assert_absent(&failed_absent, "verbosity");

        let enabled_snapshot = parse_event_fields(lines[4]);
        assert_eq!(
            enabled_snapshot.get("event").and_then(|v| v.as_str()),
            Some("webview_runtime_snapshot")
        );
        assert_string(&enabled_snapshot, "webview_version", "151.0.4129.59");
        assert_absent(&enabled_snapshot, "webview_version_error");
        assert_u64(&enabled_snapshot, "verbosity", 1);
        assert_string(
            &enabled_snapshot,
            "webview_log_path",
            r"C:\DrawCode Logs\webview2-internal\owned.log",
        );

        let disabled_snapshot = parse_event_fields(lines[5]);
        assert_eq!(
            disabled_snapshot.get("event").and_then(|v| v.as_str()),
            Some("webview_runtime_snapshot")
        );
        assert_absent(&disabled_snapshot, "webview_version");
        assert_string(&disabled_snapshot, "webview_version_error", "unavailable");
        assert_absent(&disabled_snapshot, "verbosity");
        assert_absent(&disabled_snapshot, "webview_log_path");
    }
}
