# Task 1 Implementer Report

## Status

DONE_WITH_CONCERNS

Task 1 is implemented and committed. The Rust ABI behavior, C/C++ contracts,
and full pinned EUI-NEO native link pass focused verification. The required
Cargo test and `codeg-eui/scripts/build.sh` were both attempted, but this host
has 4 GiB RAM, no swap, and does not permit `swapon`; their shared `codeg`
compilation was killed by the kernel with `SIGKILL`. No Rust diagnostic or test
failure was produced.

## Commit

- `6fcfd6999d69d16d829b0410c1e828069aec0628` - `feat(eui): add optional native shell build spine`

## Implementation

- Added EUI-NEO as a git submodule at the required URL and exact commit
  `cb70ea8bea263efa7805a40c07135df028ad44b1`.
- Added the independent `codeg-eui-core` Rust `staticlib`/`rlib` crate with
  `codeg` default features disabled.
- Repeated the parent crate's vendored `sacp-tokio` and `kill_tree` Cargo
  patches in the standalone manifest. Cargo only honors patches from the
  top-level manifest; without these entries the standalone crate selected the
  registry `sacp-tokio`, which lacks methods used by the current shared core.
- Added ABI v1 constants, the 24-byte `CodegEuiFrame`, unmangled C exports,
  panic containment, null-poll validation, and the M0 two-phase lifecycle:
  `init -> running -> begin_shutdown -> stopping poll/ready -> shutdown`.
- Added the fixed-width C ABI mirror and compile-time/runtime size, alignment,
  and offset checks.
- Added repository-owned C++ test harness v1, exact CTest registration helper,
  and RED evidence helper.
- Added contracts-only CMake targets that require no EUI, GLFW, or OpenGL
  packages.
- Added the full optional CMake target using EUI's `glfw_app_main.cpp`,
  `eui_neo_configure_app`, GLFW, and OpenGL.
- Added a 1180x760, 60 fps `Codeg EUI Spike` hello window that initializes and
  polls the bridge and drains the M0 shutdown protocol through an RAII guard.
- Added a Linux-only deterministic build script that verifies the submodule
  pin, builds the Rust release archive, explicitly selects GLFW/OpenGL, builds
  CMake, and prints the absolute binary path last.
- Added exact generated-output ignores. No root Cargo workspace, existing
  Cargo manifest, package manifest, or default build path was changed.

## TDD Evidence

### RED

Command, run after adding `tests/abi_smoke.rs` and before creating the crate:

```text
cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke
```

Expected failure, exit 101:

```text
error: manifest path `src-tauri/codeg-eui-core/Cargo.toml` does not exist
```

This was the Task 1 brief's specified RED: the independent crate and ABI
symbols did not exist yet.

### GREEN Behavior

The exact Cargo command could not complete on this host because its single
`rustc` process compiling the existing `codeg` library exceeded 4 GiB and was
killed with signal 9. One-job, no-incremental, no-debug builds and a 1024
codegen-unit attempt reached the same `codeg` invocation and the same
`SIGKILL`. `/proc/meminfo` reported `MemTotal: 4024496 kB`, `/proc/swaps` was
empty, and a task-specific `swapon` probe failed with `Operation not
permitted`.

The committed test and production source were therefore compiled directly,
without bypassing any ABI code under test:

```text
rustc -D warnings --edition=2021 --crate-name codeg_eui_core --crate-type rlib \
  src-tauri/codeg-eui-core/src/lib.rs -o /tmp/libcodeg_eui_core_verify.rlib
rustc -D warnings --edition=2021 --test \
  src-tauri/codeg-eui-core/tests/abi_smoke.rs \
  --extern codeg_eui_core=/tmp/libcodeg_eui_core_verify.rlib \
  -o /tmp/codeg_eui_abi_smoke_verify
/tmp/codeg_eui_abi_smoke_verify
```

Result: `1 passed; 0 failed`.

`nm` on the directly produced static archive confirmed these five exported
symbols:

```text
codeg_eui_api_version
codeg_eui_begin_shutdown
codeg_eui_init
codeg_eui_poll
codeg_eui_shutdown
```

## Verification

Passed:

- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
- Direct Rust ABI smoke test: 1/1 passed, 0 failed, `-D warnings`.
- C header compilation under C11 with `-Wall -Wextra -Wpedantic -Werror`.
- C++ harness and ABI layout compilation under C++17 with
  `-Wall -Wextra -Wpedantic -Werror`.
- `cmake -S codeg-eui -B codeg-eui/build-contract -DCODEG_EUI_CONTRACTS_ONLY=ON`.
- `cmake --build codeg-eui/build-contract --parallel`.
- Exact registration guard selected one test each for
  `codeg_eui_harness_self` and `codeg_eui_abi_layout`.
- `ctest --test-dir codeg-eui/build-contract --output-on-failure`: 2/2 passed,
  0 failed.
- Full native configure/build against the directly built Rust static archive:
  EUI-NEO, bundled GLFW/OpenGL dependencies, and `codeg-eui` all linked
  successfully.
- `sh -n` passed for all three shell scripts.
- `git diff --check` and cached diff check passed before commit.
- Design SHA-256 matched
  `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
- Plan SHA-256 matched
  `76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f`.
- EUI gitlink matched `cb70ea8bea263efa7805a40c07135df028ad44b1`.
- Worktree and submodule status were clean after the producer commit.

Host-limited commands:

- `cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke`
  reached the existing shared `codeg` crate and ended with `signal: 9,
  SIGKILL: kill` before the Task 1 test executable could run.
- `codeg-eui/scripts/build.sh` verified the pin and entered the required
  release Cargo build, then ended at the same shared `codeg` compile with
  `signal: 9, SIGKILL: kill`; its CMake phase was not reached by the script.
  The same full CMake graph was separately built successfully with the exact
  Task 1 ABI archive produced directly by `rustc`.

## Files Changed

- `.gitmodules`
- `codeg-eui/.gitignore`
- `codeg-eui/CMakeLists.txt`
- `codeg-eui/app/app.cpp`
- `codeg-eui/app/bridge/codeg_eui_bridge.h`
- `codeg-eui/scripts/build.sh`
- `codeg-eui/tests/abi_layout_test.cpp`
- `codeg-eui/tests/assert_ctest_red.sh`
- `codeg-eui/tests/assert_ctest_registered.sh`
- `codeg-eui/tests/harness_self_test.cpp`
- `codeg-eui/tests/test_harness.h`
- `codeg-eui/tests/test_main.cpp`
- `codeg-eui/third_party/EUI-NEO` (gitlink)
- `src-tauri/codeg-eui-core/.gitignore`
- `src-tauri/codeg-eui-core/Cargo.toml`
- `src-tauri/codeg-eui-core/src/abi.rs`
- `src-tauri/codeg-eui-core/src/lib.rs`
- `src-tauri/codeg-eui-core/tests/abi_smoke.rs`

## Self-Review

- Confirmed the Rust/C field order, size, alignment, offsets, constants, and
  symbol names match ABI v1.
- Confirmed early final shutdown is rejected and stopping poll readiness is
  required before final shutdown.
- Confirmed public Rust entry points contain panics and null output is checked
  before dereference.
- Confirmed generated targets/builds and the standalone Cargo lockfile are not
  included in the commit.
- Confirmed no Task 2+ data-root, AppState, async command, or session behavior
  was implemented.
- Confirmed no default product manifest or build path references EUI.
- No source defect found during self-review.

## Concerns

- Mandatory Cargo and end-to-end build-script verification require a host with
  more than 4 GiB available memory or usable swap. They remain producer
  evidence gaps on this host despite passing direct ABI tests and a successful
  full native EUI link.

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done","summary":"Added the optional EUI-NEO build spine, ABI v1 M0 lifecycle, headless C++ contracts, and linked hello window.","commits":[{"sha":"6fcfd6999d69d16d829b0410c1e828069aec0628","subject":"feat(eui): add optional native shell build spine"}],"tests":{"status":"pass","passed":3,"failed":0,"summary":"3/3 focused tests pass and the full EUI link passes; mandatory Cargo/build.sh runs were host-memory limited by a 4 GiB SIGKILL."},"concerns":["The exact Cargo test and build.sh need rerun on a host with more than 4 GiB memory or usable swap."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-1-report.md"}
-->

<!-- codeg-card-summary-v1
{"kind":"implementation","phase":"implementation","status":"done_with_concerns","summary":"M0 EUI build spine: submodule, staticlib ABI v1, CMake hello, harness/CTest; host SIGKILL on full cargo test due 4GiB RAM.","commits":[{"sha":"6fcfd6999d69d16d829b0410c1e828069aec0628","subject":"feat(eui): add optional native shell build spine"}],"tests":{"status":"pass","passed":3,"failed":0,"summary":"3 focused ABI/contract tests pass; full cargo test SIGKILL on 4GiB host"},"concerns":["Full cargo test and build.sh hit SIGKILL compiling shared codeg on 4GiB no-swap host"],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-1-report.md"}
-->
