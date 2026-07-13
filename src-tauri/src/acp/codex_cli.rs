//! Host Codex CLI path resolution for `CODEX_PATH` (CLI mode via `CODEX_ACP_USE_CLI`).
//!
//! Pure selection prefers: explicit path → PATH hit → npm global `@openai/codex` entry.
//! Auto-detect never overwrites an existing non-empty `CODEX_PATH`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CODEX_PATH_ENV: &str = "CODEX_PATH";

/// Prefer explicit path, then PATH hit, then npm global `@openai/codex` entry.
pub fn select_codex_cli_path(
    explicit: Option<&Path>,
    on_path: Option<PathBuf>,
    npm_global_prefix: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(p) = explicit {
        if is_usable_cli_path(p) {
            return Some(p.to_path_buf());
        }
    }
    if let Some(p) = on_path {
        if is_usable_cli_path(&p) {
            return Some(p);
        }
    }
    if let Some(prefix) = npm_global_prefix {
        let js = prefix
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js");
        if is_usable_cli_path(&js) {
            return Some(js);
        }
        // Windows npm also drops `codex.cmd` next to the prefix root.
        for name in ["codex.cmd", "codex.exe", "codex"] {
            let cand = prefix.join(name);
            if is_usable_cli_path(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

fn is_usable_cli_path(path: &Path) -> bool {
    path.is_file()
}

/// Host resolution used at launch/preflight time.
pub fn resolve_codex_cli_path() -> Option<PathBuf> {
    let explicit = std::env::var_os(CODEX_PATH_ENV).map(PathBuf::from);
    let on_path = which::which("codex")
        .ok()
        .or_else(|| which::which("codex.cmd").ok());
    // Best-effort sync npm prefix: APPDATA\npm on Windows, or `npm root -g` parent.
    // Keep this function free of async; use known Windows location first:
    let npm_prefix = default_npm_global_prefix();
    select_codex_cli_path(explicit.as_deref(), on_path, npm_prefix.as_deref())
}

fn default_npm_global_prefix() -> Option<PathBuf> {
    // Windows: %APPDATA%\npm is the default global bin/prefix layout for npm shims.
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let p = PathBuf::from(appdata).join("npm");
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    // Non-Windows: ~/.npm-global is not reliable; which::which already covers PATH.
    None
}

pub fn ensure_codex_path_in_env(env: &mut BTreeMap<String, String>) -> Result<(), String> {
    if env
        .get(CODEX_PATH_ENV)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(());
    }
    // Honor process-level CODEX_PATH only when it points at a usable file.
    // Stale User/System CODEX_PATH must not block auto-detect / PATH resolve.
    if let Ok(v) = std::env::var(CODEX_PATH_ENV) {
        if !v.trim().is_empty() && Path::new(&v).is_file() {
            env.insert(CODEX_PATH_ENV.to_string(), v);
            return Ok(());
        }
    }
    match resolve_codex_cli_path() {
        Some(path) => {
            env.insert(
                CODEX_PATH_ENV.to_string(),
                path.to_string_lossy().into_owned(),
            );
            Ok(())
        }
        None => Err(
            "Codex CLI not found. Install with `npm install -g @openai/codex`, \
             or set CODEX_PATH to your codex executable (or codex.js). \
             MyCodeBuddy's bundled codex-acp adapter requires a host Codex CLI \
             when CODEX_ACP_USE_CLI=1."
                .to_string(),
        ),
    }
}

/// True when the effective launch env asks for CLI mode.
pub fn cli_mode_enabled(env: &BTreeMap<String, String>) -> bool {
    env.get("CODEX_ACP_USE_CLI")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or_else(|| {
            std::env::var("CODEX_ACP_USE_CLI")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false)
        })
}

/// Prepare env for a Codex ACP launch: inject `CODEX_PATH` when CLI mode is on.
///
/// Does not overwrite an explicit non-empty `CODEX_PATH` already present in `env`.
/// Returns `Err` with a user-facing message when CLI mode is requested but no CLI
/// can be resolved (caller maps this to `AcpError::SdkNotInstalled`).
pub fn prepare_codex_launch_env(
    mut env: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    if cli_mode_enabled(&env) {
        ensure_codex_path_in_env(&mut env)?;
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn touch_file(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn explicit_wins_over_path_and_npm() {
        let temp = tempfile::tempdir().unwrap();
        let explicit = touch_file(temp.path(), "explicit-codex.cmd");
        let on_path = touch_file(temp.path(), "path-codex.cmd");
        let prefix = temp.path().join("npm-prefix");
        fs::create_dir_all(prefix.join("node_modules").join("@openai").join("codex").join("bin"))
            .unwrap();
        let npm_js = touch_file(
            &prefix
                .join("node_modules")
                .join("@openai")
                .join("codex")
                .join("bin"),
            "codex.js",
        );
        let _ = npm_js;
        let selected =
            select_codex_cli_path(Some(&explicit), Some(on_path), Some(&prefix)).unwrap();
        assert_eq!(selected, explicit);
    }

    #[test]
    fn path_wins_over_npm_when_no_explicit() {
        let temp = tempfile::tempdir().unwrap();
        let on_path = touch_file(temp.path(), "path-codex.cmd");
        let prefix = temp.path().join("npm-prefix");
        fs::create_dir_all(prefix.join("node_modules").join("@openai").join("codex").join("bin"))
            .unwrap();
        touch_file(
            &prefix
                .join("node_modules")
                .join("@openai")
                .join("codex")
                .join("bin"),
            "codex.js",
        );
        let selected = select_codex_cli_path(None, Some(on_path.clone()), Some(&prefix)).unwrap();
        assert_eq!(selected, on_path);
    }

    #[test]
    fn npm_global_js_used_when_nothing_else() {
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("npm-prefix");
        let bin = prefix
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin");
        fs::create_dir_all(&bin).unwrap();
        let js = touch_file(&bin, "codex.js");
        let selected = select_codex_cli_path(None, None, Some(&prefix)).unwrap();
        assert_eq!(selected, js);
    }

    #[test]
    fn missing_everything_returns_none() {
        assert!(select_codex_cli_path(None, None, None).is_none());
    }

    #[test]
    fn ensure_does_not_overwrite_existing_codex_path() {
        let mut env = BTreeMap::new();
        env.insert("CODEX_PATH".into(), "C:\\already\\set\\codex.cmd".into());
        ensure_codex_path_in_env(&mut env).unwrap();
        assert_eq!(env.get("CODEX_PATH").unwrap(), "C:\\already\\set\\codex.cmd");
    }

    #[test]
    fn prepare_injects_when_cli_mode_and_missing_path() {
        let temp = tempfile::tempdir().unwrap();
        let fake = touch_file(temp.path(), "codex.cmd");
        let fake_str = fake.to_string_lossy().into_owned();

        // Map lacks CODEX_PATH; process CODEX_PATH points at a usable fake CLI.
        // Unset process CODEX_ACP_USE_CLI so map key alone drives cli mode.
        temp_env::with_vars(
            [
                (CODEX_PATH_ENV, Some(fake_str.as_str())),
                ("CODEX_ACP_USE_CLI", None),
            ],
            || {
                let mut env = BTreeMap::new();
                env.insert("CODEX_ACP_USE_CLI".into(), "1".into());
                let out = prepare_codex_launch_env(env).unwrap();
                assert_eq!(out.get(CODEX_PATH_ENV).map(String::as_str), Some(fake_str.as_str()));
            },
        );
    }

    #[test]
    fn prepare_skips_when_cli_mode_off() {
        let temp = tempfile::tempdir().unwrap();
        let fake = touch_file(temp.path(), "codex.cmd");
        let fake_str = fake.to_string_lossy().into_owned();

        // CLI mode off: even if process has CODEX_PATH, prepare must not inject.
        temp_env::with_vars(
            [
                (CODEX_PATH_ENV, Some(fake_str.as_str())),
                ("CODEX_ACP_USE_CLI", None),
            ],
            || {
                let env = BTreeMap::new();
                let out = prepare_codex_launch_env(env).unwrap();
                assert!(!out.contains_key(CODEX_PATH_ENV));
            },
        );
    }

    #[test]
    fn prepare_preserves_existing_map_codex_path() {
        let temp = tempfile::tempdir().unwrap();
        let process_path = touch_file(temp.path(), "process-codex.cmd");
        let process_str = process_path.to_string_lossy().into_owned();
        let map_path = "C:\\already\\set\\codex.cmd";

        temp_env::with_vars(
            [
                (CODEX_PATH_ENV, Some(process_str.as_str())),
                ("CODEX_ACP_USE_CLI", None),
            ],
            || {
                let mut env = BTreeMap::new();
                env.insert("CODEX_ACP_USE_CLI".into(), "1".into());
                env.insert(CODEX_PATH_ENV.into(), map_path.into());
                let out = prepare_codex_launch_env(env).unwrap();
                assert_eq!(out.get(CODEX_PATH_ENV).map(String::as_str), Some(map_path));
            },
        );
    }

    #[test]
    fn ensure_skips_nonexistent_process_codex_path() {
        // Stale process CODEX_PATH must not be injected as success, and must not
        // block resolve from finding a real CLI (or Err when nothing is available).
        let temp = tempfile::tempdir().unwrap();
        let real = touch_file(temp.path(), "codex.cmd");
        let stale = temp
            .path()
            .join("does-not-exist-codex.cmd")
            .to_string_lossy()
            .into_owned();
        assert!(!Path::new(&stale).is_file());

        // Prepend temp dir so which::which can find codex.cmd as a fallback.
        let path_var = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = std::ffi::OsString::new();
        new_path.push(temp.path().as_os_str());
        #[cfg(windows)]
        new_path.push(";");
        #[cfg(not(windows))]
        new_path.push(":");
        new_path.push(&path_var);
        let new_path_str = new_path.to_string_lossy().into_owned();

        temp_env::with_vars(
            [
                (CODEX_PATH_ENV, Some(stale.as_str())),
                ("PATH", Some(new_path_str.as_str())),
                ("CODEX_ACP_USE_CLI", None),
            ],
            || {
                let mut env = BTreeMap::new();
                match ensure_codex_path_in_env(&mut env) {
                    Ok(()) => {
                        let injected = env.get(CODEX_PATH_ENV).expect("CODEX_PATH injected");
                        assert_ne!(
                            injected, &stale,
                            "stale process CODEX_PATH must not be injected"
                        );
                        assert!(
                            Path::new(injected).is_file(),
                            "injected CODEX_PATH must be a real file: {injected}"
                        );
                        // Prefer the PATH hit we planted when resolve finds it.
                        assert_eq!(
                            Path::new(injected).file_name(),
                            real.file_name(),
                            "expected resolve to surface the real CLI under PATH"
                        );
                    }
                    Err(msg) => {
                        assert!(
                            !env.contains_key(CODEX_PATH_ENV),
                            "must not insert stale CODEX_PATH on failure"
                        );
                        assert!(
                            msg.contains("Codex CLI not found"),
                            "unexpected error: {msg}"
                        );
                    }
                }
            },
        );
    }
}
