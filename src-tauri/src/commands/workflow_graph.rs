//! Read-only workflow graph snapshot surface (Tauri + shared `_core`).
//!
//! Mutations (`publish_workflow_manifest`, `settle_workflow_gate`,
//! `get_workflow_state`) stay on root companion MCP — this module only
//! exposes the redacted frontend `WorkflowGraphSnapshot` used by conversation
//! detail and live Graph refetch.

use crate::acp::delegation::workflow::{project_workflow_graph_core, WorkflowGraphSnapshot};
use crate::app_error::AppCommandError;
use crate::db::AppDatabase;

/// Shared core for desktop and server: same projector as conversation detail.
pub async fn get_workflow_graph_snapshot_core(
    db: &AppDatabase,
    conversation_id: i32,
) -> Option<WorkflowGraphSnapshot> {
    project_workflow_graph_core(db, conversation_id).await
}

#[cfg_attr(feature = "tauri-runtime", tauri::command)]
pub async fn get_workflow_graph_snapshot(
    #[cfg(feature = "tauri-runtime")] db: tauri::State<'_, AppDatabase>,
    conversation_id: i32,
) -> Result<Option<WorkflowGraphSnapshot>, AppCommandError> {
    #[cfg(feature = "tauri-runtime")]
    {
        Ok(get_workflow_graph_snapshot_core(&db, conversation_id).await)
    }
    #[cfg(not(feature = "tauri-runtime"))]
    {
        let _ = conversation_id;
        Err(AppCommandError::configuration_invalid("tauri-only command"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestDocument, ManifestEdge, ManifestGate, ManifestNode,
        ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState, ResolutionMode,
        WorkUnitKeyParts, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS,
        WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use crate::acp::delegation::workflow::{
        project_workflow_graph_core, publish_workflow_manifest_core, PublishWorkflowRequest,
    };
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::AgentType;
    use crate::web::event_bridge::EventEmitter;

    fn phase(id: &str) -> ManifestPhase {
        ManifestPhase {
            id: id.into(),
            kind: Some(id.into()),
            title: Some(id.into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn wu(
        id: &str,
        phase_id: &str,
        role: ManifestNodeRole,
        agent: &str,
        profile_id: Option<&str>,
        task_index: Option<u32>,
        key: String,
        deps: Vec<String>,
    ) -> ManifestNode {
        ManifestNode {
            id: id.into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(phase_id.into()),
            role: Some(role),
            agent_type: Some(agent.into()),
            profile_id: profile_id.map(|s| s.into()),
            task_index,
            work_unit_key: Some(key),
            deps,
            required: Some(true),
            node_outcome: None,
            title: None,
        }
    }

    fn sample_doc(token: &str) -> ManifestDocument {
        let design_path = "docs/superpowers/specs/x.md";
        let plan_path = "docs/superpowers/plans/p.md";
        let design_key = build_work_unit_key(&WorkUnitKeyParts::Design {
            rel_doc_path: design_path,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let plan_key = build_work_unit_key(&WorkUnitKeyParts::Plan {
            rel_plan_path: plan_path,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let task_impl = build_work_unit_key(&WorkUnitKeyParts::TaskImplementer {
            task_index: 1,
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();
        let task_rev = build_work_unit_key(&WorkUnitKeyParts::TaskReviewer {
            task_index: 1,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let final_rev = build_work_unit_key(&WorkUnitKeyParts::FinalReviewer {
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let final_fix = build_work_unit_key(&WorkUnitKeyParts::FinalFixer {
            agent_type: "grok",
            profile_id: None,
        })
        .unwrap();

        ManifestDocument {
            schema_version: 1,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.to_string(),
            workflow_id: None,
            expected_manifest_revision: None,
            publication_token: token.into(),
            workflow_state: ManifestWorkflowState::Estimated,
            design: Some(DocumentRef {
                rel_path: design_path.into(),
                digest: "sha256:design".into(),
            }),
            plan: Some(DocumentRef {
                rel_path: plan_path.into(),
                digest: "sha256:plan".into(),
            }),
            phases: vec![
                phase(PHASE_DESIGN),
                phase(PHASE_PLAN),
                phase(PHASE_TASKS),
                phase(PHASE_FINAL),
            ],
            nodes: vec![
                wu(
                    "design-reviewer-1",
                    PHASE_DESIGN,
                    ManifestNodeRole::Reviewer,
                    "codex",
                    None,
                    None,
                    design_key,
                    vec![],
                ),
                wu(
                    "plan-reviewer-1",
                    PHASE_PLAN,
                    ManifestNodeRole::Reviewer,
                    "codex",
                    None,
                    None,
                    plan_key,
                    vec!["design-reviewer-1".into()],
                ),
                wu(
                    "task-1-impl",
                    PHASE_TASKS,
                    ManifestNodeRole::Implementer,
                    "grok",
                    None,
                    Some(1),
                    task_impl,
                    vec!["plan-reviewer-1".into()],
                ),
                wu(
                    "task-1-rev",
                    PHASE_TASKS,
                    ManifestNodeRole::Reviewer,
                    "codex",
                    None,
                    Some(1),
                    task_rev,
                    vec!["task-1-impl".into()],
                ),
                wu(
                    "final-reviewer",
                    PHASE_FINAL,
                    ManifestNodeRole::Reviewer,
                    "codex",
                    None,
                    None,
                    final_rev,
                    vec!["task-1-rev".into()],
                ),
                wu(
                    "final-fixer",
                    PHASE_FINAL,
                    ManifestNodeRole::Fixer,
                    "grok",
                    None,
                    None,
                    final_fix,
                    vec!["final-reviewer".into()],
                ),
            ],
            edges: vec![ManifestEdge {
                id: Some("e1".into()),
                from: "task-1-impl".into(),
                to: "task-1-rev".into(),
            }],
            gates: vec![
                ManifestGate {
                    id: "design".into(),
                    required_reviewer_node_ids: vec!["design-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some(DocumentGateKind::Design),
                },
                ManifestGate {
                    id: "plan".into(),
                    required_reviewer_node_ids: vec!["plan-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some(DocumentGateKind::Plan),
                },
            ],
        }
    }

    #[tokio::test]
    async fn snapshot_equals_detail_projection() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/wf-api-snapshot").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;

        publish_workflow_manifest_core(
            &db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest {
                document: sample_doc("api-snap-token"),
            },
        )
        .await
        .expect("publish");

        let via_command = get_workflow_graph_snapshot_core(&db, parent).await;
        let via_detail_projector = project_workflow_graph_core(&db, parent).await;

        assert!(via_command.is_some(), "snapshot expected after publish");
        assert_eq!(
            via_command, via_detail_projector,
            "read API must return the same WorkflowGraphSnapshot as detail projection"
        );

        let json = serde_json::to_string(&via_command).unwrap();
        assert!(
            !json.contains("work_unit_key"),
            "read snapshot must not leak work_unit_key: {json}"
        );
    }

    #[tokio::test]
    async fn snapshot_none_without_workflow() {
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, "/tmp/wf-api-empty").await;
        let parent = seed_conversation(&db, folder, AgentType::Codex).await;
        assert!(get_workflow_graph_snapshot_core(&db, parent)
            .await
            .is_none());
    }
}
