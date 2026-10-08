//! `codeg-mcp` — the per-launch stdio MCP companion that an agent CLI runs
//! to surface codeg's tools to its LLM: the multi-agent delegation tools
//! (`delegate_to_agent` etc.), coordination decision tools
//! (`request_parent_decision` / `reply_to_delegation`, role-gated),
//! `check_user_feedback` (pull the user's mid-turn steering notes),
//! `ask_user_question` (block on a multiple-choice card), and
//! `get_session_info` (resolve a referenced session by id), gated by the
//! `--features` groups (`delegation` / `coordination_v1` / `feedback` /
//! `ask` / `sessions` / `workflow_v2`) and `--role` (`root` | `delegation_child`).
//!
//! Service roundtable mode is a separate entry:
//! `codeg-mcp --service-roundtable --socket-path <path> --incarnation <id>`.
//! Its attempt token arrives only in the child environment. That mode does
//! not read a host parent PID and watches broker EOF instead.
//!
//! The agent's MCP config (injected by codeg via `load_mcp_servers_for_agent`)
//! spawns this binary with three required flags:
//!
//!   codeg-mcp \
//!     --parent-connection-id <uuid> \
//!     --socket-path <abs path> \
//!     --token <ephemeral secret>
//!
//! All three are required and the binary exits early if any is missing.
//! `--disabled-agents` may narrow the closed built-in delegate target enum.
//! `--custom-agents` is accepted for launcher compatibility but never expands
//! that public schema.
//! Everything heavyweight — JSON-RPC dispatch, UDS round-trip, MCP tool
//! schema, cancellation tracking — lives in
//! `codeg_lib::acp::delegation::{companion, transport}` so it's
//! unit-testable without spawning a process.
//!
//! Stdin lines are dispatched concurrently: synchronous methods
//! (`initialize`, `tools/list`) emit a response inline, `tools/call`
//! spawns a tokio task that drives the UDS round-trip racing a cancel
//! channel, and `notifications/cancelled` wakes the relevant task without
//! blocking the reader. Stdout writes are serialized through a mutex so
//! interleaved frames never corrupt the wire.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use codeg_lib::acp::delegation::companion::{
    dispatch_line, drain_and_cancel_all, serialize_jsonrpc_line, InflightCalls, JsonRpcResponse,
    LineAction,
};
use codeg_lib::acp::delegation::parent_watcher::{wait_for_parent_exit, DEFAULT_POLL_INTERVAL};
use codeg_lib::acp::delegation::transport::client_establish_ready_lease;
use codeg_lib::roundtable::{
    bind_service_process, legacy_companion_context, parse_companion_args, CompanionMode,
    CompanionParse,
};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// Serialize a `JsonRpcResponse` and append a newline; small enough to keep
/// inline so the write-mutex critical section stays tight.
async fn write_response<W: AsyncWrite + Unpin>(
    stdout: &Arc<Mutex<W>>,
    resp: &JsonRpcResponse,
) -> std::io::Result<()> {
    let mut guard = stdout.lock().await;
    write_response_locked(&mut *guard, resp).await
}

async fn write_response_locked<W: AsyncWrite + Unpin>(
    stdout: &mut W,
    resp: &JsonRpcResponse,
) -> std::io::Result<()> {
    let frame = serialize_jsonrpc_line(resp).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
    })?;
    stdout.write_all(&frame).await?;
    stdout.flush().await?;
    Ok(())
}

async fn run_service(socket_path: &str, incarnation: &str) -> ExitCode {
    let env = std::env::vars().collect::<std::collections::HashMap<_, _>>();
    let process = match bind_service_process(socket_path, incarnation, &env) {
        Ok(process) => process,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "codeg-mcp: {}", err.message);
            return ExitCode::from(2);
        }
    };
    if process.reads_host_parent_pid()
        || process.parent_pid().is_some()
        || !process.token_is_present()
    {
        return ExitCode::from(2);
    }
    let connection = match process.connect().await {
        Ok(connection) => connection,
        Err(_) => return ExitCode::from(2),
    };
    let relay = match env.get("CODEG_RT_MODEL_SOCKET") {
        None => None,
        Some(path) if path == "/run/codeg/gateway.sock" => {
            match codeg_lib::roundtable::ServiceModelRelay::start().await {
                Ok(relay) => Some(relay),
                Err(_) => return ExitCode::from(2),
            }
        }
        Some(_) => return ExitCode::from(2),
    };
    let result = connection
        .serve_stdio(tokio::io::stdin(), tokio::io::stdout())
        .await;
    if let Some(relay) = relay {
        relay.shutdown().await;
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(2),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // Stderr-only subscriber: stdout is the JSON-RPC protocol channel, and
    // concurrent mcp processes share no log file. No hub/buffer/emitter.
    let _log_guard = codeg_lib::logging::init::init_mcp();

    let parsed = match parse_companion_args(std::env::args().skip(1)) {
        Ok(parsed) => parsed,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "codeg-mcp: {e}");
            return ExitCode::from(2);
        }
    };
    let args = match parsed {
        CompanionParse::Help(text) => {
            println!("{text}");
            return ExitCode::SUCCESS;
        }
        CompanionParse::Launch(CompanionMode::ServiceRoundtable {
            socket_path,
            incarnation,
        }) => {
            return run_service(&socket_path, &incarnation).await;
        }
        CompanionParse::Launch(CompanionMode::LegacyParent(args)) => args,
    };
    let ctx = legacy_companion_context(&args);

    // When delegation is enabled, establish the authenticated ready lease
    // BEFORE serving stdio tools. Failure exits non-zero so the agent never
    // sees a half-ready companion.
    let mut ready_hold = if ctx.features.delegation {
        match client_establish_ready_lease(&args.socket_path, &args.token).await {
            Ok(hold) => Some(hold),
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "codeg-mcp: ready lease failed: {e}");
                return ExitCode::from(3);
            }
        }
    } else {
        None
    };

    let stdin = tokio::io::stdin();
    let stdout = Arc::new(Mutex::new(tokio::io::stdout()));
    let inflight = Arc::new(InflightCalls::new());
    let mut lines = BufReader::new(stdin).lines();

    // Optional parent-PID watchdog. Composed as a separate future so the
    // main loop can race it against stdin reads via `tokio::select!`;
    // when no PID was provided we substitute a never-ready future, which
    // tokio's branch evaluation skips for free.
    let watchdog: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        match args.parent_pid {
            Some(pid) => Box::pin(wait_for_parent_exit(pid, DEFAULT_POLL_INTERVAL)),
            None => Box::pin(std::future::pending()),
        };
    tokio::pin!(watchdog);

    let lease_closed = async {
        match ready_hold.take() {
            Some(hold) => hold.wait_until_closed().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(lease_closed);

    loop {
        tokio::select! {
            // Bias toward parent-exit detection: if the watchdog fires
            // mid-stdin-read we want to bail rather than finish a round-trip
            // whose response no one will read.
            biased;
            _ = &mut watchdog => {
                // Best-effort: cancel every in-flight delegation BEFORE we
                // hard-exit so the broker doesn't park each pending row
                // on `rx.await` waiting for a TurnComplete it can never
                // deliver. cancel_by_parent on the codeg main side is the
                // ultimate backstop, but firing the explicit cancels here
                // closes the window between MCP shutdown and parent ACP
                // disconnect detection on the codeg side.
                drain_and_cancel_all(&ctx, &inflight, "parent process exited").await;
                let _ = writeln!(
                    std::io::stderr(),
                    "codeg-mcp: parent process exited, shutting down"
                );
                // Hard exit on purpose: `tokio::io::stdin()` parks a
                // blocking worker thread that the runtime can't cancel,
                // so returning normally would keep the process alive
                // until the parent agent CLI also closes stdin — defeating
                // the watchdog. The agent CLI sees the stdout pipe close
                // and tears down its MCP client cleanly.
                std::process::exit(0);
            }
            _ = &mut lease_closed => {
                drain_and_cancel_all(&ctx, &inflight, "ready lease closed").await;
                let _ = writeln!(
                    std::io::stderr(),
                    "codeg-mcp: ready lease closed, shutting down"
                );
                std::process::exit(0);
            }
            line_result = lines.next_line() => {
                let line = match line_result {
                    Ok(Some(l)) => l,
                    Ok(None) => {
                        // Parent closed stdin. Same shutdown rationale as
                        // the watchdog branch: drain pending delegations
                        // before returning so the broker can resolve them
                        // immediately.
                        drain_and_cancel_all(&ctx, &inflight, "companion stdio closed").await;
                        break;
                    }
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "codeg-mcp: read stdin: {e}");
                        drain_and_cancel_all(&ctx, &inflight, "companion stdin error").await;
                        return ExitCode::from(1);
                    }
                };
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let action = dispatch_line(&ctx, inflight.clone(), &line).await;
                match action {
                    LineAction::Respond(resp) => {
                        if let Err(e) = write_response(&stdout, &resp).await {
                            let _ = writeln!(std::io::stderr(), "codeg-mcp: write stdout: {e}");
                            return ExitCode::from(1);
                        }
                    }
                    LineAction::Spawn(spawned) => {
                        // Drive the round-trip in a detached task so the
                        // stdin reader stays responsive — the next line may
                        // be a `notifications/cancelled` for THIS request.
                        let stdout = stdout.clone();
                        tokio::spawn(async move {
                            let result = spawned.future.await;
                            let mut stdout = stdout.lock().await;
                            // Claim under the stdout mutex so cancellation
                            // remains authoritative while a response waits to
                            // relay behind another writer.
                            let Some((resp, after_relay)) = result.claim_relay().await else {
                                return;
                            };
                            if let Err(e) = write_response_locked(&mut *stdout, &resp).await {
                                let _ = writeln!(
                                    std::io::stderr(),
                                    "codeg-mcp: write stdout: {e}"
                                );
                                // Relay failed (agent stdin gone) → skip any
                                // post-relay action so feedback notes stay
                                // pending for the next check (at-least-once).
                                return;
                            }
                            drop(stdout);
                            // The response reached the agent's stdin. Only now
                            // run any post-relay action — for
                            // `check_user_feedback`, the delivery commit that
                            // marks the pulled notes `Delivered`.
                            if let Some(after) = after_relay {
                                after.await;
                            }
                        });
                    }
                    LineAction::Silent => {}
                }
            }
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use codeg_lib::acp::delegation::companion::{CompanionContext, CompanionFeatures};
    use codeg_lib::acp::delegation::transport::CompanionRole;
    use serde_json::{json, Value};
    use tokio::io::AsyncWrite;

    use super::*;

    #[derive(Default)]
    struct RecordingWriter {
        writes: Vec<Vec<u8>>,
        flushes: usize,
    }

    impl AsyncWrite for RecordingWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.writes.push(buf.to_vec());
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            self.flushes += 1;
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[cfg(windows)]
    fn workflow_broker_with_outcome(outcome: Value) -> (String, tokio::task::JoinHandle<()>) {
        use codeg_lib::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, BrokerResponse,
        };
        use tokio::net::windows::named_pipe::ServerOptions;

        let pipe_name = format!(
            r"\\.\pipe\codeg-writer-parity-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)
            .unwrap();
        let task = tokio::spawn(async move {
            server.connect().await.unwrap();
            let message: BrokerMessage = read_frame(&mut server).await.unwrap();
            assert!(matches!(message, BrokerMessage::GetWorkflowState(_)));
            write_frame(&mut server, &BrokerResponse { outcome })
                .await
                .unwrap();
        });
        (pipe_name, task)
    }

    #[cfg(unix)]
    fn workflow_broker_with_outcome(outcome: Value) -> (String, tokio::task::JoinHandle<()>) {
        use codeg_lib::acp::delegation::transport::{
            read_frame, write_frame, BrokerMessage, BrokerResponse,
        };
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("writer-parity.sock");
        let socket = socket_path.to_string_lossy().to_string();
        let listener = UnixListener::bind(&socket_path).unwrap();
        let task = tokio::spawn(async move {
            let _dir = dir;
            let (mut stream, _) = listener.accept().await.unwrap();
            let message: BrokerMessage = read_frame(&mut stream).await.unwrap();
            assert!(matches!(message, BrokerMessage::GetWorkflowState(_)));
            write_frame(&mut stream, &BrokerResponse { outcome })
                .await
                .unwrap();
        });
        (socket, task)
    }

    async fn dispatched_workflow_response(outcome: Value, id: Value) -> JsonRpcResponse {
        let (socket_path, server) = workflow_broker_with_outcome(outcome);
        let context = CompanionContext {
            parent_connection_id: "parent".into(),
            socket_path,
            token: "token".into(),
            features: CompanionFeatures {
                delegation: true,
                coordination_v1: false,
                feedback: false,
                ask: false,
                sessions: false,
                compact_catalog: false,
                workflow_v2: true,
                completion_v2: false,
                tasks: false,
                automations: false,
                taskboard: false,
                browser: false,
                browser_eval: false,
                computer: false,
                computer_clipboard: false,
                computer_launch: false,
            },
            role: CompanionRole::Root,
            can_spawn_child: true,
            connection_incarnation_id: "test-incarnation".into(),
            disabled_agents: Vec::new(),
        };
        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": "get_workflow_state", "arguments": {} }
        })
        .to_string();
        let LineAction::Spawn(spawned) =
            dispatch_line(&context, Arc::new(InflightCalls::new()), &line).await
        else {
            panic!("expected get_workflow_state spawn");
        };
        let response = spawned.future.await.response.unwrap();
        server.await.unwrap();
        response
    }

    #[tokio::test]
    async fn write_response_emits_one_complete_jsonl_write() {
        use codeg_lib::acp::delegation::companion::{
            GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES, GET_WORKFLOW_STATE_MAX_RESULT_BYTES,
        };

        let request_id = Value::String("x".repeat(GET_WORKFLOW_STATE_MAX_REQUEST_ID_BYTES - 2));
        let synthetic_message = "deterministic persistence failure ".repeat(512);
        let oversized_workflow_id = "missing-workflow-quote\"slash\\界".repeat(600);
        let large_success = dispatched_workflow_response(
            json!({
                "workflow_id": format!("wf-{}", "x".repeat(5_000)),
                "parent_conversation_id": 42,
                "workflow_kind": "brainstorm_to_delivery",
                "capability_version": "workflow_manifest_v2",
                "publication_token": "publication-token",
                "workflow_state": "estimated",
                "manifest_revision": 7,
                "graph_revision": 11,
                "schema_version": 2,
                "plan_target_rel_path": "docs/plan.md",
                "risk_policy_version": "b2d_task_risk_v1",
                "completion_protocol": {
                    "version": 2,
                    "mode": "v2_enforce",
                    "creation_mode": "v2_enforce",
                    "automatic_root_wake": true
                },
                "detail": "index",
                "inline_findings": false,
                "payload_truncated": false,
                "evidence_truncated": false,
                "gates": [],
                "actionable_task_routes": [],
                "_codeg_omission_state": { "nodes": [] }
            }),
            request_id.clone(),
        )
        .await;
        let bounded_error = dispatched_workflow_response(
            json!({
                "error": {
                    "code": "persistence",
                    "message": format!("{synthetic_message}: {oversized_workflow_id}")
                }
            }),
            request_id,
        )
        .await;

        for (response, expected_is_error) in [(large_success, false), (bounded_error, true)] {
            let stdout = Arc::new(Mutex::new(RecordingWriter::default()));
            let measured = serialize_jsonrpc_line(&response).unwrap();
            write_response(&stdout, &response).await.unwrap();

            let writer = stdout.lock().await;
            assert_eq!(writer.writes.len(), 1, "response must use one write");
            assert_eq!(writer.flushes, 1, "response must flush once");
            let frame = &writer.writes[0];
            assert_eq!(frame.last(), Some(&b'\n'));
            assert_eq!(frame, &measured, "writer bytes must equal measured bytes");
            assert!(frame.len() <= GET_WORKFLOW_STATE_MAX_RESULT_BYTES);
            let decoded: JsonRpcResponse =
                serde_json::from_slice(&frame[..frame.len() - 1]).unwrap();
            assert_eq!(decoded.id, response.id);
            let result = decoded.result.unwrap();
            assert_eq!(result["isError"], expected_is_error);
            if expected_is_error {
                assert_eq!(result["structuredContent"]["error"]["code"], "persistence");
            }
            let text = String::from_utf8(frame.clone()).unwrap();
            assert!(!text.contains(&synthetic_message));
            assert!(!text.contains(&oversized_workflow_id));
        }
    }
}
