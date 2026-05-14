//! Remote-workspace IPC proxy.
//!
//! When a desktop window is opened against a remote codeg-server, every API
//! call and WebSocket event for that connection is funnelled through Rust
//! commands defined here. The webview never opens an HTTP/WS connection to
//! the remote host directly — that path is blocked by the Tauri webview's
//! secure-context mixed-content rules whenever the remote URL is plain
//! `http://`. Routing through Rust (reqwest + tokio-tungstenite) bypasses
//! those rules and gives us a single place to manage auth, reconnect, and
//! per-window event isolation.
//!
//! ## Isolation contract
//!
//! - Different `connection_id`s use distinct Tauri event channels
//!   (`remote-ws-event-{id}`) AND distinct background WS tasks. Two remote
//!   workspaces opened side-by-side never mix events.
//! - Within one `connection_id`, multiple webviews (main + remote-settings
//!   child window, etc.) share **one** underlying WS connection but each
//!   event is dispatched only to the webview labels that have explicitly
//!   subscribed. We never `app.emit(...)` globally — every emit is
//!   `app.emit_to(EventTarget::webview(label), ...)`.
//! - When the last subscriber for a connection unsubscribes (or its window
//!   is destroyed), the WS task shuts down and the entry is removed from
//!   the proxy state.
//!
//! The whole module is gated to `feature = "tauri-runtime"` via `mod.rs`;
//! the inner-attribute form here would duplicate the predicate.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tauri::{AppHandle, Emitter, EventTarget, State, WebviewWindow};
use tokio::sync::{watch, Mutex, RwLock};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, handshake::client::Request, http::HeaderValue, Message,
};

use crate::app_error::AppCommandError;
use crate::db::service::remote_workspace_connection_service;
use crate::db::AppDatabase;

/// HTTP request timeout. Long enough to survive remote ACP prompts (which
/// can stream for a while) but bounded so a hung remote can't lock a
/// webview indefinitely. Matches the JS-side `REMOTE_CALL_TIMEOUT_MS`.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Number of consecutive WS connect failures before we give up and emit
/// `__unauthorized__`. Matches the JS-side `wsFailCount >= 3` threshold.
const WS_RECONNECT_FAIL_THRESHOLD: u32 = 3;

/// Exponential backoff bounds for WS reconnect. 1s/2s/4s/8s/16s/32s.
const WS_BACKOFF_INITIAL_SECS: u64 = 1;
const WS_BACKOFF_MAX_SECS: u64 = 32;

/// MUST match the values in `src-tauri/src/web/auth.rs` and
/// `src/lib/transport/ws-auth.ts`. The server's auth middleware reads the
/// `sec-websocket-protocol` header looking for `codeg-token.{base64url}`.
const WS_EVENT_PROTOCOL: &str = "codeg-events";
const WS_TOKEN_PROTOCOL_PREFIX: &str = "codeg-token.";

/// Internal Tauri-event channels emitted by this proxy. The frontend
/// `RemoteDesktopTransport` reserves these names. MUST match the
/// equivalents in `src/lib/transport/constants.ts` and the
/// `__disconnected__` / `__unauthorized__` literals in
/// `src/lib/transport/remote-desktop-transport.ts`.
const WS_READY_CHANNEL: &str = "__ready__";
const WS_DISCONNECTED_CHANNEL: &str = "__disconnected__";
const WS_UNAUTHORIZED_CHANNEL: &str = "__unauthorized__";

/// One entry per active remote `connection_id` with at least one webview
/// subscribed. Held inside `RemoteProxyState::tasks` behind an `Arc` so the
/// background WS task and the subscribe/unsubscribe commands can share it.
struct WsTaskEntry {
    /// Set of webview labels (`window.label()`) currently subscribed for
    /// this connection. Every WS message is fan-out to exactly these
    /// labels — non-subscribers in other workspace windows never see the
    /// events even if they happen to listen for the same event name.
    subscribers: Mutex<HashSet<String>>,
    /// True only after the current underlying WebSocket has emitted
    /// `__ready__`, and reset to false on disconnect / reconnect wait.
    /// Late subscribers can only receive an immediate synthetic ready when
    /// this flag is true; otherwise they must wait for the next real
    /// ready frame so we do not weaken the readiness contract during
    /// reconnect windows.
    ready: RwLock<bool>,
    /// Signals the background WS task to exit. Writing `true` triggers
    /// graceful shutdown; the receiver side is owned by the task.
    shutdown_tx: watch::Sender<bool>,
}

/// Tauri-managed singleton. Wired into the runtime via `.manage(...)`.
pub struct RemoteProxyState {
    /// connection_id → live WS task entry. Entries are inserted on first
    /// subscribe and removed when the last subscriber leaves or the task
    /// terminates (e.g. unauthorized).
    tasks: Mutex<HashMap<i32, Arc<WsTaskEntry>>>,
    /// Shared HTTP client. reqwest pools connections internally, so reusing
    /// one client keeps each command call cheap.
    http: reqwest::Client,
}

impl RemoteProxyState {
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .expect("failed to build reqwest client for remote proxy"),
        }
    }

    /// Remove `label` from every active subscription. Called from the global
    /// `WindowEvent::Destroyed` hook so closing a webview cleans up even if
    /// the frontend never had a chance to unsubscribe (forced quit, crash).
    pub async fn remove_subscriber_globally(self: &Arc<Self>, label: &str) {
        let connection_ids: Vec<i32> = {
            // Snapshot the task map under a single lock so we don't hold it
            // across the per-entry async work below.
            let tasks = self.tasks.lock().await;
            tasks.keys().copied().collect()
        };

        for connection_id in connection_ids {
            self.remove_subscriber(connection_id, label).await;
        }
    }

    /// Remove `label` from one connection's subscriber set, shutting down
    /// the WS task if it was the last one.
    async fn remove_subscriber(self: &Arc<Self>, connection_id: i32, label: &str) {
        let entry = {
            let tasks = self.tasks.lock().await;
            match tasks.get(&connection_id) {
                Some(e) => e.clone(),
                None => return,
            }
        };

        let should_shutdown = {
            let mut subs = entry.subscribers.lock().await;
            subs.remove(label);
            subs.is_empty()
        };

        if should_shutdown {
            // Best-effort signal; if the task already exited (e.g. unauth)
            // the receiver is dropped and this is a no-op.
            let _ = entry.shutdown_tx.send(true);
            // The task self-removes from `tasks` on exit, so we don't
            // race-clean here.
        }
    }
}

impl Default for RemoteProxyState {
    fn default() -> Self {
        Self::new()
    }
}

// ─── HTTP proxy command ────────────────────────────────────────────────

/// Forward an HTTP API call to the remote codeg-server identified by
/// `connection_id`. The frontend's `RemoteDesktopTransport.call(cmd, args)`
/// delegates to this; it never opens a fetch from the webview.
///
/// Error mapping:
///
/// - HTTP 401 → `AppErrorCode::AuthenticationFailed` ("token expired"). The
///   frontend recognises this code and surfaces the connection-expired UI
///   in just the calling window — by design we don't broadcast to siblings.
/// - Other non-2xx with a structured `AppCommandError` body → forwarded
///   verbatim so the original `code` / `message` / `i18n_key` /
///   `i18n_params` reach the caller intact. This preserves the i18n
///   pipeline across the proxy boundary.
/// - Other non-2xx without a structured body → wrapped as
///   `NetworkError` with the raw body in `detail`.
/// - Connect / read errors → wrapped as `NetworkError`.
#[tauri::command]
pub async fn remote_http_call(
    db: State<'_, AppDatabase>,
    proxy: State<'_, Arc<RemoteProxyState>>,
    connection_id: i32,
    command: String,
    args: Option<Value>,
) -> Result<Value, AppCommandError> {
    let conn = remote_workspace_connection_service::get(&db.conn, connection_id)
        .await
        .map_err(AppCommandError::db)?
        .ok_or_else(|| {
            AppCommandError::not_found(format!("Remote connection {connection_id} not found"))
        })?;

    let url = format!(
        "{}/api/{}",
        conn.base_url.trim_end_matches('/'),
        command.trim_start_matches('/')
    );

    let body = args.unwrap_or(Value::Object(serde_json::Map::new()));

    let response = proxy
        .http
        .post(&url)
        .bearer_auth(conn.token.trim())
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            AppCommandError::network("Remote HTTP request failed").with_detail(e.to_string())
        })?;

    let status = response.status();

    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AppCommandError::authentication_failed(
            "Remote Workspace token is invalid",
        ));
    }

    if !status.is_success() {
        let raw_body = response.text().await.unwrap_or_default();
        // The remote codeg-server always returns `Json(AppCommandError)`
        // on errors (see `web/handlers/error.rs::IntoResponse`). Try to
        // deserialize so the caller sees the original code + i18n hint.
        if let Ok(structured) = serde_json::from_str::<AppCommandError>(&raw_body) {
            return Err(structured);
        }
        return Err(
            AppCommandError::network(format!("Remote returned HTTP {status}"))
                .with_detail(if raw_body.is_empty() {
                    status.canonical_reason().unwrap_or("error").to_string()
                } else {
                    raw_body
                }),
        );
    }

    response.json::<Value>().await.map_err(|e| {
        AppCommandError::network("Failed to parse remote response").with_detail(e.to_string())
    })
}

// ─── WebSocket proxy commands ─────────────────────────────────────────

/// Subscribe the calling webview to the remote server's WS event stream.
/// If this is the first subscriber for `connection_id`, also spawns the
/// background task that maintains the underlying WebSocket. Subsequent
/// subscribes from the same window are no-ops (idempotent).
///
/// The frontend `RemoteDesktopTransport` must first `listen()` for
/// `remote-ws-event-{connection_id}` before invoking this — otherwise the
/// `__ready__` frame emitted by the WS task may arrive before any
/// listener is registered.
#[tauri::command]
pub async fn remote_ws_subscribe(
    app: AppHandle,
    db: State<'_, AppDatabase>,
    proxy: State<'_, Arc<RemoteProxyState>>,
    window: WebviewWindow,
    connection_id: i32,
) -> Result<(), AppCommandError> {
    let label = window.label().to_string();
    let event_name = format!("remote-ws-event-{connection_id}");
    let proxy_arc: Arc<RemoteProxyState> = (*proxy).clone();

    // Fast path: existing entry — just record this label as a subscriber.
    // Idempotent: if the same label is already subscribed, returns OK.
    // If the underlying WS is already ready, immediately emit a synthetic
    // `__ready__` to this label only. Without this, secondary windows (e.g.
    // remote settings) would always sit through the 5s frontend timeout even
    // though the shared socket is live. If the socket is reconnecting,
    // `ready == false` and the new subscriber waits for the next real ready.
    let needs_new_task = {
        let tasks = proxy_arc.tasks.lock().await;
        match tasks.get(&connection_id) {
            Some(entry) => {
                entry.subscribers.lock().await.insert(label.clone());
                let is_ready = *entry.ready.read().await;
                if is_ready {
                    emit_internal_to_label(&app, &label, &event_name, WS_READY_CHANNEL);
                }
                false
            }
            None => true,
        }
    };

    if !needs_new_task {
        return Ok(());
    }

    // Slow path: load credentials, create entry, spawn WS task.
    let conn = remote_workspace_connection_service::get(&db.conn, connection_id)
        .await
        .map_err(AppCommandError::db)?
        .ok_or_else(|| {
            AppCommandError::not_found(format!("Remote connection {connection_id} not found"))
        })?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let entry = Arc::new(WsTaskEntry {
        subscribers: Mutex::new({
            let mut set = HashSet::new();
            set.insert(label.clone());
            set
        }),
        ready: RwLock::new(false),
        shutdown_tx,
    });

    // Insert under the proxy lock. If a concurrent subscribe for the same
    // connection_id raced us and already inserted, fold our label into
    // theirs and abort our task spawn.
    {
        let mut tasks = proxy_arc.tasks.lock().await;
        if let Some(existing) = tasks.get(&connection_id) {
            existing.subscribers.lock().await.insert(label);
            return Ok(());
        }
        tasks.insert(connection_id, entry.clone());
    }

    let task_app = app.clone();
    let task_proxy = proxy_arc.clone();
    let base_url = conn.base_url.clone();
    let token = conn.token.clone();
    let task_entry = entry.clone();

    tauri::async_runtime::spawn(async move {
        run_ws_task(
            task_app,
            task_proxy,
            connection_id,
            base_url,
            token,
            task_entry,
            shutdown_rx,
        )
        .await;
    });

    Ok(())
}

/// Unsubscribe the calling webview from `connection_id`. If this was the
/// last subscriber, signals the background task to shut down; the task
/// removes itself from the proxy state on exit.
#[tauri::command]
pub async fn remote_ws_unsubscribe(
    proxy: State<'_, Arc<RemoteProxyState>>,
    window: WebviewWindow,
    connection_id: i32,
) -> Result<(), AppCommandError> {
    let proxy_arc: Arc<RemoteProxyState> = (*proxy).clone();
    let label = window.label().to_string();
    proxy_arc.remove_subscriber(connection_id, &label).await;
    Ok(())
}

// ─── WS background task ───────────────────────────────────────────────

/// Long-running task that maintains one WebSocket per `connection_id`.
/// Lifecycle:
///   1. Connect (with subprotocol-auth header).
///   2. On successful upgrade, emit `__ready__` to current subscribers.
///   3. Read messages, fan out to subscribers as `(channel, payload)`
///      envelopes.
///   4. On disconnect, emit `__disconnected__`, increment fail count, back
///      off, retry.
///   5. After `WS_RECONNECT_FAIL_THRESHOLD` consecutive failures, emit
///      `__unauthorized__` and exit.
///   6. At any point, a `shutdown_tx.send(true)` causes graceful exit.
///
/// On exit (any path), the task removes its entry from `proxy.tasks` —
/// but only if the entry still matches its own `Arc`, so a racy
/// resubscribe that already replaced the entry isn't clobbered.
async fn run_ws_task(
    app: AppHandle,
    proxy: Arc<RemoteProxyState>,
    connection_id: i32,
    base_url: String,
    token: String,
    entry: Arc<WsTaskEntry>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let event_name = format!("remote-ws-event-{connection_id}");
    let ws_url = http_url_to_ws_url(&base_url);
    let mut fail_count: u32 = 0;

    'reconnect: loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let connect_result = tokio::select! {
            biased;
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
                continue;
            }
            res = connect_with_subprotocol_auth(&ws_url, &token) => res,
        };

        let mut socket = match connect_result {
            Ok(s) => s,
            Err(err) => {
                eprintln!(
                    "[RemoteProxy] WS connect failed for connection {connection_id}: {err}"
                );
                fail_count += 1;
                if fail_count >= WS_RECONNECT_FAIL_THRESHOLD {
                    emit_internal(&app, &entry, &event_name, WS_UNAUTHORIZED_CHANNEL).await;
                    break;
                }
                if backoff_sleep(&mut shutdown_rx, fail_count).await {
                    break;
                }
                continue;
            }
        };

        // Connect succeeded — reset fail count. We do not emit `__ready__`
        // here; the remote server emits the real `__ready__` only after it
        // has subscribed to its broadcaster, and that is the readiness
        // contract the frontend relies on.
        fail_count = 0;

        // Read loop. Exits on shutdown, error, or remote close.
        loop {
            tokio::select! {
                biased;
                changed = shutdown_rx.changed() => {
                    if changed.is_ok() && *shutdown_rx.borrow() {
                        let _ = socket.send(Message::Close(None)).await;
                        break 'reconnect;
                    }
                }
                msg = socket.next() => match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Err(err) = forward_text_message(&app, &entry, &event_name, &text).await {
                            eprintln!(
                                "[RemoteProxy] failed to forward WS message on connection {connection_id}: {err}"
                            );
                        }
                    }
                    Some(Ok(Message::Binary(_))) => {
                        // Server only emits text frames today; ignore binary.
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = socket.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Frame(_))) => {}
                    Some(Ok(Message::Close(_))) | None => {
                        break;
                    }
                    Some(Err(err)) => {
                        eprintln!(
                            "[RemoteProxy] WS read error on connection {connection_id}: {err}"
                        );
                        break;
                    }
                },
            }
        }

        // Disconnected (not via shutdown). Notify and try again.
        *entry.ready.write().await = false;
        emit_internal(&app, &entry, &event_name, WS_DISCONNECTED_CHANNEL).await;
        fail_count += 1;
        if fail_count >= WS_RECONNECT_FAIL_THRESHOLD {
            emit_internal(&app, &entry, &event_name, WS_UNAUTHORIZED_CHANNEL).await;
            break;
        }
        if backoff_sleep(&mut shutdown_rx, fail_count).await {
            break;
        }
    }

    *entry.ready.write().await = false;

    // Cleanup: remove our entry from the proxy state. We only remove if
    // the stored Arc still points to us — a fresh resubscribe between
    // our shutdown signal and this cleanup could have already replaced
    // it, and we mustn't blow away the newer entry.
    let mut tasks = proxy.tasks.lock().await;
    if let Some(stored) = tasks.get(&connection_id) {
        if Arc::ptr_eq(stored, &entry) {
            tasks.remove(&connection_id);
        }
    }
}

/// Sleep for the exponential-backoff duration corresponding to
/// `fail_count` (1s, 2s, 4s, … capped at `WS_BACKOFF_MAX_SECS`). Returns
/// `true` if shutdown was requested during the wait — caller should exit
/// its loop in that case.
async fn backoff_sleep(shutdown_rx: &mut watch::Receiver<bool>, fail_count: u32) -> bool {
    let shift = fail_count.saturating_sub(1).min(8) as u64;
    let secs = (WS_BACKOFF_INITIAL_SECS << shift).min(WS_BACKOFF_MAX_SECS);
    tokio::select! {
        biased;
        changed = shutdown_rx.changed() => changed.is_ok() && *shutdown_rx.borrow(),
        _ = tokio::time::sleep(Duration::from_secs(secs)) => false,
    }
}

/// Forward a text frame from the remote WS to all current subscribers of
/// this connection. The remote codeg-server's `ws.rs` emits frames shaped
/// `{ "channel": "...", "payload": ... }` (see `WebEventBroadcaster`).
/// We re-emit the payload as-is into the Tauri event named
/// `remote-ws-event-{connection_id}`, but only to webview labels listed in
/// the subscriber set — never broadcast.
async fn forward_text_message(
    app: &AppHandle,
    entry: &Arc<WsTaskEntry>,
    event_name: &str,
    text: &str,
) -> Result<(), String> {
    // Validate the JSON shape minimally to surface server-side bugs
    // (malformed frames) without dropping the frame entirely.
    let envelope: Value =
        serde_json::from_str(text).map_err(|e| format!("invalid WS frame: {e}"))?;

    if envelope
        .get("channel")
        .and_then(Value::as_str)
        .is_some_and(|channel| channel == WS_READY_CHANNEL)
    {
        *entry.ready.write().await = true;
    }

    let labels = snapshot_subscribers(entry).await;
    for label in labels {
        if let Err(e) = app.emit_to(EventTarget::webview(&label), event_name, &envelope) {
            eprintln!(
                "[RemoteProxy] emit_to {label} for {event_name} failed: {e}"
            );
        }
    }
    Ok(())
}

/// Emit one of the internal lifecycle channels (`__ready__`,
/// `__disconnected__`, `__unauthorized__`) to all current subscribers.
async fn emit_internal(
    app: &AppHandle,
    entry: &Arc<WsTaskEntry>,
    event_name: &str,
    channel: &'static str,
) {
    let labels = snapshot_subscribers(entry).await;
    for label in labels {
        emit_internal_to_label(app, &label, event_name, channel);
    }
}

fn emit_internal_to_label(
    app: &AppHandle,
    label: &str,
    event_name: &str,
    channel: &'static str,
) {
    let envelope = serde_json::json!({
        "channel": channel,
        "payload": Value::Null,
    });
    if let Err(e) = app.emit_to(EventTarget::webview(label), event_name, &envelope) {
        eprintln!("[RemoteProxy] emit_to {label} for {event_name} ({channel}) failed: {e}");
    }
}

/// Clone the current subscriber set so the emit loop holds no locks while
/// calling into Tauri (`emit_to` is sync but may be slow under contention).
async fn snapshot_subscribers(entry: &Arc<WsTaskEntry>) -> Vec<String> {
    entry.subscribers.lock().await.iter().cloned().collect()
}

// ─── Helpers ──────────────────────────────────────────────────────────

/// Connect to the remote WebSocket with subprotocol-based token auth.
/// The remote server's auth middleware (see `web/auth.rs`) accepts either
/// `Authorization: Bearer …` or a subprotocol entry shaped
/// `codeg-token.{base64url(token)}`. The latter is what browser
/// WebSocket clients use because browsers cannot set arbitrary headers
/// on WS handshakes; we follow the same convention here so both transports
/// share one server-side codepath.
async fn connect_with_subprotocol_auth(
    ws_url: &str,
    token: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    let mut request: Request = ws_url
        .into_client_request()
        .map_err(|e| format!("invalid WS URL: {e}"))?;

    let encoded_token = URL_SAFE_NO_PAD.encode(token.trim().as_bytes());
    let protocols_value = format!("{WS_EVENT_PROTOCOL}, {WS_TOKEN_PROTOCOL_PREFIX}{encoded_token}");
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_str(&protocols_value)
            .map_err(|e| format!("invalid subprotocol value: {e}"))?,
    );

    let (stream, _resp) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("connect_async: {e}"))?;
    Ok(stream)
}

/// Convert an `http://…` or `https://…` base URL into the corresponding
/// WebSocket URL ending in `/ws/events`. Anything else is passed through
/// untouched so tungstenite can surface a clean parse error.
fn http_url_to_ws_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}/ws/events")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}/ws/events")
    } else {
        format!("{trimmed}/ws/events")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_url_to_ws_url_http() {
        assert_eq!(
            http_url_to_ws_url("http://localhost:8080"),
            "ws://localhost:8080/ws/events"
        );
    }

    #[test]
    fn http_url_to_ws_url_https_trailing_slash() {
        assert_eq!(
            http_url_to_ws_url("https://example.com/"),
            "wss://example.com/ws/events"
        );
    }

    #[test]
    fn http_url_to_ws_url_unknown_scheme() {
        // tungstenite will reject this, but our helper passes it through.
        assert_eq!(
            http_url_to_ws_url("ftp://example.com"),
            "ftp://example.com/ws/events"
        );
    }
}
