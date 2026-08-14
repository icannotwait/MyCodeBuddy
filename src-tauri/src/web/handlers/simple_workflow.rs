use crate::app_error::AppCommandError;
use crate::commands::simple_workflow::continue_archived_workflow_in_simple_core;

pub async fn continue_archived_workflow_in_simple() -> Result<(), AppCommandError> {
    continue_archived_workflow_in_simple_core()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use sea_orm::{EntityTrait, PaginatorTrait, QueryOrder, QuerySelect};

    use crate::acp::delegation::workflow::register_simple_workflow;
    use crate::app_state::AppState;
    use crate::commands::simple_workflow::test_support::{
        seed_archived_workflow, seed_bound_child,
    };
    use crate::db::entities::delegation_workflow::CompletionProtocolMode;
    use crate::db::entities::{
        auto_title_job, conversation, delegation_attention_request, delegation_task_run,
        delegation_workflow, recovery_authorization, simple_workflow,
    };
    use crate::db::service::conversation_service;
    use crate::db::test_helpers::{fresh_in_memory_db, seed_conversation, seed_folder};
    use crate::db::AppDatabase;
    use crate::models::AgentType;
    use crate::web::router::build_router;
    use crate::web::shutdown::ShutdownSignal;

    async fn call_raw(
        state: Arc<AppState>,
        static_dir: &std::path::Path,
        body: &str,
        content_type: Option<&str>,
        auth: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let app = build_router(
            state,
            "secret".into(),
            static_dir.to_path_buf(),
            Arc::new(ShutdownSignal::new()),
        );
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind HTTP test listener");
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let mut request = client
            .post(format!(
                "http://{addr}/api/continue_archived_workflow_in_simple"
            ))
            .body(body.to_owned());
        if let Some(content_type) = content_type {
            request = request.header("Content-Type", content_type);
        }
        if let Some(auth) = auth {
            request = request.header("Authorization", auth);
        }
        let response = request.send().await.expect("HTTP response");
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        let value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        handle.abort();
        (status, value)
    }

    #[derive(Debug, PartialEq, Eq)]
    struct SideEffectSnapshot {
        conversations: u64,
        simple_workflows: u64,
        delegation_workflows: u64,
        delegation_runs: u64,
        attention_requests: u64,
        recovery_authorizations: u64,
        auto_title_jobs: u64,
        message_counts: Vec<(i32, i32)>,
    }

    async fn side_effect_snapshot(db: &AppDatabase) -> SideEffectSnapshot {
        SideEffectSnapshot {
            conversations: conversation::Entity::find().count(&db.conn).await.unwrap(),
            simple_workflows: simple_workflow::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_workflows: delegation_workflow::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            delegation_runs: delegation_task_run::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            attention_requests: delegation_attention_request::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            recovery_authorizations: recovery_authorization::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            auto_title_jobs: auto_title_job::Entity::find()
                .count(&db.conn)
                .await
                .unwrap(),
            message_counts: conversation::Entity::find()
                .select_only()
                .columns([conversation::Column::Id, conversation::Column::MessageCount])
                .order_by_asc(conversation::Column::Id)
                .into_tuple::<(i32, i32)>()
                .all(&db.conn)
                .await
                .unwrap(),
        }
    }

    #[tokio::test]
    async fn simple_successor_http_route_requires_server_auth() {
        let data_dir = tempfile::tempdir().unwrap();
        let state = Arc::new(AppState::new_for_test(
            fresh_in_memory_db().await,
            data_dir.path().to_path_buf(),
        ));
        let app = build_router(
            state,
            "secret".into(),
            data_dir.path().to_path_buf(),
            Arc::new(ShutdownSignal::new()),
        );
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind HTTP test listener");
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let response = reqwest::Client::new()
            .post(format!(
                "http://{addr}/api/continue_archived_workflow_in_simple"
            ))
            .header("Content-Type", "application/json")
            .body("{".to_owned())
            .send()
            .await
            .expect("HTTP response");
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        handle.abort();
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn simple_successor_creation_retired_http_ignores_every_authenticated_body() {
        let workspace = tempfile::tempdir().unwrap();
        let db = fresh_in_memory_db().await;
        let folder = seed_folder(&db, workspace.path().to_str().unwrap()).await;
        let ordinary = seed_conversation(&db, folder, AgentType::Codex).await;
        let simple = seed_conversation(&db, folder, AgentType::Codex).await;
        register_simple_workflow(&db.conn, simple, "docs/plan.md", None)
            .await
            .unwrap();
        let archived = seed_conversation(&db, folder, AgentType::Codex).await;
        let archived_child = seed_conversation(&db, folder, AgentType::Codex).await;
        seed_archived_workflow(
            &db,
            archived,
            "workflow-http-retired-successor",
            "docs/missing-plan.md",
            None,
            2,
            CompletionProtocolMode::V2Enforce,
        )
        .await;
        seed_bound_child(
            &db,
            archived,
            archived_child,
            "workflow-http-retired-successor",
        )
        .await;
        let deleted = seed_conversation(&db, folder, AgentType::Codex).await;
        conversation_service::soft_delete(&db.conn, deleted)
            .await
            .unwrap();
        let missing = deleted + 1_000_000;
        let state = Arc::new(AppState::new_for_test(db, workspace.path().to_path_buf()));

        let oversized_token = "x".repeat(257);
        let bodies = vec![
            String::new(),
            "{".into(),
            "null".into(),
            serde_json::json!({}).to_string(),
            serde_json::json!({
                "sourceConversationId": "wrong",
                "clientRequestToken": false,
            })
            .to_string(),
            serde_json::json!({
                "sourceConversationId": 0,
                "clientRequestToken": "",
            })
            .to_string(),
            serde_json::json!({
                "sourceConversationId": -1,
                "clientRequestToken": oversized_token,
                "extra": true,
            })
            .to_string(),
            serde_json::json!({ "sourceConversationId": ordinary, "clientRequestToken": "ordinary" }).to_string(),
            serde_json::json!({ "sourceConversationId": simple, "clientRequestToken": "simple" }).to_string(),
            serde_json::json!({ "sourceConversationId": archived, "clientRequestToken": "archived" }).to_string(),
            serde_json::json!({ "sourceConversationId": archived_child, "clientRequestToken": "archived-child" }).to_string(),
            serde_json::json!({ "sourceConversationId": deleted, "clientRequestToken": "deleted" }).to_string(),
            serde_json::json!({ "sourceConversationId": missing, "clientRequestToken": "missing" }).to_string(),
            // Intentional replay of the archived body: the second call must remain
            // a no-op retirement conflict with no additional side effects.
            serde_json::json!({ "sourceConversationId": archived, "clientRequestToken": "archived" }).to_string(),
        ];

        let before = side_effect_snapshot(&state.db).await;
        let mut receiver = state.event_broadcaster.subscribe();

        for body in &bodies {
            let (status, value) = call_raw(
                state.clone(),
                workspace.path(),
                body,
                Some("application/json"),
                Some("Bearer secret"),
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT);
            assert_eq!(
                value,
                serde_json::json!({
                    "code": "simple_successor_creation_retired",
                    "message": "Automatic Simple successor creation is retired; create a new conversation and use a new Design."
                })
            );
        }

        assert_eq!(side_effect_snapshot(&state.db).await, before);
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }
}
