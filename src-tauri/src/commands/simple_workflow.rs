use crate::app_error::{AppCommandError, AppErrorCode};

pub const SIMPLE_SUCCESSOR_CREATION_RETIRED_MESSAGE: &str =
    "Automatic Simple successor creation is retired; create a new conversation and use a new Design.";

pub fn continue_archived_workflow_in_simple_core() -> Result<(), AppCommandError> {
    Err(AppCommandError::new(
        AppErrorCode::SimpleSuccessorCreationRetired,
        SIMPLE_SUCCESSOR_CREATION_RETIRED_MESSAGE,
    ))
}

#[cfg(feature = "tauri-runtime")]
#[tauri::command]
pub fn continue_archived_workflow_in_simple() -> Result<(), AppCommandError> {
    continue_archived_workflow_in_simple_core()
}

#[cfg(test)]
pub(crate) mod test_support {
    use chrono::Utc;
    use sea_orm::{ActiveModelTrait, Set};

    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestDocument, ManifestGate, ManifestNode,
        ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestWorkflowState, ResolutionMode,
        WorkUnitKeyParts, MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_PLAN,
        TASK_RISK_POLICY_VERSION, WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use crate::db::entities::delegation_task_run::{self, AdmissionClass, DelegationRunStatus};
    use crate::db::entities::delegation_workflow::{self, CompletionProtocolMode, WorkflowState};
    use crate::db::entities::{
        delegation_workflow_manifest_revision, delegation_workflow_run_binding,
    };
    use crate::db::AppDatabase;

    fn phase(id: &str) -> ManifestPhase {
        ManifestPhase {
            id: id.into(),
            kind: Some(id.into()),
            title: None,
        }
    }

    pub fn archived_manifest(
        token: &str,
        plan_rel_path: &str,
        design_rel_path: Option<&str>,
    ) -> ManifestDocument {
        let plan_author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
            rel_plan_path: plan_rel_path,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap_or_else(|_| {
            build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
                rel_plan_path: "docs/plan.md",
                agent_type: "codex",
                profile_id: None,
            })
            .expect("fallback Plan author key")
        });
        let mut phases = vec![phase(PHASE_PLAN)];
        let mut nodes = vec![ManifestNode {
            id: "plan-author".into(),
            kind: ManifestNodeKind::WorkUnit,
            phase_id: Some(PHASE_PLAN.into()),
            role: Some(ManifestNodeRole::Author),
            agent_type: Some("codex".into()),
            profile_id: None,
            task_index: None,
            work_unit_key: Some(plan_author_key),
            deps: vec![],
            required: Some(true),
            node_outcome: None,
            title: None,
        }];
        let mut gates = Vec::new();
        let design = design_rel_path.map(|rel_path| {
            phases.insert(0, phase(PHASE_DESIGN));
            let key = build_work_unit_key(&WorkUnitKeyParts::Design {
                rel_doc_path: rel_path,
                agent_type: "codex",
                profile_id: None,
            })
            .expect("Design reviewer key");
            nodes.insert(
                0,
                ManifestNode {
                    id: "design-reviewer".into(),
                    kind: ManifestNodeKind::WorkUnit,
                    phase_id: Some(PHASE_DESIGN.into()),
                    role: Some(ManifestNodeRole::Reviewer),
                    agent_type: Some("codex".into()),
                    profile_id: None,
                    task_index: None,
                    work_unit_key: Some(key),
                    deps: vec![],
                    required: Some(true),
                    node_outcome: None,
                    title: None,
                },
            );
            gates.push(ManifestGate {
                id: "design".into(),
                reviewer_cohort_node_ids: vec!["design-reviewer".into()],
                required_reviewer_node_ids: vec!["design-reviewer".into()],
                resolution_mode: ResolutionMode::ParentAdjudication,
                gate_kind: Some(DocumentGateKind::Design),
            });
            DocumentRef {
                rel_path: rel_path.into(),
                digest: format!("sha256:{}", "d".repeat(64)),
            }
        });

        ManifestDocument {
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into(),
            plan_target_rel_path: plan_rel_path.into(),
            risk_policy_version: TASK_RISK_POLICY_VERSION.into(),
            workflow_id: None,
            expected_manifest_revision: None,
            publication_token: token.into(),
            workflow_state: ManifestWorkflowState::Skeleton,
            design,
            plan: None,
            phases,
            nodes,
            edges: vec![],
            gates,
            task_policies: vec![],
        }
    }

    pub async fn seed_archived_workflow(
        db: &AppDatabase,
        parent_conversation_id: i32,
        workflow_id: &str,
        plan_rel_path: &str,
        design_rel_path: Option<&str>,
        version: i64,
        mode: CompletionProtocolMode,
    ) {
        let now = Utc::now();
        let document = archived_manifest(
            &format!("publication-{workflow_id}"),
            plan_rel_path,
            design_rel_path,
        );
        delegation_workflow::ActiveModel {
            workflow_id: Set(workflow_id.into()),
            parent_conversation_id: Set(parent_conversation_id),
            workflow_kind: Set(WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.into()),
            schema_version: Set(MANIFEST_SCHEMA_VERSION as i64),
            active_manifest_revision: Set(1),
            graph_revision: Set(7),
            workflow_state: Set(WorkflowState::Skeleton),
            capability_version: Set("workflow_manifest_v2".into()),
            publication_token: Set(format!("publication-{workflow_id}")),
            supersedes_approved_revision: Set(None),
            structural_revision: Set(1),
            design_fingerprint: Set("design-fingerprint".into()),
            plan_fingerprint: Set("plan-fingerprint".into()),
            block_cause_code: Set(None),
            block_source_manifest_revision: Set(None),
            completion_protocol_version: Set(version),
            completion_protocol_mode: Set(mode),
            legacy_source_workflow_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&db.conn)
        .await
        .expect("archived workflow header");
        delegation_workflow_manifest_revision::ActiveModel {
            workflow_id: Set(workflow_id.into()),
            manifest_revision: Set(1),
            manifest_state: Set("skeleton".into()),
            document_json: Set(serde_json::to_string(&document).expect("manifest JSON")),
            document_digest: Set(format!("sha256:{}", "a".repeat(64))),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("archived workflow revision");
    }

    pub async fn seed_bound_child(
        db: &AppDatabase,
        root_conversation_id: i32,
        child_conversation_id: i32,
        workflow_id: &str,
    ) {
        let now = Utc::now();
        delegation_task_run::ActiveModel {
            task_id: Set(format!("{workflow_id}-bound-task")),
            root_task_id: Set(format!("{workflow_id}-bound-task")),
            previous_task_id: Set(None),
            generation: Set(1),
            parent_conversation_id: Set(root_conversation_id),
            parent_tool_use_id: Set(None),
            child_conversation_id: Set(child_conversation_id),
            agent_type: Set("codex".into()),
            admission_class: Set(AdmissionClass::NormalRevision),
            lineage_root_task_id: Set(format!("{workflow_id}-bound-task")),
            history_only: Set(false),
            status: Set(DelegationRunStatus::Completed),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("bound child run");
        delegation_workflow_run_binding::ActiveModel {
            task_id: Set(format!("{workflow_id}-bound-task")),
            workflow_id: Set(workflow_id.into()),
            node_id: Set("archived-node".into()),
            manifest_revision: Set(1),
            lineage_ordinal: Set(1),
            summary_validated: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db.conn)
        .await
        .expect("bound child workflow binding");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_error::AppErrorCode;

    #[test]
    fn simple_successor_creation_retired_core_is_exact_and_state_free() {
        let error = continue_archived_workflow_in_simple_core().unwrap_err();
        assert_eq!(error.code, AppErrorCode::SimpleSuccessorCreationRetired);
        assert_eq!(
            error.message,
            "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
        );
        assert_eq!(
            serde_json::to_value(error).unwrap(),
            serde_json::json!({
                "code": "simple_successor_creation_retired",
                "message": "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
            })
        );
    }

    #[cfg(feature = "tauri-runtime")]
    #[test]
    fn simple_successor_creation_retired_tauri_wrapper_matches_core() {
        let wrapper = continue_archived_workflow_in_simple().unwrap_err();
        let core = continue_archived_workflow_in_simple_core().unwrap_err();
        assert_eq!(wrapper.code, core.code);
        assert_eq!(wrapper.message, core.message);
        assert_eq!(
            serde_json::to_value(wrapper).unwrap(),
            serde_json::to_value(core).unwrap()
        );
    }

    #[cfg(all(feature = "tauri-runtime", feature = "test-utils"))]
    #[test]
    fn simple_successor_creation_retired_tauri_ipc_ignores_stale_arguments() {
        use tauri::ipc::{CallbackFn, InvokeBody};
        use tauri::test::{get_ipc_response, mock_builder, mock_context, noop_assets, INVOKE_KEY};
        use tauri::webview::InvokeRequest;

        let app = mock_builder()
            .invoke_handler(tauri::generate_handler![
                crate::commands::simple_workflow::continue_archived_workflow_in_simple
            ])
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("webview");
        let local_url = if cfg!(any(windows, target_os = "android")) {
            "http://tauri.localhost"
        } else {
            "tauri://localhost"
        };

        let value = get_ipc_response(
            &webview,
            InvokeRequest {
                cmd: "continue_archived_workflow_in_simple".into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: local_url.parse().unwrap(),
                body: InvokeBody::from(serde_json::json!({
                    "sourceConversationId": -1,
                    "clientRequestToken": "",
                    "extra": { "malformed": true }
                })),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.to_string(),
            },
        )
        .expect_err("retired command must reject");
        assert_eq!(
            value,
            serde_json::json!({
                "code": "simple_successor_creation_retired",
                "message": "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
            })
        );
    }
}
