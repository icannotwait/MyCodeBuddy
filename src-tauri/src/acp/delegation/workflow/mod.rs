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
pub mod state_dto;
pub mod store;
pub mod types;
pub mod validate;

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
    project_terminal_completion, validated_design_self_review_outcome, CompletionProjectionV2,
    DesignSelfReviewDecisionError,
};
pub use dto::{
    redact_display_string, safe_public_id, ProjectedNodeStatus, PublicIdAllocator,
    WorkflowCompatibility, WorkflowEdgeSnapshot, WorkflowGateSnapshot, WorkflowGraphSnapshot,
    WorkflowNodeSnapshot, WorkflowOverallState, WorkflowPhaseSnapshot,
    WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
};
pub use error::{CompletionEvidenceError, CompletionRecoveryFenceError, WorkflowStoreError};
pub use events::{
    emit_workflow_compatibility_nudge, emit_workflow_graph_changed, emit_workflow_recovery_event,
    CompletionDecisionResolvedPayloadV1, WorkflowRecoveryEvent, COMPLETION_DECISION_RESOLVED_EVENT,
    WORKFLOW_GRAPH_CHANGED_EVENT, WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
};
pub use final_findings::{
    build_final_findings_package_v1, capture_report_context_v1, decode_final_findings_package_v1,
    encode_final_findings_package_v1, load_active_final_findings_package_v1,
    persist_final_findings_package_v1, resolve_active_final_findings_packages_v1,
    verify_final_findings_package_model_v1, verify_final_findings_package_v1,
    EncodedFinalFindingsPackageV1, FinalFindingInputV1, FinalFindingItemV1, FinalFindingsError,
    FinalFindingsPackageInputV1, FinalFindingsPackageV1, RemediationContextAvailability,
    RemediationContextInputV1, RemediationContextSnapshotV1, RemediationContextSourceKind,
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
    guard_final_delivery_core, load_workflow_recovery_snapshot_txn, publish_workflow_manifest_core,
    recover_workflow_core, settle_workflow_gate_core, settle_workflow_gate_v2_core,
    FinalDeliveryGuardRequest, FinalDeliveryGuardResult, FinalReviewReopened, PublishResult,
    PublishWorkflowRequest, RecoverWorkflowRequest, RecoverWorkflowResult, SettleGateEvidence,
    SettleResult, SettleWorkflowRequest, SettleWorkflowV2Request, StateOnlyRevisionRequest,
    StateOnlyRevisionResult, WorkflowBlockEntryRequest, WorkflowPublicationDisposition,
    WorkflowRecoveryRequiredProjection, WORKFLOW_CAPABILITY_VERSION,
};
pub use types::*;
pub use validate::validate_manifest_document;

#[cfg(test)]
mod recovery_tests;
