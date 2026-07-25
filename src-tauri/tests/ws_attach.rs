//! WebSocket `/ws/events` attach-protocol integration tests.
//!
//! Mounts the real router via `web::router::build_router`, drives the
//! `axum-test` HTTP transport (required for WS upgrade), and exercises the
//! attach handshake end-to-end: auth, cold snapshot, live event delivery,
//! re-attach replay, and the ConnectionGone detach reason. Phase 4 in the
//! test rollout plan.

use std::sync::Arc;
use std::time::Duration;

use axum_test::TestServer;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use codeg_lib::acp::types::{AcpEvent, EventEnvelope};
use codeg_lib::app_state::AppState;
use codeg_lib::db::test_helpers::fresh_in_memory_db;
use codeg_lib::models::agent::AgentType;
use codeg_lib::web::event_bridge::emit_with_state;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use serde_json::{json, Value};

const SEC_WEBSOCKET_PROTOCOL: &str = "sec-websocket-protocol";

const TEST_TOKEN: &str = "ws-test-token";

/// Builds an HTTP-transport TestServer wired to the real router, plus a live
/// `Arc<AppState>` for tests that need to manipulate the connection manager.
/// Both tempdirs are returned so they outlive the server.
async fn build_ws_server() -> (
    TestServer,
    Arc<AppState>,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let data_dir = tempfile::tempdir().expect("data dir");
    let static_dir = tempfile::tempdir().expect("static dir");

    let db = fresh_in_memory_db().await;
    let state = Arc::new(AppState::new_for_test(db, data_dir.path().to_path_buf()));
    let shutdown = Arc::new(ShutdownSignal::new());

    let router = build_router(
        Arc::clone(&state),
        TEST_TOKEN.to_string(),
        static_dir.path().to_path_buf(),
        shutdown,
    );

    let server = TestServer::builder()
        .http_transport()
        .build(router)
        .expect("test server");
    (server, state, data_dir, static_dir)
}

fn ws_auth_protocol(token: &str) -> String {
    let encoded = URL_SAFE_NO_PAD.encode(token);
    format!("codeg-events, codeg-token.{encoded}")
}

/// Receive the next text frame, with a hard timeout so a missing frame fails
/// the test fast instead of hanging.
async fn next_text(ws: &mut axum_test::TestWebSocket) -> String {
    tokio::time::timeout(Duration::from_secs(3), ws.receive_text())
        .await
        .expect("ws frame within 3s")
}

async fn next_json(ws: &mut axum_test::TestWebSocket) -> Value {
    let text = next_text(ws).await;
    serde_json::from_str(&text).expect("frame is valid json")
}

// ───────────────────────────────────────────────────────────────────────────
// 1. Unauthenticated upgrade is rejected.
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_upgrade_without_token_is_rejected() {
    let (server, _state, _d, _s) = build_ws_server().await;
    // No Sec-WebSocket-Protocol containing the token, no Authorization header.
    let resp = server.get_websocket("/ws/events").await;
    // Auth middleware returns 401 before the upgrade handshake completes.
    assert_eq!(
        resp.status_code(),
        401,
        "expected 401 without token, got {}",
        resp.status_code()
    );
}

// ───────────────────────────────────────────────────────────────────────────
// 2. Authenticated upgrade delivers the legacy __ready__ handshake frame.
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_authenticated_receives_ready_frame() {
    let (server, _state, _d, _s) = build_ws_server().await;
    let mut ws = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;

    let frame = next_json(&mut ws).await;
    assert_eq!(frame["channel"], "__ready__");
}

// ───────────────────────────────────────────────────────────────────────────
// 3. Attach to an unknown connection_id detaches with ConnectionGone.
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_attach_unknown_connection_detaches() {
    let (server, _state, _d, _s) = build_ws_server().await;
    let mut ws = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;

    // Drain the legacy ready frame first.
    let _ready = next_json(&mut ws).await;

    ws.send_json(&json!({
        "action": "attach",
        "subscription_id": "sub-1",
        "connection_id": "does-not-exist",
        "since_seq": null
    }))
    .await;

    let resp = next_json(&mut ws).await;
    assert_eq!(resp["type"], "detached");
    assert_eq!(resp["subscription_id"], "sub-1");
    assert_eq!(resp["reason"], "connection_gone");
}

// ───────────────────────────────────────────────────────────────────────────
// 4. Cold attach to a live connection returns snapshot, then live events
//    flow through as `event` frames.
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_cold_attach_receives_snapshot_then_live_events() {
    let (server, state, _d, _s) = build_ws_server().await;

    // Pre-register a synthetic connection bound to the same emitter the
    // router serves from, so events emitted via `emit_with_state` reach the
    // per-connection broadcaster the WS attach handler subscribes to.
    let conn_id = "test-conn-1";
    state
        .connection_manager
        .insert_test_connection(conn_id, AgentType::ClaudeCode, None, state.emitter.clone())
        .await;

    let mut ws = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;
    let _ready = next_json(&mut ws).await;

    ws.send_json(&json!({
        "action": "attach",
        "subscription_id": "sub-cold",
        "connection_id": conn_id,
        "since_seq": null
    }))
    .await;

    let snapshot = next_json(&mut ws).await;
    assert_eq!(snapshot["type"], "snapshot");
    assert_eq!(snapshot["subscription_id"], "sub-cold");
    assert_eq!(snapshot["connection_id"], conn_id);
    assert_eq!(snapshot["event_seq"], 0, "fresh state has seq 0");

    // Drive a real event through the same path production uses. This
    // increments event_seq under the SessionState write lock, pushes to
    // the recent_events buffer, and broadcasts to the per-connection
    // broadcaster — which the WS attach forwarder reads from.
    let state_arc = state
        .connection_manager
        .get_state(conn_id)
        .await
        .expect("registered connection");
    emit_with_state(
        &state_arc,
        &state.emitter,
        AcpEvent::ContentDelta {
            text: "hello-world".into(),
        },
    )
    .await;

    let live = next_json(&mut ws).await;
    assert_eq!(live["type"], "event");
    assert_eq!(live["subscription_id"], "sub-cold");
    let envelope = &live["envelope"];
    assert_eq!(envelope["connection_id"], conn_id);
    assert_eq!(envelope["seq"], 1, "first event has seq 1");
    assert_eq!(envelope["type"], "content_delta");
    assert_eq!(envelope["text"], "hello-world");
}

// ───────────────────────────────────────────────────────────────────────────
// 5. Hot attach with a cursor older than the head returns a replay frame
//    containing the events the client missed.
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_hot_attach_with_cursor_receives_replay() {
    let (server, state, _d, _s) = build_ws_server().await;

    let conn_id = "test-conn-replay";
    state
        .connection_manager
        .insert_test_connection(conn_id, AgentType::ClaudeCode, None, state.emitter.clone())
        .await;

    // Emit three events BEFORE the WS even connects. The recent_events
    // ring buffer should hold all three (well under capacity).
    let state_arc = state
        .connection_manager
        .get_state(conn_id)
        .await
        .expect("conn");
    for i in 0..3 {
        emit_with_state(
            &state_arc,
            &state.emitter,
            AcpEvent::ContentDelta {
                text: format!("delta-{i}"),
            },
        )
        .await;
    }

    let mut ws = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;
    let _ready = next_json(&mut ws).await;

    // since_seq = 1 → client claims to have seen seq 1, wants 2 and 3.
    ws.send_json(&json!({
        "action": "attach",
        "subscription_id": "sub-replay",
        "connection_id": conn_id,
        "since_seq": 1
    }))
    .await;

    let frame = next_json(&mut ws).await;
    assert_eq!(frame["type"], "replay");
    assert_eq!(frame["subscription_id"], "sub-replay");
    assert_eq!(frame["connection_id"], conn_id);
    let events = frame["events"].as_array().expect("events array");
    assert_eq!(
        events.len(),
        2,
        "expected 2 missed events, got {:?}",
        events
    );
    assert_eq!(events[0]["seq"], 2);
    assert_eq!(events[1]["seq"], 3);
    assert_eq!(frame["high_water_seq"], 3);
}

// ───────────────────────────────────────────────────────────────────────────
// Cold attach preserves parent card + watchdog projections; viewer attach
// count does not touch last_agent_activity_at or lease projections.
// ───────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_cold_attach_retains_watchdog_and_does_not_touch_activity_clocks() {
    use codeg_lib::acp::delegation::runtime_stats::DelegationRuntimeStats;
    use codeg_lib::acp::delegation::types::TaskObservation;
    use codeg_lib::acp::session_state::ActiveDelegationState;
    use codeg_lib::acp::tool_watchdog::{ToolCategory, ToolWatchdogPhase, ToolWatchdogProjection};
    use chrono::Utc;

    let (server, state, _d, _s) = build_ws_server().await;
    let conn_id = "parent-live";
    state
        .connection_manager
        .insert_test_connection(conn_id, AgentType::ClaudeCode, None, state.emitter.clone())
        .await;

    let state_arc = state
        .connection_manager
        .get_state(conn_id)
        .await
        .expect("parent connection");

    {
        let mut s = state_arc.write().await;
        s.last_agent_activity_at = Utc::now();
        let now = s.last_agent_activity_at;
        s.active_delegations.insert(
            "parent-tool".into(),
            ActiveDelegationState {
                parent_tool_use_id: "parent-tool".into(),
                child_connection_id: "child-live".into(),
                child_conversation_id: 7,
                agent_type: AgentType::Codex,
                task_preview: "live work".into(),
                task_id: "task-live".into(),
                started_at: now,
                runtime_stats: DelegationRuntimeStats::empty(now),
                attention_request: None,
                observation: Some(TaskObservation::Active),
                last_agent_activity_at: Some(now),
                stalled_since: None,
            },
        );
        s.tool_watchdog_projections.insert(
            "lease-live".into(),
            ToolWatchdogProjection {
                lease_id: "lease-live".into(),
                version: 2,
                tool_title: ToolCategory::Delegation,
                phase: ToolWatchdogPhase::Grace,
                last_progress_at: now.to_rfc3339(),
                transition_at: now.to_rfc3339(),
                transition_seq: 1,
                grace_deadline: Some((now + chrono::Duration::seconds(600)).to_rfc3339()),
                cancellation_scope: None,
                error_code: None,
            },
        );
    }

    let activity_before_viewers = state_arc.read().await.last_agent_activity_at;
    let projections_before_viewers = state_arc
        .read()
        .await
        .to_snapshot()
        .tool_watchdog_projections
        .clone();

    async fn cold_attach(
        server: &TestServer,
        conn_id: &str,
        sub_id: &str,
    ) -> (axum_test::TestWebSocket, Value) {
        let mut ws = server
            .get_websocket("/ws/events")
            .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
            .await
            .into_websocket()
            .await;
        let _ready = next_json(&mut ws).await;
        ws.send_json(&json!({
            "action": "attach",
            "subscription_id": sub_id,
            "connection_id": conn_id,
            "since_seq": null
        }))
        .await;
        let frame = next_json(&mut ws).await;
        assert_eq!(frame["type"], "snapshot");
        assert_eq!(frame["subscription_id"], sub_id);
        let snapshot = frame["snapshot"].clone();
        (ws, snapshot)
    }

    let (ws1, first) = cold_attach(&server, conn_id, "sub-viewer-1").await;
    let (ws2, second) = cold_attach(&server, conn_id, "sub-viewer-2").await;

    assert_eq!(first["active_delegations"][0]["task_id"], "task-live");
    assert_eq!(
        first["active_delegations"][0]["observation"],
        "active"
    );
    assert_eq!(
        first["tool_watchdog_projections"]["lease-live"]["phase"],
        "grace"
    );
    assert_eq!(
        second["tool_watchdog_projections"]["lease-live"]["version"],
        2
    );

    // Drop both viewer sockets; health clocks and projections must be unchanged.
    drop(ws1);
    drop(ws2);

    assert_eq!(
        state_arc.read().await.last_agent_activity_at,
        activity_before_viewers,
    );
    assert_eq!(
        state_arc.read().await.to_snapshot().tool_watchdog_projections,
        projections_before_viewers,
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Compile-time sanity: the types we serialize against actually exist and
// the AcpEvent variant we use serializes the way we asserted.
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn content_delta_envelope_serializes_to_expected_shape() {
    let env = EventEnvelope {
        seq: 7,
        connection_id: "c".into(),
        payload: AcpEvent::ContentDelta { text: "x".into() },
    };
    let v = serde_json::to_value(&env).unwrap();
    assert_eq!(v["seq"], 7);
    assert_eq!(v["connection_id"], "c");
    assert_eq!(v["type"], "content_delta");
    assert_eq!(v["text"], "x");
}
