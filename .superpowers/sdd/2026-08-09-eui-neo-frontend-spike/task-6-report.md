# Task 6 Implementer Report

## Status

DONE_WITH_CONCERNS

Task 6 adds an epoch-fenced EUI live projector over the authoritative ACP
`SessionState` snapshot and per-connection event stream. It recovers from
sequence gaps, receiver lag, local queue saturation, attempt rollbacks, lost
terminal events, connection removal, and stream closure. Live and snapshot-only
permission, question, and plan requests use one fail-closed deduplicated policy.

## Implementation

- `snapshot_and_subscribe` captures the snapshot, subscription,
  `last_assistant_text`, and parent-turn generation under one `SessionState`
  read lock. No await or event emission occurs under that lock.
- A bounded 128-event pump never blocks producers. Gap, lag, and overflow mark
  `needs_resync`, stop and join the old pump, discard its receiver, reconcile
  retained interaction/user-turn evidence, and atomically resubscribe after an
  authoritative snapshot.
- Queue saturation retains the rejected envelope outside the bounded queue.
  Draining the stopped queue preserves permission/question/plan evidence before
  snapshot replacement, including interactions already cleared by a terminal
  core snapshot.
- Dropped `TurnComplete` recovery uses the state-owned final assistant text and
  turn generation captured with the replacement snapshot. It commits the user
  and assistant turns to structured transcript JSON, sets first/end markers,
  and resumes at the authoritative cursor.
- Live user events append transcript turns once by ID. Turn completion moves
  final assistant text into the transcript and clears the live buffer, so the
  next user turn cannot erase the prior completed answer. Transcript generation
  advances only when transcript bytes change.
- Assistant text is rebased when a new tool-call reference is anchored, matching
  `visible_assistant_text` and snapshot parity. Tools remain reduced to
  `{id,name,status}` summaries.
- A pending user message or `Prompting` snapshot is active even before the first
  token. `StatusChanged(Prompting)` clears stale errors and starts a clean marker
  window.
- Broadcast closure and missing connection state terminalize the selected
  projection with a surfaced error and `t_end_ns`. Selection fencing still
  prevents the old task from overwriting a newer projection.
- Native duration markers use a process-pinned monotonic `Instant` origin.
  Separate RFC 3339 wall timestamps are used only for projected transcript
  entries.
- Snapshot and event interactions share `decline_once`: permission selects a
  reject/deny option or cancels the turn, questions call `cancel_question`, and
  plans call `cancel_plan_approvals_by_parent`. Snapshot reconciliation finishes
  before the receiver pump starts.
- Decline failure cancels the active turn, sets a hard error and end marker,
  aborts the pump, and never resumes a potentially parked responder.
- Runtime selection remains fenced by epoch and connection ID. A new selection
  stops the old live task while accepted old requests still complete exactly
  once as stale.

## TDD Evidence

### RED

- The dependency-complete focused target was attempted during the initial Task 6
  cycle and was SIGKILLed while compiling shared `codeg`; it was not retried on
  this approximately 4 GiB host.
- Actual-source probes first failed on the missing atomic attach, projector, and
  interaction boundaries.
- The post-review RED suite failed on unchanged transcript payloads, retained
  pre-tool text, inactive pending-user snapshots, dropped terminal/permission
  evidence under a full 128-event queue, silent stream closure, and lost
  gap-trigger interaction evidence.
- Failure output was observed before each corresponding production correction.

### GREEN

- Direct actual `live.rs`/`perf.rs` source probe with warnings denied: **9/9**.
- Actual ABI/model/runtime/live/perf unit modules through the focused boundary
  with warnings denied: **16/16**.
- Committed Task 6 integration sources through the focused boundary with
  warnings denied: **23/23** (`live_recovery` 17, `interaction_decline` 6).
- Contracts-only CMake/CTest: **3/3**.
- The committed `session_contract.rs` compiles with warnings denied in the same
  focused actual-source harness.

The shape harness substitutes only the memory-heavy shared `codeg` boundary; it
does not replace dependency-complete compilation of the production crate.

## Verification

Passed:

- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`.
- Focused source, unit, recovery, and interaction probes: **48/48 Rust tests**.
- Compile-only focused `session_contract` verification with `-D warnings`.
- Contracts-only CTest: **3/3**.
- `git diff --check` and `git diff --cached --check`.
- Approved design SHA-256:
  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
- Frozen plan SHA-256:
  `76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f`.
- No standalone `src-tauri/codeg-eui-core/Cargo.lock` remains.

Per parent instruction, every full Cargo suite was skipped. No
`cargo test --lib --features test-utils`, package/workspace Cargo test, or broad
shared-codeg suite was run.

## Files Changed

- `src-tauri/codeg-eui-core/src/lib.rs`
- `src-tauri/codeg-eui-core/src/live.rs`
- `src-tauri/codeg-eui-core/src/model.rs`
- `src-tauri/codeg-eui-core/src/perf.rs`
- `src-tauri/codeg-eui-core/src/runtime.rs`
- `src-tauri/codeg-eui-core/tests/live_recovery.rs`
- `src-tauri/codeg-eui-core/tests/interaction_decline.rs`
- `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-6-report.md`

`lib.rs` is the necessary Task 6 module/export integration point. No shared
`SessionState` source or prior-task behavior was modified.

## Concern

Dependency-complete `codeg-eui-core` compilation and execution of the real
focused tests remains host-OOM-limited. Focused actual-source and contracts-only
proof is green; the dependency-complete target must be rerun on a host with more
memory or usable swap.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Added epoch-fenced live snapshot projection with atomic attach, bounded recovery, retained terminal/interaction evidence, structured transcript projection, native markers, and one deduplicated fail-closed interaction policy.","commits":[{"sha":"9cf90829","subject":"feat(eui): add recoverable live stream projection"},{"sha":"90372cf5","subject":"fix(eui): harden live turn recovery boundaries"},{"sha":"48e083d7","subject":"fix(eui): preserve terminal live recovery evidence"}],"tests":{"status":"partial","passed":51,"failed":0,"summary":"48 focused Rust tests/probes and 3 contracts-only CTest cases pass; session_contract compiles in the focused harness. Dependency-complete shared-codeg compilation is host-OOM-limited."},"concerns":["Dependency-complete focused codeg-eui-core tests require more memory or usable swap; full Cargo suites were parent-skipped."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-6-report.md"}
-->
