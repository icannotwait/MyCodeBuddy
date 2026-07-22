# Task 6 Report: continue_delegation + replacement inputs

## Status

DONE

## Commits

- Base: `f21091457acc79c1e3af1effb5500985dbfb786d`
- `e0f8c64b256e5cab5eb17053d26729ef6207564c` feat(mcp): continue_delegation and replacement lineage
- `e2db84ca5da410ea2a9d33d93ac03c31e370fc5b` fix(delegation): continue admission drain, cancel gates, and contract gaps
- `2b988325403620eff3d7cf619aaf878ce2fd138f` fix(delegation): close continue cancel gap and replacement contract holes

## Summary

Task 6 implements `continue_delegation` as an asynchronous MCP operation that
reserves a new run on the existing child conversation, registers a
pre-bootstrap parent-cancel handoff immediately, resumes only the recorded
external session, sends the follow-up prompt, and promotes the run to
`running` only after prompt admission. It also adds durable replacement
lineage inputs and server-side validation.

## Files

- `src-tauri/src/acp/delegation/tool_schema.json`
- `src-tauri/src/acp/delegation/types.rs`
- `src-tauri/src/acp/delegation/companion.rs`
- `src-tauri/src/acp/delegation/listener.rs`
- `src-tauri/src/acp/delegation/run_store.rs`
- `src-tauri/src/acp/delegation/broker.rs`
- `src-tauri/src/acp/delegation/spawner.rs`
- `src-tauri/src/acp/delegation/store.rs`
- `src-tauri/src/acp/manager.rs`
- `src-tauri/src/acp/connection.rs`
- `src-tauri/src/acp/lifecycle.rs`
- `.superpowers/sdd/task-6-report.md`

`transport.rs`, `meta_writer.rs`, and `bin/codeg_mcp.rs` use the existing
generic delegation transport, meta persistence, and companion dispatch paths;
the new tool is exposed through the shared schema/companion layer without a
separate binary protocol branch.

## Verification

| Command | Result |
| --- | --- |
| `cargo test --lib --features test-utils continue_parent_cancel_after_reserve_before_config_never_spawns` | 1 passed |
| `cargo test --lib --features test-utils replacement_missing` | 2 passed |
| `cargo test --lib --features test-utils replacement_` | 15 passed |
| `cargo test --lib --features test-utils acp::delegation` | 620 passed |
| `cargo test --lib --features test-utils acp::connection` | 167 passed |
| `cargo check --lib --features test-utils` | passed |
| `cargo clippy --lib --features test-utils -- -D warnings` | passed |
| `cargo check --no-default-features --bin codeg-mcp` | passed |
| `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings` | passed |
| `git diff --check` | passed before source commit |

## Self Review

| Brief interface | Result |
| --- | --- |
| Continue MCP wiring | Done. Schema, companion tools/list and tools/call tagging, listener dispatch, lifecycle recognition without `agent_type`, broker dispatch, resume-capable spawner, manager, and `codeg-mcp` companion path agree on `continue_delegation`. |
| Async acknowledgement and typed precedence | Done. A continuation returns a task acknowledgement after successful prompt admission; the ordering is not_found, fingerprint duplicate handling, not_supported, busy, stale, not_continuable, budget, unresumable, then replacement validation. |
| Continuability decision table | Done. `ContinueEligibility` and `decide_continue_eligibility` cover completed/failed, reserving host restart inheritance, unexpected and unknown-origin cancellation, policy rejection, replacement class, superseded/deleted children, and agent-type mismatch. |
| Duplicate parent tool semantics | Done. Matching durable fingerprints return the same run before busy/stale, including reserving and terminal rows; mismatched or legacy-missing fingerprints reject. Fingerprints never derive from `task_preview`. |
| Replacement inputs | Done. `delegate_to_agent` accepts paired `replaces_task_id` and `replacement_reason`, plus `work_unit_key`; parsing rejects incomplete or unsupported pairs. |
| Work-unit bypass closure | Done. A same-key generation-1 re-dispatch with an established `reached_running_at` lineage and no replacement linkage returns `invalid_replacement`; never-running priors do not establish lineage. |
| Replacement seven checks | Done. The transaction validates ownership with cross-parent redaction, role/profile, normalized workspace, terminal/latest source, durable reason, budget room, and lineage inheritance before reserving. |
| Counter charging and retry behavior | Done. Gen-1 re-dispatch ignores never-running priors, and replacement/unexpected counters increment only in `promote_running` with `reached_running_at`; failed reservations remain uncharged. |
| Parent card correlation and missing metadata | Done. Continue uses the explicit parent tool id even without `agent_type`; missing `_meta.tool_use_id` fails closed with `missing_parent_tool_use_id`, including concurrent-card coverage. |
| Resume-only continuation safety | Done. The path is `admit_continue_reserving` then `begin_run_admission` then `spawn_resume_existing` with a preallocated connection id. It does not use `session/new`, checks cancellation after awaited boundaries, and settles/unregisters pre-spawn failures. |
| Parent result hygiene | Done. Card-summary comments are stripped from parent MCP results and stored only as validated durable/event data. |

## Concerns

- `cargo fmt --check` reports pre-existing formatting drift in unrelated Rust
  files. It was not run as a formatter to avoid unrelated churn; the Task 6
  diff passes `git diff --check`.
- Cargo reports existing non-fatal environment warnings about the missing
  packaged `codeg-mcp` sidecar placeholder and a future-incompatible third
  party procedural macro. The `codeg-mcp` check and strict clippy commands pass.

## Out of Scope

- Task 7 frontend work
- Task 8 skill markdown
- Task 9 live-agent end-to-end fixtures

---

## Codex Re-Review (2026-07-22, after `2b988325`)

**Scope:** functional range `e2db84ca..2b988325`, with the continuation path
back to `e0f8c64b` inspected where required. Commit `7ce5313c` changed this
report only while the review was in progress and is not part of the functional
assessment.

### Verdict

- **Spec:** FAIL
- **Quality:** REQUEST_CHANGES
- **Findings:** 1 critical, 0 important, 0 minor

### Finding

1. **[Critical] Parent cancellation can still be lost after the durable
   continuation reserve and before the in-memory handoff is visible.**
   [`admit_continue_reserving`](src-tauri/src/acp/delegation/run_store.rs:1478)
   commits the reserving insert and then performs a separate awaited
   `load_by_task_id` at [run_store.rs](src-tauri/src/acp/delegation/run_store.rs:1481).
   Only after it returns does the broker call
   [`begin_run_admission`](src-tauri/src/acp/delegation/broker.rs:5951), which
   itself awaits the pending-state mutex before inserting the coordination
   entry at [broker.rs](src-tauri/src/acp/delegation/broker.rs:5755). A parent
   end in either gap finds no `coordination_by_child` entry for the durable
   reserving row; [drain_parent_tree](src-tauri/src/acp/delegation/broker.rs:7389)
   only traverses in-memory coordination entries and does not reconcile
   parent-scoped durable reserves. The later handoff is therefore open, the
   abort checks see a reserving row, and the canceled parent can still resume,
   prompt, and acknowledge the child. The added regression begins cancellation
   only after `child_connection_id.is_some()`
   [broker.rs](src-tauri/src/acp/delegation/broker.rs:19433), which already
   proves the handoff is registered and cannot cover this interval. Make
   parent-end visibility atomic with the durable reserve (or reconcile durable
   reserving rows during the drain), and add a deterministic post-commit,
   pre-handoff regression.

### Re-Verification

| Prior item | Result | Evidence |
| --- | --- | --- |
| Critical: parent cancel after reserve before handoff | FAIL | The handoff remains non-atomic with the durable reserve, as described above. |
| Important: reserving idempotent `reused_session` | PASS | Reserving replay clears the flag at [broker.rs](src-tauri/src/acp/delegation/broker.rs:1580); `continue_reserving_idempotent_replay_does_not_claim_reused_session` passes. |
| Important: foreign `replaces_task_id` returns `not_found` | PASS | Cross-parent source returns `TaskStoreError::NotFound` at [run_store.rs](src-tauri/src/acp/delegation/run_store.rs:1110); focused missing/foreign test passes. |
| Important: replacement seven-check matrix | PASS | Isolated ownership, agent, profile, normalized-workspace, terminal/latest, reason, dual-counter, and second-replacement tests are present and pass. |
| Admission-window drain | PASS | Continue terminal and disconnect drain tests pass; the post-promotion drain remains in place. |
| Terminal idempotence | PASS | Terminal fingerprint replay projects the durable terminal row without a reuse claim. |
| Error precedence | PASS | Busy/stale still win before work-unit mismatch; focused overlap test passes. |
| Pre-cancel handling | PASS | Entry and post-registration external-handle gates remain covered. |
| Companion contract | PASS | Tool-list/count/order companion module is green. |

### Verification

- `cargo test --lib --features test-utils continue_`: 18 passed.
- `cargo test --lib --features test-utils replacement_`: 15 passed.
- `cargo test --lib --features test-utils acp::delegation::broker::tests`:
  231 passed.
- `cargo test --lib --features test-utils acp::delegation::run_store::tests`:
  52 passed.
- `cargo test --lib --features test-utils acp::delegation::companion::tests`:
  76 passed.
- `cargo clippy --lib --features test-utils -- -D warnings`: passed.
- `cargo check --no-default-features --bin codeg-mcp`: passed.
- `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`: passed.
- `git diff --check e2db84ca..2b988325`: the original commit's report had
  one trailing-whitespace line; docs-only `7ce5313c` removed it, and
  `git diff --check e2db84ca..HEAD` is clean.

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":1,"important":0,"minor":0,
 "summary":"Task 6 still fails review: a parent end can be lost after durable continuation reserve and before the in-memory handoff exists."}
-->

## Independent Codex Task 6 review (after 2b988325)

**Spec: FAIL**

**Quality: REQUEST_CHANGES**

### Critical

1. **[C1] A parent end can still escape between the durable continuation
   reserve and the in-memory handoff.** In the reviewed object,
   `admit_continue_reserving` completes `insert_reserving` and then awaits a
   reload (`src-tauri/src/acp/delegation/run_store.rs:1478-1483`). The broker
   does not call `begin_run_admission` until that await returns
   (`src-tauri/src/acp/delegation/broker.rs:5933-5951`). In that interval the
   new `reserving` row has no in-flight setup, coordination identity, live
   registration, or child connection id. `drain_parent_tree`
   (`broker.rs:7365`) only walks those in-memory structures and does not scan
   non-terminal rows by parent, so a parent cancellation settles nothing. The
   continuation then registers, resumes, sends the prompt, and can return a
   running acknowledgement after the parent ended. The claimed regression at
   `broker.rs:19364` waits for `child_connection_id.is_some()` at `:19433`,
   which is already after handoff registration and cannot exercise this gap.

### Important

1. **[I1] A continued prompt is not canceled when its post-admission budget
   charge is refused.** After `send_prompt_linked_for_delegation` has accepted
   the prompt, the continuation calls `promote_running` at
   `broker.rs:6283`. Its failure branch only calls `disconnect` at `:6300`
   before settling `budget_exhausted`; it never calls `ConnectionSpawner::cancel`.
   That trait explicitly distinguishes canceling an in-flight prompt
   (`spawner.rs:164-166`) from tearing down a connection. The analogous gen-1
   branch correctly calls both at `broker.rs:4589-4590`. This violates
   Continuation Flow step 12's required best-effort cancellation after a
   conditional counter update loses, and can leave externally admitted work
   running without a durable running run. Add the cancellation and a focused
   promotion-budget-race assertion for both cancel and disconnect.

### Minor

- None.

### Verified positives

- The earlier admission-window terminal drain is present and covered by the
  exact-head terminal and disconnect tests.
- Terminal fingerprint replay projects the durable terminal state rather than
  returning a false running/reused-session acknowledgement.
- Busy/stale precedence remains ahead of a work-unit-key mismatch, and the
  replacement ownership, role/profile, workspace, latest-terminal, durable
  reason, counter, and bypass checks are represented in the exact-head tests.
- Schema, companion, listener, lifecycle recognition, and the manager's
  `ResumeExistingOnly` attach mode are wired through the shared codeg-mcp path.
- The resume-only connection code contains explicit no-`session/new` refusal
  paths, and final-result handling continues to strip card-summary comments
  before parent-facing results.

### Verification notes

- Reviewed the requested Git objects `f2109145..2b988325`, the Task 6 brief,
  design specification, and the supplied review package. The working tree has
  later uncommitted source changes, so they were deliberately excluded from
  the findings above.
- A focused `continue_admission_window_terminal_drains_after_promote` Cargo
  invocation was attempted but timed out during compilation after 64 seconds;
  it produced no test result and is not counted as a pass. No full suite was
  re-run.

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":1,"important":1,"minor":0,
 "summary":"Task 6 still loses a parent cancellation before handoff registration and misses prompt cancellation after a continued admission charge refusal."}
-->

## Fix pass — Critical C1 durable parent-end settle (post-reserve pre-handoff)

### Status: DONE_WITH_CONCERNS (awaiting Codex re-review)

### Critical fixed (shared Task 5/6)

**C1: Parent cancel between durable continue reserve and handoff registration**

Implemented Option A (recommended):

- `PendingInner::note_parent_conversation` before `admit_continue_reserving`.
- `drain_parent_tree` returns parent conversation ids for visited connections.
- Parent-end settle path calls `RunStore::list_non_terminal_for_parent` and
  settles each reserving/running row as the parent-end code (first-write-wins).
- Continue observes durable `parent_canceled` before spawn/prompt.
- Deterministic regression:
  `continue_parent_cancel_between_reserve_commit_and_handoff_never_spawns`
  (gate between reserve commit and `begin_run_admission`).

### Not fixed in this pass

- **I1 (Important):** post-admission `promote_running` budget refusal on continue
  still disconnects without `ConnectionSpawner::cancel` (gen-1 calls both).
  Out of scope for the Critical-only fix request.

### Verification

| Command | Result |
| --- | --- |
| `cargo test --lib --features test-utils continue_` | 19 passed |
| `cargo test --lib --features test-utils admission_window` | 8 passed |
| `cargo test --lib --features test-utils parent_cancel` | 16 passed |
| `cargo test --lib --features test-utils pre_bootstrap` | 2 passed |
| `cargo clippy --lib --features test-utils -- -D warnings` | passed |

### Concerns

- Do **not** mark progress.md complete until Codex re-review PASS.
- I1 remains for a follow-up if still required by Task 6 contract.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done_with_concerns",
 "summary":"Option A closes Critical C1: parent-end settles durable non-terminal continue reserves before handoff registration.",
 "report_file":".superpowers/sdd/task-6-report.md"}
-->
