# Task 6 Report — Startup reconcile bound/unbound split

**Branch:** `feat/delegation-promote-reliability`
**Worktree:** `D:\MyCodeBuddy\.worktrees\delegation-promote-reliability`
**Date:** 2026-07-26
**Implementer:** Grok
**Base HEAD:** `8dd2c0f3` (Tasks 1–5 complete)
**Prior partial:** `7ffb293c` (bound/unbound split in `reconcile_non_terminal` + audit helper — incomplete without named tests / full contract)

## Status

**COMPLETE** — startup reconcile splits unbound reserving (`host_restarted`) from bound reserving (`admission_unknown` + structured audit); not continuable; not auto-replayed; process-local `PendingTerminalRetry` documented as non-surviving across restart; four named tests prove the contract end-to-end.

## Summary

### Behavior

| Prior state | After reconcile | Continuable? | Recovery |
| --- | --- | --- | --- |
| Unbound `reserving` (`child_connection_id IS NULL`) | `failed` / `host_restarted` + audit `prior_status: reserving` | Yes (inherits `admission_class` via existing pre-admission path) | Safe continue |
| Bound `reserving` (`child_connection_id IS NOT NULL`) | `failed` / `admission_unknown` + audit `{ prior_status: reserving, restart_provenance: bound_reserving }` | **No** | Explicit `replacement_reason = admission_unknown` only |
| `running` | unchanged from prior: `canceled` / `host_restarted` | Unexpected-continue path when budget remains | Existing |

### Audit (bound reserving)

```json
{
  "version": 1,
  "source": "host_restart",
  "reason": "admission_unknown",
  "prior_status": "reserving",
  "restart_provenance": "bound_reserving",
  "note": "child_connection_id was bound; prompt may have been accepted before restart"
}
```

### Non-continuability / no auto-replay

- Bound reconcile outcome uses `error_code = admission_unknown`, which is deny-listed in `is_revision_eligible_failure` and never matches the pre-admission `host_restarted` continue inherit path.
- No automatic prompt replay after restart; Skill must issue an explicit replacement.
- Doc comment on `reconcile_non_terminal`: process-local `PendingTerminalRetry` does **not** survive host restart; still-non-terminal rows are handled only by this durable gate.

### Completes partial `7ffb293c`

Partial already introduced:
- `host_restarted_bound_reserving_audit()`
- bound vs unbound branch inside `reconcile_non_terminal`

This task completed the contract with named tests and the `PendingTerminalRetry` comment, and re-verified eligibility through the real replace-admit path.

## Named tests

| Test | Result |
| --- | --- |
| `reconcile_unbound_reserving_host_restarted` | PASS |
| `reconcile_bound_reserving_admission_unknown_with_audit` | PASS |
| `gen1_post_accept_pre_promote_bound_crash_not_continuable` | PASS |
| `admission_unknown_replacement_eligible` | PASS |

Also still green: existing `reconcile_status_and_audit_split_reserving_vs_running`.

## Verify commands

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils --lib reconcile_ -- --nocapture
# 34 passed (includes 2 new named reconcile_ tests + existing)

cargo test --features test-utils --lib admission_unknown -- --nocapture
# 6 passed (includes admission_unknown_replacement_eligible + bound reconcile)

cargo test --features test-utils --lib gen1_post_accept -- --nocapture
# 1 passed (gen1_post_accept_pre_promote_bound_crash_not_continuable)

cargo check
# Finished ok
```

## Files

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/run_store.rs` | PendingTerminalRetry comment on reconcile; four named contract tests |

## Commits

| Hash | Message |
| --- | --- |
| `7ffb293c` | prior partial: split implementation (incomplete) |
| `33c42260` | `fix(delegation): split reserving restart into host_restarted vs admission_unknown` |
| `bc48496d` | `docs(delegation): Task 6 reconcile bound/unbound split report` |

## Concerns / residual

- Bound-but-pre-send false positives remain by design (bind precedes prompt send); recovery is explicit replacement with duplicate-execution warning (Task 5 surfaces).
- `PendingTerminalRetry` itself is process-local in broker/store memory — this task only documents the restart interaction; no durable retry-record migration.
- Tasks 7–8 not started (timestamps/metrics; full verification).
- Historic `host_restarted` rows (pre-split, bound reserving classified as host_restarted) are not rewritten — by design.

## Out of scope (confirmed)

- No Tasks 7–8 work
- No frontend card redesign
- No `settle_terminal` write-first refactor
- No automatic prompt replay
