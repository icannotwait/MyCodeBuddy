# Task 2 Implementer Report

## Status

DONE_WITH_CONCERNS

Task 2 is implemented and committed. The isolated data-root resolver and
process pin, authoritative ABI root, EUI bootstrap ownership, `WebOnly`
`AppState` profile, excluded-service dormancy, and same-root ABI restart were
covered by focused tests. Both high-risk reviewers found no Critical issue,
and the separate Codex reviewer marked the final staged package READY.

The four required Cargo commands were attempted with the repository's
one-job/no-debug low-memory profile. Each reached the existing shared `codeg`
crate and its `rustc` process was killed with signal 9 on this 4 GiB/no-swap
host before Task 2 could compile or run under Cargo. No Rust diagnostic or
test assertion failure was emitted.

## Commit

- `8bac8d78bcdf7f189304fa714d068e2d73ddb541` - `feat(eui): add isolated core bootstrap profile`

## Implementation

- Added a pure EUI root resolver with the required precedence:
  `CODEG_EUI_DATA_DIR`, `XDG_DATA_HOME/codeg-eui`, then
  `HOME/.local/share/codeg-eui`.
- Captured the startup working directory and lexically absolutized relative
  roots without requiring the destination to exist.
- Added a process-once immutable root pin. It rejects a different normalized
  root, rejects embedded NUL before committing the pin, removes `CODEG_HOME`,
  and overwrites `CODEG_DATA_DIR` before logging, Tokio, DB, or `AppState`.
- Made a non-empty ABI data-root argument authoritative, with the frozen
  32,768-byte bound plus null, UTF-8, and embedded-NUL validation.
- Made the ABI own a real `EuiBootstrap`, join its Tokio runtime only after the
  stopping poll reports ready, and permit a full same-root restart after stop.
- Added process-idempotent EUI logging so legal same-root restart does not try
  to reinstall the global tracing subscriber. General `LogGuard` cloning was
  not exposed.
- Added `AppState::new_eui` with `EventEmitter::WebOnly`, zero connections,
  the complete shared field map, disabled document translation, and none of
  the services excluded by the design started by the EUI profile.
- Added explicit joined bootstrap shutdown for product and test paths; `Drop`
  retains background shutdown only as a fallback.
- Added child-process isolation coverage for ambient main roots, ABI argument
  precedence, filesystem placement, input rejection, two-phase shutdown,
  same-root restart, and divergent-root rejection.

## TDD Evidence

### RED

- Direct compilation against the unchanged Task 1 archive failed on the
  missing `EuiRootInputs`, resolver, pin, `DataRootError`, and `EuiBootstrap`
  interfaces.
- The required Cargo RED reached the shared `codeg` compile and was SIGKILLed
  before the new test target could compile.
- The embedded-NUL regression reproduced a panic from `std::env::set_var`
  after the old pin accepted `invalid\0root`; the follow-on valid init was then
  poisoned.
- A same-root init/shutdown/re-init probe reproduced a second global tracing
  subscriber installation panic before EUI logging became process-idempotent.

### GREEN

The actual Task 2 crate modules compiled with `rustc -D warnings` against a
shape-compatible `codeg_lib` stub. The committed tests then produced:

- ABI smoke: 1 passed, 0 failed.
- Data-root and ABI lifecycle isolation: 7 passed, 0 failed.
- EUI bootstrap profile: 1 passed, 0 failed.
- Embedded-NUL real resolver-module probe: 1 passed, 0 failed.

The stub models only consumed shared-core interfaces and filesystem effects;
this evidence does not replace compiling the real shared `codeg` crate.

## Verification

Passed:

- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`
- Actual Task 2 modules compiled directly with `rustc -D warnings` against the
  shape-compatible shared-core boundary.
- Focused direct test probes: 9/9 passed, 0 failed; the additional real
  resolver embedded-NUL probe passed 1/1.
- `cargo metadata --manifest-path src-tauri/codeg-eui-core/Cargo.toml --no-deps`
  confirmed `staticlib`/`rlib` and `codeg` default features disabled.
- `cargo tree` confirmed no Tauri crate in the EUI normal dependency graph.
- `git diff --check` and cached diff check passed before commit.
- Approved design SHA-256 matched
  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
- EUI gitlink remained
  `cb70ea8bea263efa7805a40c07135df028ad44b1`.

Host-limited commands, all ending at shared `codeg` `rustc` with signal 9:

- `cargo --config .cargo/low-memory.toml test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test data_root_isolation -- --test-threads=1`
- `cargo --config .cargo/low-memory.toml test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test bootstrap_profile -- --test-threads=1`
- `cargo --config .cargo/low-memory.toml check --manifest-path src-tauri/codeg-eui-core/Cargo.toml`
- `CARGO_TARGET_DIR=src-tauri/codeg-eui-core/target cargo --config .cargo/low-memory.toml check --manifest-path src-tauri/Cargo.toml --no-default-features --lib`

## Review Disposition

- Grok found no Critical data-root issue. Its lifecycle findings led to joined
  test teardown, an observable absent delegation socket assertion, and a more
  precise EUI profile comment.
- The separate Codex review found the same-root logging restart defect and two
  test calls that mutated environment after worker startup. Logging is now
  process-idempotent, the unsafe calls were removed, and its final re-review
  status was READY.
- The exact brief field map calls the shared delegation-stack constructor,
  which starts recovery-authorization pruning. That task is outside the
  design's explicit excluded-service list; no listener, supervisor, outbox,
  web, pet, updater, chat, automation, auto-title, translation, or reference
  sweeper start function is called by the EUI profile.

## Files Changed

- `src-tauri/codeg-eui-core/src/abi.rs`
- `src-tauri/codeg-eui-core/src/bootstrap.rs`
- `src-tauri/codeg-eui-core/src/data_root.rs`
- `src-tauri/codeg-eui-core/src/lib.rs`
- `src-tauri/codeg-eui-core/tests/abi_smoke.rs`
- `src-tauri/codeg-eui-core/tests/bootstrap_profile.rs`
- `src-tauri/codeg-eui-core/tests/data_root_isolation.rs`
- `src-tauri/src/app_state.rs`
- `src-tauri/src/document_translate/service.rs`
- `src-tauri/src/logging/init.rs`

The brief's example Task 2 staging list omitted `abi.rs` and `abi_smoke.rs`;
both were necessarily updated to connect and verify the authoritative root and
real bootstrap. `Cargo.toml` required no dependency change and was not staged.
The generated standalone `Cargo.lock` was removed before commit.

## Concerns

- The mandatory real shared-core Cargo tests/checks remain unverified on this
  host and need rerun on a machine with more than 4 GiB memory or usable swap.
- EUI keeps the tracing writer guard for process lifetime to support legal ABI
  restart. ABI shutdown therefore cannot finalize that writer; abrupt process
  exit may leave a small tail of queued log lines unflushed.
- Recovery-authorization maintenance is started by the required shared
  delegation-stack constructor even though every explicitly excluded EUI
  auxiliary service remains dormant.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Pinned an isolated EUI data root and added the dormant WebOnly AppState/bootstrap profile with joined ABI lifecycle ownership.","commits":[{"sha":"8bac8d78bcdf7f189304fa714d068e2d73ddb541","subject":"feat(eui): add isolated core bootstrap profile"}],"tests":{"status":"partial","passed":9,"failed":0,"summary":"9/9 focused Task 2 probes pass with -D warnings; all four mandatory Cargo targets were SIGKILLed compiling shared codeg on the 4 GiB no-swap host."},"concerns":["Mandatory real shared-core Cargo verification requires more memory or usable swap.","The process-retained EUI log writer cannot be finalized at ABI shutdown while same-root restart remains legal.","The required shared delegation constructor starts recovery-authorization maintenance outside the explicit excluded-service list."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-report.md"}
-->

## High-Gate Fix Round 1/5

Status: DONE_WITH_CONCERNS

Commit:

- `1e92ed75da0702bc628b5f42e0af7fe5d48c7814` - `fix(eui): avoid env writes on root re-pin`

I1 resolution:

- `verify_or_set_process_pin` now distinguishes the caller that first installs
  the `OnceLock` from callers that only verify an equal pinned root.
- Only the first installer removes `CODEG_HOME` and sets `CODEG_DATA_DIR`.
  Equal same-root ABI restarts return without touching process environment;
  divergent roots still return `DataRootError::AlreadyPinned`.
- Added a test-only env-write phase counter and a full ABI
  init/shutdown/re-init regression. The test reads the counter after the
  process-retained logging worker exists and performs no environment mutation
  from the test itself.

TDD evidence:

- RED: the new full-ABI regression completed the restart but failed with
  `left: 2, right: 1` for environment-write phases.
- GREEN: the same regression passed 1/1 after equal verification became a
  read-only return path.
- Focused direct verification compiled the actual EUI modules with
  `rustc -D warnings`: regression 1/1, isolation 7/7, ABI smoke 1/1.
- The dependency-complete focused Cargo command was attempted with the
  repository low-memory profile and was again SIGKILLed while compiling the
  existing shared `codeg` crate, before the EUI unit test could run.

Remaining concerns are unchanged: real shared-core Cargo verification needs a
higher-memory host; the process-retained log writer has no ABI finalization
path; and recovery-authorization maintenance remains the one background task
started by the brief-required delegation constructor.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done_with_concerns","summary":"Task 2 high-gate I1: equal root re-pins are now read-only, so same-root ABI restart does not repeat process environment writes.","commits":[{"sha":"1e92ed75da0702bc628b5f42e0af7fe5d48c7814","subject":"fix(eui): avoid env writes on root re-pin"}],"tests":{"status":"pass","passed":9,"failed":0,"summary":"Focused -D warnings probes pass: env-write ABI restart 1/1, root isolation 7/7, ABI smoke 1/1; dependency-complete Cargo remains host-SIGKILLed."},"concerns":["Full Cargo test/check still SIGKILLs compiling shared codeg on the 4 GiB no-swap host.","The process-retained EUI log writer still has no ABI finalization path.","Recovery-authorization maintenance remains active via the required delegation constructor."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-report.md"}
-->

## High-Gate Fix Round 2/5

Status: DONE_WITH_CONCERNS

Commit:

- `be8b41cf8545470694e2d0b490ec5b6f6cb1a227` - `fix(eui): make root pin initialization atomic`

N1 resolution:

- The first `CODEG_HOME` removal and `CODEG_DATA_DIR` assignment now execute
  inside `PINNED_EUI_DATA_ROOT.get_or_init`. The `OnceLock` publishes the root
  only after both environment writes complete, so concurrent equal or
  divergent callers wait for the complete trust-boundary transition.
- Later equal re-pins remain read-only, preserving the Round 1 same-root ABI
  restart behavior. Divergent roots still return
  `DataRootError::AlreadyPinned` after initialization completes.
- Added a barrier-controlled concurrency regression that pauses the first
  initializer in the environment-write phase, verifies the root is not yet
  published, and verifies an equal caller cannot return until the first pin
  completes.

TDD evidence:

- RED: against the Round 1 implementation, the regression failed with
  `equal pin returned before env write completed` (0 passed, 1 failed).
- GREEN: after moving the writes into the `OnceLock` initializer, the atomic
  pin lifecycle regression passed 1/1. The root isolation suite passed 7/7
  and ABI smoke passed 1/1; all three focused probes use the actual Task 2
  modules compiled with `rustc -D warnings` against the shape-compatible
  shared-core boundary.
- Fresh verification reran all 9 focused tests successfully and both EUI-core
  and shared Rust formatting checks passed.
- The dependency-complete focused Cargo test was attempted with the
  repository low-memory profile and was again SIGKILLed while compiling the
  existing shared `codeg` crate, before the EUI unit test could run.

Remaining concerns are unchanged: real shared-core Cargo verification needs a
higher-memory host; the process-retained log writer has no ABI finalization
path; and recovery-authorization maintenance remains the one background task
started by the brief-required delegation constructor.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"fix","status":"done_with_concerns","summary":"Task 2 high-gate N1: root publication and first environment writes now complete atomically, so concurrent equal callers cannot return early.","commits":[{"sha":"be8b41cf8545470694e2d0b490ec5b6f6cb1a227","subject":"fix(eui): make root pin initialization atomic"}],"tests":{"status":"pass","passed":9,"failed":0,"summary":"Focused -D warnings probes pass: atomic pin lifecycle 1/1, root isolation 7/7, ABI smoke 1/1; dependency-complete Cargo remains host-SIGKILLed."},"concerns":["Full Cargo test/check still SIGKILLs compiling shared codeg on the 4 GiB no-swap host.","The process-retained EUI log writer still has no ABI finalization path.","Recovery-authorization maintenance remains active via the required delegation constructor."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-report.md"}
-->
