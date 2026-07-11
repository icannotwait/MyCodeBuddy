//! Minisign signature verification for downloaded release archives.
//!
//! The release pipeline signs each server tarball/zip with the **same**
//! minisign key the desktop updater uses (`tauri signer sign`), producing a
//! detached `<asset>.sig`. We verify that signature against the public key
//! embedded below before extracting or installing anything — executing a
//! downloaded binary without verifying its provenance would be the whole
//! ballgame for an attacker.
//!
//! Wire format note: both the public key (loaded from the compile-time embedded
//! `tauri.conf.json`) and the `.sig` produced by `tauri signer sign` are
//! **base64 of a minisign text file**. We base64-decode that outer wrapper,
//! then hand the inner minisign text to `minisign-verify`.

use base64::Engine;
use minisign_verify::{PublicKey, Signature};

const TAURI_CONFIG_JSON: &str = include_str!("../../tauri.conf.json");

/// Read the Tauri-format minisign public key from the embedded updater config.
fn configured_public_key_b64() -> Result<String, String> {
    let config: serde_json::Value = serde_json::from_str(TAURI_CONFIG_JSON)
        .map_err(|e| format!("failed to parse embedded tauri.conf.json: {e}"))?;
    config
        .pointer("/plugins/updater/pubkey")
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "embedded tauri.conf.json has no updater public key".to_string())
}

/// Decode an outer base64 wrapper into the inner minisign text file.
fn unwrap_base64(b64: &str) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("base64 decode failed: {e}"))?;
    String::from_utf8(bytes).map_err(|e| format!("invalid utf-8 in minisign text: {e}"))
}

/// Build a [`PublicKey`] from a two-line minisign `.pub` text (or just the
/// bare `RW…` key line).
fn parse_public_key(minisign_pub_text: &str) -> Result<PublicKey, String> {
    let key_line = minisign_pub_text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .ok_or_else(|| "minisign public key text has no key line".to_string())?;
    PublicKey::from_base64(key_line)
        .map_err(|e| format!("failed to parse minisign public key: {e}"))
}

/// The embedded release-signing public key.
fn embedded_public_key() -> Result<PublicKey, String> {
    parse_public_key(&unwrap_base64(&configured_public_key_b64()?)?)
}

/// Verify `data` against a minisign signature text (the `.minisig` file
/// contents) using an explicit public key. Testable core — production code
/// goes through [`verify_release_signature`].
pub fn verify_minisign(
    public_key: &PublicKey,
    data: &[u8],
    minisig_text: &str,
) -> Result<(), String> {
    let signature =
        Signature::decode(minisig_text).map_err(|e| format!("failed to parse signature: {e}"))?;
    // `allow_legacy = true` to match tauri-plugin-updater's own
    // `verify_signature` (it passes `true`). `tauri signer sign` can emit
    // legacy-format signatures; with `false` we would reject valid release
    // signatures. `true` is a strict superset — a valid signature from our
    // key is still required.
    public_key
        .verify(data, &signature, true)
        .map_err(|e| format!("signature verification failed: {e}"))
}

/// Verify `data` against a Tauri-format `.sig` (base64 of a `.minisig`)
/// using the embedded release-signing public key. Returns `Ok(())` only if
/// the signature is valid.
pub fn verify_release_signature(data: &[u8], tauri_sig_b64: &str) -> Result<(), String> {
    let public_key = embedded_public_key()?;
    let minisig_text = unwrap_base64(tauri_sig_b64)?;
    verify_minisign(&public_key, data, &minisig_text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_pubkey_comes_from_tauri_config() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        let expected = config["plugins"]["updater"]["pubkey"].as_str().unwrap();

        assert_eq!(configured_public_key_b64().unwrap(), expected);
    }

    #[test]
    fn embedded_pubkey_parses() {
        // The key baked from tauri.conf.json must always decode to a usable
        // key, or release verification would be disabled.
        embedded_public_key().expect("embedded pubkey should parse");
    }

    #[test]
    fn bare_key_line_parses() {
        let text = unwrap_base64(&configured_public_key_b64().unwrap()).unwrap();
        let key_line = text.lines().find(|line| line.starts_with("RW")).unwrap();
        parse_public_key(key_line).expect("bare key line should parse");
    }

    #[test]
    fn garbage_signature_is_rejected() {
        let pk = embedded_public_key().unwrap();
        let err = verify_minisign(&pk, b"hello", "not a signature").unwrap_err();
        assert!(err.contains("parse") || err.contains("signature"));
    }

    #[test]
    fn non_base64_sig_wrapper_is_rejected() {
        assert!(verify_release_signature(b"data", "%%% not base64 %%%").is_err());
    }
}
