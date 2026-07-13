//! ACP `session/fork` support via raw JSON-RPC messages.
//!
//! The `sacp` crate does not yet provide typed request/response types for
//! `session/fork`, so we use `UntypedMessage` (the same pattern used for
//! `session/set_config_option` in connection.rs).

use sacp::schema::{ForkSessionRequest, ForkSessionResponse, Meta, SessionId};
use sacp::{Agent, ConnectionTo, UntypedMessage};

use crate::acp::error::AcpError;

/// Build a `session/fork` request with the connection's terminal metadata.
///
/// Separated so unit tests can assert serialized `_meta` without a live
/// connection. Callers must pass metadata built from the immutable connection
/// shell snapshot (never re-read global terminal settings).
pub fn build_fork_session_request(
    session_id: SessionId,
    cwd: impl Into<std::path::PathBuf>,
    terminal_meta: Meta,
) -> ForkSessionRequest {
    ForkSessionRequest::new(session_id, cwd).meta(terminal_meta)
}

/// Send a `session/fork` request over an existing ACP connection.
///
/// Returns the full `ForkSessionResponse` so the caller can attach directly
/// without a separate `session/load` round-trip.
///
/// `terminal_meta` must come from the connection's launch shell snapshot
/// (via [`crate::acp::terminal_context::terminal_metadata`]); fork never
/// reads system terminal settings.
pub async fn fork_session(
    cx: &ConnectionTo<Agent>,
    session_id: &SessionId,
    cwd: &str,
    terminal_meta: Meta,
) -> Result<ForkSessionResponse, AcpError> {
    let req = build_fork_session_request(session_id.clone(), cwd, terminal_meta);
    let untyped_req = UntypedMessage::new("session/fork", &req)
        .map_err(|e| AcpError::protocol(format!("Failed to build fork request: {e}")))?;

    let raw_response: serde_json::Value = cx
        .send_request_to(Agent, untyped_req)
        .block_task()
        .await
        .map_err(|e| AcpError::protocol(format!("session/fork failed: {e}")))?;

    let response: ForkSessionResponse = serde_json::from_value(raw_response)
        .map_err(|e| AcpError::protocol(format!("Failed to parse fork response: {e}")))?;

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::terminal_adapter::adapter_for;
    use crate::acp::terminal_context::terminal_metadata;
    use crate::models::agent::AgentType;
    use crate::terminal::shell::test_support::{
        posix_spec as test_posix_spec, pwsh_spec as test_pwsh_spec,
    };

    fn assert_terminal_meta(value: &serde_json::Value, dialect: &str, shell: &str) {
        let term = &value["_meta"]["codeg.dev/terminal"];
        assert_eq!(term["dialect"], dialect);
        assert_eq!(term["shell"], shell);
        assert_eq!(term["platform"], std::env::consts::OS);
        assert_eq!(term["commandMode"], "selected-shell-for-command-lines");
    }

    #[test]
    fn fork_request_contains_terminal_metadata() {
        let spec = test_pwsh_spec();
        let meta = terminal_metadata(Meta::default(), &spec, adapter_for(AgentType::Codex)).unwrap();
        let req = build_fork_session_request(
            SessionId::new("s-fork"),
            "/tmp/project",
            meta,
        );
        let value = serde_json::to_value(req).unwrap();
        assert_terminal_meta(
            &value,
            "powershell",
            &spec.executable.to_string_lossy(),
        );
        assert_eq!(value["sessionId"], "s-fork");
    }

    #[test]
    fn fork_request_terminal_metadata_uses_posix_snapshot() {
        let spec = test_posix_spec();
        let meta =
            terminal_metadata(Meta::default(), &spec, adapter_for(AgentType::ClaudeCode)).unwrap();
        let req = build_fork_session_request(SessionId::new("s2"), "/tmp/p", meta);
        let value = serde_json::to_value(req).unwrap();
        assert_terminal_meta(&value, "posix", "/bin/sh");
    }
}
