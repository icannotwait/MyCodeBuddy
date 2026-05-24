//! Companion-side MCP protocol — the bits that live inside the `codeg-mcp`
//! binary but are factored out into the library so they can be unit-tested
//! without spawning the binary.
//!
//! The companion speaks newline-delimited JSON-RPC 2.0 on stdio:
//! one request → one response per line. It exposes exactly one tool —
//! `delegate_to_agent` — whose schema is embedded at compile time from
//! [`tool_schema_json`].
//!
//! Notifications (id = None) are silently ignored, matching MCP's expectation
//! that `notifications/initialized` etc. produce no response.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::acp::delegation::transport::{client_round_trip, BrokerRequest};

/// Static MCP tool schema. Lives next to this module so codeg-mcp ships
/// a single embedded copy — no runtime file IO, no version skew with the
/// broker's [`super::types::DelegationRequest`].
pub const TOOL_SCHEMA_JSON: &str = include_str!("tool_schema.json");

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    /// MCP notifications carry no `id`. We dispatch a response only when this
    /// is `Some`.
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

pub fn ok(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}

pub fn err(id: Value, code: i64, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

/// Process arguments threaded through every `tools/call` so the dispatcher
/// can build a [`BrokerRequest`] without re-parsing argv per call.
#[derive(Debug, Clone)]
pub struct CompanionContext {
    pub parent_connection_id: String,
    pub socket_path: String,
    pub token: String,
}

/// Parse, dispatch, and return the response JSON-RPC envelope, or `None` for
/// notifications. The caller is responsible for writing the line to stdout.
///
/// Errors that happen before we have an `id` (parse failures with no `id` in
/// the parsed object) get reported with `id = null`, per JSON-RPC 2.0.
pub async fn handle_line(ctx: &CompanionContext, line: &str) -> Option<JsonRpcResponse> {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return Some(err(Value::Null, -32700, format!("parse error: {e}")));
        }
    };
    let id_opt = req.id.clone();
    let response = handle_request(ctx, req).await;
    // A response is only sent when the request carried an id (i.e. it was a
    // call, not a notification). For notifications we return None even on
    // dispatch errors — that's what the MCP spec requires.
    match (id_opt, response) {
        (Some(_), resp) => resp,
        (None, _) => None,
    }
}

async fn handle_request(ctx: &CompanionContext, req: JsonRpcRequest) -> Option<JsonRpcResponse> {
    let id = req.id.unwrap_or(Value::Null);
    let resp = match req.method.as_str() {
        "initialize" => ok(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {
                    "name": "codeg-mcp",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": { "tools": {} },
            }),
        ),
        "tools/list" => {
            let tool: Value = match serde_json::from_str(TOOL_SCHEMA_JSON) {
                Ok(v) => v,
                Err(e) => return Some(err(id, -32603, format!("embedded schema invalid: {e}"))),
            };
            ok(id, json!({ "tools": [tool] }))
        }
        "tools/call" => handle_tool_call(ctx, id, req.params).await,
        _ => err(id, -32601, format!("method not found: {}", req.method)),
    };
    Some(resp)
}

async fn handle_tool_call(ctx: &CompanionContext, id: Value, params: Value) -> JsonRpcResponse {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    if name != "delegate_to_agent" {
        return err(id, -32602, format!("unknown tool: {name}"));
    }
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    // MCP clients (Codex / Claude Code) generally do NOT populate
    // `_meta.tool_use_id` when calling an MCP server. We still surface it
    // when present (it's the most precise binding), but a missing one is
    // expected — the broker falls back to claiming the most recent
    // `delegate_to_agent` tool_call_id observed on the parent's ACP event
    // stream.
    let tool_use_id = params
        .get("_meta")
        .and_then(|m| m.get("tool_use_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let req = BrokerRequest {
        token: ctx.token.clone(),
        parent_connection_id: ctx.parent_connection_id.clone(),
        parent_tool_use_id: tool_use_id,
        input: arguments,
    };
    match client_round_trip(&ctx.socket_path, &req).await {
        Ok(resp) => ok(id, render_tool_result(&resp.outcome)),
        Err(e) => err(id, -32603, format!("broker round-trip failed: {e}")),
    }
}

/// Map a serialized [`super::types::DelegationOutcome`] into MCP `tools/call`
/// result content. Kept as a separate function so unit tests can assert the
/// mapping without a real socket.
pub fn render_tool_result(outcome: &Value) -> Value {
    let kind = outcome.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let is_error = kind == "err";
    let text = if is_error {
        outcome
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("delegation failed")
            .to_string()
    } else {
        outcome
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
        "structuredContent": outcome.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CompanionContext {
        CompanionContext {
            parent_connection_id: "p1".into(),
            socket_path: "/tmp/nope.sock".into(),
            token: "tok".into(),
        }
    }

    #[tokio::test]
    async fn initialize_returns_protocol_version() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#;
        let resp = handle_line(&ctx(), line).await.unwrap();
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "codeg-mcp");
    }

    #[tokio::test]
    async fn tools_list_returns_delegate_to_agent() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp = handle_line(&ctx(), line).await.unwrap();
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "delegate_to_agent");
        // Schema enumerates all 6 agent types.
        let agents = tools[0]["inputSchema"]["properties"]["agent_type"]["enum"]
            .as_array()
            .unwrap();
        assert_eq!(agents.len(), 6);
    }

    #[tokio::test]
    async fn notification_produces_no_response() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let resp = handle_line(&ctx(), line).await;
        assert!(resp.is_none());
    }

    #[tokio::test]
    async fn parse_error_returns_null_id_error() {
        let line = "not json";
        let resp = handle_line(&ctx(), line).await.unwrap();
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32700);
        assert!(e.message.contains("parse"));
        assert_eq!(resp.id, Value::Null);
    }

    #[tokio::test]
    async fn unknown_method_returns_32601() {
        let line = r#"{"jsonrpc":"2.0","id":9,"method":"resources/list"}"#;
        let resp = handle_line(&ctx(), line).await.unwrap();
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32601);
    }

    #[tokio::test]
    async fn tools_call_with_unknown_tool_rejected() {
        let line = r#"{
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/call",
            "params": {
                "name": "other_tool",
                "arguments": {},
                "_meta": {"tool_use_id": "tu1"}
            }
        }"#;
        let resp = handle_line(&ctx(), line).await.unwrap();
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32602);
        assert!(e.message.contains("other_tool"));
    }

    #[tokio::test]
    async fn tools_call_without_tool_use_id_passes_through_to_broker() {
        // MCP clients (Codex / Claude Code) generally don't fill
        // `_meta.tool_use_id`, so the companion must NOT reject the call —
        // it must forward to the broker, which falls back to claiming the
        // most recent ACP-side tool_call_id. With a bogus socket path the
        // round-trip fails downstream, surfacing as -32603 (NOT -32602).
        let line = r#"{
            "jsonrpc":"2.0",
            "id":4,
            "method":"tools/call",
            "params": {
                "name": "delegate_to_agent",
                "arguments": {"agent_type": "codex", "task": "x"}
            }
        }"#;
        let resp = handle_line(&ctx(), line).await.unwrap();
        let e = resp.error.unwrap();
        assert_eq!(e.code, -32603);
        assert!(e.message.contains("broker round-trip"));
    }

    #[test]
    fn render_tool_result_maps_ok_outcome() {
        let outcome = json!({"kind": "ok", "text": "hi", "child_conversation_id": 42});
        let rendered = render_tool_result(&outcome);
        assert_eq!(rendered["isError"], false);
        assert_eq!(rendered["content"][0]["text"], "hi");
        assert_eq!(rendered["structuredContent"]["child_conversation_id"], 42);
    }

    #[test]
    fn render_tool_result_maps_err_outcome() {
        let outcome = json!({
            "kind": "err",
            "code": "timeout",
            "message": "timeout after 5000ms"
        });
        let rendered = render_tool_result(&outcome);
        assert_eq!(rendered["isError"], true);
        assert_eq!(rendered["content"][0]["text"], "timeout after 5000ms");
        assert_eq!(rendered["structuredContent"]["code"], "timeout");
    }
}
