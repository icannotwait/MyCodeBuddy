//! Workflow graph live events via shared [`EventEmitter`].

use serde::{Deserialize, Serialize};

use crate::acp::delegation::workflow::CompletionOutcome;
use crate::db::entities::delegation_attention_request::AttentionKind;

use crate::web::event_bridge::{emit_event, EventEmitter};

#[cfg(test)]
thread_local! {
    static INJECT_WORKFLOW_RECOVERY_EVENT_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Live clock event after a durable manifest or settlement mutation commits.
pub const WORKFLOW_GRAPH_CHANGED_EVENT: &str = "workflow_graph://changed";

/// Observed-only compatibility nudge (no durable graph clock).
pub const WORKFLOW_GRAPH_COMPATIBILITY_NUDGE_EVENT: &str = "workflow_graph://compatibility_nudge";

pub const COMPLETION_DECISION_RESOLVED_EVENT: &str = "completion_decision_resolved";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletionDecisionResolvedPayloadV1 {
    pub version: u32,
    pub event_id: String,
    pub workflow_id: String,
    pub task_id: String,
    pub node_id: String,
    pub kind: AttentionKind,
    pub outcome: CompletionOutcome,
    pub evidence_scope_digest: String,
    pub graph_revision: u64,
}

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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "event")]
pub enum WorkflowRecoveryEvent {
    #[serde(rename = "workflow.recovery_decision")]
    RecoveryDecision {
        workflow_id: String,
        source_manifest_revision: u64,
        graph_revision: u64,
        action: String,
        target_state: Option<super::types::ManifestWorkflowState>,
        cause_code: String,
    },
    #[serde(rename = "workflow.recovery_confirmation_requested")]
    RecoveryConfirmationRequested {
        workflow_id: String,
        recovery_authorization_id: String,
        source_manifest_revision: u64,
        graph_revision: u64,
        action: String,
        target_state: Option<super::types::ManifestWorkflowState>,
        cause_code: String,
    },
    #[serde(rename = "workflow.recovery_authorization_consumed")]
    RecoveryAuthorizationConsumed {
        workflow_id: String,
        recovery_authorization_id: String,
        manifest_revision: u64,
        graph_revision: u64,
        action: String,
    },
    #[serde(rename = "workflow.recovery_rejected")]
    RecoveryRejected {
        workflow_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        recovery_authorization_id: Option<String>,
        source_manifest_revision: u64,
        graph_revision: u64,
        action: String,
        cause_code: String,
        rejection_code: String,
    },
    #[serde(rename = "workflow.state_only_revision_created")]
    StateOnlyRevisionCreated {
        workflow_id: String,
        source_manifest_revision: u64,
        manifest_revision: u64,
        graph_revision: u64,
        target_state: super::types::ManifestWorkflowState,
        cause_code: String,
    },
    #[serde(rename = "workflow.plan_lineage_reset")]
    PlanLineageReset {
        workflow_id: String,
        recovery_authorization_id: String,
        source_manifest_revision: u64,
        manifest_revision: u64,
        graph_revision: u64,
        action: String,
        target_state: super::types::ManifestWorkflowState,
        cause_code: String,
    },
    #[serde(rename = "workflow.binding_reactivated")]
    BindingReactivated {
        workflow_id: String,
        manifest_revision: u64,
        graph_revision: u64,
        target_state: super::types::ManifestWorkflowState,
    },
}

impl WorkflowRecoveryEvent {
    pub const fn channel(&self) -> &'static str {
        match self {
            Self::RecoveryDecision { .. } => "workflow.recovery_decision",
            Self::RecoveryConfirmationRequested { .. } => {
                "workflow.recovery_confirmation_requested"
            }
            Self::RecoveryAuthorizationConsumed { .. } => {
                "workflow.recovery_authorization_consumed"
            }
            Self::RecoveryRejected { .. } => "workflow.recovery_rejected",
            Self::StateOnlyRevisionCreated { .. } => "workflow.state_only_revision_created",
            Self::PlanLineageReset { .. } => "workflow.plan_lineage_reset",
            Self::BindingReactivated { .. } => "workflow.binding_reactivated",
        }
    }
}

#[cfg(test)]
pub(crate) fn set_inject_workflow_recovery_event_failure(enabled: bool) {
    INJECT_WORKFLOW_RECOVERY_EVENT_FAILURE.with(|flag| flag.set(enabled));
}

pub fn emit_workflow_recovery_event(
    emitter: &EventEmitter,
    event: WorkflowRecoveryEvent,
) -> Result<(), &'static str> {
    #[cfg(test)]
    if INJECT_WORKFLOW_RECOVERY_EVENT_FAILURE.with(std::cell::Cell::get) {
        return Err("injected workflow recovery event failure");
    }
    let channel = event.channel();
    emit_event(emitter, channel, event);
    Ok(())
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
