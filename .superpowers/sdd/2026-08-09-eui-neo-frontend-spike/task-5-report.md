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
  rows. Workspace selection projects only Grok/Codex regular conversations in
  activity order.
- Restricted conversation/session creation to Grok and Codex, delegated row
  creation to `create_project_conversation_core`, and delegated history loading
  to `get_folder_conversation_with_live_core` with a 100-user-turn window.
- Added an injected `EuiSessionOps` seam. Production session creation performs
  `verify_agent_installed`, builds launch inputs with
  `AcpRouteRequest::root(Some(conversation_id), None)`, loads the persisted user
  launch context, and calls `spawn_agent` with owner `"eui"` and no delegation
  override. The returned connection is immediately bound to its folder and
  conversation through the canonical `ConversationLinked` state event. A
  recording test proves verify/build/spawn/bind order and arguments.
- Session selection reuses a live connection by conversation ID or resumes via
  the persisted external session ID. Sends build exactly one text block, create
  a UUID client message ID, and call
  `send_prompt_linked_with_message_id` with the selected folder/conversation.
- Routed set-workspace, create-session, select-session, and send operations
  through asynchronous `CoreOps` workers. Successful create/select completion
  JSON includes `conversationId` and `connectionId`; model session/transcript
  projections are applied only at the captured selection epoch.
- Captured an immutable workspace or session selection while each command is
  admitted under the runtime admission lock. Queued create/select/send work
  consumes that owned snapshot and never re-reads a newer mutable selection
  before DB or ACP side effects.
- Centralized EUI session eligibility as `Regular` plus Grok/Codex. Selection
  checks the persisted row before history/live lookup, so unsupported live rows
  and direct non-regular IDs fail before ACP or parser access.
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

For the consolidated review fix, focused RED runs proved all three reported
defects: create-then-select called verify/build/spawn a second time; unsupported
selection reached `find_connection` and the workspace list included a regular
Claude row; and the runtime regression could not compile because accepted
commands carried no immutable context. The latter failure named the missing
`CommandContext`, worker method arguments, and queue field directly.

The dependency-complete `session_contract` target was also attempted before
implementation, but the kernel killed shared-codeg `rustc` before the test
binary linked. That host failure is not counted as behavioral RED evidence.

### GREEN

- Actual Task 5 `abi.rs`, `commands.rs`, `model.rs`, and `runtime.rs` compiled
  with `-D warnings` against the established narrow shared-core boundary; **11/11
  focused unit tests passed**.
- Actual `eui_facade.rs`, including its test module, compiled with `-D warnings`
  against shape-compatible existing-core signatures. Five focused facade tests
  passed for create/send/bind ordering, create-before-send reuse, real
  connection-state binding, list eligibility, and pre-lookup selection rejection
  (**5/5**).
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
- Actual-source ABI/runtime/model tests with `RUSTFLAGS='-D warnings'`: **11/11**.
- Focused facade orchestration, eligibility, and binding tests: **5/5**.
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

- Workspace validation precedes folder persistence; only Grok/Codex regular
  conversations enter the EUI session list or pass direct selection.
- Grok/Codex guards execute before conversation or ACP access. The facade adds
  no direct persistence schema, parser, Axum/Tauri handler call, or filesystem
  write path.
- Create/resume launch uses the selected absolute workspace, persisted external
  ID, root route with no override, user launch context, owner `"eui"`, and no
  parent/operation ownership. Successful spawn binds folder/conversation IDs
  before returning, so reselect before first send reuses the live connection.
- Linked sends carry one text block, a UUID client ID, and the exact selected
  folder/conversation/connection IDs.
- Selection epoch advancement and completion reservation share one model lock.
  Stale results never mutate sessions, connection ID, or transcript, but still
  drain once through the existing completion ledger.
- Every worker receives the workspace/selection captured synchronously at
  admission. A delayed send either dispatches to its admitted IDs and later
  terminalizes stale or fails without dispatch; it cannot borrow a newer
  selection.
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
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added the EUI workspace/session/history/send loop and consolidated review fixes for immutable admission context, eligible-session boundaries, and pre-send live reuse.","commits":[{"subject":"feat(eui): add workspace and session command loop"},{"subject":"fix(eui): bind session context and eligibility for task 5"}],"tests":{"status":"pass","passed":19,"failed":0,"summary":"11 actual-source ABI/runtime/model tests, 5 focused facade orchestration/eligibility/binding tests, and 3 contracts-only CTest cases pass; the real session contract compiles against the focused boundary."},"concerns":["Full Cargo tests remain parent-skipped; dependency-complete shared-codeg verification requires more than the available 3.8 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-5-report.md"}
-->
