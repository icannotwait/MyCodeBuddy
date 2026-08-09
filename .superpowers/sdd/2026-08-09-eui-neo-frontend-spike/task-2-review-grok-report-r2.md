# Task 2 Independent Re-Review (Grok) r2 — High-Gate I1 Fix

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 2 high-risk Reviewer 2 (Grok) |
| Work unit | `task\|2\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| reviewed_task_id (latest) | `315c9c36-091c-4146-95de-0f071d43b7cf` |
| Prior reviewed_task_id | `eb250a5f-e61e-441f-af46-f5130a615ed8` |
| Commit (HEAD) | `1e92ed75da0702bc628b5f42e0af7fe5d48c7814` |
| FIX_BASE | `8bac8d78bcdf7f189304fa714d068e2d73ddb541` |
| Fix package | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-fix-review-package.md` |
| Prior Grok review | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-grok-report.md` (`approve_with_minors`) |
| Producer report (fix round) | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-report.md` § High-Gate Fix Round 1/5 |
| Global constraints | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/global-constraints.md` |
| Risk | `high` (`security_trust_boundary` + `concurrency_lifecycle`); policy `b2d_task_risk_v1` |

This re-review is independent of the implementer and of any Codex reviewer
thread. Scope is the fix delta plus confirmation that prior approved Task 2
behavior is not regressed.

## Overall Assessment

The high-gate fix is a single-file, focused correction to the process pin:

- First successful pin still clears `CODEG_HOME` and overwrites `CODEG_DATA_DIR`
  before logging/Tokio/DB.
- Equal already-pinned same-root calls return success **without** repeating
  process-environment mutation.
- Divergent roots still return `DataRootError::AlreadyPinned` with no env write.
- A regression seam counts env-write phases across full ABI
  init → two-phase shutdown → same-root re-init.

That is exactly the Codex I1 remediation. It does **not** regress the isolation,
precedence, ABI ownership, dormant `WebOnly` profile, or joined shutdown
surfaces that grounded the prior Grok `approve_with_minors`.

**Verdict: `approve_with_minors`**

I1 is resolved. Prior non-blocking residuals remain (host Cargo evidence;
synthetic `StartedServices` dormancy flags). No Critical or Important defect on
the fixed artifact.

## I1 Fix Audit

### Defect (Codex I1, accepted as valid)

On `8bac8d78`, every successful `pin_eui_data_root` re-ran
`remove_var("CODEG_HOME")` / `set_var("CODEG_DATA_DIR", …)`, including legal
same-root ABI re-init after `init_eui` had installed a process-retained logging
worker (`EUI_LOG_GUARD` / `WorkerGuard`). That violated the startup-only pin
contract: env mutation after a surviving worker thread.

Prior Grok r1 did not elevate this to Important (under-weighted relative to the
Codex finding). The defect is real; this r2 treats the fix as required hardening
of the lifecycle surface, not optional polish.

### Remediation (`1e92ed75`)

| Behavior | Result |
| --- | --- |
| `verify_or_set_process_pin` → `Result<bool, DataRootError>` | `true` = first installer; `false` = equal already-pinned verify |
| First installer path | Still runs env remove/set **only** after successful pin install |
| Equal re-pin | Early `Ok(())` **before** env writes |
| Race loser on `OnceLock::set` | `roots_match` then `Ok(false)` — no second env write |
| Divergent root | `AlreadyPinned` — no env write |
| NUL / absolutize order | Unchanged: reject embedded NUL before pin/env |
| Production surface touched | Only `src-tauri/codeg-eui-core/src/data_root.rs` |

### Regression coverage

```text
same_root_abi_restart_does_not_repeat_environment_write_phase
  init → ENVIRONMENT_WRITE_PHASES == 1
  complete two-phase shutdown
  same-root init → ENVIRONMENT_WRITE_PHASES still == 1
  complete shutdown
```

The counter is `#[cfg(test)]` only and increments solely inside the real env
write path — appropriate seam for I1. Producer RED/GREEN narrative
(`left: 2, right: 1` → pass) is consistent with the shipped code.

### Does the fix weaken first-pin isolation?

No. Ambient-root isolation still depends on the **first** pin writing env
before logging and DB. Same-root restart continues to use the immutable process
pin and the already-correct env from that first write; bootstrap still passes
the explicit absolute root into DB/`AppState`. Skipping a second env rewrite is
the intended safety property, not a trust-boundary hole in the product model.

Edge note (non-blocking residual): if out-of-band host code mutated
`CODEG_*` between stop and re-init, re-pin would no longer re-assert env. That
is outside the approved EUI lifecycle (host must not race the pin) and is the
correct trade against concurrent env mutation with a live logging worker.

## Prior Approve Regression Check

| Prior approved surface (Grok r1) | After I1 fix |
| --- | --- |
| Pure resolver precedence / ambient ignore | Unchanged (no resolver edit) |
| First-pin env mutation order before workers | Preserved |
| ABI argument authority + bounds/UTF-8/NUL | Unchanged |
| Process-once pin; divergent → error | Preserved |
| Same-root re-init legal | Preserved; **safer** (no second env write) |
| `WebOnly` `new_eui` + excluded starts dormant | Unchanged (out of fix scope) |
| Joined ABI bootstrap shutdown | Unchanged |
| Isolation child-process tests | Unchanged |
| Product isolation / design SHA / gitlink | Unchanged |

**Conclusion:** prior `approve_with_minors` is not regressed; lifecycle safety is
strictly improved.

## Independent Verification (this host)

| Check | Result |
| --- | --- |
| `HEAD == 1e92ed75…` | Yes |
| Diff vs `8bac8d78` is only `data_root.rs` | Yes (+58/−10) |
| Static control-flow: env writes only after first-install `true` | Pass |
| Divergent pin still errors before env | Pass (source) |
| Pure pin-phase simulation (first write once; re-pin no write) | Pass |
| Full Cargo / shared `codeg` compile | Still unpaid on ~4 GiB no-swap host (producer SIGKILL residual) |

## Findings

### Critical

None.

### Important

None. Codex I1 is fixed on this artifact.

### Minor

1. **Residual mandatory Cargo verification on ≤4 GiB hosts**  
   Still applies from r1 / producer. Dependency-complete
   `cargo test`/`check` for Task 2 targets need a higher-memory host. Not a
   source defect in the fix.

2. **`StartedServices` remains a synthetic dormancy seam**  
   Unchanged from r1; out of I1 scope. Optional later hardening only.

## Non-Findings / Notes

- Process-retained EUI log writer still cannot flush via ABI shutdown (Codex M2 /
  producer residual). Unchanged by I1; not a data-root regression.
- Recovery-authorization maintenance via required `build_delegation_stack`
  remains residual, not re-opened by this fix.
- Unit-test env-write counter is process-global; currently only this lib unit
  test drives full ABI pin in-process. Acceptable for the focused regression;
  child-process isolation tests remain separate.

## Verdict Card

```text
VERDICT: approve_with_minors
critical: 0
important: 0
minor: 2
reviewed_commit: 1e92ed75da0702bc628b5f42e0af7fe5d48c7814
reviewed_task_id: 315c9c36-091c-4146-95de-0f071d43b7cf
prior_verdict: approve_with_minors (8bac8d78)
i1_status: fixed
continue_sequence: yes
code_changes_required: no
residual: re-run four mandatory Cargo targets on higher-memory host; optional stronger dormancy observability beyond StartedServices defaults
```

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":2,"summary":"I1 fixed: same-root re-pin is env-read-only; prior Grok approve not regressed. Residuals: host Cargo SIGKILL; synthetic StartedServices.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-grok-report-r2.md"}
-->
