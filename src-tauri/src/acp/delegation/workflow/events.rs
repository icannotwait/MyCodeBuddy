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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::event_bridge::{EventEmitter, WebEventBroadcaster};
    use std::sync::Arc;

    #[test]
    fn emit_changed_and_nudge_payload_shapes() {
        let broadcaster = Arc::new(WebEventBroadcaster::new());
        let mut rx = broadcaster.subscribe();
        let emitter = EventEmitter::test_web_only(broadcaster);

        emit_workflow_graph_changed(&emitter, 7, "wf-1", 3);
        let evt = rx.try_recv().expect("changed");
        assert_eq!(evt.channel, WORKFLOW_GRAPH_CHANGED_EVENT);
        assert_eq!(evt.payload["parent_conversation_id"], 7);
        assert_eq!(evt.payload["workflow_id"], "wf-1");
        assert_eq!(evt.payload["graph_revision"], 3);

        emit_workflow_compatibility_nudge(&emitter, 9);
        let nudge = rx.try_recv().expect("nudge");
        assert_eq!(nudge.channel, WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT);
        assert_eq!(nudge.payload["parent_conversation_id"], 9);
        assert!(nudge.payload.get("workflow_id").is_none());
        assert!(nudge.payload.get("graph_revision").is_none());
    }
}
