//! Atomic protocol-v2 terminal evidence and artifact-recovery materialization.

use std::path::Path;

use chrono::{SecondsFormat, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
    TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::admission::bump_graph_revision;
use super::artifact_resolver::{
    resolve_document, resolve_producer_completion, resolve_reviewer_completion, ArtifactError,
    ArtifactFailure, ArtifactKind, GitHeadV1Artifact, ResolvedArtifact,
};
use super::completion_intent::{
    resolve_completion_intent, CompletionCandidate, CompletionDiagnostic, CompletionIntent,
    CompletionIntentReason, CompletionIntentSource, CompletionOutcome, CompletionReportCandidate,
    CompletionResolution, CompletionResolverInput, CompletionRole, CompletionToolIntent,
};
use super::error::CompletionEvidenceError;
use super::evidence_scope::{
    build_admission_completion_context, validate_completion_evidence, AdmissionCandidate,
    WorkflowStore,
};
use super::types::{
    AdmissionCompletionContextV2, ArtifactSubjectIdentityV2, CompletionArtifactV2,
    CompletionEvidenceBindingV2, CompletionEvidenceV2, EvidenceValidationContext,
    COMPLETION_PROTOCOL_VERSION_V2,
};
use crate::acp::delegation::attention::{
    open_terminal_completion_attention_txn, TerminalCompletionAttentionInput,
    ATTENTION_PAYLOAD_MAX_BYTES,
};
use crate::db::entities::delegation_attention_request::{self, AttentionKind};
use crate::db::entities::delegation_task_run::{self, CompletionState, DelegationRunStatus};
use crate::db::entities::delegation_workflow::CompletionProtocolMode;
use crate::db::entities::{
    delegation_completion_tool_intent, delegation_workflow, delegation_workflow_node_binding,
    delegation_workflow_outbox_event, delegation_workflow_run_binding,
};
use crate::db::AppDatabase;

const MAX_DOCUMENT_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
const COMPLETION_DECISION_MESSAGE: &str = "Completion outcome requires a direct decision.";
const ARTIFACT_RECOVERY_MESSAGE: &str = "Completion artifact is not yet available.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedReportCandidate {
    pub path: String,
    pub contents: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCompletionInput {
    pub task_id: String,
    pub terminal_status: DelegationRunStatus,
    pub final_assistant_text: String,
    pub pre_read_reports: Vec<ValidatedReportCandidate>,
    pub pre_read_artifact: Option<ResolvedArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionAttentionCas {
    pub attention_id: String,
    pub task_id: String,
    pub kind: AttentionKind,
    pub captured_scope_digest: String,
    pub latest_run_id: String,
    pub node_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCompletionResult {
    pub state: CompletionState,
    pub evidence: Option<CompletionEvidenceV2>,
    pub attention: Option<CompletionAttentionCas>,
    pub graph_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionSourceAuditRef {
    ToolIntent { intent_id: String },
    AssistantConclusion,
    Report { report_file: Option<String> },
    UserAdjudication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecoveryPayloadV1 {
    pub version: u32,
    pub normalized_intent: CompletionIntent,
    pub source_audit_ref: CompletionSourceAuditRef,
    pub resolver_failure: ArtifactFailure,
    pub producer_scope_digest: String,
    pub producer_baseline_head: String,
    pub expected_resolver_kind: ArtifactKind,
    pub producer_task_id: String,
    pub producer_generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletionDecisionPayloadV1 {
    version: u32,
    reason_code: CompletionIntentReason,
    role: CompletionRole,
    legal_outcomes: Vec<CompletionOutcome>,
    bounded_candidates: Vec<CompletionCandidate>,
    diagnostics: Vec<CompletionDiagnostic>,
}

#[derive(Debug)]
struct LoadedTerminal {
    run: delegation_task_run::Model,
    binding: delegation_workflow_run_binding::Model,
    workflow: delegation_workflow::Model,
    node: delegation_workflow_node_binding::Model,
}

#[derive(Debug)]
struct LoadedToolIntent {
    intent_id: String,
    intent: CompletionToolIntent,
}

#[derive(Debug)]
enum RetryTxnOutcome {
    Resolved(Box<TerminalCompletionResult>),
    Superseded,
}

pub async fn materialize_terminal_completion_txn<C: ConnectionTrait>(
    conn: &C,
    input: TerminalCompletionInput,
) -> Result<TerminalCompletionResult, CompletionEvidenceError> {
    if input.terminal_status != DelegationRunStatus::Completed {
        return Err(CompletionEvidenceError::InvalidTerminalState(
            "only completed runs can materialize semantic completion".into(),
        ));
    }
    let loaded = load_terminal(conn, &input.task_id).await?;
    validate_v2_terminal(&loaded, &input)?;
    if let Some(existing) = existing_result(conn, &loaded).await? {
        return Ok(existing);
    }

    let context = rebuild_completion_context(conn, &loaded).await?;
    ensure_context_matches_binding(&context, &loaded.binding)?;
    let tool_intents = load_tool_intents(conn, &input.task_id).await?;
    let resolution = resolve_completion_intent(&CompletionResolverInput {
        role: context.scope_role.completion_role(),
        tool_intents: tool_intents
            .iter()
            .map(|loaded| loaded.intent.clone())
            .collect(),
        final_assistant_text: input.final_assistant_text,
        report_candidates: input
            .pre_read_reports
            .into_iter()
            .map(|report| CompletionReportCandidate {
                path: report.path,
                contents: report.contents,
                summary: report.summary,
            })
            .collect(),
        touched_report_candidates: Vec::new(),
        report_read_failures: Vec::new(),
    });

    match resolution {
        CompletionResolution::NeedsDecision {
            reason_code,
            bounded_candidates,
            diagnostics,
        } => {
            open_completion_decision(
                conn,
                &loaded,
                &context,
                reason_code,
                bounded_candidates,
                diagnostics,
            )
            .await
        }
        CompletionResolution::Resolved(intent) => {
            match resolve_terminal_artifact(conn, &loaded, &context, intent.outcome).await {
                Ok(artifact) => {
                    let evidence = persist_evidence_state(
                        conn,
                        &loaded,
                        &context,
                        intent,
                        artifact,
                        Utc::now(),
                    )
                    .await?;
                    let graph_revision =
                        bump_completion_graph(conn, &loaded, "completion_resolved").await?;
                    Ok(TerminalCompletionResult {
                        state: CompletionState::Resolved,
                        evidence: Some(evidence),
                        attention: None,
                        graph_revision,
                    })
                }
                Err(ArtifactError::Unavailable(failure)) => {
                    let source_audit_ref = source_audit_ref(&tool_intents, &intent);
                    open_artifact_recovery(
                        conn,
                        &loaded,
                        &context,
                        intent,
                        source_audit_ref,
                        failure,
                    )
                    .await
                }
                Err(error) => Err(error.into()),
            }
        }
    }
}

/// Execute a typed artifact retry as its own atomic transaction. Returning a
/// superseded error happens only after the stale attention resolution commits.
pub async fn retry_completion_artifact_txn(
    db: &AppDatabase,
    request: CompletionAttentionCas,
) -> Result<TerminalCompletionResult, CompletionEvidenceError> {
    let outcome = db
        .conn
        .transaction::<_, RetryTxnOutcome, CompletionEvidenceError>(|txn| {
            let request = request.clone();
            Box::pin(async move { retry_completion_artifact_once(txn, &request).await })
        })
        .await;
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(sea_orm::TransactionError::Connection(error)) => {
            return Err(CompletionEvidenceError::Persistence(error.to_string()))
        }
        Err(sea_orm::TransactionError::Transaction(error)) => return Err(error),
    };
    match outcome {
        RetryTxnOutcome::Resolved(result) => Ok(*result),
        RetryTxnOutcome::Superseded => Err(CompletionEvidenceError::DecisionSuperseded),
    }
}

async fn retry_completion_artifact_once<C: ConnectionTrait>(
    conn: &C,
    request: &CompletionAttentionCas,
) -> Result<RetryTxnOutcome, CompletionEvidenceError> {
    if request.kind != AttentionKind::CompletionArtifactRecovery {
        return Err(CompletionEvidenceError::InvalidAttention(
            "retry requires completion_artifact_recovery".into(),
        ));
    }
    let attention = delegation_attention_request::Entity::find_by_id(request.attention_id.clone())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| CompletionEvidenceError::InvalidAttention("attention not found".into()))?;
    if !attention_cas_fields_match(&attention, request) {
        return Err(CompletionEvidenceError::InvalidAttention(
            "attention CAS mismatch".into(),
        ));
    }
    if attention.status == "resolved" {
        return match attention.resolution_code.as_deref() {
            Some("artifact_resolved") => {
                let loaded = load_terminal(conn, &request.task_id).await?;
                existing_result(conn, &loaded)
                    .await?
                    .filter(|result| result.state == CompletionState::Resolved)
                    .map(|result| RetryTxnOutcome::Resolved(Box::new(result)))
                    .ok_or_else(|| {
                        CompletionEvidenceError::InvalidAttention(
                            "resolved artifact attention has no resolved evidence".into(),
                        )
                    })
            }
            Some("superseded") => Ok(RetryTxnOutcome::Superseded),
            _ => Err(CompletionEvidenceError::InvalidAttention(
                "artifact attention has an incompatible resolution".into(),
            )),
        };
    }
    validate_attention_cas(&attention, request)?;
    let payload_json = attention.payload_json.as_deref().ok_or_else(|| {
        CompletionEvidenceError::InvalidAttention("artifact payload is missing".into())
    })?;
    let payload: ArtifactRecoveryPayloadV1 = serde_json::from_str(payload_json)
        .map_err(|error| CompletionEvidenceError::InvalidAttention(error.to_string()))?;
    if payload.version != 1
        || payload.producer_task_id != request.task_id
        || payload.producer_scope_digest != request.captured_scope_digest
    {
        return Err(CompletionEvidenceError::InvalidAttention(
            "artifact payload does not match its CAS".into(),
        ));
    }

    let loaded = load_terminal(conn, &request.task_id).await?;
    let context = rebuild_completion_context(conn, &loaded).await;
    let current = context.as_ref().ok().and_then(|context| {
        ensure_context_matches_binding(context, &loaded.binding)
            .ok()
            .map(|_| context)
    });
    let current_matches = current.is_some_and(|context| {
        context.evidence_scope_digest == payload.producer_scope_digest
            && loaded.run.generation == payload.producer_generation
            && loaded
                .binding
                .producer_baseline_head
                .as_deref()
                .unwrap_or_default()
                == payload.producer_baseline_head
    });
    if !current_matches {
        resolve_attention_txn(conn, request, "superseded", None).await?;
        let _ = bump_completion_graph(conn, &loaded, "completion_decision_superseded").await?;
        return Ok(RetryTxnOutcome::Superseded);
    }
    let context = current.expect("checked current completion context");
    let artifact =
        resolve_terminal_artifact(conn, &loaded, context, payload.normalized_intent.outcome)
            .await?;
    if artifact.kind() != payload.expected_resolver_kind {
        return Err(CompletionEvidenceError::InvalidAttention(
            "resolved artifact kind changed".into(),
        ));
    }
    let evidence = persist_evidence_state(
        conn,
        &loaded,
        context,
        payload.normalized_intent,
        artifact.clone(),
        Utc::now(),
    )
    .await?;
    let resolution_json = serde_json::to_string(&json!({
        "version": 1,
        "code": "artifact_resolved",
        "resolver_kind": artifact.kind(),
        "artifact": completion_artifact(&artifact),
    }))
    .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    resolve_attention_txn(conn, request, "artifact_resolved", Some(resolution_json)).await?;
    let graph_revision =
        bump_completion_graph(conn, &loaded, "completion_artifact_resolved").await?;
    Ok(RetryTxnOutcome::Resolved(Box::new(
        TerminalCompletionResult {
            state: CompletionState::Resolved,
            evidence: Some(evidence),
            attention: None,
            graph_revision,
        },
    )))
}

async fn load_terminal<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<LoadedTerminal, CompletionEvidenceError> {
    let run = delegation_task_run::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| CompletionEvidenceError::InvalidTerminalState("run not found".into()))?;
    let binding = delegation_workflow_run_binding::Entity::find_by_id(task_id.to_string())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            CompletionEvidenceError::InvalidTerminalState("run is not workflow-bound".into())
        })?;
    let workflow = delegation_workflow::Entity::find_by_id(binding.workflow_id.clone())
        .one(conn)
        .await
        .map_err(db_error)?
        .ok_or_else(|| {
            CompletionEvidenceError::InvalidTerminalState("workflow not found".into())
        })?;
    let node = delegation_workflow_node_binding::Entity::find_by_id((
        binding.workflow_id.clone(),
        binding.node_id.clone(),
    ))
    .one(conn)
    .await
    .map_err(db_error)?
    .ok_or_else(|| CompletionEvidenceError::InvalidTerminalState("node not found".into()))?;
    Ok(LoadedTerminal {
        run,
        binding,
        workflow,
        node,
    })
}

fn validate_v2_terminal(
    loaded: &LoadedTerminal,
    input: &TerminalCompletionInput,
) -> Result<(), CompletionEvidenceError> {
    if loaded.run.task_id != input.task_id
        || loaded.run.status != DelegationRunStatus::Completed
        || loaded.workflow.completion_protocol_version != i64::from(COMPLETION_PROTOCOL_VERSION_V2)
        || loaded.workflow.completion_protocol_mode != CompletionProtocolMode::V2Enforce
    {
        return Err(CompletionEvidenceError::InvalidTerminalState(
            "run is not a completed v2-enforce workflow run".into(),
        ));
    }
    Ok(())
}

async fn rebuild_completion_context<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
) -> Result<AdmissionCompletionContextV2, CompletionEvidenceError> {
    let workspace = loaded
        .run
        .workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or(ArtifactError::Unavailable(
            ArtifactFailure::WorkspaceUnavailable,
        ))?;
    build_admission_completion_context(
        &WorkflowStore::new(conn, Path::new(workspace)),
        &AdmissionCandidate {
            workflow: &loaded.workflow,
            node: &loaded.node,
            task_id: &loaded.run.task_id,
            artifact_digest: loaded.binding.artifact_digest.as_deref(),
            reviewed_task_id: loaded.binding.reviewed_task_id.as_deref(),
            reviewed_generation: loaded.binding.reviewed_implementer_generation,
            producer_baseline_head: loaded.binding.producer_baseline_head.as_deref(),
        },
    )
    .await
    .map_err(Into::into)
}

fn ensure_context_matches_binding(
    context: &AdmissionCompletionContextV2,
    binding: &delegation_workflow_run_binding::Model,
) -> Result<(), CompletionEvidenceError> {
    let matches = binding.evidence_scope_digest.as_deref()
        == Some(context.evidence_scope_digest.as_str())
        && binding.gate_lineage == context.evidence_scope.gate_lineage
        && binding.review_round == context.evidence_scope.review_round.map(i64::from)
        && binding.instruction_block_digest.as_deref() == Some(context.instruction.digest.as_str())
        && binding.material_selector_digest == context.material_selector_digest
        && binding.subject_material_digest == context.subject_material_digest
        && binding.requirements_identity == context.requirements_identity
        && binding.task_specification_identity == context.task_specification_identity
        && binding.final_findings_identity == context.final_findings_identity;
    if matches {
        Ok(())
    } else {
        Err(super::evidence_scope::EvidenceScopeError::ScopeChanged.into())
    }
}

async fn load_tool_intents<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
) -> Result<Vec<LoadedToolIntent>, CompletionEvidenceError> {
    let rows = delegation_completion_tool_intent::Entity::find()
        .filter(delegation_completion_tool_intent::Column::TaskId.eq(task_id))
        .order_by_asc(delegation_completion_tool_intent::Column::AcceptedOrdinal)
        .all(conn)
        .await
        .map_err(db_error)?;
    rows.into_iter()
        .map(|row| {
            let outcome = parse_outcome(&row.outcome).ok_or_else(|| {
                CompletionEvidenceError::InvalidTerminalState(
                    "stored tool intent has an invalid outcome".into(),
                )
            })?;
            Ok(LoadedToolIntent {
                intent_id: row.intent_id,
                intent: CompletionToolIntent {
                    accepted_ordinal: row.accepted_ordinal,
                    outcome,
                    summary: row.summary,
                    report_file: row.report_hint,
                },
            })
        })
        .collect()
}

async fn resolve_terminal_artifact<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    outcome: CompletionOutcome,
) -> Result<ResolvedArtifact, ArtifactError> {
    let workspace = loaded
        .run
        .workspace_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or(ArtifactError::Unavailable(
            ArtifactFailure::WorkspaceUnavailable,
        ))?;
    match &context.evidence_scope.artifact_subject {
        ArtifactSubjectIdentityV2::DocumentSha256 { rel_path, .. }
        | ArtifactSubjectIdentityV2::PendingDocument { rel_path }
        | ArtifactSubjectIdentityV2::PlanMaterial {
            plan_rel_path: rel_path,
            ..
        } => resolve_document(Path::new(workspace), rel_path, MAX_DOCUMENT_ARTIFACT_BYTES)
            .await
            .map(Into::into),
        ArtifactSubjectIdentityV2::GitHeadV1 { digest } => {
            if matches!(
                context.scope_role,
                super::types::CompletionScopeRole::TaskImplementer
                    | super::types::CompletionScopeRole::FinalFixer
            ) {
                if !matches!(
                    outcome,
                    CompletionOutcome::Done | CompletionOutcome::DoneWithConcerns
                ) {
                    return Ok(GitHeadV1Artifact {
                        head: digest.clone(),
                    }
                    .into());
                }
                let allow_noop = context.scope_role
                    == super::types::CompletionScopeRole::TaskImplementer
                    && durable_task_allows_noop(conn, &loaded.workflow, loaded.node.task_index)
                        .await
                        .unwrap_or(false);
                resolve_producer_completion(Path::new(workspace), outcome, digest, allow_noop)
                    .await?
                    .ok_or(ArtifactError::Unavailable(ArtifactFailure::CommitRequired))
            } else {
                resolve_reviewer_completion(Path::new(workspace), digest).await
            }
        }
    }
}

async fn durable_task_allows_noop<C: ConnectionTrait>(
    conn: &C,
    workflow: &delegation_workflow::Model,
    task_index: Option<i64>,
) -> Result<bool, CompletionEvidenceError> {
    let Some(task_index) = task_index.and_then(|value| u32::try_from(value).ok()) else {
        return Ok(false);
    };
    let snapshot = super::store::load_active_manifest_snapshot(conn, workflow)
        .await
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    Ok(snapshot
        .normalized
        .task_policies
        .iter()
        .find(|policy| policy.task_index == task_index)
        .is_some_and(|policy| policy.allow_noop_verification))
}

async fn persist_evidence_state<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    intent: CompletionIntent,
    artifact: ResolvedArtifact,
    captured_at: chrono::DateTime<Utc>,
) -> Result<CompletionEvidenceV2, CompletionEvidenceError> {
    let artifact = completion_artifact(&artifact);
    let binding_projection = completion_binding(loaded, context)?;
    let evidence = CompletionEvidenceV2 {
        version: COMPLETION_PROTOCOL_VERSION_V2,
        intent,
        binding: binding_projection.clone(),
        artifact: artifact.clone(),
        review_scope_digest: context.review_scope_digest.clone(),
        evidence_scope_digest: context.evidence_scope_digest.clone(),
        captured_at: captured_at.to_rfc3339_opts(SecondsFormat::Millis, true),
    };
    let evidence_json = serde_json::to_string(&evidence)
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    let validated = validate_completion_evidence(
        &evidence_json,
        &EvidenceValidationContext {
            role: context.scope_role.completion_role(),
            binding: binding_projection,
            artifact,
            scope: context.evidence_scope.clone(),
        },
    )?;
    if !validated.evidence_validated || validated.evidence != evidence {
        return Err(CompletionEvidenceError::InvalidTerminalState(
            "evidence round trip changed the platform value".into(),
        ));
    }

    let mut run: delegation_task_run::ActiveModel = loaded.run.clone().into();
    run.completion_state = Set(Some(CompletionState::Resolved));
    run.completion_outcome = Set(Some(evidence.intent.outcome.as_str().into()));
    run.completion_evidence_json = Set(Some(evidence_json));
    run.card_summary_json = Set(None);
    run.updated_at = Set(captured_at);
    run.update(conn).await.map_err(db_error)?;
    update_binding_projection(conn, loaded, context, Some(evidence.artifact.digest())).await?;
    Ok(evidence)
}

async fn open_completion_decision<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    reason_code: CompletionIntentReason,
    bounded_candidates: Vec<CompletionCandidate>,
    diagnostics: Vec<CompletionDiagnostic>,
) -> Result<TerminalCompletionResult, CompletionEvidenceError> {
    let payload = CompletionDecisionPayloadV1 {
        version: 1,
        reason_code,
        role: context.scope_role.completion_role(),
        legal_outcomes: legal_outcomes(context.scope_role.completion_role()),
        bounded_candidates,
        diagnostics,
    };
    let payload_json = serialize_completion_decision_payload(payload)?;
    let attention = open_attention(
        conn,
        loaded,
        AttentionKind::CompletionDecision,
        COMPLETION_DECISION_MESSAGE,
        &payload_json,
        &context.evidence_scope_digest,
    )
    .await?;
    persist_unresolved_state(conn, loaded, context, CompletionState::NeedsDecision).await?;
    let graph_revision = bump_completion_graph(conn, loaded, "completion_decision_opened").await?;
    Ok(TerminalCompletionResult {
        state: CompletionState::NeedsDecision,
        evidence: None,
        attention: Some(attention),
        graph_revision,
    })
}

fn serialize_completion_decision_payload(
    mut payload: CompletionDecisionPayloadV1,
) -> Result<String, CompletionEvidenceError> {
    loop {
        let json = serde_json::to_string(&payload)
            .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
        if json.len() <= ATTENTION_PAYLOAD_MAX_BYTES {
            return Ok(json);
        }
        if payload.diagnostics.pop().is_some() {
            continue;
        }
        if payload.bounded_candidates.len() > 2 {
            let penultimate = payload.bounded_candidates.len() - 2;
            payload.bounded_candidates.remove(penultimate);
            continue;
        }
        return Err(CompletionEvidenceError::Persistence(
            "minimum completion decision payload exceeds attention budget".into(),
        ));
    }
}

async fn open_artifact_recovery<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    intent: CompletionIntent,
    source_audit_ref: CompletionSourceAuditRef,
    failure: ArtifactFailure,
) -> Result<TerminalCompletionResult, CompletionEvidenceError> {
    let expected_resolver_kind = match context.evidence_scope.artifact_subject {
        ArtifactSubjectIdentityV2::GitHeadV1 { .. } => ArtifactKind::GitHeadV1,
        _ => ArtifactKind::DocumentSha256,
    };
    let payload = ArtifactRecoveryPayloadV1 {
        version: 1,
        normalized_intent: intent,
        source_audit_ref,
        resolver_failure: failure,
        producer_scope_digest: context.evidence_scope_digest.clone(),
        producer_baseline_head: loaded
            .binding
            .producer_baseline_head
            .clone()
            .unwrap_or_default(),
        expected_resolver_kind,
        producer_task_id: loaded.run.task_id.clone(),
        producer_generation: loaded.run.generation,
    };
    let payload_json = serde_json::to_string(&payload)
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    let attention = open_attention(
        conn,
        loaded,
        AttentionKind::CompletionArtifactRecovery,
        ARTIFACT_RECOVERY_MESSAGE,
        &payload_json,
        &payload.producer_scope_digest,
    )
    .await?;
    persist_unresolved_state(conn, loaded, context, CompletionState::ArtifactRecovery).await?;
    let graph_revision =
        bump_completion_graph(conn, loaded, "completion_artifact_recovery_opened").await?;
    Ok(TerminalCompletionResult {
        state: CompletionState::ArtifactRecovery,
        evidence: None,
        attention: Some(attention),
        graph_revision,
    })
}

async fn persist_unresolved_state<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    state: CompletionState,
) -> Result<(), CompletionEvidenceError> {
    let mut run: delegation_task_run::ActiveModel = loaded.run.clone().into();
    run.completion_state = Set(Some(state));
    run.completion_outcome = Set(None);
    run.completion_evidence_json = Set(None);
    run.card_summary_json = Set(None);
    run.updated_at = Set(Utc::now());
    run.update(conn).await.map_err(db_error)?;
    update_binding_projection(conn, loaded, context, None).await
}

async fn update_binding_projection<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
    artifact_digest: Option<&str>,
) -> Result<(), CompletionEvidenceError> {
    let mut binding: delegation_workflow_run_binding::ActiveModel = loaded.binding.clone().into();
    binding.evidence_scope_digest = Set(Some(context.evidence_scope_digest.clone()));
    binding.gate_lineage = Set(context.evidence_scope.gate_lineage.clone());
    binding.review_round = Set(context.evidence_scope.review_round.map(i64::from));
    binding.instruction_block_digest = Set(Some(context.instruction.digest.clone()));
    binding.material_selector_digest = Set(context.material_selector_digest.clone());
    binding.subject_material_digest = Set(context.subject_material_digest.clone());
    binding.requirements_identity = Set(context.requirements_identity.clone());
    binding.task_specification_identity = Set(context.task_specification_identity.clone());
    binding.final_findings_identity = Set(context.final_findings_identity.clone());
    if let Some(digest) = artifact_digest {
        binding.artifact_digest = Set(Some(digest.to_string()));
    }
    binding.updated_at = Set(Utc::now());
    binding.update(conn).await.map_err(db_error)?;
    Ok(())
}

async fn open_attention<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    kind: AttentionKind,
    message: &'static str,
    payload_json: &str,
    captured_scope_digest: &str,
) -> Result<CompletionAttentionCas, CompletionEvidenceError> {
    let row = open_terminal_completion_attention_txn(
        conn,
        &TerminalCompletionAttentionInput {
            task_id: &loaded.run.task_id,
            kind,
            message,
            payload_json,
            captured_scope_digest,
            node_id: &loaded.node.node_id,
            created_at: Utc::now(),
        },
    )
    .await
    .map_err(|error| CompletionEvidenceError::InvalidAttention(error.to_string()))?;
    attention_cas(&row)
}

async fn existing_result<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
) -> Result<Option<TerminalCompletionResult>, CompletionEvidenceError> {
    let Some(state) = loaded.run.completion_state.clone() else {
        return Ok(None);
    };
    let graph_revision = u64::try_from(loaded.workflow.graph_revision).map_err(|_| {
        CompletionEvidenceError::InvalidTerminalState("negative graph revision".into())
    })?;
    match state {
        CompletionState::Resolved => {
            let evidence = loaded
                .run
                .completion_evidence_json
                .as_deref()
                .ok_or_else(|| {
                    CompletionEvidenceError::InvalidTerminalState(
                        "resolved run is missing evidence".into(),
                    )
                })
                .and_then(|json| {
                    serde_json::from_str(json).map_err(|error| {
                        CompletionEvidenceError::InvalidTerminalState(error.to_string())
                    })
                })?;
            Ok(Some(TerminalCompletionResult {
                state,
                evidence: Some(evidence),
                attention: None,
                graph_revision,
            }))
        }
        CompletionState::NeedsDecision | CompletionState::ArtifactRecovery => {
            let kind = if state == CompletionState::NeedsDecision {
                AttentionKind::CompletionDecision
            } else {
                AttentionKind::CompletionArtifactRecovery
            };
            let row = delegation_attention_request::Entity::find()
                .filter(delegation_attention_request::Column::TaskId.eq(&loaded.run.task_id))
                .filter(delegation_attention_request::Column::Kind.eq(kind))
                .filter(delegation_attention_request::Column::Status.eq("open"))
                .one(conn)
                .await
                .map_err(db_error)?
                .ok_or_else(|| {
                    CompletionEvidenceError::InvalidAttention(
                        "unresolved completion has no open attention".into(),
                    )
                })?;
            Ok(Some(TerminalCompletionResult {
                state,
                evidence: None,
                attention: Some(attention_cas(&row)?),
                graph_revision,
            }))
        }
    }
}

fn completion_binding(
    loaded: &LoadedTerminal,
    context: &AdmissionCompletionContextV2,
) -> Result<CompletionEvidenceBindingV2, CompletionEvidenceError> {
    Ok(CompletionEvidenceBindingV2 {
        workflow_id: loaded.workflow.workflow_id.clone(),
        task_id: loaded.run.task_id.clone(),
        node_id: loaded.node.node_id.clone(),
        role: context.scope_role.completion_role(),
        phase_id: loaded.node.phase_id.clone(),
        task_index: loaded
            .node
            .task_index
            .map(u32::try_from)
            .transpose()
            .map_err(|_| {
                CompletionEvidenceError::InvalidTerminalState("invalid task index".into())
            })?,
        gate_id: context.evidence_scope.gate_id.clone(),
        gate_lineage: context.evidence_scope.gate_lineage.clone(),
        review_round: context.evidence_scope.review_round,
        reviewed_task_id: loaded.binding.reviewed_task_id.clone(),
        reviewed_generation: loaded.binding.reviewed_implementer_generation,
        manifest_revision_observed: u64::try_from(loaded.binding.manifest_revision).map_err(
            |_| CompletionEvidenceError::InvalidTerminalState("invalid manifest revision".into()),
        )?,
    })
}

fn completion_artifact(artifact: &ResolvedArtifact) -> CompletionArtifactV2 {
    match artifact {
        ResolvedArtifact::DocumentSha256(document) => CompletionArtifactV2::DocumentSha256 {
            rel_path: document.rel_path().to_string(),
            digest: document.digest().to_string(),
        },
        ResolvedArtifact::GitHeadV1(git) => CompletionArtifactV2::GitHeadV1 {
            head: git.head.clone(),
        },
    }
}

fn source_audit_ref(
    tool_intents: &[LoadedToolIntent],
    intent: &CompletionIntent,
) -> CompletionSourceAuditRef {
    match intent.source {
        CompletionIntentSource::CompleteWork => tool_intents
            .iter()
            .max_by_key(|loaded| loaded.intent.accepted_ordinal)
            .map_or(CompletionSourceAuditRef::AssistantConclusion, |loaded| {
                CompletionSourceAuditRef::ToolIntent {
                    intent_id: loaded.intent_id.clone(),
                }
            }),
        CompletionIntentSource::AssistantConclusion => {
            CompletionSourceAuditRef::AssistantConclusion
        }
        CompletionIntentSource::Report => CompletionSourceAuditRef::Report {
            report_file: intent.report_file.clone(),
        },
        CompletionIntentSource::UserAdjudication => CompletionSourceAuditRef::UserAdjudication,
    }
}

fn legal_outcomes(role: CompletionRole) -> Vec<CompletionOutcome> {
    match role {
        CompletionRole::Reviewer => vec![
            CompletionOutcome::Approve,
            CompletionOutcome::ApproveWithMinors,
            CompletionOutcome::RequestChanges,
            CompletionOutcome::Block,
        ],
        CompletionRole::Author | CompletionRole::Implementer | CompletionRole::Fixer => vec![
            CompletionOutcome::Done,
            CompletionOutcome::DoneWithConcerns,
            CompletionOutcome::Blocked,
        ],
    }
}

fn parse_outcome(value: &str) -> Option<CompletionOutcome> {
    Some(match value {
        "approve" => CompletionOutcome::Approve,
        "approve_with_minors" => CompletionOutcome::ApproveWithMinors,
        "request_changes" => CompletionOutcome::RequestChanges,
        "block" => CompletionOutcome::Block,
        "done" => CompletionOutcome::Done,
        "done_with_concerns" => CompletionOutcome::DoneWithConcerns,
        "blocked" => CompletionOutcome::Blocked,
        _ => return None,
    })
}

fn attention_cas(
    row: &delegation_attention_request::Model,
) -> Result<CompletionAttentionCas, CompletionEvidenceError> {
    Ok(CompletionAttentionCas {
        attention_id: row.request_id.clone(),
        task_id: row.task_id.clone(),
        kind: row.kind.clone(),
        captured_scope_digest: row.captured_scope_digest.clone().ok_or_else(|| {
            CompletionEvidenceError::InvalidAttention("captured scope is missing".into())
        })?,
        latest_run_id: row.latest_run_id.clone().ok_or_else(|| {
            CompletionEvidenceError::InvalidAttention("latest run is missing".into())
        })?,
        node_id: row
            .node_id
            .clone()
            .ok_or_else(|| CompletionEvidenceError::InvalidAttention("node is missing".into()))?,
    })
}

fn validate_attention_cas(
    row: &delegation_attention_request::Model,
    request: &CompletionAttentionCas,
) -> Result<(), CompletionEvidenceError> {
    if row.status == "open" && attention_cas_fields_match(row, request) {
        Ok(())
    } else {
        Err(CompletionEvidenceError::InvalidAttention(
            "attention CAS mismatch".into(),
        ))
    }
}

fn attention_cas_fields_match(
    row: &delegation_attention_request::Model,
    request: &CompletionAttentionCas,
) -> bool {
    row.request_id == request.attention_id
        && row.task_id == request.task_id
        && row.kind == request.kind
        && row.captured_scope_digest.as_deref() == Some(&request.captured_scope_digest)
        && row.latest_run_id.as_deref() == Some(&request.latest_run_id)
        && row.node_id.as_deref() == Some(&request.node_id)
}

async fn resolve_attention_txn<C: ConnectionTrait>(
    conn: &C,
    request: &CompletionAttentionCas,
    code: &str,
    resolution_json: Option<String>,
) -> Result<(), CompletionEvidenceError> {
    let result = delegation_attention_request::Entity::update_many()
        .col_expr(
            delegation_attention_request::Column::Status,
            sea_orm::sea_query::Expr::value("resolved"),
        )
        .col_expr(
            delegation_attention_request::Column::ResolutionCode,
            sea_orm::sea_query::Expr::value(Some(code.to_string())),
        )
        .col_expr(
            delegation_attention_request::Column::ResolutionJson,
            sea_orm::sea_query::Expr::value(resolution_json),
        )
        .col_expr(
            delegation_attention_request::Column::ResolvedAt,
            sea_orm::sea_query::Expr::value(Some(Utc::now())),
        )
        .filter(delegation_attention_request::Column::RequestId.eq(&request.attention_id))
        .filter(delegation_attention_request::Column::TaskId.eq(&request.task_id))
        .filter(delegation_attention_request::Column::Kind.eq(request.kind.clone()))
        .filter(
            delegation_attention_request::Column::CapturedScopeDigest
                .eq(&request.captured_scope_digest),
        )
        .filter(delegation_attention_request::Column::LatestRunId.eq(&request.latest_run_id))
        .filter(delegation_attention_request::Column::NodeId.eq(&request.node_id))
        .filter(delegation_attention_request::Column::Status.eq("open"))
        .exec(conn)
        .await
        .map_err(db_error)?;
    if result.rows_affected == 1 {
        Ok(())
    } else {
        Err(CompletionEvidenceError::InvalidAttention(
            "attention CAS lost".into(),
        ))
    }
}

async fn bump_completion_graph<C: ConnectionTrait>(
    conn: &C,
    loaded: &LoadedTerminal,
    event_kind: &str,
) -> Result<u64, CompletionEvidenceError> {
    let graph_revision = bump_graph_revision(conn, &loaded.workflow.workflow_id, Utc::now())
        .await
        .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    let payload_json = serde_json::to_string(&json!({
        "workflow_id": loaded.workflow.workflow_id,
        "task_id": loaded.run.task_id,
        "node_id": loaded.node.node_id,
        "graph_revision": graph_revision,
        "event_kind": event_kind,
    }))
    .map_err(|error| CompletionEvidenceError::Persistence(error.to_string()))?;
    delegation_workflow_outbox_event::ActiveModel {
        event_id: Set(uuid::Uuid::new_v4().to_string()),
        workflow_id: Set(loaded.workflow.workflow_id.clone()),
        graph_revision: Set(i64::try_from(graph_revision).map_err(|_| {
            CompletionEvidenceError::Persistence("graph revision exceeds i64".into())
        })?),
        event_kind: Set(event_kind.to_string()),
        subject_key: Set(loaded.run.task_id.clone()),
        payload_json: Set(payload_json),
        dispatch_attempts: Set(0),
        created_at: Set(Utc::now()),
        delivered_at: Set(None),
    }
    .insert(conn)
    .await
    .map_err(db_error)?;
    Ok(graph_revision)
}

fn db_error(error: sea_orm::DbErr) -> CompletionEvidenceError {
    CompletionEvidenceError::Persistence(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    use super::{
        materialize_terminal_completion_txn, retry_completion_artifact_txn,
        CompletionDecisionPayloadV1, TerminalCompletionInput, ValidatedReportCandidate,
    };
    use crate::acp::delegation::run_store::{ReservingRunInsert, RunStore};
    use crate::acp::delegation::store::{Settlement, TerminalTaskWrite};
    use crate::acp::delegation::workflow::store::{
        publish_workflow_manifest_core, PublishWorkflowRequest,
    };
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestDocument, ManifestGate, ManifestNode,
        ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState, ResolutionMode,
        WorkUnitKeyParts, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN,
        PHASE_TASKS, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use crate::acp::delegation::workflow::{
        build_work_unit_key, resolve_document, CompletionIntentReason, CompletionIntentSource,
        CompletionOutcome,
    };
    use crate::db::entities::conversation::ConversationStatus;
    use crate::db::entities::delegation_attention_request::{self, AttentionKind};
    use crate::db::entities::delegation_completion_tool_intent;
    use crate::db::entities::delegation_task_run::{
        self, AdmissionClass, CompletionState, DelegationRunStatus,
    };
    use crate::db::entities::delegation_workflow::{self, CompletionProtocolMode};
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::AgentType;
    use crate::web::event_bridge::EventEmitter;

    const DESIGN_REL_PATH: &str = "docs/superpowers/specs/task-10-design.md";
    const PLAN_REL_PATH: &str = "docs/superpowers/plans/task-10-plan.md";
    const DESIGN_BYTES: &[u8] = b"# Design\n\nTask 10 fixture.\n";

    #[derive(Clone, Copy)]
    enum IntentFixture {
        Tool,
        AssistantText,
        Report,
        Missing,
    }

    struct TerminalFixture {
        db: Arc<AppDatabase>,
        _workspace: TempDir,
        workspace_path: std::path::PathBuf,
        task_id: String,
        workflow_id: String,
        input: TerminalCompletionInput,
    }

    impl TerminalFixture {
        async fn new(source: IntentFixture, write_plan: bool) -> Self {
            let workspace = tempfile::tempdir().expect("workspace");
            let workspace_path = workspace.path().to_path_buf();
            let design_path = workspace_path.join(DESIGN_REL_PATH);
            std::fs::create_dir_all(design_path.parent().unwrap()).unwrap();
            std::fs::write(&design_path, DESIGN_BYTES).unwrap();
            if write_plan {
                let plan_path = workspace_path.join(PLAN_REL_PATH);
                std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
                std::fs::write(&plan_path, b"# Plan\n\nInitial.\n").unwrap();
            }

            let db = Arc::new(fresh_in_memory_db().await);
            let folder = seed_folder(&db, workspace_path.to_str().unwrap()).await;
            let parent = seed_conversation(&db, folder, AgentType::Codex).await;
            let child = seed_conversation(&db, folder, AgentType::Codex).await;
            let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
                rel_plan_path: PLAN_REL_PATH,
                agent_type: "codex",
                profile_id: None,
            })
            .unwrap();
            let document = skeleton_document("task-10", &author_key);
            let published = publish_workflow_manifest_core(
                &db,
                &EventEmitter::Noop,
                parent,
                PublishWorkflowRequest { document },
            )
            .await
            .expect("publish workflow");
            let workflow_id = published.workflow_id;
            let header = delegation_workflow::Entity::find_by_id(workflow_id.clone())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut active: delegation_workflow::ActiveModel = header.into();
            active.completion_protocol_version = Set(2);
            active.completion_protocol_mode = Set(CompletionProtocolMode::V2Enforce);
            active.update(&db.conn).await.unwrap();

            let task_id = format!("task-10-{}", uuid::Uuid::new_v4());
            let runs = RunStore::new(db.clone());
            runs.admit_gen1_reserving(ReservingRunInsert {
                task_id: task_id.clone(),
                root_task_id: task_id.clone(),
                previous_task_id: None,
                generation: 1,
                parent_conversation_id: parent,
                parent_tool_use_id: Some(format!("tool-{task_id}")),
                child_conversation_id: child,
                agent_type: "codex".into(),
                profile_id: None,
                workspace_path: Some(workspace_path.to_string_lossy().into_owned()),
                route_fingerprint: Some("task-10-route".into()),
                launch_snapshot_version: Some("v1".into()),
                mode_id: None,
                config_values_json: Some("{}".into()),
                task_preview: Some("Task 10".into()),
                request_fingerprint: Some(format!("fp-{task_id}")),
                admission_class: AdmissionClass::NormalRevision,
                lineage_root_task_id: task_id.clone(),
                work_unit_key: Some(author_key),
                history_only: false,
                replaced_task_id: None,
                replacement_reason: None,
                started_at: Some(Utc::now()),
            })
            .await
            .expect("admit Plan Author");

            let run = delegation_task_run::Entity::find_by_id(task_id.clone())
                .one(&db.conn)
                .await
                .unwrap()
                .unwrap();
            let mut active: delegation_task_run::ActiveModel = run.into();
            active.status = Set(DelegationRunStatus::Completed);
            active.finished_at = Set(Some(Utc::now()));
            active.card_summary_json = Set(Some(
                r#"{"kind":"author","status":"done","plan_digest":"model"}"#.into(),
            ));
            active.update(&db.conn).await.unwrap();

            if matches!(source, IntentFixture::Tool) {
                delegation_completion_tool_intent::ActiveModel {
                    intent_id: Set(format!("intent-{task_id}")),
                    task_id: Set(task_id.clone()),
                    child_tool_call_id: Set(format!("call-{task_id}")),
                    accepted_ordinal: Set(1),
                    outcome: Set(CompletionOutcome::Done.as_str().into()),
                    summary: Set(Some("tool summary".into())),
                    report_hint: Set(None),
                    request_digest: Set("digest".into()),
                    created_at: Set(Utc::now()),
                }
                .insert(&db.conn)
                .await
                .unwrap();
            }

            let (final_assistant_text, pre_read_reports) = match source {
                IntentFixture::Tool => ("tool selected".into(), Vec::new()),
                IntentFixture::AssistantText => {
                    ("Conclusion: done\n\nassistant summary".into(), Vec::new())
                }
                IntentFixture::Report => (
                    "See the report.".into(),
                    vec![ValidatedReportCandidate {
                        path: "reports/task-10.md".into(),
                        contents: "# Conclusion\n\ndone\n".into(),
                        summary: Some("report summary".into()),
                    }],
                ),
                IntentFixture::Missing => ("No explicit conclusion.".into(), Vec::new()),
            };
            let input = TerminalCompletionInput {
                task_id: task_id.clone(),
                terminal_status: DelegationRunStatus::Completed,
                final_assistant_text,
                pre_read_reports,
                pre_read_artifact: None,
            };
            Self {
                db,
                _workspace: workspace,
                workspace_path,
                task_id,
                workflow_id,
                input,
            }
        }

        async fn materialize(&self) -> super::TerminalCompletionResult {
            let txn = self.db.conn.begin().await.unwrap();
            let result = materialize_terminal_completion_txn(&txn, self.input.clone())
                .await
                .unwrap();
            txn.commit().await.unwrap();
            result
        }

        async fn stored_run(&self) -> delegation_task_run::Model {
            delegation_task_run::Entity::find_by_id(self.task_id.clone())
                .one(&self.db.conn)
                .await
                .unwrap()
                .unwrap()
        }
    }

    fn skeleton_document(token: &str, author_key: &str) -> ManifestDocument {
        let design_key = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: DESIGN_REL_PATH,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
            plan_target_rel_path: PLAN_REL_PATH.into(),
            risk_policy_version: "b2d_task_risk_v1".into(),
            workflow_id: None,
            expected_manifest_revision: None,
            publication_token: token.into(),
            workflow_state: ManifestWorkflowState::Skeleton,
            design: Some(DocumentRef {
                rel_path: DESIGN_REL_PATH.into(),
                digest: format!("sha256:{:x}", Sha256::digest(DESIGN_BYTES)),
            }),
            plan: None,
            phases: [PHASE_DESIGN, PHASE_PLAN, PHASE_TASKS, PHASE_FINAL]
                .into_iter()
                .map(|id| ManifestPhase {
                    id: id.into(),
                    kind: Some(id.into()),
                    title: None,
                })
                .collect(),
            nodes: vec![
                ManifestNode {
                    id: "design-reviewer".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some(PHASE_DESIGN.into()),
                    role: Some(ManifestNodeRole::Reviewer),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(design_key),
                    deps: Vec::new(),
                    required: Some(true),
                    node_outcome: None,
                    title: None,
                },
                ManifestNode {
                    id: "plan-author".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some(PHASE_PLAN.into()),
                    role: Some(ManifestNodeRole::Author),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(author_key.into()),
                    deps: Vec::new(),
                    required: Some(true),
                    node_outcome: None,
                    title: None,
                },
            ],
            edges: Vec::new(),
            gates: vec![ManifestGate {
                id: "design".into(),
                reviewer_cohort_node_ids: vec!["design-reviewer".into()],
                required_reviewer_node_ids: vec!["design-reviewer".into()],
                resolution_mode: ResolutionMode::ParentAdjudication,
                gate_kind: Some(DocumentGateKind::Design),
            }],
            task_policies: Vec::new(),
        }
    }

    #[tokio::test]
    async fn terminal_materialization_persists_identical_platform_evidence_for_each_channel() {
        let mut digests = Vec::new();
        for (source, expected) in [
            (IntentFixture::Tool, CompletionIntentSource::CompleteWork),
            (
                IntentFixture::AssistantText,
                CompletionIntentSource::AssistantConclusion,
            ),
            (IntentFixture::Report, CompletionIntentSource::Report),
        ] {
            let fixture = TerminalFixture::new(source, true).await;
            let result = fixture.materialize().await;
            assert_eq!(result.state, CompletionState::Resolved);
            let evidence = result.evidence.unwrap();
            assert_eq!(evidence.binding.workflow_id, fixture.workflow_id);
            assert_eq!(evidence.binding.task_id, fixture.task_id);
            assert_eq!(evidence.intent.source, expected);
            digests.push(evidence.artifact.digest().to_string());
            assert_eq!(fixture.stored_run().await.card_summary_json, None);
        }
        assert!(digests.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[tokio::test]
    async fn terminal_materialization_rehashes_documents_inside_the_write_critical_section() {
        let mut fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let pre_read = resolve_document(&fixture.workspace_path, PLAN_REL_PATH, 2 * 1024 * 1024)
            .await
            .unwrap()
            .into();
        fixture.input.pre_read_artifact = Some(pre_read);
        std::fs::write(
            fixture.workspace_path.join(PLAN_REL_PATH),
            b"# Plan\n\nChanged after pre-read.\n",
        )
        .unwrap();
        let current = resolve_document(&fixture.workspace_path, PLAN_REL_PATH, 2 * 1024 * 1024)
            .await
            .unwrap();
        let result = fixture.materialize().await;
        assert_eq!(result.evidence.unwrap().artifact.digest(), current.digest());
    }

    #[tokio::test]
    async fn unclear_completion_opens_one_terminal_decision_without_child_continuation() {
        let fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let first = fixture.materialize().await;
        let replay = fixture.materialize().await;
        assert_eq!(first.state, CompletionState::NeedsDecision);
        assert_eq!(first.attention, replay.attention);
        let count = delegation_attention_request::Entity::find()
            .all(&fixture.db.conn)
            .await
            .unwrap()
            .into_iter()
            .filter(|row| row.status == "open" && row.kind == AttentionKind::CompletionDecision)
            .count();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn maximum_bounded_ambiguity_commits_terminal_and_one_attention() {
        use crate::acp::delegation::attention::ATTENTION_PAYLOAD_MAX_BYTES;

        let mut fixture = TerminalFixture::new(IntentFixture::Missing, true).await;
        let run = fixture.stored_run().await;
        let mut active: delegation_task_run::ActiveModel = run.into();
        active.status = Set(DelegationRunStatus::Running);
        active.finished_at = Set(None);
        active.completion_state = Set(None);
        active.completion_outcome = Set(None);
        active.completion_evidence_json = Set(None);
        active.update(&fixture.db.conn).await.unwrap();

        fixture.input.final_assistant_text.clear();
        fixture.input.pre_read_reports = (0..8)
            .map(|index| {
                let suffix = format!("{index}.md");
                let prefix = "reports/";
                let path = format!(
                    "{prefix}{}{suffix}",
                    "x".repeat(1024 - prefix.len() - suffix.len())
                );
                assert_eq!(path.len(), 1024);
                ValidatedReportCandidate {
                    path,
                    contents: "# Conclusion\n\napprove\n".into(),
                    summary: None,
                }
            })
            .collect();
        let first_path = fixture.input.pre_read_reports.first().unwrap().path.clone();
        let last_path = fixture.input.pre_read_reports.last().unwrap().path.clone();

        let runs = RunStore::new(fixture.db.clone());
        let (settlement, completion) = runs
            .settle_terminal_with_completion(
                &fixture.task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview),
                Some(fixture.input.clone()),
            )
            .await
            .expect("bounded ambiguity must not roll back terminal settlement");
        assert!(matches!(settlement, Settlement::Won(_)));
        assert_eq!(completion.unwrap().state, CompletionState::NeedsDecision);
        assert_eq!(
            fixture.stored_run().await.status,
            DelegationRunStatus::Completed
        );

        let attentions = delegation_attention_request::Entity::find()
            .filter(delegation_attention_request::Column::TaskId.eq(&fixture.task_id))
            .all(&fixture.db.conn)
            .await
            .unwrap();
        assert_eq!(attentions.len(), 1);
        let payload = attentions[0].payload_json.as_deref().unwrap();
        assert!(payload.len() <= ATTENTION_PAYLOAD_MAX_BYTES);
        let payload: CompletionDecisionPayloadV1 = serde_json::from_str(payload).unwrap();
        assert_eq!(payload.reason_code, CompletionIntentReason::RoleMismatch);
        assert!(!payload.bounded_candidates.is_empty());
        assert_eq!(
            payload
                .bounded_candidates
                .first()
                .unwrap()
                .report_file
                .as_deref(),
            Some(first_path.as_str())
        );
        assert_eq!(
            payload
                .bounded_candidates
                .last()
                .unwrap()
                .report_file
                .as_deref(),
            Some(last_path.as_str())
        );
    }

    #[tokio::test]
    async fn artifact_retry_reuses_persisted_intent_and_supersedes_only_scope_changes() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, false).await;
        let opened = fixture.materialize().await;
        assert_eq!(opened.state, CompletionState::ArtifactRecovery);
        let cas = opened.attention.unwrap();
        let row = delegation_attention_request::Entity::find_by_id(cas.attention_id.clone())
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let payload: super::ArtifactRecoveryPayloadV1 =
            serde_json::from_str(row.payload_json.as_deref().unwrap()).unwrap();
        assert_eq!(payload.normalized_intent.outcome, CompletionOutcome::Done);
        assert_eq!(
            payload.normalized_intent.source,
            CompletionIntentSource::AssistantConclusion
        );

        let plan_path = fixture.workspace_path.join(PLAN_REL_PATH);
        std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        std::fs::write(&plan_path, b"# Plan\n\nRecovered.\n").unwrap();
        let resolved = retry_completion_artifact_txn(&fixture.db, cas.clone())
            .await
            .unwrap();
        assert_eq!(resolved.state, CompletionState::Resolved);
        assert_eq!(
            resolved.evidence.unwrap().intent.source,
            CompletionIntentSource::AssistantConclusion
        );
        let replay = retry_completion_artifact_txn(&fixture.db, cas.clone())
            .await
            .unwrap();
        assert_eq!(replay.state, CompletionState::Resolved);
        assert_eq!(
            replay.evidence.unwrap().intent.source,
            CompletionIntentSource::AssistantConclusion
        );
        let row = delegation_attention_request::Entity::find_by_id(cas.attention_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.resolution_code.as_deref(), Some("artifact_resolved"));

        let stale = TerminalFixture::new(IntentFixture::Tool, false).await;
        let opened = stale.materialize().await;
        let cas = opened.attention.unwrap();
        let binding = crate::db::entities::delegation_workflow_run_binding::Entity::find_by_id(
            stale.task_id.clone(),
        )
        .one(&stale.db.conn)
        .await
        .unwrap()
        .unwrap();
        let mut active: crate::db::entities::delegation_workflow_run_binding::ActiveModel =
            binding.into();
        active.instruction_block_digest = Set(Some(format!("sha256:{}", "f".repeat(64))));
        active.update(&stale.db.conn).await.unwrap();
        let error = retry_completion_artifact_txn(&stale.db, cas)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "completion_decision_superseded");
    }

    #[tokio::test]
    async fn artifact_recovery_preserves_report_source_audit_reference() {
        let fixture = TerminalFixture::new(IntentFixture::Report, false).await;
        let opened = fixture.materialize().await;
        assert_eq!(opened.state, CompletionState::ArtifactRecovery);
        let cas = opened.attention.unwrap();
        let row = delegation_attention_request::Entity::find_by_id(cas.attention_id)
            .one(&fixture.db.conn)
            .await
            .unwrap()
            .unwrap();
        let payload: super::ArtifactRecoveryPayloadV1 =
            serde_json::from_str(row.payload_json.as_deref().unwrap()).unwrap();
        assert_eq!(
            payload.source_audit_ref,
            super::CompletionSourceAuditRef::Report {
                report_file: Some("reports/task-10.md".into()),
            }
        );
    }

    #[tokio::test]
    async fn run_store_settlement_commits_v2_evidence_with_terminal_state_atomically() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let run = fixture.stored_run().await;
        let mut active: delegation_task_run::ActiveModel = run.into();
        active.status = Set(DelegationRunStatus::Running);
        active.finished_at = Set(None);
        active.completion_state = Set(None);
        active.completion_outcome = Set(None);
        active.completion_evidence_json = Set(None);
        active.update(&fixture.db.conn).await.unwrap();

        let runs = RunStore::new(fixture.db.clone());
        let (settlement, completion) = runs
            .settle_terminal_with_completion(
                &fixture.task_id,
                TerminalTaskWrite::completed(Utc::now(), ConversationStatus::PendingReview)
                    .with_card_summary_json(
                        r#"{"kind":"author","status":"done","plan_digest":"model"}"#,
                    ),
                Some(fixture.input.clone()),
            )
            .await
            .unwrap();
        assert!(matches!(settlement, Settlement::Won(_)));
        assert_eq!(completion.unwrap().state, CompletionState::Resolved);
        let run = fixture.stored_run().await;
        assert_eq!(run.status, DelegationRunStatus::Completed);
        assert_eq!(run.completion_state, Some(CompletionState::Resolved));
        assert_eq!(run.card_summary_json, None);
    }

    #[tokio::test]
    async fn failed_v2_settlement_clears_cards_without_completion_evidence() {
        let fixture = TerminalFixture::new(IntentFixture::AssistantText, true).await;
        let run = fixture.stored_run().await;
        let mut active: delegation_task_run::ActiveModel = run.into();
        active.status = Set(DelegationRunStatus::Running);
        active.finished_at = Set(None);
        active.update(&fixture.db.conn).await.unwrap();

        let runs = RunStore::new(fixture.db.clone());
        let (settlement, completion) = runs
            .settle_terminal_with_completion(
                &fixture.task_id,
                TerminalTaskWrite::failed("task_failed", Utc::now(), ConversationStatus::Cancelled)
                    .with_card_summary_json(r#"{"kind":"author","status":"done"}"#),
                None,
            )
            .await
            .unwrap();

        assert!(matches!(settlement, Settlement::Won(_)));
        assert_eq!(completion, None);
        let run = fixture.stored_run().await;
        assert_eq!(run.status, DelegationRunStatus::Failed);
        assert_eq!(run.completion_state, None);
        assert_eq!(run.completion_outcome, None);
        assert_eq!(run.completion_evidence_json, None);
        assert_eq!(run.card_summary_json, None);
    }
}
