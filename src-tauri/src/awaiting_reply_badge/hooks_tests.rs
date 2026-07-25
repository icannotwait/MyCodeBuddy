//! Production-connected hook tests 1–4 for awaiting-reply taskbar badge.
//!
//! Each test exercises a real production entry point and asserts the schedule
//! recorder was bumped. Lifecycle hook test 5 lives in `acp/lifecycle.rs`.

use axum::Extension;
use axum::Json;

use crate::app_state::AppState;
use crate::commands::conversations::{
    create_conversation_core, delete_conversation_with_cleanup_core, emit_conversation_state,
    update_conversation_status_and_notify,
};
use crate::db::test_helpers::{fresh_in_memory_db, seed_folder};
use crate::models::agent::AgentType;
use crate::models::ConversationStatePatch;
use crate::web::event_bridge::EventEmitter;
use crate::web::handlers::conversations::{
    update_conversation_status, UpdateConversationStatusParams,
};

use super::{hook_test_lock, reset_schedule_calls, schedule_call_count};

#[tokio::test]
async fn hook_emit_conversation_state_schedules() {
    let _guard = hook_test_lock().await;
    reset_schedule_calls();

    let patch = ConversationStatePatch {
        id: 1,
        status: "pending_review".into(),
        awaiting_reply_token: Some("tok".into()),
        updated_at: chrono::Utc::now(),
    };
    emit_conversation_state(&EventEmitter::Noop, patch);

    assert!(
        schedule_call_count() >= 1,
        "emit_conversation_state must schedule badge refresh"
    );
}

#[tokio::test]
async fn hook_soft_delete_schedules() {
    let _guard = hook_test_lock().await;
    reset_schedule_calls();

    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/badge-hook-soft-delete").await;
    let id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
        .await
        .expect("create conversation");
    let coordinator = crate::auto_title::AutoTitleCoordinator::new_inert_for_test(db.conn.clone());

    delete_conversation_with_cleanup_core(&EventEmitter::Noop, &db.conn, coordinator.as_ref(), id)
        .await
        .expect("soft-delete");

    assert!(
        schedule_call_count() >= 1,
        "delete_conversation_with_cleanup_core must schedule badge refresh"
    );
}

#[tokio::test]
async fn hook_http_status_schedules() {
    let _guard = hook_test_lock().await;
    reset_schedule_calls();

    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/badge-hook-http-status").await;
    let id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
        .await
        .expect("create conversation");
    let dir = tempfile::tempdir().expect("tempdir");
    let state = std::sync::Arc::new(AppState::new_for_test(db, dir.path().to_path_buf()));

    let _body = update_conversation_status(
        Extension(state),
        Json(UpdateConversationStatusParams {
            conversation_id: id,
            status: "completed".into(),
        }),
    )
    .await
    .expect("http update status");

    assert!(
        schedule_call_count() >= 1,
        "HTTP update_conversation_status must schedule badge refresh"
    );
}

#[tokio::test]
async fn hook_shared_status_notify_schedules() {
    let _guard = hook_test_lock().await;
    reset_schedule_calls();

    let db = fresh_in_memory_db().await;
    let folder_id = seed_folder(&db, "/tmp/badge-hook-shared-status").await;
    let id = create_conversation_core(&db.conn, folder_id, AgentType::ClaudeCode, None, None)
        .await
        .expect("create conversation");

    update_conversation_status_and_notify(&db.conn, &EventEmitter::Noop, id, "completed".into())
        .await
        .expect("shared status notify");

    assert!(
        schedule_call_count() >= 1,
        "update_conversation_status_and_notify must schedule badge refresh"
    );
}
