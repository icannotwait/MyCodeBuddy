//! Automatic conversation-title enrollment, cancellation, and atomic commit.
//!
//! Task 3 owns the transactional enrollment/precedence primitives. Capture,
//! claims, the runner, and the coordinator land in later tasks.

pub mod service;
pub mod types;

pub use service::{cancel_job, enroll_new_conversation, finalize_generated_title};
pub use types::{AutoTitleClaim, FinalizeTitleOutcome};
