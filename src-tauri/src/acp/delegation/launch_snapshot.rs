//! Immutable gen-1 launch snapshot helpers.
//!
//! Design (`Durable Run Model` / `Launch snapshot and secrets`):
//! - Persist only **allowlisted non-secret** mode/config on the run row.
//! - Secrets are re-resolved live at spawn/resume and never stored.
//! - Secret rotation that still launches the same profile is allowed and does
//!   not mutate the durable non-secret snapshot.
//! - Profile deleted or snapshot incomplete → continue path is `unresumable`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::models::AgentType;

/// Schema tag written to `delegation_task_runs.launch_snapshot_version`.
pub const LAUNCH_SNAPSHOT_VERSION: &str = "v1";

/// Resolve the root workspace once before it participates in a durable launch
/// snapshot. The returned string is an existing, absolute canonical directory
/// suitable for both the route fingerprint and the child process cwd.
pub fn resolve_workspace_path(working_dir: Option<&str>) -> Result<String, String> {
    let path = match working_dir {
        Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        Some(_) => return Err("working_dir is empty".into()),
        None => std::env::current_dir()
            .map_err(|error| format!("could not resolve the process working directory: {error}"))?,
    };
    let canonical = std::fs::canonicalize(&path)
        .map_err(|error| format!("could not resolve working_dir: {error}"))?;
    if !canonical.is_dir() {
        return Err("working_dir is not a directory".into());
    }
    Ok(canonical.to_string_lossy().into_owned())
}

/// Allowlisted non-secret ACP config option keys accepted into
/// `config_values_json`. Keys not listed here are treated as secret or
/// non-snapshot material and are stripped before persistence.
const ALLOWLISTED_CONFIG_KEYS: &[&str] = &[
    "model",
    "model_id",
    "modelId",
    "thinking",
    "reasoning",
    // Grok ACP config-option id (see connection::GROK_EFFORT_OPTION_ID).
    // Without this, live `preferred_config_values["reasoning_effort"]` is
    // stripped from the durable snapshot and workflow cards never see effort.
    "reasoning_effort",
    "effort",
    "approval_policy",
    "sandbox_mode",
    "permissionMode",
    "permission_mode",
];

/// Inputs that become the durable gen-1 launch snapshot (non-secret only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSnapshot {
    pub workspace_path: String,
    pub route_fingerprint: String,
    pub launch_snapshot_version: String,
    pub mode_id: Option<String>,
    pub config_values_json: String,
    pub profile_id: Option<String>,
}

/// Live spawn material: allowlisted snapshot + secrets re-resolved for the
/// process environment. Secrets are never written to `LaunchSnapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLaunchConfig {
    pub preferred_mode_id: Option<String>,
    /// Full config map passed to spawn (allowlisted keys + live secrets).
    pub preferred_config_values: BTreeMap<String, String>,
    /// Non-secret snapshot for durable storage.
    pub snapshot: LaunchSnapshot,
}

/// Result of checking whether a stored snapshot can still be launched for
/// continuation (profile present, snapshot complete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotLaunchability {
    Launchable,
    /// Wire / settlement code for continue path: `unresumable`.
    Unresumable {
        reason: &'static str,
    },
}

/// Filter config values down to allowlisted non-secret keys only.
pub fn allowlist_config_values(values: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    values
        .iter()
        .filter(|(k, _)| is_allowlisted_config_key(k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

pub fn is_allowlisted_config_key(key: &str) -> bool {
    ALLOWLISTED_CONFIG_KEYS
        .iter()
        .any(|allowed| key.eq_ignore_ascii_case(allowed))
}

/// Deterministic JSON for durable `config_values_json` (sorted keys via BTreeMap).
pub fn config_values_to_json(values: &BTreeMap<String, String>) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "{}".into())
}

/// Stable non-secret hash of agent type + profile id + workspace + snapshot
/// version (design: `route_fingerprint` on the run row).
pub fn launch_route_fingerprint(
    agent_type: AgentType,
    profile_id: Option<&str>,
    workspace_path: &str,
    launch_snapshot_version: &str,
) -> String {
    #[derive(Serialize)]
    struct Payload<'a> {
        v: u32,
        agent_type: AgentType,
        profile_id: &'a str,
        workspace_path: &'a str,
        launch_snapshot_version: &'a str,
    }
    let payload = Payload {
        v: 1,
        agent_type,
        profile_id: profile_id.unwrap_or(""),
        workspace_path,
        launch_snapshot_version,
    };
    let bytes = serde_json::to_vec(&payload).expect("launch fingerprint payload");
    let digest = Sha256::digest(&bytes);
    hex_lower(&digest)
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Build the durable snapshot + live spawn config from resolved profile /
/// agent defaults. `live_config_values` may include secret keys; only the
/// allowlisted subset is stored on the snapshot.
pub fn build_live_launch_config(
    agent_type: AgentType,
    profile_id: Option<&str>,
    workspace_path: &str,
    mode_id: Option<String>,
    live_config_values: BTreeMap<String, String>,
) -> LiveLaunchConfig {
    let allowlisted = allowlist_config_values(&live_config_values);
    let config_values_json = config_values_to_json(&allowlisted);
    let launch_snapshot_version = LAUNCH_SNAPSHOT_VERSION.to_string();
    let route_fingerprint = launch_route_fingerprint(
        agent_type,
        profile_id,
        workspace_path,
        &launch_snapshot_version,
    );
    LiveLaunchConfig {
        preferred_mode_id: mode_id.clone(),
        preferred_config_values: live_config_values,
        snapshot: LaunchSnapshot {
            workspace_path: workspace_path.to_string(),
            route_fingerprint,
            launch_snapshot_version,
            mode_id,
            config_values_json,
            profile_id: profile_id.map(|s| s.to_string()),
        },
    }
}

/// True when all fields required for continuation are present and non-empty.
pub fn snapshot_is_complete(snapshot: &LaunchSnapshot) -> bool {
    !snapshot.workspace_path.trim().is_empty()
        && !snapshot.route_fingerprint.trim().is_empty()
        && !snapshot.launch_snapshot_version.trim().is_empty()
        && !snapshot.config_values_json.trim().is_empty()
}

/// Evaluate whether a stored gen-1 snapshot can still be used for continue.
///
/// - Incomplete / missing snapshot → unresumable
/// - `profile_id` present but profile no longer exists → unresumable
/// - Secret rotation that still yields a launchable profile is allowed (the
///   non-secret snapshot is compared separately and must remain unchanged)
pub fn evaluate_snapshot_launchability(
    snapshot: Option<&LaunchSnapshot>,
    profile_still_exists: Option<bool>,
) -> SnapshotLaunchability {
    let Some(snapshot) = snapshot else {
        return SnapshotLaunchability::Unresumable {
            reason: "missing_launch_snapshot",
        };
    };
    if !snapshot_is_complete(snapshot) {
        return SnapshotLaunchability::Unresumable {
            reason: "incomplete_launch_snapshot",
        };
    }
    if snapshot.profile_id.is_some() && profile_still_exists == Some(false) {
        return SnapshotLaunchability::Unresumable {
            reason: "profile_deleted",
        };
    }
    SnapshotLaunchability::Launchable
}

/// Re-resolve live secrets for spawn without mutating the stored snapshot.
///
/// `stored_snapshot` is the durable non-secret material; `live_secrets` are
/// current credentials/options reloaded from the profile or agent defaults.
/// Returns the spawn map (allowlisted keys from snapshot + live secret keys).
pub fn re_resolve_spawn_config(
    stored_snapshot: &LaunchSnapshot,
    live_secrets: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> =
        serde_json::from_str(&stored_snapshot.config_values_json).unwrap_or_default();
    for (k, v) in live_secrets {
        if !is_allowlisted_config_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_strips_secret_keys() {
        let mut raw = BTreeMap::new();
        raw.insert("model".into(), "gpt-5".into());
        raw.insert("api_key".into(), "sk-secret-value".into());
        raw.insert("auth_token".into(), "tok".into());
        let filtered = allowlist_config_values(&raw);
        assert_eq!(filtered.get("model").map(String::as_str), Some("gpt-5"));
        assert!(!filtered.contains_key("api_key"));
        assert!(!filtered.contains_key("auth_token"));
    }

    #[test]
    fn snapshot_excludes_secrets_while_live_config_keeps_them() {
        let mut live = BTreeMap::new();
        live.insert("model".into(), "sonnet".into());
        live.insert("api_key".into(), "sk-live-1".into());
        let built = build_live_launch_config(
            AgentType::Codex,
            Some("prof-1"),
            "/tmp/ws",
            Some("default".into()),
            live,
        );
        assert!(built.preferred_config_values.contains_key("api_key"));
        assert!(
            !built.snapshot.config_values_json.contains("sk-live-1"),
            "snapshot must not store secret values"
        );
        assert!(built.snapshot.config_values_json.contains("sonnet"));
        assert_eq!(
            built.snapshot.launch_snapshot_version,
            LAUNCH_SNAPSHOT_VERSION
        );
        assert!(!built.snapshot.route_fingerprint.is_empty());
    }

    #[test]
    fn allowlist_keeps_grok_reasoning_effort() {
        let mut raw = BTreeMap::new();
        raw.insert("model".into(), "grok-4.5".into());
        raw.insert("reasoning_effort".into(), "high".into());
        raw.insert("api_key".into(), "sk-secret".into());
        let filtered = allowlist_config_values(&raw);
        assert_eq!(
            filtered.get("reasoning_effort").map(String::as_str),
            Some("high")
        );
        assert_eq!(filtered.get("model").map(String::as_str), Some("grok-4.5"));
        assert!(!filtered.contains_key("api_key"));

        let built = build_live_launch_config(
            AgentType::Grok,
            None,
            "/tmp/ws",
            None,
            raw,
        );
        assert!(
            built.snapshot.config_values_json.contains("reasoning_effort"),
            "durable snapshot must keep Grok effort key: {}",
            built.snapshot.config_values_json
        );
        assert!(built.snapshot.config_values_json.contains("high"));
        assert!(!built.snapshot.config_values_json.contains("sk-secret"));
    }

    #[test]
    fn secret_rotation_does_not_mutate_snapshot() {
        let mut live_v1 = BTreeMap::new();
        live_v1.insert("model".into(), "sonnet".into());
        live_v1.insert("api_key".into(), "sk-old".into());
        let v1 = build_live_launch_config(
            AgentType::Codex,
            Some("prof-1"),
            "/tmp/ws",
            Some("default".into()),
            live_v1,
        );

        let mut live_v2 = BTreeMap::new();
        live_v2.insert("model".into(), "sonnet".into());
        live_v2.insert("api_key".into(), "sk-rotated".into());
        let v2 = build_live_launch_config(
            AgentType::Codex,
            Some("prof-1"),
            "/tmp/ws",
            Some("default".into()),
            live_v2.clone(),
        );

        assert_eq!(
            v1.snapshot, v2.snapshot,
            "secret rotation must not change non-secret snapshot"
        );

        let re_resolved = re_resolve_spawn_config(&v1.snapshot, &live_v2);
        assert_eq!(
            re_resolved.get("api_key").map(String::as_str),
            Some("sk-rotated")
        );
        assert_eq!(re_resolved.get("model").map(String::as_str), Some("sonnet"));
        // Snapshot JSON remains the original allowlisted content.
        assert!(!v1.snapshot.config_values_json.contains("sk-rotated"));
        assert!(!v1.snapshot.config_values_json.contains("sk-old"));
    }

    #[test]
    fn profile_deleted_is_unresumable() {
        let snap = LaunchSnapshot {
            workspace_path: "/tmp/ws".into(),
            route_fingerprint: "abc".into(),
            launch_snapshot_version: LAUNCH_SNAPSHOT_VERSION.into(),
            mode_id: Some("default".into()),
            config_values_json: "{}".into(),
            profile_id: Some("gone".into()),
        };
        assert_eq!(
            evaluate_snapshot_launchability(Some(&snap), Some(false)),
            SnapshotLaunchability::Unresumable {
                reason: "profile_deleted"
            }
        );
    }

    #[test]
    fn missing_or_incomplete_snapshot_is_unresumable() {
        assert_eq!(
            evaluate_snapshot_launchability(None, None),
            SnapshotLaunchability::Unresumable {
                reason: "missing_launch_snapshot"
            }
        );
        let incomplete = LaunchSnapshot {
            workspace_path: String::new(),
            route_fingerprint: "abc".into(),
            launch_snapshot_version: LAUNCH_SNAPSHOT_VERSION.into(),
            mode_id: None,
            config_values_json: "{}".into(),
            profile_id: None,
        };
        assert_eq!(
            evaluate_snapshot_launchability(Some(&incomplete), None),
            SnapshotLaunchability::Unresumable {
                reason: "incomplete_launch_snapshot"
            }
        );
    }

    #[test]
    fn complete_snapshot_with_existing_profile_is_launchable() {
        let snap = LaunchSnapshot {
            workspace_path: "/tmp/ws".into(),
            route_fingerprint: "abc".into(),
            launch_snapshot_version: LAUNCH_SNAPSHOT_VERSION.into(),
            mode_id: Some("default".into()),
            config_values_json: r#"{"model":"x"}"#.into(),
            profile_id: Some("prof".into()),
        };
        assert_eq!(
            evaluate_snapshot_launchability(Some(&snap), Some(true)),
            SnapshotLaunchability::Launchable
        );
    }

    #[test]
    fn route_fingerprint_stable_for_same_inputs() {
        let a = launch_route_fingerprint(AgentType::Codex, Some("p"), "/ws", "v1");
        let b = launch_route_fingerprint(AgentType::Codex, Some("p"), "/ws", "v1");
        assert_eq!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        let c = launch_route_fingerprint(AgentType::Codex, Some("p2"), "/ws", "v1");
        assert_ne!(a, c);
    }
}
