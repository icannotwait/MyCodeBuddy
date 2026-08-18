# Task 3 Implementer Report

- Status: `DONE`
- Base: `344d2ab99fabbf0c7e62d14bfb852f8272ce0f9c`
- Commit: `7b9417243921266e5d346447ccae0d81fc87e5d6`
- Commit subject: `feat(delegation): expose binding snapshots`

## RED / GREEN Evidence

- Required RED was observed with `running 1 test`: the filtered query catalog
  test failed because `get_delegation_orchestration_bindings` and its broker
  query path were absent (`0 passed; 1 failed`).
- Query GREEN:
  `cargo test --no-default-features --features server,test-utils --lib orchestration_binding_query_ -- --nocapture`
  ran 11 tests: `11 passed; 0 failed; 4632 filtered out`.
- Fixed-budget GREEN:
  `cargo test --no-default-features --features server,test-utils --lib acp::delegation::companion::tests::grok_tools_list_excludes_companion_ask_and_stays_within_fixed_stdio_budget -- --exact --nocapture`
  ran 1 test: `1 passed; 0 failed; 4642 filtered out`, and printed
  `Grok tools/list JSONL bytes: 7677`.
- Companion regression GREEN:
  `cargo test --no-default-features --features server,test-utils --lib acp::delegation::companion::tests -- --nocapture`
  ran 122 tests: `122 passed; 0 failed; 4521 filtered out`.
- Server compile GREEN:
  `cargo check --no-default-features --features server,test-utils --lib --bin codeg-server --bin codeg-mcp`
  exited 0.
- The existing Grok assertion comparison literal remains `7_680`, and its
  failure text still says `7680`.

## Snapshot Contract

- The conflict set is the deduplicated parent-scoped union of rows whose
  `orchestration_namespace` equals the requested namespace and every row with
  a non-null `work_unit_key`. This includes requested-namespace unkeyed rows,
  same-namespace keyed rows, unbound keyed rows, and foreign-namespace keyed
  rows; foreign-namespace unkeyed rows are excluded.
- Materialization is ordered by `(created_at, task_id)`, queries at most 4097
  rows, accepts 4096, and rejects 4097 without a page as
  `orchestration_binding_query_too_large`.
- Pages expose only durable run identity, complete lineage, actual canonical
  Agent/profile identity, durable status, and the reconstructed binding.
  Four null binding columns become `orchestration_binding: null`; any partial
  binding is `orchestration_binding_query_failed`.
- The serialized redaction scan excludes prompt, preview, output, result,
  termination, card, completion, profile-configuration, and related internal
  keys.
- Snapshot IDs are server-minted lowercase UUIDs. Cursors are stored opaque
  base64url values bound to parent, namespace, snapshot, limit, and exact page
  start. First pages echo `request_cursor: null`; later pages echo the supplied
  cursor exactly. Replays are byte-shape stable, non-final pages return a next
  cursor, and final pages return `next_cursor: null` with `complete: true`.
- Snapshot revisions are parent-scoped `u64` counters serialized as decimal
  strings. Revision changes, cross-parent reuse, expiry at 60 seconds, unknown
  snapshots, and process restart return
  `orchestration_binding_snapshot_stale` without a partial page.

## Mutation Fence

One process-local async read/write gate covers first-page revision read,
database materialization, and cache insertion. The write side covers commit and
revision increment for:

- direct and admitted reserving inserts, including continuation/replacement;
- provisional reserving/canceled row deletion;
- reserving-to-running promotion;
- pre-admission terminalization;
- normal completed/failed settlement;
- cancellation/cleanup terminal settlement.

Failed, idempotent, and rollback paths do not advance the parent revision.

## Auth And Retirement

- The broker request contains only token plus namespace, limit, snapshot ID,
  and cursor. No parent or conversation identity is accepted from the client.
- The dedicated read-only auth path performs token lookup, Root enforcement,
  immutable `coordination_v1` enforcement, then current parent-conversation
  resolution. It does not call `workflow_auth_context` and does not inspect or
  mutate `workflow_v2`.
- A valid Root token with `coordination_v1: true` and `workflow_v2: false`
  succeeds. Invalid token, child role, disabled coordination, and missing
  current parent fail as `invalid_token`, `root_only`,
  `coordination_unavailable`, and `no_active_conversation` respectively,
  before any query or partial page.
- Companion catalog/call gating independently requires delegation,
  coordination, and Root role. Token-derived parent identity plus the
  parent-scoped cache and SQL predicate prevent cross-parent enumeration.
- In the production-shaped `workflow_v2: false` matrix, every entry in
  `WORKFLOW_V2_TOOLS` is absent and direct calls are rejected as unavailable
  before broker I/O. In particular, `publish_workflow_manifest`,
  `settle_workflow_gate`, and `recover_workflow` remain retired without writes.

## Stable Query Errors

- `orchestration_binding_query_invalid`
- `orchestration_binding_query_too_large`
- `orchestration_binding_query_failed`
- `orchestration_binding_snapshot_stale`

Successful MCP responses contain the raw page only in `structuredContent` and
do not add a divergent text copy.

## Concerns

- Snapshot entries are process-local, 60-second TTL-bounded, and individually
  capped at 4096 rows, but the brief defines no explicit cap on the number of
  simultaneously live snapshots. Expired entries are purged opportunistically.
- The Grok catalog is within the fixed limit at 7677 bytes, leaving 3 bytes of
  headroom for this exact feature set.
- macOS test linking emitted the existing `__eh_frame section too large`
  warning; all requested test and check commands still completed successfully.

## Fix Round 1

- Status: `DONE`
- Commit: `9983bcac6130861003791b66a3b89ce1cc1ce483`
- Commit subject: `fix(delegation): harden binding snapshots`

### RED / GREEN Evidence

- The focused query RED ran 12 tests: `10 passed; 2 failed; 4632 filtered
  out`. The schema test failed because `dependentRequired` was absent, and the
  promotion regression returned the cached `reserving` continuation page
  instead of `orchestration_binding_snapshot_stale`.
- A separate public-call RED ran exactly 1 test: `0 passed; 1 failed; 4644
  filtered out`. It received JSON-RPC `-32602` instead of a successful MCP tool
  error envelope.
- Final query GREEN ran 13 tests: `13 passed; 0 failed; 4632 filtered out`.
- Final companion GREEN ran 123 tests: `123 passed; 0 failed; 4522 filtered
  out`.
- The exact fixed-budget test ran 1 test: `1 passed; 0 failed; 4644 filtered
  out` and printed `Grok tools/list JSONL bytes: 7670`. Its comparison remains
  `line.len() <= 7_680`, and its user-facing message still says `7680`.
- `cargo check --no-default-features --features server,test-utils --lib --bin
  codeg-server --bin codeg-mcp` exited 0.
- `git diff --cached --check` passed before the focused commit.

### Fixes

- Every malformed public query argument path now returns a synchronous
  successful JSON-RPC response whose MCP result has `isError: true` and
  `structuredContent.error.code = "orchestration_binding_query_invalid"`.
  Coverage includes absent/null/non-object arguments, unknown fields, wrong
  JSON types, invalid namespace/limit, one-sided paging fields, malformed UUID
  and cursor values, and explicit null paging fields.
- The published query schema now uses bidirectional `dependentRequired` for
  `snapshot_id` and `cursor`.
- A successful `PromoteRunningKind::Promoted` advances the revision using the
  transaction result's `parent_conversation_id`. The lossy unlocked pre-read
  no longer gates invalidation, while `AlreadyRunning` remains non-mutating.
- Operational descriptions again identify Simple Plan/progress registration,
  the read-only parent-scoped snapshot query, continuation of a terminal task
  with child reuse, and `reset_plan_lineage`-only recovery reasons.
- To fit the fixed catalog budget without changing accepted values, redundant
  length keywords were removed where anchored schema patterns already enforce
  the same exact bounds, and two semantically empty `properties` maps were
  omitted. No tool input field or budget literal was removed or weakened.

### Concerns

- The auxiliary review's no-active-conversation process-path assertion remains
  a coverage-only Minor outside this fix brief. Production still returns
  `no_active_conversation` before obtaining a run store or materializing a
  page, and the dedicated auth path is covered.
- The Grok catalog now has 10 bytes of headroom under the fixed 7680-byte
  limit.
- Snapshot entries remain process-local and opportunistically expired, with no
  explicit cap on the number of simultaneously live snapshots.
- macOS test linking continues to emit the existing `__eh_frame section too
  large` warning; all covering tests and compile checks completed successfully.
