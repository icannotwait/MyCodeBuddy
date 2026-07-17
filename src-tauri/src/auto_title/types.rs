use crate::models::agent::AgentType;
use crate::models::system::AppLocale;

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

/// Outcome of an atomic generated-title commit. Only [`Committed`] may trigger
/// the post-commit conversation upsert once the coordinator lands in Task 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeTitleOutcome {
    Committed,
    Cancelled,
}
