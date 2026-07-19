//! Wire types and domain errors for on-demand document translation.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::app_error::{AppCommandError, AppErrorCode};
use crate::models::system::AppLocale;

/// Backend-authoritative max input size (Unicode scalars).
pub const MAX_INPUT_SCALARS: usize = 24_000;
/// Max UTF-8 bytes collected from the runner before fail-closed.
pub const MAX_OUTPUT_BYTES: usize = 96_000;
/// Overall runner deadline (seconds).
pub const DEADLINE_SECS: u64 = 120;
/// Process-wide in-flight capacity (v1: no queue).
pub const TRANSLATE_CAPACITY: usize = 1;

/// Stable i18n keys (Task 7 catalogs under `Folder.fileWorkspace`).
pub const I18N_AGENT_NOT_CONFIGURED: &str = "Folder.fileWorkspace.translateAgentNotConfigured";
pub const I18N_AGENT_UNAVAILABLE: &str = "Folder.fileWorkspace.translateAgentUnavailable";
pub const I18N_CONTENT_EMPTY: &str = "Folder.fileWorkspace.translateContentEmpty";
pub const I18N_CONTENT_TOO_LARGE: &str = "Folder.fileWorkspace.translateContentTooLarge";
pub const I18N_UNSUPPORTED_FORMAT: &str = "Folder.fileWorkspace.translateUnsupportedFormat";
pub const I18N_BUSY: &str = "Folder.fileWorkspace.translateBusy";
pub const I18N_TIMEOUT: &str = "Folder.fileWorkspace.translateTimeout";
pub const I18N_PLACEHOLDER: &str = "Folder.fileWorkspace.translatePlaceholderIntegrityFailed";
pub const I18N_FAILED: &str = "Folder.fileWorkspace.translateFailed";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DocumentTranslateFormat {
    Markdown,
    PlainText,
}

impl DocumentTranslateFormat {
    /// Parse the wire format string (`markdown` | `plainText`).
    ///
    /// Unknown values become [`DocumentTranslateError::UnsupportedFormat`] so
    /// callers can stamp the stable i18n key (serde enum reject cannot).
    pub fn parse_wire(raw: &str) -> Result<Self, DocumentTranslateError> {
        match raw {
            "markdown" => Ok(Self::Markdown),
            "plainText" => Ok(Self::PlainText),
            _ => Err(DocumentTranslateError::UnsupportedFormat),
        }
    }
}

/// Request body / IPC payload for `translate_document`.
///
/// `format` is a plain string so unknown values reach the service as
/// [`DocumentTranslateError::UnsupportedFormat`] rather than a generic serde
/// rejection. Tauri commands accept the same fields as **flat** camelCase args
/// (not a nested `params` object) to match `api.ts` / `reference_search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateDocumentParams {
    pub content: String,
    /// Wire format: `"markdown"` | `"plainText"`.
    pub format: String,
    /// Optional wire locale (snake_case). Invalid/missing → system language.
    #[serde(default)]
    pub locale: Option<String>,
    /// Display basename only (never used for FS access).
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateDocumentResult {
    pub translated_content: String,
    pub locale: String,
    pub format: DocumentTranslateFormat,
}

/// Domain outcome of a document translation attempt.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DocumentTranslateError {
    #[error("auto-title agent is not configured")]
    AgentNotConfigured,
    #[error("translate agent unavailable or disabled")]
    Unavailable,
    #[error("document content is empty")]
    ContentEmpty,
    #[error("document content exceeds size limit")]
    ContentTooLarge,
    #[error("unsupported document format")]
    UnsupportedFormat,
    #[error("another document translation is already in progress")]
    Busy,
    #[error("translate run cancelled")]
    Cancelled,
    #[error("translate agent spawn failed: {0}")]
    Spawn(String),
    #[error("translate agent identity wait failed: {0}")]
    Identity(String),
    #[error("internal session registry failed: {0}")]
    Registry(String),
    #[error("interactive permission or question on translate run")]
    Interactive,
    #[error("translate run timed out")]
    Timeout,
    #[error("translate run stopped abnormally: {0}")]
    AbnormalStop(String),
    #[error("translate run produced empty output")]
    EmptyOutput,
    #[error("translate output exceeds size limit")]
    OutputTooLarge,
    #[error("placeholder integrity check failed")]
    PlaceholderIntegrity,
    #[error("translate failed: {0}")]
    Failed(String),
}

impl DocumentTranslateError {
    pub fn into_app_command_error(self) -> AppCommandError {
        use std::collections::BTreeMap;
        match self {
            Self::AgentNotConfigured => AppCommandError::new(
                AppErrorCode::ConfigurationMissing,
                "Automatic title agent is not configured for document translation",
            )
            .with_i18n(I18N_AGENT_NOT_CONFIGURED, BTreeMap::new()),
            Self::Unavailable => AppCommandError::new(
                AppErrorCode::DependencyMissing,
                "Configured translation agent is unavailable or disabled",
            )
            .with_i18n(I18N_AGENT_UNAVAILABLE, BTreeMap::new()),
            Self::ContentEmpty => AppCommandError::new(
                AppErrorCode::InvalidInput,
                "Document content is empty",
            )
            .with_i18n(I18N_CONTENT_EMPTY, BTreeMap::new()),
            Self::ContentTooLarge => {
                let mut params = BTreeMap::new();
                params.insert("limit".into(), MAX_INPUT_SCALARS.to_string());
                AppCommandError::new(
                    AppErrorCode::InvalidInput,
                    format!("Document exceeds {MAX_INPUT_SCALARS} Unicode scalars"),
                )
                .with_i18n(I18N_CONTENT_TOO_LARGE, params)
            }
            Self::UnsupportedFormat => AppCommandError::new(
                AppErrorCode::InvalidInput,
                "Unsupported document format for translation",
            )
            .with_i18n(I18N_UNSUPPORTED_FORMAT, BTreeMap::new()),
            Self::Busy => AppCommandError::new(
                AppErrorCode::TurnInProgress,
                "Another document translation is already in progress",
            )
            .with_i18n(I18N_BUSY, BTreeMap::new()),
            Self::Timeout | Self::Cancelled => AppCommandError::new(
                AppErrorCode::TaskExecutionFailed,
                "Document translation timed out",
            )
            .with_i18n(I18N_TIMEOUT, BTreeMap::new()),
            Self::PlaceholderIntegrity => AppCommandError::new(
                AppErrorCode::TaskExecutionFailed,
                "Translation failed placeholder integrity check",
            )
            .with_i18n(I18N_PLACEHOLDER, BTreeMap::new()),
            Self::Spawn(msg)
            | Self::Identity(msg)
            | Self::Registry(msg)
            | Self::AbnormalStop(msg)
            | Self::Failed(msg) => AppCommandError::new(
                AppErrorCode::TaskExecutionFailed,
                "Document translation failed",
            )
            .with_detail(msg)
            .with_i18n(I18N_FAILED, BTreeMap::new()),
            Self::Interactive | Self::EmptyOutput | Self::OutputTooLarge => AppCommandError::new(
                AppErrorCode::TaskExecutionFailed,
                "Document translation failed",
            )
            .with_i18n(I18N_FAILED, BTreeMap::new()),
        }
    }
}

/// Display language name for the translation prompt (not the serde wire id).
pub fn locale_display_name(locale: AppLocale) -> &'static str {
    match locale {
        AppLocale::En => "English",
        AppLocale::ZhCn => "Simplified Chinese",
        AppLocale::ZhTw => "Traditional Chinese",
        AppLocale::Ja => "Japanese",
        AppLocale::Ko => "Korean",
        AppLocale::Es => "Spanish",
        AppLocale::De => "German",
        AppLocale::Fr => "French",
        AppLocale::Pt => "Portuguese",
        AppLocale::Ar => "Arabic",
    }
}

/// Build the exact translation prompt (unit-tested for required fragments).
pub fn build_translate_prompt(locale: AppLocale, body: &str) -> String {
    let language = locale_display_name(locale);
    format!(
        "Translate the following document into {language}.\n\
Return only the full translated document body.\n\
Do not use tools. Do not wrap the entire answer in an outer code fence.\n\
Do not add a preface or commentary.\n\
Leave every placeholder like ⟦CGCODE_…⟧ and ⟦CGINLINE_…⟧ exactly unchanged.\n\
Do not translate source code, shell commands, file paths, URLs, or identifiers.\n\
Keep proper nouns, product names, API names, and established technical English terms in English when standard.\n\
Translate surrounding prose into {language}.\n\
\n\
Document:\n\
{body}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auto_title::app_locale_to_wire;

    #[test]
    fn prompt_contains_required_fragments() {
        let p = build_translate_prompt(AppLocale::Ja, "Hello **world**");
        assert!(p.contains("Translate the following document into Japanese."));
        assert!(p.contains("Return only the full translated document body."));
        assert!(p.contains("Do not use tools."));
        assert!(p.contains("⟦CGCODE_…⟧"));
        assert!(p.contains("⟦CGINLINE_…⟧"));
        assert!(p.contains("Document:\nHello **world**"));
        assert!(!p.contains("conversation title"));
    }

    #[test]
    fn format_serde_camel_case() {
        let md = serde_json::to_string(&DocumentTranslateFormat::Markdown).unwrap();
        let pt = serde_json::to_string(&DocumentTranslateFormat::PlainText).unwrap();
        assert_eq!(md, "\"markdown\"");
        assert_eq!(pt, "\"plainText\"");
    }

    #[test]
    fn format_parse_wire_accepts_known_and_rejects_unknown() {
        assert_eq!(
            DocumentTranslateFormat::parse_wire("markdown").unwrap(),
            DocumentTranslateFormat::Markdown
        );
        assert_eq!(
            DocumentTranslateFormat::parse_wire("plainText").unwrap(),
            DocumentTranslateFormat::PlainText
        );
        assert_eq!(
            DocumentTranslateFormat::parse_wire("docx").unwrap_err(),
            DocumentTranslateError::UnsupportedFormat
        );
        assert_eq!(
            DocumentTranslateFormat::parse_wire("plain_text").unwrap_err(),
            DocumentTranslateError::UnsupportedFormat
        );
    }

    #[test]
    fn unsupported_format_maps_to_invalid_input_with_i18n() {
        let err = DocumentTranslateError::UnsupportedFormat.into_app_command_error();
        assert_eq!(err.code, AppErrorCode::InvalidInput);
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_UNSUPPORTED_FORMAT));
    }

    #[test]
    fn params_result_serde_camel_case_roundtrip() {
        let params = TranslateDocumentParams {
            content: "hi".into(),
            format: "markdown".into(),
            locale: Some("zh_cn".into()),
            display_name: Some("README.md".into()),
        };
        let v = serde_json::to_value(&params).unwrap();
        assert!(v.get("content").is_some());
        assert!(v.get("displayName").is_some());
        assert!(v.get("display_name").is_none());
        // Flat body: no nested `params` envelope.
        assert!(v.get("params").is_none());
        assert_eq!(v["format"], "markdown");

        // Flat JS object keys deserialize directly (HTTP + constructed core).
        let from_flat: TranslateDocumentParams = serde_json::from_value(serde_json::json!({
            "content": "hi",
            "format": "plainText",
            "locale": "en",
            "displayName": "a.txt",
        }))
        .unwrap();
        assert_eq!(from_flat.format, "plainText");
        assert_eq!(from_flat.display_name.as_deref(), Some("a.txt"));

        let result = TranslateDocumentResult {
            translated_content: "你好".into(),
            locale: app_locale_to_wire(AppLocale::ZhCn).into(),
            format: DocumentTranslateFormat::Markdown,
        };
        let v = serde_json::to_value(&result).unwrap();
        assert!(v.get("translatedContent").is_some());
        assert_eq!(v["locale"], "zh_cn");
    }

    #[test]
    fn busy_maps_to_turn_in_progress_with_i18n() {
        let err = DocumentTranslateError::Busy.into_app_command_error();
        assert_eq!(err.code, AppErrorCode::TurnInProgress);
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_BUSY));
    }

    #[test]
    fn agent_none_maps_to_configuration_missing() {
        let err = DocumentTranslateError::AgentNotConfigured.into_app_command_error();
        assert_eq!(err.code, AppErrorCode::ConfigurationMissing);
        assert_eq!(err.i18n_key.as_deref(), Some(I18N_AGENT_NOT_CONFIGURED));
    }
}
