//! Automatic conversation-title enrollment, cancellation, and atomic commit.
//!
//! Task 3 owns the transactional enrollment/precedence primitives. Capture,
//! claims, the runner, and the coordinator land in later tasks.

pub mod context;
pub mod coordinator;
pub mod internal_sessions;
pub mod partial_source;
pub mod runner;
pub mod service;
pub mod types;

pub use coordinator::{
    build_production_coordinator, notify_live_coordinator_ready, AutoTitleCoordinator,
};
pub use internal_sessions::{
    InternalAgentSessionRegistry, InternalSessionFilter, InternalSessionPurpose,
};

pub use context::{bound_context, project_visible_prompt};
pub use partial_source::{ManagerPartialSource, PartialAssistantTextSource};
pub use runner::{
    normalize_generated_title, HiddenAgentRunner, ManagerTitleConnectionDriver, TitleAgentRunner,
};
pub use service::{
    apply_usable_completion, cancel_job, capture_prompt_context, claim_is_still_running,
    claim_next_ready, enroll_new_conversation, finalize_generated_title, list_deadline_candidates,
    promote_deadline_elapsed_jobs, promote_deadline_jobs_by_ids, record_attempt_failure,
    recover_interrupted_jobs, DeadlinePromoteParams,
};
pub use types::{
    app_locale_to_wire, parse_supported_app_locale, prompt_capture_from_wire,
    user_launch_context_from_db, AutoTitleAttempt, AutoTitleClaim, AutoTitleRunError,
    CapturedPrompt, CompletionTransition, ConnectionLaunchContext, ConnectionPurpose,
    FailureTransition, FinalizeTitleOutcome, PromptCaptureContext, TurnCompletionSnapshot,
};
