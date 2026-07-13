//! Shared terminal shell resolution and command strategies.
//!
//! One module owns dialect detection, system/explicit/custom resolution, and
//! both non-interactive (`build_command_line`) and interactive
//! (`configure_interactive_command`) invocation matrices so interactive tabs
//! and ACP terminal execution share a single specification.

use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};

use crate::models::SystemTerminalSettings;

/// Known command dialect for agent declaration and strategy selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellDialect {
    Cmd,
    PowerShell,
    Posix,
    Custom,
}

/// How the resolved shell was selected from settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellSource {
    System,
    Explicit,
    Custom,
}

/// Non-interactive argv pattern used to run one command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellCommandStrategy {
    Cmd,
    PowerShell,
    Posix,
    GenericDashC,
}

/// Fully resolved shell executable plus execution metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedShellSpec {
    pub executable: PathBuf,
    pub dialect: ShellDialect,
    pub display_name: String,
    pub source: ShellSource,
    pub command_strategy: ShellCommandStrategy,
}

/// Immutable snapshot of a settings selection and its resolved executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedShellSnapshot {
    pub spec: ResolvedShellSpec,
    pub selection_key: String,
}

/// Errors from resolving or classifying a terminal shell selection.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ShellResolveError {
    #[error("selected terminal shell '{display_name}' is unavailable at {executable}")]
    Unavailable {
        display_name: String,
        executable: String,
    },
    #[error(
        "selected terminal shell '{display_name}' has no supported command strategy at {executable}"
    )]
    Unsupported {
        display_name: String,
        executable: String,
    },
}

/// Stable key for settings comparison (system vs stored default_shell value).
pub fn terminal_shell_selection_key(settings: &SystemTerminalSettings) -> String {
    match normalized_setting(settings.default_shell.as_deref()) {
        None => "system".to_string(),
        Some(value) => value.to_string(),
    }
}

/// User-facing path for the current selection: resolved executable when
/// available, otherwise the stored value (or system executable on success).
pub fn effective_shell_for_display(settings: &SystemTerminalSettings) -> String {
    match resolve_terminal_shell(settings) {
        Ok(snapshot) => snapshot.spec.executable.display().to_string(),
        Err(_) => normalized_setting(settings.default_shell.as_deref())
            .unwrap_or("system")
            .to_string(),
    }
}

/// Resolve the persisted terminal setting against the host environment.
pub fn resolve_terminal_shell(
    settings: &SystemTerminalSettings,
) -> Result<ResolvedShellSnapshot, ShellResolveError> {
    if let Some(value) = normalized_setting(settings.default_shell.as_deref()) {
        let found = resolve_setting_path(value);
        return resolve_from_candidates(Some(value), found, None, None);
    }

    let shell_env = env_var_path("SHELL");

    #[cfg(windows)]
    {
        let comspec = env_var_path("COMSPEC");
        if let Ok(snapshot) =
            resolve_from_candidates(None, None, shell_env.clone(), comspec.clone())
        {
            return Ok(snapshot);
        }

        let cmd = which::which("cmd.exe").ok().or_else(|| {
            let fallback = PathBuf::from(r"C:\Windows\System32\cmd.exe");
            if is_usable_executable(&fallback) {
                Some(fallback)
            } else {
                None
            }
        });
        if let Ok(snapshot) = resolve_from_candidates(None, None, None, cmd) {
            return Ok(snapshot);
        }

        // Last-resort label when nothing is resolvable as a file.
        let fallback = PathBuf::from("cmd.exe");
        let spec = classify_resolved_shell(fallback, ShellSource::System)?;
        return Ok(ResolvedShellSnapshot {
            selection_key: terminal_shell_selection_key(settings),
            spec,
        });
    }

    #[cfg(not(windows))]
    {
        if let Ok(snapshot) = resolve_from_candidates(None, None, shell_env, None) {
            return Ok(snapshot);
        }

        for candidate in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
            let path = PathBuf::from(candidate);
            if !is_usable_executable(&path) {
                continue;
            }
            if let Ok(snapshot) = resolve_from_candidates(None, None, Some(path), None) {
                return Ok(snapshot);
            }
        }

        Err(ShellResolveError::Unavailable {
            display_name: "system".into(),
            executable: "system".into(),
        })
    }
}

/// Pure candidate resolver for tests and host resolution (no process env reads).
///
/// * `setting` — persisted `default_shell` (None = system).
/// * `explicit_path` — resolved path for an explicit/custom setting.
/// * `shell_env` — candidate from `SHELL` (system branch).
/// * `comspec` — candidate from `COMSPEC` (system branch, Windows).
pub(crate) fn resolve_from_candidates(
    setting: Option<&str>,
    explicit_path: Option<PathBuf>,
    shell_env: Option<PathBuf>,
    comspec: Option<PathBuf>,
) -> Result<ResolvedShellSnapshot, ShellResolveError> {
    if let Some(value) = normalized_setting(setting) {
        let source = if is_explicit_picker_value(value) {
            ShellSource::Explicit
        } else {
            ShellSource::Custom
        };
        let display_name = display_name_for_setting(value);
        let Some(path) = explicit_path else {
            return Err(ShellResolveError::Unavailable {
                display_name,
                executable: value.to_string(),
            });
        };
        let mut spec = classify_resolved_shell(path, source)?;
        // Prefer stable picker labels when the stored value is a known option.
        if is_explicit_picker_value(value) {
            spec.display_name = display_name_for_setting(value);
        }
        return Ok(ResolvedShellSnapshot {
            selection_key: terminal_shell_selection_key(&SystemTerminalSettings {
                default_shell: Some(value.to_string()),
            }),
            spec,
        });
    }

    // System branch: try SHELL, then COMSPEC; skip unavailable/unsupported.
    // Only this branch continues after a bad candidate — explicit/custom fail closed.
    for candidate in [shell_env, comspec].into_iter().flatten() {
        match classify_system_candidate(candidate) {
            Ok(spec) => {
                return Ok(ResolvedShellSnapshot {
                    selection_key: terminal_shell_selection_key(&SystemTerminalSettings {
                        default_shell: None,
                    }),
                    spec,
                });
            }
            Err(ShellResolveError::Unsupported { .. })
            | Err(ShellResolveError::Unavailable { .. }) => continue,
        }
    }

    Err(ShellResolveError::Unavailable {
        display_name: "system".into(),
        executable: "system".into(),
    })
}

/// Classify a concrete executable path into dialect + command strategy.
///
/// Basename-only: does not probe the filesystem. Callers that require a
/// usable file (System candidates, explicit/custom path resolution) must
/// gate with [`is_usable_executable`] or use [`classify_system_candidate`].
pub(crate) fn classify_resolved_shell(
    executable: PathBuf,
    source: ShellSource,
) -> Result<ResolvedShellSpec, ShellResolveError> {
    let display_name = display_name_for_path(&executable);
    let basename = shell_basename(&executable);
    let name = strip_exe_suffix(&basename);

    let (dialect, command_strategy) = match name.as_str() {
        "cmd" => (ShellDialect::Cmd, ShellCommandStrategy::Cmd),
        "powershell" | "pwsh" => (ShellDialect::PowerShell, ShellCommandStrategy::PowerShell),
        "bash" | "zsh" | "sh" | "dash" | "ksh" | "ash" | "mksh" | "busybox" => {
            (ShellDialect::Posix, ShellCommandStrategy::Posix)
        }
        "fish" | "nu" | "xonsh" | "elvish" => {
            (ShellDialect::Custom, ShellCommandStrategy::GenericDashC)
        }
        _ => {
            return Err(ShellResolveError::Unsupported {
                display_name: display_name.clone(),
                executable: executable.display().to_string(),
            });
        }
    };

    Ok(ResolvedShellSpec {
        executable,
        dialect,
        display_name,
        source,
        command_strategy,
    })
}

/// System-branch classification: missing/non-executable paths are
/// [`ShellResolveError::Unavailable`] so the fallback chain can continue.
fn classify_system_candidate(executable: PathBuf) -> Result<ResolvedShellSpec, ShellResolveError> {
    if !is_usable_executable(&executable) {
        let display_name = display_name_for_path(&executable);
        return Err(ShellResolveError::Unavailable {
            display_name,
            executable: executable.display().to_string(),
        });
    }
    classify_resolved_shell(executable, ShellSource::System)
}

/// Build a non-interactive process that runs `line` through the resolved shell.
///
/// Used by the ACP terminal runtime (later tasks); kept public so both
/// interactive and non-interactive paths share one strategy table.
#[allow(dead_code)] // consumed by ACP runtime wiring in subsequent tasks
pub fn build_command_line(spec: &ResolvedShellSpec, line: &str) -> tokio::process::Command {
    let mut command = crate::process::tokio_command(&spec.executable);
    #[cfg(windows)]
    command.envs([("PYTHONUTF8", "1"), ("PYTHONIOENCODING", "utf-8")]);
    match spec.command_strategy {
        ShellCommandStrategy::Cmd => {
            command.args(["/D", "/S", "/C"]);
            command.arg(format!("chcp 65001 >nul & {line}"));
        }
        ShellCommandStrategy::PowerShell => {
            command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]);
            command.arg(format!(
                "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; \
                 $OutputEncoding = [System.Text.Encoding]::UTF8; {line}"
            ));
        }
        ShellCommandStrategy::Posix | ShellCommandStrategy::GenericDashC => {
            command.args(["-c", line]);
        }
    }
    command
}

/// Configure an interactive PTY command from a resolved shell specification.
///
/// Preserves the historical interactive matrix from `terminal/manager.rs`
/// (login flags, CODEG_CMD indirection, UTF-8 env) without routing through
/// `build_command_line`.
pub fn configure_interactive_command(
    spec: &ResolvedShellSpec,
    cmd: &mut CommandBuilder,
    initial_command: Option<&str>,
) {
    #[cfg(windows)]
    {
        // Force UTF-8 output for all Windows shell flavors
        cmd.env("PYTHONUTF8", "1");
        cmd.env("PYTHONIOENCODING", "utf-8");

        match interactive_flavor(spec) {
            InteractiveFlavor::Cmd => {
                if let Some(command) = initial_command {
                    cmd.env("CODEG_CMD", command);
                    // Set UTF-8 code page before running the actual command
                    cmd.args(["/D", "/S", "/C", "chcp 65001 >nul & %CODEG_CMD%"]);
                } else {
                    // /K runs the command then stays open for interactive use
                    cmd.args(["/D", "/S", "/K", "chcp 65001 >nul"]);
                }
            }
            InteractiveFlavor::PowerShell => {
                if let Some(command) = initial_command {
                    cmd.env("CODEG_CMD", command);
                    cmd.args([
                        "-NoLogo",
                        "-NoProfile",
                        "-Command",
                        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $ErrorActionPreference = 'Stop'; Invoke-Expression $env:CODEG_CMD",
                    ]);
                } else {
                    // -NoExit runs the command then stays open for interactive use
                    cmd.args([
                        "-NoLogo",
                        "-NoProfile",
                        "-NoExit",
                        "-Command",
                        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; $host.UI.RawUI.WindowTitle = 'codeg'",
                    ]);
                }
            }
            InteractiveFlavor::BashLike => {
                cmd.env("TERM", "xterm-256color");
                cmd.env("COLORTERM", "truecolor");
                cmd.env("TERM_PROGRAM", "codeg");
                cmd.env("LANG", "C.UTF-8");
                if let Some(command) = initial_command {
                    cmd.env("CODEG_CMD", command);
                    cmd.args(["-l", "-i", "-c", "eval \"$CODEG_CMD\""]);
                } else {
                    cmd.args(["-l", "-i"]);
                }
            }
            InteractiveFlavor::Generic => {
                if let Some(command) = initial_command {
                    cmd.args(["-c", command]);
                }
            }
        }
    }

    #[cfg(not(windows))]
    {
        // GUI app environments often miss TERM; force a sane terminal type so
        // readline/zle can redraw lines correctly (history navigation, etc.).
        // Locale env (LANG/LC_ALL) is intentionally NOT injected — interactive
        // PTYs should respect whatever the user's shell rc files set up.
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "codeg");

        match interactive_flavor(spec) {
            InteractiveFlavor::BashLike => {
                if let Some(command) = initial_command {
                    // Indirection via env var avoids quoting/escaping bugs
                    // for arbitrary commands (and keeps long commands off
                    // argv for readability in `ps`).
                    cmd.env("CODEG_CMD", command);
                    cmd.args(["-l", "-i", "-c", "eval \"$CODEG_CMD\""]);
                } else {
                    cmd.args(["-l", "-i"]);
                }
            }
            InteractiveFlavor::Generic
            | InteractiveFlavor::Cmd
            | InteractiveFlavor::PowerShell => {
                // No-flag spawn for nu/xonsh/elvish/pwsh on Linux/etc. Most
                // modern shells default to interactive when stdin is a TTY,
                // so we get a usable session without guessing flag syntax.
                // PowerShell/Cmd on Unix use the same generic path.
                if let Some(command) = initial_command {
                    cmd.args(["-c", command]);
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum InteractiveFlavor {
    Cmd,
    PowerShell,
    /// bash / zsh / sh / dash / ksh / ash / mksh / busybox / fish
    BashLike,
    /// nu / xonsh / elvish / other custom
    Generic,
}

fn interactive_flavor(spec: &ResolvedShellSpec) -> InteractiveFlavor {
    match spec.command_strategy {
        ShellCommandStrategy::Cmd => InteractiveFlavor::Cmd,
        ShellCommandStrategy::PowerShell => InteractiveFlavor::PowerShell,
        ShellCommandStrategy::Posix => InteractiveFlavor::BashLike,
        ShellCommandStrategy::GenericDashC => {
            let name = strip_exe_suffix(&shell_basename(&spec.executable));
            // fish remains bash-like for interactive `-l -i` (historical matrix)
            // even though non-interactive dialect is Custom/GenericDashC.
            if name == "fish" {
                InteractiveFlavor::BashLike
            } else {
                InteractiveFlavor::Generic
            }
        }
    }
}

fn normalized_setting(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn is_explicit_picker_value(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "pwsh.exe" | "pwsh" | "powershell.exe" | "powershell" | "cmd.exe" | "cmd"
    )
}

fn shell_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn strip_exe_suffix(name: &str) -> String {
    name.strip_suffix(".exe")
        .unwrap_or(name)
        .to_ascii_lowercase()
}

fn display_name_for_setting(value: &str) -> String {
    let name = strip_exe_suffix(&value.to_ascii_lowercase());
    match name.as_str() {
        "pwsh" => "PowerShell 7".into(),
        "powershell" => "Windows PowerShell".into(),
        "cmd" => "CMD".into(),
        _ => {
            // Prefer the last path component for custom absolute paths.
            Path::new(value)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(value)
                .to_string()
        }
    }
}

fn display_name_for_path(path: &Path) -> String {
    let base = shell_basename(path);
    let name = strip_exe_suffix(&base);
    match name.as_str() {
        "pwsh" => "PowerShell 7".into(),
        "powershell" => "Windows PowerShell".into(),
        "cmd" => "CMD".into(),
        other if !other.is_empty() => other.to_string(),
        _ => path.display().to_string(),
    }
}

fn looks_like_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        || value.contains('/')
        || value.contains('\\')
        || path.components().count() > 1
}

fn is_usable_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match path.metadata() {
            Ok(meta) => meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn resolve_setting_path(value: &str) -> Option<PathBuf> {
    if looks_like_path(value) {
        let path = PathBuf::from(value);
        if is_usable_executable(&path) {
            Some(path)
        } else {
            None
        }
    } else {
        which::which(value).ok().filter(|p| is_usable_executable(p))
    }
}

fn env_var_path(key: &str) -> Option<PathBuf> {
    let raw = std::env::var(key).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Only surface resolvable, usable executables. Stale SHELL/COMSPEC values
    // must not block the System fallback chain.
    resolve_setting_path(trimmed)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub fn pwsh_spec() -> ResolvedShellSpec {
        ResolvedShellSpec {
            executable: PathBuf::from(if cfg!(windows) {
                r"C:\Program Files\PowerShell\7\pwsh.exe"
            } else {
                "/opt/powershell/pwsh"
            }),
            dialect: ShellDialect::PowerShell,
            display_name: "PowerShell 7".into(),
            source: ShellSource::Explicit,
            command_strategy: ShellCommandStrategy::PowerShell,
        }
    }

    pub fn posix_spec() -> ResolvedShellSpec {
        ResolvedShellSpec {
            executable: PathBuf::from("/bin/sh"),
            dialect: ShellDialect::Posix,
            display_name: "sh".into(),
            source: ShellSource::System,
            command_strategy: ShellCommandStrategy::Posix,
        }
    }

    #[allow(dead_code)] // shared fixture for later ACP shell tests
    pub fn snapshot(selection_key: &str, spec: ResolvedShellSpec) -> ResolvedShellSnapshot {
        ResolvedShellSnapshot {
            spec,
            selection_key: selection_key.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_pwsh_resolves_to_powershell_strategy() {
        let found = PathBuf::from(if cfg!(windows) {
            r"C:\Program Files\PowerShell\7\pwsh.exe"
        } else {
            "/opt/powershell/pwsh"
        });
        let snapshot =
            resolve_from_candidates(Some("pwsh.exe"), Some(found.clone()), None, None).unwrap();
        assert_eq!(snapshot.spec.executable, found);
        assert_eq!(snapshot.spec.dialect, ShellDialect::PowerShell);
        assert_eq!(
            snapshot.spec.command_strategy,
            ShellCommandStrategy::PowerShell
        );
        assert_eq!(snapshot.spec.source, ShellSource::Explicit);
    }

    #[test]
    fn unavailable_explicit_shell_does_not_fall_back() {
        let err = resolve_from_candidates(Some("pwsh.exe"), None, None, None).unwrap_err();
        assert!(matches!(err, ShellResolveError::Unavailable { .. }));
    }

    /// Create a regular file that passes [`is_usable_executable`] (Unix: +x).
    fn make_usable_shell(dir: &std::path::Path, basename: &str) -> PathBuf {
        let path = dir.join(basename);
        std::fs::write(&path, b"").expect("write temp shell");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    #[test]
    fn system_selection_uses_usable_shell_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let shell = make_usable_shell(dir.path(), if cfg!(windows) { "cmd.exe" } else { "zsh" });
        let snapshot = resolve_from_candidates(None, None, Some(shell.clone()), None).unwrap();
        assert_eq!(snapshot.spec.executable, shell);
        assert_eq!(snapshot.spec.source, ShellSource::System);
        assert_eq!(
            snapshot.spec.dialect,
            if cfg!(windows) {
                ShellDialect::Cmd
            } else {
                ShellDialect::Posix
            }
        );
    }

    #[test]
    fn system_selection_skips_existing_but_unsupported_shell() {
        let dir = tempfile::tempdir().unwrap();
        let unsupported = make_usable_shell(
            dir.path(),
            if cfg!(windows) {
                "mystery.exe"
            } else {
                "mystery"
            },
        );
        let fallback = make_usable_shell(dir.path(), if cfg!(windows) { "cmd.exe" } else { "sh" });
        let snapshot =
            resolve_from_candidates(None, None, Some(unsupported), Some(fallback.clone())).unwrap();
        assert_eq!(snapshot.spec.executable, fallback);
        assert_eq!(
            snapshot.spec.dialect,
            if cfg!(windows) {
                ShellDialect::Cmd
            } else {
                ShellDialect::Posix
            }
        );
    }

    #[test]
    fn system_selection_skips_unusable_known_basename_shell() {
        // Stale SHELL with a known basename (missing/non-executable) must not
        // block COMSPEC / the next System candidate.
        let unusable = PathBuf::from(if cfg!(windows) {
            r"C:\definitely\missing\Git\bin\bash.exe"
        } else {
            "/definitely/missing/bin/bash"
        });
        assert!(
            !is_usable_executable(&unusable),
            "precondition: path must be unusable"
        );

        let dir = tempfile::tempdir().unwrap();
        let fallback = make_usable_shell(dir.path(), if cfg!(windows) { "cmd.exe" } else { "sh" });
        let snapshot =
            resolve_from_candidates(None, None, Some(unusable), Some(fallback.clone())).unwrap();
        assert_eq!(snapshot.spec.executable, fallback);
        assert_eq!(snapshot.spec.source, ShellSource::System);
        assert_eq!(
            snapshot.spec.dialect,
            if cfg!(windows) {
                ShellDialect::Cmd
            } else {
                ShellDialect::Posix
            }
        );
    }

    #[test]
    fn system_unusable_only_candidate_is_unavailable() {
        let unusable = PathBuf::from(if cfg!(windows) {
            r"C:\definitely\missing\bash.exe"
        } else {
            "/definitely/missing/bash"
        });
        let err = resolve_from_candidates(None, None, Some(unusable), None).unwrap_err();
        assert!(matches!(err, ShellResolveError::Unavailable { .. }));
    }

    #[test]
    fn unknown_custom_shell_is_rejected_without_strategy() {
        let err = classify_resolved_shell(
            PathBuf::from(if cfg!(windows) {
                r"C:\tools\mystery.exe"
            } else {
                "/tmp/mystery"
            }),
            ShellSource::Custom,
        )
        .unwrap_err();
        assert!(matches!(err, ShellResolveError::Unsupported { .. }));
    }

    #[test]
    fn selection_key_distinguishes_system_and_explicit() {
        assert_ne!(
            terminal_shell_selection_key(&SystemTerminalSettings {
                default_shell: None
            }),
            terminal_shell_selection_key(&SystemTerminalSettings {
                default_shell: Some("cmd.exe".into()),
            })
        );
    }

    #[test]
    fn known_basename_mappings() {
        let cases: &[(&str, ShellDialect, ShellCommandStrategy)] = &[
            ("cmd.exe", ShellDialect::Cmd, ShellCommandStrategy::Cmd),
            (
                "powershell.exe",
                ShellDialect::PowerShell,
                ShellCommandStrategy::PowerShell,
            ),
            (
                "pwsh",
                ShellDialect::PowerShell,
                ShellCommandStrategy::PowerShell,
            ),
            ("bash", ShellDialect::Posix, ShellCommandStrategy::Posix),
            ("zsh", ShellDialect::Posix, ShellCommandStrategy::Posix),
            ("sh", ShellDialect::Posix, ShellCommandStrategy::Posix),
            ("dash", ShellDialect::Posix, ShellCommandStrategy::Posix),
            ("ksh", ShellDialect::Posix, ShellCommandStrategy::Posix),
            ("ash", ShellDialect::Posix, ShellCommandStrategy::Posix),
            ("mksh", ShellDialect::Posix, ShellCommandStrategy::Posix),
            ("busybox", ShellDialect::Posix, ShellCommandStrategy::Posix),
            (
                "fish",
                ShellDialect::Custom,
                ShellCommandStrategy::GenericDashC,
            ),
            (
                "nu",
                ShellDialect::Custom,
                ShellCommandStrategy::GenericDashC,
            ),
            (
                "xonsh",
                ShellDialect::Custom,
                ShellCommandStrategy::GenericDashC,
            ),
            (
                "elvish",
                ShellDialect::Custom,
                ShellCommandStrategy::GenericDashC,
            ),
        ];

        for (basename, dialect, strategy) in cases {
            let path = if cfg!(windows) {
                PathBuf::from(format!(r"C:\shells\{basename}"))
            } else {
                PathBuf::from(format!("/usr/bin/{basename}"))
            };
            let spec = classify_resolved_shell(path, ShellSource::Custom).unwrap();
            assert_eq!(spec.dialect, *dialect, "dialect for {basename}");
            assert_eq!(spec.command_strategy, *strategy, "strategy for {basename}");
        }
    }

    #[test]
    fn custom_source_even_for_known_basename_path() {
        let path = PathBuf::from(if cfg!(windows) {
            r"C:\tools\bash.exe"
        } else {
            "/opt/homebrew/bin/bash"
        });
        let snapshot =
            resolve_from_candidates(Some(path.to_str().unwrap()), Some(path.clone()), None, None)
                .unwrap();
        assert_eq!(snapshot.spec.source, ShellSource::Custom);
        assert_eq!(snapshot.spec.dialect, ShellDialect::Posix);
    }

    #[test]
    fn cmd_build_command_line_uses_d_s_c_and_utf8() {
        let spec = ResolvedShellSpec {
            executable: PathBuf::from(if cfg!(windows) {
                r"C:\Windows\System32\cmd.exe"
            } else {
                "/bin/cmd"
            }),
            dialect: ShellDialect::Cmd,
            display_name: "CMD".into(),
            source: ShellSource::Explicit,
            command_strategy: ShellCommandStrategy::Cmd,
        };
        let cmd = build_command_line(&spec, "echo hi");
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..3], &["/D", "/S", "/C"]);
        assert!(args[3].contains("chcp 65001"));
        assert!(args[3].contains("echo hi"));
        #[cfg(windows)]
        {
            let envs: std::collections::HashMap<_, _> = cmd
                .as_std()
                .get_envs()
                .filter_map(|(k, v)| Some((k.to_string_lossy().into_owned(), v?.to_string_lossy().into_owned())))
                .collect();
            assert_eq!(envs.get("PYTHONUTF8").map(String::as_str), Some("1"));
            assert_eq!(
                envs.get("PYTHONIOENCODING").map(String::as_str),
                Some("utf-8")
            );
        }
    }

    #[test]
    fn powershell_build_command_line_uses_four_noninteractive_flags() {
        let spec = test_support::pwsh_spec();
        let cmd = build_command_line(&spec, "Get-Location");
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            &args[..4],
            &["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
        );
        assert!(args[4].contains("Get-Location"));
        assert!(args[4].contains("OutputEncoding"));
    }

    #[test]
    fn posix_build_command_line_uses_dash_c() {
        let spec = test_support::posix_spec();
        let cmd = build_command_line(&spec, "pwd");
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["-c", "pwd"]);
    }

    #[test]
    fn generic_dash_c_build_command_line() {
        let spec = ResolvedShellSpec {
            executable: PathBuf::from("/usr/bin/nu"),
            dialect: ShellDialect::Custom,
            display_name: "nu".into(),
            source: ShellSource::Custom,
            command_strategy: ShellCommandStrategy::GenericDashC,
        };
        let cmd = build_command_line(&spec, "ls");
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["-c", "ls"]);
    }
}
