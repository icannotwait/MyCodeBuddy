// WebSocket event bridge: subscribes to a remote daemon's `/ws/events`
// stream and re-emits every frame on the desktop's local `EventEmitter`.
// One bridge runs per Live SSH connection; it is spawned alongside the
// reconnect supervisor and torn down via the same kill-switch.
//
// Wire format (matches `web::event_bridge::WebEvent`):
//   { "channel": "<event-name>", "payload": <arbitrary JSON> }
//
// The bridge is intentionally dumb: it forwards every event regardless of
// channel name. Routing back to specific frontend connections happens via
// the standard envelope `connection_id` field already embedded in payloads.
//
// Self-retry (Phase C): connect failures and mid-stream errors trigger an
// exponential backoff retry (1/2/4/8/16s, capped) until the killer fires.
// The supervisor (CG-002.7) is the authority on whether the SSH-level
// connection is dead — the bridge just keeps trying until told to stop.
//
// Snapshot rehydrate (Phase D): on every reconnect after the first, the
// bridge emits a `connection://remote_resync` event so the frontend can
// re-hydrate ACP session snapshots for connections living on this SSH.
// Events emitted by the daemon during the WS gap are otherwise lost.

use std::time::Duration;

use serde::Deserialize;
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

use crate::web::event_bridge::{emit_event, EventEmitter};

#[derive(Debug, Deserialize)]
struct DaemonEventFrame {
    channel: String,
    payload: serde_json::Value,
}

const MAX_BACKOFF_SECS: u64 = 16;

/// Run a bridge against `ws://127.0.0.1:<local_port>/ws/events?token=...`.
///
/// `ssh_connection_id` is the desktop's SSH connection id (used to label the
/// `connection://remote_resync` event so the frontend can re-hydrate the
/// matching ACP sessions; see Phase D dev design).
///
/// The bridge auths via the `?token=` query string (matches
/// `web::auth::require_token`). Returns only when the killer fires.
pub async fn bridge_loop(
    ssh_connection_id: String,
    local_port: u16,
    token: String,
    emitter: EventEmitter,
    mut killer: oneshot::Receiver<()>,
) {
    let url = format!(
        "ws://127.0.0.1:{}/ws/events?token={}",
        local_port,
        urlencoding::encode(&token)
    );
    let mut backoff_secs: u64 = 1;
    let mut is_first_connect = true;

    loop {
        // 1. Connect (killer-aware).
        let connect_fut = Box::pin(tokio_tungstenite::connect_async(&url));
        let ws_stream = tokio::select! {
            biased;
            _ = &mut killer => return,
            res = connect_fut => match res {
                Ok((stream, _)) => stream,
                Err(e) => {
                    eprintln!(
                        "[Remote ws-bridge] connect failed (retry in {}s): {e}",
                        backoff_secs
                    );
                    let sleep = tokio::time::sleep(Duration::from_secs(backoff_secs));
                    tokio::pin!(sleep);
                    tokio::select! {
                        biased;
                        _ = &mut killer => return,
                        _ = &mut sleep => {}
                    }
                    backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
                    continue;
                }
            }
        };

        eprintln!(
            "[Remote ws-bridge] connected ssh={} port={}",
            ssh_connection_id, local_port
        );
        backoff_secs = 1;
        if !is_first_connect {
            // Reconnect: tell the frontend to re-hydrate snapshots for ACP
            // sessions on this SSH. Daemon-emitted events during the WS gap
            // are otherwise lost; HYDRATE_FROM_SNAPSHOT closes that gap.
            emit_event(
                &emitter,
                "connection://remote_resync",
                serde_json::json!({ "ssh_connection_id": ssh_connection_id.clone() }),
            );
        }
        is_first_connect = false;

        use futures_util::StreamExt;
        let (_write, mut read) = ws_stream.split();

        // 2. Read until close / error / killer.
        let mut killer_fired = false;
        loop {
            tokio::select! {
                biased;
                _ = &mut killer => { killer_fired = true; break; }
                msg = read.next() => {
                    let Some(msg) = msg else {
                        eprintln!("[Remote ws-bridge] stream ended (will reconnect)");
                        break;
                    };
                    let msg = match msg {
                        Ok(m) => m,
                        Err(e) => {
                            eprintln!("[Remote ws-bridge] read error (will reconnect): {e}");
                            break;
                        }
                    };
                    match msg {
                        Message::Text(text) => relay_frame(&emitter, &text),
                        Message::Binary(_)
                        | Message::Ping(_)
                        | Message::Pong(_)
                        | Message::Frame(_) => {}
                        Message::Close(_) => {
                            eprintln!("[Remote ws-bridge] daemon closed (will reconnect)");
                            break;
                        }
                    }
                }
            }
        }

        if killer_fired {
            return;
        }
        // Outer loop reconnects with the (still-1s) backoff.
    }
}

fn relay_frame(emitter: &EventEmitter, text: &str) {
    let frame: DaemonEventFrame = match serde_json::from_str(text) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[Remote ws-bridge] frame parse error: {e}");
            return;
        }
    };
    emit_event(emitter, &frame.channel, frame.payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let raw = r#"{"channel":"acp://event","payload":{"connection_id":"abc","type":"agent_message_start"}}"#;
        let frame: DaemonEventFrame = serde_json::from_str(raw).unwrap();
        assert_eq!(frame.channel, "acp://event");
        assert_eq!(
            frame.payload.get("connection_id").and_then(|v| v.as_str()),
            Some("abc")
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn killer_during_initial_connect_returns_quickly() {
        // Use port 1 (privileged) which fails to connect on macOS without
        // sudo; the bridge should burn through retries while the killer
        // races the backoff sleep. We fire the killer immediately so the
        // very first sleep wakes up right away.
        let (kill_tx, kill_rx) = oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            bridge_loop("ssh-1".into(), 1, "tok".into(), EventEmitter::Noop, kill_rx)
                .await;
        });

        // Yield once so the bridge can attempt its first connect_async, then
        // signal kill. Under `start_paused = true`, time advances on demand
        // — `tokio::time::advance` would fast-forward the backoff sleep, but
        // the killer should preempt it regardless.
        tokio::task::yield_now().await;
        let _ = kill_tx.send(());

        // Bound: bridge must return within a small wall-clock budget even
        // though `connect_async` typically fails after a TCP timeout. The
        // killer races inside both the connect and sleep selects; if the
        // wiring is right we exit immediately.
        let res =
            tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(res.is_ok(), "bridge_loop did not exit on killer in time");
    }
}
