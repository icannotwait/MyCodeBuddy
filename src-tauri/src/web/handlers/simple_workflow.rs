use std::sync::Arc;

use axum::{extract::Extension, Json};
use serde::Deserialize;

use crate::app_error::AppCommandError;
use crate::app_state::AppState;
use crate::commands::simple_workflow::{
    continue_archived_workflow_in_simple_core, SimpleSuccessorResult,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueArchivedWorkflowInSimpleParams {
    pub source_conversation_id: i32,
    pub client_request_token: String,
}

pub async fn continue_archived_workflow_in_simple(
    Extension(state): Extension<Arc<AppState>>,
    Json(params): Json<ContinueArchivedWorkflowInSimpleParams>,
) -> Result<Json<SimpleSuccessorResult>, AppCommandError> {
    continue_archived_workflow_in_simple_core(
        &state.db,
        &state.emitter,
        params.source_conversation_id,
        &params.client_request_token,
    )
    .await
    .map(Json)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;

    use axum::http::StatusCode;
    use serde_json::{json, Value};

    use crate::acp::delegation::workflow::register_simple_workflow;
    use crate::app_state::AppState;
    use crate::commands::simple_workflow::test_support::seed_archived_workflow;
    use crate::db::entities::delegation_workflow::CompletionProtocolMode;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::models::AgentType;
    use crate::web::router::build_router;
    use crate::web::shutdown::ShutdownSignal;

    async fn call_json(
        state: Arc<AppState>,
        static_dir: &std::path::Path,
        body: Value,
        auth: Option<&str>,
    ) -> (StatusCode, Value) {
        let app = build_router(
            state,
            "secret".into(),
            static_dir.to_path_buf(),
            Arc::new(ShutdownSignal::new()),
        );
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind HTTP test listener");
        let addr = listener.local_addr().unwrap();
        let server = axum::serve(listener, app);
        let handle = tokio::spawn(async move {
            let _ = server.await;
        });
        tokio::task::yield_now().await;
        let client = reqwest::Client::new();
        let mut request = client
            .post(format!(
                "http://{addr}/api/continue_archived_workflow_in_simple"
            ))
            .json(&body);
        if let Some(auth) = auth {
            request = request.header("Authorization", auth);
        }
        let response = request.send().await.expect("HTTP response");
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        let bytes = response.bytes().await.unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            json!({ "raw": String::from_utf8_lossy(&bytes).into_owned() })
        });
        handle.abort();
        (status, value)
    }

    #[tokio::test]
    async fn simple_successor_http_route_requires_server_auth() {
        let data_dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new_for_test(
            fresh_in_memory_db().await,
            data_dir.path().to_path_buf(),
        ));
        let (status, _) = call_json(
            state,
            data_dir.path(),
            json!({
                "sourceConversationId": 1,
                "clientRequestToken": "unauthorized-request",
            }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn simple_successor_http_route_has_desktop_parity_json_shape() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("docs")).unwrap();
        std::fs::write(workspace.path().join("docs/plan.md"), "# Plan\n").unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let source = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            source,
            "workflow-http-successor",
            "docs/plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let state = Arc::new(AppState::new_for_test(db, workspace.path().to_path_buf()));

        let (status, body) = call_json(
            state,
            workspace.path(),
            json!({
                "sourceConversationId": source,
                "clientRequestToken": "http-request",
            }),
            Some("Bearer secret"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["successor_conversation_id"].as_i64().is_some());
        assert_eq!(body["created"], true);
        assert_eq!(body["plan_rel_path"], "docs/plan.md");
        assert!(body["progress_rel_path"].as_str().is_some());
        assert!(body["bootstrap_prompt"].as_str().is_some());
        assert!(body.get("successorConversationId").is_none());
    }

    #[tokio::test]
    async fn simple_successor_http_maps_source_and_plan_errors_stably() {
        let workspace = tempfile::tempdir().unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let ordinary = seed_conversation(&db, folder, AgentType::Codex).await;
        let simple = seed_conversation(&db, folder, AgentType::Codex).await;
        register_simple_workflow(&db.conn, simple, "docs/plan.md", None)
            .await
            .unwrap();
        let archived = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            archived,
            "workflow-http-missing-plan",
            "docs/missing.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        let state = Arc::new(AppState::new_for_test(db, workspace.path().to_path_buf()));

        let (ordinary_status, ordinary_body) = call_json(
            state.clone(),
            workspace.path(),
            json!({
                "sourceConversationId": ordinary,
                "clientRequestToken": "ordinary-http-request",
            }),
            Some("Bearer secret"),
        )
        .await;
        assert_eq!(ordinary_status, StatusCode::BAD_REQUEST);
        assert_eq!(
            ordinary_body["code"],
            "simple_successor_source_not_archived"
        );
        assert_eq!(
            ordinary_body["message"],
            "Source conversation is not an archived workflow"
        );

        let (simple_status, simple_body) = call_json(
            state.clone(),
            workspace.path(),
            json!({
                "sourceConversationId": simple,
                "clientRequestToken": "simple-http-request",
            }),
            Some("Bearer secret"),
        )
        .await;
        assert_eq!(simple_status, StatusCode::CONFLICT);
        assert_eq!(
            simple_body["code"],
            "simple_successor_source_already_simple"
        );
        assert_eq!(
            simple_body["message"],
            "Source conversation already uses Simple"
        );

        let (plan_status, plan_body) = call_json(
            state,
            workspace.path(),
            json!({
                "sourceConversationId": archived,
                "clientRequestToken": "missing-plan-http-request",
            }),
            Some("Bearer secret"),
        )
        .await;
        assert_eq!(plan_status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            plan_body["code"],
            "simple_successor_plan_unavailable"
        );
        assert_eq!(plan_body["detail"], "docs/missing.md");
    }
}
