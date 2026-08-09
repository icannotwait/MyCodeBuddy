# EUI-NEO Frontend Spike Plan Author Report

## Status

Done.

- Work unit: `plan|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md|author|codex|none`
- Plan: `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md`
- Plan digest: `sha256:4256189d01c83f97adb8c53f04952eebf5d73c3d7a627f8e940e38422792a2db`
- Plan commit: `6a573c602817b77887b4dbcf2d6a8c96e04f2f19`
- Commit subject: `docs: plan EUI-NEO frontend spike`
- Committed scope: the plan file only

## Inputs

- Approved design: `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md`
- Verified design digest: `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`
- Design review: `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/design/design-review-codex-report-r2.md`
- Planning method: `writing-plans`, with explicit files, interfaces, RED/GREEN checks, verification commands, producer commits, and review packages
- EUI-NEO pin verified for the plan: tag `v0.5.5`, commit `cb70ea8bea263efa7805a40c07135df028ad44b1`

## Task And Risk Summary

The plan contains 11 Tasks under `b2d_task_risk_v1`:

- High: 10 Tasks (`1-9`, `11`), implementer `codex`, reviewers `codex` in a separate reviewer thread plus `grok`
- Normal: 1 Task (`10`), implementer `grok`, reviewer `codex`
- Every Task has hard-trigger evidence or `none`, enumerated soft-signal evidence, a checked soft total, a final level with reason, implementer, reviewer set, and policy version.
- Sequence: Tasks 1-9 implement, automatically verify, commit, and prepare producer review evidence; Task 10 performs pre-Final aggregation; Task 11 performs Final verification, independent reviews, report commit, and delivery.
- No human UAT, manual sign-off, or user approval is a mid-sequence gate. Human acceptance is confined to post-delivery residual work.

## Design Coverage

- M0: optional independent Rust `staticlib`, pinned EUI submodule, CMake-led Linux GLFW/OpenGL shell, and ABI hello path
- M1: process-pinned EUI root, ambient `CODEG_HOME` removal, `CODEG_DATA_DIR` overwrite, EUI logging prefix, isolated SQLite, and `WebOnly` AppState profile
- Bridge contract: ABI v1, bounded inputs and queues, async request/completion, immutable polled frames, UI affinity, panic containment, and deterministic shutdown
- M2: narrow Grok/Codex settings and probe facade over existing backend persistence
- M3: workspace, conversation, history, connection spawn/resume, selection epochs, and linked send
- M4: authoritative snapshot plus subscription, gap/lag/saturation recovery, live projection, and deterministic permission/question/plan decline
- M5: native shell/chat/settings controls, stale completion handling, cancel fencing, crash recovery, and P1 settings
- M6: common EUI/WebView markers, fixed 50 ms threshold, shell-only RSS, reproducible aggregation, and filled local comparison evidence
- Final regression: default desktop, server, and MCP builds are checked with EUI sources unavailable

The four design-review minors are resolved in the plan with `int codeg_eui_shutdown(void)`, ambient `CODEG_HOME` removal before core startup, successful-poll completion draining with retained frame bytes, and a shared fixed 50 ms long-frame threshold.

## Self-Review And Verification

- Spec coverage: M0-M6, acceptance requirements, optional-build boundary, real-agent evidence, and default-build independence map to producer Tasks and Final evidence.
- Placeholder scan: no planning placeholders remain; the only broad-scan matches are intentional `tests/*.rs` and `tests/*.cpp` file globs.
- Type consistency: root-pin, facade, session, smoke-hook, frame-marker, and completion names/signatures were normalized across interface and implementation steps.
- Routing validation: 11 Task headings, 11 global rows, 10 high, 1 normal, and 11 local `b2d_task_risk_v1` rows.
- Git validation: `git diff --cached --check` and `git diff HEAD^ --check` exited zero; the committed diff adds only the plan file.
- Digest validation: `sha256sum` was run after final plan write and again after commit; both produced `4256189d01c83f97adb8c53f04952eebf5d73c3d7a627f8e940e38422792a2db`.

No implementation, application build, Rust/C++/frontend test suite, lint suite, or runtime smoke was run in this work unit because its scope is plan authoring. The plan assigns those commands and evidence gates to producer and Final Tasks.

## Revision After Initial Plan Review

The initial Codex Plan review returned `request_changes` with 7 Important and
2 Minor findings; Grok approved. The Plan was revised in place without changing
the approved design.

- Revised Plan digest: `sha256:06ad8b84d175fde69662e2b29a6a7f975554f69f962fbda165ee2c6b37e50767`
- Revision commit: `255f965c607fb7cb42bbdf70008b33f0144e49ec`
- Revision subject: `docs: revise EUI-NEO spike plan after review`
- Committed scope: the Plan file only

### Finding Disposition

- I1: Added an externally observable two-phase shutdown: begin drain, legal
  stopping polls, a copied `shutdown_ready=1` frame after all accepted IDs are
  exposed, then final join/free. Added Rust and black-box C++ blocked-request
  drain tests plus exact contract-only Rust archive link instructions.
- I2: Made `t_first_presented` post-presentation in both shells: EUI marks in
  the next-update `onFrame`; WebView marks in RAF2 after RAF1 and an eligible
  paint opportunity. Added phase-order RED/GREEN tests.
- I3: Added explicit EUI build/results/screenshots and nested Cargo target
  ignores, exact producer staging paths and dry runs, ignored local evidence
  ownership, `git add -f` for the Final report, and clean-worktree checks.
- I4: Added broad default Rust tests and clippy, server tests/clippy, and MCP
  check/clippy while the EUI source directory is safely held aside and restored.
- I5: Made successful real Grok and Codex streaming evidence mandatory for
  Final delivery. Missing or unsuccessful agent setup is a blocker, not an
  acceptance skip; skip notation remains performance-only.
- I6: Initial attach and every resync now decline snapshot-pending permission,
  question, and plan interactions through one deduplicated policy before event
  receive resumes. Snapshot-only tests cover each interaction kind.
- I7: Split bridge, projector, native UI, recovery controls, presentation hooks,
  and performance CLI work into focused test-first RED/implementation/GREEN
  steps with concrete Rust, C++, TypeScript, CMake, and shell skeletons.
- M1: A non-empty `codeg_eui_init(data_dir,len)` argument is authoritative;
  only an empty argument consults `CODEG_EUI_DATA_DIR`, followed by XDG/home
  defaults. A differing argument/environment integration case is required.
- M2: `perf_compare.sh` now has exact `record-eui`, `record-webview`,
  `aggregate`, `validate`, and `self-test` interfaces, with public-dispatcher
  end-to-end tests and self-test coverage of every subcommand.

### Revision Verification

- Approved design digest rechecked as
  `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
- Plan structure rechecked: 11 Task headings, 11 global risk rows, 11 local
  risk rows, 10 high Tasks, and 1 normal Task under `b2d_task_risk_v1`.
- Placeholder scan and stale-protocol searches returned no unresolved matches.
- `git diff --check` and `git diff --cached --check` exited zero before commit.
- The staged index contained only the Plan path; `git show --stat` confirms the
  revision commit changes one file.
- `sha256sum` after the final Plan write and after commit returned the revised
  digest above.

This report is an ignored workflow artifact and is intentionally not included
in the Plan-only revision commit.

## Revision After Plan Re-review R2

Codex re-review R2 confirmed I1-I6 and M1-M2 addressed, but retained one
Important residual under I7: the C++ RED/GREEN suites had no defined harness
or complete CMake/CTest registration and could pass through an empty filtered
CTest selection. Grok re-approved. The Plan was revised without changing the
approved design or the 11-Task risk routing.

- Plan digest: `sha256:76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f`
- Plan commit: `ac1e38d52dc48d9038a33e964086f665d1b21148`
- Commit subject: `docs: pin C++ test harness for EUI-NEO plan`
- Committed scope: the Plan file only

### Residual I7 Disposition

- Chose a repository-owned, dependency-free C++17 harness frozen as
  `CODEG_EUI_TEST_HARNESS_VERSION=1`; no GoogleTest/Catch2 fetch or native EUI
  dependency is introduced for contracts-only tests.
- Added exact Plan ownership for `test_harness.h`, `test_main.cpp`, harness
  self-test, exact-registration guard, and RED-evidence guard in Task 1 before
  the first later C++ RED.
- Defined `codeg_eui_add_contract_test(name, source)` to create executable
  `${name}_test`, link the shared custom main, and call `add_test` exactly once.
- Added a complete registry for 11 CTest names: harness self-test, ABI layout,
  shutdown drain, UI snapshot, client completion, app lifecycle, shell page,
  chat page, settings page, M5 controls, and performance metrics.
- Required every C++ RED to add its CMake registration no later than that RED,
  build a compile-safe neutral implementation, prove anchored `ctest -N`
  selection count `1`, and then match the named harness `[FAIL]` assertion.
  Configure/compile failure and zero-test selection are explicitly invalid RED.
- Separated RED evidence commands from GREEN commands. GREEN runs rebuild the
  exact target, reassert one anchored registration, and require the anchored
  CTest name to pass.
- Final verification enumerates all 11 exact registrations before the full
  CTest suite, so an omitted target cannot produce false-green evidence.
- Updated Task 10/Final delivery-base lookup to this latest Plan commit subject.

### R2 Revision Verification

- Design digest remains
  `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.
- Plan structure remains 11 Tasks, 22 total risk rows (global plus local),
  10 high Tasks, and 1 normal Task under `b2d_task_risk_v1`.
- Static registry audit found 11 registry rows and a matching
  `codeg_eui_add_contract_test` invocation for every exact name.
- Placeholder scan, exact-filter scan, and `git diff --check` exited clean.
- `git diff --cached --check` exited zero and the staged index contained only
  the Plan path.
- The Plan digest matched before and after the one-file commit.

The ignored author report remains outside the Plan-only commit.
