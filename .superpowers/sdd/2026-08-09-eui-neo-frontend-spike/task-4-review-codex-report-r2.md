# Task 4 Fix Re-review (Codex R2)

## Verdict

**`approve`**

I1 and N1 are addressed. The focused integration target now imports the
shutdown entry point it calls, and the same contract covers oversized settings
JSON, an unsupported public-agent request, and the real public probe ABI. No
new defect was found in the scoped two-file fix delta.

## Finding Disposition

| Finding | Disposition | Result |
| --- | --- | --- |
| I1: mandatory settings contract did not compile | **ADDRESSED** | `codeg_eui_shutdown` is imported; the complete integration-test source compiles with `rustc -D warnings` against a fresh ABI-shaped boundary. |
| N1: required boundary checks were absent | **ADDRESSED** | New isolated cases cover the frozen settings-size limit, unsupported-agent error completion, and public probe JSON completion. |

## Fix Analysis

`settings_contract.rs` now imports `codeg_eui_shutdown` from
`codeg_eui_core`, resolving the exact unqualified call in `complete_shutdown`
that caused the prior deterministic `E0425` failure. A fresh direct compile of
the complete committed test source discovered all six contract tests with no
warning or name-resolution failure.

The three added tests exercise the intended public boundaries:

- `oversized_patch_is_rejected_before_acceptance` submits
  `CODEG_EUI_MAX_SETTINGS_JSON_BYTES + 1`, expects
  `CODEG_EUI_ERR_TOO_LARGE`, and verifies the caller's request ID is unchanged.
- `unsupported_agent_completes_with_an_error` accepts `claude_code` through the
  async get ABI and verifies a terminal error with no result payload.
- `probe_result_arrives_through_the_public_abi` calls
  `codeg_eui_probe_agent` and validates the serialized probe DTO fields in the
  completion payload.

Each case retains the existing child-process isolation, fresh EUI data root,
and terminal shutdown helper. The fix changes no production Rust/C ABI,
persistence path, worker logic, or public layout.

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 4 high-risk Reviewer 1 (Codex), scoped fix re-review |
| Work unit | `task\|4\|reviewer\|codex\|none` |
| Reviewed task ID | `03e0633c-037b-47a2-85c4-af48570e824e` |
| Fix base | `89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf` |
| Producer artifact / commit | `29904a3a8fe6a741372809dfccb08f7a2e194e9f` |
| Scope | Prior I1/N1 plus breakage introduced by the fix delta |

The producer commit is `HEAD`, its sole parent is the stated fix base, and the
package accurately describes the two changed paths. The worktree was clean
before this review artifact was written. The approved design digest remains
`b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef`.

## Independent Verification

Passed locally:

- Fresh `rustc --test -D warnings` compilation of the complete committed
  `settings_contract.rs` against a minimal ABI-shaped dependency; **6 tests
  discovered, 0 benchmarks**
- Both Cargo formatting checks
- Standalone-crate `cargo metadata --no-deps`
- C11 and C++17 public-header syntax with
  `-Wall -Wextra -Wpedantic -Werror`
- Fresh contracts-only CMake configure/build and CTest: **3/3 passed**
- Fix parent/scope, `git diff --check`, approved-design digest, and clean
  baseline checks

Per the explicit parent instruction, no full package/workspace Cargo test,
full library test, or broad shared-`codeg` suite was run. Dependency-complete
execution of the native-file round trip remains limited by this host's memory;
this scoped approval does not claim that unavailable evidence.

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve","verdict":"approve","critical":0,"important":0,"minor":0,"summary":"Task 4 I1 and N1 are addressed: settings_contract imports the shutdown entry point and now covers oversized JSON, unsupported-agent completion, and the public probe ABI.","reviewed_task_id":"03e0633c-037b-47a2-85c4-af48570e824e","artifact_digest":"29904a3a8fe6a741372809dfccb08f7a2e194e9f","concerns":["Dependency-complete settings_contract and shared-codeg verification remain host-memory-limited and are not claimed by this scoped approval."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-review-codex-report-r2.md"}
-->
