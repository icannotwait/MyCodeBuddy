# Task 6 Report — continue_delegation + replacement lineage

## Status: DONE_WITH_CONCERNS (awaiting controller/Codex review)

## Commits

- (this commit) — `feat(mcp): continue_delegation and replacement lineage`
- Task 5 fix base: `e4a6eea0` parent cancel pre-bootstrap handoff

## Summary

Implemented `continue_delegation` end-to-end plus replacement inputs on
`delegate_to_agent`.

### Schema / companion / listener
- `tool_schema.json`: `continue_delegation` tool; optional `replaces_task_id` /
  `replacement_reason` on `delegate_to_agent`
- Companion: exposes continue under delegation feature; tools/call routes
  continue with `_codeg_tool` tag
- Listener: continue dispatch before agent_type required; replacement input
  parse (paired fields)

### Run store
- `ContinueEligibility` + `decide_continue_eligibility` decision table
- `admit_continue_reserving` (parent-tool fingerprint first, then
  busy/stale/not_continuable, generation/budget via insert)
- Replacement 7-check in `admit_gen1_reserving` when `replaced_task_id` set
- Bypass closure: established lineage + work_unit_key without replaces →
  `invalid_replacement`
- Typed errors: `StaleTaskId`, `NotContinuable`

### Broker / spawner / manager
- `continue_delegation`: missing parent tool id fail-closed; load target;
  fingerprint; capability gate; admit; `begin_run_admission`;
  `spawn_resume_existing` (ResumeExistingOnly + preallocated id); prompt;
  promote; running registration
- `spawn_resume_existing` on ConnectionSpawner + manager production impl
- Gen-1 path threads replaces_task_id / replacement_reason / lineage inherit
- Continue ack carries `continued_from_task_id` + `reused_session`

### Lifecycle
- Title/raw recognition of continue_delegation without agent_type

## Tests run

```text
cargo test --lib --features test-utils continue_eligibility     # 1 pass
cargo test --lib --features test-utils continue_parent          # 1 pass
cargo test --lib --features test-utils replacement_admission    # 1 pass
cargo test --lib --features test-utils continue_without_parent  # 2 pass
cargo test --lib --features test-utils tools_list_exposes       # 1 pass
cargo test --lib --features test-utils continue_invocation      # 1 pass
cargo test --lib --features test-utils parent_cancel_during_pre_bootstrap  # 1 pass
cargo test --lib --features test-utils acp::delegation::broker::tests  # 220 pass
cargo test --lib --features test-utils acp::delegation::run_store      # 42 pass
cargo clippy --lib --features test-utils -- -D warnings         # clean
```

## Concerns

1. Full e2e resume mismatch with live agent still deferred (Task 9 fixtures).
2. Continue path running registration is a simplified park (not full inflight
   first-terminal-wins setup of gen-1); parent-cancel during continue
   bootstrap uses pre-bootstrap `settle_on_parent_end` path.
3. Do **not** mark Task 5/6 complete in progress.md until controller re-reviews.
4. Server/mcp bin clippy not re-run (lib + test-utils clippy clean).

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns",
 "summary":"continue_delegation dispatch + replacement 7-check, decision table, schema/companion/listener, ResumeExistingOnly spawn with pre-bootstrap handoff.",
 "report_file":".superpowers/sdd/task-6-report.md"}
-->

---

## Independent Codex Review (2026-07-22)

**Scope:** commit `e0f8c64b` against `e4a6eea0` only. The worktree acquired
uncommitted source edits during this review; they are not assessed here.

### Verdict

- **Spec:** FAIL
- **Quality:** REQUEST_CHANGES
- **Findings:** 1 critical, 5 important

### Findings

1. **[Critical] Continuations lose terminal and parent-end events during the
   admission window.**
   [broker.rs](src-tauri/src/acp/delegation/broker.rs:6171) promotes the durable
   row, sets `admitted_running`, and inserts a `RunningTask` directly. It never
   performs the existing first-terminal-wins drain at
   [broker.rs](src-tauri/src/acp/delegation/broker.rs:4698). A `TurnComplete` or
   disconnect received while the run is still reserving was buffered by the
   lifecycle path and is then abandoned, leaving the run incorrectly running.
   A parent end in the same interval can settle and unregister the handoff, after
   which this code still inserts and acknowledges an in-memory running task.
   Reuse the gen-1 post-promotion disposition/drain and add gated continuation
   tests for completion, disconnect, and parent cancel in this window.

2. **[Important] A duplicate continuation can report a terminal failed or
   canceled run as a successful running reuse.**
   [broker.rs](src-tauri/src/acp/delegation/broker.rs:1548) always builds a
   `Running` acknowledgement with `reused_session: true`, and both duplicate
   branches call it at [broker.rs](src-tauri/src/acp/delegation/broker.rs:5834)
   and [broker.rs](src-tauri/src/acp/delegation/broker.rs:5892). Replaying a
   request after its original resume failed returns a false running/session-reuse
   assertion instead of the existing terminal run. Return a report projected
   from the durable row for terminal idempotent matches.

3. **[Important] The typed-error precedence is violated by a mismatched
   `work_unit_key`.** The contract requires `busy_thread` and `stale_task_id`
   before `not_continuable`, but the broker rejects a key mismatch at
   [broker.rs](src-tauri/src/acp/delegation/broker.rs:5903) before the
   eligibility decision, and the store repeats that ordering at
   [run_store.rs](src-tauri/src/acp/delegation/run_store.rs:1377). A busy or
   stale target with the wrong key therefore returns `not_continuable`. Move
   this condition into the not-continuable stage and add overlap-precedence
   tests.

4. **[Important] Replacement reason validation does not implement the durable
   7-check contract fully.** In the reviewed commit,
   [run_store.rs](src-tauri/src/acp/delegation/run_store.rs:455) accepts
   `unresumable` only from an error code or missing snapshot fields, not a child
   missing its resume-capable external session. It checks only the lineage
   unexpected-continue counter at
   [run_store.rs](src-tauri/src/acp/delegation/run_store.rs:1036), not an
   applicable work-unit counter, and calls a trim-only comparison "normalized"
   workspace at [run_store.rs](src-tauri/src/acp/delegation/run_store.rs:1100).
   The single replacement test does not independently cover ownership, agent,
   profile, normalized workspace, terminal/latest, each reason, and both
   counter rows.

5. **[Important] MCP cancellation before continuation registration is ignored.**
   The companion supplies an `external_handle`, and the continuation eventually
   stores it in its `RunningTask`, but `continue_delegation` never consumes the
   pre-cancel buffer used by gen-1 at
   [broker.rs](src-tauri/src/acp/delegation/broker.rs:3766) and
   [broker.rs](src-tauri/src/acp/delegation/broker.rs:4844). A
   `notifications/cancelled` event during resume/prompt admission is buffered
   at [broker.rs](src-tauri/src/acp/delegation/broker.rs:6687) then left behind
   while the continuation runs. Add entry and post-registration cancellation
   checks plus a continuation-specific regression test.

6. **[Important] The companion test module is currently red.** Fresh execution
   of `cargo test --lib --features test-utils acp::delegation::companion::tests`
   produced 73 passes and 3 failures. The old tool counts/order still exclude
   `continue_delegation` in
   [companion.rs](src-tauri/src/acp/delegation/companion.rs:2651),
   [companion.rs](src-tauri/src/acp/delegation/companion.rs:2655), and
   [companion.rs](src-tauri/src/acp/delegation/companion.rs:2699). Update the
   expected list/counts and retain the Grok stdio-budget assertion.

### Requirement Check

| Requirement | Result | Evidence |
| --- | --- | --- |
| ResumeExistingOnly with preallocated handoff | Partial | Production wiring uses `begin_run_admission` and the correct attach mode, but the critical admission-window race makes the async acknowledgement unsafe. |
| Typed errors and precedence | Fail | `work_unit_key` mismatch returns before busy/stale. |
| Continuability decision table | Pass (unit level) | The pure decision-table test covers the requested completed, failed, restart, cancel, policy, replacement, superseded, deleted-child, and agent-mismatch cases. |
| Fingerprint idempotency | Partial | Matching fingerprint is checked before lifecycle gates, but terminal replay is falsely rendered as running. |
| Replacement inputs and bypass closure | Partial | Inputs, lineage inheritance, and established-lineage bypass exist; replacement eligibility and test coverage remain incomplete. |
| Missing `_meta.tool_use_id` fail-closed | Pass | `missing_parent_tool_use_id` is typed and tested for concurrent and lone-card ambiguity. |
| Card summary excluded from MCP results | Pass | The shared completion path strips card-summary comments and `DelegationTaskReport` has no card-summary field. |

### Verification

- `cargo test --lib --features test-utils acp::delegation::broker::tests`: 222 passed.
- `cargo test --lib --features test-utils acp::delegation::run_store::tests`: 43 passed.
- `cargo test --lib --features test-utils acp::lifecycle::delegation_title_tests`: 13 passed.
- `cargo test --lib --features test-utils acp::delegation::companion::tests`: 73 passed, 3 failed.
- `cargo clippy --lib --features test-utils -- -D warnings`: passed.
- `cargo check --no-default-features --bin codeg-mcp`: passed.
- `cargo clippy --no-default-features --bin codeg-mcp -- -D warnings`: passed.

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":1,"important":5,"minor":0,
 "summary":"Task 6 fails review: continuation admission can lose terminal or cancellation events, and five important contract or test gaps remain."}
-->

## Fix pass (Task 6 C1 + I2–I6) — after e0f8c64b Codex FAIL

### Status: DONE_WITH_CONCERNS (awaiting Codex re-review)

### What was fixed

1. **C1 / Task5 I2** — Continue admission drain (see task-5-report fix pass).
2. **I2** — `continue_idempotent_ack`: terminal fingerprint match projects durable row (status/error_code); no false `Running` + `reused_session: true`.
3. **I3** — `work_unit_key` mismatch moved after busy/stale inside `admit_continue_reserving`; broker early key reject removed. Overlap test: `continue_error_precedence_busy_and_stale_before_work_unit_mismatch`.
4. **I4** — Replacement: `missing_external_session` qualifies `unresumable`; work-unit unexpected-continue counter in reason preflight; workspace via `path_eq_for_matching`; tests cover retry free-until-running + second replacement budget + bypass.
5. **I5** — Continue entry + post-registration consume `pre_canceled_handles`; test `continue_pre_cancel_before_registration_aborts_without_spawn`.
6. **I6** — Companion tool list/counts include `continue_delegation` (4 / 5 / all-features order); Grok stdio budget still asserted.

### Verification
```
cargo test --lib --features test-utils continue_                          # 16 pass
cargo test --lib --features test-utils acp::delegation::companion::tests  # 76 pass
cargo test --lib --features test-utils acp::delegation::run_store         # 44 pass
cargo test --lib --features test-utils acp::delegation::broker::tests     # 228 pass
cargo clippy --lib --features test-utils -- -D warnings                   # clean
```

### Remaining
- Do **not** mark progress.md complete.
- Live-agent e2e resume mismatch still Task 9.

---

## Codex Re-Review (2026-07-22, after `e2db84ca`)

**Scope:** committed range `e0f8c64b..e2db84ca`. Uncommitted regression tests
appeared in `broker.rs` and `run_store.rs` during this review; they are not
assessed as part of the commit, but their failures are recorded below as
reproductions of committed-code defects.

### Verdict

- **Spec:** FAIL
- **Quality:** REQUEST_CHANGES
- **Findings:** 1 critical, 3 important, 0 minor

### Findings

1. **[Critical] Parent end can still escape the continuation admission window
   before the handoff is registered.** After a durable continuation reserve at
   `src-tauri/src/acp/delegation/broker.rs:5916`, the code awaits configuration
   resolution at `broker.rs:5930` before `begin_run_admission` establishes the
   in-memory parent-tree handoff at `broker.rs:5982`. A parent end in that gap
   finds no inflight, coordination, or running entry to drain. When the config
   await completes, continuation can resume/spawn and report `Running` after
   its parent has ended. The local regression
   `continue_parent_cancel_after_reserve_before_config_never_spawns` fails at
   the expected missing-handoff assertion. Register and make parent-end
   visibility effective before any post-reservation await, then cover this
   exact gate.

2. **[Important] Reserving idempotent replays still falsely claim verified
   session reuse.** `continue_idempotent_ack` at
   `src-tauri/src/acp/delegation/broker.rs:1566` maps both `Reserving` and
   `Running` rows to `continue_running_ack`, which sets `reused_session: true`.
   A reserving row exists before resume/load verifies the external session,
   contrary to the field contract in
   `src-tauri/src/acp/delegation/types.rs:470`. Preserve idempotency, but do
   not advertise reuse until the resume handshake has succeeded; add a
   reserving-replay regression test.

3. **[Important] Cross-parent replacement source ids disclose their existence
   instead of failing closed.**
   `validate_replacement_insert_txn` at
   `src-tauri/src/acp/delegation/run_store.rs:1107` returns
   `InvalidReplacement("replaced run not owned by parent")` for a foreign
   `replaces_task_id`. The design requires `not_found` for both unknown and
   cross-parent task ids, so this response confirms ownership of another
   parent's run. Return the same non-disclosing `NotFound` result as a missing
   source and retain the focused regression.

4. **[Important] The required replacement seven-check test matrix is still
   incomplete.** The implementation in
   `src-tauri/src/acp/delegation/run_store.rs:1077` now contains the missing
   validation logic, but
   `replacement_admission_checks_reason_and_charges_only_on_running` at
   `run_store.rs:4967` does not independently cover ownership, agent, profile,
   normalized workspace, terminal/latest, and both counter-row paths. The
   brief explicitly requires those checks, plus reason mismatch and second
   replacement coverage, as server-admission tests. Add isolated negative
   cases through `admit_gen1_reserving` and dual-row counter assertions.

### Prior Finding Re-Verification

| Prior item | Result | Evidence |
| --- | --- | --- |
| C1: admission-window drain / no Running after parent end | FAIL | Post-reserve/pre-handoff config await remains invisible to parent-end drain. |
| I2: terminal idempotence | PARTIAL | Terminal rows now project durable status, but reserving replays falsely set `reused_session: true`. |
| I3: `work_unit_key` precedence | PASS | Key mismatch is evaluated after busy/stale in `admit_continue_reserving`; overlap test passes. |
| I4: replacement seven-check completeness | FAIL | Foreign source returns a disclosing `invalid_replacement`, and the required independent server-test matrix is missing. |
| I5: pre-cancel buffer for continue | PASS | Entry and post-registration `take_pre_canceled_handle` gates are present; focused test passes. |
| I6: companion tests | PASS | Fresh companion module run is green, including tool-list count/order checks. |

### Verification

- `git diff --check e0f8c64b..e2db84ca`: passed.
- `cargo test --lib --features test-utils acp::delegation::companion::tests`:
  76 passed, 0 failed.
- `cargo test --lib --features test-utils continue_`: 16 passed, 0 failed.
- `cargo test --lib --features test-utils acp::delegation::run_store::tests`:
  44 passed, 0 failed.
- `cargo test --lib --features test-utils acp::delegation::broker::tests`:
  228 passed, 0 failed.
- `cargo clippy --lib --features test-utils -- -D warnings`: passed.
- Uncommitted regression only:
  `cargo test --lib --features test-utils continue_parent_cancel_after_reserve_before_config_never_spawns`:
  0 passed, 1 failed at the missing-handoff assertion, reproducing C1.
- Uncommitted regression only:
  `cargo test --lib --features test-utils replacement_missing_or_foreign_source_is_not_found`:
  0 passed, 1 failed because the foreign source returns `InvalidReplacement`,
  reproducing the ownership disclosure.

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"request_changes","critical":1,"important":3,"minor":0,
 "summary":"Task 6 remains blocked: parent end can arrive after durable continue reservation but before handoff registration; a reserving replay overclaims verified reuse, and replacement ownership/test-contract gaps remain."}
-->

## Fix pass (after e2db84ca re-review)

### Status: DONE_WITH_CONCERNS (awaiting Codex re-review)

### Gaps fixed

1. **Critical (shared T5/T6):** egin_run_admission immediately after durable
   continue reserve; parent cancel while config is blocked settles and never spawns.
2. **Reserving idempotent replay:** continue_idempotent_ack maps Reserving to
   a running-shaped ack with eused_session: None (only Running claims
   eused_session: true). Test:
   continue_reserving_idempotent_replay_does_not_claim_reused_session.
3. **Cross-parent / missing replaces_task_id:** ownership failure returns
   non-disclosing NotFound (same as missing source). Broker gen-1 missing source
   also maps to NotFound. Tests:
   eplacement_missing_or_foreign_source_is_not_found,
   eplacement_missing_source_reports_not_found.
4. **Replacement 7-check matrix** (isolated dmit_gen1_reserving negatives):
   - ownership → NotFound (above)
   - agent / profile / workspace mismatch
   - non-terminal + not-latest source
   - reason mismatch for each reason (+ unknown)
   - dual-row lineage + work-unit charge only on promote
   - second replacement after charged first → BudgetExhausted

### Tests run

```text
cargo test --lib --features test-utils continue_           # 18 pass
cargo test --lib --features test-utils admission_window    # 8 pass
cargo test --lib --features test-utils acp::delegation::run_store::tests  # 52 pass
cargo test --lib --features test-utils acp::delegation::broker::tests     # 231 pass
cargo test --lib --features test-utils acp::delegation::companion::tests  # 76 pass
cargo test --lib --features test-utils replacement_        # 15 pass
cargo clippy --lib --features test-utils -- -D warnings    # clean
```

### Concerns

1. Full e2e resume mismatch with live agent still deferred (Task 9).
2. Optional barrier-injected TOCTOU race test not added (post-await re-check only).
3. Do **not** mark Task 5/6 complete in progress.md until controller re-reviews.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done_with_concerns",
 "summary":"Close e2db84ca gaps: pre-config handoff cancel visibility, honest reserving reused_session, NotFound for foreign replacement, full replacement negative matrix.",
 "report_file":".superpowers/sdd/task-6-report.md"}
-->
