# EUI-NEO Frontend Spike Plan Review (Grok)

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Plan Reviewer (Grok), separate from Plan Author and Codex plan reviewer |
| Work unit | `plan\|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| Design | `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md` |
| Design digest (verified) | `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| Plan | `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md` |
| Plan digest (verified) | `sha256:4256189d01c83f97adb8c53f04952eebf5d73c3d7a627f8e940e38422792a2db` |
| Author report | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-author-report.md` |
| Policy | `b2d_task_risk_v1` |
| Review brief | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-review-brief.md` |

Digest verification was re-run with `sha256sum` on both files; both match the brief and author report.

## Overall Assessment

The plan is an executable SDD decomposition of the approved design. It covers
M0–M6, the bridge/FFI contracts, acceptance checklist, optional-build boundary,
and post-delivery residual human work. Risk rows are complete, arithmetically
correct under `b2d_task_risk_v1`, and correctly routed. No mid-sequence human
UAT, manual sign-off, or user-approval gates appear. High-risk FFI and
concurrency work is implementer-`codex` with dual reviewers; the only normal
Task is pre-Final aggregation (Task 10, implementer `grok`).

**Verdict: `approve`**

No Critical, Important, or Minor findings that require Plan revision before
Task admission.

## Spec / Design Coverage

### Milestones M0–M6

| Design milestone / contract | Plan coverage | Evidence |
| --- | --- | --- |
| M0 optional staticlib, pinned EUI, CMake hello, init/poll | Task 1 | Submodule pin `v0.5.5` / `cb70ea8…`, independent crate, ABI v1 skeleton, `build.sh`, headless layout test |
| M1 isolated data root + `WebOnly` AppState | Task 2 | Pure resolver, process pin, `CODEG_HOME` removal, `AppState::new_eui`, isolation/bootstrap tests |
| Bridge lifecycle, async request/completion, immutable frames | Task 3 | Lifecycle machine, bounds, panic fence, queues, exactly-once completion, shutdown drain, C++ deep-copy |
| M2 Grok/Codex settings + probe facade | Task 4 | Narrow DTO facade over ACP cores; async get/set/probe; secrets redaction |
| M3 workspace / create / select / history (+ send admission) | Task 5 | Folder/conversation cores, spawn order, linked send, selection epochs |
| M4 live stream, resync, P0 interaction decline | Task 6 | Snapshot+subscribe under one lock, lag/overflow/switch, permission/question/plan terminal decline |
| Recognizable shell / chat / P0 settings UI | Task 7 | Frame copy model, pages, markdown throttle 75 ms, smoke-exit hook |
| M5 error recovery, switch, cancel, P1 settings | Task 8 | Epoch-fenced cancel, crash retain text, structured P1 controls |
| M6 comparable perf + README table | Task 9 | Shared anchors, fixed 50 ms threshold, shell-only RSS, fixture, filled local table |
| Pre-Final freeze / scope audit | Task 10 | Allowlist, pin proof, traceability checklist, no suite re-run required |
| Final verify + dual independent review + delivery | Task 11 | Headless suites, default-build isolation with EUI sources moved, native smoke, real-agent/skips, dual Final reviews, delivery report |

### Acceptance checklist mapping

| Acceptance item | Plan gates |
| --- | --- |
| Linux `scripts/build.sh` produces runnable `codeg-eui` | Tasks 1, 7, 11 |
| Default data dir isolated from main app | Tasks 2, 11 smoke with ambient `CODEG_DATA_DIR`/`CODEG_HOME` |
| Grok and Codex create session + stream when installed | Tasks 5–7, 11 Step 7 (skip notation required when absent) |
| Settings P0 read/write/persist launch fields | Tasks 4, 7 |
| Default desktop/server/MCP builds independent of EUI | Tasks 1, 10, 11 Step 5 |
| README perf steps + one filled local table | Task 9 Steps 7–9; Task 11 Step 8 validates |

### Design-review minors (design R2) in the plan

| R2 residual | Plan resolution |
| --- | --- |
| `void` vs error-returning shutdown | Global constraint + ABI: `int codeg_eui_shutdown(void)` with double-shutdown errors |
| Ambient `CODEG_HOME` vs EUI root | Task 2: remove `CODEG_HOME` before core init; overwrite `CODEG_DATA_DIR` |
| Completion drain + retained frame bytes | Global constraint + Task 3: successful poll drains completions; bytes valid until next successful poll or completed shutdown |
| Fixed long-frame threshold | Global freeze + Task 9: `50 ms` for both shells |

### Non-goals respected

Windows/macOS first delivery, full tool cards, interactive Approve/Deny as
success criteria, second config schema, main-app DB sharing, Tauri/React
replacement, and default-build EUI coupling are all excluded from producer
success criteria and left to post-delivery residual decisions where relevant.

## `b2d_task_risk_v1` Matrix Audit

Policy applied independently:

- Hard triggers always `high`: `concurrency_lifecycle`, `security_trust_boundary`,
  `migration_destructive_persistence`, `public_compatibility`, `unsafe_ffi`,
  `update_rollback`.
- Soft weights: `cross_runtime_or_process=2`; each of
  `broad_production_surface`, `multiple_ownership_modules`, `shared_interface`,
  `dependency_or_build`, `multi_layer_without_test_seam` = 1.
- Soft total `≥3` → `high`; `0–2` → `normal` when no hard trigger.
- Routes: `normal` → implementer `grok`, reviewer `[codex]`;
  `high` → implementer `codex`, reviewers
  `[codex (separate, ≠ Author/implementer), grok]`.

### Recomputed matrix

| T | Hard (recomputed) | Soft (recomputed sum) | Claimed | Level | Route | Match |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `unsafe_ffi`, `public_compatibility` | 2+1+1+1 = 5 | 5 | high | codex + dual | OK |
| 2 | `security_trust_boundary`, `concurrency_lifecycle` | 1+1 = 2 | 2 | high (hard) | codex + dual | OK |
| 3 | `unsafe_ffi`, `concurrency_lifecycle` | 2+1+1 = 4 | 4 | high | codex + dual | OK |
| 4 | `security_trust_boundary`, `public_compatibility` | 1+1 = 2 | 2 | high (hard) | codex + dual | OK |
| 5 | `concurrency_lifecycle` | 2+1+1+1 = 5 | 5 | high | codex + dual | OK |
| 6 | `concurrency_lifecycle`, `security_trust_boundary` | 2+1+1 = 4 | 4 | high | codex + dual | OK |
| 7 | none | 2+1+1+1 = 5 | 5 | high (soft≥3) | codex + dual | OK |
| 8 | `concurrency_lifecycle` | 2+1+1 = 4 | 4 | high | codex + dual | OK |
| 9 | none | 2+1+1+1+1 = 6 | 6 | high (soft≥3) | codex + dual | OK |
| 10 | none | 1 | 1 | normal | grok + codex | OK |
| 11 | none | 1+1+1 = 3 | 3 | high (soft≥3) | codex + dual | OK |

### Matrix integrity checks

- **11 / 11** global rows; **11 / 11** local per-Task rows; levels and routes agree.
- Soft arithmetic matches claimed totals on every row.
- Every hard-trigger Task is `high` (no under-routing).
- Soft-only Tasks 7, 9, 11 correctly cross the ≥3 threshold to `high`.
- Task 10 is the only `normal` Task; soft=1 does not force high.
- No hard-trigger Task is assigned implementer `grok`.
- Policy version is `b2d_task_risk_v1` on every row.
- Hard-trigger evidence for FFI (Tasks 1, 3), data-root/credentials (2, 4),
  agent lifecycle (5, 8), live recovery/permissions (6) is substantive, not
  bare labels.
- Soft-only high Tasks (7 UI, 9 dual-shell instrumentation, 11 aggregate
  verification) have plausible multi-surface evidence.

**No risk/route corrections required.**

## Human UAT / Mid-Gate Audit

Searched the plan for human acceptance, UAT, sign-off, and user-approval
gating language.

| Location | Behavior | Status |
| --- | --- | --- |
| Global Constraints | Explicit ban on human UAT/sign-off between Tasks | Pass |
| Task 7 native screenshots / prepared-host smoke | Producer-run evidence, “not a human gate” | Pass |
| Task 9 filled comparison table | Producer-run on prepared host | Pass |
| Task 10 | Continues to Task 11 without human acceptance | Pass |
| Task 11 real-agent E2E / Final reviews | Automated + independent agent reviewers; no user gate | Pass |
| Post-Delivery Human Acceptance section | Explicitly post-delivery residual only | Pass |
| Task 13 Deliver | “Do not request mid-flow UAT” | Pass |

No mid-sequence human UAT or product sign-off gate exists.

## Task Quality for SDD Execution

### Structure

- Sequence is implement → automated verify → producer commit/review package →
  next Task → Task 10 pre-Final aggregation → Task 11 Final verify/review/deliver.
- Each producer Task uses checkbox steps, RED-first tests where behavior is
  added, GREEN verification commands, scoped `git add`/`commit`, and review-package
  handoff.
- Exact files/modules are listed per Task; File Structure table and Design
  Traceability table reinforce ownership and coverage.
- Input bounds, queue bounds, ABI version, EUI pin, markdown throttle, and
  long-frame threshold are frozen in Global Constraints (no planning placeholders).

### Executability spot-checks against the current tree

| Claim in plan | Codebase check |
| --- | --- |
| Package `codeg` with lib name `codeg_lib`, `default-features` includes Tauri | `src-tauri/Cargo.toml` matches; `default-features = false` path dep is correct |
| `AppState` field set in Task 2 | Matches current `AppState` members; helpers `default_*`, `build_delegation_stack`, `new_pet_state_handle` exist |
| `DocumentTranslationService::new_disabled` | New API planned; `new_inert` exists today — plan explicitly adds production-visible alias/replacement |
| Core ops: `open_folder_core`, ACP settings/preflight, `send_prompt_linked_with_message_id`, `to_snapshot`/`event_stream`, permission/plan cancel paths | Present in existing modules |
| WebView instrumentation hooks | Targets real files: `acp-connections-context.tsx`, `live-transcript-row.tsx` |

### Bite size

Tasks 1–9 are milestone-sized rather than tiny single-function units, but each
is internally step-decomposed with focused failing tests and verification
commands suitable for SDD producer admission. Task 10 is read-only aggregation.
Task 11 is appropriately the Final verification umbrella. No further split is
required for plan approval.

## Findings

### Critical

None.

### Important

None.

### Minor

None that block approval or require a Plan revision before admission.

## Residual Non-Blocking Observations

These are not findings and do not change the verdict:

1. Local Task 8/9 markdown separator rows have an extra column delimiter
   (cosmetic only; table body columns still parse).
2. Local Task titles are slightly abbreviated versus the global matrix; risk
   fields still match.
3. Tasks 2, 3, and 7 are large single commits by design; implementers should
   keep RED/GREEN steps sequential and avoid scope creep outside the listed
   files.

## Author Report Cross-Check

Author claims that were independently verified:

- Design digest and plan digest match disk.
- 11 Tasks; 10 high + 1 normal; Task 10 implementer `grok`; high implementers
  `codex` with dual reviewers.
- No mid-sequence human UAT.
- Design-review minors encoded in the plan.
- Planning placeholders absent (only intentional `tests/*.rs` / `tests/*.cpp`
  globs in the File Structure table).

## Verdict

```text
VERDICT: approve
critical: 0
important: 0
minor: 0
```

The plan is ready for Plan Gate settlement and subsequent workspace-gate /
Task admission under `b2d_task_risk_v1` routing.
