# Task 5 Implementer Report

## Status

DONE_WITH_CONCERNS

Task 5 adds the EUI workspace/session command loop over the existing folder,
conversation, ACP connection, history, and linked-send cores. Selection-changing
requests advance the model epoch at acceptance, and stale worker results remain
exactly-once terminal completions without overwriting the active projection.

## Implementation

- Added narrow `EuiWorkspace`, `EuiSessionSummary`, and
  `EuiSessionSelection` DTOs. They expose canonical workspace identity,
  persisted session fields, connection identity, and backend `MessageTurn`
  history without exposing AppState, database handles, or parsers.
- Canonicalized and verified existing directories before calling
  `open_folder_core`; invalid and non-directory paths cannot create folder
  rows. Workspace selection projects regular persisted conversations in
  activity order.
- Restricted conversation/session creation to Grok and Codex, delegated row
  creation to `create_project_conversation_core`, and delegated history loading
  to `get_folder_conversation_with_live_core` with a 100-user-turn window.
- Added an injected `EuiSessionOps` seam. Production session creation performs
  `verify_agent_installed`, builds launch inputs with
  `AcpRouteRequest::root(Some(conversation_id), None)`, loads the persisted user
  launch context, and calls `spawn_agent` with owner `"eui"` and no delegation
  override. A recording test proves verify/build/spawn order and arguments.
- Session selection reuses a live connection by conversation ID or resumes via
  the persisted external session ID. Sends build exactly one text block, create
  a UUID client message ID, and call
  `send_prompt_linked_with_message_id` with the selected folder/conversation.
- Routed set-workspace, create-session, select-session, and send operations
  through asynchronous `CoreOps` workers. Successful create/select completion
  JSON includes `conversationId` and `connectionId`; model session/transcript
  projections are applied only at the captured selection epoch.
- Advanced `selection_epoch` atomically with accepted workspace/create/select
  completion reservation, cleared the previous active projection immediately,
  and added a gated slow-create contract proving one stale completion and no
  stale model application after a newer selection.
- Recorded `t0_ns` immediately after successful send enqueue. Positive
  conversation IDs are validated inside the standard UI-thread/lifecycle ABI
  admission guard, preserving Task 3 error precedence.
- Added `session_contract.rs` for real ABI workspace selection, canonical path
  JSON, epoch/session projection, invalid workspace terminalization, and
  pre-accept invalid conversation IDs. Updated the public header's async
  session completion/timing documentation.

## TDD Evidence

### RED

The actual `model.rs` was compiled in isolation against its narrow ABI/command
boundary before the epoch implementation. The focused test
`accepted_workspace_and_session_changes_advance_the_selection_epoch` failed as
intended with `left: 0`, `right: 1`.

The dependency-complete `session_contract` target was also attempted before
implementation, but the kernel killed shared-codeg `rustc` before the test
binary linked. That host failure is not counted as behavioral RED evidence.

### GREEN

- Actual Task 5 `abi.rs`, `commands.rs`, `model.rs`, and `runtime.rs` compiled
  with `-D warnings` against the established narrow shared-core boundary; **9/9
  focused unit tests passed**.
- Actual `eui_facade.rs`, including its test module, compiled with `-D warnings`
  against shape-compatible existing-core signatures. The deterministic
  create/send orchestration test passed (**1/1**).
- The complete committed `session_contract.rs` compiled with `-D warnings`
  against the actual ABI/runtime/model modules.
- Contracts-only CMake/CTest passed **3/3** (harness, ABI layout, UI snapshot).

Shape-compatible probes validate the actual changed modules and their boundary
types, but do not replace compiling/running them against the complete shared
`codeg` crate.

## Verification

Passed:

- `cargo fmt --check` for the shared facade and standalone EUI crate files.
- Actual-source facade check with `RUSTFLAGS='-D warnings'`.
- Actual-source ABI/runtime/model tests with `RUSTFLAGS='-D warnings'`: **9/9**.
- Deterministic session orchestration test: **1/1**.
- Actual `session_contract.rs` compile-only check with `-D warnings`.
- Fresh contracts-only CMake build and CTest: **3/3**.
- `git diff --check`.
- Approved design SHA-256 matched
  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
- No standalone `src-tauri/codeg-eui-core/Cargo.lock` remains.

Per parent instruction, **all full Cargo tests were skipped**. No full package,
workspace, or `cargo test --lib --features test-utils` command was run.

A one-job, non-incremental, no-debug dependency-complete standalone-crate
`cargo check` reached the shared `codeg` crate with no emitted Rust diagnostic,
then the kernel OOM-killed `rustc`. Kernel evidence records approximately
3.07 GiB anonymous RSS for that compiler on a 3.8 GiB host with no swap.

## Files Changed

- `src-tauri/src/commands/eui_facade.rs`
- `src-tauri/codeg-eui-core/src/commands.rs`
- `src-tauri/codeg-eui-core/src/model.rs`
- `src-tauri/codeg-eui-core/src/runtime.rs`
- `src-tauri/codeg-eui-core/src/abi.rs`
- `src-tauri/codeg-eui-core/tests/session_contract.rs`
- `codeg-eui/app/bridge/codeg_eui_bridge.h`
- `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md`

`runtime.rs` is a scoped Task 5 dependency even though the brief's enumerated
file list omits it: Task 4 established the asynchronous worker dispatch in that
module, and Task 5 must extend that dispatch to execute the already-exposed
workspace/session/send enqueue operations and apply their epoch-guarded model
updates. No unrelated runtime behavior was changed.

## Self-Review

- Workspace validation precedes folder persistence; only regular conversations
  enter the EUI session list.
- Grok/Codex guards execute before conversation or ACP access. The facade adds
  no direct persistence schema, parser, Axum/Tauri handler call, or filesystem
  write path.
- Create/resume launch uses the selected absolute workspace, persisted external
  ID, root route with no override, user launch context, owner `"eui"`, and no
  parent/operation ownership.
- Linked sends carry one text block, a UUID client ID, and the exact selected
  folder/conversation/connection IDs.
- Selection epoch advancement and completion reservation share one model lock.
  Stale results never mutate sessions, connection ID, or transcript, but still
  drain once through the existing completion ledger.
- The worker context is invalidated synchronously at accepted selection change,
  preventing sends from borrowing an old selection while new selection work is
  in flight.
- ABI input validation stays inside panic containment and the Task 3
  UI-thread/lifecycle checks. Existing frame layout and header constants remain
  unchanged.
- Generated Cargo/CMake outputs and temporary probe crates are excluded from
  the implementation package.

## Concern

Dependency-complete shared-codeg compilation and execution of the real
`session_contract`/facade tests must be rerun on a host with more memory or
usable swap. The focused actual-source probes and C++ contracts are green; the
remaining limitation is host capacity, not a known Task 5 diagnostic.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the EUI workspace/session/history/send command loop with canonical workspace persistence, Grok/Codex ACP orchestration, epoch-safe model projection, and linked send timing admission.","commits":[{"subject":"feat(eui): add workspace and session command loop"}],"tests":{"status":"partial","passed":13,"failed":0,"summary":"9 actual-source ABI/runtime/model tests, 1 deterministic facade orchestration test, and 3 contracts-only CTest cases pass; the real session contract compiles against the focused boundary, while dependency-complete shared-codeg checking is host-OOM-limited."},"concerns":["Dependency-complete session_contract and shared-codeg verification require more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md"}
-->
