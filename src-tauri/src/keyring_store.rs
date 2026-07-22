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
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Err(
                    "token store is empty or whitespace-only (corrupt or truncated)".into(),
                );
            }
            serde_json::from_str(trimmed)
                .map_err(|e| format!("failed to parse token store: {e}"))
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
        std::fs::rename(path, &bak)
            .map_err(|e| format!("failed to backup token store: {e}"))?;
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

#[cfg(not(feature = "tauri-runtime"))]
pub fn set_token(account_id: &str, token: &str) -> Result<(), String> {
    let _guard = tokens_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Never treat an unreadable store as empty — that would wipe unrelated
    // credentials on the subsequent write.
    let mut tokens = read_tokens_map()?;
    tokens.insert(token_key(account_id), token.to_string());
    write_tokens_map(&tokens)
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
    let _guard = tokens_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
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
    let _guard = tokens_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut tokens = match read_tokens_map() {
        Ok(m) => m,
        // Empty map on read error would wipe unrelated secrets on write —
        // surface the error instead.
        Err(e) => return Err(e),
    };
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
    let _guard = tokens_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Same fail-closed rule as set_token: unprovable prior state must not wipe.
    let mut tokens = read_tokens_map()?;
    tokens.insert(channel_token_key(channel_id), token.to_string());
    write_tokens_map(&tokens)
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn get_channel_token(channel_id: i32) -> Option<String> {
    let _guard = tokens_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    read_tokens_map()
        .ok()
        .and_then(|m| m.get(&channel_token_key(channel_id)).cloned())
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn delete_channel_token(channel_id: i32) -> Result<(), String> {
    let _guard = tokens_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut tokens = match read_tokens_map() {
        Ok(m) => m,
        Err(e) => return Err(e),
    };
    tokens.remove(&channel_token_key(channel_id));
    write_tokens_map(&tokens)
}

#[cfg(all(test, not(feature = "tauri-runtime")))]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

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
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
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
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
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
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
            std::fs::write(dir.path().join("tokens.json"), "  \n\t  \n")
                .expect("write whitespace");
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
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
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
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
            assert!(matches!(get_token_state("nobody"), CredentialState::Absent));
        });
    }

    #[test]
    fn atomic_publish_leaves_valid_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
            set_token("acct-a", "secret-a").expect("set a");
            set_token("acct-b", "secret-b").expect("set b");
            let raw = std::fs::read_to_string(dir.path().join("tokens.json")).expect("read");
            let map: std::collections::HashMap<String, String> =
                serde_json::from_str(&raw).expect("must be valid JSON after publish");
            assert_eq!(map.get("github-token:acct-a").map(String::as_str), Some("secret-a"));
            assert_eq!(map.get("github-token:acct-b").map(String::as_str), Some("secret-b"));
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
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
            match get_token_state("keep") {
                CredentialState::Present(s) => assert_eq!(s, "live-secret"),
                other => panic!("expected Present(live-secret), got {other:?}"),
            }
            assert_eq!(
                get_channel_token(1).as_deref(),
                Some("channel-secret")
            );
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
        std::fs::write(
            &bak,
            r#"{"github-token:survived":"from-backup"}"#,
        )
        .expect("write bak");
        assert!(!path.exists());

        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
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
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
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
}
