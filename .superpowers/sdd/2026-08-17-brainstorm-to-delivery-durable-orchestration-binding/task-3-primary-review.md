# Task 3 Primary Review

## Verdict

**CHANGES REQUESTED**

Counts: **0 Critical, 2 Important, 1 Minor**.

- Spec compliance: **FAIL**
- Task quality: **CHANGES REQUESTED**

## Finding One-Liners

- **Important:** Malformed public query calls bypass the required
  `orchestration_binding_query_invalid` result, while the published schema also
  accepts an illegal one-sided snapshot/cursor request.
- **Important:** A transient failure in promotion's best-effort pre-read can
  allow a committed `reserving` to `running` transition without advancing the
  parent revision or invalidating an existing snapshot.
- **Minor:** Budget-driven description edits make active tools misleading or
  undocumented, most notably describing `continue_delegation` as a Join.

## Findings

### Critical

None.

### Important

1. **Malformed MCP calls do not expose the required stable invalid-query
   contract.** The published input schema requires only `namespace` and has no
   `dependentRequired`, `oneOf`, or equivalent rule pairing `snapshot_id` with
   `cursor`, so a schema-valid caller can send either field alone
   (`src-tauri/src/acp/delegation/tool_schema.json:163-193`). The companion then
   parses and validates the public arguments locally and returns JSON-RPC
   `-32602` for unknown fields, a one-sided pair, malformed UUID/cursor, or an
   invalid limit (`src-tauri/src/acp/delegation/companion.rs:757-770`). Those
   calls never reach the listener's structured error mapper and therefore do
   not return the mandated
   `error.code = "orchestration_binding_query_invalid"`. This contradicts Task
   3 Step 5 and the Design's stable malformed cursor/snapshot behavior. Encode
   the all-or-none relation in the schema and preserve the stable structured
   query error on every public invalid-input path.

2. **Promotion can commit without invalidating an existing snapshot after a
   transient pre-read failure.** `promote_running_detailed` reads the parent and
   pre-transition status before acquiring the mutation write guard, discards
   every read error with `.ok().flatten()`, and records the mutation only when
   that optional read returned a reserving row
   (`src-tauri/src/acp/delegation/run_store.rs:3913-3947`). If a snapshot already
   exists, this pre-read encounters a transient SQLite error, and the guarded
   promotion subsequently succeeds through its retry policy, the durable row
   becomes `running` but the parent revision is not incremented and the cached
   snapshot is not evicted. A later page can then be accepted with the stale
   `reserving` status. Derive the affected parent and transition result from the
   successful guarded transaction (or use a non-lossy read under the guard),
   and cover the transient-pre-read/successful-promote sequence.

### Minor

1. **Schema compaction changed active tool guidance rather than only shortening
   it.** `continue_delegation` now says `Join task_id`, even though Join is
   performed by `get_delegation_status`, and the new query plus
   `register_simple_workflow` have empty descriptions
   (`src-tauri/src/acp/delegation/tool_schema.json:142-197`). The
   `proposed_user_reason` description also drops the prior
   `reset_plan_lineage`-only restriction in favor of the weaker `Workflow only`
   text (`src-tauri/src/acp/delegation/tool_schema.json:479-482`). These are
   model-facing public instructions; the first can cause a new continuation
   side effect when the caller intended only to wait. Keep descriptions terse
   enough for the frozen budget while retaining their operational meaning.

## Spec Compliance

The reviewed range `344d2ab9..7b941724` otherwise implements the Task 3
contract coherently:

- `BrokerOrchestrationBindingsRequest` carries only the token and query fields;
  no parent, conversation, or connection identity is accepted or serialized.
- The listener uses the dedicated private read-only auth path in the required
  order: token lookup, Root role, immutable `coordination_v1`, and current
  conversation resolution. It neither calls `workflow_auth_context` nor reads
  or changes `workflow_v2`. The focused matrix proves authorization with
  `workflow_v2: false` and the four stable auth failures.
- Companion catalog and call dispatch independently require delegation,
  coordination, and Root role. With production-shaped `workflow_v2: false`,
  all five `WORKFLOW_V2_TOOLS` are absent and direct calls are rejected before
  broker I/O, including the three retired mutation tools.
- The database predicate is parent-scoped and selects the exact union of the
  requested namespace and every non-null `work_unit_key`, ordered by
  `(created_at, task_id)` and limited to 4097. Tests cover requested unkeyed,
  same-namespace keyed, unbound keyed, foreign keyed, foreign unkeyed exclusion,
  ordering, 4096 success, and 4097 rejection.
- Pages have token-derived parent isolation, opaque stored cursors, exact cursor
  echo, immutable namespace/limit, stable replay, parent revisions, 60-second
  expiry, and stale handling for unknown, restarted, cross-parent, and mutated
  snapshots. Binding reconstruction fails closed on partial columns.
- The response DTO maps only approved durable identity, lineage, actual
  Agent/profile, status, and binding fields. Successful MCP rendering uses only
  `structuredContent` with an empty text-content array; no prompt, preview,
  output, result, termination, card, completion, or profile-configuration field
  is serialized.
- The Grok regression keeps the exact test name, `println!`, comparison literal
  `7_680`, and message text `7680`. The fresh measurement is 7,677 bytes.

The first Important finding breaks the public stable-error and schema contract;
the second breaks the concurrency revision fence. Both are blockers for a Task
whose hard triggers include public compatibility and concurrency lifecycle.

## Verification

Fresh reviewer verification, without default features or `tauri-runtime`:

- `cargo test --no-default-features --features server,test-utils --lib orchestration_binding_query_ -- --nocapture`:
  **11 passed, 0 failed**.
- Exact Grok tools/list budget test: **1 passed, 0 failed**, printed
  **7,677 bytes** against the unchanged **7,680-byte** limit.
- `git diff --check 344d2ab9..7b941724`: **passed**.

The focused Rust tests emitted only the already reported macOS `__eh_frame`
compact-unwind warning; execution was unaffected. Per the review instruction,
the full companion suite was not rerun.

## Assessment

The token boundary, dedicated authorization, conflict-set materialization,
retirement gating, fixed Grok budget, and redacted structured output are sound.
Approval is blocked by the public invalid-input result mismatch and the
promotion revision-fence gap. The description regressions should be corrected
in the same focused follow-up without weakening the fixed budget.
