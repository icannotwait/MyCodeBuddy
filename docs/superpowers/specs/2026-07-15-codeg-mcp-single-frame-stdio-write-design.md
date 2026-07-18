# codeg-mcp Single-Frame Stdio Write Design

## Problem

`codeg-mcp` emits each newline-delimited JSON-RPC response with two explicit
`write_all` calls: one for the serialized JSON and one for the trailing newline.
Raw Windows pipe probes consistently observe the current five-tool response as
two chunks: 5,043 bytes followed by a one-byte newline.

Grok 0.2.98 has already demonstrated incorrect behavior around split stdio MCP
messages. Combining the JSON and delimiter removes one avoidable framing
variable while preserving the newline-delimited protocol.

This change addresses the confirmed double-write behavior. It does not by
itself prove that every MCP initialization timeout has the same cause, and it
cannot require an operating-system pipe reader to return the frame in one read.

## Selected Approach

Construct the complete JSONL frame before locking stdout:

1. Serialize `JsonRpcResponse` directly to UTF-8 bytes.
2. Append one newline byte to that buffer.
3. Acquire the existing stdout mutex.
4. Call `write_all` once with the complete frame.
5. Flush once.

Keep the existing mutex and error mapping. Do not change response contents,
MCP protocol versions, tool schemas, dispatch behavior, or concurrency.

`write_response` will be generic over `AsyncWrite + Unpin`. Production still
uses Tokio stdout, while a recording writer can verify the write boundary in a
focused unit test without spawning a process.

## Alternatives Rejected

### BufWriter

A buffered writer may coalesce calls, but the behavior is indirect and still
leaves two application-level writes. It adds state without improving the
contract.

### Vectored Write

`write_vectored` keeps JSON and newline in separate slices and may legally
perform partial writes. It is less explicit than constructing the final frame.

### Remove the Newline

The companion intentionally uses newline-delimited JSON-RPC. Removing the
delimiter would break framing for compliant clients.

## Error Handling

Serialization failures remain `InvalidData` I/O errors. Write and flush errors
continue to propagate unchanged to the existing stderr error path and process
exit behavior. The response mutex remains held through write and flush so
concurrent tool responses cannot interleave.

## Testing

Follow a red-green cycle in `src-tauri/src/bin/codeg_mcp.rs`:

1. Add a recording `AsyncWrite` test double that accepts the full supplied
   buffer and counts writes and flushes.
2. Assert that `write_response` performs one write containing a trailing
   newline, that the preceding bytes deserialize as the original response, and
   that exactly one flush occurs.
3. Run the test against the current implementation and confirm it fails because
   two writes are recorded.
4. Implement complete-frame construction and confirm the focused test passes.
5. Run the codeg-mcp binary tests, `cargo check`, and clippy required by
   `AGENTS.md`.
6. Repeat the raw Windows pipe probe against the newly built binary. Treat this
   as diagnostic evidence, not as a portable guarantee about read boundaries.

## Acceptance Criteria

1. Each response is passed to one explicit `write_all` call as JSON plus its
   newline delimiter.
2. The response remains valid newline-delimited JSON-RPC.
3. Writes remain serialized and flushed before returning.
4. Existing error behavior and MCP response contents do not change.
5. Focused tests, codeg-mcp checks, and clippy pass without warnings.
