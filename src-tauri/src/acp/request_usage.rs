//! Normalized per-request (or per-turn) output usage extracted from agent
//! notifications.
//!
//! Agents speak different shapes; this module is the extension point. Add a
//! parser here and emit [`RequestUsage`] — the live gauge and the persisted
//! overlay do not care which agent produced it.

use serde::{Deserialize, Serialize};

/// Produced tokens for one completed model request (Claude / Codex) or one
/// completed user turn (Grok `turn_completed`).
///
/// `output_tokens` is the agent's billed output, which already includes
/// thinking / reasoning for the three agents we wire today. `duration_ms` is
/// the agent-reported generation span when it has one (Grok `apiDurationMs`);
/// `None` means the client should measure the request clock itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestUsage {
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Pull a request-usage sample out of a raw ACP / extension notification.
///
/// `method` is the JSON-RPC method; `params` is the notification params object.
/// Returns `None` when the payload is not a usage-bearing shape we know.
pub fn extract_request_usage(method: &str, params: &serde_json::Value) -> Option<RequestUsage> {
    extract_claude_sdk_request_usage(method, params)
        .or_else(|| extract_session_update_request_usage(method, params))
}

fn extract_claude_sdk_request_usage(
    method: &str,
    params: &serde_json::Value,
) -> Option<RequestUsage> {
    if method != "_claude/sdkMessage" {
        return None;
    }
    let message = params.get("message")?;
    if message.get("type").and_then(|v| v.as_str()) != Some("assistant") {
        return None;
    }
    let parented = message
        .get("parent_tool_use_id")
        .or_else(|| message.get("parentToolUseId"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .is_some();
    if parented {
        return None;
    }
    let usage = message
        .get("message")
        .and_then(|m| m.get("usage"))
        .or_else(|| message.get("usage"))?;
    let output_tokens = json_u64(usage, &["output_tokens", "outputTokens"])?;
    if output_tokens == 0 {
        return None;
    }
    Some(RequestUsage {
        output_tokens,
        duration_ms: None,
    })
}

fn extract_session_update_request_usage(
    method: &str,
    params: &serde_json::Value,
) -> Option<RequestUsage> {
    if method != "session/update" && method != "_x.ai/session/update" {
        return None;
    }
    let update = params.get("update")?;
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(|v| v.as_str())?;
    match kind {
        "turn_completed" => extract_grok_turn_completed_usage(update),
        "usage_update" => extract_codex_usage_update(update),
        "request_usage" => extract_generic_request_usage(update),
        _ => None,
    }
}

fn extract_grok_turn_completed_usage(update: &serde_json::Value) -> Option<RequestUsage> {
    let usage = update.get("usage")?;
    let output_tokens = json_u64(usage, &["outputTokens", "output_tokens"])?;
    if output_tokens == 0 {
        return None;
    }
    Some(RequestUsage {
        output_tokens,
        duration_ms: json_u64(usage, &["apiDurationMs", "api_duration_ms"]),
    })
}

/// Extension-point payload for agents that can emit a dedicated sample
/// without piggy-backing on `usage_update` / `turn_completed`.
fn extract_generic_request_usage(update: &serde_json::Value) -> Option<RequestUsage> {
    let output_tokens = json_u64(update, &["outputTokens", "output_tokens"])?;
    if output_tokens == 0 {
        return None;
    }
    Some(RequestUsage {
        output_tokens,
        duration_ms: json_u64(update, &["durationMs", "duration_ms", "apiDurationMs"]),
    })
}

/// Codex `usage_update` carries context `used`/`size`. Request output rides in
/// `_meta.codeg.outputTokens` (or a top-level `outputTokens` extra) so the
/// typed ACP schema can keep ignoring unknown fields.
fn extract_codex_usage_update(update: &serde_json::Value) -> Option<RequestUsage> {
    let output_tokens = update
        .pointer("/_meta/codeg/outputTokens")
        .and_then(as_u64)
        .or_else(|| json_u64(update, &["outputTokens", "output_tokens"]))?;
    if output_tokens == 0 {
        return None;
    }
    Some(RequestUsage {
        output_tokens,
        duration_ms: None,
    })
}

fn json_u64(obj: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(v) = obj.get(*key).and_then(as_u64) {
            return Some(v);
        }
    }
    None
}

fn as_u64(v: &serde_json::Value) -> Option<u64> {
    if let Some(u) = v.as_u64() {
        return Some(u);
    }
    let i = v.as_i64()?;
    u64::try_from(i).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_assistant_sdk_message_yields_output_tokens() {
        let usage = extract_request_usage(
            "_claude/sdkMessage",
            &json!({
                "sessionId": "s",
                "message": {
                    "type": "assistant",
                    "message": {
                        "usage": { "input_tokens": 100, "output_tokens": 42 }
                    }
                }
            }),
        )
        .expect("claude usage");
        assert_eq!(
            usage,
            RequestUsage {
                output_tokens: 42,
                duration_ms: None
            }
        );
    }

    #[test]
    fn claude_parented_assistant_usage_is_ignored() {
        assert!(extract_request_usage(
            "_claude/sdkMessage",
            &json!({
                "sessionId": "s",
                "message": {
                    "type": "assistant",
                    "parent_tool_use_id": "toolu_subagent",
                    "message": {
                        "usage": { "input_tokens": 10, "output_tokens": 42 }
                    }
                }
            }),
        )
        .is_none());
    }

    #[test]
    fn claude_api_retry_and_user_messages_are_ignored() {
        assert!(extract_request_usage(
            "_claude/sdkMessage",
            &json!({
                "sessionId": "s",
                "message": { "type": "system", "subtype": "api_retry" }
            }),
        )
        .is_none());
        assert!(extract_request_usage(
            "_claude/sdkMessage",
            &json!({
                "sessionId": "s",
                "message": { "type": "user", "message": { "content": "hi" } }
            }),
        )
        .is_none());
    }

    #[test]
    fn grok_turn_completed_yields_output_and_api_duration() {
        let usage = extract_request_usage(
            "session/update",
            &json!({
                "update": {
                    "sessionUpdate": "turn_completed",
                    "stop_reason": "end_turn",
                    "usage": {
                        "outputTokens": 2105,
                        "reasoningTokens": 1511,
                        "apiDurationMs": 40849,
                        "modelCalls": 2
                    }
                }
            }),
        )
        .expect("grok usage");
        assert_eq!(
            usage,
            RequestUsage {
                output_tokens: 2105,
                duration_ms: Some(40849)
            }
        );
    }

    #[test]
    fn grok_xai_private_method_also_maps() {
        let usage = extract_request_usage(
            "_x.ai/session/update",
            &json!({
                "update": {
                    "sessionUpdate": "turn_completed",
                    "usage": { "outputTokens": 9, "apiDurationMs": 100 }
                }
            }),
        )
        .expect("xai method");
        assert_eq!(usage.output_tokens, 9);
        assert_eq!(usage.duration_ms, Some(100));
    }

    #[test]
    fn grok_turn_completed_without_usage_is_ignored() {
        assert!(extract_request_usage(
            "session/update",
            &json!({
                "update": {
                    "sessionUpdate": "turn_completed",
                    "stop_reason": "end_turn"
                }
            }),
        )
        .is_none());
    }

    #[test]
    fn codex_usage_update_reads_codeg_meta() {
        let usage = extract_request_usage(
            "session/update",
            &json!({
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": 2500,
                    "size": 128000,
                    "_meta": { "codeg": { "outputTokens": 450 } }
                }
            }),
        )
        .expect("codex usage");
        assert_eq!(
            usage,
            RequestUsage {
                output_tokens: 450,
                duration_ms: None
            }
        );
    }

    #[test]
    fn plain_context_usage_update_is_not_request_usage() {
        assert!(extract_request_usage(
            "session/update",
            &json!({
                "update": {
                    "sessionUpdate": "usage_update",
                    "used": 2500,
                    "size": 128000
                }
            }),
        )
        .is_none());
    }

    #[test]
    fn generic_request_usage_update_is_the_extension_point() {
        let usage = extract_request_usage(
            "session/update",
            &json!({
                "update": {
                    "sessionUpdate": "request_usage",
                    "outputTokens": 77,
                    "durationMs": 1100
                }
            }),
        )
        .expect("generic request_usage");
        assert_eq!(
            usage,
            RequestUsage {
                output_tokens: 77,
                duration_ms: Some(1100)
            }
        );
    }

    #[test]
    fn zero_output_is_not_a_sample() {
        assert!(extract_request_usage(
            "session/update",
            &json!({
                "update": {
                    "sessionUpdate": "turn_completed",
                    "usage": { "outputTokens": 0, "apiDurationMs": 10 }
                }
            }),
        )
        .is_none());
    }
}
