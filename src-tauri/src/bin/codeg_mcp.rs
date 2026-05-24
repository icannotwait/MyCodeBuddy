//! `codeg-mcp` — the per-launch stdio MCP companion that an agent CLI runs
//! to surface the `delegate_to_agent` tool to its LLM.
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
//! Everything heavyweight — JSON-RPC dispatch, UDS round-trip, MCP tool
//! schema — lives in `codeg_lib::acp::delegation::{companion, transport}`
//! so it's unit-testable without spawning a process.

use std::io::Write;
use std::process::ExitCode;

use codeg_lib::acp::delegation::companion::{handle_line, CompanionContext};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Args {
    parent_connection_id: String,
    socket_path: String,
    token: String,
}

fn parse_args() -> Result<Args, String> {
    let mut parent_connection_id = None;
    let mut socket_path = None;
    let mut token = None;

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--parent-connection-id" => {
                parent_connection_id = Some(
                    iter.next()
                        .ok_or_else(|| "--parent-connection-id requires a value".to_string())?,
                );
            }
            "--socket-path" => {
                socket_path = Some(
                    iter.next()
                        .ok_or_else(|| "--socket-path requires a value".to_string())?,
                );
            }
            "--token" => {
                token = Some(
                    iter.next()
                        .ok_or_else(|| "--token requires a value".to_string())?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "codeg-mcp --parent-connection-id <uuid> --socket-path <path> --token <secret>"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok(Args {
        parent_connection_id: parent_connection_id
            .ok_or_else(|| "missing --parent-connection-id".to_string())?,
        socket_path: socket_path.ok_or_else(|| "missing --socket-path".to_string())?,
        token: token.ok_or_else(|| "missing --token".to_string())?,
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "codeg-mcp: {e}");
            return ExitCode::from(2);
        }
    };
    let ctx = CompanionContext {
        parent_connection_id: args.parent_connection_id,
        socket_path: args.socket_path,
        token: args.token,
    };

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break, // parent closed stdin → graceful exit
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "codeg-mcp: read stdin: {e}");
                return ExitCode::from(1);
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(resp) = handle_line(&ctx, line).await {
            match serde_json::to_string(&resp) {
                Ok(serialized) => {
                    if let Err(e) = stdout.write_all(serialized.as_bytes()).await {
                        let _ = writeln!(std::io::stderr(), "codeg-mcp: write stdout: {e}");
                        return ExitCode::from(1);
                    }
                    if let Err(e) = stdout.write_all(b"\n").await {
                        let _ = writeln!(std::io::stderr(), "codeg-mcp: write stdout: {e}");
                        return ExitCode::from(1);
                    }
                    if let Err(e) = stdout.flush().await {
                        let _ = writeln!(std::io::stderr(), "codeg-mcp: flush stdout: {e}");
                        return ExitCode::from(1);
                    }
                }
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "codeg-mcp: encode response: {e}");
                    return ExitCode::from(1);
                }
            }
        }
    }
    ExitCode::SUCCESS
}
