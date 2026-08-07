//! HTTP handlers for the read-only workflow graph snapshot.
//!
//! Mirrors the Tauri command `get_workflow_graph_snapshot`. Mutation tools
//! (`publish_workflow_manifest`, `settle_workflow_gate`, `get_workflow_state`)
//! are intentionally absent from the Axum route table.

use std::sync::Arc;

use axum::{
    extract::Extension,
    http::HeaderValue,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::workflow_graph::get_workflow_graph_snapshot_core;
use crate::web::auth::COMPLETION_CONTEXT_HEADER;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkflowGraphSnapshotParams {
    pub conversation_id: i32,
}

pub async fn get_workflow_graph_snapshot(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<GetWorkflowGraphSnapshotParams>,
) -> Result<Response, AppCommandError> {
    let snapshot = get_workflow_graph_snapshot_core(&state.db, params.conversation_id).await;
    let has_snapshot = snapshot.is_some();
    let mut response = Json(snapshot).into_response();
    if has_snapshot {
        let context = state
            .web_server_state
            .completion_authorizations()
            .issue(params.conversation_id);
        response.headers_mut().insert(
            COMPLETION_CONTEXT_HEADER,
            HeaderValue::from_str(&context).expect("UUID completion context is a valid header"),
        );
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::delegation::workflow::key::build_work_unit_key;
    use crate::acp::delegation::workflow::types::{
        DocumentGateKind, DocumentRef, ManifestDocument, ManifestEdge, ManifestGate, ManifestNode,
        ManifestNodeKind, ManifestNodeRole, ManifestPhase, ManifestTaskPolicy, ManifestTaskRisk,
        ManifestTaskRoute, ManifestWorkflowState, ResolutionMode, TaskRiskLevel, WorkUnitKeyParts,
        MANIFEST_SCHEMA_VERSION, PHASE_DESIGN, PHASE_FINAL, PHASE_PLAN, PHASE_TASKS,
        WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY,
    };
    use crate::acp::delegation::workflow::WorkflowGraphSnapshot;
    use crate::acp::delegation::workflow::{
        project_workflow_graph_core, publish_workflow_manifest_core, PublishWorkflowRequest,
    };
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::AgentType;
    use crate::web::auth::require_token_with_completion_authorizations;
    use crate::web::event_bridge::EventEmitter;
    use axum::http::StatusCode;
    use axum::routing::post;
    use serde_json::json;
    use std::net::SocketAddr;

    /// Forbidden mutation / agent-facing tools — must not be HTTP routes.
    const FORBIDDEN_HTTP_COMMANDS: &[&str] = &[
        "publish_workflow_manifest",
        "settle_workflow_gate",
        "get_workflow_state",
    ];

    async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
        let db = fresh_in_memory_db().await;
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new_for_test(db, dir.path().to_path_buf()));
        (state, dir)
    }

    /// Mini router matching production: POST snapshot only + 501 fallback.
    fn snapshot_router(state: Arc<AppState>, token: String) -> axum::Router {
        let completion_authorizations = state.web_server_state.completion_authorizations();
        axum::Router::new()
            .nest(
                "/api",
                axum::Router::new()
                    .route(
                        "/get_workflow_graph_snapshot",
                        post(get_workflow_graph_snapshot),
                    )
                    .fallback(api_not_found)
                    .layer(axum::middleware::from_fn(move |req, next| {
                        let token = token.clone();
                        let completion_authorizations = completion_authorizations.clone();
                        async move {
                            require_token_with_completion_authorizations(
                                req,
                                next,
                                token,
                                completion_authorizations,
                            )
                            .await
                        }
                    })),
            )
            .layer(axum::Extension(state))
    }

    async fn api_not_found(uri: axum::http::Uri) -> axum::response::Response {
        use axum::response::IntoResponse;
        let command = uri.path().trim_start_matches('/');
        (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({
                "code": "not_implemented",
                "message": format!("API endpoint '{command}' is not available in web mode"),
            })),
        )
            .into_response()
    }

    async fn call_json(
        state: Arc<AppState>,
        token: &str,
        path: &str,
        body: serde_json::Value,
        auth_header: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let app = snapshot_router(state, token.to_string());
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind");
        let addr = listener.local_addr().unwrap();
        let server = axum::serve(listener, app);
        let handle = tokio::spawn(async move {
            let _ = server.await;
        });
        tokio::task::yield_now().await;
        let client = reqwest::Client::new();
        let mut req = client.post(format!("http://{addr}{path}")).json(&body);
        if let Some(h) = auth_header {
            req = req.header("Authorization", h);
        }
        let resp = req.send().await.expect("http");
        let status = StatusCode::from_u16(resp.status().as_u16())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let value = resp.json::<serde_json::Value>().await.unwrap_or(json!({}));
        handle.abort();
        (status, value)
    }

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
        let plan_key = build_work_unit_key(&WorkUnitKeyParts::PlanReviewer {
            rel_plan_path: plan_path,
            agent_type: "codex",
            profile_id: None,
        })
        .unwrap();
        let author_key = build_work_unit_key(&WorkUnitKeyParts::PlanAuthor {
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
            schema_version: MANIFEST_SCHEMA_VERSION,
            workflow_kind: WORKFLOW_KIND_BRAINSTORM_TO_DELIVERY.to_string(),
            plan_target_rel_path: plan_path.into(),
            risk_policy_version: "b2d_task_risk_v1".into(),
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
                wu(
                    "plan-author",
                    PHASE_PLAN,
                    ManifestNodeRole::Author,
                    "codex",
                    None,
                    None,
                    author_key,
                    vec![],
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
                    reviewer_cohort_node_ids: vec!["design-reviewer-1".into()],
                    required_reviewer_node_ids: vec!["design-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some(DocumentGateKind::Design),
                },
                ManifestGate {
                    id: "plan".into(),
                    reviewer_cohort_node_ids: vec!["plan-reviewer-1".into()],
                    required_reviewer_node_ids: vec!["plan-reviewer-1".into()],
                    resolution_mode: ResolutionMode::ParentAdjudication,
                    gate_kind: Some(DocumentGateKind::Plan),
                },
            ],
            task_policies: vec![ManifestTaskPolicy {
                task_index: 1,
                risk: ManifestTaskRisk {
                    level: TaskRiskLevel::Normal,
                    hard_triggers: vec![],
                    soft_signals: vec![],
                    score: 0,
                    reason: "normal fixture".into(),
                },
                route: ManifestTaskRoute {
                    implementer_node_id: "task-1-impl".into(),
                    reviewer_node_ids: vec!["task-1-rev".into()],
                },
                allow_noop_verification: false,
            }],
        }
    }

    #[tokio::test]
    async fn http_snapshot_equals_detail_projection() {
        let (state, _dir) = test_state().await;
        let folder = seed_folder(&state.db, "/tmp/wf-http-snap").await;
        let parent = seed_conversation(&state.db, folder, AgentType::Codex).await;
        publish_workflow_manifest_core(
            &state.db,
            &EventEmitter::Noop,
            parent,
            PublishWorkflowRequest {
                document: sample_doc("http-snap-token"),
            },
        )
        .await
        .expect("publish");

        let expected = project_workflow_graph_core(&state.db, parent)
            .await
            .expect("detail projection");

        let (status, body) = call_json(
            state,
            "secret",
            "/api/get_workflow_graph_snapshot",
            json!({ "conversationId": parent }),
            Some("Bearer secret"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let got: Option<WorkflowGraphSnapshot> =
            serde_json::from_value(body).expect("decode snapshot");
        assert_eq!(got.as_ref(), Some(&expected));
    }

    #[tokio::test]
    async fn http_snapshot_requires_auth() {
        let (state, _dir) = test_state().await;
        let (status, _) = call_json(
            state,
            "secret",
            "/api/get_workflow_graph_snapshot",
            json!({ "conversationId": 1 }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn mutation_tools_not_on_http() {
        let (state, _dir) = test_state().await;
        for cmd in FORBIDDEN_HTTP_COMMANDS {
            let (status, body) = call_json(
                state.clone(),
                "secret",
                &format!("/api/{cmd}"),
                json!({}),
                Some("Bearer secret"),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::NOT_IMPLEMENTED,
                "{cmd} must not be registered as an HTTP route"
            );
            assert_eq!(body["code"], "not_implemented");
        }
    }
}
