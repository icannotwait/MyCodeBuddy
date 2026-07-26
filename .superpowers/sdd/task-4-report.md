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

## Fix round 1/5 (Codex review)

**Commit:** `120d4fd7` `fix(delegation): Task 4 FWW report ownership and promote diagnostics`

### Important 1 — Lost first-terminal claim reports owner
- Occupied disposition path now builds report from durable terminal when present, else `TerminalIntent::to_report` from the **existing** disposition/overlay.
- Never falls back to the losing promote `outcome` classification.
- Test: `promote_lost_claim_reports_existing_owner_not_admission` asserts exact `parent_canceled`.

### Important 2 — Adopt different retry owner + disposition alignment
- On `put_retry` miss, replace local `closed_handoff_dispositions` (and completed overlay when present) with the adopted owner's terminal payload.
- Caller report uses adopted code; `continue_abort_if_handoff_closed` can no longer override with stale `admission_failed`.
- Tests:
  - `promote_existing_retry_owner_different_payload_adopted` — exact `parent_canceled` + disposition aligned
  - `continue_promote_existing_retry_owner_different_payload_adopted` — exact FWW on continue path

### Important 3 — Structured diagnostics
- `PromoteAttemptMeta` carries `last_sqlite_primary` / `last_sqlite_extended` from raw `DbErr` during retry.
- `PromoteOnceError::Retry` threads codes; BUSY_SNAPSHOT log uses structured fields.
- Settlement enqueue / Won / Existing / transient exhaust / freeze logs include generation, agent_type, admission_class, attempt, sqlite codes, failure class.

### Minor
- Fixed misaligned `ensure_bound` / promote call sites; `git diff --check` clean for trailing whitespace.

### Verify
```
cargo test --features test-utils --lib promote_     # 48 passed
cargo test --features test-utils --lib admission_   # 31 passed
cargo test --features test-utils --lib finalizer_   # 1 passed
cargo test --features test-utils --lib cancel_failure # 1 passed
cargo check # ok
```

## Fix round 2/5 (Codex re-review residuals)

**Commit:** `35d15011`

### Important 2 residual — Completed FWW adoption
- Adoption now projects the full `TerminalTaskWrite` via `outcome_from_terminal_write` / `report_from_terminal_write`.
- `status = Completed` with `error_code = None` reports **Completed** (never invents `admission_failed`).
- Disposition/overlay alignment uses the full payload (`build_completed` from adopted outcome).
- Tests:
  - `promote_existing_retry_owner_completed_payload_adopted` (gen-1)
  - `continue_promote_existing_retry_owner_completed_payload_adopted`

### New Important — Retry adopt recheck fence
- After first `get_retry` clone, honor test gate then recheck durable terminal and re-fetch retry before any disposition alignment.
- Durable terminal wins: disconnect/release and report durable truth — **no** stale reinsert.
- Retry still live: align from full TerminalTaskWrite (skip align if finalized overlay already terminal).
- Retry gone + non-terminal durable: project process-local intent only (no stale clone).
- Test gate: `install_post_accept_adopt_retry_recheck_gate`
- Test: `promote_adopt_recheck_fence_avoids_stale_disposition_after_durable_finalize`

### Non-regression
- Important 1/3 and Minor paths untouched in spirit; lost-claim / sqlite meta / cancel tests still pass.

### Verify
```
cargo test --features test-utils --lib promote_       # 51 passed
cargo test --features test-utils --lib admission_     # 31 passed
cargo test --features test-utils --lib finalizer_     # 1 passed
cargo test --features test-utils --lib cancel_failure # 1 passed
cargo check # ok
```

## Fix round 3/5 (Codex re-review2 residual)

**Commit:** `05dd547b`

### Important — Retry gone must not release without settlement ownership
- After `put_retry` loss + recheck fence, durable load is **error-aware**:
  load `Err` is not treated as terminal truth; log and reacquire ownership.
- Retry gone + durable non-terminal/unknown: **reacquire** intended
  `PendingTerminalRetry` (bounded loop) then fall through to
  `settle_with_retry` (Won / Existing / transient worker / permanent freeze).
- Never call `post_accept_release_coordination` while durable is
  non-terminal/unknown and no retry/freeze owner exists.
- Exhaust path forces intended put (or adopts concurrent owner) before settle.

### Test
- `promote_retry_gone_reacquires_ownership_before_release` — gate between
  get_retry and recheck; remove retry + inject durable load failure; assert
  retry/freeze **or** durable terminal owner remains.

### Cleanup
- Removed tracked duplicate report
  `.superpowers/sdd/2026-07-26-delegation-promote-reliability/task-4-report.md`
  (canonical: `.superpowers/sdd/task-4-report.md`).

### Non-regression
- Prior FWW / fence / completed adoption / lost-claim / sqlite meta paths kept.

### Verify
```
cargo test --features test-utils --lib promote_       # 52 passed
cargo test --features test-utils --lib admission_     # 31 passed
cargo test --features test-utils --lib finalizer_     # 1 passed
cargo test --features test-utils --lib cancel_failure # 1 passed
cargo check # ok
```
