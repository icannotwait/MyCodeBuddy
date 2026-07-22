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

## Codex Re-Review (2026-07-22, after `e6843f96`)

**Scope:** committed Task 6 behavior through `e6843f96`. During this review,
an unrelated uncommitted regression test was added to `broker.rs`; it is not
part of the commit verdict, but its deterministic failure confirms Finding I1
against the committed implementation.

### Verdict

- **Spec:** FAIL
- **Quality:** REQUEST_CHANGES
- **Findings:** 1 critical, 1 important, 0 minor

### Findings

1. **[Critical] The durable parent-end scan fixes the post-commit test window,
   but parent end can still win its snapshot immediately before the continue
   reserve commits.** `continue_delegation` records only a parent-conversation
   association before awaiting `admit_continue_reserving`
   (`src-tauri/src/acp/delegation/broker.rs:5971-5980`). It does not register
   an in-flight setup or durable parent-end marker; by contrast, gen-1
   registers an in-flight record at entry (`broker.rs:3834-3839`). A parent
   end after `note_parent_conversation` and before/during the reservation can
   drain the association, then call `list_non_terminal_for_parent` after the
   pending lock is released (`broker.rs:7506`, `:7630`). If that SELECT sees
   no row, the later reserve is never revisited or marked canceled. The
   continuation can then create its handoff, resume, send the prompt, and
   return a running acknowledgement because no parent-end state remains for
   `continue_abort_if_handoff_closed` to observe. The new committed regression
   only gates *after* the reserve commits, so it proves the narrower
   post-commit/pre-handoff case but cannot cover this ordering. Add a
   deterministic post-note/pre-reserve gate and make parent-end visibility
   synchronize with admission (or persist/consult a parent-end fence) so a
   completed parent end cannot be overtaken by a later reserve.

2. **[Important] A continued prompt admitted before a recovery-budget charge
   refusal is disconnected without being canceled.** The continuation sends
   the prompt, then calls `promote_running`; on any error its failure branch
   calls only `disconnect` before terminal settlement
   (`src-tauri/src/acp/delegation/broker.rs:6340-6371`).
   `ConnectionSpawner::cancel` explicitly represents best-effort cancellation
   of an in-flight prompt (`spawner.rs:164-166`), and the analogous gen-1
   branch calls `cancel` followed by `disconnect` (`broker.rs:4626-4627`).
   This violates Continuation Flow step 12 for a zero-row
   `unexpected_continue` counter update and can leave externally accepted
   work running after the broker reports `budget_exhausted`. The concurrent
   uncommitted regression
   `continue_promote_budget_refusal_cancels_accepted_prompt` fails with an
   empty cancel record while the expected child connection id is present.

### Re-Check

| Prior Task 6 item | Result | Evidence |
| --- | --- | --- |
| Critical parent end after durable reserve before handoff | PARTIAL | The committed post-reserve gate regression passes, and `list_non_terminal_for_parent` is invoked on parent end. Finding C1 shows the unsynchronized pre-reserve/snapshot ordering remains. |
| Reserving and terminal idempotent replay | PASS | `continue_` coverage passes `continue_reserving_idempotent_replay_does_not_claim_reused_session` and `continue_terminal_idempotent_projects_durable_not_running`. |
| Error precedence and continuability table | PASS | `continue_` coverage passes the decision-table, busy/stale precedence, capability, and duplicate-parent-tool cases. |
| Parent-card binding, missing metadata, and pre-cancel | PASS | Both missing-parent-tool-id tests and `continue_pre_cancel_before_registration_aborts_without_spawn` pass. |
| Admission-window terminal/disconnect drain and spawn-window parent cancel | PASS | The three continuation admission/cancel regressions pass. |
| Replacement inputs, ownership redaction, seven checks, and counter rails | PASS | `replacement_` runs 15 tests successfully, including foreign/missing source, role/profile/workspace, latest/terminal, reason, and dual-budget cases. |
| Companion/schema contract | PASS | `tools_list_exposes_continue_and_replacement_inputs` passes in both focused groups. |
| Post-admission `promote_running` budget refusal | FAIL | Finding I1; the deterministic regression fails before a `cancel` call is recorded. |

### Verification

- Direct compiled test binary, exact
  `continue_parent_cancel_between_reserve_commit_and_handoff_never_spawns`:
  **1 passed**.
- Direct compiled test binary, `continue_`: **19 passed, 1 failed**. The only
  failure is the concurrently added uncommitted
  `continue_promote_budget_refusal_cancels_accepted_prompt` regression,
  confirming Finding I1 against the committed source.
- Direct compiled test binary, `replacement_`: **15 passed**.
- The initial Cargo invocation timed out while the shared target directory was
  compiling in other active Cargo processes; the already-built test binary was
  used for the focused results above.
- `git diff --check e6843f96^ e6843f96` reports three trailing-whitespace
  lines in the unrelated `.superpowers/sdd/task-5-report.md`; no source-file
  whitespace issue was found in the reviewed fix.

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":1,"important":1,"minor":0,
 "summary":"The post-commit parent-end regression passes, but parent end can still race the durable scan before reserve; a continued budget-charge refusal also fails to cancel an accepted prompt."}
-->

## Fix pass — T6 Critical + Important + shared T5 races (after e6843f96)

### Status: DONE_WITH_CONCERNS (awaiting Codex re-review)

### Critical fixed

**Parent end after note / before durable reserve commit**
- Shared first-writer-wins inflight fence registered at continue entry (with parent-conversation note, same lock).
- Post-note gate + cancel before/after `admit_continue_reserving`; Created reserve settled if cancel wins pre-handoff.
- Test: `continue_parent_cancel_after_note_before_reserve_never_admits`.

### Important fixed

**promote_running budget refusal cancels accepted prompt**
- `spawner.cancel` then `disconnect` on continue promote failure (gen-1 parity).
- Test: `continue_promote_budget_refusal_cancels_accepted_prompt`.

### Shared Task 5 fixes in same commit

- Durable sweep excludes drained/reserving task ids (events/attention/live unregister preserved).
- Prompt-send lease atomic with final open check; post-send cancel on parent-end win.

### Verification

| Command | Result |
| --- | --- |
| `cargo test --lib --features test-utils continue_` | 22 passed |
| `cargo test --lib --features test-utils parent_cancel` | 19 passed |
| `cargo test --lib --features test-utils admission_window` | 8 passed |
| `cargo test --lib --features test-utils pre_bootstrap` | 2 passed |
| `cargo test --lib --features test-utils replacement_` | 15 passed |
| `cargo test --lib --features test-utils acp::delegation::companion::tests` | 76 passed |
| `cargo clippy --lib --features test-utils -- -D warnings` | passed |

### Concerns

- Do **not** mark progress.md complete until Codex re-review PASS.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done_with_concerns",
 "summary":"Continue pre-reserve inflight fence + budget cancel + shared durable-sweep/prompt-lease fixes.",
 "report_file":".superpowers/sdd/task-6-report.md"}
-->

## Codex Re-Review (2026-07-22, after `bcad1dff`)

**Scope:** Task 6 behavior at committed `bcad1dff`. The review re-checked the
two findings open after `e6843f96`, the earlier Task 6 PASS items, and the
shared parent-end changes needed by continuation.

### Verdict

- **Spec:** FAIL
- **Quality:** REQUEST_CHANGES
- **Findings:** 1 critical, 0 important, 0 minor

### Critical

1. **[C1] Parent end can still escape the non-atomic post-reserve transfer
   from the in-flight fence to the admission handoff.** The new entry fence
   correctly protects the pre-reserve window, and the continuation checks it
   once more after `admit_continue_reserving`. However, a no-cancel result is
   then followed by `drop_inflight` and only afterwards by the separately
   locked `begin_run_admission` registration
   (`broker.rs:6063-6127`). `drain_parent_tree` marks in-flight records under
   `pending.inner` (`broker.rs:7630-7688`), but performs its durable sweep
   only after releasing that lock (`broker.rs:7803-7862`). If parent end wins
   the lock after the second `take_inflight_cancel` observes `None` and before
   `drop_inflight` removes the record, it stamps the record, snapshots no
   handoff, and then its marker is discarded. The later durable scan may
   terminalize the row, but it has no ownership of the subsequently registered
   live handoff: it neither drains that registration nor cancels/disconnects
   the resumed child. The continuation can therefore advance through resume,
   prompt admission, promotion, and a running acknowledgement before the
   out-of-lock DB CAS catches up; a live registration can remain after that
   terminal write.

   The new `continue_parent_cancel_after_note_before_reserve_never_admits`
   regression pauses before the first pre-reserve check, and
   `continue_parent_cancel_between_reserve_commit_and_handoff_never_spawns`
   pauses before the second post-reserve check. Neither can exercise the gap
   after that check and before the fence-to-handoff transfer. Keep the
   in-flight record through an atomic pending-lock transfer into the admission
   handoff (or retain a parent-end tombstone consulted by that transfer), and
   add a deterministic gate in this exact interval. The regression must assert
   no resume/prompt/running acknowledgement and no surviving live registration.

### Re-Check

| Prior Task 6 item | Result | Evidence |
| --- | --- | --- |
| Critical: parent end before durable reserve | PASS | Entry-side `register_inflight` plus the post-note/pre-reserve deterministic test prevent a reserve after the parent end wins. |
| Important: promotion-budget refusal cancels accepted prompt | PASS | Continue now calls `spawner.cancel` before `disconnect` (`broker.rs:6465-6507`); `continue_promote_budget_refusal_cancels_accepted_prompt` passes. |
| Reserving and terminal fingerprint replay | PASS | `continue_reserving_idempotent_replay_does_not_claim_reused_session` and `continue_terminal_idempotent_projects_durable_not_running` pass. |
| Replacement ownership and seven-check matrix | PASS | Focused replacement suite passes, including foreign-source redaction, role/profile/workspace/latest/reason validation, and dual budget charging. |
| Error precedence, pre-cancel, and missing parent card id | PASS | Focused continuation tests cover busy/stale precedence, external-handle cancellation, and fail-closed parent-tool correlation. |
| Admission-window terminal drain and prompt-send lease | PASS | Continuation admission terminal/disconnect and post-send parent-end cancellation regressions pass. |
| Companion/schema and resume-only contract | PASS | Companion contract suite passes; the full delegation suite includes the continue dispatch and no-fallback coverage. |

### Verification

- `cargo test --lib --features test-utils continue_`: 22 passed.
- `cargo test --lib --features test-utils replacement_`: 15 passed.
- `cargo test --lib --features test-utils parent_cancel`: 19 passed.
- `cargo test --lib --features test-utils admission_window`: 8 passed.
- `cargo test --lib --features test-utils acp::delegation::companion::tests`:
  76 passed.
- `cargo test --lib --features test-utils acp::delegation`: 625 passed.
- `git diff --check e6843f96..bcad1dff`: passed.
- Fresh strict Clippy and `codeg-mcp` checks were not completed in this review:
  the shared worktree had active Cargo builds holding the target lock, and the
  queued review Clippy process was stopped after two harness timeouts without
  a diagnostic. This is a verification gap, not a lint pass or a finding.

### Residual Note

The prompt-send lease still has an unavoidable best-effort external-cancel
window after a transport accepts a prompt; the specification explicitly
permits rare external orphans in analogous crash windows. That is not this
finding. C1 is an in-process ownership-transfer race that can leave a live
registration after parent end and requires correction before approval.

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":1,"important":0,"minor":0,
 "summary":"Task 6 still has a critical non-atomic post-reserve fence-to-handoff transfer: parent end can be stamped, discarded, and miss a later live continuation."}
-->

## Fix pass (after bcad1dff C1 fence-to-handoff) — atomic transfer

### Status: DONE_WITH_CONCERNS (awaiting Codex re-review)

### What was fixed

**C1 — non-atomic post-reserve fence-to-handoff transfer**
- Continue path no longer `drop_inflight` before `begin_run_admission`.
- New `begin_run_admission_transfer`: under one `pending.inner` lock,
  re-observe parent-end stamp on the inflight fence, register admission
  handoff, then drop the fence only after handoff is parent-visible.
- Deterministic gate `continue_post_reserve_pre_handoff_gate` between
  post-reserve cancel-check and the transfer.
- Regression: `continue_parent_cancel_after_post_reserve_check_before_handoff_never_spawns`
  asserts no resume/prompt/running ack and no surviving live registration.

### Verification (controller; parent_cancel suite skipped per user)

```text
cargo test --lib --features test-utils continue_
# 23 passed (includes new pre-handoff transfer cancel regression)

cargo clippy --lib --features test-utils -- -D warnings
# clean
```

### Remaining
- Do **not** mark Task 6 complete in progress.md until Codex re-review PASS.
- parent_cancel full filter not re-run this pass (user skip).

## Independent Codex Task 6 re-review (after b70fbef6)

**Spec: PASS**

**Quality: APPROVED**

### Critical

- None.

### Important

- None.

### Minor

1. `git diff --check bcad1dff b70fbef6` reports a new blank line at EOF in
   the committed Task 6 report. This is non-functional; this appended record
   removes the EOF condition in the working tree.

### Verified

- C1 is fixed: the post-reserve no-cancel check retains its inflight fence,
  and `begin_run_admission_transfer` observes a stamped parent end or
  publishes setup/live/coordination state before deregistering that fence,
  under one `pending.inner` lock.
- The deterministic post-reserve/pre-handoff gate holds a committed reserving
  row with no bound child connection while the inflight fence remains present.
  A parent cancel then produces `parent_canceled`, consumes neither the
  continuation spawn nor prompt result, and leaves the durable row canceled.
- All production parent-end paths use the same drain/sweep ownership model.
  The added `settling` exclusion prevents the durable sweep from stealing a
  concurrent terminal producer's CAS or its broker side effects; its
  regression covers that ordering.

### Notes

- No full suite or `parent_cancel` filter was re-run, per instruction. The
  focused test was not independently retried after concurrent shared Cargo
  jobs prevented a clean result capture; the deterministic test and its
  implementation were inspected directly.
- The new regression uses the setup-count as its no-live-handoff proxy. The
  transfer error branch returns before either `reserve` or
  `register_live_run`, so no surviving live registration is reachable there.

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approved","critical":0,"important":0,"minor":1,
 "summary":"C1 fence-to-handoff is atomic in b70fbef6; no Critical or Important regression found."}
-->

## Important fix (after b70fbef6 re-review)

**Finding:** `continue_closed_handoff_report` hard-coded
`ParentTurnEndReason::ParentCanceled` when the handoff was closed but durable
settle had not committed. Parent-end drains release the pending lock before
settlement, so `parent_turn_failed`, `join_abandoned`, `parent_disconnected`,
and earlier child terminals could be misreported as `parent_canceled`.

**Fix:**
- `take_reserving_handoffs_for_parent_end` parks
  `ReservingHandoffDisposition` by `task_id` on
  `PendingInner::closed_handoff_dispositions` before unreserve.
- `continue_closed_handoff_report` preference: durable terminal → parked
  disposition (parent-end via `parent_end_setup_report`, child terminal via
  `report_from_outcome`) → last-resort fail-closed `parent_canceled` only if
  disposition was lost.
- Parked entries clear on durable settle Won/Existing and when the continue
  path consumes them (or projects durable terminal).

**C1:** Unchanged — atomic `begin_run_admission_transfer` not modified.

### Verification

| Command | Result |
| --- | --- |
| `cargo test --lib --features test-utils continue_closed_handoff_ -- --test-threads=1` | 2 passed |
| `cargo test --lib --features test-utils continue_ -- --test-threads=1` | 25 passed |
| `cargo clippy --lib --features test-utils -- -D warnings` | passed |
| `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings` | passed |
| `git diff --check` (staged files) | clean |

### New regressions

- `continue_closed_handoff_preserves_parent_turn_failed_while_settle_races`
- `continue_closed_handoff_prefers_earlier_child_terminal_disposition`

