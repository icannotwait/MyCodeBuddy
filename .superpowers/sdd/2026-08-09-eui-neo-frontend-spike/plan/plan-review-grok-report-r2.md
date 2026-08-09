# EUI-NEO Frontend Spike Plan Re-review (Grok R2)

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Plan Reviewer (Grok), full-cohort re-review |
| Work unit | `plan\|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| Design | `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md` |
| Design digest (verified) | `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| Plan | `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md` |
| Plan digest (verified) | `sha256:06ad8b84d175fde69662e2b29a6a7f975554f69f962fbda165ee2c6b37e50767` |
| Plan revision commit | `255f965c607fb7cb42bbdf70008b33f0144e49ec` (`docs: revise EUI-NEO spike plan after review`) |
| Prior Grok review | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-review-grok-report.md` (`approve`) |
| Codex prior review | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-review-codex-report.md` (`request_changes`) |
| Policy | `b2d_task_risk_v1` |

`sha256sum` on the Plan file matches the stated R2 digest. Design digest is unchanged
and still referenced in the Plan global baseline.

## Overall Assessment

The revision addresses every Codex Important/Minor finding without weakening the
prior Grok approve. Design coverage M0–M6 remains complete and is strengthened
(dual-agent E2E is now a hard Final blocker; shutdown is externally observable;
snapshot-pending interactions are declined). The `b2d_task_risk_v1` matrix is
unchanged in levels/routes and still arithmetically correct. No human UAT or
sign-off mid-gates were introduced.

**Verdict: `approve`**

No Critical, Important, or Minor findings.

## Regression Checks vs Prior Grok Approve

| Check | R1 status | R2 status |
| --- | --- | --- |
| Design M0–M6 + acceptance mapping | Covered | Covered; dual-agent product loop tightened |
| Optional-build / default independence | Covered | Covered; broader Final Rust gates with EUI held aside |
| Bridge contracts (async, frames, panic, affinity) | Covered | Covered; two-phase shutdown adds observability |
| Risk matrix hard/soft → level → route | Correct | Correct; 10 high / 1 normal unchanged |
| No human mid-sequence UAT | Pass | Pass |
| High FFI/concurrency dual-reviewed | Pass | Pass |
| Task 10 normal / implementer `grok` | Pass | Pass |

**Prior approve still holds.** The revision is additive quality, not a scope
or routing regression.

## Codex Finding Disposition (Independent Check)

| ID | Codex concern | Plan evidence in `255f965c` | Status |
| --- | --- | --- | --- |
| I1 | Shutdown vs exactly-once public completion | Global two-phase shutdown: `begin_shutdown` → `stopping` polls legal → frame with `shutdown_ready=1` exposes remaining completions → final `shutdown` join/free; blocked-request Rust + black-box C++ tests (Task 3) | Addressed |
| I2 | WebView `t_first_presented` pre-paint | Common post-presentation proxies: EUI next-update `onFrame`; WebView second RAF after RAF1 paint opportunity; phase-order tests (Task 9) | Addressed |
| I3 | Generated trees staged; Final report unstageable | Explicit `codeg-eui/.gitignore` and `codeg-eui-core/.gitignore`; exact-path staging + dry-run; forbidden recursive parent `git add`; `git add -f` for `.superpowers` delivery report; results as ignored local evidence | Addressed |
| I4 | Narrow Final Rust regression | Task 11 broad `cargo test --lib --features test-utils`, server tests/clippy, MCP check/clippy while EUI sources held aside and restored | Addressed |
| I5 | Skip weakens dual-agent E2E goal | Global + Task 11: both Grok and Codex mandatory successful presented stream; missing/failing agent **blocks** Final; performance-only skips do not satisfy product loop | Addressed |
| I6 | Snapshot pending interactions not declined | Attach/resync call shared deduplicated decline on `pending_permission` / `pending_question` / `pending_plan_approval` before resume; snapshot-only RED tests | Addressed |
| I7 | Non bite-sized RED-GREEN | Tasks 3/6/7/8/9 expanded to many test-first steps with concrete skeletons (19/18/22/18/20 steps) | Addressed |
| M1 | Init `data_dir` vs env disagreement | Non-empty ABI argument authoritative; empty → `CODEG_EUI_DATA_DIR` → XDG/home; differing arg/env RED case | Addressed |
| M2 | `perf_compare.sh` subcommand mismatch | Explicit `record-eui`, `record-webview`, `aggregate`, `validate`, `self-test` interfaces + self-test coverage | Addressed |

## Design Coverage (R2)

| Design area | Producer Task | Notes |
| --- | --- | --- |
| M0 optional build spine | Task 1 | Pin, staticlib, CMake hello, ignores, exact staging |
| M1 isolated root / WebOnly | Task 2 | Authoritative init arg, `CODEG_HOME` clear, profile tests |
| Bridge lifecycle / frames | Task 3 | Two-phase shutdown, exactly-once observable completions |
| M2 settings/probe | Task 4 | Narrow facade, async completions |
| M3 workspace/session/send | Task 5 | Existing cores, epochs, linked send |
| M4 live + decline | Task 6 | Snapshot+subscribe, resync, snapshot-pending decline |
| Shell/chat/P0 settings UI | Task 7 | Frame copy, pages, throttle, smoke hook |
| M5 recovery/cancel/P1 | Task 8 | Epoch fence, crash retain, structured controls |
| M6 perf comparison | Task 9 | Shared anchors, 50 ms, shell RSS, README table |
| Pre-Final audit | Task 10 | Allowlist, pin, traceability (incl. dual-agent E2E item) |
| Final verify/review/deliver | Task 11 | Broad default regressions, mandatory Grok+Codex E2E |

Acceptance checklist items remain mapped. Non-goals (no default-shell
replacement, no mid-flow human UAT, no second config schema) remain respected.

## `b2d_task_risk_v1` Matrix Re-audit

Independent recomputation of global rows:

| T | Hard | Soft sum | Level | Implementer | Reviewers | Match |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `unsafe_ffi`, `public_compatibility` | 5 | high | codex | codex (separate)+grok | OK |
| 2 | `security_trust_boundary`, `concurrency_lifecycle` | 2 | high | codex | dual | OK |
| 3 | `unsafe_ffi`, `concurrency_lifecycle` | 4 | high | codex | dual | OK |
| 4 | `security_trust_boundary`, `public_compatibility` | 2 | high | codex | dual | OK |
| 5 | `concurrency_lifecycle` | 5 | high | codex | dual | OK |
| 6 | `concurrency_lifecycle`, `security_trust_boundary` | 4 | high | codex | dual | OK |
| 7 | none | 5 | high | codex | dual | OK |
| 8 | `concurrency_lifecycle` | 4 | high | codex | dual | OK |
| 9 | none | 6 | high | codex | dual | OK |
| 10 | none | 1 | normal | grok | codex | OK |
| 11 | none | 3 | high | codex | dual | OK |

- Soft arithmetic equals claimed totals on all 11 rows.
- Hard trigger → always high; soft ≥3 → high; soft 0–2 without hard → normal only for Task 10.
- Global and local matrix rows agree on hard sets, soft totals, levels, and routes.
- Policy version `b2d_task_risk_v1` on every row.
- No under-routing of FFI/concurrency/security Tasks; no Grok implementer on high Tasks.

**No risk/route correction required.**

## Human UAT / Mid-Gate Audit (R2)

| Location | Behavior | Status |
| --- | --- | --- |
| Global constraint line | Ban on human UAT/sign-off between Tasks | Pass |
| Task 10 | Continue to Task 11 without human acceptance | Pass |
| Task 11 deliver | Explicit no mid-flow UAT | Pass |
| Post-Delivery section | Human acceptance residual only | Pass |
| Real-agent / native smoke | Producer-run automated evidence gates | Pass |

No regression: still no mid-sequence human gate. Note that dual-agent E2E is a
**producer-host capability blocker**, not a human approval gate.

## Task Quality Notes

- Sequence remains implement → automated verify → commit/review package → …
  → pre-Final aggregation → Final.
- Producer Tasks expanded step counts for RED/GREEN fidelity without changing
  risk routing or inserting gates.
- Generated-output ownership and Final report force-add remove the prior
  commit-path defects.
- Placeholder scan: no unresolved planning TODOs/TBDs.

## Findings

### Critical

None.

### Important

None.

### Minor

None.

## Verdict

```text
VERDICT: approve
critical: 0
important: 0
minor: 0
```

The revised Plan is ready for Plan Gate settlement and Task admission under the
existing `b2d_task_risk_v1` routes.
