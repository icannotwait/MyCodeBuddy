//! Brainstorm-to-delivery workflow graph: keys, validation, store, gates, projection.

pub mod admission;
pub mod dto;
pub mod error;
pub mod events;
pub mod gates;
pub mod key;
pub mod plan_review;
pub mod project;
pub mod state_dto;
pub mod store;
pub mod types;
pub mod validate;

pub use admission::{
    admit_workflow_run_txn, emit_workflow_side_effect, on_mapped_run_transition_txn,
    on_provisional_abandon_txn, on_terminal_settle_txn, AdmissionDispatchKind, WorkflowAdmitInput,
    WorkflowTxnSideEffect,
};
pub use dto::{
    redact_display_string, safe_public_id, ProjectedNodeStatus, PublicIdAllocator,
    WorkflowCompatibility, WorkflowEdgeSnapshot, WorkflowGateSnapshot, WorkflowGraphSnapshot,
    WorkflowNodeSnapshot, WorkflowOverallState, WorkflowPhaseSnapshot,
    WORKFLOW_GRAPH_SNAPSHOT_SCHEMA_VERSION,
};
pub use error::WorkflowStoreError;
pub use events::{
    emit_workflow_compatibility_nudge, emit_workflow_graph_changed, WORKFLOW_GRAPH_CHANGED_EVENT,
    WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
};
pub use gates::{
    evaluate_execution_gate, ExecutionGateEval, ExecutionGateInput, ExecutionGateKind,
    ExecutionGateReason, ExecutionGateRunEvidence, RequiredReviewerEvidence, TerminalRunStatus,
};
pub use key::{build_work_unit_key, normalize_rel_path, parse_recognized_work_unit_key};
pub use plan_review::{
    derive_plan_review_round, FindingSeverity, FindingStatus, PlanFindingUpdate, PlanReviewError,
    PlanReviewNextAction, PlanReviewRoundState, PlanReviewRoundSubmission, PlanReviewScope,
    PlanRevisionKind,
};
pub use project::{
    evaluate_task_gate_from_pairs, evidence_from_run_and_binding, project_workflow_graph_core,
    soft_attach_workflow_graph,
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
    get_workflow_state_core, publish_workflow_manifest_core, settle_workflow_gate_core,
    PublishResult, PublishWorkflowRequest, SettleGateEvidence, SettleResult, SettleWorkflowRequest,
    WORKFLOW_CAPABILITY_VERSION,
};
pub use types::*;
pub use validate::validate_manifest_document;
