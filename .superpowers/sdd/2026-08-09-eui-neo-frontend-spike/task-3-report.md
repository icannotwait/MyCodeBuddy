# Task 3 Implementer Report

## Status

DONE_WITH_CONCERNS

Task 3 implements the asynchronous EUI bridge lifecycle, bounded command
admission, exactly-once request completions, immutable polled frames, and the
two-phase shutdown drain. The public ABI and C++ value-copy boundary are
covered by focused headless contracts.

## Implementation

- Expanded ABI v1 with stable errors `0..9`, lifecycle/operation/completion
  discriminants, pointer-plus-length slices, session summaries, completion
  records, and the complete 160-byte frame layout.
- Replaced the bootstrap slot/atomics with a process-global
  `OnceLock<Mutex<BridgeSlot>>`, UI-thread affinity checks, checked frame
  generations, contained FFI panics, and diagnostic error-strip recording.
- Added Tokio bounded command admission (256), monotonic non-zero request IDs,
  completion-capacity reservation (256), worker error/panic conversion,
  selection-epoch stale marking, and exactly-once terminalization.
- Added immutable `OwnedFrame` backing with owned nested strings and parallel
  C views. Successful polls atomically transfer ready completions into the
  retained frame; failed polls leave the prior frame untouched.
- Added shutdown admission closure, worker/task cancellation, ACP disconnect,
  stopping polls, observable `shutdown_ready`, final runtime teardown, frame
  free, and stopped-state behavior. Worker-exit admission is serialized so an
  unexpected worker exit cannot strand a request accepted concurrently with
  cancellation.
- Added the opt-in `ffi-test-hooks` feature exposing only the blocked-request
  C stimulus, plus Rust shutdown contracts and C++ shutdown-drain and
  deep-copy `UiSnapshot` contracts.
- Corrected message input routing so `send_user_message` accepts message UTF-8
  (including embedded NUL bytes) while path-like APIs reject embedded NULs.

## TDD Evidence

### RED

- The legacy C++ ABI layout test failed against the old 24-byte frame once the
  expanded session/completion frame contract was asserted.
- Direct Rust bridge-contract compilation against the pre-Task-3 archive
  failed because the async request/completion symbols and model types were
  absent.
- A temporary drain implementation that mutated/cancelled the wrong lifecycle
  path failed the named C++ shutdown-drain assertion.
- A neutral snapshot copier failed the named ownership and null/length
  validation assertions.
- The final input regression was reproduced when `send_user_message` was
  accidentally routed through the path validator; the corrected focused path
  NUL contract now passes.

### GREEN

The Task 3 modules were compiled directly with `rustc -D warnings` against a
shape-compatible `codeg_lib`/`EuiBootstrap` boundary, then exercised without
the memory-heavy shared crate:

- Internal ABI/model/runtime tests: **6 passed, 0 failed**.
- `bridge_contract`: **6 passed, 0 failed** (layout, lifecycle/thread,
  invalid input, path NUL rejection, 256-request admission, immutable frame
  retention/completion transfer).
- `shutdown_contract`: **1 passed, 0 failed**.
- ABI smoke: **1 passed, 0 failed**.

## Verification

Passed:

- Direct Rust compilation with `-D warnings` for rlib and staticlib outputs.
- C11 header syntax check with `-Wall -Wextra -Wpedantic -Werror`.
- Contracts-only CMake build and CTest: **3/3 passed** (harness, ABI layout,
  UI snapshot).
- ABI-linked CMake build using the `ffi-test-hooks` archive and CTest:
  **4/4 passed**, including `codeg_eui_shutdown_drain`.
- Exact CTest registration checks for all four Task 3 targets.
- Normal static archive exports no `codeg_eui_test_*` symbols; the
  `ffi-test-hooks` archive exports exactly `codeg_eui_test_enqueue_blocked`.
- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`.
- `cargo fmt --manifest-path src-tauri/Cargo.toml -- --check`.
- `git diff --check`.

Per the parent instruction, **all full Cargo tests were skipped for this Task
and the remaining spike work**. No broad package/workspace test command was
run. A focused dependency-complete Cargo check was attempted with the
repository low-memory configuration; the existing shared `codeg` rustc was
killed by the 4 GiB/no-swap host with `SIGKILL` before the EUI target could
compile. This is an authorized host limitation, not an open Task 3 defect.

## Files Changed

- `src-tauri/codeg-eui-core/Cargo.toml`
- `src-tauri/codeg-eui-core/src/{abi,commands,model,runtime}.rs`
- `src-tauri/codeg-eui-core/src/{bootstrap,data_root,lib}.rs`
- `src-tauri/codeg-eui-core/tests/{abi_smoke,bridge_contract,data_root_isolation,shutdown_contract}.rs`
- `codeg-eui/CMakeLists.txt`
- `codeg-eui/app/bridge/{codeg_eui_bridge.h,ui_snapshot.h}`
- `codeg-eui/tests/{abi_layout_test,ui_snapshot_test,shutdown_drain_test}.cpp`

## Self-Review

- Rust/C field order, sizes, alignment, offsets, discriminants, and exported
  lifecycle declarations match ABI v1.
- All public operations validate UI affinity, lifecycle state, output/input
  pointers, UTF-8, frozen bounds, and path NUL policy before acceptance.
- Raw frame pointers reference only backing vectors owned by the retained
  `OwnedFrame`; empty slices/arrays use null pointers with zero lengths.
- Completion reservation and terminalization are guarded against duplicate
  IDs, and shutdown readiness is latched only after the successful frame copy.
- Generated Cargo/CMake outputs and temporary archives remain ignored and are
  excluded from the implementation package.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"Implemented EUI ABI v1 async lifecycle, bounded request/completion ledger, immutable frames, shutdown drain, and C++ deep-copy boundary.","commits":[{"subject":"feat(eui): implement async bridge lifecycle"}],"tests":{"status":"pass","passed":14,"failed":0,"summary":"14 focused Rust tests plus 3 contracts-only and 4 ABI-linked CTest cases pass; full Cargo tests skipped by parent instruction and shared-core Cargo check SIGKILLed on the 4GiB/no-swap host."},"concerns":["Dependency-complete Cargo verification requires a host with more memory or usable swap; the parent explicitly authorized skipping full Cargo tests for this spike."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md"}
-->
