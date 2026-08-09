# Task 3 High-Risk Review (Codex)

## Verdict

**`request_changes`**

The ABI layout, bounded admission, exactly-once completion ledger, immutable
frame retention, and observable two-phase shutdown pass focused source and
headless contract verification. One Important scope defect remains: the
synthetic blocked-request hook is publicly callable from the normal-feature
Rust library even though the brief requires that test hook to be absent from
normal builds.

## Findings

### Critical

None.

### Important

#### I1. The blocked-request test hook is exposed by the normal Rust library

The manifest correctly makes `ffi-test-hooks` opt-in and keeps it out of the
default feature set (`src-tauri/codeg-eui-core/Cargo.toml:7-9`). The C export
is also gated (`src-tauri/codeg-eui-core/src/abi.rs:447-455`). However,
`enqueue_blocked_for_test` is an unconditional public function at
`abi.rs:432-445`, and `lib.rs:8` re-exports every public ABI-module item.
`CommandPayload::Blocked` and its forever-pending executor branch are likewise
compiled without the feature (`commands.rs:27`, `runtime.rs:237-240`).

This is observable in a normal-feature build, not just a source-style concern:
a fresh Rust type-check against the normal rlib accepted this public API:

```rust
let _hook: fn() -> Result<u64, i32> =
    codeg_eui_core::enqueue_blocked_for_test;
```

Calling it after init reserves a real request/completion slot and spawns work
that can never terminalize except through shutdown cancellation. The normal C
static archive correctly has no `codeg_eui_test_*` symbol, but the crate also
produces an `rlib`; C-symbol dead stripping therefore does not satisfy the
brief's explicit requirement that the test hook be absent from normal builds.

Required change: feature-gate or remove the public Rust helper and gate the
`Blocked` payload/executor path with the same test-only boundary. Configure the
Rust shutdown contract to run with the opt-in feature (for example via a
feature-required test target using the gated hook), or use a non-public
deterministic seam. Add a normal-feature API check proving the hook cannot be
referenced, while retaining the existing normal-vs-feature C symbol check.

### Minor

None.

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 3 Reviewer 1 (Codex) |
| Work unit | `task\|3\|reviewer\|codex\|none` |
| Reviewed task ID | `e53d2f15-9667-4dc8-94d0-ff366f390e36` |
| Base | `be8b41cf8545470694e2d0b490ec5b6f6cb1a227` |
| Producer artifact / commit | `b55f20ddb97706ebd78126e5ffd5ef4cb249ab57` |
| Policy | `b2d_task_risk_v1` (`high`: unsafe FFI and concurrency lifecycle) |

The producer commit exists at `HEAD`, its sole parent is the stated base, and
the package's 19-path diff matches the commit. The worktree was clean before
the review artifact was written. The approved design digest independently
recomputes to
`b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.

## Specification Audit

- **ABI and inputs:** Rust and C agree on stable errors `0..9`, lifecycle,
  operation and completion discriminants, struct order, 64-bit Linux sizes,
  alignment, and offsets. Pointer/length inputs enforce null, UTF-8, frozen
  bounds, and the path-NUL policy before acceptance.
- **Thread and lifecycle:** successful init captures the UI thread; non-API
  calls reject the wrong thread; running/stopping poll order and final
  shutdown readiness are enforced through the process-global slot and panic
  boundary.
- **Admission and terminalization:** a Tokio queue permit is acquired before
  request allocation/reservation. Completion reservation caps all unobserved
  accepted work at 256, and worker success, error, panic, stale, cancellation,
  and unexpected-exit paths converge on one terminal ledger entry.
- **Shutdown race:** worker exit takes the admission guard, cancels the
  remaining ledger, and only then publishes quiescence. A ready stopping frame
  therefore snapshots and drains every remaining completion before
  `shutdown_ready_observed` permits runtime/frame destruction.
- **Frame ownership:** nested session/completion bytes and their C views are
  retained by one `OwnedFrame`; enqueue and failed polls do not replace it,
  while a successful poll atomically transfers ready completions and replaces
  the prior frame.
- **C++ copy boundary:** `UiSnapshot` deep-copies all frame and nested slice
  fields and rejects null/nonzero pairs. The headless ownership and
  shutdown-drain consumers are registered under the exact required CTest
  names.
- **Open defect:** the normal Rust API still exposes the synthetic blocked
  stimulus described in I1, so Task 3 does not yet meet the opt-in test-hook
  boundary.

## Independent Verification

Passed locally:

- Fresh direct `rustc -D warnings` compilation of the Task 3
  `abi`/`commands`/`model`/`runtime` modules against a shape-compatible
  `AppState`/bootstrap stub
- Fresh direct Rust unit probes: **6 passed, 0 failed**
- Fresh direct `bridge_contract`: **6 passed, 0 failed**
- Fresh direct `shutdown_contract`: **1 passed, 0 failed**
- Fresh direct ABI smoke: **1 passed, 0 failed**
- Fresh normal and `ffi-test-hooks` staticlib compilation; the normal archive
  exports no `codeg_eui_test_*`, and the feature archive exports exactly
  `codeg_eui_test_enqueue_blocked`
- Normal-feature rlib type-check proving the unintended public Rust helper in
  I1 is reachable
- C11/C++17 header syntax with
  `-Wall -Wextra -Wpedantic -Werror`
- Fresh contracts-only ABI-linked CMake build, exact registration guard, and
  CTest: **4/4 passed**
- Both Cargo formatting checks, manifest metadata inspection,
  `git diff --check`, commit-parent/scope checks, and clean-worktree check

Per the mandatory parent rule, no full package/workspace Cargo test, full
library test, or broad shared-`codeg` suite was run or required. This is an
authorized residual and is not counted as a finding.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"request_changes","verdict":"request_changes","critical":0,"important":1,"minor":0,"summary":"Task 3 core ABI, bounded completion, immutable-frame, and shutdown contracts pass focused verification, but the blocked-request test stimulus remains publicly callable from the normal-feature Rust library.","reviewed_task_id":"e53d2f15-9667-4dc8-94d0-ff366f390e36","artifact_digest":"b55f20ddb97706ebd78126e5ffd5ef4cb249ab57","concerns":["Gate or remove the normal-feature public enqueue_blocked_for_test API and its forever-pending payload path; retain the stimulus only behind the opt-in test feature."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-review-codex-report.md"}
-->
