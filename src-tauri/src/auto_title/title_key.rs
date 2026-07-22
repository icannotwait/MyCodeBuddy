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
    match keyring_store::get_token_state(TITLE_API_KEY_ACCOUNT) {
        CredentialState::Present(s) => TitleKeyState::Present(s),
        CredentialState::Absent => TitleKeyState::Absent,
        CredentialState::Unavailable => TitleKeyState::Unavailable,
    }
}

/// Persist the title API key secret.
pub fn set_title_api_key(secret: &str) -> Result<(), String> {
    keyring_store::set_token(TITLE_API_KEY_ACCOUNT, secret)
}

/// Delete the title API key secret (idempotent if already absent).
pub fn delete_title_api_key() -> Result<(), String> {
    keyring_store::delete_token(TITLE_API_KEY_ACCOUNT)
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
        temp_env::with_var("CODEG_DATA_DIR", Some(data_dir.as_str()), || {
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
}
