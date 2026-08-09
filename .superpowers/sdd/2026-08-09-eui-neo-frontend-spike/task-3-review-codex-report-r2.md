# Task 3 Fix Re-review (Codex R2)

## Verdict

**`approve`**

I1 is addressed. The synthetic blocked-request stimulus is absent from the
normal Rust API and normal C archive, while the opt-in feature retains the
Rust shutdown contract and the single intended C test export. No new defect
was found in the scoped six-file fix delta.

## Finding Disposition

| Finding | Disposition | Result |
| --- | --- | --- |
| I1: blocked-request test hook exposed by normal Rust library | **ADDRESSED** | The helper, payload variant, pending executor branch/import, and shutdown integration test are all gated by `ffi-test-hooks`; normal API and symbol probes confirm absence. |

## I1 Analysis

`enqueue_blocked_for_test` now shares the `ffi-test-hooks` gate with the C
export (`src-tauri/codeg-eui-core/src/abi.rs:432-455`). The only constructor of
the synthetic payload is therefore unavailable in a normal build. The payload
variant itself is gated at `commands.rs:27-28`, and both
`std::future::pending` and the forever-pending executor match arm are gated at
`runtime.rs:2-3` and `runtime.rs:240-241`.

The Rust shutdown integration test is explicitly opt-in at
`tests/shutdown_contract.rs:1`; with `ffi-test-hooks` enabled it still accepts
one blocked request, observes exactly one cancelled completion in a stopping
frame, and completes final shutdown. Without the feature, a normal rlib
consumer cannot resolve `enqueue_blocked_for_test`.

The new `assert_rust_hook_absent.sh` probe checks that negative API contract.
With a dependency-complete normal rlib directory it passes on the expected
missing-item diagnostic; when deliberately pointed at the feature-enabled
rlib it fails with its explicit "unexpectedly exposes" message. This proves
the guard is discriminating API availability rather than accepting an
unrelated compiler failure. Fresh archive inspection also finds zero
`codeg_eui_test_*` exports normally and exactly
`codeg_eui_test_enqueue_blocked` with the feature.

## Regression Audit

- Normal and feature-enabled builds both compile the changed modules with
  `-D warnings`.
- Normal internal, bridge, and ABI probes retain their prior behavior.
- Feature-enabled internal and shutdown probes pass, preserving cancellation,
  ready-frame visibility, and final-free ordering.
- The ABI-linked C++ shutdown-drain consumer still links only against the
  feature archive and passes together with the unchanged ABI layout and
  deep-copy contracts.
- The fix adds no public production symbol, changes no ABI layout or error
  code, and does not alter admission, completion, frame, or lifecycle logic.

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 3 Reviewer 1 (Codex), scoped I1 re-review |
| Work unit | `task\|3\|reviewer\|codex\|none` |
| Reviewed task ID | `e83e1833-71b2-412f-a158-bea9a83bd423` |
| Fix base | `b55f20ddb97706ebd78126e5ffd5ef4cb249ab57` |
| Producer artifact / commit | `66f7cff1ee5b02773f19f938482c3a112792ecb0` |
| Scope | Prior I1 plus breakage introduced by the fix delta |

The producer commit exists at `HEAD`, its sole parent is the stated fix base,
and the package accurately describes the six changed paths. The worktree was
clean before the review artifact was written. The approved design digest
remains
`b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.

## Independent Verification

Passed locally:

- Fresh direct normal and `ffi-test-hooks` rlib/staticlib compilation with
  `rustc -D warnings` against the established shape-compatible core stub
- Dependency-complete negative normal-rlib API probe; positive feature-rlib
  API type-check; and two-direction validation of
  `assert_rust_hook_absent.sh`
- Normal static archive: zero `codeg_eui_test_*` exports
- Feature static archive: exactly `codeg_eui_test_enqueue_blocked`
- Normal focused probes: **13 passed, 0 failed** (6 internal, 6 bridge, 1 ABI)
- Feature-focused probes: **7 passed, 0 failed** (6 internal, 1 shutdown)
- Fresh ABI-linked contracts-only CMake build, exact registration guard, and
  CTest: **4/4 passed**
- C11/C++17 header checks with
  `-Wall -Wextra -Wpedantic -Werror`
- Shell syntax, both Cargo formatting checks, manifest metadata, commit
  parent/scope, and `git diff --check`

Per the mandatory parent rule, no full package/workspace Cargo test, full
library test, or broad shared-`codeg` suite was run or required.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"Task 3 I1 is addressed: the blocked shutdown stimulus is absent from normal Rust/C surfaces and remains available only under ffi-test-hooks, with focused shutdown coverage preserved.","reviewed_task_id":"e83e1833-71b2-412f-a158-bea9a83bd423","artifact_digest":"66f7cff1ee5b02773f19f938482c3a112792ecb0","concerns":[],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-review-codex-report-r2.md"}
-->
