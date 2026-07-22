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
