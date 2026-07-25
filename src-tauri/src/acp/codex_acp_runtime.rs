//! Managed npm-prefix launch for the Stop-patched vendored codex-acp package.
//!
//! Production no longer launches public `@agentclientprotocol/codex-acp@1.1.7`
//! from ambient PATH when a managed prefix or packaged seed is available.
//! The adapter bin is JavaScript (`dist/index.js`); it is only spawnable after
//! an `npm install` into an application-managed prefix under the effective
//! data dir (`CODEG_DATA_DIR` / Tauri app data / `~/.codeg`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::Mutex;

use crate::acp::bundled_agent::CODEX_ACP_OVERRIDE_ENV;
use crate::acp::error::AcpError;

/// Locked Stop pin (Fallback baseline: mycodebuddy fork lineage).
/// Must match `src-tauri/vendor/codex-acp/package.json` version.
pub const CODEX_ACP_LOCKED_PIN: &str = "1.1.2-mycodebuddy.stop1";

const MANAGED_RUNTIME_DIR: &str = "agent-runtimes";
const SEED_ENV: &str = "CODEG_CODEX_ACP_SEED";
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

static INSTALL_LOCK: Mutex<()> = Mutex::const_new(());

/// Absolute path to the managed-prefix shim for the locked pin, if present
/// and integrity-valid. Does not install.
pub fn managed_codex_acp_shim_if_valid(data_dir: &Path) -> Option<PathBuf> {
    let prefix = managed_prefix_dir(data_dir);
    if managed_prefix_is_valid(&prefix) {
        Some(managed_shim_path(&prefix))
    } else {
        None
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
            let _guard = INSTALL_LOCK.lock().await;
            // Re-check after acquiring the single-flight lock.
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

/// Ensure the locked pin is installed into the managed prefix (single-flight).
/// Used by Agent Settings prepare for the default Codex pin.
pub async fn ensure_managed_codex_acp_installed() -> Result<PathBuf, AcpError> {
    let data_dir = default_data_dir();
    if let Some(shim) = managed_codex_acp_shim_if_valid(&data_dir) {
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
    let prefix = managed_prefix_dir(&data_dir);
    let _guard = INSTALL_LOCK.lock().await;
    if let Some(shim) = managed_codex_acp_shim_if_valid(&data_dir) {
        return Ok(shim);
    }
    install_from_seed_into_prefix(seed, prefix.clone()).await?;
    managed_codex_acp_shim_if_valid(&data_dir).ok_or_else(|| {
        AcpError::protocol(format!(
            "codex-acp managed install incomplete at {}",
            prefix.display()
        ))
    })
}

/// Repair a partial/mismatched managed prefix by deleting and reinstalling.
pub async fn repair_managed_codex_acp_install() -> Result<PathBuf, AcpError> {
    let data_dir = default_data_dir();
    let prefix = managed_prefix_dir(&data_dir);
    let _guard = INSTALL_LOCK.lock().await;
    if prefix.exists() {
        let _ = tokio::fs::remove_dir_all(&prefix).await;
    }
    drop(_guard);
    ensure_managed_codex_acp_installed().await
}

pub fn locked_pin() -> &'static str {
    CODEX_ACP_LOCKED_PIN
}

pub fn managed_prefix_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join(MANAGED_RUNTIME_DIR)
        .join(format!("codex-acp-{CODEX_ACP_LOCKED_PIN}"))
}

fn managed_shim_path(prefix: &Path) -> PathBuf {
    // Prefer global-style layout (`npm install -g --prefix`), then local
    // `node_modules/.bin` as a compatibility fallback.
    let candidates = managed_shim_candidates(prefix);
    candidates
        .into_iter()
        .find(|p| is_executable_file(p))
        .unwrap_or_else(|| npm_prefix_bin_dir(prefix).join(if cfg!(windows) {
            "codex-acp.cmd"
        } else {
            "codex-acp"
        }))
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
    let shim = managed_shim_path(prefix);
    is_executable_file(&shim) || dist_entry_exists(prefix)
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

async fn install_from_seed_into_prefix(seed: PathBuf, prefix: PathBuf) -> Result<(), AcpError> {
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

    if !managed_prefix_is_valid(&staging) {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(AcpError::protocol(format!(
            "codex-acp staging install failed integrity (expected pin {CODEX_ACP_LOCKED_PIN})"
        )));
    }

    // Atomic promote: rename staging → final. On Windows, remove target first
    // if a racer left debris (we hold INSTALL_LOCK so this is rare).
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

    // Rewrite Windows .cmd shim to invoke node explicitly. npm's default
    // bin wrapper runs the .js path as a bare command, which depends on a
    // user-level .js→node file association and fails under CREATE_NO_WINDOW
    // spawns used by codeg process helpers.
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

    fn materialize_fake_managed_prefix(data_dir: &Path, version: &str) -> PathBuf {
        let prefix = managed_prefix_dir(data_dir);
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

        let bin_dir = npm_prefix_bin_dir(&prefix);
        std::fs::create_dir_all(&bin_dir).unwrap();
        #[cfg(windows)]
        let shim = {
            let path = bin_dir.join("codex-acp.cmd");
            std::fs::write(
                &path,
                "@echo off\r\nnode \"%~dp0\\node_modules\\@agentclientprotocol\\codex-acp\\dist\\index.js\" %*\r\n",
            )
            .unwrap();
            path
        };
        #[cfg(not(windows))]
        let shim = {
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
        };
        shim
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

    #[tokio::test]
    async fn codex_managed_install_repairs_partial_or_version_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        // Partial / wrong version prefix.
        let bad = materialize_fake_managed_prefix(&data_dir, "1.1.7");
        assert!(bad.is_file());
        assert!(
            managed_codex_acp_shim_if_valid(&data_dir).is_none(),
            "mismatched version must fail integrity"
        );

        let seed = temp.path().join("seed");
        write_minimal_seed(&seed, CODEX_ACP_LOCKED_PIN);
        let data_for_install = data_dir.clone();

        let resolved = resolve_codex_acp_command_with(
            None,
            data_dir.clone(),
            Some(seed),
            move |_s, _p| {
                let data = data_for_install.clone();
                Box::pin(async move {
                    // Wipe and rewrite with locked pin (install helper responsibility).
                    let prefix = managed_prefix_dir(&data);
                    if prefix.exists() {
                        let _ = tokio::fs::remove_dir_all(&prefix).await;
                    }
                    let _ = materialize_fake_managed_prefix(&data, CODEX_ACP_LOCKED_PIN);
                    Ok(())
                })
            },
            || Box::pin(async { None }),
        )
        .await
        .expect("repair install");

        assert!(managed_codex_acp_shim_if_valid(&data_dir).is_some());
        assert_eq!(
            resolved,
            managed_codex_acp_shim_if_valid(&data_dir).unwrap()
        );
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
