//! Platform-generated projections over durable protocol-v2 completion state.

use serde::{Deserialize, Serialize};

use super::{
    CompletionAttentionCas, CompletionIntentSource, CompletionOutcome, TerminalCompletionResult,
    COMPLETION_PROTOCOL_VERSION_V2,
};
use crate::db::entities::delegation_task_run::CompletionState;

/// Bounded parent/status projection derived only from the terminal transaction
/// result. Task 16 extends this into the full display card and transport DTOs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionProjectionV2 {
    pub protocol_version: u32,
    pub state: CompletionState,
    pub outcome: Option<CompletionOutcome>,
    pub source: Option<CompletionIntentSource>,
    pub attention: Option<CompletionAttentionCas>,
    pub graph_revision: u64,
}

pub fn project_terminal_completion(result: &TerminalCompletionResult) -> CompletionProjectionV2 {
    let intent = result.evidence.as_ref().map(|evidence| &evidence.intent);
    CompletionProjectionV2 {
        protocol_version: COMPLETION_PROTOCOL_VERSION_V2,
        state: result.state.clone(),
        outcome: intent.map(|intent| intent.outcome),
        source: intent.map(|intent| intent.source),
        attention: result.attention.clone(),
        graph_revision: result.graph_revision,
    }
}

#[cfg(test)]
mod tests {
    use crate::acp::delegation::workflow::{CompletionAttentionCas, TerminalCompletionResult};
    use crate::db::entities::delegation_attention_request::AttentionKind;
    use crate::db::entities::delegation_task_run::CompletionState;

    #[test]
    fn terminal_projection_exposes_only_platform_completion_state() {
        let attention = CompletionAttentionCas {
            attention_id: "attention-1".into(),
            task_id: "task-1".into(),
            kind: AttentionKind::CompletionDecision,
            captured_scope_digest: format!("sha256:{}", "a".repeat(64)),
            latest_run_id: "task-1".into(),
            node_id: "plan-reviewer".into(),
        };
        let result = TerminalCompletionResult {
            state: CompletionState::NeedsDecision,
            evidence: None,
            attention: Some(attention.clone()),
            graph_revision: 7,
        };

        let projection = super::project_terminal_completion(&result);

        assert_eq!(projection.protocol_version, 2);
        assert_eq!(projection.state, CompletionState::NeedsDecision);
        assert_eq!(projection.outcome, None);
        assert_eq!(projection.source, None);
        assert_eq!(projection.attention, Some(attention));
        assert_eq!(projection.graph_revision, 7);
    }
}
