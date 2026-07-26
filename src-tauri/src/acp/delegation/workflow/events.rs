//! Workflow graph live events via shared [`EventEmitter`].

use serde::Serialize;

use crate::web::event_bridge::{emit_event, EventEmitter};

/// Live clock event after a durable manifest or settlement mutation commits.
pub const WORKFLOW_GRAPH_CHANGED_EVENT: &str = "workflow_graph://changed";

/// Observed-only compatibility nudge (no durable graph clock).
pub const WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT: &str = "workflow_graph://compatibility_nudge";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowGraphChangedPayload {
    pub parent_conversation_id: i32,
    pub workflow_id: String,
    pub graph_revision: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkflowCompatibilityNudgePayload {
    pub parent_conversation_id: i32,
}

/// Emit `workflow_graph://changed` after a successful publish/settle commit.
pub fn emit_workflow_graph_changed(
    emitter: &EventEmitter,
    parent_conversation_id: i32,
    workflow_id: &str,
    graph_revision: u64,
) {
    emit_event(
        emitter,
        WORKFLOW_GRAPH_CHANGED_EVENT,
        WorkflowGraphChangedPayload {
            parent_conversation_id,
            workflow_id: workflow_id.to_string(),
            graph_revision,
        },
    );
}

/// Emit `workflow_graph://compatibility_nudge` for recognized A1 keys without
/// a durable manifest (Task 6 admission path; defined here for shared use).
pub fn emit_workflow_compatibility_nudge(emitter: &EventEmitter, parent_conversation_id: i32) {
    emit_event(
        emitter,
        WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT,
        WorkflowCompatibilityNudgePayload {
            parent_conversation_id,
        },
    );
}
