# Task 1 High-Risk Review (Codex)

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 1 Reviewer 1 (Codex) |
| Work unit | `task\|1\|reviewer\|codex\|none` |
| Reviewed task ID | `0bec4a1c-3f2a-4d20-9dab-379a187dc435` |
| Base | `ac1e38d52dc48d9038a33e964086f665d1b21148` |
| Producer commit / artifact digest | `6fcfd6999d69d16d829b0410c1e828069aec0628` |
| Policy | `b2d_task_risk_v1` (`high`: public ABI and unsafe FFI surface) |

The producer commit exists at `HEAD`, its sole parent is the stated base, and
its 18 changed paths exactly match the Task 1 package. The approved design
digest was independently recomputed as
`b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.

## Verdict

**`approve_with_minors`**

No source defect or Task 1 specification violation was found. The only minor
is an evidence limitation caused by the disclosed 4 GiB host constraint; it
does not require a Task 1 code change.

## Findings

### Critical

None.

### Important

None.

### Minor

#### M1. Dependency-complete Cargo and build-script gates remain host-limited

The producer's exact
`cargo test --manifest-path src-tauri/codeg-eui-core/Cargo.toml --test abi_smoke`
and `codeg-eui/scripts/build.sh` runs were killed while compiling the existing
shared `codeg` dependency on a 4 GiB/no-swap host. Direct `rustc` verification
exercises the committed ABI source and test, but it does not independently
prove the complete Cargo dependency graph or the full orchestration sequence.

This is accurately disclosed in the producer report, produced no Rust
diagnostic, and is not evidence of a source failure. Retain a rerun of those
two exact commands on a higher-memory prepared Linux host as residual delivery
evidence.

## Specification Audit

- **Provenance and scope:** the commit parent equals the supplied base;
  `git diff --check` passes; only the 18 Task 1 paths changed; no generated
  build, target, result, screenshot, object, archive, or lockfile is committed.
- **Pinned dependency:** `.gitmodules` uses the required GitHub URL and the
  gitlink/submodule both resolve to
  `cb70ea8bea263efa7805a40c07135df028ad44b1` (`v0.5.5`).
- **Default-build isolation:** `src-tauri/Cargo.toml`, `package.json`, and
  `next.config.ts` have no Task 1 diff and contain no `codeg-eui` reference.
  The new manifest is standalone, produces `staticlib` and `rlib`, and depends
  on `codeg` with `default-features = false`. Its two local Cargo patches mirror
  the parent manifest and are necessary for standalone resolution.
- **ABI v1:** Rust and C agree on constants, symbol names, field order, size,
  alignment, and offsets. All five exports are unmangled. Public entries use
  panic containment; poll rejects null output before dereference; M0 enforces
  `init -> begin_shutdown -> successful stopping poll/ready -> shutdown`, and
  rejects final shutdown before readiness.
- **C++ contracts:** the repository-owned harness is version 1, retains the
  requested assertion behavior, and registers the two exact headless tests.
  Contracts-only configuration does not traverse EUI, GLFW, OpenGL, or the
  Rust archive.
- **Native shell:** CMake consumes the pinned EUI `glfw_app_main.cpp`, calls
  `eui_neo_configure_app`, and links the imported Rust archive. The app creates
  the specified 1180x760/60 fps window, initializes once before compose, polls
  during compose, renders the required bridge-v1 text, and drains M0 shutdown
  from its lifecycle guard.
- **Build orchestration:** `build.sh` is executable, POSIX-valid, Linux-only,
  resolves an absolute repository root, verifies the exact submodule commit,
  builds the standalone release archive, selects GLFW/OpenGL, passes the
  absolute archive path to CMake, and prints the absolute executable path last.
  It does not initialize or mutate the submodule.
- **TDD/package quality:** the reported RED is the brief-prescribed missing
  manifest failure. Focused GREEN evidence is reproducible. The producer
  report and review package accurately describe the commit and the remaining
  host limitation.

## Independent Verification

Passed locally:

- `cargo fmt --manifest-path src-tauri/codeg-eui-core/Cargo.toml -- --check`
- Direct `rustc -D warnings` ABI library/test compilation and smoke execution:
  `1 passed; 0 failed`
- Direct static archive build plus `nm`: exactly the five required
  `codeg_eui_*` global symbols are present
- C11 and C++17 bridge-header compilation with
  `-Wall -Wextra -Wpedantic -Werror`
- Fresh contracts-only CMake configure and build
- Exact registration guard for `codeg_eui_harness_self` and
  `codeg_eui_abi_layout`
- Exact CTest runs and full contracts suite: `2 passed; 0 failed`
- `sh -n` for all three Task 1 shell scripts
- Existing native probe executable resolves its shared libraries and remained
  alive in its GLFW loop for a three-second Xvfb launch smoke (timeout exit)

Not rerun by this reviewer, per the explicit low-memory review instruction:

- Dependency-complete Cargo ABI test
- End-to-end `codeg-eui/scripts/build.sh`

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve_with_minors","verdict":"approve_with_minors","summary":"Task 1 matches the approved M0 ABI/build brief; focused Rust, C/C++, CMake, CTest, symbol, provenance, and native-launch checks pass.","reviewed_task_id":"0bec4a1c-3f2a-4d20-9dab-379a187dc435","artifact_digest":"6fcfd6999d69d16d829b0410c1e828069aec0628","concerns":["The exact dependency-complete Cargo test and build.sh remain unverified on this 4 GiB/no-swap host and should be rerun on a higher-memory prepared Linux host."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-1-review-codex-report.md"}
-->

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":1,"summary":"Task 1 ABI/build spine OK; minor residual: full cargo/build.sh SIGKILL on 4GiB host.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-1-review-codex-report.md"}
-->
