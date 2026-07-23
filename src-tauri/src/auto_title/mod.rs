//! Automatic conversation-title enrollment, cancellation, and atomic commit.
//!
//! Task 3 owns the transactional enrollment/precedence primitives. Capture,
//! claims, the runner, and the coordinator land in later tasks.

pub mod context;
pub mod coordinator;
pub mod http;
pub mod internal_sessions;
pub mod partial_source;
pub mod runner;
pub mod service;
pub mod title_key;
pub mod title_settings;
pub mod types;

pub use title_key::{
    delete_title_api_key, get_title_api_key, set_title_api_key, title_key_fingerprint,
    TitleKeyState, TITLE_API_KEY_ACCOUNT,
};
pub use title_settings::{
    auto_title_enabled, next_config_gen, normalize_and_validate_api_url, parse_config_barrier,
    parse_config_gen, ApiKeyUpdate, SetAutoTitleApiConfigRequest, SetDocumentTranslateAgentRequest,
    BARRIER_RAISED, CONFIG_GEN_I64_MAX, KEY_AUTO_TITLE_API_KEY_FP, KEY_AUTO_TITLE_API_URL,
    KEY_AUTO_TITLE_CONFIG_BARRIER, KEY_AUTO_TITLE_CONFIG_GEN, KEY_AUTO_TITLE_JOBS_PURGED_FOR_API_V1,
    KEY_AUTO_TITLE_MODEL, KEY_DOCUMENT_TRANSLATE_AGENT,
};

pub use coordinator::{
    build_production_coordinator, notify_live_coordinator_ready, AutoTitleCoordinator,
};
pub use internal_sessions::{
    InternalAgentSessionRegistry, InternalSessionFilter, InternalSessionPurpose,
};

pub use context::{bound_context, project_visible_prompt};
pub use partial_source::{ManagerPartialSource, PartialAssistantTextSource};
pub use http::{
    extract_completion_content, normalize_chat_completions_url, DirectCompletionTitleRunner,
    LazyReqwestTitleTransport, TitleHttpError, TitleHttpResponse, TitleHttpTransport,
};
pub use runner::{normalize_generated_title, TitleAgentRunner};
#[cfg(any(test, feature = "test-utils"))]
pub use runner::{HiddenAgentRunner, ManagerTitleConnectionDriver};
pub use service::{
    apply_usable_completion, cancel_job, capture_prompt_context, claim_is_still_running,
    claim_next_ready, claim_next_ready_with_config, enroll_new_conversation,
    finalize_generated_title, list_deadline_candidates, promote_deadline_elapsed_jobs,
    promote_deadline_jobs_by_ids, purge_auto_title_jobs_for_api_v1_if_needed,
    record_attempt_failure, recover_interrupted_jobs, DeadlinePromoteParams,
};
#[cfg(any(test, feature = "test-utils"))]
pub use service::enable_title_api_for_test;
pub use types::{
    app_locale_to_wire, parse_supported_app_locale, prompt_capture_from_wire,
    user_launch_context_from_db, AutoTitleApiConfig, AutoTitleAttempt, AutoTitleClaim,
    AutoTitleRunError, CapturedPrompt, CompletionTransition, ConnectionLaunchContext,
    ConnectionPurpose, FailureTransition, FinalizeTitleOutcome, PromptCaptureContext,
    TurnCompletionSnapshot,
};
