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
use codeg_lib::acp::session_attach::SessionAttachMode;
use codeg_lib::acp::shared_session::{
    SharedActiveTurnProjection, SharedLaunchIdentity, SharedQueuedPromptState,
    SharedQueuedPromptSummary, SharedReserveRequest, SharedSessionAttachment, SharedSessionKey,
};
use codeg_lib::acp::types::{AcpEvent, EventEnvelope};
use codeg_lib::app_state::AppState;
use codeg_lib::auto_title::ConnectionPurpose;
use codeg_lib::db::test_helpers::fresh_in_memory_db;
use codeg_lib::models::agent::AgentType;
use codeg_lib::web::event_bridge::emit_with_state;
use codeg_lib::web::router::build_router;
use codeg_lib::web::shutdown::ShutdownSignal;
use codeg_lib::web::ws_attach::{spawn_forwarder, DetachReason, ServerMsg};
use serde_json::{json, Value};
use tokio::sync::mpsc;

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

fn shared_launch_identity() -> SharedLaunchIdentity {
    SharedLaunchIdentity {
        agent_type: AgentType::Codex,
        working_dir_fingerprint: "ws-test-cwd".into(),
        external_session_id: None,
        attach_mode: SessionAttachMode::Default,
        route_fingerprint: "ws-test-route".into(),
        terminal_shell_fingerprint: "ws-test-shell".into(),
        purpose: ConnectionPurpose::User,
    }
}

fn shared_request(
    conversation_id: i32,
    connection_id: &str,
    client_instance_id: &str,
    request_id: &str,
    retry_failed_generation: Option<u64>,
) -> SharedReserveRequest {
    SharedReserveRequest {
        key: SharedSessionKey::Conversation(conversation_id),
        connection_id: connection_id.into(),
        launch_identity: shared_launch_identity(),
        client_instance_id: client_instance_id.into(),
        device_id: "ws-test-device".into(),
        request_id: request_id.into(),
        retry_failed_generation,
        now: tokio::time::Instant::now(),
        now_utc: chrono::Utc::now(),
    }
}

async fn seed_shared_root(
    state: &Arc<AppState>,
    conversation_id: i32,
    connection_id: &str,
    client_instance_id: &str,
    request_id: &str,
) -> SharedSessionAttachment {
    let attachment = state
        .connection_manager
        .shared_session_broker()
        .reserve_or_attach(shared_request(
            conversation_id,
            connection_id,
            client_instance_id,
            request_id,
            None,
        ))
        .await
        .expect("shared root reservation")
        .attachment;
    state
        .connection_manager
        .install_test_shared_connection(&attachment, Some(conversation_id))
        .await
        .expect("shared public state registration");
    attachment
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

#[tokio::test]
async fn ws_attach_shared_rejects_fenced_unknown_connection() {
    let (server, _state, _d, _s) = build_ws_server().await;
    let mut ws = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;

    let _ready = next_json(&mut ws).await;
    ws.send_json(&json!({
        "action": "attach",
        "subscription_id": "sub-shared-fenced",
        "connection_id": "unknown-shared",
        "generation": 7,
        "lease_id": "lease-fenced",
        "since_seq": null
    }))
    .await;

    let response = next_json(&mut ws).await;
    assert_eq!(response["type"], "detached");
    assert_eq!(response["subscription_id"], "sub-shared-fenced");
    assert_eq!(response["reason"], "generation_stale");
}

#[tokio::test]
async fn ws_attach_shared_tabs_renew_independently_and_overlay_lease_expiry() {
    let (server, state, _d, _s) = build_ws_server().await;
    let first = seed_shared_root(&state, 41, "shared-root", "client-a", "request-a").await;
    let second = state
        .connection_manager
        .shared_session_broker()
        .reserve_or_attach(shared_request(
            41,
            "ignored-by-shared-root",
            "client-b",
            "request-b",
            None,
        ))
        .await
        .expect("second shared lease")
        .attachment;

    let mut ws_a = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;
    let _ready_a = next_json(&mut ws_a).await;
    ws_a.send_json(&json!({
        "action": "attach",
        "subscription_id": "tab-a",
        "connection_id": first.connection_id,
        "generation": first.generation,
        "lease_id": first.lease_id,
        "since_seq": null
    }))
    .await;
    let snapshot_a = next_json(&mut ws_a).await;
    assert_eq!(
        snapshot_a["snapshot"]["shared_session"]["lease_expires_at"],
        serde_json::to_value(first.lease_expires_at).unwrap()
    );

    let state_arc = state
        .connection_manager
        .get_state(&first.connection_id)
        .await
        .expect("retained shared state");
    assert_eq!(
        state_arc
            .read()
            .await
            .to_snapshot()
            .shared_session
            .as_ref()
            .and_then(|shared| shared.lease_expires_at),
        None
    );

    let mut ws_b = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;
    let _ready_b = next_json(&mut ws_b).await;
    ws_b.send_json(&json!({
        "action": "attach",
        "subscription_id": "tab-b",
        "connection_id": second.connection_id,
        "generation": second.generation,
        "lease_id": second.lease_id,
        "since_seq": null
    }))
    .await;
    let snapshot_b = next_json(&mut ws_b).await;
    assert_eq!(
        snapshot_b["snapshot"]["shared_session"]["lease_expires_at"],
        serde_json::to_value(second.lease_expires_at).unwrap()
    );

    ws_a.send_json(&json!({
        "action": "attach",
        "subscription_id": "wrong-generation",
        "connection_id": first.connection_id,
        "generation": first.generation + 1,
        "lease_id": first.lease_id,
        "since_seq": null
    }))
    .await;
    let wrong_generation = next_json(&mut ws_a).await;
    assert_eq!(wrong_generation["reason"], "generation_stale");

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    ws_a.send_json(&json!({"action": "ping"})).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(next_json(&mut ws_a).await["type"], "pong");
    tokio::time::advance(Duration::from_secs(31)).await;
    let expired = state
        .connection_manager
        .shared_session_broker()
        .expire_leases(tokio::time::Instant::now())
        .await;
    assert_eq!(expired, vec![second.lease_id]);
}

#[tokio::test]
async fn ws_attach_shared_generation_retry_detaches_replaced_subscription() {
    let (server, state, _d, _s) = build_ws_server().await;
    let first = seed_shared_root(&state, 42, "shared-old", "client-a", "request-a").await;
    let broker = state.connection_manager.shared_session_broker();

    let mut ws = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;
    let _ready = next_json(&mut ws).await;
    ws.send_json(&json!({
        "action": "attach",
        "subscription_id": "old-live",
        "connection_id": first.connection_id,
        "generation": first.generation,
        "lease_id": first.lease_id,
        "since_seq": null
    }))
    .await;
    assert_eq!(next_json(&mut ws).await["type"], "snapshot");

    broker
        .mark_failed(
            &first.connection_id,
            first.generation,
            "companion_initialization_failed",
            false,
        )
        .await
        .expect("failed generation");
    broker
        .mark_cleanup_complete(&first.connection_id, first.generation)
        .await
        .expect("failed generation cleanup");
    let replacement = broker
        .reserve_or_attach(shared_request(
            42,
            "shared-new",
            "client-b",
            "request-b",
            Some(first.generation),
        ))
        .await
        .expect("replacement generation")
        .attachment;
    state
        .connection_manager
        .install_test_shared_connection(&replacement, Some(42))
        .await
        .expect("replacement public state");

    ws.send_json(&json!({"action": "ping"})).await;
    let detached = next_json(&mut ws).await;
    assert_eq!(detached["type"], "detached");
    assert_eq!(detached["subscription_id"], "old-live");
    assert_eq!(detached["reason"], "session_replaced");
    assert_eq!(next_json(&mut ws).await["type"], "pong");

    ws.send_json(&json!({
        "action": "attach",
        "subscription_id": "new-live",
        "connection_id": replacement.connection_id,
        "generation": replacement.generation,
        "lease_id": replacement.lease_id,
        "since_seq": null
    }))
    .await;
    let new_snapshot = next_json(&mut ws).await;
    assert_eq!(new_snapshot["type"], "snapshot");
    assert_eq!(
        new_snapshot["connection_id"],
        replacement.connection_id.as_str()
    );
    assert_eq!(
        new_snapshot["snapshot"]["shared_session"]["generation"],
        replacement.generation
    );

    ws.send_json(&json!({
        "action": "attach",
        "subscription_id": "old-direct",
        "connection_id": first.connection_id,
        "generation": first.generation,
        "lease_id": first.lease_id,
        "since_seq": null
    }))
    .await;
    let direct = next_json(&mut ws).await;
    assert_eq!(direct["type"], "detached");
    assert_eq!(direct["subscription_id"], "old-direct");
    assert_eq!(direct["reason"], "session_replaced");
}

#[tokio::test]
async fn ws_lag_reconnect_snapshot_restores_shared_state_and_lease_expiry() {
    let (server, state, _d, _s) = build_ws_server().await;
    let attached = seed_shared_root(
        &state,
        43,
        "shared-reconnect",
        "client-reconnect",
        "request-reconnect",
    )
    .await;
    let state_arc = state
        .connection_manager
        .get_state(&attached.connection_id)
        .await
        .expect("retained shared state");
    let receiver = state_arc.read().await.event_stream().subscribe();
    let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
    let (cleanup_tx, _cleanup_rx) = mpsc::channel(1);
    let forwarder = spawn_forwarder(
        "lagged-shared".into(),
        1,
        state.acp_event_bus.metrics().clone(),
        receiver,
        outbound_tx,
        cleanup_tx,
        state.connection_manager.shared_session_broker(),
        None,
    );

    let submitted_at = chrono::DateTime::parse_from_rfc3339("2026-08-16T00:00:00Z")
        .expect("valid fixture timestamp")
        .with_timezone(&chrono::Utc);
    let queued = |queue_item_id: &str, enqueue_seq: u64, client_message_id: &str| {
        SharedQueuedPromptSummary {
            queue_item_id: queue_item_id.into(),
            enqueue_seq,
            client_message_id: client_message_id.into(),
            visible_text: Some(format!("prompt-{enqueue_seq}")),
            visible_text_truncated: false,
            attachment_count: 0,
            submitted_at,
            state: SharedQueuedPromptState::Queued,
        }
    };
    emit_with_state(
        &state_arc,
        &state.emitter,
        AcpEvent::PromptQueued {
            generation: attached.generation,
            item: queued("queue-active", 1, "message-active"),
        },
    )
    .await;
    emit_with_state(
        &state_arc,
        &state.emitter,
        AcpEvent::PromptDispatchStarted {
            generation: attached.generation,
            turn: SharedActiveTurnProjection {
                turn_id: "turn-active".into(),
                queue_item_id: "queue-active".into(),
                enqueue_seq: 1,
                client_message_id: "message-active".into(),
                stop_requested: false,
            },
        },
    )
    .await;
    emit_with_state(
        &state_arc,
        &state.emitter,
        AcpEvent::PromptQueued {
            generation: attached.generation,
            item: queued("queue-waiting", 2, "message-waiting"),
        },
    )
    .await;
    emit_with_state(
        &state_arc,
        &state.emitter,
        AcpEvent::PermissionRequest {
            request_id: "permission-current".into(),
            tool_call: json!({"toolCallId": "tool-current", "title": "Inspect"}),
            options: Vec::new(),
            queued: 0,
        },
    )
    .await;
    emit_with_state(
        &state_arc,
        &state.emitter,
        AcpEvent::QuestionRequest {
            question_id: "question-current".into(),
            questions: Vec::new(),
        },
    )
    .await;
    emit_with_state(
        &state_arc,
        &state.emitter,
        AcpEvent::PlanApprovalRequest {
            approval_id: "plan-current".into(),
            tool_call_id: "plan-tool-current".into(),
            plan_markdown: "# Current plan".into(),
        },
    )
    .await;

    // The bounded outbound queue stalls the forwarder after two events. A
    // larger-than-broadcast-capacity burst then produces a real Lagged detach.
    for index in 0..4_097 {
        emit_with_state(
            &state_arc,
            &state.emitter,
            AcpEvent::ContentDelta {
                text: format!("{index}"),
                parent_tool_use_id: None,
            },
        )
        .await;
    }
    let first = tokio::time::timeout(Duration::from_secs(3), outbound_rx.recv())
        .await
        .expect("first forwarded frame within 3s");
    let second = tokio::time::timeout(Duration::from_secs(3), outbound_rx.recv())
        .await
        .expect("second forwarded frame within 3s");
    let detached = tokio::time::timeout(Duration::from_secs(3), outbound_rx.recv())
        .await
        .expect("lagged detach within 3s");
    assert!(matches!(first, Some(ServerMsg::Event { .. })));
    assert!(matches!(second, Some(ServerMsg::Event { .. })));
    assert!(matches!(
        detached,
        Some(ServerMsg::Detached {
            reason: DetachReason::Lagged,
            ..
        })
    ));
    forwarder.await.expect("lagged forwarder exits cleanly");

    let mut ws = server
        .get_websocket("/ws/events")
        .add_header(SEC_WEBSOCKET_PROTOCOL, ws_auth_protocol(TEST_TOKEN))
        .await
        .into_websocket()
        .await;
    let _ready = next_json(&mut ws).await;
    ws.send_json(&json!({
        "action": "attach",
        "subscription_id": "shared-reconnected",
        "connection_id": attached.connection_id,
        "generation": attached.generation,
        "lease_id": attached.lease_id,
        "since_seq": 0
    }))
    .await;

    let recovered = next_json(&mut ws).await;
    assert_eq!(recovered["type"], "snapshot");
    assert_eq!(
        recovered["snapshot"]["shared_session"]["phase"],
        json!({"phase": "bootstrapping"})
    );
    assert_eq!(
        recovered["snapshot"]["shared_session"]["queue"][0]["queue_item_id"],
        "queue-waiting"
    );
    assert_eq!(
        recovered["snapshot"]["shared_session"]["active_turn"]["turn_id"],
        "turn-active"
    );
    assert_eq!(
        recovered["snapshot"]["pending_permission"]["request_id"],
        "permission-current"
    );
    assert_eq!(
        recovered["snapshot"]["pending_question"]["question_id"],
        "question-current"
    );
    assert_eq!(
        recovered["snapshot"]["pending_plan_approval"]["approval_id"],
        "plan-current"
    );
    assert_eq!(
        recovered["snapshot"]["shared_session"]["lease_expires_at"],
        serde_json::to_value(attached.lease_expires_at).unwrap()
    );
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
            parent_tool_use_id: None,
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
                parent_tool_use_id: None,
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
    use chrono::Utc;
    use codeg_lib::acp::delegation::runtime_stats::DelegationRuntimeStats;
    use codeg_lib::acp::delegation::types::TaskObservation;
    use codeg_lib::acp::session_state::ActiveDelegationState;
    use codeg_lib::acp::tool_watchdog::{ToolCategory, ToolWatchdogPhase, ToolWatchdogProjection};

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
    assert_eq!(first["active_delegations"][0]["observation"], "active");
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
        state_arc
            .read()
            .await
            .to_snapshot()
            .tool_watchdog_projections,
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
        payload: AcpEvent::ContentDelta {
            text: "x".into(),
            parent_tool_use_id: None,
        },
    };
    let v = serde_json::to_value(&env).unwrap();
    assert_eq!(v["seq"], 7);
    assert_eq!(v["connection_id"], "c");
    assert_eq!(v["type"], "content_delta");
    assert_eq!(v["text"], "x");
}
