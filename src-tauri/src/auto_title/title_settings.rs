//! Title API settings helpers: URL validation, On predicate, and wire types.
//!
//! Metadata key names for automatic-title HTTP config and document-translate
//! agent live here so loaders and setters share one source of truth.

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::app_error::{AppCommandError, AppErrorCode};

// ── Metadata keys ───────────────────────────────────────────────────────────

pub const KEY_AUTO_TITLE_API_URL: &str = "conversation_experience.auto_title_api_url";
pub const KEY_AUTO_TITLE_MODEL: &str = "conversation_experience.auto_title_model";
pub const KEY_AUTO_TITLE_CONFIG_BARRIER: &str =
    "conversation_experience.auto_title_config_barrier";
pub const KEY_AUTO_TITLE_CONFIG_GEN: &str = "conversation_experience.auto_title_config_gen";
pub const KEY_AUTO_TITLE_API_KEY_FP: &str = "conversation_experience.auto_title_api_key_fp";
pub const KEY_DOCUMENT_TRANSLATE_AGENT: &str =
    "conversation_experience.document_translate_agent";
pub const KEY_AUTO_TITLE_JOBS_PURGED_FOR_API_V1: &str =
    "conversation_experience.auto_title_jobs_purged_for_api_v1";

/// Barrier metadata value when raised (`"1"`).
pub const BARRIER_RAISED: &str = "1";

// ── On predicate ────────────────────────────────────────────────────────────

/// Title enroll/claim/UI enabled when barrier is clear and all three fields are set.
pub fn auto_title_enabled(
    url: &str,
    key_present: bool,
    model: &str,
    config_barrier: bool,
) -> bool {
    !config_barrier
        && !url.trim().is_empty()
        && key_present
        && !model.trim().is_empty()
}

// ── URL validation ──────────────────────────────────────────────────────────

/// Trim, parse, require http/https, reject userinfo, strip query/fragment.
///
/// Stores origin + path only (trailing slash preserved as returned by the
/// url crate's serialization of path — we strip a trailing `/` only when the
/// path is not `/`? Spec: store origin+path; strip query/fragment).
pub fn normalize_and_validate_api_url(raw: &str) -> Result<String, AppCommandError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }

    let parsed = reqwest::Url::parse(trimmed).map_err(|error| {
        AppCommandError::new(
            AppErrorCode::ConfigurationInvalid,
            "Automatic title API URL is invalid",
        )
        .with_detail(error.to_string())
    })?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(AppCommandError::new(
                AppErrorCode::ConfigurationInvalid,
                "Automatic title API URL must use http or https",
            )
            .with_detail(format!("scheme={other}")));
        }
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppCommandError::new(
            AppErrorCode::ConfigurationInvalid,
            "Automatic title API URL must not include userinfo",
        ));
    }

    // Rebuild origin + path only (no query, no fragment, no userinfo).
    let mut out = reqwest::Url::parse(&format!(
        "{}://{}",
        parsed.scheme(),
        parsed
            .host_str()
            .ok_or_else(|| AppCommandError::new(
                AppErrorCode::ConfigurationInvalid,
                "Automatic title API URL is missing a host",
            ))?
    ))
    .map_err(|error| {
        AppCommandError::new(
            AppErrorCode::ConfigurationInvalid,
            "Automatic title API URL is invalid",
        )
        .with_detail(error.to_string())
    })?;

    if let Some(port) = parsed.port() {
        let _ = out.set_port(Some(port));
    }
    out.set_path(parsed.path());
    // Explicitly clear query/fragment even if parse reintroduced them.
    out.set_query(None);
    out.set_fragment(None);

    Ok(out.to_string())
}

// ── ApiKeyUpdate wire type ──────────────────────────────────────────────────

/// Discriminated keyring action for `set_auto_title_api_config`.
///
/// Custom deserializer only (not a plain untagged enum). Exactly one of
/// `keep` | `set` | `clear`. See design r8 / Task 2 brief.
#[derive(Clone, PartialEq, Eq, Default)]
pub enum ApiKeyUpdate {
    #[default]
    Keep,
    Set(String),
    Clear,
}

impl std::fmt::Debug for ApiKeyUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiKeyUpdate::Keep => f.write_str("Keep"),
            ApiKeyUpdate::Set(_) => f.write_str("Set(***)"),
            ApiKeyUpdate::Clear => f.write_str("Clear"),
        }
    }
}

impl<'de> Deserialize<'de> for ApiKeyUpdate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ApiKeyUpdateVisitor;

        impl<'de> Visitor<'de> for ApiKeyUpdateVisitor {
            type Value = ApiKeyUpdate;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "an object with exactly one of keep:true, set:<nonempty string>, or clear:true",
                )
            }

            fn visit_map<M>(self, mut map: M) -> Result<ApiKeyUpdate, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut keep: Option<bool> = None;
                let mut set: Option<String> = None;
                let mut clear: Option<bool> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "keep" => {
                            if keep.is_some() || set.is_some() || clear.is_some() {
                                return Err(de::Error::custom(
                                    "api_key_update must contain exactly one of keep|set|clear",
                                ));
                            }
                            let v: serde_json::Value = map.next_value()?;
                            match v {
                                serde_json::Value::Bool(true) => keep = Some(true),
                                serde_json::Value::Bool(false) => {
                                    return Err(de::Error::custom(
                                        "api_key_update.keep must be true",
                                    ));
                                }
                                _ => {
                                    return Err(de::Error::custom(
                                        "api_key_update.keep must be a boolean true",
                                    ));
                                }
                            }
                        }
                        "set" => {
                            if keep.is_some() || set.is_some() || clear.is_some() {
                                return Err(de::Error::custom(
                                    "api_key_update must contain exactly one of keep|set|clear",
                                ));
                            }
                            let v: serde_json::Value = map.next_value()?;
                            match v {
                                serde_json::Value::String(s) if !s.is_empty() => set = Some(s),
                                serde_json::Value::String(_) => {
                                    return Err(de::Error::custom(
                                        "api_key_update.set must be a non-empty string",
                                    ));
                                }
                                _ => {
                                    return Err(de::Error::custom(
                                        "api_key_update.set must be a non-empty string",
                                    ));
                                }
                            }
                        }
                        "clear" => {
                            if keep.is_some() || set.is_some() || clear.is_some() {
                                return Err(de::Error::custom(
                                    "api_key_update must contain exactly one of keep|set|clear",
                                ));
                            }
                            let v: serde_json::Value = map.next_value()?;
                            match v {
                                serde_json::Value::Bool(true) => clear = Some(true),
                                serde_json::Value::Bool(false) => {
                                    return Err(de::Error::custom(
                                        "api_key_update.clear must be true",
                                    ));
                                }
                                _ => {
                                    return Err(de::Error::custom(
                                        "api_key_update.clear must be a boolean true",
                                    ));
                                }
                            }
                        }
                        other => {
                            return Err(de::Error::custom(format!(
                                "unknown api_key_update key: {other}"
                            )));
                        }
                    }
                }

                match (keep, set, clear) {
                    (Some(true), None, None) => Ok(ApiKeyUpdate::Keep),
                    (None, Some(s), None) => Ok(ApiKeyUpdate::Set(s)),
                    (None, None, Some(true)) => Ok(ApiKeyUpdate::Clear),
                    (None, None, None) => Err(de::Error::custom(
                        "api_key_update must contain exactly one of keep|set|clear",
                    )),
                    _ => Err(de::Error::custom(
                        "api_key_update must contain exactly one of keep|set|clear",
                    )),
                }
            }
        }

        deserializer.deserialize_map(ApiKeyUpdateVisitor)
    }
}

fn default_api_key_update() -> ApiKeyUpdate {
    ApiKeyUpdate::Keep
}

/// Request body for `set_auto_title_api_config`.
///
/// `api_key_update` omitted → Keep. Present as JSON `null` → error.
#[derive(Debug, Clone, Deserialize)]
pub struct SetAutoTitleApiConfigRequest {
    pub api_url: String,
    #[serde(
        default = "default_api_key_update",
        deserialize_with = "deserialize_api_key_update_field"
    )]
    pub api_key_update: ApiKeyUpdate,
    pub model: String,
}

fn deserialize_api_key_update_field<'de, D>(deserializer: D) -> Result<ApiKeyUpdate, D::Error>
where
    D: Deserializer<'de>,
{
    // When the field is present, we always land here (including null).
    // Missing field uses the default = Keep and never calls this function.
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Err(de::Error::custom(
            "api_key_update must not be null; omit the field to Keep",
        ));
    }
    ApiKeyUpdate::deserialize(value).map_err(de::Error::custom)
}

/// Request body for `set_document_translate_agent`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SetDocumentTranslateAgentRequest {
    pub agent: Option<crate::models::agent::AgentType>,
}

/// Parse barrier metadata (`"1"` ⇒ true; anything else / absent ⇒ false).
pub fn parse_config_barrier(raw: Option<&str>) -> bool {
    raw == Some(BARRIER_RAISED)
}

/// Parse monotonic config generation (decimal u64 string).
pub fn parse_config_gen(raw: Option<&str>) -> u64 {
    let Some(raw) = raw.filter(|v| !v.is_empty()) else {
        return 0;
    };
    if raw.chars().all(|c| c.is_ascii_digit()) {
        raw.parse::<u64>().unwrap_or(0)
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auto_title_enabled_requires_all_three_and_no_barrier() {
        assert!(auto_title_enabled("https://api.example/v1", true, "m", false));
        assert!(!auto_title_enabled("", true, "m", false));
        assert!(!auto_title_enabled("https://api.example/v1", false, "m", false));
        assert!(!auto_title_enabled("https://api.example/v1", true, "", false));
        assert!(!auto_title_enabled(
            "https://api.example/v1",
            true,
            "m",
            true
        ));
        assert!(!auto_title_enabled("  ", true, "m", false));
        assert!(!auto_title_enabled("https://api.example/v1", true, "  ", false));
    }

    #[test]
    fn url_accepts_http_https_strips_query_fragment() {
        assert_eq!(
            normalize_and_validate_api_url("  https://api.openai.com/v1/?tenant=x#frag  ")
                .expect("ok"),
            "https://api.openai.com/v1/"
        );
        assert_eq!(
            normalize_and_validate_api_url("http://127.0.0.1:8080/v1")
                .expect("ok"),
            "http://127.0.0.1:8080/v1"
        );
        assert_eq!(
            normalize_and_validate_api_url("https://gateway.example/openai/v1")
                .expect("ok"),
            "https://gateway.example/openai/v1"
        );
    }

    #[test]
    fn url_empty_ok() {
        assert_eq!(normalize_and_validate_api_url("").expect("ok"), "");
        assert_eq!(normalize_and_validate_api_url("   ").expect("ok"), "");
    }

    #[test]
    fn url_rejects_userinfo_and_bad_scheme() {
        let err = normalize_and_validate_api_url("https://user:pass@api.example/v1")
            .expect_err("userinfo");
        assert!(matches!(err.code, AppErrorCode::ConfigurationInvalid));

        let err = normalize_and_validate_api_url("ftp://api.example/v1").expect_err("scheme");
        assert!(matches!(err.code, AppErrorCode::ConfigurationInvalid));

        let err = normalize_and_validate_api_url("not a url").expect_err("parse");
        assert!(matches!(err.code, AppErrorCode::ConfigurationInvalid));
    }

    #[test]
    fn api_key_update_serde_accepts_keep_set_clear() {
        assert_eq!(
            serde_json::from_value::<ApiKeyUpdate>(json!({"keep": true})).unwrap(),
            ApiKeyUpdate::Keep
        );
        assert_eq!(
            serde_json::from_value::<ApiKeyUpdate>(json!({"set": "sk-secret"})).unwrap(),
            ApiKeyUpdate::Set("sk-secret".into())
        );
        assert_eq!(
            serde_json::from_value::<ApiKeyUpdate>(json!({"clear": true})).unwrap(),
            ApiKeyUpdate::Clear
        );
    }

    #[test]
    fn api_key_update_serde_rejection_matrix() {
        let reject = [
            json!({"keep": false}),
            json!({"clear": false}),
            json!({"set": ""}),
            json!({"set": 1}),
            json!({"keep": true, "clear": true}),
            json!({"unknown": true}),
            json!({}),
            json!({"keep": "yes"}),
        ];
        for case in reject {
            assert!(
                serde_json::from_value::<ApiKeyUpdate>(case.clone()).is_err(),
                "expected reject for {case}"
            );
        }
    }

    #[test]
    fn request_omitted_key_update_is_keep_null_is_error() {
        let req: SetAutoTitleApiConfigRequest = serde_json::from_value(json!({
            "api_url": "https://api.example/v1",
            "model": "m"
        }))
        .expect("omit");
        assert_eq!(req.api_key_update, ApiKeyUpdate::Keep);

        let err = serde_json::from_value::<SetAutoTitleApiConfigRequest>(json!({
            "api_url": "https://api.example/v1",
            "api_key_update": null,
            "model": "m"
        }));
        assert!(err.is_err(), "null must not be Keep");
    }

    #[test]
    fn api_key_update_debug_redacts_set() {
        let u = ApiKeyUpdate::Set("sk-do-not-leak".into());
        let rendered = format!("{u:?}");
        assert_eq!(rendered, "Set(***)");
        assert!(!rendered.contains("sk-do-not-leak"));
    }

    #[test]
    fn parse_barrier_and_gen() {
        assert!(parse_config_barrier(Some("1")));
        assert!(!parse_config_barrier(Some("0")));
        assert!(!parse_config_barrier(None));
        assert_eq!(parse_config_gen(Some("42")), 42);
        assert_eq!(parse_config_gen(None), 0);
        assert_eq!(parse_config_gen(Some("xyz")), 0);
    }
}
