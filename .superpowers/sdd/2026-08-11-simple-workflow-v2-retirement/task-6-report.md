# Task 6 Report: Continue archived work in Simple

Status: implementation complete with runtime verification deferred by explicit
task instruction.

## Changes

- Added `continue_archived_workflow_in_simple` as a shared Rust core, Tauri
  command, authenticated Axum route, and TypeScript transport call.
- Resolved archived roots and durably bound children to the archived root,
  preserving distinct ordinary, Simple, legacy-v1, and corrupt-identity errors.
- Loaded the active persisted manifest revision, normalized its Plan locator,
  and bounded-read the Plan inside the source workspace before any successor
  write. Missing, escaped, absolute, oversized, and non-UTF-8 Plans return
  `simple_successor_plan_unavailable` without leaking absolute paths.
- Added a transaction-aware conversation creation helper so the new regular
  root conversation, title enrollment, Simple descriptor, isolated progress
  locator, and immutable source-workflow link commit atomically.
- Used the unique `simple_workflows.source_workflow_id` index as the durable
  race arbiter. Replays reopen the linked successor, concurrent distinct
  tokens converge with `created=true` for one winner, and public soft deletion
  releases the link for explicit recreation.
- Copied only source folder/workspace identity, root agent type, route override,
  and Design/Plan locator hints. No workflow, gate, task, completion, approval,
  evidence, recovery, model, branch, or session semantics are imported.
- Added the stable structured error code and HTTP 422 mapping, snake-case
  result DTO, command registration, route registration, and frontend invoke/
  fetch arguments.

## Changed Files

- `src-tauri/src/commands/simple_workflow.rs`
- `src-tauri/src/commands/mod.rs`
- `src-tauri/src/lib.rs`
- `src-tauri/src/web/handlers/simple_workflow.rs`
- `src-tauri/src/web/handlers/mod.rs`
- `src-tauri/src/web/handlers/error.rs`
- `src-tauri/src/web/router.rs`
- `src-tauri/src/db/service/conversation_service.rs`
- `src-tauri/src/acp/delegation/workflow/simple.rs`
- `src-tauri/src/app_error.rs`
- `src/lib/api.ts`
- `src/lib/tauri.ts`
- `src/lib/types.ts`
- `src/lib/api.test.ts`

## Tests Authored

- Archived root creation, safe inheritance, isolated progress path, bootstrap
  content bounds, semantic v2 row invariance, replay, and immediate navigation.
- Bound-child resolution to the root and immediate retired-navigation linkage.
- Ordinary, Simple, legacy-v1, corrupt-identity, and invalid request-token
  rejection.
- Missing, escaped, absolute, oversized, and non-UTF-8 Plan failures with zero
  conversation/descriptor writes and no absolute-path error disclosure.
- Multi-connection concurrent requests converging through the unique source
  link with one live conversation, one descriptor, and one `created` winner.
- Public deletion followed by successor recreation.
- Tauri command registration, Axum auth/route, success JSON shape, structured
  source/Plan error mapping, and frontend transport arguments.

## Verification

- Ran `git diff --check`; it reported no whitespace errors. Git emitted only
  the repository's existing LF-to-CRLF working-copy warnings.
- Per the explicit Tasks 5-8 coordination instruction, no tests, builds, lint,
  clippy, rustfmt, formatters, or compile commands were run.

## Concerns

- Rust and TypeScript runtime/compile evidence remains deferred to the unified
  Tasks 5-8 validation pass.
- No existing durable global request-token authority was found. The unique
  source-workflow link and `created` flag form the one-shot bootstrap admission
  gate: downstream prompt enqueue must occur only when `created` is true. The
  command returns bootstrap material only after the atomic transaction commits
  and does not itself own an ACP connection or enqueue the prompt.

## Fix Round 1

Status: implementation and self-review complete; runtime verification remains
deferred by the explicit Tasks 5-8 coordination instruction.

### Review Findings Addressed

- Authorization now accepts an archived source only when it is the workflow
  root or a child with a durable `delegation_task_runs` row and matching
  `delegation_workflow_run_bindings` row for that workflow. An ordinary child
  of an archived root returns `simple_successor_source_not_archived` and makes
  no successor writes.
- Successor creation now classifies SQLite busy, locked, busy-snapshot, and
  unique-link races; it rolls back the losing transaction, waits with bounded
  backoff, reloads the unique source link from a fresh connection snapshot,
  and retries until it converges or exhausts the contention budget. The
  concurrency test uses a deterministic barrier after both callers observed an
  empty link.
- Conflict recovery always reopens the durable winner with `created=false`;
  only a transaction whose commit returned success reports `created=true`, so
  a rolled-back candidate's reusable SQLite row ID cannot create two winners.
- Test-only task-local hooks force a failure immediately after the candidate
  conversation insert. The rollback coverage asserts removal of the candidate
  conversation, auto-title enrollment, Simple descriptor/link, and unchanged
  archived workflow rows. These hooks are behind `#[cfg(test)]` and have no
  production behavior.
- Ordinary, registered-Simple, and observed-Simple sources now return stable,
  distinct public error codes and messages. HTTP mapping distinguishes the
  ordinary 400 path from the Simple 409 path.
- Plan, Design, and persisted successor progress locators are normalized and
  bounded before they are used in bootstrap output. Malformed and oversized
  Design locators are omitted rather than injected.
- Replaced the source-text Tauri registration test with a typed
  `tauri::generate_handler!` assertion.
- The public deletion regression now reuses the same valid request token when
  creating the replacement successor, documenting that no extra durable token
  authority exists beyond the unique source-workflow link.
- Successor title behavior remains source title plus ` (Simple)` as required
  by the approved design.

### Files Changed

- `src-tauri/src/commands/simple_workflow.rs`
- `src-tauri/src/app_error.rs`
- `src-tauri/src/web/handlers/error.rs`
- `src-tauri/src/web/handlers/simple_workflow.rs`
- `.superpowers/sdd/2026-08-11-simple-workflow-v2-retirement/task-6-report.md`

### Tests Authored or Strengthened

- Unbound archived-root child rejection with zero writes.
- Registered- and observed-Simple public error identities.
- Barrier-coordinated empty-link concurrency race convergence.
- Post-candidate-insert rollback including auto-title enrollment cleanup.
- Malformed and oversized Design locator bootstrap bounds.
- Same-token recreation after public successor deletion.
- Typed Tauri command handler integration assertion.

### Deferred Verification and Concerns

- Per the user override, RED/GREEN execution and all tests, builds, cargo
  checks, lint, clippy, rustfmt, prettier, formatters, and compilation are
  deferred until Tasks 5-8 edits are complete. The authored tests have not
  been executed in this round.
- `git diff --check` was run and reported no whitespace errors; Git emitted
  only the repository's existing LF-to-CRLF working-copy warnings.
- The contention retry relies on the existing bounded SQLite retry convention.
  Full runtime validation must exercise both in-memory and multi-connection
  disk SQLite configurations during the unified validation pass.

## Fix Round 2

Status: authored the remaining rollback regression coverage; runtime execution
remains deferred by the explicit Tasks 5-8 coordination instruction.

### Change

- The post-candidate-insert rollback test now holds the established
  `title_key::test_hooks::SuiteGuard` and enables the real title API through
  `enable_title_api_for_test` before creating the successor candidate.
- The existing cfg(test) task-local successor control now queries
  `auto_title_jobs` through the candidate transaction immediately after the
  shared conversation service enrolls it and before the injected rollback.
  The test proves that enrollment was visible inside that transaction, then
  verifies the persisted count returns to its baseline after rollback.

### Files Changed

- `src-tauri/src/commands/simple_workflow.rs`
- `.superpowers/sdd/2026-08-11-simple-workflow-v2-retirement/task-6-report.md`

### Deferred Verification and Concerns

- Per the user override, no tests, cargo checks/builds, lint, clippy,
  rustfmt, prettier, or other formatters were run. The updated test has not
  been executed in this round.
- `git diff --check` is the only verification command run. Unified runtime
  validation must execute the rollback regression with the title API enabled.

## Fix Round 3

Status: implementation and regression coverage authored; runtime and compile
verification remain deferred by the binding user override.

### Root Causes and Changes

- The task-local rollback failure was injected after conversation creation but
  before `register_simple_workflow_txn`, so a persisted descriptor count of zero
  could not prove descriptor or source-link rollback. The failure point now runs
  only after successful descriptor registration and before commit. In the same
  candidate transaction, the test control records both enabled auto-title
  enrollment and a descriptor carrying the expected `source_workflow_id`.
- The rollback regression now compares persisted conversation, auto-title job,
  descriptor, source-link, workflow header, manifest revision, task-run, and
  run-binding state with their pre-attempt baselines. It also proves the
  candidate title does not survive.
- Plan and Design locator checks previously bounded only the raw string before
  normalization. A shared helper now rejects either a raw or normalized value
  over `MAX_SIMPLE_SUCCESSOR_LOCATOR_BYTES`. A required Plan maps such a failure
  to `simple_successor_plan_unavailable`; an optional Design is omitted. The
  existing persisted progress-locator validation also uses the same helper.
- The isolated one-command `tauri::generate_handler!` test did not exercise the
  production invoke registry. The existing production command list is now one
  shared macro source. Production `.invoke_handler` generates its handler from
  that source, while the test derives compiled command paths from the same
  source and asserts the exact Simple successor command is present. The full
  command list is not duplicated and no source substring is inspected.
- Registered and observed Simple sources continue to share
  `SimpleSuccessorSourceAlreadySimple`. Source title plus ` (Simple)` and
  same-token recreation behavior are unchanged.

### Files Changed

- `src-tauri/src/commands/simple_workflow.rs`
- `src-tauri/src/lib.rs`
- `.superpowers/sdd/2026-08-11-simple-workflow-v2-retirement/task-6-report.md`

### Tests Authored or Strengthened

- Post-registration rollback coverage now proves the title job and exact
  descriptor/source link were visible inside the candidate transaction before
  the injected failure, then proves every persisted surface returned to its
  baseline.
- Windows regression coverage uses `U+0130` lowercasing expansion to keep the
  raw locator at 4096 bytes while expanding the normalized locator to 6141
  bytes. Required Plan creation rejects without writes or leaked detail;
  optional Design creation succeeds without injecting Design and keeps the
  bootstrap bounded.
- Production Tauri registry coverage asserts the exact successor command path
  through the same compiled registry source consumed by `.invoke_handler`.

### Static Verification

- Compared the original HEAD production handler list with the refactored shared
  list after trimming indentation: both contain 426 entries and `Compare-Object`
  reported zero differences.
- Replaced both old and new registries with the same placeholder and removed the
  new registry test for a canonical comparison; every remaining `lib.rs` line
  was identical to HEAD.
- Searched the owned production files for the old pre-registration failure hook
  and isolated handler-test names; no matches remain.
- `git diff --check` reported no whitespace errors. Git emitted only the
  repository's existing LF-to-CRLF working-copy warnings.

### Deferred Runtime Concern

- Per the user override, no tests, cargo checks/builds, lint, clippy, rustfmt,
  prettier, formatters, or compilation were run. The authored tests and shared
  macro expansion have not been runtime- or compile-validated in this round.
- The Unicode expansion regressions are intentionally `cfg(windows)` because
  `normalize_rel_path` lowercases paths only on Windows. Unified validation must
  run the Task 6 Rust regressions on Windows with the title API enabled and must
  compile the desktop Tauri registry.
