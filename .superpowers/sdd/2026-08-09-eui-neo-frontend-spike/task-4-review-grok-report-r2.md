# Task 4 Independent Re-Review (Grok) r2 — High-Gate I1 Fix

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 4 high-risk Reviewer 2 (Grok) |
| Work unit | `task\|4\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| reviewed_task_id (latest) | `03e0633c-037b-47a2-85c4-af48570e824e` |
| Prior reviewed_task_id | `48d79f89-e4ef-4240-8092-f98bc9306cf2` |
| Commit (HEAD) | `29904a3a8fe6a741372809dfccb08f7a2e194e9f` |
| FIX_BASE | `89c0889f6faf8d3ad482c9e4e1a6a34df65d8cbf` |
| Fix package | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-fix-review-package.md` |
| Prior Grok review | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-review-grok-report.md` (`request_changes` on `89c0889f`) |
| Producer report (fix round) | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-report.md` § High-Gate Fix Round 1/5 |
| Global constraints | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/global-constraints.md` |
| Risk | `high` (`security_trust_boundary` + `public_compatibility`; soft total 2); policy `b2d_task_risk_v1` |
| Parent rule | **SKIP all full cargo test** — package/source/CTest/ABI evidence only |

This re-review is independent of the implementer and of any Codex reviewer
thread. Scope is the fix delta plus confirmation that the prior Task 4 facade /
async worker / pre-accept surfaces are not regressed.

## Overall Assessment

The high-gate fix is minimal and correctly scoped:

- `settings_contract.rs` now imports `codeg_eui_shutdown`, so every case that
  reaches `complete_shutdown` can resolve the final free call.
- The same contract target gains pre-accept oversize rejection, public
  unsupported-agent error completion, and a public probe ABI completion case —
  covering cheap Grok r1 minors without touching production modules.
- Diff against FIX_BASE is only the report section and
  `settings_contract.rs`. Production facade, ABI set-path, bootstrap `Arc`,
  and runtime `CoreOps` paths from `89c0889f` are unchanged.

I1 from Grok r1 is resolved. No Critical or Important defect remains on this
artifact under authorized (non–full-Cargo) evidence.

**Verdict: `approve_with_minors`**

## Finding Dispositions

| Finding (Grok r1) | Disposition | Result |
| --- | --- | --- |
| I1: missing `codeg_eui_shutdown` import in `settings_contract` | **Fixed** | Import present; all shutdown paths can resolve the symbol |
| M1: residual dependency-complete Cargo on ≤4 GiB hosts | **Unchanged residual** | Parent SKIP + historical SIGKILL; not re-attempted |
| M2: unsupported agent “before FS” only unit-guard | **Partially improved** | Public get ABI now accepts `claude_code` then one error completion with empty payload; still no spy proving zero ACP/DB/fs |
| M3: nested structured patches lack `deny_unknown_fields` | **Unchanged residual** | Optional; outer deny still holds |
| M4: no ABI-level probe contract | **Addressed** | `probe_result_arrives_through_the_public_abi` added |

## I1 Fix Audit

### Defect (Grok r1 I1)

On `89c0889f`, `complete_shutdown` called `codeg_eui_shutdown()` while the
`use codeg_eui_core::{…}` list omitted it. Under Rust 2018+ hygiene that is
`E0425` for every isolated case that drains lifecycle — including malformed
reject, get completion, and native round-trip. The primary Task 4 integration
artifact could not compile even on a higher-memory host.

### Remediation (`29904a3a`)

```rust
use codeg_eui_core::{
    codeg_eui_begin_shutdown, codeg_eui_get_agent_settings, codeg_eui_init, codeg_eui_poll,
    codeg_eui_probe_agent, codeg_eui_set_agent_settings, codeg_eui_shutdown, CodegEuiCompletion,
    CodegEuiFrame, CodegEuiSlice, CODEG_EUI_ERR_INVALID_STATE, CODEG_EUI_ERR_TOO_LARGE,
    CODEG_EUI_MAX_SETTINGS_JSON_BYTES, CODEG_EUI_OK,
};
```

`codeg_eui_shutdown` is now imported alongside the existing begin/poll path.
No production code change was required for I1.

### Additional contract coverage in the same fix

| New case | Asserts |
| --- | --- |
| `oversized_patch_is_rejected_before_acceptance` | `CODEG_EUI_MAX_SETTINGS_JSON_BYTES + 1` → `TOO_LARGE`; `request_id` unchanged |
| `unsupported_agent_completes_with_an_error` | get of `claude_code` accepts, then status `1` (Error), empty result, error text contains `unsupported EUI agent` |
| `probe_result_arrives_through_the_public_abi` | `codeg_eui_probe_agent("codex")` → OK completion with `launchable` / `message` / optional `installedVersion` |

These strengthen the public ABI story without widening the settings DTO or
persistence boundary.

## Prior Task 4 Surface Regression Check

| Prior Grok r1 production surface (`89c0889f`) | After `29904a3a` |
| --- | --- |
| Narrow `EuiAgentSettings` / patch / probe DTOs | Unchanged (no `eui_facade.rs` edit) |
| Wire `"codex"` / `"grok"` only; typed guard | Unchanged |
| Cross-agent field exclusivity on set | Unchanged |
| ACP-only writes; no facade FS | Unchanged |
| Pre-accept outer `deny_unknown_fields` on set JSON | Unchanged |
| `CoreOps` + async worker completion ledger | Unchanged |
| Bootstrap `Arc<AppState>` share | Unchanged |
| C header async settings comment | Unchanged |

Delta file list matches the package: report + `settings_contract.rs` only.

## Spec / High-Risk Re-Check (delta-relevant)

- **Trust boundary:** oversize and malformed still reject before acceptance;
  unsupported wire agents still fail closed on the worker before settings
  projection (error completion, empty payload). No new write path.
- **Public compatibility:** probe/get/set ABI signatures unchanged; new tests
  only consume existing exports.
- **Exactly-once / async:** unsupported and probe cases still go through
  accept → poll completion; request IDs on pre-accept failures remain
  unchanged at the caller-supplied sentinel.

## Independent Verification (this host)

Host: Linux, `MemTotal` ≈ 3.8 GiB, no swap.
`HEAD == 29904a3a…`; FIX_BASE `89c0889f` is an ancestor.

| Check | Result |
| --- | --- |
| Commit message / ancestry vs package | Match (`fix(eui): compile settings bridge contract`; FIX_BASE `89c0889f`) |
| Diff scope | Report + `settings_contract.rs` only |
| I1 import present on HEAD | **Pass** (`codeg_eui_shutdown` in use list and `complete_shutdown`) |
| Design SHA-256 | `b3446ec31cc8b0457ed1ca3e7c6e8b3ec421eb4b997af6efae3d8975ecd2bdef` |
| EUI-NEO gitlink | `cb70ea8bea263efa7805a40c07135df028ad44b1` |
| C11 header syntax `-Wall -Wextra -Wpedantic -Werror` (`-c`) | **Pass** |
| Contracts-only CTest (`build-contract-task4`) | **3/3 Pass** |
| `git diff --check` FIX_BASE..HEAD | **Pass** |
| Full / package Cargo / dependency-complete `settings_contract` | **Skipped** (parent rule); not re-attempted |

Producer-claimed shape-compatible contract case runs and focused `rustc --test`
GREEN for the import fix were not re-executed here (shared-`codeg` link cost on
this host). Static import resolution of I1 does not require that link.

## Findings

### Critical

None.

### Important

None. I1 is fixed.

### Minor

1. **Residual dependency-complete Cargo verification on ≤4 GiB hosts**  
   Parent SKIP remains. Native-file round-trip
   (`codex_and_grok_settings_round_trip_through_native_files`) and full
   `settings_contract` execution still need a higher-memory host. Source is
   now import-complete; runtime green is still residual evidence debt.

2. **Unsupported-agent “zero ACP/DB/fs” still not spied**  
   The new public ABI case proves accept → single error completion with no
   settings payload for `claude_code`. Placement of `parse_supported_agent` /
   `ensure_supported` before `acp_*` is unchanged and correct in source. A
   mock-based zero-touch proof remains optional.

3. **Nested structured patch objects still lack `deny_unknown_fields`**  
   Outer patch still rejects unknown keys. Nested Grok/Codex structured types
   remain permissive (existing ACP types). Optional.

## Non-Findings / Notes

- Fix round correctly does **not** claim dependency-complete native round-trip
  green on this host.
- `completion.status == 1` for unsupported agent matches
  `CompletionStatus::Error` (`CODEG_EUI_COMPLETION_ERROR`).
- Oversized body of spaces hits the length bound before JSON parse — correct
  `TOO_LARGE` path.
- Generated Cargo/CMake outputs remain unstaged.

## Verdict Card

```text
VERDICT: approve_with_minors
critical: 0
important: 0
minor: 3
reviewed_commit: 29904a3a8fe6a741372809dfccb08f7a2e194e9f
reviewed_task_id: 03e0633c-037b-47a2-85c4-af48570e824e
prior_reviewed_task_id: 48d79f89-e4ef-4240-8092-f98bc9306cf2
prior_verdict: request_changes (89c0889f)
continue_sequence: yes
code_changes_required: no
residual: re-run settings_contract (incl. native round-trip) on higher-memory host; optional zero-touch spy for unsupported agents; optional nested deny_unknown_fields
```

<!-- codeg-card-summary-v1
{"kind":"review","phase":"review","status":"approve_with_minors","verdict":"approve_with_minors","critical":0,"important":0,"minor":3,"summary":"I1 fixed: settings_contract imports codeg_eui_shutdown; oversize/unsupported/probe ABI cases added. Production facade/CoreOps unchanged. Residuals: host Cargo/native round-trip; optional zero-touch spy; nested deny_unknown.","reviewed_task_id":"03e0633c-037b-47a2-85c4-af48570e824e","artifact_digest":"29904a3a8fe6a741372809dfccb08f7a2e194e9f","concerns":["Dependency-complete settings_contract/native round-trip not re-run on this 3.8GiB host (parent SKIP).","Optional: mock-based zero ACP/DB/fs proof for unsupported agents.","Optional: nested deny_unknown_fields on structured patch objects."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-4-review-grok-report-r2.md"}
-->
