# Task 1 Independent Review (Grok) — EUI Build Spine & Hello Window

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 1 high-risk Reviewer 2 (Grok) |
| Work unit | `task\|1\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| reviewed_task_id | `0bec4a1c-3f2a-4d20-9dab-379a187dc435` |
| Commit (HEAD) | `6fcfd6999d69d16d829b0410c1e828069aec0628` |
| BASE | `ac1e38d52dc48d9038a33e964086f665d1b21148` |
| Producer status | `DONE_WITH_CONCERNS` (host SIGKILL on full cargo) |
| Brief | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-1-brief.md` |
| Report | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-1-report.md` |
| Review package | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-1-review-package.md` |
| Global constraints | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/global-constraints.md` |
| Risk | `high` (`unsafe_ffi` + `public_compatibility`); policy `b2d_task_risk_v1` |

This review is independent of the implementer and of any Codex reviewer thread.
Findings are from commit contents, brief/spec cross-check, and re-run headless
verification on this host.

## Overall Assessment

Task 1 delivers the optional EUI M0 spine as specified: pinned EUI-NEO
submodule, independent `codeg-eui-core` staticlib/rlib, ABI v1 with two-phase
shutdown, C mirror header, repository harness v1 + exact CTest registration,
contracts-only CMake path, hello `codeg-eui` target + build orchestrator, and
generated-output ignores. Product manifests and default build paths remain
uncoupled.

High-risk ABI surface (layout, symbols, null safety, panic containment, lifecycle
ordering) checks out under independent re-verification. No source defect that
blocks Task 2 was found.

**Verdict: `approve_with_minors`**

Minors are residual host cargo/`build.sh` evidence debt and a soft test-coverage
nit. Neither requires a code change before continuing the task sequence.

## Spec Compliance Matrix

| Requirement | Result | Notes |
| --- | --- | --- |
| Submodule URL `https://github.com/sudoevolve/EUI-NEO.git` | Pass | `.gitmodules` |
| Gitlink pin `cb70ea8bea263efa7805a40c07135df028ad44b1` (v0.5.5) | Pass | mode `160000`; local `rev-parse` matches; `build.sh` enforces pin |
| Independent crate `codeg-eui-core` with `staticlib`+`rlib` | Pass | |
| `codeg = { path = "..", default-features = false }` | Pass | |
| No root Cargo workspace; no refs from `src-tauri/Cargo.toml`, `package.json`, default paths | Pass | Grepped product manifests / default tauri paths |
| ABI constants v1: OK=0, INVALID_STATE=1, NULL_POINTER=2, NOT_READY=9 | Pass | Rust + C agree |
| `CodegEuiFrame` 24 bytes; generation@8; shutdown_ready@16 | Pass | C sizeof/offset + C++ static_assert + layout test |
| Exports: `api_version`, `init`, `poll`, `begin_shutdown`, `shutdown` | Pass | `nm` on staticlib shows all five `T` symbols |
| Two-phase shutdown: reject final shutdown before ready poll; stopping poll → `shutdown_ready=1` | Pass | Smoke + independent C black-box link |
| Panic does not unwind across ABI | Pass | `catch_unwind` on all entry points |
| Harness v1 + shared `test_main` + five macros | Pass | |
| `codeg_eui_add_contract_test`; registered `codeg_eui_harness_self`, `codeg_eui_abi_layout` | Pass | |
| Contracts-only build without EUI/GLFW/OpenGL | Pass | Rebuilt `build-contract-review`; 2/2 ctest pass |
| Hello app 1180×760, 60 fps, title `Codeg EUI Spike`, text `Codeg EUI / bridge v1` | Pass | `app.cpp` |
| RAII drain: begin_shutdown → poll until ready → shutdown | Pass | `BridgeLifecycle` |
| `build.sh`: `set -eu`, Linux-only, pin check, release rustc archive, absolute `CODEG_EUI_RUST_LIB`, final binary path line, no implicit submodule update | Pass | |
| Ignore files exact | Pass | `codeg-eui/{build,build-*,results,screenshots}`; `codeg-eui-core/target` |
| Commit stages only owned source/gitlink (no build artifacts) | Pass | 18 paths / 538 insertions; no `target`/`build`/binaries |
| Scripts executable | Pass | `100755` for `build.sh` and both assert scripts |

### Justified deviations from brief snippets (not defects)

1. **`[patch.crates-io]` for vendored `sacp-tokio` and `kill_tree`** in the
   standalone crate. Cargo only applies patches from the top-level manifest of
   the build; without these, resolving `codeg` selects registry `sacp-tokio`
   that lacks APIs used by shared core. Required for any real `cargo` build of
   this crate; mirrors parent `src-tauri/Cargo.toml` patches.
2. **Full M0 lifecycle in `abi.rs`** rather than the Step 4 stub that returned
   only `NOT_READY` for non-null poll. Brief Step 4 prose and Step 6 smoke
   requirements mandate `init → running → begin_shutdown → stopping poll with
   shutdown_ready=1 → shutdown`. Implementation correctly prefers the lifecycle
   contract.
3. **`build.sh` pins `-DEUI_WINDOW_BACKEND=glfw -DEUI_RENDER_BACKEND=opengl`** —
   aligns with global Linux GLFW+OpenGL constraint and makes the optional full
   build deterministic.

## Independent Verification (this host)

Host: Linux, `MemTotal` ≈ 4.0 GiB, no swap (same class as producer).

| Check | Result |
| --- | --- |
| `HEAD == 6fcfd699…` | Yes |
| Direct `rustc -D warnings` rlib + `abi_smoke` integration test | **1 passed; 0 failed** |
| `nm` staticlib exports | Five `codeg_eui_*` text symbols present |
| C11 sizeof/offset probe | `sizeof=24 gen=8 ready=16` |
| C black-box link of `/tmp/libcodeg_eui_core.a` exercising full lifecycle | `c_link_abi_ok gen=1 lifecycle=3` |
| C++17 standalone harness + abi_layout (`-Wall -Wextra -Wpedantic -Werror`) | Both `[PASS]` |
| `cmake -S codeg-eui -B … -DCODEG_EUI_CONTRACTS_ONLY=ON` + build + `assert_ctest_registered` + `ctest` | **2/2 passed** |
| `sh -n` on three shell scripts | Pass |
| Product isolation grep | No coupling |
| `cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke` | **SIGKILL (signal 9)** while compiling shared `codeg` (confirmed residual host limit) |
| Producer native artifact `codeg-eui/build-native-probe/codeg-eui` | Present, dynamically linked (GLFW stack resolved at producer build time) |

Headless ABI/contract gates required by global constraints are green when
evaluated without compiling the full `codeg` graph. Full Cargo and `build.sh`
remain unpaid on this memory class, as the producer reported.

## High-Risk Focus (FFI / public ABI)

### Layout & linkage

- Rust `#[repr(C)]` field order and widths match the C header (name difference
  `_reserved` vs `reserved` is ABI-irrelevant).
- `extern "C"` + `#[no_mangle]` on Rust; C++ header wraps declarations in
  `extern "C"`.
- Null `poll` checked before any write; `out.write(frame)` only after null and
  state checks.
- Fixed-width types only in the public C surface.

### Lifecycle machine

```
uninitialized → starting → running → stopping → stopped
```

- `init` allows only `uninitialized` or `stopped`; null pointer with positive
  length rejected.
- `begin_shutdown` is a CAS `running → stopping`; sets ready false.
- First (and subsequent) successful stopping `poll` exposes `shutdown_ready=1`
  and latches `SHUTDOWN_READY`.
- `shutdown` requires `stopping` **and** latched ready; early final shutdown
  returns `INVALID_STATE` (smoke covers this).
- Wrong-state `poll` returns `INVALID_STATE` (appropriate for lifecycle; reserved
  `NOT_READY=9` remains available for later async “not ready” semantics).

UI-thread-only contract means the non-CAS window during `init`’s
`starting → running` stores is acceptable for M0. Process-local atomics are a
reasonable extra belt.

### Panic containment

All five exports use `catch_unwind`; status helpers map panic to
`CODEG_EUI_ERR_INVALID_STATE` (api_version falls back to `0`). Satisfies “Rust
panics never unwind across it.”

### Scope discipline

- No data-root resolution, AppState, async request queues, sessions, or settings
  in this commit (Task 2+).
- `CODEG_EUI_ABI_LINK_TESTS` path references future `shutdown_drain_test.cpp`
  only when the option is on — correct forward hook, not a Task 1 incompleteness.
- Hello binary links imported static archive + Threads/dl/m + EUI configure —
  matches integration contract.

## Findings

### Critical

None.

### Important

None.

### Minor

1. **Residual mandatory Cargo / `build.sh` evidence on ≤4 GiB hosts**  
   Exact `cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke`
   and end-to-end `codeg-eui/scripts/build.sh` still hit kernel SIGKILL while
   compiling shared `codeg`, confirmed independently. Direct `rustc` ABI tests
   and contracts-only CMake/CTest are green; producer also linked a full native
   binary via a hand-built archive. Per global constraints, real native build is
   prepared-host evidence and must not weaken headless gates — but the **cargo
   path remains unverified on this class of machine**. Re-run both commands on a
   host with >4 GiB usable memory (or usable swap) and attach logs before treating
   the Cargo-orchestrated M0 path as CI-green. **No source change required for
   Task 1 acceptance.**

2. **Soft coverage gap in `abi_smoke`**  
   The smoke test asserts `shutdown_ready == 1` but not
   `lifecycle_state == STOPPING (3)` or non-zero `generation`. Independent C
   black-box link observed `lifecycle=3` and `gen=1`, so behavior is correct;
   tightening the smoke assertions in a later touch would harden regression
   signal. Optional; not blocking.

## Non-Findings / Notes

- Standalone `Cargo.lock` may appear locally after cargo attempts; it was not
  in the producer commit (correct). Only `/target/` is gitignored under
  `codeg-eui-core`; future hygiene may ignore the lockfile if the crate stays
  unpublished — out of Task 1 scope.
- Unused crate dependencies (`serde`, `tokio`, `thiserror`, and presently unused
  `codeg` symbols) match the brief’s dependency boundary for upcoming tasks;
  not treated as bloat for M0.
- Harness unused includes (`<string>`, `<utility>`) match the brief pin.

## Verdict Card

```text
VERDICT: approve_with_minors
critical: 0
important: 0
minor: 2
reviewed_commit: 6fcfd6999d69d16d829b0410c1e828069aec0628
reviewed_task_id: 0bec4a1c-3f2a-4d20-9dab-379a187dc435
continue_sequence: yes
code_changes_required: no
residual: re-run cargo test + build.sh on higher-memory host
```

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":2,"summary":"Task 1 OK: pin, ABI layout, lifecycle; minors: host SIGKILL cargo evidence; soft lifecycle asserts in abi_smoke.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-1-review-grok-report.md"}
-->
