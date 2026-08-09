# Task 2 Independent Re-Review (Grok) r3 — High-Gate N1 Fix

## Review Basis

| Item | Value |
| --- | --- |
| Role | Independent Task 2 high-risk Reviewer 2 (Grok) |
| Work unit | `task\|2\|reviewer\|grok\|none` |
| Workspace | `/workspace/MyCodeBuddy/.worktrees/feat/eui-neo-frontend-spike` |
| reviewed_task_id (latest) | `dc04d65a-a464-4e31-9c57-497a4792a0e6` |
| Prior reviewed_task_id (I1 fix) | `315c9c36-091c-4146-95de-0f071d43b7cf` |
| Commit (HEAD) | `be8b41cf8545470694e2d0b490ec5b6f6cb1a227` |
| FIX_BASE | `1e92ed75da0702bc628b5f42e0af7fe5d48c7814` |
| Fix package | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-fix2-review-package.md` |
| Prior Grok reviews | r1 `task-2-review-grok-report.md`, r2 `task-2-review-grok-report-r2.md` (both `approve_with_minors`) |
| Codex N1 context | `task-2-review-codex-report-r2.md` (`request_changes` on `1e92ed75`) |
| Producer report | `task-2-report.md` § High-Gate Fix Round 2/5 |
| Global constraints | `.superpowers/sdd/2026-08-09-eui-neo-frontend-spike/global-constraints.md` |
| Risk | `high` (`security_trust_boundary` + `concurrency_lifecycle`); policy `b2d_task_risk_v1` |

Independent of the implementer and of Codex. Scope: N1 fix delta plus
confirmation that I1 and the prior approved Task 2 surfaces remain intact.

## Overall Assessment

N1 is fixed. First-pin environment mutation (`CODEG_HOME` clear +
`CODEG_DATA_DIR` overwrite) now runs **inside** `PINNED_EUI_DATA_ROOT.get_or_init`,
so the process pin is published only after that trust-boundary phase completes.
Concurrent equal callers block in `get_or_init` until publication; they cannot
observe a half-initialized pin or return success while ambient main roots still
apply.

Serial I1 behavior is preserved: later equal re-pins do not re-enter the env
write path (`ENVIRONMENT_WRITE_PHASES` stays `1` across full ABI restart).
Divergent roots still become `AlreadyPinned` after init completes.

No regression of isolation, resolver precedence, ABI ownership, dormant
`WebOnly` profile, or joined shutdown. Scope is again a single file:
`src-tauri/codeg-eui-core/src/data_root.rs`.

**Verdict: `approve_with_minors`**

N1 and I1 are both resolved on this artifact. Non-blocking residuals from
prior Grok reviews remain (host Cargo evidence; synthetic `StartedServices`).

## Finding Dispositions

| Finding | Disposition | Result |
| --- | --- | --- |
| I1: same-root re-init repeats env writes after logging worker | **Still fixed** | `get_or_init` does not re-run; write-phase counter stays `1` after second ABI init |
| N1: pin published before first env-write phase completes | **Fixed** | Env writes inside `get_or_init`; concurrency test proves no early publish / no early equal return |
| Grok r1/r2 minors (Cargo SIGKILL; synthetic `StartedServices`) | **Unchanged residual** | Out of N1 scope |

## N1 Fix Audit

### Defect (Codex N1 on `1e92ed75`)

I1 moved equal re-pins off the env-write path by publishing via
`OnceLock::set` **before** `remove_var`/`set_var`. A concurrent equal caller
could then:

1. observe the published root,
2. return `Ok(())` immediately,
3. proceed into logging/bootstrap while ambient `CODEG_HOME` / `CODEG_DATA_DIR`
   were still live.

That is a real trust-boundary race on the exported Rust pin API (even though
the public C ABI is UI-thread-only). Grok r2 under-weighted concurrent
first-pin publication order; this r3 accepts N1 as valid Important-class
breakage introduced by the I1 shape, now corrected.

### Remediation (`be8b41cf`)

```rust
let pinned = PINNED_EUI_DATA_ROOT.get_or_init(|| {
    // pause seam (test only)
    env::remove_var("CODEG_HOME");
    env::set_var("CODEG_DATA_DIR", &absolute);
    // phase counter (test only)
    absolute.clone()
});
roots_match(pinned, &absolute)
```

| Scenario | Behavior |
| --- | --- |
| First installer | Runs env phase inside init closure; publishes only on success |
| Concurrent equal caller during init | Blocks in `get_or_init`; `OnceLock::get()` remains `None` until complete |
| Concurrent equal after publish | Returns existing pin; `roots_match` OK; **no** second env write |
| Concurrent / later divergent | Waits if needed, then `AlreadyPinned`; **no** env write for loser |
| Serial same-root ABI restart (I1) | Init closure not re-run; write phase stays `1` |
| Embedded NUL | Still rejected **before** `get_or_init` |

### Regression coverage

`pin_lifecycle_publishes_only_after_environment_write_phase`:

1. Installs a two-barrier pause inside the first env-write phase.
2. Asserts `PINNED_EUI_DATA_ROOT.get().is_none()` while paused.
3. Starts an equal concurrent pin; asserts it does **not** return within 250 ms
   while the first is paused.
4. Releases the first writer; both complete `Ok(())`; write phases == `1`.
5. Runs full ABI init/shutdown/re-init and asserts write phases still == `1`
   (I1 still covered in the same test).

Producer RED (`equal pin returned before env write completed`) / GREEN
narrative matches this seam.

## Prior Approve Regression Check

| Surface | After N1 fix |
| --- | --- |
| Pure resolver / ambient ignore | Unchanged |
| First-pin env mutation before logging/Tokio/DB | Preserved; now **atomic with publication** |
| I1: no env rewrite on same-root re-init | Preserved |
| ABI argument authority, bounds, UTF-8, NUL | Unchanged |
| Divergent root → stable error | Preserved |
| `WebOnly` / dormant profile / joined shutdown | Unchanged (out of fix scope) |
| Isolation child-process tests | Unchanged |

**Conclusion:** prior Grok `approve_with_minors` is not regressed; concurrent
first-pin safety is improved.

## Independent Verification (this host)

| Check | Result |
| --- | --- |
| `HEAD == be8b41cf…` | Yes |
| Diff vs `1e92ed75` only `data_root.rs` | Yes (+85/−35) |
| Static: env writes only inside `get_or_init` | Pass |
| Static: I1 restart assertion still present | Pass |
| Static: concurrency early-publish / early-return asserts present | Pass |
| Full Cargo / shared `codeg` compile | Still unpaid on ~4 GiB no-swap class (producer residual) |

## Findings

### Critical

None.

### Important

None. Codex N1 is fixed on this artifact. I1 remains fixed.

### Minor

1. **Residual mandatory Cargo verification on ≤4 GiB hosts**  
   Unchanged. Dependency-complete Cargo targets still need a higher-memory
   host. Not a defect in the N1 source fix.

2. **`StartedServices` remains a synthetic dormancy seam**  
   Unchanged from r1/r2; out of N1 scope.

## Non-Findings / Notes

- Process-retained EUI log writer still cannot flush via ABI shutdown
  (long-standing residual). Unchanged by N1.
- Recovery-authorization maintenance via required `build_delegation_stack`
  remains residual.
- UI-thread-only ABI does not remove the need for atomic pin publication on
  the safe Rust export; the `get_or_init` design is the right fix.
- Test pause hooks are `#[cfg(test)]` only and do not affect product builds.

## Verdict Card

```text
VERDICT: approve_with_minors
critical: 0
important: 0
minor: 2
reviewed_commit: be8b41cf8545470694e2d0b490ec5b6f6cb1a227
reviewed_task_id: dc04d65a-a464-4e31-9c57-497a4792a0e6
prior_verdicts: r1/r2 approve_with_minors
i1_status: fixed
n1_status: fixed
continue_sequence: yes
code_changes_required: no
residual: re-run four mandatory Cargo targets on higher-memory host; optional stronger dormancy observability beyond StartedServices defaults
```

<!-- codeg-card-summary-v1
{"kind":"review","verdict":"approve_with_minors","critical":0,"important":0,"minor":2,"summary":"N1 fixed: pin publish atomic with first env write; I1 still holds. Prior approve not regressed. Residuals: Cargo host limit; synthetic StartedServices.","report_file":".superpowers/sdd/2026-08-09-eui-neo-frontend-spike/task-2-review-grok-report-r3.md"}
-->
