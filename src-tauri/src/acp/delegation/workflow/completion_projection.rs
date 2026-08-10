//! Platform-generated projections over durable protocol-v2 completion state.

use std::collections::HashMap;

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

use super::completion_evidence::{
    completion_attention_public_node_id, completion_validation_workspace,
    preload_completion_validation_context, validate_preloaded_completion_evidence,
    validate_preloaded_completion_evidence_with_context,
};
use super::{
    ArtifactRecoveryPayloadV1, CompletionAttentionCas, CompletionCandidate, CompletionDiagnostic,
    CompletionEvidenceError, CompletionIntentReason, CompletionIntentSource, CompletionOutcome,
    CompletionRole, DesignSelfReviewPayloadV1, TerminalCompletionResult,
    ValidatedCompletionEvidence, COMPLETION_PROTOCOL_VERSION_V2,
};
use crate::db::entities::delegation_task_run::{CompletionState, DelegationRunStatus};
use crate::db::entities::{
    delegation_attention_request, delegation_task_run, delegation_workflow,
    delegation_workflow_design_root_binding, delegation_workflow_node_binding,
    delegation_workflow_run_binding,
};

pub const COMPLETION_CARD_SUMMARY_MAX_BYTES: usize = 1024;

#[cfg(test)]
thread_local! {
    static COMPLETION_PROJECTION_LOAD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_completion_projection_load_count() {
    COMPLETION_PROJECTION_LOAD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn completion_projection_load_count() -> usize {
    COMPLETION_PROJECTION_LOAD_COUNT.with(std::cell::Cell::get)
}

#[cfg(test)]
fn note_completion_projection_load() {
    COMPLETION_PROJECTION_LOAD_COUNT.with(|count| count.set(count.get() + 1));
}

#[cfg(not(test))]
fn note_completion_projection_load() {}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionCardState {
    Resolved,
    NeedsDecision,
    Blocked,
}

impl CompletionCardState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::NeedsDecision => "needs_decision",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionCardV2 {
    pub state: CompletionCardState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<CompletionRole>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CompletionOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CompletionIntentSource>,
    pub evidence_validated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<CompletionAttentionCas>,
}

impl CompletionCardV2 {
    pub fn project(
        validated: &ValidatedCompletionEvidence,
        attention: Option<CompletionAttentionCas>,
    ) -> Self {
        let evidence = &validated.evidence;
        Self {
            state: display_state(CompletionState::Resolved, Some(evidence.intent.outcome)),
            role: Some(evidence.binding.role),
            outcome: Some(evidence.intent.outcome),
            summary: bounded_summary(evidence.intent.summary.as_deref()),
            report_file: evidence.intent.report_file.clone(),
            source: Some(evidence.intent.source),
            evidence_validated: validated.evidence_validated,
            attention,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionProjectionV2 {
    pub protocol_version: u32,
    pub card: CompletionCardV2,
    pub graph_revision: u64,
}

pub fn project_terminal_completion(result: &TerminalCompletionResult) -> CompletionProjectionV2 {
    let card = match result.evidence.as_ref() {
        Some(evidence) => CompletionCardV2::project(
            &ValidatedCompletionEvidence {
                evidence: evidence.clone(),
                evidence_validated: true,
            },
            result.attention.clone(),
        ),
        None => CompletionCardV2 {
            state: display_state(result.state.clone(), None),
            role: None,
            outcome: None,
            summary: None,
            report_file: None,
            source: None,
            evidence_validated: false,
            attention: result.attention.clone(),
        },
    };
    CompletionProjectionV2 {
        protocol_version: COMPLETION_PROTOCOL_VERSION_V2,
        card,
        graph_revision: result.graph_revision,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionAttentionPayload {
    version: u32,
    reason_code: CompletionIntentReason,
    role: CompletionRole,
    legal_outcomes: Vec<CompletionOutcome>,
    bounded_candidates: Vec<CompletionCandidate>,
    diagnostics: Vec<CompletionDiagnostic>,
}

pub(crate) struct WorkflowCompletionProjectionBatch {
    pub validated_by_task: HashMap<String, ValidatedCompletionEvidence>,
    pub completion_by_task: HashMap<String, CompletionProjectionV2>,
}

pub async fn load_completion_projection<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<Option<CompletionProjectionV2>, CompletionEvidenceError> {
    note_completion_projection_load();
    let run = delegation_task_run::Entity::find_by_id(task_id)
        .one(conn)
        .await
        .map_err(persistence)?;
    let Some(run) = run else {
        return load_design_self_review_projection(conn, task_id).await;
    };
    let Some(state) = run.completion_state.clone() else {
        return Ok(None);
    };
    let Some(binding) = delegation_workflow_run_binding::Entity::find_by_id(task_id)
        .one(conn)
        .await
        .map_err(persistence)?
    else {
        return Err(invalid_projection("completion run binding is missing"));
    };
    let Some(workflow) = delegation_workflow::Entity::find_by_id(&binding.workflow_id)
        .one(conn)
        .await
        .map_err(persistence)?
    else {
        return Err(invalid_projection("completion workflow is missing"));
    };
    if workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2) {
        return Ok(None);
    }

    let node = delegation_workflow_node_binding::Entity::find_by_id((
        binding.workflow_id.clone(),
        binding.node_id.clone(),
    ))
    .one(conn)
    .await
    .map_err(persistence)?
    .ok_or_else(|| invalid_projection("completion node binding is missing"))?;
    let latest_binding = delegation_workflow_run_binding::Entity::find()
        .filter(delegation_workflow_run_binding::Column::WorkflowId.eq(&binding.workflow_id))
        .filter(delegation_workflow_run_binding::Column::NodeId.eq(&binding.node_id))
        .order_by_desc(delegation_workflow_run_binding::Column::LineageOrdinal)
        .one(conn)
        .await
        .map_err(persistence)?
        .ok_or_else(|| invalid_projection("completion node has no latest run binding"))?;
    if latest_binding.task_id != run.task_id
        || run.status != DelegationRunStatus::Completed
        || run.parent_conversation_id != workflow.parent_conversation_id
        || node.retired_revision.is_some()
        || node.node_outcome.is_some()
    {
        return Err(invalid_projection(
            "completion is not attached to the current terminal workflow run",
        ));
    }

    let graph_revision = u64::try_from(workflow.graph_revision).map_err(|_| {
        CompletionEvidenceError::InvalidTerminalState("negative graph revision".into())
    })?;
    let validated = if state == CompletionState::Resolved {
        Some(validate_preloaded_completion_evidence(conn, &run, &binding, &workflow, &node).await?)
    } else {
        None
    };
    let attentions = if state == CompletionState::Resolved {
        Vec::new()
    } else {
        delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(task_id))
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .filter(
                delegation_attention_request::Column::Kind
                    .ne(delegation_attention_request::AttentionKind::ChildQuestion),
            )
            .order_by_desc(delegation_attention_request::Column::CreatedAt)
            .all(conn)
            .await
            .map_err(persistence)?
    };
    project_loaded_workflow_completion(
        &run,
        &binding,
        &workflow,
        &node,
        &latest_binding,
        validated.as_ref(),
        &attentions,
        graph_revision,
    )
}

#[allow(clippy::too_many_arguments)]
fn project_loaded_workflow_completion(
    run: &delegation_task_run::Model,
    binding: &delegation_workflow_run_binding::Model,
    workflow: &delegation_workflow::Model,
    node: &delegation_workflow_node_binding::Model,
    latest_binding: &delegation_workflow_run_binding::Model,
    validated: Option<&ValidatedCompletionEvidence>,
    attentions: &[delegation_attention_request::Model],
    graph_revision: u64,
) -> Result<Option<CompletionProjectionV2>, CompletionEvidenceError> {
    let Some(state) = run.completion_state.clone() else {
        return Ok(None);
    };
    if workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2) {
        return Ok(None);
    }
    if binding.workflow_id != workflow.workflow_id
        || binding.node_id != node.node_id
        || node.workflow_id != workflow.workflow_id
        || latest_binding.workflow_id != workflow.workflow_id
        || latest_binding.node_id != node.node_id
        || latest_binding.task_id != run.task_id
        || run.status != DelegationRunStatus::Completed
        || run.parent_conversation_id != workflow.parent_conversation_id
        || node.retired_revision.is_some()
        || node.node_outcome.is_some()
    {
        return Err(invalid_projection(
            "completion is not attached to the current terminal workflow run",
        ));
    }
    if state == CompletionState::Resolved {
        let validated = validated.ok_or_else(|| {
            invalid_projection("resolved completion has no validated durable evidence")
        })?;
        return Ok(Some(CompletionProjectionV2 {
            protocol_version: COMPLETION_PROTOCOL_VERSION_V2,
            card: CompletionCardV2::project(validated, None),
            graph_revision,
        }));
    }
    if run.completion_outcome.is_some() || run.completion_evidence_json.is_some() {
        return Err(invalid_projection(
            "unresolved completion carries resolved evidence fields",
        ));
    }
    if attentions.len() != 1 {
        return Err(invalid_projection(
            "unresolved completion must have exactly one open typed attention",
        ));
    }
    let attention = &attentions[0];
    let expected_kind = match state {
        CompletionState::NeedsDecision => {
            delegation_attention_request::AttentionKind::CompletionDecision
        }
        CompletionState::ArtifactRecovery => {
            delegation_attention_request::AttentionKind::CompletionArtifactRecovery
        }
        CompletionState::Resolved => unreachable!("resolved completion returned above"),
    };
    let cas = validate_terminal_attention(attention, run, binding, workflow, &expected_kind)?;
    let node_role = parse_role(&node.role)
        .ok_or_else(|| invalid_projection("completion node has an unsupported durable role"))?;

    let (role, outcome, source, summary) = match attention.kind {
        delegation_attention_request::AttentionKind::CompletionDecision => {
            let payload: DecisionAttentionPayload = parse_attention_payload(attention)?;
            if payload.version != 1
                || payload.role != node_role
                || !legal_outcomes_match(payload.role, &payload.legal_outcomes)
                || payload
                    .bounded_candidates
                    .iter()
                    .any(|candidate| !payload.role.accepts(candidate.outcome))
            {
                return Err(invalid_projection(
                    "completion decision payload does not match durable scope",
                ));
            }
            let _ = (&payload.reason_code, &payload.diagnostics);
            let source = payload
                .bounded_candidates
                .first()
                .map(|candidate| candidate.source);
            (
                Some(payload.role),
                None,
                source,
                bounded_summary(Some(&attention.message)),
            )
        }
        delegation_attention_request::AttentionKind::CompletionArtifactRecovery => {
            let payload: ArtifactRecoveryPayloadV1 = parse_attention_payload(attention)?;
            if payload.version != 1
                || payload.producer_task_id != run.task_id
                || payload.producer_scope_digest != cas.captured_scope_digest
                || !node_role.accepts(payload.normalized_intent.outcome)
            {
                return Err(invalid_projection(
                    "artifact recovery payload does not match durable scope",
                ));
            }
            (
                Some(node_role),
                Some(payload.normalized_intent.outcome),
                Some(payload.normalized_intent.source),
                bounded_summary(payload.normalized_intent.summary.as_deref())
                    .or_else(|| bounded_summary(Some(&attention.message))),
            )
        }
        delegation_attention_request::AttentionKind::DesignSelfReviewDecision
        | delegation_attention_request::AttentionKind::ChildQuestion => {
            return Err(invalid_projection(
                "terminal completion has an incompatible attention kind",
            ))
        }
    };

    Ok(Some(CompletionProjectionV2 {
        protocol_version: COMPLETION_PROTOCOL_VERSION_V2,
        card: CompletionCardV2 {
            state: display_state(state, outcome),
            role,
            outcome,
            summary,
            report_file: None,
            source,
            evidence_validated: false,
            attention: Some(cas),
        },
        graph_revision,
    }))
}

pub(crate) async fn load_workflow_completion_projection_batch<C: ConnectionTrait>(
    conn: &C,
    workflow: &delegation_workflow::Model,
    normalized: &super::types::NormalizedManifest,
    nodes: &[delegation_workflow_node_binding::Model],
    run_bindings: &[delegation_workflow_run_binding::Model],
    runs: &[delegation_task_run::Model],
) -> Result<WorkflowCompletionProjectionBatch, CompletionEvidenceError> {
    let mut batch = WorkflowCompletionProjectionBatch {
        validated_by_task: HashMap::new(),
        completion_by_task: HashMap::new(),
    };
    if workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2) {
        return Ok(batch);
    }

    let node_by_id: HashMap<&str, &delegation_workflow_node_binding::Model> = nodes
        .iter()
        .map(|node| (node.node_id.as_str(), node))
        .collect();
    let mut latest_by_node: HashMap<&str, &delegation_workflow_run_binding::Model> = HashMap::new();
    for binding in run_bindings {
        latest_by_node
            .entry(binding.node_id.as_str())
            .and_modify(|latest| {
                if binding.lineage_ordinal > latest.lineage_ordinal {
                    *latest = binding;
                }
            })
            .or_insert(binding);
    }

    let mut run_by_id: HashMap<String, delegation_task_run::Model> = runs
        .iter()
        .cloned()
        .map(|run| (run.task_id.clone(), run))
        .collect();
    let missing_task_ids = latest_by_node
        .values()
        .filter(|binding| !run_by_id.contains_key(binding.task_id.as_str()))
        .map(|binding| binding.task_id.clone())
        .collect::<Vec<_>>();
    if !missing_task_ids.is_empty() {
        for run in delegation_task_run::Entity::find()
            .filter(delegation_task_run::Column::TaskId.is_in(missing_task_ids))
            .all(conn)
            .await
            .map_err(persistence)?
        {
            run_by_id.insert(run.task_id.clone(), run);
        }
    }

    let attention_task_ids = latest_by_node
        .values()
        .filter_map(|binding| {
            run_by_id
                .get(binding.task_id.as_str())
                .filter(|run| {
                    matches!(
                        run.completion_state,
                        Some(CompletionState::NeedsDecision | CompletionState::ArtifactRecovery)
                    )
                })
                .map(|_| binding.task_id.clone())
        })
        .collect::<Vec<_>>();
    let attentions = if attention_task_ids.is_empty() {
        Vec::new()
    } else {
        delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.is_in(attention_task_ids))
            .filter(delegation_attention_request::Column::Status.eq("open"))
            .filter(
                delegation_attention_request::Column::Kind
                    .ne(delegation_attention_request::AttentionKind::ChildQuestion),
            )
            .order_by_desc(delegation_attention_request::Column::CreatedAt)
            .all(conn)
            .await
            .map_err(persistence)?
    };
    let mut attentions_by_task: HashMap<String, Vec<delegation_attention_request::Model>> =
        HashMap::new();
    for attention in attentions {
        attentions_by_task
            .entry(attention.task_id.clone())
            .or_default()
            .push(attention);
    }
    let graph_revision = u64::try_from(workflow.graph_revision).map_err(|_| {
        CompletionEvidenceError::InvalidTerminalState("negative graph revision".into())
    })?;
    let mut validation_context_by_workspace = HashMap::new();

    for binding in latest_by_node.values() {
        let Some(run) = run_by_id.get(binding.task_id.as_str()) else {
            continue;
        };
        if run.completion_state.is_none() {
            continue;
        }
        let node = node_by_id
            .get(binding.node_id.as_str())
            .copied()
            .ok_or_else(|| invalid_projection("completion node binding is missing"))?;
        if run.completion_state == Some(CompletionState::Resolved) {
            let workspace = completion_validation_workspace(run)?;
            if !validation_context_by_workspace.contains_key(workspace) {
                let preload =
                    preload_completion_validation_context(conn, workflow, normalized, workspace)
                        .await?;
                validation_context_by_workspace.insert(workspace.to_string(), preload);
            }
            let preload = validation_context_by_workspace
                .get(workspace)
                .expect("completion validation preload was inserted");
            let validated = match validate_preloaded_completion_evidence_with_context(
                conn, run, binding, workflow, node, preload,
            )
            .await
            {
                Ok(validated) => validated,
                Err(CompletionEvidenceError::Persistence(message)) => {
                    return Err(CompletionEvidenceError::Persistence(message));
                }
                Err(_) => continue,
            };
            batch
                .validated_by_task
                .insert(binding.task_id.clone(), validated);
        }
        let empty = Vec::new();
        let attentions = attentions_by_task
            .get(binding.task_id.as_str())
            .unwrap_or(&empty);
        if let Some(completion) = project_loaded_workflow_completion(
            run,
            binding,
            workflow,
            node,
            binding,
            batch.validated_by_task.get(binding.task_id.as_str()),
            attentions,
            graph_revision,
        )? {
            batch
                .completion_by_task
                .insert(binding.task_id.clone(), completion);
        }
    }
    Ok(batch)
}

async fn load_design_self_review_projection<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<Option<CompletionProjectionV2>, CompletionEvidenceError> {
    let Some(binding) = delegation_workflow_design_root_binding::Entity::find()
        .filter(delegation_workflow_design_root_binding::Column::TaskId.eq(task_id))
        .one(conn)
        .await
        .map_err(persistence)?
    else {
        return Ok(None);
    };
    let Some(workflow) = delegation_workflow::Entity::find_by_id(&binding.workflow_id)
        .one(conn)
        .await
        .map_err(persistence)?
    else {
        return Err(invalid_projection("Design self-review workflow is missing"));
    };
    if workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2) {
        return Ok(None);
    }
    let attention = delegation_attention_request::Entity::find()
        .filter(delegation_attention_request::Column::TaskId.eq(task_id))
        .filter(
            delegation_attention_request::Column::Kind
                .eq(delegation_attention_request::AttentionKind::DesignSelfReviewDecision),
        )
        .order_by_desc(delegation_attention_request::Column::CreatedAt)
        .one(conn)
        .await
        .map_err(persistence)?;
    let Some(attention) = attention else {
        return Err(invalid_projection(
            "Design self-review binding has no typed attention",
        ));
    };
    if attention.parent_conversation_id != workflow.parent_conversation_id
        || attention.task_id != binding.task_id
        || attention.latest_run_id.as_deref() != Some(binding.latest_run_id.as_str())
        || attention.node_id.as_deref() != Some(binding.node_id.as_str())
        || attention.captured_scope_digest.as_deref()
            != Some(binding.evidence_scope_digest.as_str())
        || !matches!(attention.status.as_str(), "open" | "resolved")
    {
        return Err(invalid_projection(
            "Design self-review attention does not match durable scope",
        ));
    }
    let payload: DesignSelfReviewPayloadV1 = parse_attention_payload(&attention)?;
    if payload.version != 1
        || payload.design_identity != binding.design_identity
        || payload.gate_lineage != binding.gate_lineage
        || !legal_outcomes_match(CompletionRole::Reviewer, &payload.legal_outcomes)
    {
        return Err(invalid_projection(
            "Design self-review payload does not match durable scope",
        ));
    }
    let outcome = validated_design_self_review_outcome(&binding, Some(&attention))
        .map_err(|error| CompletionEvidenceError::InvalidTerminalState(error.to_string()))?;
    let open = attention.status == "open";
    let card = CompletionCardV2 {
        state: if open {
            CompletionCardState::NeedsDecision
        } else {
            display_state(CompletionState::Resolved, outcome)
        },
        role: Some(CompletionRole::Reviewer),
        outcome,
        summary: bounded_summary(Some(&attention.message)),
        report_file: None,
        source: outcome.map(|_| CompletionIntentSource::UserAdjudication),
        evidence_validated: outcome.is_some(),
        attention: if open {
            Some(CompletionAttentionCas {
                attention_id: attention.request_id.clone(),
                task_id: binding.task_id.clone(),
                kind: attention.kind.clone(),
                captured_scope_digest: binding.evidence_scope_digest.clone(),
                latest_run_id: binding.latest_run_id.clone(),
                node_id: completion_attention_public_node_id(&binding.node_id),
            })
        } else {
            None
        },
    };
    Ok(Some(CompletionProjectionV2 {
        protocol_version: COMPLETION_PROTOCOL_VERSION_V2,
        card,
        graph_revision: u64::try_from(workflow.graph_revision).map_err(|_| {
            CompletionEvidenceError::InvalidTerminalState("negative graph revision".into())
        })?,
    }))
}

fn persistence(error: sea_orm::DbErr) -> CompletionEvidenceError {
    CompletionEvidenceError::Persistence(error.to_string())
}

fn invalid_projection(message: impl Into<String>) -> CompletionEvidenceError {
    CompletionEvidenceError::InvalidTerminalState(message.into())
}

fn parse_attention_payload<T: serde::de::DeserializeOwned>(
    attention: &delegation_attention_request::Model,
) -> Result<T, CompletionEvidenceError> {
    let json = attention
        .payload_json
        .as_deref()
        .ok_or_else(|| invalid_projection("typed completion attention payload is missing"))?;
    serde_json::from_str(json).map_err(|error| {
        invalid_projection(format!("typed completion attention is corrupt: {error}"))
    })
}

fn validate_terminal_attention(
    attention: &delegation_attention_request::Model,
    run: &delegation_task_run::Model,
    binding: &delegation_workflow_run_binding::Model,
    workflow: &delegation_workflow::Model,
    expected_kind: &delegation_attention_request::AttentionKind,
) -> Result<CompletionAttentionCas, CompletionEvidenceError> {
    let binding_scope = binding
        .evidence_scope_digest
        .as_deref()
        .ok_or_else(|| invalid_projection("unresolved completion scope is missing"))?;
    if attention.kind != *expected_kind
        || attention.task_id != run.task_id
        || attention.parent_conversation_id != workflow.parent_conversation_id
        || attention.latest_run_id.as_deref() != Some(run.task_id.as_str())
        || attention.node_id.as_deref() != Some(binding.node_id.as_str())
        || attention.captured_scope_digest.as_deref() != Some(binding_scope)
    {
        return Err(invalid_projection(
            "open completion attention does not match durable scope",
        ));
    }
    Ok(CompletionAttentionCas {
        attention_id: attention.request_id.clone(),
        task_id: run.task_id.clone(),
        kind: attention.kind.clone(),
        captured_scope_digest: binding_scope.to_string(),
        latest_run_id: run.task_id.clone(),
        node_id: completion_attention_public_node_id(&binding.node_id),
    })
}

fn legal_outcomes_match(role: CompletionRole, actual: &[CompletionOutcome]) -> bool {
    let expected: &[CompletionOutcome] = match role {
        CompletionRole::Reviewer => &[
            CompletionOutcome::Approve,
            CompletionOutcome::ApproveWithMinors,
            CompletionOutcome::RequestChanges,
            CompletionOutcome::Block,
        ],
        CompletionRole::Author | CompletionRole::Implementer | CompletionRole::Fixer => &[
            CompletionOutcome::Done,
            CompletionOutcome::DoneWithConcerns,
            CompletionOutcome::Blocked,
        ],
    };
    actual.len() == expected.len()
        && expected
            .iter()
            .all(|outcome| actual.iter().any(|actual| actual == outcome))
}

fn display_state(
    state: CompletionState,
    outcome: Option<CompletionOutcome>,
) -> CompletionCardState {
    if matches!(
        outcome,
        Some(CompletionOutcome::Block | CompletionOutcome::Blocked)
    ) || state == CompletionState::ArtifactRecovery
    {
        CompletionCardState::Blocked
    } else if state == CompletionState::NeedsDecision {
        CompletionCardState::NeedsDecision
    } else {
        CompletionCardState::Resolved
    }
}

fn bounded_summary(summary: Option<&str>) -> Option<String> {
    let redacted = super::dto::redact_display_string(summary?);
    let summary = redacted.trim();
    if summary.is_empty() {
        return None;
    }
    if summary.len() <= COMPLETION_CARD_SUMMARY_MAX_BYTES {
        return Some(summary.to_string());
    }
    let mut end = COMPLETION_CARD_SUMMARY_MAX_BYTES;
    while !summary.is_char_boundary(end) {
        end -= 1;
    }
    Some(summary[..end].trim_end().to_string())
}

fn parse_role(role: &str) -> Option<CompletionRole> {
    match role {
        "reviewer" => Some(CompletionRole::Reviewer),
        "author" => Some(CompletionRole::Author),
        "implementer" => Some(CompletionRole::Implementer),
        "fixer" => Some(CompletionRole::Fixer),
        _ => None,
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
        assert_eq!(
            projection.card.state,
            super::CompletionCardState::NeedsDecision
        );
        assert_eq!(projection.card.outcome, None);
        assert_eq!(projection.card.source, None);
        assert_eq!(projection.card.attention, Some(attention));
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

        assert!(card.summary.as_deref().unwrap().len() <= super::COMPLETION_CARD_SUMMARY_MAX_BYTES);
        assert!(card.evidence_validated);
        assert_eq!(card.role, Some(CompletionRole::Reviewer));
        assert_eq!(
            card.source,
            Some(CompletionIntentSource::AssistantConclusion)
        );
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
