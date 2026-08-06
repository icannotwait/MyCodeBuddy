//! Platform-generated projections over durable protocol-v2 completion state.

use serde::{Deserialize, Serialize};

use super::{
    CompletionAttentionCas, CompletionIntentSource, CompletionOutcome, TerminalCompletionResult,
    COMPLETION_PROTOCOL_VERSION_V2,
};
use crate::db::entities::delegation_task_run::CompletionState;
use crate::db::entities::{delegation_attention_request, delegation_workflow_design_root_binding};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DesignSelfReviewDecisionError {
    #[error("Design self-review decision was superseded")]
    Superseded,
    #[error("Design self-review decision is corrupt")]
    Corrupt,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DesignSelfReviewResolutionV1 {
    version: u32,
    code: String,
    outcome: CompletionOutcome,
    actor_identity: String,
    committed_scope_digest: String,
    graph_revision: u64,
}

/// Read the semantic authority for a Design self-review only when the
/// committed typed attention still matches every platform-owned CAS field.
pub fn validated_design_self_review_outcome(
    binding: &delegation_workflow_design_root_binding::Model,
    attention: Option<&delegation_attention_request::Model>,
) -> Result<Option<CompletionOutcome>, DesignSelfReviewDecisionError> {
    let Some(attention) = attention else {
        return Ok(None);
    };
    if attention.kind != delegation_attention_request::AttentionKind::DesignSelfReviewDecision
        || attention.task_id != binding.task_id
        || attention.latest_run_id.as_deref() != Some(binding.latest_run_id.as_str())
        || attention.node_id.as_deref() != Some(binding.node_id.as_str())
        || attention.captured_scope_digest.as_deref()
            != Some(binding.evidence_scope_digest.as_str())
    {
        return Err(DesignSelfReviewDecisionError::Superseded);
    }
    if attention.status == "open" {
        return Ok(None);
    }
    if attention.status != "resolved"
        || attention.resolution_code.as_deref() != Some("user_outcome_committed")
    {
        return Err(DesignSelfReviewDecisionError::Superseded);
    }
    let resolution = attention
        .resolution_json
        .as_deref()
        .ok_or(DesignSelfReviewDecisionError::Corrupt)
        .and_then(|json| {
            serde_json::from_str::<DesignSelfReviewResolutionV1>(json)
                .map_err(|_| DesignSelfReviewDecisionError::Corrupt)
        })?;
    if resolution.version != 1
        || resolution.code != "user_outcome_committed"
        || resolution.actor_identity.trim().is_empty()
        || resolution.committed_scope_digest != binding.evidence_scope_digest
        || resolution.graph_revision == 0
        || !matches!(
            resolution.outcome,
            CompletionOutcome::Approve
                | CompletionOutcome::ApproveWithMinors
                | CompletionOutcome::RequestChanges
                | CompletionOutcome::Block
        )
    {
        return Err(DesignSelfReviewDecisionError::Corrupt);
    }
    Ok(Some(resolution.outcome))
}

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
    use crate::acp::delegation::workflow::{
        CompletionArtifactV2, CompletionAttentionCas, CompletionEvidenceBindingV2,
        CompletionEvidenceV2, CompletionIntent, CompletionIntentSource, CompletionOutcome,
        CompletionRole, TerminalCompletionResult, ValidatedCompletionEvidence,
    };
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
            final_metric_states: Vec::new(),
        };

        let projection = super::project_terminal_completion(&result);

        assert_eq!(projection.protocol_version, 2);
        assert_eq!(projection.state, CompletionState::NeedsDecision);
        assert_eq!(projection.outcome, None);
        assert_eq!(projection.source, None);
        assert_eq!(projection.attention, Some(attention));
        assert_eq!(projection.graph_revision, 7);
    }

    #[test]
    fn card_v2_is_a_bounded_projection_without_model_claimed_identity() {
        let validated = ValidatedCompletionEvidence {
            evidence: CompletionEvidenceV2 {
                version: 2,
                intent: CompletionIntent {
                    outcome: CompletionOutcome::ApproveWithMinors,
                    summary: Some("x".repeat(5_000)),
                    report_file: Some("reports/task-16.md".into()),
                    source: CompletionIntentSource::AssistantConclusion,
                },
                binding: CompletionEvidenceBindingV2 {
                    workflow_id: "workflow-1".into(),
                    task_id: "task-1".into(),
                    node_id: "plan-reviewer".into(),
                    role: CompletionRole::Reviewer,
                    phase_id: "plan".into(),
                    task_index: None,
                    gate_id: Some("plan".into()),
                    gate_lineage: Some(format!("sha256:{}", "a".repeat(64))),
                    review_round: Some(1),
                    reviewed_task_id: None,
                    reviewed_generation: None,
                    manifest_revision_observed: 1,
                },
                artifact: CompletionArtifactV2::DocumentSha256 {
                    rel_path: "docs/superpowers/plans/task-16.md".into(),
                    digest: format!("sha256:{}", "b".repeat(64)),
                },
                review_scope_digest: format!("sha256:{}", "c".repeat(64)),
                evidence_scope_digest: format!("sha256:{}", "d".repeat(64)),
                captured_at: "2026-08-06T00:00:00Z".into(),
            },
            evidence_validated: true,
        };

        let card = super::CompletionCardV2::project(&validated, None);

        assert!(card.summary.as_deref().unwrap().as_bytes().len()
            <= super::COMPLETION_CARD_SUMMARY_MAX_BYTES);
        assert!(card.evidence_validated);
        assert_eq!(card.role, CompletionRole::Reviewer);
        assert_eq!(card.source, Some(CompletionIntentSource::AssistantConclusion));
        assert_eq!(card.report_file.as_deref(), Some("reports/task-16.md"));
        let json = serde_json::to_value(card).unwrap();
        assert!(json.get("artifact_digest").is_none());
        assert!(json.get("task_id").is_none());
    }
}

#[cfg(test)]
mod design_self_review_decision {
    use chrono::Utc;

    use super::*;
    use crate::db::entities::delegation_attention_request::{self, AttentionKind};
    use crate::db::entities::delegation_workflow_design_root_binding;

    fn binding() -> delegation_workflow_design_root_binding::Model {
        delegation_workflow_design_root_binding::Model {
            workflow_id: "workflow".into(),
            gate_id: "design".into(),
            gate_lineage: format!("sha256:{}", "a".repeat(64)),
            node_id: "platform:design-root".into(),
            task_id: "platform:design-root-task".into(),
            latest_run_id: "platform:design-root-run".into(),
            design_identity: format!("sha256:{}", "b".repeat(64)),
            evidence_scope_digest: format!("sha256:{}", "c".repeat(64)),
            graph_revision: 1,
        }
    }

    fn committed_attention(
        binding: &delegation_workflow_design_root_binding::Model,
    ) -> delegation_attention_request::Model {
        delegation_attention_request::Model {
            request_id: "attention".into(),
            task_id: binding.task_id.clone(),
            parent_conversation_id: 1,
            child_conversation_id: None,
            child_tool_call_id: None,
            status: "resolved".into(),
            message: "decision".into(),
            reply: None,
            resolution_code: Some("user_outcome_committed".into()),
            created_at: Utc::now(),
            resolved_at: Some(Utc::now()),
            kind: AttentionKind::DesignSelfReviewDecision,
            latest_run_id: Some(binding.latest_run_id.clone()),
            node_id: Some(binding.node_id.clone()),
            payload_json: None,
            resolution_json: Some(
                serde_json::json!({
                    "version": 1,
                    "code": "user_outcome_committed",
                    "outcome": "approve",
                    "actor_identity": "authenticated-user",
                    "committed_scope_digest": binding.evidence_scope_digest,
                    "graph_revision": 2,
                })
                .to_string(),
            ),
            captured_scope_digest: Some(binding.evidence_scope_digest.clone()),
        }
    }

    #[test]
    fn committed_decision_requires_the_exact_current_platform_binding() {
        let binding = binding();
        let mut attention = committed_attention(&binding);
        assert_eq!(
            validated_design_self_review_outcome(&binding, Some(&attention)).unwrap(),
            Some(CompletionOutcome::Approve)
        );

        attention.captured_scope_digest = Some(format!("sha256:{}", "d".repeat(64)));
        assert_eq!(
            validated_design_self_review_outcome(&binding, Some(&attention)).unwrap_err(),
            DesignSelfReviewDecisionError::Superseded
        );
        assert_eq!(
            validated_design_self_review_outcome(&binding, None).unwrap(),
            None
        );
    }
}
