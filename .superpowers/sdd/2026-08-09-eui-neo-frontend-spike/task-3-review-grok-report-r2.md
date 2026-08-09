# Task 3 Independent Re-Review (Grok) r2 — High-Gate I1 Fix

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 3 high-risk Reviewer 2 (Grok) |
| Work unit | `task\|3\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| reviewed_task_id (latest) | `e83e1833-71b2-412f-a158-bea9a83bd423` |
| Prior reviewed_task_id | `e53d2f15-9667-4dc8-94d0-ff366f390e36` |
| Commit (HEAD) | `66f7cff1ee5b02773f19f938482c3a112792ecb0` |
| FIX_BASE | `b55f20ddb97706ebd78126e5ffd5ef4cb249ab57` |
| Fix package | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-fix-review-package.md` |
| Prior Grok review | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-review-grok-report.md` (`approve_with_minors`) |
| Codex I1 context | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-review-codex-report.md` (`request_changes` on `b55f20dd`) |
| Producer report (fix round) | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-report.md` § High-Gate Fix Round 1/5 |
| Global constraints | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/global-constraints.md` |
| Risk | `high` (`unsafe_ffi` + `concurrency_lifecycle`; soft total 4); policy `b2d_task_risk_v1` |
| Parent rule | **SKIP all full cargo test** — package/source/CTest/ABI evidence only |

This re-review is independent of the implementer and of any Codex reviewer
thread. Scope is the fix delta plus confirmation that prior Task 3 ABI /
lifecycle / completion / shutdown behavior is not regressed.

## Overall Assessment

The high-gate fix is a minimal, correctly scoped remediation of Codex I1:

- The synthetic blocked-request stimulus is present only under
  `ffi-test-hooks` (Rust helper, `CommandPayload::Blocked`, forever-pending
  executor arm, and existing C export).
- `shutdown_contract.rs` is feature-gated so the normal-feature test surface
  cannot pull the hook.
- A normal-feature rlib API probe script
  (`codeg-eui/tests/assert_rust_hook_absent.sh`) encodes the required
  “cannot name the hook” check.

That matches the brief’s opt-in test-hook boundary. Core ABI lifecycle,
bounded admission, immutable frames, and two-phase shutdown are unchanged by
the delta and still green under independent CTest re-run.

**Verdict: `approve_with_minors`**

I1 is resolved. Prior non-blocking residuals remain (host Cargo evidence;
C++ `UiSnapshot` pure-copy scope; join-handle style). No Critical or Important
defect on the fixed artifact.

## I1 Fix Audit

### Defect (Codex I1, accepted as valid)

On `b55f20dd`, `ffi-test-hooks` correctly gated the C export
`codeg_eui_test_enqueue_blocked`, and the normal static archive exported no
`codeg_eui_test_*` symbols. However:

| Surface | Pre-fix (`b55f20dd`) |
| --- | --- |
| `enqueue_blocked_for_test` | Unconditional `pub fn` in `abi.rs`, re-exported via `pub use abi::*` |
| `CommandPayload::Blocked` | Always compiled |
| `execute_command` forever-pending arm | Always compiled (`pending().await`) |
| Normal rlib | Type-checked public reference to the helper succeeded (Codex evidence) |

The brief requires the test hook to be absent from normal builds, not only
from the C archive. Prior Grok r1 under-weighted this as a non-finding note
(“Rust helper always available for shutdown_contract”). Codex correctly
raised it to Important. This r2 treats the fix as required high-gate work.

### Remediation (`66f7cff1`)

| Change | Result |
| --- | --- |
| `#[cfg(feature = "ffi-test-hooks")]` on `enqueue_blocked_for_test` | Absent from normal rlib / API surface |
| Same cfg on `CommandPayload::Blocked` | No forever-pending payload variant without feature |
| Same cfg on `pending` import + executor arm | No hang-stimulus path without feature |
| C export remains feature-gated | Unchanged correct boundary |
| `#![cfg(feature = "ffi-test-hooks")]` on `shutdown_contract.rs` | Contract runs only with opt-in feature |
| `assert_rust_hook_absent.sh` | Compiles a probe against a normal rlib; requires failure mentioning `enqueue_blocked_for_test` |
| `default = []` / `ffi-test-hooks = []` | Unchanged; feature still opt-in |

Production call graph for real operations is untouched: only the synthetic
blocked stimulus and its test harnesses move behind the feature.

### Does the fix weaken shutdown drain evidence?

No. Black-box C++ `codeg_eui_shutdown_drain` still builds with
`CODEG_EUI_TEST_HOOKS` + `ffi-test-hooks` archive and exercises
`codeg_eui_test_enqueue_blocked` through the ordinary public lifecycle/poll
ABI. Rust `shutdown_contract` remains available when the feature is enabled.
Normal product/staticlib builds cannot name either the C or Rust hook.

## Prior Approve Regression Check

| Prior Grok r1 surface | After `66f7cff1` |
| --- | --- |
| Errors `0..9`, 160-byte frame, offsets | Unchanged (no ABI layout edits) |
| UI affinity, panic boundary, lifecycle order | Unchanged |
| 256 admission + completion ledger | Unchanged |
| Immutable `OwnedFrame` + completion drain | Unchanged |
| Two-phase shutdown / `shutdown_ready` latch | Unchanged |
| Path NUL vs message UTF-8 policy | Unchanged |
| C++ `UiSnapshot` deep-copy | Unchanged |
| Product isolation / design pin / gitlink | Unchanged (fix commit does not touch them) |

Independent host re-run: `ctest --test-dir codeg-eui/build-contract` →
**4/4 passed** (harness, abi_layout, ui_snapshot, shutdown_drain). Script
`assert_rust_hook_absent.sh` has valid `sh -n` syntax.

Stale pre-fix rlibs under `/tmp` still contain
`enqueue_blocked_for_test` mangled symbols; that is expected historical
artifact noise and is not the fixed tree. Source at `HEAD` has no unguarded
`Blocked` / hook path.

## Independent Verification (this host)

| Check | Result |
| --- | --- |
| `HEAD` / package ancestry | `66f7cff1` parent `b55f20dd`; 6-path fix package matches |
| Source cfg gate completeness | **Pass** (helper, payload, executor, C export, shutdown test) |
| Features still opt-in | **Pass** (`default = []`) |
| C header still `#if CODEG_EUI_TEST_HOOKS` | **Pass** |
| `assert_rust_hook_absent.sh` shell syntax | **Pass** |
| Contracts CTest `build-contract` | **4/4 Pass** (no regression) |
| Full Cargo tests | **Skipped** (parent rule) |

Fresh producer-side normal-rlib probe evidence is accepted via the committed
assert script + gated sources; this host did not rebuild a post-fix rlib
(shared-core link cost / memory class). That does not reopen I1: without the
feature flag the items are not compiled into the crate.

## Findings

### Critical

None.

### Important

None. **Codex I1 is addressed.**

### Minor

Residuals carried from Grok r1 (unchanged by this fix):

1. **Residual dependency-complete Cargo verification on ≤4 GiB hosts**  
   Parent still forbids full Cargo suite. Re-run focused bridge/shutdown/smoke
   (with and without `ffi-test-hooks`) on a higher-memory host when available.
   **No source change required.**

2. **C++ `UiSnapshot` remains pure deep-copy**  
   Live ABI poll lifetime still covered primarily in Rust contracts. Optional
   ABI-linked C++ ownership test remains a non-blocking follow-up.

3. **`RuntimeOwner::join` still drops the worker `JoinHandle` without await**  
   Still gated by the `quiesced` latch before final free. Style/clarity only.

## Non-Findings / Notes

- Gating `CommandPayload::Blocked` together with the public helper is the right
  depth: a feature-gated C export alone would still leave a hang-capable
  payload constructible from Rust if the variant remained public to the module.
- `#[cfg(test)]` Error/Panic payloads remain for unit tests only; they are not
  a public ABI stimulus and were not part of I1.
- Fix commit stages only owned sources plus forced report update; no build
  artifacts.

## Verdict Card

```text
VERDICT: approve_with_minors
critical: 0
important: 0
minor: 3
reviewed_commit: 66f7cff1ee5b02773f19f938482c3a112792ecb0
reviewed_task_id: e83e1833-71b2-412f-a158-bea9a83bd423
prior_commit: b55f20ddb97706ebd78126e5ffd5ef4cb249ab57
i1_status: fixed
continue_sequence: yes
code_changes_required: no
residual: re-run focused Cargo bridge/shutdown/smoke (±ffi-test-hooks) on higher-memory host; optional ABI-linked C++ frame lifetime test; optional explicit worker JoinHandle wait
```

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":3,"summary":"I1 fixed: blocked shutdown stimulus gated under ffi-test-hooks (Rust helper, payload, executor, C export, shutdown_contract). Prior approve not regressed; 4/4 CTest pass. Residuals: host Cargo; UiSnapshot pure-copy; join drops JoinHandle.","reviewed_task_id":"e83e1833-71b2-412f-a158-bea9a83bd423","artifact_digest":"66f7cff1ee5b02773f19f938482c3a112792ecb0","concerns":["Full Cargo bridge/shutdown/smoke not re-run on this 4GiB host (parent SKIP).","C++ UiSnapshot still pure deep-copy.","RuntimeOwner::join still drops JoinHandle without await."],"report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-3-review-grok-report-r2.md"}
-->
