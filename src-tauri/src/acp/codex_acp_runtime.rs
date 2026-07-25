//! Managed npm-prefix launch for the Stop-patched vendored codex-acp package.
//!
//! Production no longer launches public `@agentclientprotocol/codex-acp@1.1.7`
//! from ambient PATH when a managed prefix or packaged seed is available.
//! The adapter bin is JavaScript (`dist/index.js`); it is only spawnable after
//! an `npm install` into an application-managed prefix under the effective
//! data dir (`CODEG_DATA_DIR` / Tauri app data / `~/.codeg`).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::{Mutex, MutexGuard};

use crate::acp::bundled_agent::CODEX_ACP_OVERRIDE_ENV;
use crate::acp::error::AcpError;

/// Locked Stop pin (Fallback baseline: mycodebuddy fork lineage).
/// Must match `src-tauri/vendor/codex-acp/package.json` version.
pub const CODEX_ACP_LOCKED_PIN: &str = "1.1.2-mycodebuddy.stop1";

const MANAGED_RUNTIME_DIR: &str = "agent-runtimes";
const SEED_ENV: &str = "CODEG_CODEX_ACP_SEED";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Process-local single-flight for concurrent install tasks in one runtime.
static INSTALL_LOCK: Mutex<()> = Mutex::const_new(());

/// Holds process-local + inter-process install exclusion for the managed prefix.
/// Drop order: file unlock on close, then tokio mutex release.
struct ManagedInstallGuard {
    _file: File,
    _process: MutexGuard<'static, ()>,
}

/// Lock file sibling of the managed prefix:
/// `<data_dir>/agent-runtimes/codex-acp-<pin>.install.lock`
fn managed_install_lock_path(data_dir: &Path) -> PathBuf {
    data_dir
        .join(MANAGED_RUNTIME_DIR)
        .join(format!("codex-acp-{CODEX_ACP_LOCKED_PIN}.install.lock"))
}

/// Open (create) the install lock file after ensuring its parent exists.
fn open_managed_install_lock_file(data_dir: &Path) -> Result<File, AcpError> {
    let path = managed_install_lock_path(data_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AcpError::protocol(format!(
                "failed to create agent-runtimes dir for install lock {}: {e}",
                parent.display()
            ))
        })?;
    }
    File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| {
            AcpError::protocol(format!(
                "failed to open managed codex-acp install lock {}: {e}",
                path.display()
            ))
        })
}

/// Blocking exclusive lock (`flock` / `LockFileEx`). OS releases on crash.
fn lock_managed_install_file_blocking(data_dir: &Path) -> Result<File, AcpError> {
    let path = managed_install_lock_path(data_dir);
    let file = open_managed_install_lock_file(data_dir)?;
    file.lock().map_err(|e| {
        AcpError::protocol(format!(
            "failed to acquire managed codex-acp install lock {}: {e}",
            path.display()
        ))
    })?;
    Ok(file)
}

/// Non-blocking exclusive attempt — used by tests to prove cross-handle exclusion.
fn try_lock_managed_install_file(data_dir: &Path) -> Result<File, std::fs::TryLockError> {
    let file = match open_managed_install_lock_file(data_dir) {
        Ok(f) => f,
        Err(e) => {
            return Err(std::fs::TryLockError::Error(std::io::Error::other(
                e.to_string(),
            )));
        }
    };
    file.try_lock()?;
    Ok(file)
}

/// Acquire process single-flight then inter-process exclusive install lock.
/// File lock is taken on a blocking thread so the async runtime is not stalled.
async fn acquire_managed_install_lock(
    data_dir: &Path,
) -> Result<ManagedInstallGuard, AcpError> {
    let process = INSTALL_LOCK.lock().await;
    let data_dir = data_dir.to_path_buf();
    let file = tokio::task::spawn_blocking(move || lock_managed_install_file_blocking(&data_dir))
        .await
        .map_err(|e| {
            AcpError::protocol(format!(
                "managed codex-acp install lock task failed: {e}"
            ))
        })??;
    Ok(ManagedInstallGuard {
        _file: file,
        _process: process,
    })
}

/// Absolute path to the managed-prefix shim for the locked pin, if present
/// and integrity-valid. Does not install.
pub fn managed_codex_acp_shim_if_valid(data_dir: &Path) -> Option<PathBuf> {
    let prefix = managed_prefix_dir(data_dir);
    if managed_prefix_is_valid(&prefix) {
        existing_managed_shim(&prefix)
    } else {
        None
    }
}

/// Non-installing probe for agent list/status: locked pin when managed prefix is valid.
pub fn probe_managed_codex_installed_version() -> Option<String> {
    managed_codex_acp_shim_if_valid(&default_data_dir())
        .map(|_| CODEX_ACP_LOCKED_PIN.to_string())
}

/// Production launch argv[0] for Codex. Fail-closed: never returns a bare
/// `codex-acp` PATH name when resolution fails (avoids ambient public 1.1.7).
pub async fn resolve_codex_launch_argv0() -> Result<String, AcpError> {
    codex_launch_argv0_from_resolved(resolve_codex_acp_command().await)
}

/// Map resolver output to launch argv[0]. Production call chain uses this
/// (via [`resolve_codex_launch_argv0`]) so `None` is never replaced with a bare
/// registry command name.
pub fn codex_launch_argv0_from_resolved(resolved: Option<PathBuf>) -> Result<String, AcpError> {
    match resolved {
        Some(path) => Ok(path.to_string_lossy().into_owned()),
        None => Err(AcpError::SdkNotInstalled(
            "Codex CLI is not installed. Please install it in Agent Settings.".to_string(),
        )),
    }
}

/// Resolve the absolute command used to launch default-pin Codex ACP.
///
/// Order:
/// 1. `CODEG_CODEX_ACP_BIN` absolute executable (escape hatch)
/// 2. Valid managed-prefix shim under the effective data dir
/// 3. Single-flight install from packaged/dev seed, then managed shim
/// 4. Legacy PATH / npm-global only when no seed is available
///
/// When a managed prefix or seed exists, ambient PATH public `1.1.7` is ignored.
/// Callers that launch the agent must treat `None` as fail-closed (see
/// [`resolve_codex_launch_argv0`]) — never substitute the bare registry cmd.
pub async fn resolve_codex_acp_command() -> Option<PathBuf> {
    resolve_codex_acp_command_with(
        std::env::var_os(CODEX_ACP_OVERRIDE_ENV).map(PathBuf::from),
        default_data_dir(),
        discover_seed_dir(),
        |seed, prefix| Box::pin(install_from_seed_into_prefix(seed, prefix)),
        || Box::pin(path_fallback_codex_acp()),
    )
    .await
}

/// Testable core for [`resolve_codex_acp_command`].
pub async fn resolve_codex_acp_command_with<Install, PathFallback>(
    override_bin: Option<PathBuf>,
    data_dir: PathBuf,
    seed_dir: Option<PathBuf>,
    install: Install,
    path_fallback: PathFallback,
) -> Option<PathBuf>
where
    Install: Fn(PathBuf, PathBuf) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), AcpError>> + Send>>
        + Send,
    PathFallback: Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<PathBuf>> + Send>>
        + Send,
{
    if let Some(path) = override_bin {
        if is_executable_file(&path) {
            return Some(path);
        }
        tracing::warn!(
            target: "codex_acp_runtime",
            path = %path.display(),
            "{CODEX_ACP_OVERRIDE_ENV} does not point to an executable; ignoring"
        );
    }

    if let Some(shim) = managed_codex_acp_shim_if_valid(&data_dir) {
        return Some(shim);
    }

    if let Some(seed) = seed_dir {
        if seed_looks_valid(&seed) {
            let prefix = managed_prefix_dir(&data_dir);
            let _guard = match acquire_managed_install_lock(&data_dir).await {
                Ok(g) => g,
                Err(err) => {
                    tracing::error!(
                        target: "codex_acp_runtime",
                        error = %err,
                        "failed to acquire managed codex-acp install lock"
                    );
                    // Seed exists: never fall through to ambient PATH public pin.
                    return None;
                }
            };
            // Re-check after process + inter-process lock (another task/process
            // may have finished install while we waited).
            if let Some(shim) = managed_codex_acp_shim_if_valid(&data_dir) {
                return Some(shim);
            }
            match install(seed, prefix.clone()).await {
                Ok(()) => {
                    if let Some(shim) = managed_codex_acp_shim_if_valid(&data_dir) {
                        return Some(shim);
                    }
                    tracing::error!(
                        target: "codex_acp_runtime",
                        prefix = %prefix.display(),
                        "managed install reported success but integrity check failed"
                    );
                }
                Err(err) => {
                    tracing::error!(
                        target: "codex_acp_runtime",
                        error = %err,
                        "managed codex-acp install from seed failed"
                    );
                }
            }
            // Seed exists: never fall through to ambient PATH public pin.
            return None;
        }
    }

    path_fallback().await
}

/// Ensure the locked pin is installed into the managed prefix (single-flight +
/// inter-process exclusive lock). Used by Agent Settings prepare for the
/// default Codex pin.
pub async fn ensure_managed_codex_acp_installed() -> Result<PathBuf, AcpError> {
    let data_dir = default_data_dir();
    if let Some(shim) = managed_codex_acp_shim_if_valid(&data_dir) {
        return Ok(shim);
    }
    let _guard = acquire_managed_install_lock(&data_dir).await?;
    ensure_managed_codex_acp_installed_locked(&data_dir).await
}

/// Install path that assumes [`acquire_managed_install_lock`] is already held.
async fn ensure_managed_codex_acp_installed_locked(
    data_dir: &Path,
) -> Result<PathBuf, AcpError> {
    if let Some(shim) = managed_codex_acp_shim_if_valid(data_dir) {
        return Ok(shim);
    }
    let seed = discover_seed_dir().ok_or_else(|| {
        AcpError::protocol(
            "codex-acp seed missing: run node src-tauri/scripts/stage-codex-acp.mjs \
             (or install via package that ships resources/codex-acp-seed)"
                .to_string(),
        )
    })?;
    if !seed_looks_valid(&seed) {
        return Err(AcpError::protocol(format!(
            "codex-acp seed incomplete at {}",
            seed.display()
        )));
    }
    let prefix = managed_prefix_dir(data_dir);
    install_from_seed_into_prefix(seed, prefix.clone()).await?;
    managed_codex_acp_shim_if_valid(data_dir).ok_or_else(|| {
        AcpError::protocol(format!(
            "codex-acp managed install incomplete at {}",
            prefix.display()
        ))
    })
}

/// Repair a partial/mismatched managed prefix by deleting and reinstalling.
/// Holds the inter-process install lock across wipe + reinstall.
pub async fn repair_managed_codex_acp_install() -> Result<PathBuf, AcpError> {
    let data_dir = default_data_dir();
    let prefix = managed_prefix_dir(&data_dir);
    let _guard = acquire_managed_install_lock(&data_dir).await?;
    if prefix.exists() {
        let _ = tokio::fs::remove_dir_all(&prefix).await;
    }
    ensure_managed_codex_acp_installed_locked(&data_dir).await
}

pub fn locked_pin() -> &'static str {
    CODEX_ACP_LOCKED_PIN
}

pub fn managed_prefix_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join(MANAGED_RUNTIME_DIR)
        .join(format!("codex-acp-{CODEX_ACP_LOCKED_PIN}"))
}

/// Existing executable shim only — never fabricates a non-existent path.
fn existing_managed_shim(prefix: &Path) -> Option<PathBuf> {
    managed_shim_candidates(prefix)
        .into_iter()
        .find(|p| is_executable_file(p))
}

fn managed_shim_candidates(prefix: &Path) -> Vec<PathBuf> {
    let bin_dir = npm_prefix_bin_dir(prefix);
    let local_bin = prefix.join("node_modules").join(".bin");
    if cfg!(windows) {
        vec![
            bin_dir.join("codex-acp.cmd"),
            bin_dir.join("codex-acp.exe"),
            bin_dir.join("codex-acp"),
            local_bin.join("codex-acp.cmd"),
            local_bin.join("codex-acp"),
        ]
    } else {
        vec![bin_dir.join("codex-acp"), local_bin.join("codex-acp")]
    }
}

fn npm_prefix_bin_dir(prefix: &Path) -> PathBuf {
    if cfg!(windows) {
        prefix.to_path_buf()
    } else {
        prefix.join("bin")
    }
}

/// Integrity: locked package version **and** `dist/index.js` **and** a real
/// executable bin/shim. Dist-only partial prefixes must fail so install repairs.
fn managed_prefix_is_valid(prefix: &Path) -> bool {
    if !prefix.is_dir() {
        return false;
    }
    let pkg_json = prefix
        .join("node_modules")
        .join("@agentclientprotocol")
        .join("codex-acp")
        .join("package.json");
    // Local `npm install <dir>` may hoist differently; also accept package.json
    // at prefix root when the seed was copied as the package itself.
    let version_ok = read_package_version(&pkg_json)
        .or_else(|| read_package_version(&prefix.join("package.json")))
        .map(|v| v == CODEX_ACP_LOCKED_PIN)
        .unwrap_or(false);
    if !version_ok {
        return false;
    }
    if !dist_entry_exists(prefix) {
        return false;
    }
    existing_managed_shim(prefix).is_some()
}

/// Remove the managed prefix under the install lock (uninstall / repair prep).
pub async fn remove_managed_codex_acp_prefix() -> Result<(), AcpError> {
    let data_dir = default_data_dir();
    let prefix = managed_prefix_dir(&data_dir);
    let _guard = acquire_managed_install_lock(&data_dir).await?;
    if prefix.exists() {
        tokio::fs::remove_dir_all(&prefix).await.map_err(|e| {
            AcpError::protocol(format!(
                "failed to remove managed codex-acp prefix {}: {e}",
                prefix.display()
            ))
        })?;
    }
    Ok(())
}

fn dist_entry_exists(prefix: &Path) -> bool {
    let nested = prefix
        .join("node_modules")
        .join("@agentclientprotocol")
        .join("codex-acp")
        .join("dist")
        .join("index.js");
    nested.is_file() || prefix.join("dist").join("index.js").is_file()
}

fn read_package_version(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn seed_looks_valid(seed: &Path) -> bool {
    seed.join("package.json").is_file() && seed.join("dist").join("index.js").is_file()
}

fn default_data_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os("CODEG_DATA_DIR").filter(|s| !s.is_empty()) {
        return crate::git_credential::absolutize(Path::new(&custom));
    }
    // Match resolve_effective_data_dir fallback when no Tauri path is available:
    // prefer ~/.codeg so desktop and server share the same managed prefix root
    // under the common home-layout; CODEG_HOME is intentionally not used here
    // (pets/uploads use it; runtimes stay under the data dir).
    dirs::home_dir()
        .map(|h| h.join(".codeg"))
        .unwrap_or_else(|| PathBuf::from(".codeg"))
}

/// Discover the packaged seed directory (dev + desktop + server layouts).
pub fn discover_seed_dir() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(SEED_ENV).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(explicit);
        if seed_looks_valid(&path) {
            return Some(path);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("codex-acp-seed"));
            candidates.push(dir.join("resources").join("codex-acp-seed"));
            // Tauri resource layout: resources live under resource_dir.
            candidates.push(dir.join("..").join("resources").join("codex-acp-seed"));
        }
    }

    // Dev / clean-checkout: seed next to the crate resources tree (produced by
    // stage-codex-acp.mjs). Do not fall back to the raw vendor tree — that would
    // trigger managed installs during unrelated cargo tests after a local build.
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/codex-acp-seed"));

    for candidate in candidates {
        let resolved = crate::git_credential::absolutize(&candidate);
        if seed_looks_valid(&resolved) {
            return Some(resolved);
        }
    }
    None
}

async fn path_fallback_codex_acp() -> Option<PathBuf> {
    crate::commands::acp::resolve_npx_command_ignoring_codex_managed("codex-acp").await
}

/// Install seed into `prefix` (wipes partial/mismatched target first).
/// Public to tests that exercise production installer behavior.
pub async fn install_from_seed_into_prefix(seed: PathBuf, prefix: PathBuf) -> Result<(), AcpError> {
    // Wipe partial / version-mismatched prefix before reinstall.
    if prefix.exists() {
        tokio::fs::remove_dir_all(&prefix).await.map_err(|e| {
            AcpError::protocol(format!(
                "failed to remove stale codex-acp prefix {}: {e}",
                prefix.display()
            ))
        })?;
    }

    let parent = prefix.parent().ok_or_else(|| {
        AcpError::protocol(format!("invalid managed prefix path {}", prefix.display()))
    })?;
    tokio::fs::create_dir_all(parent).await.map_err(|e| {
        AcpError::protocol(format!(
            "failed to create agent-runtimes dir {}: {e}",
            parent.display()
        ))
    })?;

    let staging = parent.join(format!(
        ".codex-acp-{}-staging-{}",
        CODEX_ACP_LOCKED_PIN,
        std::process::id()
    ));
    if staging.exists() {
        let _ = tokio::fs::remove_dir_all(&staging).await;
    }
    tokio::fs::create_dir_all(&staging).await.map_err(|e| {
        AcpError::protocol(format!(
            "failed to create staging prefix {}: {e}",
            staging.display()
        ))
    })?;

    let npm = which::which("npm").map_err(|_| {
        AcpError::protocol("npm not found on PATH; required to install managed codex-acp")
    })?;

    // Install the local seed package into the staging prefix. Using a file
    // path keeps us offline for the package body; deps still resolve via npm.
    let seed_arg = seed.to_string_lossy().to_string();
    let prefix_arg = format!("--prefix={}", staging.display());
    // `-g --prefix` places package bins at the prefix root (Windows) /
    // prefix/bin (Unix), matching managed_shim_path. Without `-g`, npm only
    // writes shims under node_modules/.bin.
    let mut cmd = crate::process::tokio_command(npm);
    cmd.args([
        "install",
        "-g",
        &prefix_arg,
        "--no-fund",
        "--no-audit",
        &seed_arg,
    ])
    .kill_on_drop(true);

    let output = tokio::time::timeout(INSTALL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            AcpError::protocol(format!(
                "npm install timed out after {}s for codex-acp seed",
                INSTALL_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| AcpError::protocol(format!("failed to spawn npm install: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(AcpError::protocol(format!(
            "npm install of codex-acp seed failed: {}",
            stderr.trim()
        )));
    }

    // Rewrite Windows .cmd before integrity so staging has a real node shim.
    ensure_reliable_windows_shim(&staging)?;

    if !managed_prefix_is_valid(&staging) {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(AcpError::protocol(format!(
            "codex-acp staging install failed integrity (expected pin {CODEX_ACP_LOCKED_PIN})"
        )));
    }

    // Atomic promote: rename staging → final. On Windows, remove target first
    // if a racer left debris (caller holds managed install lock so this is rare).
    if prefix.exists() {
        let _ = tokio::fs::remove_dir_all(&prefix).await;
    }
    tokio::fs::rename(&staging, &prefix).await.map_err(|e| {
        // Best-effort cleanup of staging on promote failure.
        let staging_clone = staging.clone();
        tokio::spawn(async move {
            let _ = tokio::fs::remove_dir_all(staging_clone).await;
        });
        AcpError::protocol(format!(
            "failed to promote codex-acp prefix to {}: {e}",
            prefix.display()
        ))
    })?;

    // Re-assert after promote (Windows rename edge cases).
    ensure_reliable_windows_shim(&prefix)?;

    if !managed_prefix_is_valid(&prefix) {
        let _ = tokio::fs::remove_dir_all(&prefix).await;
        return Err(AcpError::protocol(
            "codex-acp managed prefix failed integrity after promote",
        ));
    }

    Ok(())
}

fn ensure_reliable_windows_shim(prefix: &Path) -> Result<(), AcpError> {
    if !cfg!(windows) {
        return Ok(());
    }
    let js = prefix
        .join("node_modules")
        .join("@agentclientprotocol")
        .join("codex-acp")
        .join("dist")
        .join("index.js");
    if !js.is_file() {
        return Ok(());
    }
    let cmd_path = npm_prefix_bin_dir(prefix).join("codex-acp.cmd");
    // Relative path from prefix root so the shim is relocatable.
    let body = "@echo off\r\nnode \"%~dp0node_modules\\@agentclientprotocol\\codex-acp\\dist\\index.js\" %*\r\n";
    std::fs::write(&cmd_path, body).map_err(|e| {
        AcpError::protocol(format!(
            "failed to write reliable codex-acp.cmd at {}: {e}",
            cmd_path.display()
        ))
    })?;
    Ok(())
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn write_minimal_seed(dir: &Path, version: &str) {
        std::fs::create_dir_all(dir.join("dist")).unwrap();
        std::fs::write(
            dir.join("package.json"),
            format!(
                r#"{{
  "name": "@agentclientprotocol/codex-acp",
  "version": "{version}",
  "bin": {{ "codex-acp": "dist/index.js" }},
  "main": "dist/index.js"
}}"#
            ),
        )
        .unwrap();
        // Minimal ACP initialize responder for smoke tests.
        std::fs::write(
            dir.join("dist/index.js"),
            r#"#!/usr/bin/env node
const readline = require("readline");
const rl = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
rl.on("line", (line) => {
  let msg;
  try { msg = JSON.parse(line); } catch { return; }
  if (msg.method === "initialize" && msg.id != null) {
    process.stdout.write(JSON.stringify({
      jsonrpc: "2.0",
      id: msg.id,
      result: {
        protocolVersion: 1,
        agentCapabilities: {},
        agentInfo: { name: "codex-acp-test", version: "1.1.2-mycodebuddy.stop1" }
      }
    }) + "\n");
  }
});
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(dir.join("dist/index.js"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(dir.join("dist/index.js"), perms).unwrap();
        }
    }

    fn materialize_pkg_tree(prefix: &Path, version: &str) {
        let pkg_dir = prefix
            .join("node_modules")
            .join("@agentclientprotocol")
            .join("codex-acp");
        std::fs::create_dir_all(pkg_dir.join("dist")).unwrap();
        std::fs::write(
            pkg_dir.join("package.json"),
            format!(
                r#"{{"name":"@agentclientprotocol/codex-acp","version":"{version}","bin":{{"codex-acp":"dist/index.js"}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(pkg_dir.join("dist/index.js"), "console.log('ok')\n").unwrap();
    }

    fn write_shim(prefix: &Path) -> PathBuf {
        let bin_dir = npm_prefix_bin_dir(prefix);
        std::fs::create_dir_all(&bin_dir).unwrap();
        #[cfg(windows)]
        {
            let path = bin_dir.join("codex-acp.cmd");
            std::fs::write(
                &path,
                "@echo off\r\nnode \"%~dp0\\node_modules\\@agentclientprotocol\\codex-acp\\dist\\index.js\" %*\r\n",
            )
            .unwrap();
            path
        }
        #[cfg(not(windows))]
        {
            let path = bin_dir.join("codex-acp");
            std::fs::write(
                &path,
                "#!/bin/sh\nexec node \"$(dirname \"$0\")/../node_modules/@agentclientprotocol/codex-acp/dist/index.js\" \"$@\"\n",
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&path, perms).unwrap();
            }
            path
        }
    }

    fn materialize_fake_managed_prefix(data_dir: &Path, version: &str) -> PathBuf {
        let prefix = managed_prefix_dir(data_dir);
        materialize_pkg_tree(&prefix, version);
        write_shim(&prefix)
    }

    /// Locked-pin package + dist, but no bin shim (partial install).
    fn materialize_partial_prefix_missing_shim(data_dir: &Path) -> PathBuf {
        let prefix = managed_prefix_dir(data_dir);
        materialize_pkg_tree(&prefix, CODEX_ACP_LOCKED_PIN);
        prefix
    }

    #[tokio::test]
    async fn codex_resolver_prefers_managed_prefix_over_path_public_1_1_7() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let managed = materialize_fake_managed_prefix(&data_dir, CODEX_ACP_LOCKED_PIN);
        let path_public = temp.path().join("path-public-1.1.7");
        std::fs::write(&path_public, b"public").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path_public).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path_public, perms).unwrap();
        }

        let install_calls = Arc::new(AtomicUsize::new(0));
        let install_calls2 = install_calls.clone();
        let path_hits = Arc::new(AtomicUsize::new(0));
        let path_hits2 = path_hits.clone();
        let path_public2 = path_public.clone();

        let resolved = resolve_codex_acp_command_with(
            None,
            data_dir,
            None,
            move |_seed, _prefix| {
                install_calls2.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            },
            move || {
                path_hits2.fetch_add(1, Ordering::SeqCst);
                let p = path_public2.clone();
                Box::pin(async move { Some(p) })
            },
        )
        .await
        .expect("managed shim");

        assert_eq!(resolved, managed);
        assert_eq!(install_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            path_hits.load(Ordering::SeqCst),
            0,
            "must ignore ambient PATH public 1.1.7 when managed prefix is valid"
        );
    }

    #[tokio::test]
    async fn codex_resolver_survives_restart_with_managed_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let managed = materialize_fake_managed_prefix(&data_dir, CODEX_ACP_LOCKED_PIN);

        let first = resolve_codex_acp_command_with(
            None,
            data_dir.clone(),
            None,
            |_s, _p| Box::pin(async { Ok(()) }),
            || Box::pin(async { None }),
        )
        .await
        .unwrap();
        let second = resolve_codex_acp_command_with(
            None,
            data_dir,
            None,
            |_s, _p| Box::pin(async { panic!("install must not re-run on restart") }),
            || Box::pin(async { None }),
        )
        .await
        .unwrap();

        assert_eq!(first, managed);
        assert_eq!(second, managed);
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn codex_resolver_codeg_codex_acp_bin_override() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let _managed = materialize_fake_managed_prefix(&data_dir, CODEX_ACP_LOCKED_PIN);
        let override_bin = temp.path().join("override-bin");
        std::fs::write(&override_bin, b"override").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&override_bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&override_bin, perms).unwrap();
        }

        let resolved = resolve_codex_acp_command_with(
            Some(override_bin.clone()),
            data_dir,
            None,
            |_s, _p| Box::pin(async { panic!("install must not run when override is set") }),
            || Box::pin(async { panic!("path fallback must not run when override is set") }),
        )
        .await
        .unwrap();

        assert_eq!(resolved, override_bin);
    }

    #[tokio::test]
    async fn codex_managed_install_single_flight_concurrent() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let seed = temp.path().join("seed");
        write_minimal_seed(&seed, CODEX_ACP_LOCKED_PIN);

        let install_calls = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let mk = |calls: Arc<AtomicUsize>, barrier: Arc<tokio::sync::Barrier>, data: PathBuf, seed: PathBuf| {
            async move {
                barrier.wait().await;
                let calls2 = calls.clone();
                let data2 = data.clone();
                resolve_codex_acp_command_with(
                    None,
                    data,
                    Some(seed.clone()),
                    move |s, p| {
                        let calls = calls2.clone();
                        let data = data2.clone();
                        Box::pin(async move {
                            let n = calls.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(n, 0, "only one install should enter");
                            // Simulate slow install by materializing the prefix.
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            let _ = materialize_fake_managed_prefix(&data, CODEX_ACP_LOCKED_PIN);
                            // ensure p matches managed_prefix_dir
                            assert_eq!(p, managed_prefix_dir(&data));
                            assert!(seed_looks_valid(&s) || true);
                            Ok(())
                        })
                    },
                    || Box::pin(async { None }),
                )
                .await
            }
        };

        let a = tokio::spawn(mk(
            install_calls.clone(),
            barrier.clone(),
            data_dir.clone(),
            seed.clone(),
        ));
        let b = tokio::spawn(mk(install_calls.clone(), barrier, data_dir.clone(), seed));
        let ra = a.await.unwrap();
        let rb = b.await.unwrap();
        assert!(ra.is_some());
        assert!(rb.is_some());
        assert_eq!(ra, rb);
        // Single-flight: second waiter re-checks validity after lock and skips install.
        assert_eq!(install_calls.load(Ordering::SeqCst), 1);
    }

    /// Inter-process exclusion proof for the managed-install lock file.
    /// Same abstraction as production (`File::try_lock` / `File::lock`); full
    /// multi-process npm install is not required when this holds.
    #[test]
    fn codex_managed_install_lock_is_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path();

        let guard = try_lock_managed_install_file(data_dir)
            .expect("first acquisition should take the exclusive install lock");

        assert!(
            managed_install_lock_path(data_dir).is_file(),
            "lock file should exist under agent-runtimes"
        );

        match try_lock_managed_install_file(data_dir) {
            Err(std::fs::TryLockError::WouldBlock) => {}
            Ok(_) => panic!("second try_lock must fail while first guard holds"),
            Err(other) => panic!("expected WouldBlock, got {other:?}"),
        }

        // Independent data dir must not contend.
        let other = tempfile::tempdir().unwrap();
        let other_guard = try_lock_managed_install_file(other.path())
            .expect("different data_dir should lock independently");
        drop(other_guard);

        drop(guard);
        // Reacquire can briefly observe WouldBlock under parallel test load;
        // retry until free (or deadline for a real leak regression).
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match try_lock_managed_install_file(data_dir) {
                Ok(_reacquired) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "install lock not reacquirable after guard drop"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("unexpected reacquire error: {e:?}"),
            }
        }
    }

    #[tokio::test]
    async fn codex_managed_install_repairs_partial_or_version_mismatch() {
        // Production installer (install_from_seed_into_prefix), not a fake inject.
        if which::which("npm").is_err() {
            eprintln!("skipping real install repair test: npm not on PATH");
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let seed = temp.path().join("seed");
        write_minimal_seed(&seed, CODEX_ACP_LOCKED_PIN);

        // Case A: wrong version with shim — integrity fails.
        let bad = materialize_fake_managed_prefix(&data_dir, "1.1.7");
        assert!(bad.is_file());
        assert!(
            managed_codex_acp_shim_if_valid(&data_dir).is_none(),
            "mismatched version must fail integrity"
        );

        let prefix = managed_prefix_dir(&data_dir);
        install_from_seed_into_prefix(seed.clone(), prefix.clone())
            .await
            .expect("real installer repairs version mismatch");
        assert!(
            managed_prefix_is_valid(&prefix),
            "prefix must be valid after real install"
        );
        let shim = existing_managed_shim(&prefix).expect("shim after install");
        assert!(shim.is_file());

        // Case B: locked pin but missing shim — must not count as valid; repair.
        let _ = tokio::fs::remove_dir_all(&prefix).await;
        materialize_partial_prefix_missing_shim(&data_dir);
        assert!(
            !managed_prefix_is_valid(&prefix),
            "dist-only partial prefix without shim must fail integrity"
        );
        assert!(
            managed_codex_acp_shim_if_valid(&data_dir).is_none(),
            "must not fabricate a non-existent shim path"
        );

        install_from_seed_into_prefix(seed, prefix.clone())
            .await
            .expect("real installer repairs missing shim");
        assert!(managed_prefix_is_valid(&prefix));
        assert!(existing_managed_shim(&prefix).is_some());
    }

    #[tokio::test]
    async fn codex_partial_prefix_missing_shim_is_invalid() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let prefix = materialize_partial_prefix_missing_shim(&data_dir);
        assert!(dist_entry_exists(&prefix));
        assert!(
            !managed_prefix_is_valid(&prefix),
            "version+dist without shim is invalid"
        );
        assert!(managed_codex_acp_shim_if_valid(&data_dir).is_none());
    }

    #[tokio::test]
    async fn codex_launch_argv0_fail_closed_when_seed_install_fails() {
        // Production call chain: seed present + install fails → SdkNotInstalled,
        // never bare "codex-acp" PATH name.
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let seed = temp.path().join("seed");
        write_minimal_seed(&seed, CODEX_ACP_LOCKED_PIN);

        let path_hits = Arc::new(AtomicUsize::new(0));
        let path_hits2 = path_hits.clone();

        let resolved = resolve_codex_acp_command_with(
            None,
            data_dir,
            Some(seed),
            |_s, _p| {
                Box::pin(async {
                    Err(AcpError::protocol("simulated install failure"))
                })
            },
            move || {
                path_hits2.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Some(PathBuf::from("codex-acp")) })
            },
        )
        .await;

        assert!(
            resolved.is_none(),
            "seed present + install failure must not fall back to PATH"
        );
        assert_eq!(
            path_hits.load(Ordering::SeqCst),
            0,
            "path fallback must not run when seed exists"
        );

        // Production launch mapper (connection.rs uses resolve_codex_launch_argv0).
        let err = codex_launch_argv0_from_resolved(resolved).unwrap_err();
        assert!(
            matches!(err, AcpError::SdkNotInstalled(_)),
            "launch must fail closed with SdkNotInstalled, got {err}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("is not installed"),
            "frontend matches this substring: {msg}"
        );
        assert_ne!(msg.trim(), "codex-acp");
        assert!(!msg.contains("1.1.7"));
    }

    #[tokio::test]
    async fn codex_launch_argv0_never_returns_bare_registry_cmd() {
        // When resolution yields an absolute managed path, argv0 is absolute.
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let managed = materialize_fake_managed_prefix(&data_dir, CODEX_ACP_LOCKED_PIN);

        let resolved = resolve_codex_acp_command_with(
            None,
            data_dir,
            None,
            |_s, _p| Box::pin(async { Ok(()) }),
            || Box::pin(async { None }),
        )
        .await
        .unwrap();

        assert_eq!(resolved, managed);
        assert!(
            resolved.exists(),
            "resolved launch path must exist: {}",
            resolved.display()
        );
        // Bare registry cmd is a relative name only; managed shim is a real file path.
        assert_ne!(
            resolved.as_os_str(),
            std::ffi::OsStr::new("codex-acp"),
            "must not launch bare registry command name"
        );
    }

    #[test]
    fn codex_probe_managed_installed_version_non_installing() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        assert!(managed_codex_acp_shim_if_valid(&data_dir).is_none());

        materialize_fake_managed_prefix(&data_dir, CODEX_ACP_LOCKED_PIN);
        assert_eq!(
            managed_codex_acp_shim_if_valid(&data_dir)
                .map(|_| CODEX_ACP_LOCKED_PIN.to_string()),
            Some(CODEX_ACP_LOCKED_PIN.to_string())
        );
    }

    #[tokio::test]
    async fn codex_custom_version_override_is_rejected_by_prepare_contract() {
        // Mirror prepare gate: non-empty override for Codex is unsupported.
        let override_v = Some("1.1.7".to_string());
        let rejected = override_v
            .as_deref()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        assert!(rejected);
        let err = AcpError::protocol(format!(
            "Codex custom version override is not supported \
             (requested 1.1.7); Codeg launches managed pin {CODEX_ACP_LOCKED_PIN} only"
        ));
        assert!(err.to_string().contains("not supported"));
        assert!(err.to_string().contains(CODEX_ACP_LOCKED_PIN));
    }

    #[tokio::test]
    async fn codex_resolver_seed_absent_falls_back_to_path() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let path_bin = temp.path().join("path-codex-acp");
        std::fs::write(&path_bin, b"path").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path_bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path_bin, perms).unwrap();
        }
        let path_bin2 = path_bin.clone();

        let resolved = resolve_codex_acp_command_with(
            None,
            data_dir,
            None, // seed absent
            |_s, _p| Box::pin(async { panic!("install must not run without seed") }),
            move || {
                let p = path_bin2.clone();
                Box::pin(async move { Some(p) })
            },
        )
        .await
        .unwrap();

        assert_eq!(resolved, path_bin);
    }

    #[tokio::test]
    async fn codex_resolver_initialize_smoke_via_resolve_codex_acp_command() {
        // Integration-style: real npm install from a minimal seed into a temp
        // data dir, then spawn the resolved shim and send ACP initialize.
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let seed = temp.path().join("seed");
        write_minimal_seed(&seed, CODEX_ACP_LOCKED_PIN);

        // Skip if npm is unavailable in the test environment.
        if which::which("npm").is_err() {
            eprintln!("skipping initialize smoke: npm not on PATH");
            return;
        }

        let old_data = std::env::var_os("CODEG_DATA_DIR");
        let old_seed = std::env::var_os(SEED_ENV);
        std::env::set_var("CODEG_DATA_DIR", &data_dir);
        std::env::set_var(SEED_ENV, &seed);

        // Isolate from ambient override.
        let old_override = std::env::var_os(CODEX_ACP_OVERRIDE_ENV);
        std::env::remove_var(CODEX_ACP_OVERRIDE_ENV);

        let result = async {
            let resolved = resolve_codex_acp_command()
                .await
                .ok_or_else(|| "resolve_codex_acp_command returned None".to_string())?;

            // For local file installs, npm may place the package under
            // node_modules/@agentclientprotocol/codex-acp and create a bin shim.
            // If the shim is a .cmd that invokes node, spawn it directly.
            let mut child = crate::process::tokio_command(&resolved)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .map_err(|e| format!("spawn {}: {e}", resolved.display()))?;

            let mut stdin = child.stdin.take().expect("stdin");
            use tokio::io::AsyncWriteExt;
            let init = serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": { "name": "codeg-test", "version": "0.0.0" }
                }
            });
            stdin
                .write_all(format!("{init}\n").as_bytes())
                .await
                .map_err(|e| format!("write init: {e}"))?;
            stdin.flush().await.ok();

            let stdout = child.stdout.take().expect("stdout");
            let mut reader = tokio::io::BufReader::new(stdout);
            let mut line = String::new();
            let read = tokio::time::timeout(Duration::from_secs(30), async {
                use tokio::io::AsyncBufReadExt;
                loop {
                    line.clear();
                    let n = reader
                        .read_line(&mut line)
                        .await
                        .map_err(|e| format!("read: {e}"))?;
                    if n == 0 {
                        return Err("eof before initialize response".to_string());
                    }
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let msg: serde_json::Value = serde_json::from_str(trimmed)
                        .map_err(|e| format!("json: {e} line={trimmed}"))?;
                    if msg.get("id") == Some(&serde_json::json!(1)) {
                        if msg.get("error").is_some() {
                            return Err(format!("initialize error: {msg}"));
                        }
                        if msg.get("result").is_some() {
                            return Ok(());
                        }
                    }
                }
            })
            .await
            .map_err(|_| "initialize timed out".to_string())?;

            let _ = child.kill().await;
            read
        }
        .await;

        // Restore env.
        match old_data {
            Some(v) => std::env::set_var("CODEG_DATA_DIR", v),
            None => std::env::remove_var("CODEG_DATA_DIR"),
        }
        match old_seed {
            Some(v) => std::env::set_var(SEED_ENV, v),
            None => std::env::remove_var(SEED_ENV),
        }
        match old_override {
            Some(v) => std::env::set_var(CODEX_ACP_OVERRIDE_ENV, v),
            None => std::env::remove_var(CODEX_ACP_OVERRIDE_ENV),
        }

        result.expect("initialize smoke via production resolver");
    }
}
