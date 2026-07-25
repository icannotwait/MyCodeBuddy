//! Title API key storage helpers: tri-state keyring read + non-secret fingerprint.
//!
//! Secrets never appear in Debug output. Fingerprints are SHA-256 hex of the
//! UTF-8 secret and are safe to store in app_metadata.

use sha2::{Digest, Sha256};

use crate::keyring_store::{self, CredentialState};

/// Keyring / tokens.json account id for the automatic-title API key.
pub const TITLE_API_KEY_ACCOUNT: &str = "auto_title_api_key";

/// Result of loading the title API key from the secret store.
///
/// `Unavailable` means the backend could not prove presence or absence
/// (I/O error, corrupt tokens.json, keyring failure). Callers must fail-closed.
#[derive(Clone)]
pub enum TitleKeyState {
    Present(String),
    Absent,
    Unavailable,
}

impl std::fmt::Debug for TitleKeyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TitleKeyState::Present(_) => f.write_str("Present(***)"),
            TitleKeyState::Absent => f.write_str("Absent"),
            TitleKeyState::Unavailable => f.write_str("Unavailable"),
        }
    }
}

/// `hex_lower(SHA-256(utf8(secret)))` — non-secret identity of a key value.
pub fn title_key_fingerprint(secret: &str) -> String {
    format!("{:x}", Sha256::digest(secret.as_bytes()))
}

/// Load the title API key. Never maps backend errors to [`TitleKeyState::Absent`].
pub fn get_title_api_key() -> TitleKeyState {
    #[cfg(any(test, feature = "test-utils"))]
    {
        if let Some(state) = test_hooks::take_override_get() {
            return state;
        }
    }
    let state = match keyring_store::get_token_state(TITLE_API_KEY_ACCOUNT) {
        CredentialState::Present(s) => TitleKeyState::Present(s),
        CredentialState::Absent => TitleKeyState::Absent,
        CredentialState::Unavailable => TitleKeyState::Unavailable,
    };
    #[cfg(any(test, feature = "test-utils"))]
    {
        test_hooks::note_real_get();
    }
    state
}

/// Persist the title API key secret.
pub fn set_title_api_key(secret: &str) -> Result<(), String> {
    #[cfg(any(test, feature = "test-utils"))]
    {
        if test_hooks::take_fail_next_set() {
            return Err("injected title key set failure".into());
        }
    }
    keyring_store::set_token(TITLE_API_KEY_ACCOUNT, secret)
}

/// Delete the title API key secret (idempotent if already absent).
pub fn delete_title_api_key() -> Result<(), String> {
    #[cfg(any(test, feature = "test-utils"))]
    {
        if test_hooks::take_fail_next_delete() {
            return Err("injected title key delete failure".into());
        }
    }
    keyring_store::delete_token(TITLE_API_KEY_ACCOUNT)
}

/// Test-only injectors for fail-closed write-sequence coverage.
///
/// **Exclusive SuiteGuard:** at most one live suite process-wide (mutex).
/// Override / fail-next hooks only apply on the **owning thread** that called
/// [`SuiteGuard::enter`] (thread-local `SUITE_OWNER`). `push_override_get`,
/// `allow_real_gets`, and `fail_next_*` panic unless the current thread is the
/// suite owner.
///
/// [`super::get_title_api_key`] drains the override queue **only** when
/// `is_suite_owner()` is true. Parallel harness threads (and any other OS
/// thread that did not enter the suite) hit the real keyring and never steal
/// overrides — even while `suite_active()` is process-true for the holder.
///
/// Same-thread Tokio tasks (default `current_thread` test runtime, including
/// coordinator workers scheduled on that thread) share the owner TLS and may
/// consume overrides. Multi-task paths on **other OS threads** must use the
/// real keyring under isolated `CODEG_DATA_DIR` instead of overrides.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_hooks {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    use super::TitleKeyState;

    #[derive(Default)]
    struct Hooks {
        fail_next_set: bool,
        fail_next_delete: bool,
        /// Real store reads to allow before overrides apply.
        allow_real_gets: usize,
        /// One-shot get overrides (front of queue consumed first).
        override_gets: Vec<TitleKeyState>,
    }

    static HOOKS: Mutex<Hooks> = Mutex::new(Hooks {
        fail_next_set: false,
        fail_next_delete: false,
        allow_real_gets: 0,
        override_gets: Vec::new(),
    });

    /// Process-wide suite lock: title-key hooks are process-global, so async
    /// tests that push overrides / fail-next flags must hold this for the
    /// whole test body or parallel harness runs steal each other's queues.
    ///
    /// Same role as `temp_env`'s env mutex for `CODEG_DATA_DIR` tests.
    /// **Exclusive:** only one suite may be active at a time.
    static SUITE_LOCK: Mutex<()> = Mutex::new(());

    /// Number of live [`SuiteGuard`]s (0 or 1 under the exclusive mutex).
    static SUITE_ACTIVE: AtomicUsize = AtomicUsize::new(0);

    // Thread-local suite ownership. Set for the lifetime of SuiteGuard on the
    // entering OS thread. Parallel test threads never see this flag.
    thread_local! {
        static SUITE_OWNER: Cell<bool> = const { Cell::new(false) };
    }

    /// RAII: exclusive suite lock + owner TLS + hook reset on enter/exit.
    ///
    /// Title-key override hooks only apply on the owning thread while held.
    /// The held mutex is `!Send`, so the suite body stays on the enter thread
    /// (compatible with `block_on` under multi-thread runtimes).
    pub struct SuiteGuard {
        _lock: MutexGuard<'static, ()>,
    }

    impl SuiteGuard {
        pub fn enter() -> Self {
            let lock = SUITE_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            reset();
            SUITE_OWNER.with(|c| c.set(true));
            SUITE_ACTIVE.fetch_add(1, Ordering::SeqCst);
            Self { _lock: lock }
        }
    }

    impl Drop for SuiteGuard {
        fn drop(&mut self) {
            reset();
            SUITE_OWNER.with(|c| c.set(false));
            let prev = SUITE_ACTIVE.fetch_sub(1, Ordering::SeqCst);
            debug_assert!(prev > 0, "SuiteGuard drop with SUITE_ACTIVE==0");
        }
    }

    /// Whether a [`SuiteGuard`] is currently held in this process.
    pub fn suite_active() -> bool {
        SUITE_ACTIVE.load(Ordering::SeqCst) > 0
    }

    /// Whether the **current OS thread** owns the live [`SuiteGuard`].
    ///
    /// Override drain and claim-watch arm/reset require this, not merely
    /// [`suite_active`].
    pub fn is_suite_owner() -> bool {
        SUITE_OWNER.with(|c| c.get())
    }

    /// Hold the suite mutex without activating hooks (`suite_active() == false`).
    /// Used by tests that must prove unguarded hook ops panic, without racing
    /// parallel suite holders.
    #[cfg(test)]
    pub fn with_exclusive_idle_suite<R>(f: impl FnOnce() -> R) -> R {
        let _lock = SUITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            !suite_active(),
            "idle suite section requires no live SuiteGuard"
        );
        assert!(
            !is_suite_owner(),
            "idle suite section must not set suite owner TLS"
        );
        f()
    }

    fn require_suite(op: &str) {
        assert!(
            is_suite_owner(),
            "title-key test hook `{op}` requires SuiteGuard on the owning thread \
             (hold SuiteGuard::enter() for the whole test body on this thread)"
        );
    }

    pub fn reset() {
        let mut g = HOOKS.lock().expect("hooks");
        *g = Hooks::default();
    }

    pub fn fail_next_set() {
        require_suite("fail_next_set");
        HOOKS.lock().expect("hooks").fail_next_set = true;
    }

    pub fn fail_next_delete() {
        require_suite("fail_next_delete");
        HOOKS.lock().expect("hooks").fail_next_delete = true;
    }

    /// Allow the next `n` [`super::get_title_api_key`] calls to hit the real
    /// store before queued overrides are consumed.
    ///
    /// Requires suite ownership on the current thread.
    pub fn allow_real_gets(n: usize) {
        require_suite("allow_real_gets");
        HOOKS.lock().expect("hooks").allow_real_gets = n;
    }

    /// Queue one-shot get overrides (consumed in order by [`super::get_title_api_key`]).
    ///
    /// Requires suite ownership on the current thread. Panics otherwise so
    /// unguarded tests cannot leave process-global queue poison for others.
    pub fn push_override_get(state: TitleKeyState) {
        require_suite("push_override_get");
        HOOKS.lock().expect("hooks").override_gets.push(state);
    }

    pub(super) fn take_fail_next_set() -> bool {
        if !is_suite_owner() {
            return false;
        }
        let mut g = HOOKS.lock().expect("hooks");
        let v = g.fail_next_set;
        g.fail_next_set = false;
        v
    }

    pub(super) fn take_fail_next_delete() -> bool {
        if !is_suite_owner() {
            return false;
        }
        let mut g = HOOKS.lock().expect("hooks");
        let v = g.fail_next_delete;
        g.fail_next_delete = false;
        v
    }

    pub(super) fn take_override_get() -> Option<TitleKeyState> {
        // Only the suite-owning thread may drain overrides. Foreign harness
        // threads see suite_active but must hit the real keyring.
        if !is_suite_owner() {
            if !suite_active() {
                let g = HOOKS.lock().expect("hooks");
                assert!(
                    g.override_gets.is_empty() && g.allow_real_gets == 0,
                    "title-key override queue non-empty without SuiteGuard \
                     (hooks only apply on the SuiteGuard owning thread)"
                );
            }
            return None;
        }
        let mut g = HOOKS.lock().expect("hooks");
        if g.allow_real_gets > 0 {
            return None;
        }
        if g.override_gets.is_empty() {
            None
        } else {
            Some(g.override_gets.remove(0))
        }
    }

    pub(super) fn note_real_get() {
        if !is_suite_owner() {
            return;
        }
        let mut g = HOOKS.lock().expect("hooks");
        if g.allow_real_gets > 0 {
            g.allow_real_gets -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable_sha256_hex_lower() {
        // FIPS 180-2 / NIST test vector for "abc"
        assert_eq!(
            title_key_fingerprint("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            title_key_fingerprint(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // Same input always same output
        let a = title_key_fingerprint("sk-test-secret-value");
        let b = title_key_fingerprint("sk-test-secret-value");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn debug_redacts_present_secret() {
        let state = TitleKeyState::Present("sk-super-secret-do-not-leak".into());
        let rendered = format!("{state:?}");
        assert_eq!(rendered, "Present(***)");
        assert!(!rendered.contains("sk-super-secret"));
        assert!(!rendered.contains("do-not-leak"));

        assert_eq!(format!("{:?}", TitleKeyState::Absent), "Absent");
        assert_eq!(format!("{:?}", TitleKeyState::Unavailable), "Unavailable");
    }

    /// Server-mode file store: corrupt tokens.json must be Unavailable, not Absent.
    #[cfg(not(feature = "tauri-runtime"))]
    #[test]
    fn get_title_api_key_unavailable_on_corrupt_tokens_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        // temp_env first, then suite lock — same order as concurrent service tests
        // so we never reverse-lock against `async_with_vars` + SuiteGuard.
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
            let _suite = test_hooks::SuiteGuard::enter();
            let path = dir.path().join("tokens.json");
            std::fs::write(&path, "{not-valid-json").expect("write corrupt");
            let state = get_title_api_key();
            assert!(
                matches!(state, TitleKeyState::Unavailable),
                "expected Unavailable, got {state:?}"
            );
            // Debug still redacts (nothing to leak, but format is stable)
            assert_eq!(format!("{state:?}"), "Unavailable");
        });
    }

    #[cfg(not(feature = "tauri-runtime"))]
    #[test]
    fn set_get_delete_title_api_key_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
            let _suite = test_hooks::SuiteGuard::enter();
            assert!(matches!(get_title_api_key(), TitleKeyState::Absent));

            set_title_api_key("sk-roundtrip-secret").expect("set");
            match get_title_api_key() {
                TitleKeyState::Present(s) => assert_eq!(s, "sk-roundtrip-secret"),
                other => panic!("expected Present, got {other:?}"),
            }

            delete_title_api_key().expect("delete");
            assert!(matches!(get_title_api_key(), TitleKeyState::Absent));
            // Idempotent delete
            delete_title_api_key().expect("delete again");
        });
    }

    /// Override queue hooks only apply under SuiteGuard; unguarded push panics.
    #[test]
    fn push_override_without_suite_guard_panics() {
        let panicked = test_hooks::with_exclusive_idle_suite(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                test_hooks::push_override_get(TitleKeyState::Absent);
            }))
            .is_err()
        });
        assert!(panicked, "push_override_get without SuiteGuard must panic");
    }

    /// Without SuiteGuard, gets hit the real store and never drain overrides.
    #[cfg(not(feature = "tauri-runtime"))]
    #[test]
    fn get_without_suite_guard_ignores_hooks_uses_real_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
            // Under guard: write a real secret, then leave no overrides.
            {
                let _suite = test_hooks::SuiteGuard::enter();
                set_title_api_key("sk-real-only").expect("set");
            }
            assert!(!test_hooks::suite_active());
            assert!(!test_hooks::is_suite_owner());
            match get_title_api_key() {
                TitleKeyState::Present(s) => assert_eq!(s, "sk-real-only"),
                other => panic!("expected real Present, got {other:?}"),
            }
        });
    }

    /// Foreign OS threads must not drain overrides while suite is process-active.
    #[cfg(not(feature = "tauri-runtime"))]
    #[test]
    fn foreign_thread_does_not_drain_overrides_while_suite_active() {
        use std::sync::mpsc;

        let dir = tempfile::tempdir().expect("tempdir");
        let data_dir = dir.path().to_string_lossy().to_string();
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
            let _suite = test_hooks::SuiteGuard::enter();
            test_hooks::push_override_get(TitleKeyState::Present("sk-owner-only".into()));
            assert!(test_hooks::suite_active());
            assert!(test_hooks::is_suite_owner());

            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                // suite_active is process-true, but this thread is not owner.
                assert!(!test_hooks::is_suite_owner());
                let state = get_title_api_key();
                tx.send(format!("{state:?}")).expect("send");
            })
            .join()
            .expect("foreign thread");

            let foreign = rx.recv().expect("recv");
            assert_eq!(
                foreign, "Absent",
                "foreign thread must hit real empty store, not owner override"
            );

            // Owner still has the queued Present.
            match get_title_api_key() {
                TitleKeyState::Present(s) => assert_eq!(s, "sk-owner-only"),
                other => panic!("owner must still consume override, got {other:?}"),
            }
        });
    }
}
