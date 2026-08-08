use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use chrono::{DateTime, Utc};
use regex::Regex;

pub(crate) const REGISTERED_WINDOW_LABELS_MAX: usize = 16;
const WEBVIEW2_ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
const WEBVIEW_DEBUG_ENV: &str = "CODEG_WEBVIEW_DEBUG";
const INTERNAL_LOG_DIR: &str = "webview2-internal";
const LOG_RESERVATION_ATTEMPTS: u32 = 32;
const RETAINED_INTERNAL_LOGS_MAX: usize = 5;
const DIAGNOSTIC_TEXT_MAX_CHARS: usize = 240;

pub(crate) struct ProcessStart {
    instant: Instant,
    utc: DateTime<Utc>,
}

impl ProcessStart {
    pub(crate) fn now() -> Self {
        Self {
            instant: Instant::now(),
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

pub(crate) fn current_process_state() -> &'static ProcessState {
    PROCESS_STATE
        .get()
        .expect("window diagnostics must be initialized before window creation")
}

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

fn serialize_chromium_args(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| serialize_chromium_token(token))
        .collect::<Vec<_>>()
        .join(" ")
}

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
        Regex::new(r#"(?i)(?:[A-Z]:[\\/]|\\\\)[^\s\"'<>]+"#).expect("valid Windows path regex");
    let without_windows_paths = windows_path_pattern.replace_all(&without_quoted_paths, "<path>");
    let posix_path_pattern =
        Regex::new(r#"(?:^|([\s(]))/[^\s\"'<>]+"#).expect("valid POSIX path regex");
    let without_paths = posix_path_pattern.replace_all(&without_windows_paths, "${1}<path>");

    let query_pattern =
        Regex::new(r#"\?[^\s\"'<>]*=[^\s\"'<>]*"#).expect("valid query-fragment regex");
    query_pattern
        .replace_all(&without_paths, "<query>")
        .chars()
        .take(DIAGNOSTIC_TEXT_MAX_CHARS)
        .collect()
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
        webview_version = ?snapshot.webview_version.as_deref(),
        webview_version_error = ?snapshot.webview_version_error.as_deref(),
        disable_hardware_acceleration = snapshot.disable_hardware_acceleration,
        webview_debug_enabled = snapshot.webview_debug_enabled,
        disable_gpu = snapshot.safe_switches.disable_gpu,
        enable_logging = snapshot.safe_switches.enable_logging,
        verbosity = ?snapshot.safe_switches.verbosity,
        log_file_present = snapshot.safe_switches.log_file_present,
        browser_executable_override_present = snapshot.browser_executable_override_present,
        user_data_override_present = snapshot.user_data_override_present,
        release_channel_override_present = snapshot.release_channel_override_present,
        webview_log_path = ?webview_log_path.as_deref(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct InternalLogName {
    name: String,
    pid: u32,
}

#[derive(Debug, Clone)]
struct RetentionCandidate {
    name: InternalLogName,
    modified_ms: u128,
}

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

#[cfg(not(windows))]
fn prune_internal_logs(_dir: &Path, _current_path: &Path) -> io::Result<()> {
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
    use std::path::Path;

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
}
