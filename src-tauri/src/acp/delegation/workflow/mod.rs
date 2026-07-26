//! Brainstorm-to-delivery workflow graph: keys, validation, store, gates, projection.

pub mod dto;
pub mod error;
pub mod events;
pub mod gates;
pub mod key;
pub mod project;
pub mod state_dto;
pub mod store;
pub mod types;
pub mod validate;

pub use dto::{
    redact_display_string, safe_public_id, PublicIdAllocator, ProjectedNodeStatus,
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
    ExecutionGateReason, ExecutionGateRunEvidence, TerminalRunStatus,
};
pub use key::{build_work_unit_key, normalize_rel_path, parse_recognized_work_unit_key};
pub use project::{
    evidence_from_run_and_binding, evaluate_task_gate_from_pairs, project_workflow_graph_core,
    soft_attach_workflow_graph,
};
pub use state_dto::{WorkflowGateStateDto, WorkflowNodeStateDto, WorkflowStateDto};
pub use store::{
    get_workflow_state_core, publish_workflow_manifest_core, settle_workflow_gate_core,
    PublishResult, PublishWorkflowRequest, SettleResult, SettleWorkflowRequest,
    WORKFLOW_CAPABILITY_VERSION,
};
pub use types::*;
pub use validate::validate_manifest_document;
