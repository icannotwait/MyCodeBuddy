# EUI-NEO Frontend Spike Plan Re-review (Codex R3)

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Plan Reviewer (Codex), separate from the Plan Author |
| Work unit | `plan\|docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md\|reviewer\|codex\|none` |
| Revised Plan | `docs/superpowers/plans/2026-08-09-eui-neo-frontend-spike.md` |
| Revised digest (verified) | `sha256:76a829be1421178820652c8323e8758ffce715ef075b1f57609c0047c12f687f` |
| Revision commit (verified) | `ac1e38d52dc48d9038a33e964086f665d1b21148` (`docs: pin C++ test harness for EUI-NEO plan`) |
| Parent revision | `255f965c607fb7cb42bbdf70008b33f0144e49ec` |
| Prior review | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-review-codex-report-r2.md` |

The digest was recomputed with `sha256sum`. Commit `ac1e38d5` changes only the
Plan, and `git show --check ac1e38d5` exits zero.

## Residual Finding Disposition

### I7. Producer steps are not executable bite-sized RED/GREEN units

**Status: ADDRESSED**

Evidence:

- The Plan pins repository-owned harness version 1, defines its registry,
  shared `main`, and all used assertion macros without an external test
  dependency (Plan lines 298-425).
- `codeg_eui_add_contract_test` derives `${name}_test`, links the shared
  harness main, and registers the exact supplied CTest name (lines 477-484).
  The registry enumerates all 11 source/executable/CTest mappings (lines
  132-150), including the conditional ABI-link shutdown test (lines 500-508).
- `assert_ctest_registered.sh` requires an anchored selection count of exactly
  one, while `assert_ctest_red.sh` also requires failed CTest status, the named
  `[FAIL] Suite.Case`, and one failed test out of one (lines 380-415).
- Task 3 builds and validates the shutdown-drain and deep-copy targets before
  accepting their named behavioral failures, then provides exact GREEN runs
  (lines 1052-1145 and 1147-1193).
- Tasks 7-9 register each new source no later than its RED edit, build the
  derived target, validate the exact named failing case, and follow with an
  anchored GREEN run. This includes the reused settings and performance
  executables when later behavior is added (lines 1771-2113, 2245-2338, and
  2405-2523).
- Task-level regression gates assert the registrations present at that stage
  (lines 1204-1207, 2120-2124, and 2648-2653). Final verification enables the
  ABI-link route, asserts all 11 exact registrations, and runs the complete
  headless CTest suite (lines 2832-2847).

The prior empty-selection and undefined-harness failure mode is closed. The
planned C++ RED evidence now requires a compiled target, exactly one selected
CTest registration, and the intended named harness failure; GREEN evidence
uses the same exact registration.

## New Critical/Important Findings

None.

## Verdict

```text
VERDICT: approve
critical: 0
important: 0
minor: 0
```

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"Residual I7 is addressed by the pinned custom harness, complete 11-test CMake registry, exact-selection guards, and assertion-specific RED/GREEN routes; no new Critical or Important findings.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/plan/plan-review-codex-report-r3.md"}
-->
