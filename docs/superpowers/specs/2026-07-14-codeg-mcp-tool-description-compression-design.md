# codeg-mcp Tool Description Compression Design

## Problem

When delegation, ask, and session tools are enabled together, the compact
`tools/list` JSON-RPC response is about 8,935 UTF-8 bytes. Grok 0.2.98 splits
stdio input at 8,192 bytes and attempts to parse the remainder independently,
so the companion fails during tool discovery. Enabling all six codeg-mcp tools
produces about 10,212 bytes.

The MCP stdio specification does not define an 8 KiB message limit. This work
is a compatibility mitigation for Grok's current reader behavior, not a new
protocol constraint.

Most of the response size is explanatory prose in
`src-tauri/src/acp/delegation/tool_schema.json`. Several descriptions repeat
the same rule at the tool and property levels. The descriptions can be made
substantially shorter without changing the tool contract or removing guidance
that affects tool selection and correct use.

## Goals

- Keep the serialized all-six-tool `tools/list` response, including its final
  newline, at or below 7.5 KiB (7,680 UTF-8 bytes).
- Preserve the information an agent needs to choose and call each tool
  correctly.
- Preserve every tool name, input property, type, enum, required field, and
  numeric or collection constraint.
- Add regression coverage for both the byte budget and essential guidance.

## Non-Goals

- Changing Grok, MCP framing, or codeg-mcp's stdio writer.
- Sending different schemas to different agent clients.
- Removing tools, splitting the companion into multiple MCP servers, or
  changing feature gates.
- Changing tool execution, validation, results, or UI behavior.
- Treating 8 KiB as a general MCP protocol limit.

## Approaches Considered

### Manually compress the static descriptions (selected)

Rewrite each description for information density. State a rule once at the
most relevant level, use direct sentences, and remove examples or motivational
prose that repeat an adjacent schema constraint.

This keeps every client on one authoritative schema and has no runtime impact.
It also allows each sentence to be reviewed for retained meaning.

### Truncate descriptions for Grok at runtime

This can target the affected client, but the companion does not receive a
reliable client identity during standard MCP initialization. Client-specific
schemas would also create inconsistent tool behavior and additional test
paths.

### Split or conditionally expose tools

Smaller tool sets avoid the boundary, but hiding enabled tools or adding more
MCP servers changes discovery and configuration behavior. The size problem
does not justify that architectural change.

## Design

### Schema Changes

Only English `description` strings in
`src-tauri/src/acp/delegation/tool_schema.json` change. JSON structure and
ordering remain stable.

Compression follows these rules:

1. Put tool-selection and lifecycle guidance in the tool description.
2. Put value-specific meaning in the corresponding property description.
3. Do not repeat constraints already encoded by JSON Schema unless the value
   has non-obvious behavior, such as `wait_ms = 0` or cancellation reason
   `timeout`.
4. Prefer short declarative sentences over narrative warnings and repeated
   examples.
5. Retain exact API identifiers where they help an agent form a valid call.

### Required Semantic Content

The compressed descriptions retain these behaviors:

- `delegate_to_agent`: asynchronous return, independent parallel use, cold
  child context, self-contained task requirement, explicit profile routing,
  and later collection by task ID.
- `get_delegation_status`: single or batched IDs, stable task/result mapping,
  snapshot versus blocking modes, `wait_ms = 0`, positive wait cap, return on
  any terminal task, terminal states and results, and silent repeated waiting.
- `cancel_delegation`: required reason, normal cancellation reasons, `timeout`
  being non-canceling, preference for waiting, and completed-task behavior.
- `check_user_feedback`: non-blocking behavior, proactive calls at meaningful
  decision checkpoints, and immediate treatment of returned messages as
  steering.
- `ask_user_question`: blocking interaction, use only for genuine discrete
  user decisions, no proceed/default confirmation, built-in Other option,
  recommendation labeling, and grouping related questions into one call.
- `get_session_info`: `codeg://session/<number>` trigger, internal numeric ID,
  metadata and optional recent messages, read-only behavior, and not-found
  behavior.

### Size Budget

A focused unit test enables delegation, feedback, ask, and sessions together,
dispatches `tools/list`, serializes the actual `JsonRpcResponse` compactly,
adds the newline written by the stdio binary, and measures UTF-8 bytes. The
test requires a size of no more than 7,680 bytes.

This is stronger than the failing five-tool Grok case because it covers the
largest supported feature combination. It leaves at least 512 bytes below the
observed 8,192-byte split for the representative numeric request ID used by
the test.

## Testing

Use a red-green cycle in `companion.rs`:

1. Add the all-feature response-size test and run it against the current
   10,212-byte response to confirm the expected failure.
2. Add semantic-preservation assertions over the descriptions for the
   behaviors listed above. These should pass before rewriting and remain
   green afterward.
3. Rewrite descriptions until the size test passes without changing schema
   structure.
4. Run the focused companion tests, then the required codeg-mcp checks from
   `AGENTS.md`:
   `cargo check --no-default-features --bin codeg-mcp` and
   `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`.

Existing feature-gating and schema tests continue to verify tool counts,
agent enums, required fields, cancellation reasons, and call validation.

## Acceptance Criteria

1. The all-six-tool compact JSON-RPC response plus newline is no more than
   7,680 UTF-8 bytes.
2. The delegation, feedback, ask, and session feature combinations still
   expose the same tools.
3. Tool names and all input schema contracts are unchanged.
4. Every behavior in Required Semantic Content remains plainly stated in the
   descriptions and protected by focused assertions.
5. Focused tests, codeg-mcp check, and codeg-mcp clippy pass without warnings.
