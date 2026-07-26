# Task 4 Report — Shared post-accept failure helper + claim-filter tighten + settlement ownership

**Branch:** `feat/delegation-promote-reliability`  
**Worktree:** `D:\MyCodeBuddy\.worktrees\delegation-promote-reliability`  
**Date:** 2026-07-26  
**Implementer:** Grok  
**Base HEAD:** `1c04454a` (Tasks 1–3 complete)

## Status

**COMPLETE** — honest post-accept admission failure handling for gen-1 and continuation.

## Summary

### `run_store.rs`

1. **Claim filter tighten** — `promote_running_once` claim write filters  
   `task_id` + `status = reserving` + `child_connection_id = expected`.
2. **No null→id first write on success** — status promote write no longer sets  
   `ChildConnectionId`; retains pre-bound connection only (same expected-connection filter on status update).
3. **Test fixture updates** — seed helpers bind before promote; `ensure_bound` helper + call-site binds for legacy promote unit tests.
4. **Named test** `promote_claim_requires_expected_child_connection`.

### `broker.rs`

1. **Shared helper** `handle_post_accept_promote` + `settle_post_accept_admission_failure`:
   - Classify `PromoteRunningOutcome` (no `store_err_to_delegation_error` collapse).
   - Terminal winner → cancel/disconnect + replay durable winner.
   - Already-running / Promoted → `Proceed` (success path).
   - Budget → durable/wire `budget_exhausted`.
   - Retry exhaust / state conflict / permanent → `admission_failed` (**never** `spawn_failed`).
   - Claim local first-terminal; cancel non-blocking; `settle_with_retry` with intended code (no PE-rewrite arm).
   - Existing different retry payload → adopt FWW owner.
   - Transient settle exhaust → `PendingTerminalRetry` with **original** intended terminal + worker.
   - Permanent settle miss → freeze ownership (intended payload) before coordination release; caller gets sanitized `persistence_error`.
2. **Gen-1 + continue** promote call sites switched to `promote_running_detailed` + helper.
3. **Finalizer same-owner** — `admission_failed` / `budget_exhausted` recognized with `unresumable` / `persistence_error`.
4. **Structured logs** — task_id, generation, agent_type, admission_class, attempt/retry meta, failure class, intended code (no prompt/secrets).

## Named tests (TDD)

| Test | Result |
| --- | --- |
| `gen1_promote_transient_then_success_no_cancel` | PASS |
| `continue_promote_transient_then_success` | PASS |
| `promote_retry_exhaust_settles_admission_failed_not_spawn_failed` | PASS |
| `promote_budget_exhaust_settles_budget_exhausted` | PASS |
| `promote_failure_first_terminal_wins_replay` | PASS |
| `promote_settlement_retry_keeps_admission_code` | PASS |
| `promote_permanent_settlement_freeze_ownership` | PASS |
| `promote_existing_retry_owner_different_payload_adopted` | PASS |
| `finalizer_recognizes_admission_failed_and_budget_exhausted_same_owner` | PASS |
| `cancel_failure_does_not_block_settlement` | PASS |
| `promote_claim_requires_expected_child_connection` | PASS |

## Verify commands

```powershell
Set-Location D:\MyCodeBuddy\.worktrees\delegation-promote-reliability\src-tauri
cargo test --features test-utils --lib promote_ -- --nocapture
# 46 passed

cargo test --features test-utils --lib admission_ -- --nocapture
# 31 passed (includes finalizer_recognizes_*, promote_retry_exhaust_*, …)

cargo test --features test-utils --lib finalizer_ -- --nocapture
# 1 passed

cargo test --features test-utils --lib cancel_failure -- --nocapture
# 1 passed

cargo check
# Finished ok
```

## Files

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/run_store.rs` | Claim filter, no connection first-write, fixtures, claim test |
| `src-tauri/src/acp/delegation/broker.rs` | Shared helper, gen1/continue wire, finalizer same-owner, named tests |

## Concerns / notes

- Cancel may be observed more than once for the same connection (idempotent); existing budget-refusal test loosened to `any` match.
- `already_running` is returned from the helper for Task 7 metrics; gen-1 still records accepted only after running insert (no double path exercised here).
- Tasks 5–8 not implemented.
- Overwrote unrelated stale `task-4-report.md` from a previous plan.

## Self-review

- Post-accept outcomes never map to `spawn_failed` via `store_err_to_delegation_error`.
- Settlement ownership follows bootstrap claim-first + intended-payload retry/freeze.
- Claim filter requires expected bound connection; promote success retains bind.
