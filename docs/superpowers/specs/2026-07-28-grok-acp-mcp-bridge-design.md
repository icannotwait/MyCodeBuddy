# Grok ACP MCP Bridge Design

## Status

Amended after Design Gate cycle 1 on 2026-07-28; pending required cohort
re-review before plan authoring.

This document specifies a Codeg-side migration of the built-in `codeg-mcp`
server for Grok from an agent-spawned stdio process to Grok's real ACP reverse
MCP bridge. It is a design only; no implementation plan has been approved yet.

Baselines inspected while preparing this design and amendment:

- Codeg: `6d83de57`
- Grok checkout `D:\grok-build`: `02d9359`

## Amendment Record

Design Gate cycle 1 resolved these requirements before Plan authoring:

- strict bridge bootstrap failures are typed terminal outcomes, never
  safe-native fallback candidates;
- collision preflight covers both forwarded and Grok-native MCP names;
- peer MCP startup and bridge handshake use separate timing phases;
- every strict bridge waits for its ready latch before `Connected`, including
  delegation-off launches; and
- the implementation boundary and regression suite cover the manager decision
  boundary, native-name helper, real-Grok release smoke, and T1 observability.

## Problem

Codeg currently exposes its delegation, feedback, session-info, ask, and
workflow tools by adding a per-launch `codeg-mcp` stdio entry to the ACP
`mcpServers` list. Grok then has to discover the companion executable, spawn
it, keep its stdio transport alive, and bridge that child process back to
Codeg's broker listener.

That extra process boundary creates a failure mode during Grok connection
bootstrap. In particular, a new or empty workspace/worktree can reach a state
where the built-in MCP server is not usable but the parent ACP setup remains
waiting. The design must remove Grok's dependency on companion executable
visibility without changing the existing Codeg tool contracts.

Current Grok already advertises and implements the official SDK ACP MCP
transport:

- `initialize._meta["x.ai/mcp/sdk"] == true` advertises support.
- A client registers an in-process server through
  `session/new|load|resume._meta["x.ai/mcp/servers"]`.
- Grok sends each MCP request back to the ACP client with
  `x.ai/mcp/sdk_call`.

Codeg is already the ACP client and already owns the companion protocol core,
broker listener, token registry, ready-lease registry, feedback store, and
delegation lifecycle. It can therefore service the reverse calls in process
without changing Grok.

## Goals

- Use Grok's real `x.ai/mcp/sdk_call` bridge for Codeg's built-in
  `codeg-mcp` whenever Grok strictly advertises
  `initialize._meta["x.ai/mcp/sdk"] == true`.
- Remove all `codeg-mcp` executable lookup and child-process requirements
  from that capability path.
- Reuse one companion dispatcher so stdio and ACP expose identical schemas,
  feature gates, role gates, validation, broker calls, feedback semantics, and
  workflow behavior.
- Keep user-configured MCP servers on their existing stdio, HTTP, or SSE
  transports.
- Keep all non-Grok agents and older Grok versions on the existing stdio
  companion path.
- Treat a strict `true` capability as a strong contract: once selected, a
  bridge failure fails the connection and never changes transport.
- Emit `Connected` only after the ACP session exists and the built-in MCP
  server has successfully completed `initialize` and `tools/list`.
- Give every reverse request and every spawned tool future a connection-owned
  lifecycle, including bounded teardown.
- Preserve the practical cancellation behavior of the current Grok stdio path
  and describe the request-level cancellation limitation accurately.

## Non-Goals

- Modifying the Grok checkout or proposing an upstream Grok patch.
- Migrating user-configured MCP servers to ACP reverse transport.
- Migrating Claude Code, Codex, OpenCode, Gemini, or other agents away from
  their current `codeg-mcp` stdio companion.
- Adding `sdk_notify`, server-to-client MCP traffic, sampling, roots,
  elicitation, or any other full-duplex MCP feature.
- Adding Grok request-level cancellation support that Grok does not currently
  transmit.
- Replacing the existing named-pipe/UDS broker protocol or moving its business
  logic into the ACP connection module.
- Renaming the built-in MCP server or its tools.
- Adding a user setting or feature flag that can force stdio after Grok has
  advertised the strong capability.
- Changing database schemas, frontend workflows, delegation ownership, or
  provider timeout policy.

## Evidence and Existing Constraints

The selected behavior follows the implementations currently present on both
sides:

- Codeg builds the stdio entry in
  `src-tauri/src/acp/connection.rs::inject_codeg_mcp`.
- Codeg's protocol implementation is in
  `src-tauri/src/acp/delegation/companion.rs`.
- Grok parses ACP server registrations in
  `xai-grok-shell/src/session/acp_mcp.rs`.
- Grok's reverse request has the shape
  `{ "serverId": string, "message": object }`.
- Grok does not forward id-less MCP notifications over this bridge. Its
  `acp_transport.rs` explicitly drops them.
- Grok skips an ACP MCP registration when an owned or shared MCP client already
  has the same server name.
- Grok tool timeout currently drops the waiting future rather than sending
  `notifications/cancelled`; parent-turn cancellation aborts the outer task.

Consequently, moving this one server in process eliminates the executable and
stdio boundary, but it does not create a new upstream cancellation signal.

## Considered Approaches

### 1. Codeg in-process bridge reusing the companion core - selected

Codeg handles `x.ai/mcp/sdk_call` on the existing ACP connection and dispatches
the nested request through the same Rust functions used by the stdio binary.
Broker traffic still uses the existing authenticated local listener.

This has the smallest behavioral surface: only transport ownership changes.
It removes the failing process boundary while retaining existing business
logic, security checks, and lifecycle backstops.

### 2. Keep stdio and improve executable discovery

This could add more search paths, copy the binary into each worktree, or retry
spawn. It would not remove the process/bootstrap race and would continue to
maintain a transport Grok no longer requires. It also would not address a
companion process that starts but never becomes usable.

### 3. Patch Grok for notifications and request cancellation first

This could eventually provide stronger per-request cancellation, but it
expands the change across repositories and is not required to preserve current
behavior. Codeg cannot depend on an unshipped Grok change to fix the current
connection problem.

## Capability and Transport Contract

Transport selection happens once per ACP connection after the Grok
`initialize` response is received. The capability parser accepts only the
JSON boolean `true`; strings, numbers, null, missing metadata, and `false`
are all treated as unsupported.

| Agent/capability | Built-in Codeg transport | Binary lookup | Failure policy |
|---|---|---:|---|
| Grok, exact boolean `true` | ACP reverse bridge | Never | Fail connection; no fallback |
| Grok, anything else | Existing stdio companion | Existing behavior | Existing behavior |
| Any other agent | Existing stdio companion | Existing behavior | Existing behavior |
| No built-in feature enabled | No built-in server | None | Not applicable |

The transport decision is immutable for the connection. In particular, after
Grok returns `true`, Codeg MUST NOT:

- inject the built-in server into standard `mcpServers`;
- call `locate_codeg_mcp_binary`;
- spawn `codeg-mcp`;
- retry the bridge as stdio;
- enter a safe-native fallback because bridge bootstrap failed.

An ordinary reconnection may create a fresh ACP connection and bridge, but it
does not change the selected transport. Operational rollback is a Codeg version
rollback, not an automatic runtime fallback.

Bridge bootstrap failures must use a typed `AcpError::GrokMcpBridgeBootstrap`
error carrying a stable phase/reason. `finish_route_ready` and every earlier
bridge-bootstrap failure site MUST send
`RouteBootstrapOutcome::Fatal(AcpError::GrokMcpBridgeBootstrap { .. })`.
`RouteBootstrapOutcome::RouteSpecific`,
`RouteDegradedReason::CompanionInitializationFailed`, and every other existing
safe-native fallback reason are forbidden on the strict bridge path. The
bootstrap-outcome classifier must preserve this error as `Fatal`, so the
manager's Root attempt-one loop returns the connection error without invoking
`safe_native_fallback` or beginning attempt two. This is a control-flow
requirement, not merely a logging convention.

## Architecture

### Connection-level bridge

Add a focused module:

```text
src-tauri/src/acp/grok_mcp_bridge.rs
```

The module owns ACP reverse-MCP framing, state, admission, tracked tasks, and
readiness. It depends on the companion protocol core, but the companion core
does not depend on Grok or ACP.

```text
Grok MCP client
  -> ACP reverse request: x.ai/mcp/sdk_call
  -> GrokAcpMcpBridgeSlot
  -> GrokAcpMcpRuntime
  -> companion::dispatch_request
  -> existing UDS / named-pipe client
  -> existing broker, feedback, session-info, ask, and workflow services
```

The ACP client request handlers are constructed before Codeg receives the
agent's `initialize` response. The connection therefore creates an
`Arc<GrokAcpMcpBridgeSlot>` before building the ACP client. The slot initially
contains no runtime and rejects unexpected calls. After strict capability
negotiation and companion launch preparation, the connection installs exactly
one runtime. A slot cannot replace a live or closed runtime.

### Runtime contents

One `GrokAcpMcpRuntime` is owned by one parent ACP connection and contains:

- an unpredictable, connection-unique `server_id`;
- the existing `CompanionContext`;
- the existing `Arc<InflightCalls>`;
- `Bootstrapping | Ready | Closing | Closed` state;
- bootstrap observations for successfully relayed `initialize` and
  `tools/list`;
- a bridge-ready waiter/latch, present exactly when this connection selected
  the strict ACP bridge and never conditioned on delegation exposure;
- the authenticated ready-lease hold when delegation is exposed;
- a per-connection 64-permit admission semaphore;
- a runtime-owned task set for all spawned bridge calls;
- immutable connection identifiers used for safe structured logging.

The state lock protects short state transitions only. Dispatch, broker I/O,
response relay, and spawned futures never run while holding that lock.

### Companion launch preparation

Refactor the current injection setup into two stages:

1. Build a transport-independent launch specification from the immutable route
   plan and runtime feature snapshots. It contains role, feature bits,
   continuation support, feedback availability, working directory, parent
   connection identity, and the existing agent-specific feature policy. In
   particular, the ACP path reuses Grok's current ask-tool suppression rather
   than re-enabling `ask_user` accidentally.
2. Materialize either the ACP runtime or the existing stdio server.

The ACP materialization path registers the token and optional lease, constructs
the same `CompanionContext`, and installs the runtime. It never resolves a
binary.

The compatibility path retains the current ordering: locate the binary,
register the token/lease, build command arguments, and append the stdio
`McpServer`.

The generalized connection binding retains the token for teardown,
`feedback_available`, the optional delegation lease waiter, and a
bridge-ready waiter/guard that is present if and only if the strict bridge
transport was selected. It also records the bridge bootstrap deadline phase so
all error exits can use the typed terminal bridge error rather than a route
degradation reason.

## Shared Companion Protocol Core

The stdio-specific parser becomes a thin adapter over a structured dispatcher:

```text
dispatch_line(&str)
  -> deserialize JsonRpcRequest
  -> dispatch_request(JsonRpcRequest)
  -> DispatchAction
```

`dispatch_request` remains in
`src-tauri/src/acp/delegation/companion.rs` and is the sole implementation of:

- MCP `initialize`;
- `tools/list` and schema filtering;
- feature and role gating;
- `tools/call` validation;
- broker round trips;
- feedback relay/commit preparation;
- cancellation notification handling for transports that can deliver it;
- method-not-found and protocol errors.

The stdio binary continues to call `dispatch_line` and write responses to
stdout. The ACP bridge deserializes its typed nested object and calls
`dispatch_request` directly. No schema, tool switch, or validation branch is
copied into `grok_mcp_bridge.rs`.

The action type continues to distinguish:

- an immediate response;
- silence for a notification;
- a `SpawnedCall` whose future yields `SpawnResult { response,
  after_relay }`.

For ACP, every `SpawnedCall` is inserted into the runtime-owned task set.
It must never become an unowned detached `tokio::spawn`.

## Session Registration

On the ACP path, merge this entry into the existing session request metadata:

```json
{
  "x.ai/mcp/servers": [
    {
      "name": "codeg-mcp",
      "serverId": "<connection UUID>"
    }
  ]
}
```

The merge applies identically to `session/new`, `session/load`, and
`session/resume`, including the existing resume-to-load-to-new attach ladder.
Other metadata, such as Grok profiles and terminal data, remains intact.

The same connection-level `serverId` is used across attach attempts within
that connection. A reconnect always creates a new ID; an ID from a closed
runtime is permanently invalid.

User MCP servers remain in the ordinary `mcpServers` field. The built-in
`codeg-mcp` entry is absent from that field on the ACP path.

## Reverse Request Contract

Register a typed ACP extension handler for the exact literal
`x.ai/mcp/sdk_call` with:

```json
{
  "serverId": "string",
  "message": {
    "jsonrpc": "2.0",
    "id": "non-null string or number",
    "method": "string",
    "params": {}
  }
}
```

The method name has no leading underscore. It is distinct from Codeg's
unrelated underscore-namespaced ACP extension handlers, so the implementation
must not copy their naming convention.

For each request the handler:

1. verifies the connection is Grok and strict capability negotiation selected
   the bridge;
2. snapshots a runtime that is `Bootstrapping` or `Ready`;
3. compares the exact connection-scoped `serverId`;
4. validates a single JSON-RPC 2.0 object, a non-null scalar ID, and the input
   size;
5. acquires one admission permit without holding the runtime state lock;
6. passes the structured request to `dispatch_request`;
7. relays the nested `JsonRpcResponse` as the ACP extension response;
8. executes `after_relay` only after the original response was accepted by
   the ACP responder.

Batch messages, id-less notifications, invalid versions, invalid IDs,
oversized messages, stale IDs, closing runtimes, and excess concurrency are
rejected before dispatch. Grok currently drops id-less notifications before
they reach Codeg, so rejecting them is consistent with the upstream v1
transport.

Malformed outer parameters and a bad `serverId` return an ACP extension error.
Grok converts that reverse-call failure into a keyed MCP internal error for its
waiting client. Once the envelope is valid, MCP application and tool errors are
returned as nested JSON-RPC responses from the shared dispatcher.

An input over 8 MiB is an ACP invalid-params failure because Codeg does not
dispatch it. An admitted call over the 64-request limit returns a nested
JSON-RPC `-32000` server-busy error with the original ID. If a dispatcher
response exceeds 8 MiB, replace it with a nested `-32603` error carrying the
same ID and skip `after_relay`.

## Bootstrap and Readiness

The bridge starts in `Bootstrapping`. Grok builds ordinary stdio/HTTP/SSE MCP
clients before it builds ACP reverse clients. Bootstrap therefore has two
separate timing phases:

1. `session/new`, `session/load`, or `session/resume` performs Grok's existing
   peer-MCP startup under that request's existing ACP lifetime and error policy.
   Codeg does not start a bridge-ready timer in this phase and does not diagnose
   a peer-start failure as a bridge collision.
2. After the session attach has succeeded, Codeg captures one 30-second
   `ready_lease_timeout()` deadline, retaining its existing test override. It
   waits for the bridge-ready latch and, only when delegation is exposed, the
   ready-lease latch concurrently against that one deadline. A bridge that
   completed its reverse handshake while the attach was in flight satisfies its
   latch immediately.

Readiness uses the first successful bridge handshake in this order:

```text
Grok sends MCP initialize
  -> shared dispatcher builds initialize response
  -> ACP responder accepts initialize response
Grok sends MCP tools/list
  -> shared dispatcher builds the filtered tool list
  -> establish authenticated ready-lease hold when delegation is enabled
  -> ACP responder accepts the original tools/list response
  -> mark bridge Ready
ACP session attach succeeds
  -> bridge-ready gate succeeds for every strict-bridge connection
  -> delegation lease gate additionally succeeds when delegation is exposed
  -> emit Connected
```

The ready lease is registered before the server registration is exposed, so a
fast handshake cannot race an unknown token. Establishing the lease may wake
its waiter before `tools/list` is relayed, but `Connected` remains protected
by the separate bridge-ready latch and the still-pending ACP session request.
`finish_route_ready` (or its replacement) MUST apply the bridge-ready gate
whenever strict bridge transport was selected, even when
`expose_codeg_delegation` is false. The delegation lease remains an additional
gate, not the condition that enables bridge readiness.

No successful `initialize`/`tools/list` pair by the phase-two deadline is a
terminal `AcpError::GrokMcpBridgeBootstrap` failure. The same terminal result
applies to a first-response relay failure, a bridge registration failure, or a
local collision. It never uses `CompanionInitializationFailed`. The timeout
diagnostic can mention a possible hidden collision only after the local
wire-plus-native-name preflight has passed; it must also record whether no
reverse traffic was observed or an incomplete handshake was observed.

After `Ready`, repeat `initialize` or `tools/list` requests may be handled
normally by the shared dispatcher; they do not rotate the runtime or
`serverId`.

## Response Relay and Feedback Delivery

The nested response must pass the output-size check before relay. If the
original response is too large, Codeg sends a bounded error instead and does
not run its `after_relay` action.

`check_user_feedback` retains its relay-then-commit contract:

```text
original feedback response accepted by ACP responder
  -> run after_relay
  -> commit feedback IDs as Delivered

ACP responder failure, cancellation, or response replacement
  -> skip after_relay
  -> feedback remains Pending
```

Responder success means the response reached Codeg's local ACP writer. ACP
does not provide an acknowledgement that Grok still has a waiting future or
consumed the response. This is the same practical acknowledgement boundary as
a successful write to the current companion stdout.

## Lifecycle

### User Stop

User Stop cancels the active Grok turn and its owned task tree through the
existing `session/cancel` and `cancel_by_parent_turn` paths. It does not close
the connection-level bridge runtime. Later turns reuse the same runtime and
`serverId`.

### Connection teardown

Every normal, failed, and early-return connection exit owns the same idempotent
teardown guard. Teardown performs:

1. atomically transition `Bootstrapping` or `Ready` to `Closing`;
2. reject all new reverse requests;
3. invoke `drain_and_cancel_all` for registered in-flight calls;
4. wait for runtime-owned request tasks within a total five-second bridge
   teardown grace, then abort anything still running;
5. drop/stop the ready-lease hold;
6. revoke the token and lease;
7. invoke the existing broker `cancel_by_parent` backstop;
8. transition to `Closed` and permanently invalidate the `serverId`.

The five-second grace bounds both explicit drain work and task joining. If
individual local cancel calls or the drain exceed that outer budget, teardown
continues to the parent-level broker cancellation backstop.

Multiple teardown triggers coalesce on the state transition. No caller waits
while holding the runtime state lock.

## Cancellation Semantics

The shared companion core already supports MCP
`notifications/cancelled`, but the current Grok ACP bridge cannot deliver
that message:

- Grok drops every id-less MCP notification in its ACP transport.
- Grok tool timeout discards the pending reverse-call future.
- Grok turn cancellation aborts the outer task rather than emitting an MCP
  cancellation notification.

Therefore this design intentionally does not synthesize request-level
cancellation. The important T1 case is:

```text
Grok times out or abandons request X
  + parent turn continues
  -> Codeg receives no cancellation signal for X
  -> X may continue until normal completion or another lifecycle backstop
```

Codeg can still stop the work through:

- explicit `cancel_delegation(task_id)`;
- user Stop and `cancel_by_parent_turn`;
- connection teardown and `cancel_by_parent`;
- existing broker/task terminal conditions.

This is behaviorally equivalent to the current Grok stdio route. Observability
must not label an abandoned upstream future as a received cancellation.

## Error Policy

| Failure | Scope | Result | Runtime |
|---|---|---|---|
| Peer stdio/HTTP/SSE MCP startup fails before session attach | Session attach | Existing ACP session error; not a bridge-collision diagnosis | No bridge runtime ready |
| Exact-name collision detected locally | Bridge bootstrap | Terminal `GrokMcpBridgeBootstrap` error with rename guidance | Closed |
| No first handshake after the phase-two deadline | Bridge bootstrap | Terminal `GrokMcpBridgeBootstrap` error; mention possible hidden collision | Closed |
| Ready lease/authentication failure on bridge path | Bridge bootstrap | Terminal `GrokMcpBridgeBootstrap` error | Closed |
| First initialize/tools-list response cannot relay | Bridge bootstrap | Terminal `GrokMcpBridgeBootstrap` error | Closed |
| Invalid outer sdk-call envelope or server ID | Request | ACP extension error | Unchanged |
| Unknown nested MCP method/invalid tool params | Request | Nested JSON-RPC error | Ready |
| Tool business failure or broker error | Request | Existing nested tool result/error | Ready |
| Input/output exceeds limit | Request | Bounded error; no post-relay commit | Unchanged |
| More than 64 concurrent requests | Request | Server-busy error | Ready |
| ACP writer/channel closes | Connection | Teardown | Closed |
| Runtime invariant/task-owner failure | Connection | Teardown and connection error | Closed |

Every strict-bridge bootstrap row sends `RouteBootstrapOutcome::Fatal`; none
permits stdio or native fallback. Request-level errors after `Ready` remain
non-fatal nested MCP/ACP errors as shown above.

## Same-Name Collision Policy

The exact name `codeg-mcp` is reserved for the built-in bridge whenever the
strict Grok capability path is selected.

This is necessary because Grok's MCP state does not build an ACP client if an
owned/shared client already has the same name. Allowing a collision would let a
native client silently shadow the bridge, and Codeg would otherwise never see
the initial reverse handshake.

Codeg applies two defenses:

1. Before sending the session request, inspect the union of the user MCP names
   that will be sent to Grok and
   `agent_native_mcp_server_names(AgentType::Grok)`. The latter is required
   because Codeg deliberately removes native-configured names from the wire
   list. An exact case-sensitive `codeg-mcp` entry from either source fails
   bootstrap with an instruction to rename the user server and identifies the
   source as wire or Grok-native configuration.
2. If a Grok-owned/shared client not visible to Codeg shadows the registration,
   the bridge-ready deadline fails with a contract-violation diagnostic that
   calls out a possible same-name collision.

Codeg does not silently remove or rename the user's server. It also does not
rename the built-in server, because the server name participates in Grok's
qualified tool namespace and existing tool/UI recognition.

## Security and Resource Bounds

- Generate `serverId` with a cryptographically unpredictable UUID for every
  connection.
- Scope lookup to the current ACP connection; never use a global
  `serverId -> runtime` registry.
- Keep the existing broker token authentication, role checks, working-directory
  binding, and ready lease. The `serverId` is routing authority, not a
  substitute for broker authentication.
- Limit compact serialization of each nested request to 8 MiB.
- Limit compact serialization of each nested response to 8 MiB.
- Limit admitted reverse requests to 64 per connection.
- Reject a request before spawning broker work when any boundary check fails.
- Never log full `serverId`, broker token, tool arguments, tool results, or
  feedback content.
- Do not hold the runtime state mutex across serialization, dispatch, broker
  I/O, responder calls, or task joins.

Eight MiB is well above current tool schemas and bounded session-info results,
while preventing a compromised or faulty peer from causing unbounded bridge
processing. The 64-call limit matches the expectation that MCP concurrency is
small while placing a hard bound on task ownership and teardown work.

## Observability

Use existing structured tracing; do not add a new telemetry backend.

Emit fields/events for:

- bridge selection and capability value/type;
- bootstrap start, initialize relayed, tools-list relayed, ready, failure phase,
  and elapsed time;
- request method, request/response byte counts, latency, outcome class, and
  current in-flight count;
- rejection reason: stale ID, bad state, malformed object, size, or concurrency;
- collision detection and bootstrap contract violation;
- teardown cause, drained entries, canceled entries, join timeout, and aborted
  tasks.

Use stable reason identifiers such as:

- `grok_mcp_name_conflict`;
- `grok_mcp_bootstrap_timeout`;
- `grok_mcp_bridge_protocol`;
- `grok_mcp_bridge_unavailable`.

The user-visible connection error includes the phase and an actionable remedy
where one exists. Logs distinguish local response relay from upstream
consumption and never claim that T1 delivered a cancellation. In particular,
the T1 abandonment path must emit no `cancellation_received` log, metric, or
audit field; any later cleanup is attributed to its actual lifecycle backstop.

## Compatibility

- User-defined stdio/HTTP/SSE MCP servers are loaded, filtered, and forwarded
  exactly as today.
- Older Grok builds retain the existing stdio path.
- Other agents retain the existing stdio path.
- The `codeg-mcp` binary remains built and distributed for those compatibility
  paths.
- Tool names, JSON schemas, descriptions, role gates, feature gates, broker
  requests, and feedback rendering remain owned by the shared dispatcher.
- Root versus delegated-child behavior remains derived from the immutable route
  plan.
- No database, persisted configuration, session transcript, or frontend model
  changes are required.
- Grok's v1 half-duplex restrictions remain: no MCP notifications from Codeg,
  no server-initiated MCP requests, and no upstream request-level cancellation.

## Test Strategy

### Unit tests

- Strict capability parsing for boolean true, false, missing, null, strings,
  and numbers.
- Transport selection truth table for Grok, other agents, and no-feature
  launches.
- A strict-capability test whose binary lookup is guaranteed to fail, proving
  the lookup is never reached.
- Metadata merge tests for new, load, and resume without losing existing meta.
- Exact same-name collision detection across both forwarded wire names and
  Grok-native names, plus case-sensitive non-collisions.
- Runtime state transitions, single installation, stale IDs, and idempotent
  teardown.
- Envelope validation, JSON-RPC ID/version validation, 8 MiB boundaries, and
  64-call admission.
- Bootstrap ordering: neither lease readiness nor session success alone can
  emit `Connected`; the bridge-ready latch is required with and without
  `expose_codeg_delegation`.
- Phase-two deadline behavior: slow ordinary peer-MCP startup before session
  attach does not consume bridge-ready budget; a no-handshake timeout after
  attach is terminal and carries its observed bootstrap phase.
- Bootstrap outcome mapping: every strict-bridge failure is `Fatal`, never
  `RouteSpecific(CompanionInitializationFailed)`.
- `dispatch_line` versus `dispatch_request` parity for initialize,
  tools/list, method-not-found, invalid params, feature/role filtering, and
  mocked broker paths.
- Response relay tests proving `after_relay` runs only after the original
  response succeeds.
- Teardown tests proving drain, bounded join, abort, lease release, token
  revocation, and parent cancellation all occur once.
- T1 abandonment tests proving no cancellation-received observability is
  emitted while lifecycle-backstop cleanup remains observable.

### ACP integration tests

Use a mock Grok ACP peer that advertises the capability, consumes
`x.ai/mcp/servers`, and sends reverse requests:

- successful initialize/tools-list/Connected sequence;
- concurrent tool calls with out-of-order responses;
- session/new, load, and resume registration;
- responder failure during feedback relay;
- bootstrap timeout and malformed reverse request;
- visible wire, Grok-native, and hidden same-name collisions;
- a deliberately slow user MCP before ACP reverse-client construction, proving
  peer startup is not reported as a bridge timeout;
- delegation-off feedback/sessions-only launch, proving `Connected` still waits
  for the first relayed `initialize` and `tools/list`;
- ACP connection close with active delegation calls;
- caller abandons one request while the parent connection remains alive.

The manager-level suite must drive the real Root managed-Grok two-attempt loop
for each strict-bridge bootstrap failure and assert one attempt only, no call
to `safe_native_fallback`, and no stdio/native route event. Mock bridge tests
alone are insufficient because the fallback decision belongs to the manager.

### Required Real Grok release smoke

Against the inspected or newer Grok build:

1. Start a Grok session in an empty folder.
2. Start a Grok session in a newly created worktree.
3. Verify tools are present before Codeg reports `Connected`.
4. Run delegation, status, feedback, and explicit cancel flows.
5. Repeat with the local `codeg-mcp` binary unavailable; the bridge must still
   work.
6. Add a user MCP named `codeg-mcp`; connection must fail with the collision
   diagnostic. Repeat with the name in Grok-native configuration so it is not
   forwarded on the wire.
7. Force bridge bootstrap failure; connection must fail without either
   fallback.
8. Include a deliberately slow ordinary user MCP and verify its startup is
   reported through the session-attach path rather than a bridge timeout.

This manual smoke is a release/acceptance gate, not an optional exploratory
test. No Grok source modification is part of it.

### Repository verification

Because the dispatcher refactor affects the shared library and stdio binary,
the implementation must run the repository's Rust checks for every affected
target:

```powershell
Set-Location src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings

cargo check --no-default-features --bin codeg-server
cargo test --no-default-features --bin codeg-server --lib
cargo clippy --no-default-features --bin codeg-server --lib -- -D warnings

cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

No frontend verification is required unless implementation introduces an
unplanned frontend change.

## Rollout

Capability negotiation is the rollout gate; there is no separate feature
flag:

- New Grok with exact `true` uses only the bridge.
- Old Grok without exact `true` uses stdio.
- Other agents use stdio.

This yields compatibility without weakening the strong-capability rule. A
release rollback reverts the Codeg build. It does not silently change transport
inside a live connection.

## Acceptance Criteria

1. Current Grok connects successfully in an empty folder and a new worktree,
   with built-in tools ready before `Connected`.
2. On strict capability `true`, Codeg neither locates nor spawns
   `codeg-mcp`; the flow works when that binary is absent.
3. Every bridge bootstrap failure produces
   `RouteBootstrapOutcome::Fatal(AcpError::GrokMcpBridgeBootstrap { .. })`, a
   clear connection failure, and recorded evidence that neither stdio nor
   safe-native fallback nor a second manager attempt ran.
4. Capability values other than exact boolean `true` retain current stdio
   behavior.
5. `session/new`, `session/load`, and `session/resume` all register the
   current connection's ID, and reconnect invalidates the old ID.
6. The first successful `initialize` and `tools/list` response both precede
   `Connected` for every strict-bridge launch, including when
   `expose_codeg_delegation` is false.
7. User Stop preserves the runtime while canceling the active turn/task tree.
8. Connection teardown leaves no bridge request task, ready hold, token, lease,
   or parent-owned delegation running.
9. An exact custom `codeg-mcp` collision from either the forwarded wire list
   or Grok-native configuration fails visibly rather than shadowing the bridge.
10. Simulated T1 abandonment does not destabilize Codeg or claim a cancellation
    signal; later lifecycle cleanup remains effective.
11. Shared-dispatch parity tests prove that ACP and stdio expose the same tools
    and business behavior.
12. Desktop, server, and `codeg-mcp` Rust targets pass their required test,
    check, and clippy commands.
13. A slow ordinary peer MCP before ACP reverse-client construction neither
    consumes bridge-ready time nor produces a false bridge-collision diagnosis.
14. The required real-Grok release smoke passes.

## Implementation Boundary

Expected Codeg files/modules in scope:

- `src-tauri/src/acp/grok_mcp_bridge.rs` (new);
- `src-tauri/src/acp/mod.rs`;
- `src-tauri/src/acp/connection.rs`;
- `src-tauri/src/acp/manager.rs` for terminal outcome handling and the real
  manager-loop regression test;
- `src-tauri/src/acp/delegation/route.rs` for the explicit no-fallback boundary
  and any typed route/bootstrap test helpers;
- `src-tauri/src/acp/delegation/companion.rs`;
- `src-tauri/src/bin/codeg_mcp.rs`;
- `src-tauri/src/commands/mcp.rs` for the Grok-native name preflight helper;
- focused tests adjacent to those modules.

Changes to `D:\grok-build`, frontend code, database migrations, MCP tool
schemas, or user MCP configuration storage are outside scope.
