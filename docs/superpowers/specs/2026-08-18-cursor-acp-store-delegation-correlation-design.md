# Cursor ACP Store-Backed Delegation Correlation Design

## Status

Direction approved in conversation on 2026-08-18. This document is the
implementation specification.

No implementation plan has been approved yet. The implementation plan must
preserve the fail-closed and concurrency invariants in this document.

## Executive Decision

Codeg will add a Cursor-specific, read-only compatibility layer that recovers
missing MCP tool identity and arguments from Cursor's local `store.db`.

The compatibility layer uses Cursor's ACP `tool_call_id` as the exact join key
to Cursor's stored `toolCallId`. When the stored tool is
`delegate_to_agent` or `continue_delegation`, Codeg derives the existing typed
`DelegationMatchKey` and backfills only the broker entry with that same ACP id.

This does not restore FIFO correlation, does not infer a call from arrival
order, and does not mutate an already-consumed or terminal broker entry.
Missing, ambiguous, malformed, or incompatible Cursor data continues through
the existing correlation error path without spawning a child.

The implementation will be native Rust using Codeg's existing `rusqlite`
dependency. Codeg will not install or execute `cursor-acp-enriched` and will
not depend on Cursor's bundled Node runtime or native Node modules.

## Problem

Codeg correlates a parent ACP tool card with the matching `codeg-mcp`
`tools/call` request so the delegated child can be anchored to the parent's
real `tool_call_id`.

The normal path is:

1. ACP emits a `tool_call` containing its stable id and parseable arguments.
2. Lifecycle derives a typed `DelegationMatchKey` from those arguments.
3. The MCP companion forwards the same substantive arguments to the broker.
4. The broker waits for a unique exact-key match and claims that ACP id.

Cursor breaks step 1 for MCP tools. Its ACP stream announces the call as the
literal title `MCP: tool` with empty `raw_input`. Later updates carry status,
content, output, and locations, but do not restore the MCP tool name or input.
The MCP request also omits `_meta.tool_use_id`. Codeg therefore sees:

```text
ACP side: tool_call_id + no identity + no arguments
MCP side: identity + arguments + no tool_call_id
```

The existing broker correctly refuses to join those two sides by FIFO. That
failure is required for concurrent safety, but it leaves Cursor unable to use
`delegate_to_agent` or `continue_delegation` successfully.

## Evidence

`cursor-acp-enriched@0.4.0` demonstrates that Cursor persists the missing data
under its own session directory:

```text
~/.cursor/acp-sessions/<sessionId>/store.db
~/.cursor/chats/<hash>/<sessionId>/store.db   # legacy
```

Its reader opens the database read-only, scans JSON values in the `blobs`
table, and matches:

```text
stored content item.toolCallId == ACP tool_call_id
```

For a stored `type == "tool-call"` item, `toolName` and `args` contain the
identity and input omitted by Cursor ACP.

The affected local Cursor session was inspected read-only. Four
`delegate_to_agent` calls each had a distinct stored `toolCallId` and complete
arguments, including distinct `correlation_id` values. The stored ids use the
same compound id form already observed by Codeg's Cursor regression fixtures,
including the embedded newline between the call and provider fragments.

The package's direct runtime is not suitable for Codeg:

- version 0.4.0 documents macOS and Linux support only;
- it depends on the native `better-sqlite3` Node module; and
- the locally bundled Cursor `better-sqlite3` binary and bundled `node.exe`
  currently use incompatible Node ABI versions.

The storage algorithm is portable. Codeg already links SQLite through
`rusqlite`, so Windows support does not require a Node native module.

## Goals

- Make Cursor `delegate_to_agent` and `continue_delegation` use the real parent
  ACP `tool_call_id`.
- Preserve safe concurrent delegation when ACP events, store writes, and MCP
  requests arrive in different orders.
- Preserve the broker's exact-key and fail-closed behavior.
- Keep the lifecycle dispatcher and its serial broker-tool worker nonblocking.
- Support current and legacy Cursor session store layouts on Codeg's supported
  desktop and server platforms.
- Keep Cursor's database strictly read-only.
- Reuse the existing typed `DelegationMatchKey`; do not introduce a second
  matching model.
- Keep Codex, Grok, and other ACP adapters on their current zero-SQLite paths.
- Bound lookup time and resource use.

## Non-Goals

- General enrichment of every Cursor tool card or tool result.
- Importing Cursor transcripts into Codeg.
- Writing to, migrating, repairing, or checkpointing Cursor's database.
- Making Cursor's internal schema a stable public contract.
- Replacing `_meta.tool_use_id` when an ACP host provides it.
- Allowing FIFO, arrival-order, or currently-unique-candidate delegation
  matching.
- Rebinding persisted child rows after a delegation has already started.
- Making two byte-identical concurrent requests distinguishable when the MCP
  side provides no distinct key. Such requests remain ambiguous and fail
  closed.
- Patching or redistributing Cursor.

## Selected Architecture

### Cursor store reader

A focused ACP module owns Cursor path discovery and blocking SQLite parsing.
Its interface accepts an explicit Cursor data directory for tests and defaults
to `dirs::home_dir()/.cursor` in production.

Path discovery uses this precedence:

1. `<cursor_dir>/acp-sessions/<session_id>/store.db` when it exists;
2. exactly one `<cursor_dir>/chats/*/<session_id>/store.db` legacy match; or
3. not found or ambiguous.

`session_id` is host-controlled. Before joining it into a path, Codeg requires
one normal path component: it must be nonempty, nonabsolute, and contain no
separator, `.` component, or `..` component. The flat path is authoritative.
Multiple legacy matches fail closed instead of selecting an arbitrary hash
directory.

The reader opens SQLite with read-only flags. It does not issue a journal-mode
pragma or any statement that can write. It queries `id, data` from `blobs`,
skips non-UTF-8 and malformed JSON values, and examines only top-level
`content` arrays. A matching item must satisfy all of:

```text
item.toolCallId == requested ACP tool_call_id
item.type == "tool-call"
item.toolName is a string
item.args is present
```

The reader returns only the tool name and arguments needed for correlation.
It does not read or retain `tool-result` output.

The reader examines every exact-id tool-call match before returning. Repeated
records are accepted only when their normalized tool name and argument value
are identical. Different values for the same `toolCallId` are a conflict and
the whole lookup fails; SQLite row order never selects a winner.

### Async enrichment coordinator

SQLite work must not run on Tokio executor threads and must not delay the
lifecycle serial broker-tool worker. A small coordinator schedules each
blocking scan through `spawn_blocking` and performs retry sleeps asynchronously.

The coordinator is gated before scheduling:

- the live connection's agent type is `Cursor`;
- the ACP event title is exactly `MCP: tool`;
- `raw_input` is absent, blank, or `{}`;
- the event is nonterminal; and
- the live session has a validated Cursor external session id.

The original identityless broker entry is registered first. Only after that
await completes may the coordinator schedule enrichment for the same
`(parent_connection_id, tool_call_id)`.

An in-flight set deduplicates repeated ACP announcements of the same id. A
small semaphore bounds concurrent blocking scans. The lookup deadline begins
when the event is scheduled, so queue pressure consumes the retry budget and
fails closed rather than creating unbounded delayed work.

The retry schedule follows the proven package behavior: immediate attempt,
then exponential delays beginning at 50 ms, capped by an approximately one
second total deadline. This fits inside the broker's existing two-second exact
claim budget while allowing Cursor time to flush a newly-created blob.

### Tool and argument validation

Store data is not trusted merely because the id matched. Codeg accepts the
recovered value only when the normalized `toolName` identifies
`delegate_to_agent` or `continue_delegation`.

The stored `args` value is fed through the same delegation argument walker and
validation used for ACP `raw_input`. This preserves the current rules for:

- required, nonblank `task`;
- valid `correlation_id`;
- `agent_type` and optional `working_dir` for a new delegation; and
- nonblank normalized `task_id` for a continuation.

The implementation should extract a shared value-based helper rather than
serialize arguments and create a second parser with subtly different rules.

### Broker backfill by exact ACP id

The broker gains one narrow operation conceptually equivalent to:

```text
backfill_identityless_match_key(
  parent_connection_id,
  tool_call_id,
  recovered_key,
)
```

The operation runs under the existing `ToolCallTracker` mutex and searches
only the named parent's pending entries. Its state transitions are:

```text
pending identityless + None      -> set recovered key
pending identityless + same key  -> idempotent no-op
pending identityless + other key -> freeze first key and mark conflicted
pending non-identityless         -> reject/no-op
consumed, terminal, or missing   -> no-op
```

It never inserts a new pending entry. This is the central race-safety rule: a
late store result cannot resurrect a call after terminal tombstoning, exact
claim, cancellation, connection teardown, or TTL removal.

The first complete key remains frozen, matching the existing repeated-ACP
event conflict policy. Conflicting enrichment makes the entry permanently
unclaimable; it never changes which request would own the card.

The existing exact resolver remains authoritative. Once a backfilled key is
visible, its next polling tick applies the current unique, ambiguous,
conflicted, canceled, and missing outcomes without a new matching path.

## Data Flow

```text
Cursor ACP ToolCall("MCP: tool", empty input, id=A)
  -> lifecycle serial worker registers identityless pending id=A
  -> coordinator schedules read-only lookup(session, A)

Cursor store.db blob(type=tool-call, toolCallId=A, toolName, args)
  -> validate tool name and args
  -> derive existing DelegationMatchKey K
  -> broker backfills only pending id=A with K

codeg-mcp tools/call(args forming K, no _meta.tool_use_id)
  -> existing broker exact resolver waits out its budget
  -> uniquely claims pending id=A
  -> existing delegation startup and metadata paths continue unchanged
```

For two concurrent calls `A/KA` and `B/KB`, lookup completion may occur in any
order. Backfill targets A and B by id, while MCP claims target KA and KB by
exact key. No step selects the first or oldest pending id.

## Concurrency and Race Invariants

### Reversed arrival order

ACP A, ACP B, MCP B, MCP A and every other interleaving produce the same
bindings when KA and KB are distinct. Store lookup order is irrelevant.

### MCP before store flush

The exact resolver is already polling. The coordinator retries the store and
backfills within that budget. A later polling tick observes the key.

### MCP before ACP announcement

The broker exact resolver already tolerates this ordering. When ACP registers
the identityless entry, enrichment starts and a later tick can claim it.

### Terminal event before enrichment completes

Terminal tombstoning and backfill share the tracker mutex. If terminal wins,
backfill finds no pending entry. If backfill wins, terminal removes the keyed
entry before claim. Neither ordering resurrects or misbinds a card.

### Claim racing terminal or parent cancellation

The new path ends at the existing exact resolver, so its pre-claim and
post-claim cancellation checks remain unchanged. Backfill does not spawn a
child or bypass those checks.

### Duplicate or conflicting store content

The first valid key is frozen. Repeating it is idempotent. A different valid
key marks the entry conflicted. Multiple matching tool-call items must not be
resolved by row order.

### Identical concurrent requests

If two pending calls have the same full `DelegationMatchKey`, the current
sticky ambiguous result remains authoritative. Cursor's stored ids prove which
ACP cards exist, but the identityless MCP requests still provide no value that
distinguishes one identical request from the other. Codeg must not guess.

## Error Handling

These conditions produce no backfill:

- missing Cursor home or session directory;
- invalid session id;
- missing, locked beyond the retry budget, or unreadable `store.db`;
- missing `blobs` table or incompatible schema;
- malformed blob JSON;
- no exact `toolCallId` match;
- unsupported tool name;
- invalid or incomplete delegation arguments;
- conflicting stored values; or
- a broker entry that is no longer pending and identityless.

The MCP request then receives the existing stable correlation error. There is
no FIFO fallback and no synthetic parent id.

Diagnostics may include the agent type, parent connection id, opaque tool call
id, failure class, attempt count, and elapsed milliseconds. They must not log
task text, recovered arguments, tool output, correlation values, or blob
contents. Expected not-found retries should stay at trace/debug level; one
terminal lookup failure may be warn-level with rate limiting if existing
logging conventions support it.

## Security and Privacy

- Cursor's database is opened read-only and never copied, modified, migrated,
  checkpointed, or vacuumed.
- Path construction rejects traversal and arbitrary absolute paths.
- Enrichment runs only for a live Cursor session and the exact identityless
  MCP event shape.
- The recovered payload remains in process and is used only to derive the
  broker match key.
- No task or argument payload is added to logs or metrics.
- No recovered data is trusted to authorize a foreign parent. Broker lookup
  remains scoped by `parent_connection_id` and the exact pending ACP id.
- Schema incompatibility reduces availability for Cursor delegation but never
  weakens correlation correctness.

## Compatibility

Codex remains on its current wrapped-argument path. Grok remains on its current
`use_tool` envelope-unwrapping path. Other hosts with parseable titles or
inputs remain unchanged and never open Cursor's database.

The feature works in desktop and server runtimes when Cursor and Codeg run as
the same OS user and the Cursor session store is locally accessible. The
`codeg-mcp` companion does not read Cursor files; enrichment stays in the host
process that already owns lifecycle and broker state.

No Codeg schema migration, frontend wire change, or durable row rebinding is
required.

## Alternatives Considered

### Depend directly on `cursor-acp-enriched`

Rejected. It would add Node execution and a native `better-sqlite3` packaging
surface to a Rust application, does not currently support Windows, and is
vulnerable to Node ABI mismatches. Its algorithm is the reference, not its
runtime dependency graph.

### Result-time task-id rebinding

Rejected as the primary fix. It waits until after a child has already started
and requires atomic updates across live task state, coordination state,
durable task runs, child conversation linkage, and UI metadata. It also makes
rollback behavior harder when a race discovers a conflicting binding.

The approach remains a possible future recovery mechanism for hosts that
provide neither parseable ACP data nor a readable authoritative store, but it
is unnecessary for Cursor.

### Patch or fork Cursor ACP

Architecturally clean but outside Codeg's release control. Cursor should
eventually propagate its existing tool id and MCP arguments directly in ACP;
Codeg can retire this compatibility layer after supported Cursor versions do
so. A downstream Cursor fork would increase distribution and upgrade cost in
the meantime.

## Testing Strategy

### Store reader unit tests

- current flat path resolution;
- legacy hashed path resolution;
- flat path precedence over legacy;
- multiple legacy matches fail closed;
- traversal and absolute session ids are rejected;
- exact compound `toolCallId`, including an embedded newline, is matched;
- unrelated, binary, non-UTF-8, and malformed blobs are skipped;
- tool-call arguments are returned while tool-result output is ignored;
- missing table and incompatible JSON shapes return classified failures; and
- the database remains unchanged after a lookup.

### Broker unit tests

- an identityless pending entry can be backfilled once by exact id;
- same-key duplicate backfill is idempotent;
- conflicting backfill freezes the first key and fails closed;
- a non-identityless entry cannot be changed by Cursor enrichment;
- terminal or consumed entries are not resurrected;
- parent-scoped lookup cannot update another parent's same-named id; and
- claim, terminal tombstone, and backfill races have one safe winner.

### Lifecycle and correlation tests

- the exact Cursor `MCP: tool` plus empty input shape schedules enrichment;
- non-Cursor hosts and nonmatching Cursor calls do not schedule it;
- missing session id fails closed without blocking the worker;
- repeated ACP frames schedule one in-flight lookup;
- two Cursor delegations with distinct keys bind to their own ids when store
  writes and MCP calls complete in reverse order;
- two identical keys return ambiguous without consuming either id;
- a delayed store write inside the retry window succeeds;
- a write after the retry and broker budgets fails without spawning; and
- a terminal event during lookup prevents late backfill.

### Regression verification

- existing wrapped Codex delegation correlation tests remain green;
- existing Grok `use_tool` unwrap tests remain green;
- the identityless FIFO rejection test remains green; and
- desktop, server, and `codeg-mcp` compile checks remain green.

## Observability

Add low-cardinality counters or existing delegation metrics for:

```text
cursor_enrichment_scheduled
cursor_enrichment_resolved
cursor_enrichment_failed{reason}
cursor_enrichment_backfill{result=applied|same|conflict|stale}
cursor_enrichment_duration_ms
```

Metric labels must not contain session ids, connection ids, tool call ids,
paths, tool names, correlation ids, task text, or arguments.

## Rollout and Removal

The compatibility path is enabled automatically only for the exact known
Cursor identityless shape. There is no user-facing switch in V1.

If a future Cursor release begins providing a parseable ACP title or
`raw_input`, the normal lifecycle path wins and no store lookup is scheduled.
This makes the compatibility layer self-bypassing for corrected hosts.

Codeg may remove the store reader after the minimum supported Cursor version
reliably provides a stable tool id bridge or complete MCP arguments through
ACP. Removal must retain the existing exact-match and fail-closed broker
invariants.

## Acceptance Criteria

- Cursor can start and continue delegated tasks without `_meta.tool_use_id`
  when its local store contains the matching call.
- Two or more concurrent Cursor delegations with distinct correlation keys
  bind to their own ACP cards regardless of event, store, or MCP arrival order.
- No path uses FIFO, first-candidate, or synthetic-id correlation.
- Identical or conflicting concurrent keys fail closed without spawning the
  wrong child.
- Terminal, canceled, consumed, expired, and disconnected entries cannot be
  resurrected by a late store lookup.
- Cursor store access is read-only, path-contained, bounded, nonblocking to
  lifecycle dispatch, and free of task payload logging.
- Missing or changed Cursor internals produce the existing correlation error
  rather than a wrong binding.
- Codex, Grok, and other ACP behavior remains unchanged.
- No Codeg database migration or post-start durable rebinding is introduced.
