# EUI-NEO Frontend Spike Plan Re-review (Grok R3)

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Plan Reviewer (Grok), full-cohort re-review r3 |
| Work unit | `plan\|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| Design | `docs/superpowers/specs/2026-08-09-eui-neo-frontend-spike-design.md` |
| Design digest (unchanged baseline in Plan) | `sha256:b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| Plan | `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md` |
| Plan digest (verified) | `sha256:76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f` |
| Plan revision commit | `ac1e38d52dc48d9038a33e964086f665d1b21148` (`docs: pin C++ test harness for EUI-NEO plan`) |
| Prior Grok reviews | `plan-review-grok-report.md` (approve), `plan-review-grok-report-r2.md` (approve) |
| Policy | `b2d_task_risk_v1` |

`sha256sum` on the Plan file matches the stated r3 digest. Diff scope is the
Plan file only.

## Overall Assessment

This revision pins a repository-owned headless C++ test harness and exact CTest
registration protocol to close residual I7 executability gaps. It does **not**
change design scope, risk levels/routes, dual-agent Final acceptance, two-phase
shutdown, or the ban on human mid-sequence UAT.

**Verdict: `approve`**

No Critical, Important, or Minor findings.

## Regression Checks (R2 → R3)

| Check | R2 | R3 |
| --- | --- | --- |
| Design M0–M6 + acceptance mapping | Covered | Covered; unchanged milestones |
| Dual-agent mandatory product-loop Final | Mandatory blocker | Still mandatory (global + Task 11) |
| Two-phase shutdown / `shutdown_ready` | Present | Present |
| Post-presentation markers (EUI onFrame / WebView 2nd RAF) | Present | Present |
| Snapshot-pending interaction decline | Present | Present |
| Generated-output ignore / exact staging / `git add -f` report | Present | Present |
| `b2d_task_risk_v1` matrix (10 high / 1 normal) | Correct | Correct; soft arithmetic unchanged |
| No human mid-sequence UAT | Pass | Pass |
| High FFI/concurrency dual-reviewed; Task 10 `grok` | Pass | Pass |

**No regression** relative to prior Grok approve rounds.

## C++ Harness / CTest Change Soundness

### What was added

- Global constraint: harness version pin `CODEG_EUI_TEST_HARNESS_VERSION=1`;
  no GoogleTest/Catch2 fetch; contracts-only mode must not require EUI/GLFW/OpenGL.
- Files: `tests/test_harness.h`, `tests/test_main.cpp`, `harness_self_test.cpp`,
  `assert_ctest_registered.sh`, `assert_ctest_red.sh`.
- CMake helper `codeg_eui_add_contract_test(exact_name, source)`:
  - executable `${exact_name}_test` from source + shared `test_main.cpp`
  - CTest name exactly `${exact_name}`
  - includes `tests`, `app`, `app/bridge` only for headless contracts
- Complete planned registry table (11 exact names) across Tasks 1, 3, 7, 8, 9.
- RED protocol: register no later than RED step → build named target →
  `assert_ctest_registered` (`ctest -N -R '^name$'` count == 1) → prove named
  harness `[FAIL] Suite.Name` (compile failure / zero selection is invalid RED).

### Why this is sound

1. **Dependency boundary:** Custom C++17 harness matches the spike’s optional
   build posture; contracts-only mode stays free of native UI deps and external
   test frameworks.
2. **Deterministic selection:** Anchored CTest regex + registration assert
   prevents “No tests were found” false RED.
3. **True RED semantics:** `assert_ctest_red.sh` requires non-zero ctest status,
   exact `[FAIL] <case>`, and single-test failure summary—assertion failure, not
   link/configure accidents.
4. **Stable naming contract:** Registry table freezes executable/CTest names so
   Task 7–9 page splits can each own one registered suite without inventing
   harness mechanics mid-implementation.
5. **Version pin:** Changing harness semantics requires explicit plan/contract
   review—appropriate for a shared test surface.
6. **Risk impact:** Pure tooling for headless contract tests; no new hard
   triggers and no matrix row changes required.

No soundness defect found in the planned CMake helper, harness macros, or
assert scripts.

## Risk Matrix Spot Check (R3)

All 11 global rows rechecked: soft totals match signal weights; hard → high;
soft ≥3 → high; Task 10 soft=1 normal with implementer `grok` / reviewer
`codex`; Tasks 1–9 and 11 high with dual reviewers. Policy version remains
`b2d_task_risk_v1`.

## Human Mid-Gate Spot Check (R3)

- Global ban on human UAT/sign-off between Tasks: present.
- Task 10 continues without human acceptance: present.
- Post-delivery residual human acceptance section: present only after delivery.
- No new wait-for-user gate introduced by harness work.

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

Plan digest `76a829be…` is ready for Plan Gate settlement and Task admission
under existing `b2d_task_risk_v1` routes.
