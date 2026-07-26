//! Brainstorm-to-delivery workflow graph: keys, validation, store, events.

pub mod error;
pub mod events;
pub mod key;
pub mod state_dto;
pub mod store;
pub mod types;
pub mod validate;

pub use error::WorkflowStoreError;
pub use events::{
    emit_workflow_compatibility_nudge, emit_workflow_graph_changed, WORKFLOW_GRAPH_CHANGED_EVENT,
    WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
};
pub use key::{build_work_unit_key, normalize_rel_path, parse_recognized_work_unit_key};
pub use state_dto::{WorkflowGateStateDto, WorkflowNodeStateDto, WorkflowStateDto};
pub use store::{
    get_workflow_state_core, publish_workflow_manifest_core, settle_workflow_gate_core,
    PublishResult, PublishWorkflowRequest, SettleResult, SettleWorkflowRequest,
    WORKFLOW_CAPABILITY_VERSION,
};
pub use types::*;
pub use validate::validate_manifest_document;
