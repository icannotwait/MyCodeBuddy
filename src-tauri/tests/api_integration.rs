//! HTTP API integration tests.
//!
//! Builds the real Axum router from `web::router::build_router`, wired to an
//! in-memory SQLite database (`fresh_in_memory_db`) and a `WebOnly` event
//! emitter (`EventEmitter::test_web_only`). Drives requests through
//! `axum-test::TestServer` so no TCP socket is involved.
//!
//! Scope of this first pass:
//! - Authentication matrix on a representative protected endpoint
//! - Public endpoint (`get_system_language_settings`) reachable without token
//! - One DB-backed endpoint (`list_folders`) returns expected JSON shape
//!
//! Not covered: WebSocket attach (separate concern), endpoints that touch the
//! Tauri webview (those are gated behind `tauri-runtime`).

use std::sync::Arc;

use axum_test::TestServer;
use codeg_lib::app_state::AppState;
use codeg_lib::db::test_helpers::fresh_in_memory_db;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use serde_json::{json, Value};

const TEST_TOKEN: &str = "integration-test-token";

async fn build_test_server() -> (TestServer, tempfile::TempDir, tempfile::TempDir) {
    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");

    let db = fresh_in_memory_db().await;
    let state = Arc::new(AppState::new_for_test(db, data_dir.path().to_path_buf()));
    let shutdown = Arc::new(ShutdownSignal::new());

    let router = build_router(
        state,
        TEST_TOKEN.to_string(),
        static_dir.path().to_path_buf(),
        shutdown,
    );

    let server = TestServer::new(router).expect("test server");
    // Keep data_dir and static_dir alive for the whole test by returning them.
    (server, data_dir, static_dir)
}

// ────────────────────────────────────────────────────────────────────────────
// Auth matrix
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn protected_endpoint_rejects_missing_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server.post("/api/list_folders").json(&json!({})).await;
    assert_eq!(resp.status_code(), 401);
}

#[tokio::test]
async fn protected_endpoint_rejects_wrong_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/list_folders")
        .add_header("authorization", "Bearer wrong-token")
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 401);
}

#[tokio::test]
async fn protected_endpoint_accepts_correct_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/list_folders")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
}

#[tokio::test]
async fn grok_session_image_route_rejects_missing_authentication() {
    let (server, _data, _static) = build_test_server().await;
    let response = server
        .post("/api/resolve_grok_session_image")
        .json(&json!({
            "conversationId": 999,
            "href": "images/a.png",
            "includeData": false
        }))
        .await;
    assert_eq!(response.status_code(), 401);
}

#[tokio::test]
async fn grok_session_image_route_returns_structured_not_found_before_filesystem_lookup() {
    let (server, _data, _static) = build_test_server().await;
    let response = server
        .post("/api/resolve_grok_session_image")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({
            "conversationId": 999,
            "href": "images/a.png",
            "includeData": false
        }))
        .await;
    assert_eq!(response.status_code(), 404);
    let body: Value = response.json();
    assert_eq!(body["code"], "not_found");
    assert!(body.get("dataBase64").is_none());
}

#[tokio::test]
async fn repaired_commands_are_registered_in_web_runtime() {
    let (server, _data, _static) = build_test_server().await;
    let workspace = tempfile::tempdir().expect("workspace");

    let state_response = server
        .post("/api/acp_pi_project_trust_state")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({
            "workspace": workspace.path().to_string_lossy(),
        }))
        .await;
    assert_eq!(
        state_response.status_code(),
        200,
        "acp_pi_project_trust_state: {}",
        state_response.text()
    );

    let list_response = server
        .post("/api/acp_pi_list_trust_entries")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(
        list_response.status_code(),
        200,
        "acp_pi_list_trust_entries: {}",
        list_response.text()
    );

    for route in [
        "acp_pi_set_project_trust",
        "acp_pi_acknowledge_project_trust",
        "get_folder_conversation_turns",
        "git_update_branch",
        "git_remove_worktree",
    ] {
        let response = server
            .post(&format!("/api/{route}"))
            .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
            .json(&json!({}))
            .await;
        assert_ne!(response.status_code(), 404, "missing Web route: {route}");
        assert_ne!(
            response.status_code(),
            501,
            "Web route reached fallback: {route}"
        );
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Public endpoint
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn public_language_settings_reachable_without_token() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/get_system_language_settings")
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    // Shape contract: returns a JSON object (exact fields vary by default).
    assert!(body.is_object(), "expected object body, got {body}");
}

// ────────────────────────────────────────────────────────────────────────────
// DB-backed endpoint
// ────────────────────────────────────────────────────────────────────────────

// Note: `/api/list_folders` invokes every parser against the *real* user home
// directory, so it can't be asserted to-be-empty without elaborate filesystem
// isolation. We test DB-backed endpoints (`load_folder_history`,
// `list_open_folders`) instead — those only touch the in-memory SQLite.

#[tokio::test]
async fn load_folder_history_returns_empty_array_on_fresh_db() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/load_folder_history")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(
        body.as_array().expect("array body").len(),
        0,
        "fresh DB should have no folder history"
    );
}

#[tokio::test]
async fn open_folder_then_list_open_folders_shows_it() {
    let (server, _data, _static) = build_test_server().await;
    let open_resp = server
        .post("/api/open_folder")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({"path": "/tmp/codeg-test-folder"}))
        .await;
    assert_eq!(
        open_resp.status_code(),
        200,
        "open_folder failed: {}",
        open_resp.text()
    );

    let list_resp = server
        .post("/api/list_open_folders")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(list_resp.status_code(), 200);
    let body: Value = list_resp.json();
    let arr = body.as_array().expect("array");
    assert_eq!(
        arr.len(),
        1,
        "list_open_folders should reflect the open_folder call, got {body}"
    );
}

#[tokio::test]
async fn acp_find_connection_for_conversation_returns_null_when_none_live() {
    // No live ACP connection is bound to any conversation on a fresh server, so
    // discovery returns JSON `null` (Option::None) with 200 — the frontend
    // reads this as "no live owner, open the persisted detail instead".
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/acp_find_connection_for_conversation")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({"conversationId": 999, "agentType": "claude_code"}))
        .await;
    assert_eq!(resp.status_code(), 200, "body: {}", resp.text());
    let body: Value = resp.json();
    assert!(
        body.is_null(),
        "expected null for an unbound conversation, got {body}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// Field naming sanity (snake_case ↔ camelCase boundary)
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_endpoint_returns_status_field() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/health")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn unknown_endpoint_returns_501_with_typed_error() {
    let (server, _data, _static) = build_test_server().await;
    let resp = server
        .post("/api/this_endpoint_does_not_exist")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 501);
    let body: Value = resp.json();
    assert_eq!(body["code"], "not_implemented");
    assert!(body["message"].is_string());
}

// ────────────────────────────────────────────────────────────────────────────
// Live feedback settings + submit gate
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn feedback_settings_round_trip_defaults_off() {
    let (server, _data, _static) = build_test_server().await;
    // Default is OFF (opt-in feature).
    let resp = server
        .post("/api/get_feedback_settings")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<Value>()["enabled"], false);

    // Enable it.
    let resp = server
        .post("/api/set_feedback_settings")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "settings": { "enabled": true } }))
        .await;
    assert_eq!(resp.status_code(), 200);
    assert_eq!(resp.json::<Value>()["enabled"], true);

    // Reads back enabled.
    let resp = server
        .post("/api/get_feedback_settings")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.json::<Value>()["enabled"], true);
}

// The submit gate is per-connection (the agent's actual `check_user_feedback`
// capability), unit-tested in `ConnectionManager::submit_feedback`
// (`submit_feedback_rejected_when_tool_unavailable`), not via the global setting.

// ────────────────────────────────────────────────────────────────────────────
// Automatic conversation titles (Task 9 end-to-end)
// ────────────────────────────────────────────────────────────────────────────

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use codeg_lib::acp::delegation::spawner::DelegationLink;
use codeg_lib::acp::lifecycle::lifecycle_subscriber_task;
use codeg_lib::acp::types::{AcpEvent, EventEnvelope, PromptInputBlock};
use codeg_lib::auto_title::title_key::{self, title_key_fingerprint, TitleKeyState};
use codeg_lib::auto_title::title_settings::{
    ApiKeyUpdate, KEY_AUTO_TITLE_API_KEY_FP, KEY_AUTO_TITLE_API_URL, KEY_AUTO_TITLE_CONFIG_BARRIER,
    KEY_AUTO_TITLE_CONFIG_GEN, KEY_AUTO_TITLE_MODEL,
};
use codeg_lib::auto_title::{capture_prompt_context, TitleAgentRunner, TurnCompletionSnapshot};
use codeg_lib::auto_title::{AutoTitleAttempt, AutoTitleRunError};
use codeg_lib::commands::conversation_experience::{
    get_conversation_experience_settings_core, set_auto_title_api_config_core,
    set_document_translate_agent_core, CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT,
};
use codeg_lib::db::entities::conversation;
use codeg_lib::db::service::app_metadata_service;
use codeg_lib::db::service::conversation_service::{create, create_with_delegation};
use codeg_lib::db::test_helpers::seed_folder;
use codeg_lib::models::agent::AgentType;
use codeg_lib::models::system::AppLocale;
use sea_orm::EntityTrait;
use tokio_util::sync::CancellationToken;

/// Deterministic title runner for integration tests — never launches a CLI.
struct CountingTitleRunner {
    calls: AtomicU64,
    title_prefix: String,
}

impl CountingTitleRunner {
    fn new(title_prefix: impl Into<String>) -> Self {
        Self {
            calls: AtomicU64::new(0),
            title_prefix: title_prefix.into(),
        }
    }

    fn call_count(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TitleAgentRunner for CountingTitleRunner {
    async fn run(
        &self,
        attempt: AutoTitleAttempt,
        _cancellation: CancellationToken,
    ) -> Result<String, AutoTitleRunError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!(
            "{}-{}-{}",
            self.title_prefix, attempt.conversation_id, n
        ))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn automatic_title_root_and_delegated_child_update_once_without_updated_at() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let db = fresh_in_memory_db().await;
    let runner = Arc::new(CountingTitleRunner::new("auto"));
    let state = Arc::new(AppState::new_for_test_with_title_runner(
        db,
        data_dir.path().to_path_buf(),
        runner.clone() as Arc<dyn TitleAgentRunner>,
    ));

    // Exclusive suite so Present overrides apply on this current_thread runtime
    // (coordinator workers share the suite-owner TLS).
    let _suite = title_key::test_hooks::SuiteGuard::enter();
    for _ in 0..128 {
        title_key::test_hooks::push_override_get(TitleKeyState::Present(
            "sk-integration-auto-title".into(),
        ));
    }
    let fp = title_key_fingerprint("sk-integration-auto-title");
    app_metadata_service::upsert_value(
        &state.db.conn,
        KEY_AUTO_TITLE_API_URL,
        "https://api.example.com/v1",
    )
    .await
    .expect("url");
    app_metadata_service::upsert_value(&state.db.conn, KEY_AUTO_TITLE_MODEL, "gpt-4o-mini")
        .await
        .expect("model");
    app_metadata_service::upsert_value(&state.db.conn, KEY_AUTO_TITLE_API_KEY_FP, &fp)
        .await
        .expect("fp");
    app_metadata_service::upsert_value(&state.db.conn, KEY_AUTO_TITLE_CONFIG_BARRIER, "0")
        .await
        .expect("barrier");
    app_metadata_service::upsert_value(&state.db.conn, KEY_AUTO_TITLE_CONFIG_GEN, "1")
        .await
        .expect("gen");

    // Lifecycle subscriber + coordinator worker (no attached clients).
    tokio::spawn(lifecycle_subscriber_task(
        state.db.conn.clone(),
        state.connection_manager.clone_ref(),
        Arc::clone(&state.acp_event_bus),
        None,
    ));
    state
        .auto_title_coordinator
        .recover_and_start()
        .await
        .expect("start coordinator");

    let folder_id = seed_folder(&state.db, "/tmp/auto-title-e2e").await;
    let root = create(&state.db.conn, folder_id, AgentType::ClaudeCode, None, None)
        .await
        .expect("root");
    let child = create_with_delegation(
        &state.db.conn,
        folder_id,
        AgentType::Gemini,
        Some("child".into()),
        None,
        Some(DelegationLink {
            parent_conversation_id: root.id,
            parent_tool_use_id: "tool-e2e".into(),
            delegation_call_id: "call-e2e".into(),
        }),
    )
    .await
    .expect("child");

    let blocks = vec![PromptInputBlock::Text {
        text: "wire".into(),
    }];
    for id in [root.id, child.id] {
        capture_prompt_context(
            &state.db.conn,
            id,
            &blocks,
            Some(&codeg_lib::auto_title::PromptCaptureContext::new(
                Some(format!("task-{id}")),
                Some(AppLocale::En),
            )),
            AppLocale::En,
        )
        .await
        .expect("capture");
    }

    for (conn_id, conv_id, token) in [
        ("root-conn", root.id, "tok-root"),
        ("child-conn", child.id, "tok-child"),
    ] {
        let env = Arc::new(EventEnvelope {
            seq: 1,
            connection_id: conn_id.into(),
            payload: AcpEvent::TurnComplete {
                session_id: format!("sess-{conv_id}"),
                stop_reason: "end_turn".into(),
                agent_type: "claude_code".into(),
                mark_awaiting_reply: true,

                termination_source: None,
                provider_turn_id: None,
            },
        });
        let completion = Arc::new(TurnCompletionSnapshot {
            conversation_id: conv_id,
            turn_token: token.into(),
            locale: AppLocale::En,
            final_text: Arc::from(format!("assistant reply for {conv_id}")),
        });
        state
            .acp_event_bus
            .send_with_completion(env, Some(completion));
    }

    // Wait until lifecycle has applied usable completion (jobs ready) and
    // root status CAS has settled — that may bump updated_at. Snapshot
    // updated_at after the status write so title finalize can be checked
    // as non-mutating for that column.
    let settle_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let (root_post_status, child_post_status) = loop {
        let root_row = conversation::Entity::find_by_id(root.id)
            .one(&state.db.conn)
            .await
            .unwrap()
            .unwrap();
        let child_row = conversation::Entity::find_by_id(child.id)
            .one(&state.db.conn)
            .await
            .unwrap()
            .unwrap();
        // Root leaves InProgress on end_turn; delegate keeps broker-owned status.
        let root_settled = root_row.status
            != codeg_lib::db::entities::conversation::ConversationStatus::InProgress;
        if root_settled || tokio::time::Instant::now() > settle_deadline {
            break (root_row.updated_at, child_row.updated_at);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let root_row = conversation::Entity::find_by_id(root.id)
            .one(&state.db.conn)
            .await
            .unwrap()
            .unwrap();
        let child_row = conversation::Entity::find_by_id(child.id)
            .one(&state.db.conn)
            .await
            .unwrap()
            .unwrap();
        if root_row.auto_title_finalized && child_row.auto_title_finalized {
            // Titles are deterministic: prefix-conversationId-callN (order may
            // vary for concurrent claims; accept either call ordinal).
            assert!(
                root_row
                    .title
                    .as_deref()
                    .is_some_and(|t| t.starts_with(&format!("auto-{}-", root.id))),
                "root title {:?}",
                root_row.title
            );
            assert!(
                child_row
                    .title
                    .as_deref()
                    .is_some_and(|t| t.starts_with(&format!("auto-{}-", child.id))),
                "child title {:?}",
                child_row.title
            );
            // finalize_generated_title must not bump conversation.updated_at.
            assert_eq!(root_row.updated_at, root_post_status);
            assert_eq!(child_row.updated_at, child_post_status);
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!(
                "titles not finalized in time; root={:?} child={:?} calls={}",
                root_row.title,
                child_row.title,
                runner.call_count()
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(runner.call_count(), 2, "each conversation titles once");
}

#[tokio::test(flavor = "current_thread")]
async fn conversation_experience_settings_http_round_trip() {
    // Isolate keyring reads: ambient OS Present must not flip key_set on GET.
    // current_thread + SuiteGuard so axum-test handlers share suite-owner TLS.
    // Override queue is FIFO — queue only what each phase needs (no leftover Absent).
    let _suite = title_key::test_hooks::SuiteGuard::enter();

    let (server, _data, _static) = build_test_server().await;

    // Phase 1: default GET with Absent key (exactly one get_title_api_key read).
    title_key::test_hooks::push_override_get(TitleKeyState::Absent);
    let resp = server
        .post("/api/get_conversation_experience_settings")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    // Task 5: legacy auto_title_agent removed from the wire document.
    assert!(body.get("auto_title_agent").is_none());
    assert_eq!(body["auto_title_api_url"], "");
    assert_eq!(body["auto_title_api_key_set"], false);
    assert_eq!(body["auto_title_model"], "");
    assert_eq!(body["auto_title_config_barrier"], false);
    assert_eq!(body["document_translate_agent"], Value::Null);
    assert_eq!(body["reference_search_limit"], 50);
    assert_eq!(body["revision"], 0);
    // Secret never present on GET.
    assert!(body.get("auto_title_api_key").is_none());
    assert!(body.get("api_key").is_none());

    // Old command removed — no temporary alias (web fallback → 501 not_implemented).
    let resp = server
        .post("/api/set_auto_title_agent")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "agent": null }))
        .await;
    assert_eq!(resp.status_code(), 501);
    let body: Value = resp.json();
    assert_eq!(body["code"], "not_implemented");

    // Phase 2: new set_auto_title_api_config shape (Keep + Present key overrides).
    // Reads: preflight, barrier load_settings, verify, commit load_settings, post-commit.
    // Queue is FIFO and shared — do not leave leftover Absent from earlier phases.
    for _ in 0..16 {
        title_key::test_hooks::push_override_get(TitleKeyState::Present(
            "sk-http-round-trip".into(),
        ));
    }
    let resp = server
        .post("/api/set_auto_title_api_config")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({
            "api_url": "https://api.example.com/v1",
            "api_key_update": { "keep": true },
            "model": "gpt-4o-mini",
        }))
        .await;
    assert_eq!(
        resp.status_code(),
        200,
        "set_auto_title_api_config body: {:?}",
        resp.text()
    );
    let body: Value = resp.json();
    assert!(body.get("auto_title_agent").is_none());
    assert_eq!(body["auto_title_api_url"], "https://api.example.com/v1");
    assert_eq!(body["auto_title_api_key_set"], true, "set response: {body}");
    assert_eq!(body["auto_title_model"], "gpt-4o-mini");
    assert_eq!(body["auto_title_config_barrier"], false);
    assert_eq!(body["document_translate_agent"], Value::Null);
    assert!(body["revision"].as_u64().unwrap() >= 1);
    // Secret never present on set response.
    assert!(body.get("auto_title_api_key").is_none());
    assert!(body.get("api_key").is_none());
    assert!(body.get("api_key_update").is_none());
    // Response must not contain the secret value either.
    let body_text = body.to_string();
    assert!(
        !body_text.contains("sk-http-round-trip"),
        "secret leaked in set response JSON"
    );
    let title_revision = body["revision"].as_u64().unwrap();

    // Phase 3: translate Off must not clear title URL/model/key_set (independent paths).
    // load_settings reads key once per response.
    title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-http-round-trip".into()));
    let resp = server
        .post("/api/set_document_translate_agent")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "agent": null }))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert!(body.get("auto_title_agent").is_none());
    assert_eq!(body["document_translate_agent"], Value::Null);
    assert_eq!(body["auto_title_api_url"], "https://api.example.com/v1");
    assert_eq!(body["auto_title_model"], "gpt-4o-mini");
    assert_eq!(body["auto_title_api_key_set"], true);
    assert!(body.get("auto_title_api_key").is_none());
    assert!(body.get("api_key").is_none());
    assert!(
        body["revision"].as_u64().unwrap() > title_revision,
        "translate set must bump revision independently"
    );

    // Phase 4: GET after both mutations still omits secrets and keeps title fields.
    title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-http-round-trip".into()));
    let resp = server
        .post("/api/get_conversation_experience_settings")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({}))
        .await;
    assert_eq!(resp.status_code(), 200);
    let body: Value = resp.json();
    assert_eq!(body["auto_title_api_url"], "https://api.example.com/v1");
    assert_eq!(body["auto_title_model"], "gpt-4o-mini");
    assert_eq!(body["auto_title_api_key_set"], true);
    assert_eq!(body["document_translate_agent"], Value::Null);
    assert!(body.get("auto_title_api_key").is_none());
    assert!(body.get("api_key").is_none());
    assert!(body.get("auto_title_agent").is_none());
    assert!(!body.to_string().contains("sk-http-round-trip"));
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_auto_title_saves_hold_the_gate_through_off_cancellation() {
    let data_dir = tempfile::tempdir().expect("data dir");
    let db = fresh_in_memory_db().await;
    let runner = Arc::new(CountingTitleRunner::new("gate"));
    let state = Arc::new(AppState::new_for_test_with_title_runner(
        db,
        data_dir.path().to_path_buf(),
        runner.clone() as Arc<dyn TitleAgentRunner>,
    ));

    // Enable via metadata + suite overrides (no OS keyring dependency).
    let _suite = title_key::test_hooks::SuiteGuard::enter();
    for _ in 0..128 {
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-gate-title".into()));
    }
    let fp = title_key_fingerprint("sk-gate-title");
    app_metadata_service::upsert_value(
        &state.db.conn,
        KEY_AUTO_TITLE_API_URL,
        "https://api.example.com/v1",
    )
    .await
    .expect("url");
    app_metadata_service::upsert_value(&state.db.conn, KEY_AUTO_TITLE_MODEL, "gpt-4o-mini")
        .await
        .expect("model");
    app_metadata_service::upsert_value(&state.db.conn, KEY_AUTO_TITLE_API_KEY_FP, &fp)
        .await
        .expect("fp");
    app_metadata_service::upsert_value(&state.db.conn, KEY_AUTO_TITLE_CONFIG_BARRIER, "0")
        .await
        .expect("barrier");
    app_metadata_service::upsert_value(&state.db.conn, KEY_AUTO_TITLE_CONFIG_GEN, "1")
        .await
        .expect("gen");

    let mut broadcaster_rx = state.event_broadcaster.subscribe();

    let (arrival, release) = state
        .auto_title_coordinator
        .pause_next_cancel_all_before_effect()
        .await;

    let gate = Arc::clone(&state.conversation_experience_gate);
    let coord = Arc::clone(&state.auto_title_coordinator);
    let emitter = state.emitter.clone();
    let app_db = codeg_lib::db::AppDatabase {
        conn: state.db.conn.clone(),
    };

    // Off: Keep with Present override → empty URL/model + cancel_all under gate.
    // Use Keep (not Clear/Set) so we never touch the process OS keyring.
    for _ in 0..32 {
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-gate-title".into()));
    }
    let off_task = tokio::spawn({
        let gate = Arc::clone(&gate);
        let coord = Arc::clone(&coord);
        let emitter = emitter.clone();
        async move {
            set_auto_title_api_config_core(
                &app_db,
                &emitter,
                &coord,
                &gate,
                String::new(),
                ApiKeyUpdate::Keep,
                String::new(),
            )
            .await
        }
    });

    tokio::time::timeout(Duration::from_secs(2), arrival)
        .await
        .expect("off cancel_all arrival")
        .expect("arrival oneshot");

    let app_db_on = codeg_lib::db::AppDatabase {
        conn: state.db.conn.clone(),
    };
    for _ in 0..32 {
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-gate-title".into()));
    }
    let mut on_task = tokio::spawn({
        let gate = Arc::clone(&gate);
        let coord = Arc::clone(&coord);
        let emitter = emitter.clone();
        async move {
            set_auto_title_api_config_core(
                &app_db_on,
                &emitter,
                &coord,
                &gate,
                "https://api.example.com/v1".into(),
                ApiKeyUpdate::Keep,
                "gpt-4o-mini".into(),
            )
            .await
        }
    });

    let early = tokio::time::timeout(Duration::from_millis(50), &mut on_task).await;
    assert!(
        early.is_err(),
        "On must still be pending while Off holds the gate through cancel_all"
    );

    release.send(()).expect("release off cancel_all");

    let off_result = off_task.await.expect("join off").expect("off ok");
    let on_result = on_task.await.expect("join on").expect("on ok");

    assert!(
        on_result.revision > off_result.revision,
        "revisions must be monotonic with On last: off={} on={}",
        off_result.revision,
        on_result.revision
    );
    assert_eq!(on_result.auto_title_api_url, "https://api.example.com/v1");
    assert_eq!(on_result.auto_title_model, "gpt-4o-mini");
    assert!(!on_result.auto_title_config_barrier);

    let mut last_revision = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        match broadcaster_rx.try_recv() {
            Ok(evt) if evt.channel == CONVERSATION_EXPERIENCE_SETTINGS_CHANGED_EVENT => {
                let rev = evt.payload["revision"].as_u64().unwrap_or(0);
                assert!(rev >= last_revision, "event revisions must be monotonic");
                last_revision = rev;
                assert!(evt.payload.get("auto_title_api_key").is_none());
                assert!(evt.payload.get("api_key").is_none());
                assert!(evt.payload.get("auto_title_agent").is_none());
            }
            Ok(_) => {}
            Err(_) => {
                if last_revision == on_result.revision {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
    assert_eq!(last_revision, on_result.revision);

    for _ in 0..64 {
        title_key::test_hooks::push_override_get(TitleKeyState::Present("sk-gate-title".into()));
    }
    state
        .auto_title_coordinator
        .recover_and_start()
        .await
        .expect("start");
    tokio::spawn(lifecycle_subscriber_task(
        state.db.conn.clone(),
        state.connection_manager.clone_ref(),
        Arc::clone(&state.acp_event_bus),
        None,
    ));

    let folder_id = seed_folder(&state.db, "/tmp/auto-title-gate").await;
    let conv = create(&state.db.conn, folder_id, AgentType::ClaudeCode, None, None)
        .await
        .expect("conv");
    capture_prompt_context(
        &state.db.conn,
        conv.id,
        &[PromptInputBlock::Text {
            text: "wire".into(),
        }],
        Some(&codeg_lib::auto_title::PromptCaptureContext::new(
            Some("gate task".into()),
            Some(AppLocale::En),
        )),
        AppLocale::En,
    )
    .await
    .expect("capture");

    let env = Arc::new(EventEnvelope {
        seq: 1,
        connection_id: "gate-conn".into(),
        payload: AcpEvent::TurnComplete {
            session_id: "sess-gate".into(),
            stop_reason: "end_turn".into(),
            agent_type: "claude_code".into(),
            mark_awaiting_reply: true,

            termination_source: None,
            provider_turn_id: None,
        },
    });
    let completion = Arc::new(TurnCompletionSnapshot {
        conversation_id: conv.id,
        turn_token: "tok-gate".into(),
        locale: AppLocale::En,
        final_text: Arc::from("assistant for gate"),
    });
    state
        .acp_event_bus
        .send_with_completion(env, Some(completion));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let row = conversation::Entity::find_by_id(conv.id)
            .one(&state.db.conn)
            .await
            .unwrap()
            .unwrap();
        if row.auto_title_finalized {
            assert!(
                row.title
                    .as_deref()
                    .is_some_and(|t| t.starts_with(&format!("gate-{}-", conv.id))),
                "title not cancelled: {:?}",
                row.title
            );
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("runner cancelled or never ran; title={:?}", row.title);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let loaded = get_conversation_experience_settings_core(&state.db.conn)
        .await
        .expect("load");
    assert_eq!(loaded.revision, on_result.revision);
    assert_eq!(loaded.document_translate_agent, None);
    let _ = set_document_translate_agent_core(
        &state.db,
        &state.emitter,
        &state.conversation_experience_gate,
        None,
    )
    .await;
}

// ────────────────────────────────────────────────────────────────────────────
// Incremental reference search (Task 6 transport surface)
// ────────────────────────────────────────────────────────────────────────────

use codeg_lib::db::test_helpers::seed_folder as seed_folder_path;
use codeg_lib::reference_search::types::{ReferenceDoneReason, ReferenceSearchPage};
use uuid::Uuid;

struct ReferenceApiFixture {
    server: TestServer,
    _workspace: tempfile::TempDir,
    _data: tempfile::TempDir,
    _static: tempfile::TempDir,
    workspace_path: String,
    search_session_id: String,
    request_id: String,
}

impl ReferenceApiFixture {
    fn auth_post(&self, path: &str) -> axum_test::TestRequest {
        self.server
            .post(path)
            .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
    }

    fn start_payload(&self) -> Value {
        json!({
            "searchSessionId": self.search_session_id,
            "sourceSequence": 1,
            "requestId": self.request_id,
            "source": "file",
            "query": ".ts",
            "workspacePath": self.workspace_path,
        })
    }

    fn next_payload(&self, page_index: u32) -> Value {
        json!({
            "searchSessionId": self.search_session_id,
            "sourceSequence": 1,
            "requestId": self.request_id,
            "source": "file",
            "pageIndex": page_index,
        })
    }
}

async fn reference_api_fixture(limit: u16) -> ReferenceApiFixture {
    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");
    let workspace = tempfile::tempdir().expect("workspace");
    let workspace_path = workspace.path().to_string_lossy().to_string();

    // More than ten matching files so a backend limit of 10 is observable.
    for i in 0..15 {
        let name = format!("match{i:02}.ts");
        std::fs::write(workspace.path().join(&name), b"x").expect("write match file");
    }
    std::fs::write(workspace.path().join("other.rs"), b"x").expect("write other");

    let db = fresh_in_memory_db().await;
    seed_folder_path(&db, &workspace_path).await;

    let state = AppState::new_for_test(db, data_dir.path().to_path_buf());
    state.reference_search_registry.set_limit(limit).await;
    let state = Arc::new(state);
    let shutdown = Arc::new(ShutdownSignal::new());
    let router = build_router(
        state,
        TEST_TOKEN.to_string(),
        static_dir.path().to_path_buf(),
        shutdown,
    );
    let server = TestServer::new(router).expect("test server");

    ReferenceApiFixture {
        server,
        _workspace: workspace,
        _data: data_dir,
        _static: static_dir,
        workspace_path,
        search_session_id: Uuid::new_v4().hyphenated().to_string(),
        request_id: Uuid::new_v4().hyphenated().to_string(),
    }
}

#[tokio::test]
async fn direct_http_client_cannot_raise_the_backend_limit() {
    let app = reference_api_fixture(10).await;
    let mut payload = app.start_payload();
    payload["resultLimit"] = json!(500);
    let start = app
        .auth_post("/api/start_reference_search")
        .json(&payload)
        .await;
    assert_eq!(start.status_code(), 200, "body={}", start.text());
    let mut page: ReferenceSearchPage = start.json();
    let mut count = page.items.len();
    while !page.done {
        let next = app
            .auth_post("/api/next_reference_search_page")
            .json(&app.next_payload(page.page_index + 1))
            .await;
        assert_eq!(next.status_code(), 200, "body={}", next.text());
        page = next.json();
        count += page.items.len();
    }
    assert_eq!(count, 10);
    assert_eq!(page.done_reason, Some(ReferenceDoneReason::Limit));
}

#[tokio::test]
async fn regex_helper_http_route_accepts_a_valid_body_above_axum_default() {
    let (server, _data, _static) = build_test_server().await;

    // 100 descriptors × 4096 NUL scalars → JSON-escaped well above 2 MiB.
    let field = "\u{0000}".repeat(4096);
    let mut descriptors = Vec::with_capacity(100);
    for i in 0..100 {
        descriptors.push(json!({
            "id": format!("d{i}"),
            "sourceOrdinal": i,
            "primary": [field],
            "secondary": [],
        }));
    }
    let body = json!({
        "query": "re:nomatchpatternxyz",
        "descriptors": descriptors,
    });

    let resp = server
        .post("/api/match_reference_regex")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&body)
        .await;
    assert_eq!(
        resp.status_code(),
        200,
        "large but valid body must clear the raised route limit; body={}",
        resp.text()
    );
    let matches: Vec<Value> = resp.json();
    assert!(matches.is_empty(), "non-matching regex yields empty result");
}

// ────────────────────────────────────────────────────────────────────────────
// close_folder_if_empty — dual-runtime Axum surface
// ────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn close_folder_if_empty_route_flips_empty_open_and_emits_auto_empty() {
    use codeg_lib::db::service::folder_service;
    use codeg_lib::db::test_helpers::seed_folder;
    use codeg_lib::web::event_bridge::FOLDER_CHANGED_EVENT;

    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-close-if-empty-http").await;

    let state = Arc::new(AppState::new_for_test(db, data_dir.path().to_path_buf()));
    let mut rx = state.event_broadcaster.subscribe();
    let shutdown = Arc::new(ShutdownSignal::new());
    let router = build_router(
        Arc::clone(&state),
        TEST_TOKEN.to_string(),
        static_dir.path().to_path_buf(),
        shutdown,
    );
    let server = TestServer::new(router).expect("test server");

    let resp = server
        .post("/api/close_folder_if_empty")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "folderId": folder_id }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: Value = resp.json();
    assert_eq!(body["closed"], true);

    // DB membership closed.
    let detail = folder_service::get_folder_by_id(&state.db.conn, folder_id)
        .await
        .expect("get")
        .expect("exists");
    // FolderDetail may not expose is_open; list open details must exclude it.
    let open = folder_service::list_open_folder_details(&state.db.conn)
        .await
        .expect("list open");
    assert!(
        open.iter().all(|f| f.id != folder_id),
        "closed folder must leave open list; still in open: {open:?}"
    );
    let _ = detail;

    // Emit AutoEmpty only on flip.
    let evt = rx.try_recv().expect("must broadcast folder://changed");
    assert_eq!(evt.channel, FOLDER_CHANGED_EVENT);
    let p = &*evt.payload;
    assert_eq!(p["kind"], "close");
    assert_eq!(p["folder_id"], folder_id);
    assert_eq!(p["cause"], "auto_empty");

    // Second call: already closed → closed:false, no emit.
    let resp2 = server
        .post("/api/close_folder_if_empty")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "folderId": folder_id }))
        .await;
    assert_eq!(resp2.status_code(), 200);
    let body2: Value = resp2.json();
    assert_eq!(body2["closed"], false);
    assert!(
        rx.try_recv().is_err(),
        "idempotent close must not re-emit AutoEmpty"
    );
}

#[tokio::test]
async fn close_folder_if_empty_route_returns_false_for_nonempty_without_emit() {
    use codeg_lib::db::service::conversation_service::create;
    use codeg_lib::db::test_helpers::seed_folder;
    use codeg_lib::models::agent::AgentType;

    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");
    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/codeg-close-nonempty-http").await;
    create(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
        .await
        .expect("live conv");

    let state = Arc::new(AppState::new_for_test(db, data_dir.path().to_path_buf()));
    let mut rx = state.event_broadcaster.subscribe();
    let shutdown = Arc::new(ShutdownSignal::new());
    let router = build_router(
        Arc::clone(&state),
        TEST_TOKEN.to_string(),
        static_dir.path().to_path_buf(),
        shutdown,
    );
    let server = TestServer::new(router).expect("test server");

    let resp = server
        .post("/api/close_folder_if_empty")
        .add_header("authorization", format!("Bearer {TEST_TOKEN}"))
        .json(&json!({ "folderId": folder_id }))
        .await;
    assert_eq!(resp.status_code(), 200, "body={}", resp.text());
    let body: Value = resp.json();
    assert_eq!(body["closed"], false);
    assert!(
        rx.try_recv().is_err(),
        "non-empty conditional close must not emit"
    );
}
