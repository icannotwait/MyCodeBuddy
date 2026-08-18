//! HTTP projection + viewer-only admission for delegated children.
//!
//! Uses the real Axum router + in-memory DB so the web contract stays locked
//! to the shared `get_delegate_access_core` projection and admission guards.

use std::sync::Arc;

use axum_test::TestServer;
use codeg_lib::acp::delegation::spawner::DelegationLink;
use codeg_lib::app_state::AppState;
use codeg_lib::db::service::conversation_service;
use codeg_lib::db::test_helpers::{fresh_in_memory_db, seed_folder};
use codeg_lib::models::AgentType;
use codeg_lib::web::event_bridge::EventEmitter;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use serde_json::json;

struct LockedChildFixture {
    server: TestServer,
    state: Arc<AppState>,
    parent_id: i32,
    child_id: i32,
}

async fn locked_child_fixture() -> LockedChildFixture {
    let data = tempfile::tempdir().unwrap();
    let static_dir = tempfile::tempdir().unwrap();
    let db = fresh_in_memory_db().await;
    let folder = seed_folder(&db, "/tmp/delegate-access-api").await;
    let parent = conversation_service::create(&db.conn, folder, AgentType::ClaudeCode, None, None)
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
        Arc::clone(&state),
        "token".into(),
        static_dir.path().to_path_buf(),
        Arc::new(ShutdownSignal::new()),
    );
    let server = TestServer::new(router).unwrap();
    LockedChildFixture {
        server,
        state,
        parent_id: parent.id,
        child_id: child.id,
    }
}

async fn bind_child_connection(fixture: &LockedChildFixture, connection_id: &str) {
    fixture
        .state
        .connection_manager
        .insert_test_connection(connection_id, AgentType::Codex, None, EventEmitter::Noop)
        .await;
    fixture
        .state
        .connection_manager
        .get_state(connection_id)
        .await
        .unwrap()
        .write()
        .await
        .conversation_id = Some(fixture.child_id);
}

#[tokio::test]
async fn web_endpoint_returns_the_shared_projection() {
    let fixture = locked_child_fixture().await;
    let response = fixture
        .server
        .post("/api/get_delegate_access")
        .add_header("authorization", "Bearer token")
        .json(&json!({ "conversationId": fixture.child_id }))
        .await;
    response.assert_status_ok();
    response.assert_json(&json!({
        "mode": "viewer_only",
        "reason": "task_running",
        "parent_id": fixture.parent_id,
    }));
}

#[tokio::test]
async fn locked_mutations_return_409_delegate_viewer_only_permission_exempt() {
    let fixture = locked_child_fixture().await;
    bind_child_connection(&fixture, "child-live").await;

    // answer_question admission keys off the pending question owner, not the
    // caller connection_id alone — register a real parked ask so 409 applies.
    let pending_q = fixture
        .state
        .connection_manager
        .register_question(
            "child-live",
            vec![codeg_lib::acp::question::QuestionSpec {
                id: "qa".into(),
                question: "Locked child question?".into(),
                header: "Lock".into(),
                multi_select: false,
                options: vec![
                    codeg_lib::acp::question::QuestionOption {
                        label: "A".into(),
                        description: String::new(),
                    },
                    codeg_lib::acp::question::QuestionOption {
                        label: "B".into(),
                        description: String::new(),
                    },
                ],
                is_secret: false,
                recovery: None,
            }],
        )
        .await
        .expect("pending question on locked child");

    let guarded = fixture
        .server
        .post("/api/acp_set_mode")
        .add_header("authorization", "Bearer token")
        .json(&json!({ "connectionId": "child-live", "modeId": "plan" }))
        .await;
    assert_eq!(guarded.status_code(), 409);
    assert_eq!(
        guarded.json::<serde_json::Value>()["code"],
        "delegate_viewer_only"
    );

    for (path, body) in [
        (
            "/api/acp_set_config_option",
            json!({
                "connectionId": "child-live",
                "configId": "model",
                "valueId": "x"
            }),
        ),
        ("/api/acp_cancel", json!({ "connectionId": "child-live" })),
        (
            "/api/submit_session_feedback",
            json!({ "connectionId": "child-live", "text": "nudge" }),
        ),
        (
            "/api/acp_answer_question",
            json!({
                "connectionId": "child-live",
                "questionId": pending_q.question_id,
                "answer": { "answers": [], "declined": true }
            }),
        ),
        (
            "/api/acp_prompt",
            json!({
                "connectionId": "child-live",
                "blocks": [{ "type": "text", "text": "hi" }],
                "conversationId": fixture.child_id
            }),
        ),
        (
            "/api/acp_fork",
            json!({
                "connectionId": "child-live",
                "conversationId": fixture.child_id
            }),
        ),
    ] {
        let response = fixture
            .server
            .post(path)
            .add_header("authorization", "Bearer token")
            .json(&body)
            .await;
        assert_eq!(
            response.status_code(),
            409,
            "{path} should be 409; body={}",
            response.text()
        );
        assert_eq!(
            response.json::<serde_json::Value>()["code"],
            "delegate_viewer_only",
            "{path}"
        );
    }

    let permission = fixture
        .server
        .post("/api/acp_respond_permission")
        .add_header("authorization", "Bearer token")
        .json(&json!({
            "connectionId": "child-live",
            "requestId": "missing",
            "optionId": "allow"
        }))
        .await;
    assert_ne!(
        permission.json::<serde_json::Value>()["code"],
        "delegate_viewer_only"
    );
}

#[tokio::test]
async fn answer_question_with_interactive_connection_id_and_locked_owner_returns_409() {
    // Critical: answer_question routes by question_id, ignoring caller
    // connection_id. Guarding only the caller id would allow any interactive
    // connection to answer a locked-delegate pending question.
    let fixture = locked_child_fixture().await;
    fixture
        .state
        .connection_manager
        .insert_test_connection(
            "parent-live",
            AgentType::ClaudeCode,
            None,
            EventEmitter::Noop,
        )
        .await;
    fixture
        .state
        .connection_manager
        .get_state("parent-live")
        .await
        .unwrap()
        .write()
        .await
        .conversation_id = Some(fixture.parent_id);
    bind_child_connection(&fixture, "child-live").await;

    let reg = fixture
        .state
        .connection_manager
        .register_question(
            "child-live",
            vec![codeg_lib::acp::question::QuestionSpec {
                id: "qa".into(),
                question: "Which approach?".into(),
                header: "Approach".into(),
                multi_select: false,
                options: vec![
                    codeg_lib::acp::question::QuestionOption {
                        label: "A".into(),
                        description: String::new(),
                    },
                    codeg_lib::acp::question::QuestionOption {
                        label: "B".into(),
                        description: String::new(),
                    },
                ],
                is_secret: false,
                recovery: None,
            }],
        )
        .await
        .expect("pending question on locked child");

    let response = fixture
        .server
        .post("/api/acp_answer_question")
        .add_header("authorization", "Bearer token")
        .json(&json!({
            "connectionId": "parent-live",
            "questionId": reg.question_id,
            "answer": { "answers": [], "declined": true }
        }))
        .await;
    assert_eq!(
        response.status_code(),
        409,
        "must reject via question owner; body={}",
        response.text()
    );
    assert_eq!(
        response.json::<serde_json::Value>()["code"],
        "delegate_viewer_only"
    );

    // Rejection must leave the question pending (not consume the one-shot).
    assert_eq!(
        fixture
            .state
            .connection_manager
            .pending_question_parent_connection_id(&reg.question_id)
            .await
            .as_deref(),
        Some("child-live")
    );
    assert!(
        fixture
            .state
            .connection_manager
            .get_state("child-live")
            .await
            .unwrap()
            .read()
            .await
            .pending_question
            .is_some(),
        "pending card must remain after rejected answer"
    );
    // Receiver still parked (not resolved by the rejected HTTP attempt).
    assert!(
        !reg.answer_rx.is_terminated(),
        "oneshot must still be pending"
    );
}

#[tokio::test]
async fn unbound_prompt_and_fork_with_locked_conversation_id_return_409() {
    let fixture = locked_child_fixture().await;
    fixture
        .state
        .connection_manager
        .insert_test_connection("unbound", AgentType::Codex, None, EventEmitter::Noop)
        .await;
    // Leave conversation_id unbound — request supplies locked child id.

    let prompt = fixture
        .server
        .post("/api/acp_prompt")
        .add_header("authorization", "Bearer token")
        .json(&json!({
            "connectionId": "unbound",
            "blocks": [{ "type": "text", "text": "hi" }],
            "conversationId": fixture.child_id
        }))
        .await;
    assert_eq!(prompt.status_code(), 409);
    assert_eq!(
        prompt.json::<serde_json::Value>()["code"],
        "delegate_viewer_only"
    );

    let fork = fixture
        .server
        .post("/api/acp_fork")
        .add_header("authorization", "Bearer token")
        .json(&json!({
            "connectionId": "unbound",
            "conversationId": fixture.child_id
        }))
        .await;
    assert_eq!(fork.status_code(), 409);
    assert_eq!(
        fork.json::<serde_json::Value>()["code"],
        "delegate_viewer_only"
    );
}

#[tokio::test]
async fn connect_omitted_conversation_id_with_locked_session_rejects_without_spawn() {
    let fixture = locked_child_fixture().await;
    conversation_service::update_external_id(
        &fixture.state.db.conn,
        fixture.child_id,
        "locked-child-session".into(),
    )
    .await
    .unwrap();

    let before = fixture
        .state
        .connection_manager
        .list_connections()
        .await
        .len();

    let response = fixture
        .server
        .post("/api/acp_connect_or_attach")
        .add_header("authorization", "Bearer token")
        .json(&json!({
            "agentType": "codex",
            "externalSessionId": "locked-child-session",
            "deviceId": "delegate-device",
            "clientInstanceId": "delegate-client",
            "requestId": "locked-session-connect"
        }))
        .await;
    assert_eq!(response.status_code(), 409);
    assert_eq!(
        response.json::<serde_json::Value>()["code"],
        "delegate_viewer_only"
    );

    let after = fixture
        .state
        .connection_manager
        .list_connections()
        .await
        .len();
    assert_eq!(before, after, "admission must reject before process spawn");
}

#[tokio::test]
async fn connect_mismatched_conversation_and_session_rejects_without_spawn() {
    let fixture = locked_child_fixture().await;
    conversation_service::update_external_id(
        &fixture.state.db.conn,
        fixture.child_id,
        "child-session-x".into(),
    )
    .await
    .unwrap();
    let folder = seed_folder(&fixture.state.db, "/tmp/delegate-access-api-mismatch").await;
    let other =
        conversation_service::create(&fixture.state.db.conn, folder, AgentType::Codex, None, None)
            .await
            .unwrap();

    let before = fixture
        .state
        .connection_manager
        .list_connections()
        .await
        .len();

    let response = fixture
        .server
        .post("/api/acp_connect_or_attach")
        .add_header("authorization", "Bearer token")
        .json(&json!({
            "agentType": "codex",
            "externalSessionId": "child-session-x",
            "conversationId": other.id,
            "deviceId": "delegate-device",
            "clientInstanceId": "delegate-client",
            "requestId": "mismatched-session-connect"
        }))
        .await;
    assert_eq!(response.status_code(), 409);
    assert_eq!(
        response.json::<serde_json::Value>()["code"],
        "delegate_viewer_only"
    );

    let after = fixture
        .state
        .connection_manager
        .list_connections()
        .await
        .len();
    assert_eq!(before, after, "mismatch must reject before process spawn");
}
