//! Automatic conversation-title enrollment, cancellation, and atomic commit.
//!
//! Task 3 owns the transactional enrollment/precedence primitives. Capture,
//! claims, the runner, and the coordinator land in later tasks.

pub mod context;
pub mod service;
pub mod types;

pub use context::{bound_context, project_visible_prompt};
pub use service::{
    apply_usable_completion, cancel_job, capture_prompt_context, enroll_new_conversation,
    finalize_generated_title,
};
pub use types::{
    app_locale_to_wire, parse_supported_app_locale, prompt_capture_from_wire,
    user_launch_context_from_db, AutoTitleClaim, CapturedPrompt, CompletionTransition,
    ConnectionLaunchContext, ConnectionPurpose, FinalizeTitleOutcome, PromptCaptureContext,
    TurnCompletionSnapshot,
};
