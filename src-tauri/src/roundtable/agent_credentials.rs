//! Roundtable seat credentials from Codeg's agent settings, and the read-only
//! `roundtable_agents` status.
//!
//! Setting values are returned only to the sandbox env builder. Everything
//! that reports on them (status, traces, errors) carries presence flags, never
//! values, and every copied value joins the diagnostic redaction list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use roundtable_protocol::{FieldError, RtError};
use serde_json::{json, Value};

use super::qualification_profiles::{profile_for_agent, AdapterProfile};

/// Read-only status command name (desktop invoke and `/api/roundtable_agents`).
pub(crate) const AGENTS_COMMAND: &str = "roundtable_agents";

/// The fixed roundtable candidates, in display and default order.
pub(crate) const ROUNDTABLE_CANDIDATES: [&str; 5] =
    ["grok", "antigravity", "cursor", "codex", "code_buddy"];

/// One agent's Codeg settings row. `env` holds the saved `env_json`.
#[derive(Default, Clone)]
pub(crate) struct AgentSettingsRow {
    pub found: bool,
    pub enabled: bool,
    pub installed_version: Option<String>,
    pub env: BTreeMap<String, String>,
}

impl std::fmt::Debug for AgentSettingsRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print setting values.
        f.debug_struct("AgentSettingsRow")
            .field("found", &self.found)
            .field("enabled", &self.enabled)
            .field("installed_version", &self.installed_version)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Reads one row from the Codeg database next to the roundtable data dir,
/// read-only. A missing database or row is an empty row, not an error: the
/// seat then fails on its credential check with a precise reason.
pub(crate) fn read_agent_settings(data_dir: &Path, agent: &str) -> AgentSettingsRow {
    let path = data_dir.join(crate::db::database_file_name());
    read_agent_settings_at(&path, agent)
}

pub(crate) fn read_agent_settings_at(db_path: &Path, agent: &str) -> AgentSettingsRow {
    use rusqlite::{Connection, OpenFlags, OptionalExtension};
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return AgentSettingsRow::default();
    };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
    // `agent_type` is stored as the serde form of AgentType: a JSON string.
    let Ok(stored) = serde_json::to_string(agent) else {
        return AgentSettingsRow::default();
    };
    let row = conn
        .query_row(
            "SELECT enabled, installed_version, env_json FROM agent_setting WHERE agent_type=?1",
            [stored],
            |row| {
                Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional();
    match row {
        Ok(Some((enabled, installed_version, env_json))) => AgentSettingsRow {
            found: true,
            enabled,
            installed_version: installed_version.filter(|value| !value.trim().is_empty()),
            env: env_json
                .and_then(|raw| serde_json::from_str::<BTreeMap<String, String>>(&raw).ok())
                .unwrap_or_default(),
        },
        _ => AgentSettingsRow::default(),
    }
}

/// The container env an adapter's settings credential contributes.
pub(crate) fn settings_credential_env(
    profile: &AdapterProfile,
    settings: &AgentSettingsRow,
) -> Option<Vec<(String, String)>> {
    profile.settings_credential.container_env(&settings.env)
}

/// Where a seat's credential comes from. Presence only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CredentialSource {
    /// A key saved in Codeg's agent settings.
    Settings,
    /// Host auth files the profile names.
    AuthFiles,
    Missing,
}

impl CredentialSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::AuthFiles => "auth_file",
            Self::Missing => "missing",
        }
    }
}

/// Host auth files ready for this profile: every required file exists and,
/// when the profile names one, at least one file with that filename does.
pub(crate) fn auth_files_ready(profile: &AdapterProfile, home: &Path) -> bool {
    if profile.auth_files.is_empty() {
        return false;
    }
    let present: Vec<&str> = profile
        .auth_files
        .iter()
        .filter(|file| home.join(file.home_relative).is_file())
        .map(|file| file.destination)
        .collect();
    let required = profile
        .auth_files
        .iter()
        .filter(|file| file.required)
        .all(|file| present.contains(&file.destination));
    let one = profile.require_one_filename.is_none_or(|name| {
        present
            .iter()
            .any(|destination| destination.rsplit('/').next() == Some(name))
    });
    required && one && !present.is_empty()
}

pub(crate) fn credential_source(
    profile: &AdapterProfile,
    settings: &AgentSettingsRow,
    home: &Path,
) -> CredentialSource {
    if settings_credential_env(profile, settings).is_some() {
        CredentialSource::Settings
    } else if auth_files_ready(profile, home) {
        CredentialSource::AuthFiles
    } else {
        CredentialSource::Missing
    }
}

/// Settings-credential keys this profile reads, for messages. Names only.
pub(crate) fn credential_key_names(profile: &AdapterProfile) -> Vec<&'static str> {
    profile.settings_credential.required.to_vec()
}

/// Attaches the seat to a capability error so the readiness message can name
/// the agent. The reason itself is unchanged.
pub(crate) fn name_seat(mut error: RtError, ordinal: u32, agent: &str) -> RtError {
    let path = format!("participants[{ordinal}].agent");
    if !error
        .details
        .field_errors
        .iter()
        .any(|field| field.path == path)
    {
        error.details.field_errors.push(FieldError {
            path,
            reason: agent.to_string(),
        });
    }
    error
}

/// Rewrites a finished OCI bundle so credential values copied into the
/// container env do not stay on disk. Only `process.env` values for the
/// named keys change; ownership annotations and cgroup paths stay intact.
pub(crate) fn scrub_bundle_env(config: &Path, keys: &[String]) {
    if keys.is_empty() {
        return;
    }
    let Ok(bytes) = std::fs::read(config) else {
        return;
    };
    let Ok(mut spec) = serde_json::from_slice::<Value>(&bytes) else {
        return;
    };
    let Some(env) = spec
        .get_mut("process")
        .and_then(|process| process.get_mut("env"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let mut changed = false;
    for entry in env.iter_mut() {
        let Some(text) = entry.as_str() else { continue };
        let Some((key, _)) = text.split_once('=') else {
            continue;
        };
        if keys.iter().any(|candidate| candidate == key) {
            *entry = Value::String(format!("{key}=[redacted]"));
            changed = true;
        }
    }
    if changed {
        if let Ok(encoded) = serde_json::to_vec(&spec) {
            let _ = crate::roundtable::feature_gate::atomic_write(config, &encoded);
        }
    }
}

/// Scrubs a bundle when dropped, so every exit path (success, error,
/// cancellation) runs it.
pub(crate) struct BundleScrub {
    pub config: PathBuf,
    pub keys: Vec<String>,
}

impl Drop for BundleScrub {
    fn drop(&mut self) {
        scrub_bundle_env(&self.config, &self.keys);
    }
}

fn label(agent: &str) -> &'static str {
    match agent {
        "grok" => "Grok",
        "antigravity" => "Antigravity",
        "cursor" => "Cursor",
        "codex" => "Codex",
        "code_buddy" => "CodeBuddy",
        _ => "Agent",
    }
}

/// Short failure reason from a qualification report: failed check names only.
fn report_summary(
    data_dir: &Path,
    profile: &AdapterProfile,
) -> (Option<String>, Vec<String>, Option<String>) {
    let path = data_dir
        .join("roundtable/qualification")
        .join(profile.exact_id)
        .join("report.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return (None, Vec::new(), None);
    };
    let Ok(report) = serde_json::from_slice::<Value>(&bytes) else {
        return (None, Vec::new(), None);
    };
    let verdict = report["verdict"].as_str().map(str::to_string);
    let missing = report["missing"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .take(16)
                .collect()
        })
        .unwrap_or_default();
    let observed = report["observed_at"].as_str().map(str::to_string);
    (verdict, missing, observed)
}

fn policy_keys(data_dir: &Path) -> Option<Vec<Value>> {
    let bytes = std::fs::read(data_dir.join("roundtable/execution-policy.json")).ok()?;
    let policy: Value = serde_json::from_slice(&bytes).ok()?;
    policy["allowed_qualification_keys"].as_array().cloned()
}

/// Read-only status for the five candidates. Presence flags only.
/// Home-relative path of the Codex routing file the sandbox mounts at
/// `/rt-home/.codex/config.toml`. Codeg owns it; the user's own
/// `~/.codex/config.toml` never enters the container.
pub(crate) const CODEX_ROUTING_RELATIVE: &str = ".codeg-roundtable/codex-config.toml";

/// Codex keys that only choose where requests go. Everything else in the
/// user's config (approval/sandbox policy, MCP servers, headers, profiles,
/// projects) stays on the host.
const CODEX_PROVIDER_KEYS: [&str; 4] = ["name", "base_url", "wire_api", "requires_openai_auth"];

/// The routing part of a Codex `config.toml`: `model_provider`, `model`, and
/// that one provider's name/base_url/wire_api/requires_openai_auth. Codeg's
/// API-key mode stores the key in `auth.json` and points `model_provider` at
/// its own gateway entry; without this the sandboxed Codex sends that key
/// to the default OpenAI endpoint and the turn fails `Authentication
/// required`. Returns an empty document when no custom provider is set or
/// the base URL is not https. Never copies http_headers/env_http_headers or
/// query params (they can carry secrets).
pub(crate) fn codex_routing_config(user_config: &str) -> String {
    let Ok(parsed) = user_config.parse::<toml::Table>() else {
        return String::new();
    };
    let mut out = toml::Table::new();
    if let Some(model) = parsed.get("model").and_then(toml::Value::as_str) {
        out.insert("model".into(), toml::Value::String(model.into()));
    }
    let provider = parsed.get("model_provider").and_then(toml::Value::as_str);
    let table = provider.and_then(|name| {
        parsed
            .get("model_providers")
            .and_then(toml::Value::as_table)
            .and_then(|providers| providers.get(name))
            .and_then(toml::Value::as_table)
    });
    if let (Some(name), Some(table)) = (provider, table) {
        let https = table
            .get("base_url")
            .and_then(toml::Value::as_str)
            .is_some_and(|url| url.starts_with("https://"));
        if https {
            let mut kept = toml::Table::new();
            for key in CODEX_PROVIDER_KEYS {
                if let Some(value) = table.get(key) {
                    if value.is_str() || value.is_bool() {
                        kept.insert(key.into(), value.clone());
                    }
                }
            }
            let mut providers = toml::Table::new();
            providers.insert(name.into(), toml::Value::Table(kept));
            out.insert("model_provider".into(), toml::Value::String(name.into()));
            out.insert("model_providers".into(), toml::Value::Table(providers));
        }
    }
    toml::to_string(&out).unwrap_or_default()
}

/// The https origin Codex will call, from the same routing data, so a
/// provider binding can be checked against it.
pub(crate) fn codex_routing_origin(user_config: &str) -> Option<String> {
    let routed = codex_routing_config(user_config)
        .parse::<toml::Table>()
        .ok()?;
    let name = routed.get("model_provider")?.as_str()?;
    let url = routed
        .get("model_providers")?
        .get(name)?
        .get("base_url")?
        .as_str()?;
    let rest = url.strip_prefix("https://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    (!host.is_empty()).then(|| format!("https://{host}"))
}

/// Rewrites `<home>/CODEX_ROUTING_RELATIVE` (0600, dir 0700) from
/// `<home>/.codex/config.toml`. Always leaves a file (possibly empty) so the
/// declared mount source exists. Reads routing keys only.
pub(crate) fn refresh_codex_routing(home: &Path) -> std::io::Result<PathBuf> {
    let user = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap_or_default();
    let target = home.join(CODEX_ROUTING_RELATIVE);
    let dir = target.parent().unwrap_or(home);
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(".codex-config.toml.tmp");
    std::fs::write(&tmp, codex_routing_config(&user))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

/// Codeg app_metadata key holding the saved agent profiles
/// (`commands::delegation::KEY_DELEGATION_PROFILES_V1`).
const PROFILES_KEY: &str = "delegation.profiles.v1";

/// One saved CodeBuddy profile: ACP session options only (mode + config
/// option values such as `model`). Credentials stay agent-level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CodeBuddyProfile {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub mode_id: Option<String>,
    pub config_values: BTreeMap<String, String>,
}

/// Reads the saved CodeBuddy profiles straight from the settings DB,
/// read-only. Missing table/key/unparsable document reads as no profiles.
pub(crate) fn read_codebuddy_profiles(data_dir: &Path) -> Vec<CodeBuddyProfile> {
    read_codebuddy_profiles_at(&data_dir.join(crate::db::database_file_name()))
}

pub(crate) fn read_codebuddy_profiles_at(db_path: &Path) -> Vec<CodeBuddyProfile> {
    use rusqlite::{Connection, OpenFlags, OptionalExtension};
    let Ok(conn) = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
    let document: Option<String> = conn
        .query_row(
            "SELECT value FROM app_metadata WHERE key=?1 AND deleted_at IS NULL",
            [PROFILES_KEY],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(document) = document else {
        return Vec::new();
    };
    parse_codebuddy_profiles(&document)
}

fn parse_codebuddy_profiles(document: &str) -> Vec<CodeBuddyProfile> {
    let Ok(value) = serde_json::from_str::<Value>(document) else {
        return Vec::new();
    };
    value["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|profile| profile["agent_type"] == "code_buddy")
        .filter_map(|profile| {
            let id = profile["id"].as_str()?.to_string();
            if !roundtable_protocol::is_profile_id(&id) {
                return None;
            }
            let config_values = profile["config_values"]
                .as_object()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|(key, value)| {
                            value.as_str().map(|text| (key.clone(), text.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(CodeBuddyProfile {
                id,
                name: profile["name"].as_str().unwrap_or_default().to_string(),
                enabled: profile["enabled"].as_bool().unwrap_or(false),
                mode_id: profile["mode_id"].as_str().map(str::to_string),
                config_values,
            })
        })
        .collect()
}

/// The seat's chosen profile, or the reason it cannot be used.
pub(crate) fn codebuddy_profile(
    data_dir: &Path,
    id: &str,
) -> Result<CodeBuddyProfile, &'static str> {
    let profile = read_codebuddy_profiles(data_dir)
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or("codebuddy_profile_missing")?;
    if !profile.enabled {
        return Err("codebuddy_profile_disabled");
    }
    Ok(profile)
}

/// Profile summary for the seat dropdown: names and the model option only.
fn profile_summaries(profiles: &[CodeBuddyProfile]) -> Vec<Value> {
    profiles
        .iter()
        .map(|profile| {
            json!({
                "id": profile.id,
                "name": profile.name,
                "enabled": profile.enabled,
                "model": profile.config_values.get("model"),
                "mode_id": profile.mode_id,
            })
        })
        .collect()
}

pub(crate) fn roundtable_agents_status(data_dir: &Path, home: Option<&Path>) -> Value {
    let catalog =
        super::installed_runtime::InstalledRuntime::load_catalog(data_dir).unwrap_or_default();
    let allowed = policy_keys(data_dir);
    let agents: Vec<Value> = ROUNDTABLE_CANDIDATES
        .iter()
        .map(|agent| {
            let settings = read_agent_settings(data_dir, agent);
            let profile = profile_for_agent(agent);
            let installed = catalog.get(*agent);
            let qualified = installed.is_some_and(|runtime| {
                let key = serde_json::to_value(&runtime.qualification_key).ok();
                match (&allowed, key) {
                    (Some(keys), Some(key)) => keys.contains(&key),
                    _ => false,
                }
            });
            let credential = match (profile, home) {
                (Some(profile), Some(home)) => credential_source(profile, &settings, home),
                (Some(profile), None) if settings_credential_env(profile, &settings).is_some() => {
                    CredentialSource::Settings
                }
                _ => CredentialSource::Missing,
            };
            let (verdict, failed_checks, observed_at) = profile
                .map(|profile| report_summary(data_dir, profile))
                .unwrap_or((None, Vec::new(), None));
            let version_ok = match (profile, settings.installed_version.as_deref()) {
                (Some(profile), Some(version)) => {
                    version.contains(profile.version_needle)
                        || profile.version_needle.contains(version)
                }
                _ => false,
            };
            let status = if !settings.enabled {
                "disabled"
            } else if settings.installed_version.is_none() {
                "not_installed"
            } else if profile.is_none() {
                "unsupported"
            } else if credential == CredentialSource::Missing {
                "credential_missing"
            } else if !qualified {
                "unqualified"
            } else {
                "ready"
            };
            json!({
                "agent": agent,
                "label": label(agent),
                "status": status,
                "enabled": settings.enabled,
                "installed": settings.installed_version.is_some(),
                "installed_version": settings.installed_version,
                "profile_version": profile.map(|profile| profile.version_needle),
                "version_matches_profile": version_ok,
                "qualified": qualified,
                "credential": credential.as_str(),
                "credential_keys": profile.map(credential_key_names).unwrap_or_default(),
                "last_qualification": {
                    "verdict": verdict,
                    "failed_checks": failed_checks,
                    "observed_at": observed_at,
                },
                "profiles": if *agent == "code_buddy" {
                    profile_summaries(&read_codebuddy_profiles(data_dir))
                } else {
                    Vec::new()
                },
            })
        })
        .collect();
    json!({ "schema_version": 1, "agents": agents })
}

#[cfg(test)]
mod tests {
    #[test]
    fn codebuddy_profiles_parse_only_codebuddy_entries_with_safe_ids() {
        let doc = r#"{"profiles":[
            {"id":"p-1","agent_type":"code_buddy","name":"Fast","mode_id":"default","config_values":{"model":"m-fast","n":1},"enabled":true,"created_at":1,"updated_at":1},
            {"id":"p-2","agent_type":"code_buddy","name":"Off","config_values":{},"enabled":false,"created_at":2,"updated_at":2},
            {"id":"../bad","agent_type":"code_buddy","name":"Bad","enabled":true,"created_at":3,"updated_at":3},
            {"id":"c-1","agent_type":"codex","name":"Codex","enabled":true,"created_at":4,"updated_at":4}
        ]}"#;
        let profiles = super::parse_codebuddy_profiles(doc);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].id, "p-1");
        assert_eq!(profiles[0].mode_id.as_deref(), Some("default"));
        assert_eq!(profiles[0].config_values.get("model").map(String::as_str), Some("m-fast"));
        assert!(!profiles[0].config_values.contains_key("n"));
        assert!(!profiles[1].enabled);
        assert!(super::parse_codebuddy_profiles("not json").is_empty());
        assert!(super::parse_codebuddy_profiles("{}").is_empty());
        let summary = super::profile_summaries(&profiles);
        assert_eq!(summary[0]["model"], "m-fast");
        assert!(summary[1]["model"].is_null());
    }

    #[test]
    fn codebuddy_profile_lookup_names_missing_and_disabled() {
        let dir = tempfile::tempdir().expect("dir");
        let db = dir.path().join(crate::db::database_file_name());
        let conn = rusqlite::Connection::open(&db).expect("db");
        conn.execute_batch("CREATE TABLE app_metadata (id INTEGER PRIMARY KEY, key TEXT, value TEXT, created_at TEXT, updated_at TEXT, deleted_at TEXT);").expect("schema");
        assert_eq!(super::codebuddy_profile(dir.path(), "p-1"), Err("codebuddy_profile_missing"));
        conn.execute(
            "INSERT INTO app_metadata(key,value) VALUES('delegation.profiles.v1',?1)",
            [r#"{"profiles":[{"id":"p-1","agent_type":"code_buddy","name":"A","enabled":true,"created_at":1,"updated_at":1},{"id":"p-2","agent_type":"code_buddy","name":"B","enabled":false,"created_at":1,"updated_at":1}]}"#],
        )
        .expect("insert");
        drop(conn);
        assert_eq!(super::codebuddy_profile(dir.path(), "p-1").map(|p| p.name), Ok("A".into()));
        assert_eq!(super::codebuddy_profile(dir.path(), "p-2"), Err("codebuddy_profile_disabled"));
        assert_eq!(super::codebuddy_profile(dir.path(), "p-3"), Err("codebuddy_profile_missing"));
    }

    #[test]
    fn codex_routing_keeps_only_the_selected_provider_route() {
        let user = r#"
model_provider = "codeg"
approval_policy = "never"
sandbox_mode = "danger-full-access"
[model_providers.codeg]
name = "codeg"
base_url = "https://gw.example.test/v1"
wire_api = "responses"
requires_openai_auth = true
http_headers = { "X-Key" = "secret-header" }
env_http_headers = { "X-Other" = "SOME_ENV" }
query_params = { k = "secret-query" }
[model_providers.other]
base_url = "https://other.example.test"
[mcp_servers.x]
command = "/bin/x"
"#;
        let routed = codex_routing_config(user);
        let table: toml::Table = routed.parse().expect("toml");
        assert_eq!(table["model_provider"].as_str(), Some("codeg"));
        let providers = table["model_providers"].as_table().expect("providers");
        assert_eq!(providers.len(), 1);
        let codeg = providers["codeg"].as_table().expect("codeg");
        assert_eq!(
            codeg["base_url"].as_str(),
            Some("https://gw.example.test/v1")
        );
        assert_eq!(codeg["wire_api"].as_str(), Some("responses"));
        assert_eq!(codeg["requires_openai_auth"].as_bool(), Some(true));
        for leaked in [
            "secret-header",
            "secret-query",
            "SOME_ENV",
            "approval_policy",
            "sandbox_mode",
            "mcp_servers",
            "other.example",
        ] {
            assert!(!routed.contains(leaked), "{leaked} leaked");
        }
        assert_eq!(
            codex_routing_origin(user).as_deref(),
            Some("https://gw.example.test")
        );
    }

    #[test]
    fn codex_routing_is_empty_without_an_https_custom_provider() {
        assert_eq!(codex_routing_config(""), "");
        assert_eq!(codex_routing_config("not toml ["), "");
        let plain = "model_provider = \"codeg\"\n[model_providers.codeg]\nbase_url = \"http://gw.example.test\"\n";
        assert!(!codex_routing_config(plain).contains("model_provider"));
        assert_eq!(codex_routing_origin(plain), None);
        let home = tempfile::tempdir().expect("home");
        let path = refresh_codex_routing(home.path()).expect("refresh");
        assert!(path.ends_with(CODEX_ROUTING_RELATIVE));
        assert_eq!(std::fs::read_to_string(path).expect("read"), "");
    }

    use super::*;

    fn write_db(dir: &Path, rows: &[(&str, bool, Option<&str>, Option<&str>)]) -> PathBuf {
        let path = dir.join(crate::db::database_file_name());
        let conn = rusqlite::Connection::open(&path).expect("db");
        conn.execute_batch(
            "CREATE TABLE agent_setting (id INTEGER PRIMARY KEY, agent_type TEXT, enabled BOOLEAN, installed_version TEXT, env_json TEXT);",
        )
        .expect("schema");
        for (agent, enabled, version, env) in rows {
            conn.execute(
                "INSERT INTO agent_setting (agent_type, enabled, installed_version, env_json) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![serde_json::to_string(agent).unwrap(), enabled, version, env],
            )
            .expect("row");
        }
        path
    }

    #[test]
    fn settings_row_reads_json_quoted_agent_types_and_hides_values_in_debug() {
        let dir = tempfile::tempdir().expect("tmp");
        write_db(
            dir.path(),
            &[(
                "code_buddy",
                true,
                Some("2.161.0"),
                Some(r#"{"CODEBUDDY_API_KEY":"secret-value-123"}"#),
            )],
        );
        let row = read_agent_settings(dir.path(), "code_buddy");
        assert!(row.found && row.enabled);
        assert_eq!(row.installed_version.as_deref(), Some("2.161.0"));
        assert_eq!(
            row.env.get("CODEBUDDY_API_KEY").map(String::as_str),
            Some("secret-value-123")
        );
        let debug = format!("{row:?}");
        assert!(debug.contains("CODEBUDDY_API_KEY"));
        assert!(!debug.contains("secret-value-123"));
        assert!(!read_agent_settings(dir.path(), "cursor").found);
        assert!(!read_agent_settings(&dir.path().join("absent"), "cursor").found);
    }

    #[test]
    fn status_lists_the_five_candidates_in_order_with_presence_only() {
        let dir = tempfile::tempdir().expect("tmp");
        let home = tempfile::tempdir().expect("home");
        write_db(
            dir.path(),
            &[
                ("grok", true, Some("1.0.46"), None),
                (
                    "cursor",
                    true,
                    Some("2026.09.28-64d2043"),
                    Some(r#"{"CURSOR_AUTH_MODE":"custom","CURSOR_API_KEY":"cursor-secret-abc"}"#),
                ),
                ("codex", false, Some("2.1.1"), None),
                ("code_buddy", true, Some("2.161.0"), Some("{}")),
            ],
        );
        let status = roundtable_agents_status(dir.path(), Some(home.path()));
        let text = status.to_string();
        assert!(!text.contains("cursor-secret-abc"));
        let agents = status["agents"].as_array().expect("agents");
        let order: Vec<&str> = agents
            .iter()
            .map(|agent| agent["agent"].as_str().unwrap())
            .collect();
        assert_eq!(order, ROUNDTABLE_CANDIDATES);
        let by = |name: &str| agents.iter().find(|agent| agent["agent"] == name).unwrap();
        assert_eq!(by("grok")["status"], "credential_missing");
        assert_eq!(by("antigravity")["status"], "disabled");
        assert_eq!(by("cursor")["credential"], "settings");
        assert_eq!(by("cursor")["status"], "unqualified");
        assert_eq!(by("codex")["status"], "disabled");
        assert_eq!(by("code_buddy")["status"], "credential_missing");
        assert_eq!(
            by("code_buddy")["credential_keys"],
            json!(["CODEBUDDY_API_KEY"])
        );
        assert_eq!(by("code_buddy")["label"], "CodeBuddy");
    }

    #[test]
    fn auth_file_presence_follows_the_profile_rules() {
        let home = tempfile::tempdir().expect("home");
        let cursor = profile_for_agent("cursor").expect("cursor");
        std::fs::create_dir_all(home.path().join(".cursor")).unwrap();
        std::fs::write(home.path().join(".cursor/cli-config.json"), b"{}").unwrap();
        assert!(
            !auth_files_ready(cursor, home.path()),
            "auth.json is required by name"
        );
        std::fs::write(home.path().join(".cursor/auth.json"), b"{}").unwrap();
        assert!(auth_files_ready(cursor, home.path()));
        let codebuddy = profile_for_agent("code_buddy").expect("codebuddy");
        assert!(!auth_files_ready(codebuddy, home.path()));
    }

    #[test]
    fn scrub_replaces_only_named_env_values_and_keeps_annotations() {
        let dir = tempfile::tempdir().expect("tmp");
        let config = dir.path().join("config.json");
        std::fs::write(
            &config,
            serde_json::to_vec(&json!({
                "process": {"env": ["PATH=/usr/bin", "CURSOR_API_KEY=abc", "HOME=/rt-home"]},
                "annotations": {"io.codeg.roundtable.label": "x"}
            }))
            .unwrap(),
        )
        .unwrap();
        {
            let _guard = BundleScrub {
                config: config.clone(),
                keys: vec!["CURSOR_API_KEY".into()],
            };
        }
        let spec: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(
            spec["process"]["env"],
            json!([
                "PATH=/usr/bin",
                "CURSOR_API_KEY=[redacted]",
                "HOME=/rt-home"
            ])
        );
        assert_eq!(spec["annotations"]["io.codeg.roundtable.label"], "x");
    }

    #[test]
    fn named_seat_errors_keep_the_reason_and_add_the_agent_once() {
        let error = super::super::rt_error(
            roundtable_protocol::ErrorCode::CapabilityUnqualified,
            "adapter_unqualified",
        );
        let named = name_seat(name_seat(error, 2, "cursor"), 2, "cursor");
        assert_eq!(named.details.reason.as_deref(), Some("adapter_unqualified"));
        assert_eq!(named.details.field_errors.len(), 1);
        assert_eq!(named.details.field_errors[0].path, "participants[2].agent");
        assert_eq!(named.details.field_errors[0].reason, "cursor");
    }
}
