//! HTTP projection for delegated-child access state.
//!
//! Uses the real Axum router + in-memory DB so the web contract stays locked
//! to the shared `get_delegate_access_core` projection.

use std::sync::Arc;

use axum_test::TestServer;
use codeg_lib::acp::delegation::spawner::DelegationLink;
use codeg_lib::app_state::AppState;
use codeg_lib::db::service::conversation_service;
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_folder};
use codeg_lib::models::AgentType;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use serde_json::json;

#[tokio::test]
async fn web_endpoint_returns_the_shared_projection() {
    let data = tempfile::tempdir().unwrap();
    let static_dir = tempfile::tempdir().unwrap();
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/delegate-access-api").await;
    let parent = conversation_service::create(
        &db.conn,
        folder,
        AgentType::ClaudeCode,
        None,
        None,
    )
    .await
    .unwrap();
    let child = conversation_service::create_with_delegation(
        &db.conn,
        folder,
        AgentType::Codex,
        None,
        None,
        Some(DelegationLink {
            parent_conversation_id: parent.id,
            parent_tool_use_id: "tool".into(),
            delegation_call_id: "task".into(),
        }),
    )
    .await
    .unwrap();
    let state = Arc::new(AppState::new_for_test(db, data.path().to_path_buf()));
    let router = build_router(
        state,
        "token".into(),
        static_dir.path().to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    );
    let server = TestServer::new(router).unwrap();
    let response = server
        .post("/api/get_delegate_access")
        .add_header("authorization", "Bearer token")
        .json(&json!({ "conversationId": child.id }))
        .await;
    response.assert_status_ok();
    response.assert_json(&json!({
        "mode": "viewer_only",
        "reason": "task_running",
        "parent_id": parent.id,
    }));
}
