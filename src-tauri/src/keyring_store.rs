#[cfg(feature = "tauri-runtime")]
const SERVICE_NAME: &str = "codeg";

fn token_key(account_id: &str) -> String {
    format!("github-token:{}", account_id)
}

fn channel_token_key(channel_id: i32) -> String {
    format!("chat-channel:{}", channel_id)
}

/// Tri-state secret lookup. Callers that must fail-closed (title API key)
/// use this instead of mapping errors to "absent".
#[derive(Clone, PartialEq, Eq)]
pub enum CredentialState {
    Present(String),
    Absent,
    Unavailable,
}

impl std::fmt::Debug for CredentialState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CredentialState::Present(_) => f.write_str("Present(***)"),
            CredentialState::Absent => f.write_str("Absent"),
            CredentialState::Unavailable => f.write_str("Unavailable"),
        }
    }
}

// ── Tauri mode: OS keyring ──

#[cfg(feature = "tauri-runtime")]
pub fn set_token(account_id: &str, token: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, &token_key(account_id))
        .map_err(|e| format!("keyring init error: {e}"))?;
    entry
        .set_password(token)
        .map_err(|e| format!("keyring set error: {e}"))
}

#[cfg(feature = "tauri-runtime")]
pub fn get_token(account_id: &str) -> Option<String> {
    match get_token_state(account_id) {
        CredentialState::Present(s) => Some(s),
        CredentialState::Absent | CredentialState::Unavailable => None,
    }
}

#[cfg(feature = "tauri-runtime")]
pub fn get_token_state(account_id: &str) -> CredentialState {
    let entry = match keyring::Entry::new(SERVICE_NAME, &token_key(account_id)) {
        Ok(e) => e,
        Err(_) => return CredentialState::Unavailable,
    };
    match entry.get_password() {
        Ok(s) => CredentialState::Present(s),
        Err(keyring::Error::NoEntry) => CredentialState::Absent,
        Err(_) => CredentialState::Unavailable,
    }
}

#[cfg(feature = "tauri-runtime")]
pub fn delete_token(account_id: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, &token_key(account_id))
        .map_err(|e| format!("keyring init error: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keyring delete error: {e}")),
    }
}

// ── Server mode: file-based token store ──

#[cfg(not(feature = "tauri-runtime"))]
fn tokens_mutex() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(not(feature = "tauri-runtime"))]
fn tokens_file_path() -> std::path::PathBuf {
    tokens_file_path_for(std::env::var("CODEG_DATA_DIR").ok().as_deref())
}

/// Resolve the on-disk `tokens.json` path given an explicit
/// `CODEG_DATA_DIR` value (or `None` to fall back to the platform
/// default). Always returns an absolute path so subprocess credential
/// helpers — which inherit our env but run in git's CWD, not ours —
/// don't end up looking for `tokens.json` in the user's repo. Factored
/// out so tests can exercise path resolution without poking at process
/// env state.
#[cfg(not(feature = "tauri-runtime"))]
fn tokens_file_path_for(env_value: Option<&str>) -> std::path::PathBuf {
    let dir = env_value.map(std::path::PathBuf::from).unwrap_or_else(|| {
        dirs::data_dir()
            .map(|d| d.join("codeg"))
            .unwrap_or_else(|| std::path::PathBuf::from(".codeg-data"))
    });
    crate::git_credential::absolutize(&dir).join("tokens.json")
}

/// Backup sibling used during Windows-safe publish (and crash recovery).
#[cfg(not(feature = "tauri-runtime"))]
fn tokens_backup_path(path: &std::path::Path) -> std::path::PathBuf {
    path.with_file_name("tokens.json.bak")
}

/// Read tokens under the process-wide mutex. Distinguishes missing file
/// (empty map) from I/O / parse failures so title-key paths can return
/// [`CredentialState::Unavailable`] instead of treating errors as Absent.
///
/// Empty / whitespace-only files are treated as corrupt (`Err`), not as an
/// empty map — they can result from a truncated legacy write.
#[cfg(not(feature = "tauri-runtime"))]
fn read_tokens_map() -> Result<std::collections::HashMap<String, String>, String> {
    let path = tokens_file_path();
    #[cfg(unix)]
    if path.exists() {
        use std::os::unix::fs::PermissionsExt;
        if let Err(err) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            tracing::warn!(
                "[tokens] could not tighten {} to 0600: {err}",
                path.display()
            );
        }
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err(
                    "token store is empty or whitespace-only (corrupt or truncated)".into(),
                );
            }
            serde_json::from_str(trimmed).map_err(|e| format!("failed to parse token store: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // After a failed publish the live file may be gone while the
            // backup still holds credentials. That is Unavailable, not Absent.
            let bak = tokens_backup_path(&path);
            if bak.exists() {
                // Best-effort crash recovery: restore backup as the live store.
                match std::fs::rename(&bak, &path) {
                    Ok(()) => return read_tokens_map(),
                    Err(_) => {
                        return Err(
                            "token store missing after failed publish; backup present but unrecoverable"
                                .into(),
                        );
                    }
                }
            }
            Ok(std::collections::HashMap::new())
        }
        Err(e) => Err(format!("failed to read token store: {e}")),
    }
}

/// Publish `tmp_path` over `path` without unlink-first destruction.
///
/// Strategy: move the live file aside to a `.bak` sibling (if it exists),
/// rename the temp into place, then remove the backup. On install failure,
/// restore the backup so the previous store is not left destroyed.
///
/// `install` is the rename-or-fail step and is injectable in tests to simulate
/// a Windows-style publish failure after the live file was moved aside.
#[cfg(not(feature = "tauri-runtime"))]
fn publish_tokens_file_with<F>(
    tmp_path: &std::path::Path,
    path: &std::path::Path,
    install: F,
) -> Result<(), String>
where
    F: FnOnce(&std::path::Path, &std::path::Path) -> Result<(), String>,
{
    let bak = tokens_backup_path(path);
    let had_live = path.exists();
    if had_live {
        // Clear any stale backup so rename dest is free on Windows.
        let _ = std::fs::remove_file(&bak);
        std::fs::rename(path, &bak).map_err(|e| format!("failed to backup token store: {e}"))?;
    }

    match install(tmp_path, path) {
        Ok(()) => {
            if had_live {
                let _ = std::fs::remove_file(&bak);
            }
            // Best-effort cleanup if install left the tmp behind (should not).
            let _ = std::fs::remove_file(tmp_path);
            Ok(())
        }
        Err(e) => {
            // Restore previous credentials when possible.
            if had_live {
                match std::fs::rename(&bak, path) {
                    Ok(()) => {}
                    Err(restore_err) => {
                        // Leave .bak in place for a later read-time recovery
                        // attempt; surface the original publish error.
                        let _ = std::fs::remove_file(tmp_path);
                        return Err(format!(
                            "failed to publish token store: {e}; also failed to restore backup: {restore_err}"
                        ));
                    }
                }
            }
            let _ = std::fs::remove_file(tmp_path);
            Err(format!("failed to publish token store: {e}"))
        }
    }
}

/// Atomic publish: write temp sibling then install into place so concurrent
/// readers never observe a truncated JSON body. Never deletes the live file
/// before a successful install (backup-and-restore on replace). Callers must
/// hold [`tokens_mutex`].
#[cfg(not(feature = "tauri-runtime"))]
fn write_tokens_map(tokens: &std::collections::HashMap<String, String>) -> Result<(), String> {
    let path = tokens_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create token store directory: {e}"))?;
    }
    let json = serde_json::to_string_pretty(tokens)
        .map_err(|e| format!("failed to serialize tokens: {e}"))?;

    let tmp_path = path.with_file_name("tokens.json.tmp");
    {
        use std::io::Write;
        #[cfg(unix)]
        let mut f = {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp_path)
                .map_err(|e| format!("failed to create token store temp: {e}"))?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("failed to secure token store temp: {e}"))?;
            file
        };
        #[cfg(not(unix))]
        let mut f = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("failed to create token store temp: {e}"))?;
        f.write_all(json.as_bytes())
            .map_err(|e| format!("failed to write token store temp: {e}"))?;
        f.sync_all()
            .map_err(|e| format!("failed to sync token store temp: {e}"))?;
    }

    publish_tokens_file_with(&tmp_path, &path, |tmp, dest| {
        std::fs::rename(tmp, dest).map_err(|e| e.to_string())
    })
}

/// Test-only: after a successful tokens.json publish, optionally hold
/// `tokens_mutex` until the test releases. Proves a non-title write completed
/// under the process lock before a claim/read can observe the store.
#[cfg(all(test, not(feature = "tauri-runtime")))]
pub mod write_hold_hooks {
    use std::sync::{Condvar, Mutex};

    struct State {
        /// When true, the next successful `set_token` write will hold the
        /// tokens mutex until [`release`].
        armed: bool,
        /// Writer finished publish and is blocked before unlock.
        holding: bool,
        /// Test may proceed past the hold.
        release: bool,
    }

    static STATE: Mutex<State> = Mutex::new(State {
        armed: false,
        holding: false,
        release: false,
    });
    static CV: Condvar = Condvar::new();

    pub fn reset() {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        // Force-release any in-flight hold so panic/Drop paths cannot deadlock
        // a writer still inside `maybe_hold_after_write`.
        *g = State {
            armed: false,
            holding: false,
            release: true,
        };
        CV.notify_all();
    }

    /// Arm a one-shot hold after the next successful `set_token` publish.
    pub fn arm() {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        *g = State {
            armed: true,
            holding: false,
            release: false,
        };
        CV.notify_all();
    }

    /// Block until a writer has published and is holding `tokens_mutex`.
    pub fn wait_until_holding() {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        while !g.holding {
            g = CV.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Allow the holding writer to release `tokens_mutex`.
    pub fn release() {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        g.release = true;
        CV.notify_all();
    }

    /// Called from `set_token` after a successful publish, while still holding
    /// the process-wide tokens mutex.
    pub(super) fn maybe_hold_after_write() {
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if !g.armed {
            return;
        }
        g.armed = false;
        g.holding = true;
        CV.notify_all();
        while !g.release {
            g = CV.wait(g).unwrap_or_else(|e| e.into_inner());
        }
        g.holding = false;
        g.release = false;
    }
}

/// Test-only: claim-owned signal when a tokens read is about to acquire
/// `tokens_mutex`. Used with [`write_hold_hooks`] to prove **this** claim/read
/// overlaps a held write (foreign `get_token_state` callers cannot ack).
///
/// **SuiteGuard required:** [`reset`], [`arm_claim_watch`], and wait helpers
/// that mutate/observe the process-global watch must run on the
/// [`crate::auto_title::title_key::test_hooks::SuiteGuard`] owning thread so a
/// parallel test cannot re-arm generation B while another waits for A.
#[cfg(all(test, not(feature = "tauri-runtime")))]
pub mod read_attempt_hooks {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    // Generation installed on the claim task under test via `with_claim_gen`.
    // Only `get_token_state` calls whose task carries this gen may ack a watch.
    tokio::task_local! {
        static CLAIM_READ_GEN: u64;
    }

    struct State {
        /// Generation currently watched (`0` = none).
        watch_gen: u64,
        /// Generation that acked (`0` = none for this arm).
        acked_gen: u64,
    }

    static NEXT_GEN: AtomicU64 = AtomicU64::new(1);
    static STATE: Mutex<State> = Mutex::new(State {
        watch_gen: 0,
        acked_gen: 0,
    });
    static CV: Condvar = Condvar::new();

    fn require_suite_owner(op: &str) {
        assert!(
            crate::auto_title::title_key::test_hooks::is_suite_owner(),
            "read_attempt_hooks::{op} requires SuiteGuard on the owning thread \
             (serialize with title-key suite; no parallel re-arm)",
            op = op
        );
    }

    pub fn reset() {
        require_suite_owner("reset");
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        *g = State {
            watch_gen: 0,
            acked_gen: 0,
        };
        CV.notify_all();
    }

    /// Arm a watch for a fresh claim generation. Pair with [`with_claim_gen`] on
    /// the claim future and [`wait_until_acked`] for that same gen.
    ///
    /// Requires [`SuiteGuard`] ownership so only one suite can arm at a time.
    pub fn arm_claim_watch() -> u64 {
        require_suite_owner("arm_claim_watch");
        let gen = NEXT_GEN.fetch_add(1, Ordering::SeqCst);
        debug_assert!(gen != 0, "claim-watch generation must be non-zero");
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        *g = State {
            watch_gen: gen,
            acked_gen: 0,
        };
        CV.notify_all();
        gen
    }

    /// Run `f` as the claim task that owns `gen` (task-local; survives awaits).
    pub async fn with_claim_gen<F, T>(gen: u64, f: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        CLAIM_READ_GEN.scope(gen, f).await
    }

    /// Block until the claim task carrying `gen` has entered the pre-mutex path.
    pub fn wait_until_acked(gen: u64) {
        require_suite_owner("wait_until_acked");
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        while g.acked_gen != gen {
            g = CV.wait(g).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Whether `gen` has already acked (for negative tests / diagnostics).
    pub fn is_acked(gen: u64) -> bool {
        require_suite_owner("is_acked");
        let g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        g.acked_gen == gen
    }

    /// Wait up to `timeout` for `gen` to ack. Returns `true` if acked.
    pub fn wait_until_acked_timeout(gen: u64, timeout: Duration) -> bool {
        require_suite_owner("wait_until_acked_timeout");
        let deadline = Instant::now() + timeout;
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        while g.acked_gen != gen {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (next, result) = CV
                .wait_timeout(g, deadline.saturating_duration_since(now))
                .unwrap_or_else(|e| e.into_inner());
            g = next;
            if result.timed_out() && g.acked_gen != gen {
                return false;
            }
        }
        true
    }

    /// Called from `get_token_state` immediately before `tokens_mutex` lock.
    /// Only acks when the current task carries the armed claim generation.
    pub(super) fn note_before_mutex() {
        let gen = match CLAIM_READ_GEN.try_with(|g| *g) {
            Ok(g) if g != 0 => g,
            _ => return, // foreign / unscoped reader — do not ack
        };
        let mut g = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if g.watch_gen != gen {
            return;
        }
        if g.acked_gen == gen {
            return;
        }
        g.acked_gen = gen;
        CV.notify_all();
    }
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn set_token(account_id: &str, token: &str) -> Result<(), String> {
    let _guard = tokens_mutex().lock().unwrap_or_else(|e| e.into_inner());
    // Never treat an unreadable store as empty — that would wipe unrelated
    // credentials on the subsequent write.
    let mut tokens = read_tokens_map()?;
    tokens.insert(token_key(account_id), token.to_string());
    write_tokens_map(&tokens)?;
    #[cfg(test)]
    write_hold_hooks::maybe_hold_after_write();
    Ok(())
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn get_token(account_id: &str) -> Option<String> {
    match get_token_state(account_id) {
        CredentialState::Present(s) => Some(s),
        CredentialState::Absent | CredentialState::Unavailable => None,
    }
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn get_token_state(account_id: &str) -> CredentialState {
    // Note *before* lock so tests can observe a reader blocked/waiting while a
    // writer still holds tokens_mutex (true write/read overlap).
    #[cfg(test)]
    read_attempt_hooks::note_before_mutex();
    let _guard = tokens_mutex().lock().unwrap_or_else(|e| e.into_inner());
    match read_tokens_map() {
        Ok(map) => match map.get(&token_key(account_id)) {
            Some(v) => CredentialState::Present(v.clone()),
            None => CredentialState::Absent,
        },
        Err(_) => CredentialState::Unavailable,
    }
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn delete_token(account_id: &str) -> Result<(), String> {
    let _guard = tokens_mutex().lock().unwrap_or_else(|e| e.into_inner());
    // Propagate read errors: an empty map on failure would wipe unrelated secrets.
    let mut tokens = read_tokens_map()?;
    tokens.remove(&token_key(account_id));
    write_tokens_map(&tokens)
}

// ── Chat channel token helpers ──
// Reuse the same storage mechanism (keyring or file) with a different key prefix.

#[cfg(feature = "tauri-runtime")]
pub fn set_channel_token(channel_id: i32, token: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, &channel_token_key(channel_id))
        .map_err(|e| format!("keyring init error: {e}"))?;
    entry
        .set_password(token)
        .map_err(|e| format!("keyring set error: {e}"))
}

#[cfg(feature = "tauri-runtime")]
pub fn get_channel_token(channel_id: i32) -> Option<String> {
    let entry = keyring::Entry::new(SERVICE_NAME, &channel_token_key(channel_id)).ok()?;
    entry.get_password().ok()
}

#[cfg(feature = "tauri-runtime")]
pub fn delete_channel_token(channel_id: i32) -> Result<(), String> {
    let entry = keyring::Entry::new(SERVICE_NAME, &channel_token_key(channel_id))
        .map_err(|e| format!("keyring init error: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keyring delete error: {e}")),
    }
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn set_channel_token(channel_id: i32, token: &str) -> Result<(), String> {
    let _guard = tokens_mutex().lock().unwrap_or_else(|e| e.into_inner());
    // Same fail-closed rule as set_token: unprovable prior state must not wipe.
    let mut tokens = read_tokens_map()?;
    tokens.insert(channel_token_key(channel_id), token.to_string());
    write_tokens_map(&tokens)
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn get_channel_token(channel_id: i32) -> Option<String> {
    let _guard = tokens_mutex().lock().unwrap_or_else(|e| e.into_inner());
    read_tokens_map()
        .ok()
        .and_then(|m| m.get(&channel_token_key(channel_id)).cloned())
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn delete_channel_token(channel_id: i32) -> Result<(), String> {
    let _guard = tokens_mutex().lock().unwrap_or_else(|e| e.into_inner());
    // Propagate read errors: an empty map on failure would wipe unrelated secrets.
    let mut tokens = read_tokens_map()?;
    tokens.remove(&channel_token_key(channel_id));
    write_tokens_map(&tokens)
}

/// Process-wide lock for tests that mutate `CODEG_DATA_DIR` (or delete the
/// directory it names). `std::env::set_var` / `temp_env` are process-global;
/// concurrent tests that flip the var mid-flight otherwise race on
/// `tokens.json` publish (ENOENT rename) and lookup.
#[cfg(all(test, not(feature = "tauri-runtime")))]
pub fn codeg_data_dir_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

#[cfg(all(test, not(feature = "tauri-runtime")))]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn with_codeg_data_dir<T>(data_dir: &str, f: impl FnOnce() -> T) -> T {
        let _guard = codeg_data_dir_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir), f)
    }

    #[test]
    fn test_tokens_file_path_absolutizes_relative_env() {
        // Regression: a relative `CODEG_DATA_DIR=data` previously made
        // `tokens.json` resolve against the helper subprocess's CWD (i.e.
        // git's repo dir), even after we'd absolutized the path used for
        // the database. The token store must always land on an absolute
        // path so DB lookup and token lookup point at the same root.
        let cwd = std::env::current_dir().expect("cwd");
        let resolved = tokens_file_path_for(Some("data"));
        assert!(
            resolved.is_absolute(),
            "tokens path must be absolute, got: {}",
            resolved.display()
        );
        assert_eq!(resolved, cwd.join("data").join("tokens.json"));
    }

    #[test]
    fn test_tokens_file_path_absolute_env_unchanged() {
        let data_dir = std::env::current_dir().expect("cwd").join("codeg-data");
        let data_dir_str = data_dir.to_string_lossy().to_string();
        let resolved = tokens_file_path_for(Some(&data_dir_str));
        assert_eq!(resolved, data_dir.join("tokens.json"));
    }

    #[test]
    fn test_tokens_file_path_default_when_unset() {
        // No env var → derived from `dirs::data_dir()` (always absolute on
        // every platform we ship to). Just verify we end at `tokens.json`
        // and that the result is absolute, not the literal default.
        let resolved = tokens_file_path_for(None);
        assert!(resolved.is_absolute());
        assert!(resolved.ends_with("tokens.json"));
    }

    #[test]
    fn credential_state_debug_redacts_secret() {
        let state = CredentialState::Present("raw-secret-value".into());
        let rendered = format!("{state:?}");
        assert_eq!(rendered, "Present(***)");
        assert!(!rendered.contains("raw-secret"));
    }

    #[test]
    fn corrupt_tokens_json_is_unavailable_not_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        with_codeg_data_dir(data_dir.as_str(), || {
            std::fs::write(dir.path().join("tokens.json"), "{\"github-token:x\":")
                .expect("write truncated json");
            let state = get_token_state("x");
            assert!(
                matches!(state, CredentialState::Unavailable),
                "truncated JSON must be Unavailable, got {state:?}"
            );
            // Legacy Option path still maps Unavailable → None
            assert!(get_token("x").is_none());
        });
    }

    #[test]
    fn empty_tokens_json_is_unavailable_not_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        with_codeg_data_dir(data_dir.as_str(), || {
            std::fs::write(dir.path().join("tokens.json"), "").expect("write empty");
            let state = get_token_state("x");
            assert!(
                matches!(state, CredentialState::Unavailable),
                "empty file must be Unavailable, got {state:?}"
            );
            assert!(get_token("x").is_none());
        });
    }

    #[test]
    fn whitespace_only_tokens_json_is_unavailable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        with_codeg_data_dir(data_dir.as_str(), || {
            std::fs::write(dir.path().join("tokens.json"), "  \n\t  \n").expect("write whitespace");
            let state = get_token_state("any");
            assert!(
                matches!(state, CredentialState::Unavailable),
                "whitespace-only must be Unavailable, got {state:?}"
            );
        });
    }

    #[test]
    fn set_token_fails_when_store_unreadable_without_wiping() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        with_codeg_data_dir(data_dir.as_str(), || {
            let path = dir.path().join("tokens.json");
            let corrupt = "{\"github-token:keep-me\":\"original-secret\""; // truncated
            std::fs::write(&path, corrupt).expect("write corrupt");

            let err = set_token("new-acct", "should-not-land").expect_err("set must fail");
            assert!(
                err.contains("parse") || err.contains("token store"),
                "error should mention store/parse failure, got: {err}"
            );

            // Prior on-disk bytes must be untouched (no wipe-to-single-entry publish).
            let after = std::fs::read_to_string(&path).expect("read after failed set");
            assert_eq!(after, corrupt);

            // Channel set has the same fail-closed contract.
            let err2 = set_channel_token(42, "nope").expect_err("channel set must fail");
            assert!(
                err2.contains("parse") || err2.contains("token store"),
                "channel set error: {err2}"
            );
            let after2 = std::fs::read_to_string(&path).expect("read after channel set");
            assert_eq!(after2, corrupt);
        });
    }

    #[test]
    fn missing_file_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        with_codeg_data_dir(data_dir.as_str(), || {
            assert!(matches!(get_token_state("nobody"), CredentialState::Absent));
        });
    }

    #[test]
    fn atomic_publish_leaves_valid_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        with_codeg_data_dir(data_dir.as_str(), || {
            set_token("acct-a", "secret-a").expect("set a");
            set_token("acct-b", "secret-b").expect("set b");
            let raw = std::fs::read_to_string(dir.path().join("tokens.json")).expect("read");
            let map: std::collections::HashMap<String, String> =
                serde_json::from_str(&raw).expect("must be valid JSON after publish");
            assert_eq!(
                map.get("github-token:acct-a").map(String::as_str),
                Some("secret-a")
            );
            assert_eq!(
                map.get("github-token:acct-b").map(String::as_str),
                Some("secret-b")
            );
            // Temp file must not be left behind
            assert!(!dir.path().join("tokens.json.tmp").exists());
            assert!(!dir.path().join("tokens.json.bak").exists());
        });
    }

    /// Simulated install failure after live file was moved aside must restore
    /// the previous store (no wipe / no permanent Absent).
    #[test]
    fn publish_restores_backup_when_install_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tokens.json");
        let previous = r#"{
  "github-token:keep": "live-secret",
  "chat-channel:1": "channel-secret"
}"#;
        std::fs::write(&path, previous).expect("seed live store");

        let tmp = dir.path().join("tokens.json.tmp");
        std::fs::write(&tmp, r#"{"github-token:new":"wiped-if-published"}"#).expect("tmp");

        let err = publish_tokens_file_with(&tmp, &path, |_tmp, _dest| {
            Err("simulated rename failure".into())
        })
        .expect_err("install must fail");
        assert!(
            err.contains("simulated rename failure"),
            "expected simulated error, got: {err}"
        );

        // Live file restored with original credentials.
        let restored = std::fs::read_to_string(&path).expect("live must exist after restore");
        assert_eq!(restored, previous);
        assert!(!dir.path().join("tokens.json.bak").exists());
        assert!(!tmp.exists(), "tmp should be cleaned up on failure");

        // And the public API still sees Present, not Absent/Unavailable wipe.
        let data_dir = dir.path().to_string_lossy().to_string();
        with_codeg_data_dir(data_dir.as_str(), || {
            match get_token_state("keep") {
                CredentialState::Present(s) => assert_eq!(s, "live-secret"),
                other => panic!("expected Present(live-secret), got {other:?}"),
            }
            assert_eq!(get_channel_token(1).as_deref(), Some("channel-secret"));
        });
    }

    /// If restore itself fails, leave `.bak` so a later read reports Unavailable
    /// (or recovers) rather than silent Absent.
    #[test]
    fn missing_live_with_backup_is_unavailable_or_recovered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        let path = dir.path().join("tokens.json");
        let bak = dir.path().join("tokens.json.bak");
        // Simulate mid-publish crash: live gone, backup holds secrets.
        std::fs::write(&bak, r#"{"github-token:survived":"from-backup"}"#).expect("write bak");
        assert!(!path.exists());

        with_codeg_data_dir(data_dir.as_str(), || {
            match get_token_state("survived") {
                CredentialState::Present(s) => {
                    // Read-time recovery renamed .bak → live.
                    assert_eq!(s, "from-backup");
                    assert!(path.exists());
                    assert!(!bak.exists());
                }
                CredentialState::Unavailable => {
                    // Acceptable if recovery rename failed in the environment.
                    assert!(bak.exists() || !path.exists());
                }
                CredentialState::Absent => {
                    panic!("must not treat destroyed store as Absent");
                }
            }
        });
    }

    /// Concurrent RMW + readers under the process mutex must never observe
    /// truncated JSON (Unavailable) or panic.
    #[test]
    fn concurrent_reads_never_see_truncated_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        with_codeg_data_dir(data_dir.as_str(), || {
            set_token("seed", "initial").expect("seed");

            let threads = 8usize;
            let iters = 40usize;
            let barrier = Arc::new(Barrier::new(threads));
            let unavailable = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut handles = Vec::new();

            for t in 0..threads {
                let barrier = Arc::clone(&barrier);
                let unavailable = Arc::clone(&unavailable);
                let account = format!("worker-{t}");
                handles.push(thread::spawn(move || {
                    barrier.wait();
                    for i in 0..iters {
                        let secret = format!("val-{t}-{i}");
                        set_token(&account, &secret).expect("set under concurrency");
                        match get_token_state(&account) {
                            CredentialState::Present(s) => {
                                assert!(s.starts_with("val-"), "unexpected secret shape");
                            }
                            CredentialState::Absent => {
                                // Another writer may have RMW-deleted nothing for
                                // this key, but Present is expected after our set;
                                // Absent would mean lost update, not truncation.
                            }
                            CredentialState::Unavailable => {
                                unavailable.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        // Cross-read seed / peers should also never be Unavailable
                        if matches!(get_token_state("seed"), CredentialState::Unavailable) {
                            unavailable.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }));
            }

            for h in handles {
                h.join().expect("thread join");
            }

            let n = unavailable.load(std::sync::atomic::Ordering::Relaxed);
            assert_eq!(
                n, 0,
                "concurrent readers observed Unavailable (truncated/corrupt JSON) {n} times"
            );

            let raw = std::fs::read_to_string(dir.path().join("tokens.json")).expect("read final");
            let _: std::collections::HashMap<String, String> =
                serde_json::from_str(&raw).expect("final tokens.json must parse");
        });
    }

    /// Foreign `get_token_state` must not ack a claim-owned read watch.
    ///
    /// Holds SuiteGuard so arm/reset cannot race a parallel suite's claim watch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_attempt_ack_requires_claim_gen() {
        let _suite = crate::auto_title::title_key::test_hooks::SuiteGuard::enter();
        read_attempt_hooks::reset();
        let gen = read_attempt_hooks::arm_claim_watch();

        // Unscoped / foreign readers must not satisfy the wait.
        let _ = get_token_state("foreign-unscoped");
        assert!(
            !read_attempt_hooks::is_acked(gen),
            "foreign get_token_state must not ack claim-owned watch"
        );
        assert!(
            !read_attempt_hooks::wait_until_acked_timeout(
                gen,
                std::time::Duration::from_millis(50)
            ),
            "watch must stay unacked without claim gen scope"
        );

        // Scoped claim task acks.
        read_attempt_hooks::with_claim_gen(gen, async {
            let _ = get_token_state("claim-owned");
        })
        .await;
        assert!(
            read_attempt_hooks::is_acked(gen),
            "claim-scoped get_token_state must ack its own gen"
        );

        read_attempt_hooks::reset();
    }
}
