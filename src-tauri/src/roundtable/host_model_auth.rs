//! Host-held model credentials. The sandbox only ever sees an attempt bearer
//! and the loopback gateway. This module turns the operator's auth file or
//! provider env var into the upstream origin, bearer, and headers the gateway
//! attaches when it forwards. Auth bytes are read and refreshed on the host.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use roundtable_protocol::{ErrorCode, RtResult};
use serde_json::Value;

use super::rt_error;

pub const GROK_CLI_PROXY_ORIGIN: &str = "https://cli-chat-proxy.grok.com";
const GROK_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const GROK_PUBLIC_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REFRESH_LEAD_SECS: i64 = 120;

#[derive(Clone, Debug)]
pub struct RefreshMaterial {
    pub path: PathBuf,
    pub refresh_token: String,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub token_url: String,
}

#[derive(Clone, Debug)]
pub struct ResolvedUpstream {
    pub origin: String,
    pub bearer: String,
    pub headers: Vec<(String, String)>,
    pub refresh: Option<RefreshMaterial>,
}

pub struct ResolveInput<'a> {
    pub agent: &'a str,
    pub binding_origin: &'a str,
    pub env_secret: Option<String>,
    pub auth_files: &'a [PathBuf],
    pub adapter_version: &'a str,
    pub now_unix: i64,
}

pub async fn resolve_model_upstream(input: ResolveInput<'_>) -> RtResult<ResolvedUpstream> {
    let client = super::gateway::ClientPolicy::approved().build_client()?;
    resolve_with_client(input, &client).await
}

pub async fn resolve_with_client(
    input: ResolveInput<'_>,
    client: &reqwest::Client,
) -> RtResult<ResolvedUpstream> {
    let mut session = None;
    let mut session_path = None;
    for path in input.auth_files {
        let Some(bytes) = read_regular_file(path) else {
            continue;
        };
        if let Some(parsed) = parse_session(&bytes) {
            if parsed.access_token.is_some() || parsed.refresh_token.is_some() {
                session = Some(parsed);
                session_path = Some(path.clone());
                break;
            }
        }
    }
    if input.agent == "grok" {
        return resolve_grok(input, client, session, session_path).await;
    }
    if let Some(parsed) = session {
        let path = session_path.expect("session path");
        let token_url = if input.agent == "antigravity" {
            GOOGLE_TOKEN_URL.to_string()
        } else {
            String::new()
        };
        let (bearer, refresh) = materialize(
            input.agent,
            &path,
            parsed,
            input.now_unix,
            client,
            token_url,
        )
        .await?;
        return Ok(ResolvedUpstream {
            origin: input.binding_origin.trim_end_matches('/').to_string(),
            bearer,
            headers: Vec::new(),
            refresh,
        });
    }
    if let Some(secret) = input
        .env_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(ResolvedUpstream {
            origin: input.binding_origin.trim_end_matches('/').to_string(),
            bearer: secret.to_string(),
            headers: Vec::new(),
            refresh: None,
        });
    }
    Err(rt_error(
        ErrorCode::CapabilityUnqualified,
        "provider_credential_missing",
    ))
}

async fn resolve_grok(
    input: ResolveInput<'_>,
    client: &reqwest::Client,
    session: Option<ParsedSession>,
    session_path: Option<PathBuf>,
) -> RtResult<ResolvedUpstream> {
    if let (Some(parsed), Some(path)) = (session, session_path) {
        let (bearer, refresh) = materialize(
            "grok",
            &path,
            parsed,
            input.now_unix,
            client,
            GROK_TOKEN_URL.to_string(),
        )
        .await?;
        return Ok(ResolvedUpstream {
            origin: GROK_CLI_PROXY_ORIGIN.to_string(),
            bearer,
            headers: grok_proxy_headers(input.adapter_version),
            refresh,
        });
    }
    if let Some(secret) = input
        .env_secret
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        // API-key mode talks to the binding origin (api.x.ai) with the key
        // alone. CLI-proxy headers belong to the OIDC session, not this path.
        return Ok(ResolvedUpstream {
            origin: input.binding_origin.trim_end_matches('/').to_string(),
            bearer: secret.to_string(),
            headers: Vec::new(),
            refresh: None,
        });
    }
    Err(rt_error(
        ErrorCode::CapabilityUnqualified,
        "provider_credential_missing",
    ))
}

fn grok_proxy_headers(version: &str) -> Vec<(String, String)> {
    let version = if version.is_empty() {
        "1.0.46"
    } else {
        version
    };
    vec![
        ("X-XAI-Token-Auth".into(), "xai-grok-cli".into()),
        ("x-grok-client-version".into(), version.to_string()),
        ("x-grok-client-identifier".into(), "grok".into()),
        (
            "x-authenticateresponse".into(),
            "authenticate-response".into(),
        ),
        ("User-Agent".into(), format!("grok/{version}")),
    ]
}

#[derive(Default)]
struct ParsedSession {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
    client_id: Option<String>,
    client_secret: Option<String>,
}

fn parse_session(bytes: &[u8]) -> Option<ParsedSession> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut parsed = ParsedSession::default();
    walk(&value, None, 0, &mut parsed);
    if parsed.access_token.is_none() && parsed.refresh_token.is_none() {
        return None;
    }
    Some(parsed)
}

fn walk(value: &Value, parent_key: Option<&str>, depth: u8, parsed: &mut ParsedSession) {
    if depth > 6 {
        return;
    }
    let Some(object) = value.as_object() else {
        return;
    };
    if parsed.access_token.is_none() {
        if let Some(token) = text_field(object, "access_token") {
            parsed.access_token = Some(token);
        }
    }
    if parsed.refresh_token.is_none() {
        if let Some(token) = text_field(object, "refresh_token") {
            parsed.refresh_token = Some(token);
        }
    }
    if parsed.access_token.is_none() {
        if let Some(token) = text_field(object, "token") {
            if token.len() >= 16 {
                parsed.access_token = Some(token);
            }
        }
    }
    if parsed.access_token.is_none() {
        let sign_in = parent_key.is_some_and(|key| {
            key.contains("accounts.x.ai") || key.contains("auth.x.ai") || key.contains("sign-in")
        });
        if sign_in {
            if let Some(token) = text_field(object, "key") {
                if token.len() >= 16 {
                    parsed.access_token = Some(token);
                }
            }
        }
    }
    if parsed.client_id.is_none() {
        parsed.client_id = text_field(object, "client_id");
    }
    if parsed.client_secret.is_none() {
        parsed.client_secret = text_field(object, "client_secret");
    }
    if parsed.expires_at.is_none() {
        parsed.expires_at = expiry_field(object);
    }
    for (key, child) in object {
        if child.is_object() {
            walk(child, Some(key), depth + 1, parsed);
        }
    }
}

fn text_field(object: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn expiry_field(object: &serde_json::Map<String, Value>) -> Option<i64> {
    for key in ["expires_at", "expiry", "expires"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if let Some(number) = value.as_i64() {
            return Some(normalize_unix(number));
        }
        if let Some(number) = value.as_u64() {
            return i64::try_from(number).ok().map(normalize_unix);
        }
        if let Some(text) = value.as_str() {
            if let Ok(number) = text.parse::<i64>() {
                return Some(normalize_unix(number));
            }
            if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(text) {
                return Some(parsed.timestamp());
            }
        }
    }
    None
}

fn normalize_unix(number: i64) -> i64 {
    if number > 10_000_000_000 {
        number / 1000
    } else {
        number
    }
}

async fn materialize(
    agent: &str,
    path: &Path,
    parsed: ParsedSession,
    now_unix: i64,
    client: &reqwest::Client,
    token_url: String,
) -> RtResult<(String, Option<RefreshMaterial>)> {
    let refresh = parsed
        .refresh_token
        .as_ref()
        .filter(|_| !token_url.is_empty())
        .map(|token| RefreshMaterial {
            path: path.to_path_buf(),
            refresh_token: token.clone(),
            client_id: parsed
                .client_id
                .clone()
                .or_else(|| (agent == "grok").then(|| GROK_PUBLIC_CLIENT_ID.to_string())),
            client_secret: parsed.client_secret.clone(),
            token_url: token_url.clone(),
        });
    let expired = parsed
        .expires_at
        .is_some_and(|expires| now_unix + REFRESH_LEAD_SECS >= expires);
    if let Some(access) = parsed.access_token.clone() {
        if !expired {
            return Ok((access, refresh));
        }
    }
    let Some(material) = refresh.clone() else {
        return parsed
            .access_token
            .map(|access| (access, None))
            .ok_or_else(|| {
                rt_error(
                    ErrorCode::CapabilityUnqualified,
                    "provider_credential_missing",
                )
            });
    };
    match material.refresh(client).await {
        Ok(access) => Ok((access, Some(material))),
        Err(error) => {
            // A token that has not actually expired can still be used when
            // refresh fails. The lead window only tries to rotate early.
            let still_valid = parsed.expires_at.is_none_or(|expires| now_unix < expires);
            if let Some(access) = parsed.access_token {
                if still_valid {
                    return Ok((access, Some(material)));
                }
            }
            Err(error)
        }
    }
}

impl RefreshMaterial {
    pub async fn refresh(&self, client: &reqwest::Client) -> RtResult<String> {
        let mut form = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), self.refresh_token.clone()),
        ];
        if let Some(client_id) = &self.client_id {
            form.push(("client_id".to_string(), client_id.clone()));
        }
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret".to_string(), secret.clone()));
        }
        let response = client
            .post(&self.token_url)
            .form(&form)
            .send()
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "credential_refresh"))?;
        if !response.status().is_success() {
            return Err(rt_error(
                ErrorCode::RuntimeUnavailable,
                "credential_refresh",
            ));
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "credential_refresh"))?;
        let value: Value = serde_json::from_slice(&body)
            .map_err(|_| rt_error(ErrorCode::RuntimeUnavailable, "credential_refresh"))?;
        let access = value
            .get("access_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| rt_error(ErrorCode::RuntimeUnavailable, "credential_refresh"))?
            .to_string();
        let next_refresh = value
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .unwrap_or(self.refresh_token.as_str());
        let _ = persist_rotated_token(&self.path, &access, Some(next_refresh));
        Ok(access)
    }
}

fn persist_rotated_token(path: &Path, access: &str, refresh: Option<&str>) -> RtResult<()> {
    let meta = std::fs::symlink_metadata(path)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "credential_refresh"))?;
    if !meta.file_type().is_file() {
        return Err(rt_error(
            ErrorCode::StorageUnavailable,
            "credential_refresh",
        ));
    }
    let original = std::fs::read(path)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "credential_refresh"))?;
    let mut value: Value = serde_json::from_slice(&original)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "credential_refresh"))?;
    replace_secrets(&mut value, access, refresh);
    let rendered = serde_json::to_vec_pretty(&value)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "credential_refresh"))?;
    let tmp = path.with_extension("refresh.tmp");
    std::fs::write(&tmp, rendered)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "credential_refresh"))?;
    std::fs::rename(&tmp, path)
        .map_err(|_| rt_error(ErrorCode::StorageUnavailable, "credential_refresh"))?;
    Ok(())
}

fn replace_secrets(value: &mut Value, access: &str, refresh: Option<&str>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "access_token" || key == "token" {
                    if child.is_string() {
                        *child = Value::String(access.to_string());
                    }
                } else if key == "refresh_token" {
                    if let Some(refresh) = refresh {
                        if child.is_string() {
                            *child = Value::String(refresh.to_string());
                        }
                    }
                } else {
                    replace_secrets(child, access, refresh);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                replace_secrets(item, access, refresh);
            }
        }
        _ => {}
    }
}

fn read_regular_file(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    std::fs::read(path).ok()
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        agent: &'a str,
        origin: &'a str,
        env_secret: Option<&str>,
        files: &'a [PathBuf],
    ) -> ResolveInput<'a> {
        ResolveInput {
            agent,
            binding_origin: origin,
            env_secret: env_secret.map(str::to_string),
            auth_files: files,
            adapter_version: "1.0.46",
            now_unix: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn grok_oidc_file_uses_cli_proxy_not_the_api_key_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            br#"{"access_token":"oidc-access","refresh_token":"oidc-refresh","expires_at":1900000000}"#,
        )
        .unwrap();
        let files = [path];
        let client = reqwest::Client::new();
        let resolved = resolve_with_client(
            input(
                "grok",
                "https://api.x.ai",
                Some("xai-should-not-win"),
                &files,
            ),
            &client,
        )
        .await
        .unwrap();
        assert_eq!(resolved.origin, GROK_CLI_PROXY_ORIGIN);
        assert_eq!(resolved.bearer, "oidc-access");
        assert!(resolved
            .headers
            .iter()
            .any(|(key, value)| key == "X-XAI-Token-Auth" && value == "xai-grok-cli"));
        assert!(!resolved
            .headers
            .iter()
            .any(|(_, value)| value.contains("xai-should-not-win")));
    }

    #[tokio::test]
    async fn grok_api_key_without_a_session_stays_on_the_binding_origin() {
        let client = reqwest::Client::new();
        let resolved = resolve_with_client(
            input("grok", "https://api.x.ai", Some("xai-live-key"), &[]),
            &client,
        )
        .await
        .unwrap();
        assert_eq!(resolved.origin, "https://api.x.ai");
        assert_eq!(resolved.bearer, "xai-live-key");
        assert!(resolved.headers.is_empty());
    }

    #[tokio::test]
    async fn antigravity_token_file_keeps_the_binding_origin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acp_token.json");
        std::fs::write(
            &path,
            br#"{"token":"ya29.host-access-token","refresh_token":"rotating","expiry":"2030-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let files = [path];
        let client = reqwest::Client::new();
        let resolved = resolve_with_client(
            input(
                "antigravity",
                "https://cloudcode-pa.googleapis.com",
                None,
                &files,
            ),
            &client,
        )
        .await
        .unwrap();
        assert_eq!(resolved.origin, "https://cloudcode-pa.googleapis.com");
        assert_eq!(resolved.bearer, "ya29.host-access-token");
        assert!(resolved.headers.is_empty());
    }

    #[test]
    fn nested_grok_sign_in_key_is_an_access_token() {
        let raw = br#"{"https://accounts.x.ai/sign-in":{"key":"session-key-value-123456"}}"#;
        let parsed = parse_session(raw).unwrap();
        assert_eq!(
            parsed.access_token.as_deref(),
            Some("session-key-value-123456")
        );
    }
}
