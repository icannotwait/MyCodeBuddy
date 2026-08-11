//! Brainstorm-to-delivery workflow graph: keys, validation, store, gates, projection.

pub mod admission;
pub mod artifact_resolver;
pub mod completion_evidence;
pub mod completion_intent;
pub mod completion_projection;
pub mod dto;
pub mod error;
pub mod events;
pub mod evidence_scope;
pub mod final_findings;
pub mod gates;
pub mod key;
pub mod plan_material;
pub mod plan_review;
pub mod project;
pub mod recovery_policy;
pub mod simple;
pub mod simple_parse;
pub mod state_dto;
pub mod store;
pub mod types;
pub mod validate;
pub mod workflow_restart;

pub use admission::{
    accept_complete_work_txn, admit_workflow_run_txn, emit_workflow_side_effect,
    load_workflow_child_mcp_binding, on_mapped_run_transition_txn, on_provisional_abandon_txn,
    on_terminal_settle_txn, AdmissionDispatchKind, CompleteWorkError, WorkflowAdmitInput,
    WorkflowTxnSideEffect,
};
#[cfg(test)]
pub(crate) use admission::{accept_complete_work_txn_with_test_control, CompleteWorkTestControl};
pub use artifact_resolver::{
    resolve_document, resolve_final_delivery, resolve_git_head_clean, resolve_producer_completion,
    resolve_reviewer_completion, ArtifactError, ArtifactFailure, ArtifactKind,
    DocumentSha256Artifact, GitHeadV1Artifact, ResolvedArtifact,
};
pub use completion_evidence::{
    ensure_completion_recovery_not_fenced_txn, load_validated_completion_evidence,
    materialize_terminal_completion_txn, open_design_self_review_decision_txn,
    reconcile_completion_attentions_txn, resolve_completion_decision_txn,
    resolve_deleted_conversation_completion_attentions_txn, resolve_design_self_review_txn,
    resolve_workflow_completion_attentions_txn, retry_completion_artifact_for_user_txn,
    retry_completion_artifact_txn, ArtifactRecoveryPayloadV1, CompletionAttentionCas,
    CompletionAttentionReconcileReport, CompletionMutationError, CompletionSourceAuditRef,
    DesignSelfReviewPayloadV1, TerminalCompletionInput, TerminalCompletionResult,
    ValidatedReportCandidate,
};
pub use completion_intent::{
    build_conclusion_suffix, resolve_completion_intent, CompletionCandidate, CompletionDiagnostic,
    CompletionDiagnosticCode, CompletionIntent, CompletionIntentReason, CompletionIntentSource,
    CompletionOutcome, CompletionReportCandidate, CompletionReportReadFailure,
    CompletionResolution, CompletionResolverInput, CompletionRole, CompletionToolIntent,
};
pub use completion_projection::{
    load_completion_projection, project_terminal_completion, validated_design_self_review_outcome,
    CompletionCardState, CompletionCardV2, CompletionProjectionV2, DesignSelfReviewDecisionError,
    COMPLETION_CARD_SUMMARY_MAX_BYTES,
};
pub use dto::{
    redact_display_string, safe_public_id, ProjectedNodeStatus, PublicIdAllocator,
    ArchivedWorkflowNavigationSnapshot, SimpleWorkflowLocatorSnapshot, WorkflowCompatibility,
    WorkflowEdgeSnapshot, WorkflowGateSnapshot, WorkflowGraphSnapshot, WorkflowNodeSnapshot,
    WorkflowNodeSyncState, WorkflowOverallState, WorkflowPhaseSnapshot,
    WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
};
pub use error::{
    require_v2_mutation, CompletionEvidenceError, CompletionProtocolConfigurationRemoved,
    CompletionRecoveryFenceError, WorkflowStoreError,
};
pub use events::{
    emit_workflow_compatibility_nudge, emit_workflow_graph_changed, emit_workflow_recovery_event,
    CompletionDecisionResolvedPayloadV1, WorkflowRecoveryEvent, COMPLETION_DECISION_RESOLVED_EVENT,
    WORKFLOW_GRAPH_CHANGED_EVENT, WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
};
pub use final_findings::{
    build_final_findings_package_v1, capture_report_context_v1, decode_final_findings_package_v1,
    encode_final_findings_package_v1, load_active_final_findings_package_v1,
    persist_final_findings_package_v1, remediation_context_inputs_from_snapshots_v1,
    resolve_active_final_findings_packages_v1, snapshot_remediation_contexts_v1,
    verify_final_findings_package_model_v1, verify_final_findings_package_v1,
    EncodedFinalFindingsPackageV1, FinalFindingInputV1, FinalFindingItemV1, FinalFindingsError,
    FinalFindingsPackageInputV1, FinalFindingsPackageV1, FinalReviewerEvaluationV1,
    RemediationContextAvailability, RemediationContextInputV1, RemediationContextSnapshotV1,
    RemediationContextSourceKind,
};
pub use gates::{
    evaluate_execution_gate, reduce_design_gate, ExecutionGateEval, ExecutionGateInput,
    ExecutionGateKind, ExecutionGateReason, ExecutionGateRunEvidence, RequiredReviewerEvidence,
    TerminalRunStatus,
};
pub use key::{build_work_unit_key, normalize_rel_path, parse_recognized_work_unit_key};
pub use plan_material::{
    authorize_localized_plan_change, bind_plan_material, classify_plan_change,
    derive_holistic_full_cohort_selector, derive_plan_reviewer_selector,
    localized_plan_change_context, parse_plan_material, plan_publication_material_decision,
    plan_publication_requires_new_lineage, select_corrective_reviewers, BoundPlanMaterialMap,
    MaterialSelectorV1, PlanLocalizedChangeAuthorizationV1, PlanMaterialChangeInputV1,
    PlanMaterialEntryV1, PlanMaterialError, PlanMaterialErrorKind, PlanMaterialMap,
    PlanMaterialSchemaV1, PlanPublicationMaterialDecisionV1,
};
pub use plan_review::{
    derive_plan_review_round, derive_plan_review_round_v2, reviewer_outcome_rank,
    strictly_improves, FindingSeverity, FindingStatus, PlanFindingUpdate, PlanReviewChangeV2,
    PlanReviewDecisionV2, PlanReviewError, PlanReviewNextAction, PlanReviewRoundInputV2,
    PlanReviewRoundState, PlanReviewRoundStateV2, PlanReviewRoundSubmission, PlanReviewScope,
    PlanReviewerOutcomeV2, PlanRevisionKind,
};
pub use project::{
    evaluate_task_gate_from_pairs, evidence_from_run_and_binding, project_workflow_graph_core,
    soft_attach_workflow_graph,
};
pub use recovery_policy::{
    decide_workflow_recovery, hash_displayed_reset_reason, WorkflowRecoveryActiveRun,
    WorkflowRecoveryBindingLifecycle, WorkflowRecoveryBlocker, WorkflowRecoveryCauseCode,
    WorkflowRecoveryConfirmation, WorkflowRecoveryDecision, WorkflowRecoveryDisposition,
    WorkflowRecoveryDocumentIdentity, WorkflowRecoveryFrozenTaskCohort,
    WorkflowRecoveryPlanGateEvidence, WorkflowRecoveryPlanIdentity, WorkflowRecoveryProjection,
    WorkflowRecoveryRiskClass, WorkflowRecoverySnapshot, WorkflowRecoveryStopCode,
};
pub use simple::{
    load_simple_workflow, register_simple_workflow, register_simple_workflow_with_source,
    resolve_conversation_workflow_mode, ConversationWorkflowMode, SimpleWorkflowError,
    SimpleWorkflowRegistration,
};
pub use simple_parse::{
    parse_simple_plan, parse_simple_progress, read_simple_plan, read_simple_progress,
    SimpleDeclaredStatus, SimpleFinalReviewStatus, SimpleParseError, SimplePlanDocument,
    SimplePlanTask, SimpleProgressDocument, SimpleProgressRun, SimpleProgressSnapshot,
    SimpleProgressTask,
};
pub use state_dto::{
    project_workflow_state_index, ActionableTaskRouteDto, PlanFindingStubDto,
    PlanRecoverySourceDto, PlanReviewIndexDto, TaskPolicyIndexDto, WorkflowGateStateDto,
    WorkflowIndexNodeOmissionMeta, WorkflowIndexOmissionState, WorkflowIndexOmissionStep,
    WorkflowIndexProtectedError, WorkflowNodeIndexDto, WorkflowNodeStateDto, WorkflowStateDetail,
    WorkflowStateDto, WorkflowStateIndexDto, DIGEST_PREFIX_HEX_CHARS, INDEX_MAX_FINDING_STUBS,
    INDEX_MAX_NODES,
};
pub use store::{
    append_state_only_revision_txn, append_workflow_block_revision_txn,
    estimated_plan_publication_material_decision, get_workflow_state_core,
    guard_current_final_delivery_core, guard_final_delivery_core, guard_task_final_delivery_core,
    load_completion_protocol_for_conversation, load_completion_protocol_header,
    load_workflow_recovery_snapshot_txn, publish_workflow_manifest_core, recover_workflow_core,
    settle_workflow_gate_v2_core, FinalDeliveryGuardRequest, FinalDeliveryGuardResult,
    FinalReviewReopened, PublishResult, PublishWorkflowRequest, RecoverWorkflowRequest,
    RecoverWorkflowResult, SettleResult, SettleWorkflowV2Request, StateOnlyRevisionRequest,
    StateOnlyRevisionResult, WorkflowBlockEntryRequest, WorkflowPublicationDisposition,
    WorkflowRecoveryRequiredProjection, WORKFLOW_CAPABILITY_VERSION,
};
#[cfg(test)]
use store::{
    settle_workflow_gate_v2_from_fixture as settle_workflow_gate_core, SettleGateEvidence,
    SettleWorkflowRequest,
};
pub use types::*;
pub use validate::validate_manifest_document;
pub use workflow_restart::load_historical_workflow_context;

#[cfg(test)]
mod recovery_tests;
