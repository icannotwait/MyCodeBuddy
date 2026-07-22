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

/// Read tokens under the process-wide mutex. Distinguishes missing file
/// (empty map) from I/O / parse failures so title-key paths can return
/// [`CredentialState::Unavailable`] instead of treating errors as Absent.
#[cfg(not(feature = "tauri-runtime"))]
fn read_tokens_map() -> Result<std::collections::HashMap<String, String>, String> {
    let path = tokens_file_path();
    match std::fs::read_to_string(&path) {
        Ok(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(std::collections::HashMap::new());
            }
            serde_json::from_str(trimmed)
                .map_err(|e| format!("failed to parse token store: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::collections::HashMap::new())
        }
        Err(e) => Err(format!("failed to read token store: {e}")),
    }
}

/// Atomic publish: write temp sibling then rename into place so concurrent
/// readers never observe a truncated JSON body. Callers must hold
/// [`tokens_mutex`].
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

    // Windows rename fails if the destination exists; under the process
    // mutex remove+rename is coherent for in-process readers.
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("failed to replace token store: {e}"))?;
    }
    std::fs::rename(&tmp_path, &path)
        .map_err(|e| format!("failed to publish token store: {e}"))?;
    Ok(())
}

#[cfg(not(feature = "tauri-runtime"))]
pub fn set_token(account_id: &str, token: &str) -> Result<(), String> {
    let _guard = tokens_mutex()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut tokens = read_tokens_map().unwrap_or_default();
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
    let mut tokens = read_tokens_map().unwrap_or_default();
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
