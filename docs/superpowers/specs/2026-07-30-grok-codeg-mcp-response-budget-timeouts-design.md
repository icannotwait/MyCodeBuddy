# Grok Codeg MCP Response Budget and Timeout Design

## Status

Approved in conversation on 2026-07-30. This design covers the built-in
`codeg-mcp` server when hosted by Grok. It does not change user-configured MCP
servers or other ACP agents.

## Problem

Grok 0.2.112 splits an MCP JSONL line at 8,192 bytes and does not reassemble
the fragments. `get_session_info` currently bounds compacted message content
by Unicode character count, then duplicates that content into human-readable
`content` and `structuredContent`. It does not measure the final serialized
JSON-RPC line. A sufficiently large result is therefore decoded as a partial
JSON fragment by Grok, while the tool card remains running until Grok's
6,000-second default tool timeout expires.

The same 6,000-second default is inappropriate for most `codeg-mcp` tools.
Fast local queries should fail promptly, while intentionally blocking tools
must retain enough time for a user decision or a delegated task.

## Goals

- Keep every successful `get_session_info` JSON-RPC response at or below 7,680
  bytes, including the trailing newline and the original request id.
- Preserve the newest available session context and expose explicit truncation
  metadata when older content is omitted.
- Keep the existing `content`, `isError`, and `structuredContent` result shape.
- Apply a Grok-only timeout policy to every currently exposed `codeg-mcp` tool.
- Apply the same policy to `session/new`, `session/load`, and `session/resume`.
- Leave user-configured MCP servers and non-Grok agents unchanged.

## Non-Goals

- Changing Grok's 8,192-byte stdio behavior.
- Adding pagination or a second session-info tool.
- Changing the maximum accepted `max_messages` argument.
- Treating a Grok timeout as cancellation of a delegated child task.
- Adding timeout settings to the Codeg UI.

## Considered Approaches

### Final JSONL measurement with progressive omission (selected)

Render the complete JSON-RPC response with its real request id, serialize it
with the trailing newline, and compare the exact byte length with 7,680.
When oversized, remove oldest messages first, then UTF-8-safely shorten the
remaining newest message. This preserves the existing result contract and
directly verifies the host compatibility boundary.

### Lower the existing character constants

This is smaller but remains incorrect. Unicode scalar count is not UTF-8 byte
count, JSON escaping is data-dependent, metadata is not included in the
existing budget, and the two MCP result surfaces duplicate message content.

### Remove message data from one result surface

This would reduce response size, but it changes the current result contract.
Some MCP hosts retain only text content while others use structured content,
so either choice would reduce compatibility.

## Session-Info Response Budget

Add a dedicated `GET_SESSION_INFO_MAX_RESULT_BYTES` constant with the value
`7_680`. Route `get_session_info` through a response renderer that owns the
request id, matching the existing bounded `get_workflow_state` pattern.
Apply the existing 256-byte serialized request-id ceiling to
`get_session_info` before inflight registration as well. An oversized or
unserializable id is rejected as JSON-RPC `-32600` with a null response id, so
the original id cannot consume the entire response budget.

The renderer performs these steps in order:

1. Render the preferred response without omissions.
2. Serialize the full JSON-RPC response with `serialize_jsonrpc_line`, including
   its trailing newline.
3. If oversized, remove the oldest message item and update `included` and
   `truncated`. Repeat until the response fits or only the newest item remains.
4. If the newest item alone is oversized, reduce its text at UTF-8 character
   boundaries. Measure the complete response after each candidate reduction;
   do not estimate JSON escaping overhead.
5. Remove tool names from the remaining item in their existing order when text
   reduction alone cannot fit the response.
6. If untrusted metadata still makes a metadata-only result oversized, return
   a bounded session-info result containing `found`, `session_id`, counts, and a
   stable note that metadata was omitted to satisfy the transport budget.

The human-readable summary continues to show
`Recent messages (included/total, older turns omitted)` whenever content was
removed. The structured message envelope reports the same `included` count and
sets `truncated: true`. No byte slicing may split a UTF-8 code point.

The existing upstream character compaction remains as an early memory/content
bound. It is not treated as the transport compatibility guarantee.

## Grok Timeout Injection

Grok reads per-server MCP configuration from the ACP session request metadata:

```text
_meta.mcpConfig.codeg-mcp
```

For Grok sessions, merge this entry into the existing session metadata without
overwriting terminal metadata, route profiles, or other adapter contributions.
Use `startupTimeoutMs: 30000`, `toolTimeoutMs: 30000`, and the following
`toolTimeoutsMs` map:

| Tool | Timeout |
|---|---:|
| `get_workflow_capabilities` | 5 seconds |
| `check_user_feedback` | 10 seconds |
| `get_session_info` | 15 seconds |
| `get_workflow_state` | 15 seconds |
| `cancel_delegation` | 15 seconds |
| `reply_to_delegation` | 15 seconds |
| `publish_workflow_manifest` | 30 seconds |
| `settle_workflow_gate` | 30 seconds |
| `delegate_to_agent` | 3 minutes |
| `continue_delegation` | 5 minutes |
| `ask_user_question` | 30 minutes |
| `request_parent_decision` | 30 minutes |
| `get_delegation_status` | 90 minutes |

The 30-second server default covers future tools until they receive an explicit
classification. `delegate_to_agent` and `continue_delegation` need longer than
ordinary mutations because they return only after child process startup, ACP
initialization, session creation or restoration, prompt admission, and durable
state setup. Their limits still bound a broken startup chain well below Grok's
current 6,000-second default.

`get_delegation_status` uses one per-tool limit for both immediate snapshots and
Join calls. Immediate snapshots still return as soon as the broker responds;
90 minutes is only the ceiling for a blocking Join.

## Data Flow

### Session info

```text
tools/call(get_session_info)
  -> broker session lookup and character compaction
  -> bounded session-info renderer with original request id
  -> exact JSONL serialization
  -> progressive omission until line <= 7,680 bytes
  -> Grok stdio decoder
```

### Timeouts

```text
Codeg builds session/new|load|resume
  -> session_request_meta for Grok
  -> _meta.mcpConfig.codeg-mcp timeout policy
  -> Grok resolves per-tool override
  -> codeg-mcp call runs with the classified ceiling
```

## Error and Cancellation Semantics

- An oversized preferred session result is not a tool error. It becomes a
  successful, explicitly truncated result.
- Serialization failure returns JSON-RPC internal error `-32603` with a bounded
  stable message.
- A `get_session_info` request id over 256 serialized bytes is rejected before
  inflight registration using the same bounded invalid-request behavior as
  `get_workflow_state`.
- Grok tool timeout does not imply child task cancellation. In particular, a
  timed-out `get_delegation_status` wait must not cancel any delegated task.
- Existing MCP `notifications/cancelled` handling remains unchanged.
- Timeout metadata is emitted only for Grok. Other agents retain their current
  host behavior.

## Testing

### Session-info budget tests

- A 50-message response that previously exceeded 8,192 bytes serializes to no
  more than 7,680 bytes and reports truncation.
- A 200-message response preserves the newest fitting messages.
- A single long Chinese message is truncated on a valid UTF-8 boundary.
- Quotes, backslashes, control characters, and newlines are measured after JSON
  escaping rather than by source length.
- A maximum accepted serialized request id is included in the byte assertion.
- A pathological metadata-only outcome uses the bounded fallback.
- Small responses preserve all messages and the existing structured shape.

### Timeout metadata tests

- Grok `session/new`, `session/load`, and `session/resume` requests contain the
  complete `mcpConfig.codeg-mcp` timeout map.
- The two decision tools resolve to 30 minutes, delegation Join to 90 minutes,
  cold delegation to 3 minutes, and continuation to 5 minutes.
- A future unknown tool inherits 30 seconds.
- Non-Grok requests do not contain the Grok timeout metadata.
- Existing terminal and route metadata remains present after the merge.

## Verification

Run focused companion and connection tests first, followed by the required
`codeg-mcp` checks:

```powershell
cd src-tauri
cargo test --no-default-features --bin codeg-mcp companion
cargo test --features test-utils session_request_meta
cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```
