use std::sync::Arc;

use sea_orm::DatabaseConnection;

use crate::models::agent::AgentType;
use crate::models::system::AppLocale;

/// Immutable, event-owned snapshot of a completed turn for lifecycle title work.
/// Built under the SessionState lock at TurnComplete and never re-reads live state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnCompletionSnapshot {
    pub conversation_id: i32,
    pub turn_token: String,
    pub locale: AppLocale,
    pub final_text: Arc<str>,
}

/// Result of applying a usable completion to an auto-title job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionTransition {
    pub usable_turn_seq: i32,
    pub became_ready: bool,
}

/// Claim for a single automatic-title generation attempt. Task 8's coordinator
/// builds this from a claimed `running` job; Task 3 only needs it as the
/// input to [`super::service::finalize_generated_title`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTitleClaim {
    pub conversation_id: i32,
    pub attempt: i32,
    pub agent: AgentType,
    pub first_user_text: String,
    pub first_assistant_text: String,
    pub locale: AppLocale,
    pub attempt_turn_seq: i32,
}

/// Runner input for one hidden title-generation attempt (Task 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTitleAttempt {
    pub conversation_id: i32,
    pub attempt: i32,
    pub agent: AgentType,
    pub locale: AppLocale,
    pub first_user_text: String,
    pub first_assistant_text: String,
}

/// Failure modes for an isolated title run. Cancellation and timeout are
/// distinct so the coordinator can decide retry policy without re-parsing
/// strings.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AutoTitleRunError {
    #[error("title run cancelled")]
    Cancelled,
    #[error("title agent unavailable or disabled")]
    Unavailable,
    #[error("title agent spawn failed: {0}")]
    Spawn(String),
    #[error("title agent identity wait failed: {0}")]
    Identity(String),
    #[error("internal session registry failed: {0}")]
    Registry(String),
    #[error("interactive permission or question on title run")]
    Interactive,
    #[error("title run timed out")]
    Timeout,
    #[error("title run stopped abnormally: {0}")]
    AbnormalStop(String),
    #[error("title run produced empty output")]
    EmptyOutput,
}

/// Outcome of an atomic generated-title commit. Only [`Committed`] may trigger
/// the post-commit conversation upsert once the coordinator lands in Task 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeTitleOutcome {
    Committed,
    Cancelled,
}

/// Durable transition after a failed title attempt. `Ready` means attempt two
/// can start immediately (a newer usable turn already exists).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureTransition {
    Ready,
    RetryWait,
    Exhausted,
    Cancelled,
}

/// Why a connection was launched. Title capture bypasses internal purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionPurpose {
    User,
    Delegation,
    InternalProbe,
    InternalTitle,
}

/// Launch-time purpose and optional inherited locale for a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionLaunchContext {
    pub purpose: ConnectionPurpose,
    pub inherited_locale: Option<AppLocale>,
}

impl Default for ConnectionLaunchContext {
    /// Test-only English user default. Production UI/automation roots must use
    /// [`user_launch_context_from_db`]; chat roots/resume use
    /// `channel_launch_context_from_db`.
    fn default() -> Self {
        Self {
            purpose: ConnectionPurpose::User,
            inherited_locale: Some(AppLocale::En),
        }
    }
}

/// Optional explicit visible text and locale supplied by a prompt producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptCaptureContext {
    pub visible_text: Option<String>,
    pub locale: Option<AppLocale>,
}

impl PromptCaptureContext {
    pub fn new(visible_text: Option<String>, locale: Option<AppLocale>) -> Self {
        Self {
            visible_text,
            locale,
        }
    }
}

/// Normalized visible prompt text and resolved locale after capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedPrompt {
    pub visible_text: String,
    pub locale: AppLocale,
}

/// Lossy parser for the ten supported snake-case wire locale identifiers.
/// Unknown, empty, and mixed-case values return `None` so callers can fall back.
pub fn parse_supported_app_locale(value: Option<&str>) -> Option<AppLocale> {
    match value? {
        "en" => Some(AppLocale::En),
        "zh_cn" => Some(AppLocale::ZhCn),
        "zh_tw" => Some(AppLocale::ZhTw),
        "ja" => Some(AppLocale::Ja),
        "ko" => Some(AppLocale::Ko),
        "es" => Some(AppLocale::Es),
        "de" => Some(AppLocale::De),
        "fr" => Some(AppLocale::Fr),
        "pt" => Some(AppLocale::Pt),
        "ar" => Some(AppLocale::Ar),
        _ => None,
    }
}

/// Persist locale as the same snake-case wire identifier accepted by
/// [`parse_supported_app_locale`].
pub fn app_locale_to_wire(locale: AppLocale) -> &'static str {
    match locale {
        AppLocale::En => "en",
        AppLocale::ZhCn => "zh_cn",
        AppLocale::ZhTw => "zh_tw",
        AppLocale::Ja => "ja",
        AppLocale::Ko => "ko",
        AppLocale::Es => "es",
        AppLocale::De => "de",
        AppLocale::Fr => "fr",
        AppLocale::Pt => "pt",
        AppLocale::Ar => "ar",
    }
}

/// Build optional capture context from wire `visibleText` / `locale` fields.
///
/// - Both absent → `None` (manager falls back to ACP blocks + connection locale).
/// - `Some(visible_text)` including empty is preserved as authoritative.
/// - Locale is lossy: unknown/mixed-case wire values become `None` so the
///   request is accepted and the connection effective locale is used.
pub fn prompt_capture_from_wire(
    visible_text: Option<String>,
    locale: Option<String>,
) -> Option<PromptCaptureContext> {
    if visible_text.is_none() && locale.is_none() {
        return None;
    }
    Some(PromptCaptureContext::new(
        visible_text,
        parse_supported_app_locale(locale.as_deref()),
    ))
}

/// User-purpose launch context from persisted `SystemLanguageSettings.language`.
/// Production UI and automation roots must use this rather than a context-free
/// English default. Load failures fall back to English.
pub async fn user_launch_context_from_db(conn: &DatabaseConnection) -> ConnectionLaunchContext {
    let language = crate::commands::system_settings::load_system_language_settings(conn)
        .await
        .map(|settings| settings.language)
        .unwrap_or_default();
    ConnectionLaunchContext {
        purpose: ConnectionPurpose::User,
        inherited_locale: Some(language),
    }
}
