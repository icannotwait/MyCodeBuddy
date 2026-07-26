# Final Fix Report: `feat/delegation-promote-reliability`

**Date:** 2026-07-27  
**Branch:** `feat/delegation-promote-reliability`  
**Worktree:** `D:\MyCodeBuddy\.worktrees\delegation-promote-reliability`  
**Source review:** `.superpowers/sdd/final-branch-review.md`  
**Fix commit:** `407a45a5` — `fix(delegation): final promote reliability review residuals`

## Status

**All three Important findings fixed in a single pass.**  
Minor formatter residual left as documented separate debt (no 54-file format).

## Fixes

### 1. Identity pre-read no longer bypasses promote retries

**Finding:** `load_promote_retry_identity` errors (including transient BUSY/LOCKED) collapsed to `PromoteRunningKind::Permanent` with `attempts == 0`, canceling admission without retries.

**Change (`run_store.rs`):**
- Removed fallible identity pre-read from the admission-critical path.
- Identity for structured retry logs is loaded **lazily / best-effort** via `try_load_promote_retry_identity` only when a promote-local retry is about to be logged.
- Load failure returns `None` and **skips** structured retry emission (never fabricates `"unknown"` labels).
- Promote loop always runs its bounded attempts regardless of identity load outcome.

**Regression tests:**
- `promote_identity_load_failure_no_unknown_retry_logs` — inject identity fail + claim BUSY → still promotes (`attempts == 2`); no fabricated unknown labels.
- `promote_identity_load_busy_still_gets_bounded_attempts` — identity inject + three claim faults → `RetryExhausted` with `attempts == 3` (not 0).

### 2. `promote_connection_matches` requires bound expected owner

**Finding:** `None` child_connection_id returned `true`, so unbound `running` rereads looked like success for an unrelated caller.

**Change (`run_store.rs`):**
```rust
fn promote_connection_matches(run: &PersistedRun, expected: &str) -> bool {
    match run.child_connection_id.as_deref() {
        Some(id) => id == expected,
        None => false, // unbound running = ownership conflict
    }
}
```

**Regression tests:**
- `promote_zero_row_running_null_connection_is_ownership_conflict`
- `promote_commit_ambiguity_running_null_connection_is_ownership_conflict`

Both force `running + child_connection_id NULL` then assert `StateConflict { Ownership }`.

### 3. `admission_failed_by_agent` counts durable winners only

**Finding:** Counter incremented before first-terminal-wins settle; losers (Existing cancel/completion, different retry owner, permanent PE freeze) still inflated the metric.

**Change (`broker.rs`):**
- Removed pre-settle `record_admission_failed` from `RetryExhausted` / `StateConflict` / `Permanent` arms.
- Record only on durable `Settlement::Won` when the **winner report** `error_code` is exactly `admission_failed`.

**Regression tests:**
- `admission_failed_metric_not_inflated_when_existing_cancel_wins`
- `admission_failed_metric_not_inflated_for_different_retry_owner`
- `admission_failed_metric_not_inflated_on_permanent_settle_failure`
- `promote_retry_exhaust_settles_admission_failed_not_spawn_failed` — asserts counter == 1 on durable Won.

## Minor (not fixed)

**Workspace formatter gate** remains red for ~54 out-of-map files (pre-existing drift). Mapped files untouched by this fix; left as separate repository maintenance debt per review recommendation.

## Verification

| Command | Result |
| --- | --- |
| `cargo check --features test-utils` | PASS |
| `cargo test --features test-utils --lib promote_` | **61/61 PASS** |
| `cargo test --features test-utils --lib admission_` | **51/51 PASS** |

Focused new tests (all PASS):
- `promote_identity_load_failure_no_unknown_retry_logs`
- `promote_identity_load_busy_still_gets_bounded_attempts`
- `promote_zero_row_running_null_connection_is_ownership_conflict`
- `promote_commit_ambiguity_running_null_connection_is_ownership_conflict`
- `admission_failed_metric_not_inflated_when_existing_cancel_wins`
- `admission_failed_metric_not_inflated_for_different_retry_owner`
- `admission_failed_metric_not_inflated_on_permanent_settle_failure`
- `promote_retry_exhaust_settles_admission_failed_not_spawn_failed`

## Files touched

| File | Role |
| --- | --- |
| `src-tauri/src/acp/delegation/run_store.rs` | Identity deferral; strict ownership match; promote tests |
| `src-tauri/src/acp/delegation/broker.rs` | Won-gated admission metric; race metric tests |

## Assessment

Ready for re-review / merge of Important residuals, subject to reviewer confirmation. Formatter residual remains separate.
