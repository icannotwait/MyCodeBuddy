# Task 8 Report: Coordinator deadline sweep loop + liveness

## Status

**DONE**

## Commits

| SHA | Message |
| --- | --- |
| `4bf5e8ce6abf7dd36fae3721acd94b86fd4ada58` | feat(auto-title): 101s deadline sweep with ready re-notify |

Short: `4bf5e8ce`

Branch: `feat/auto-title-deadline-sweep`

## Files changed

| Path | Change |
| --- | --- |
| `src-tauri/src/auto_title/coordinator.rs` | **Modify** — partial source + deadline fields, sweep loop, production wiring, liveness tests |

`mod.rs` unchanged (exports already cover service helpers / partial trait).

## Implementation summary

### Coordinator fields

```rust
partial_source: Arc<dyn PartialAssistantTextSource>,
deadline: Duration,           // default 300s
sweep_interval: Duration,     // default 101s
batch_limit: usize,           // default 64
```

- `new` → `EmptyPartialSource` + production defaults (inert/tests).
- `new_with_deadline(...)` for injectable timings/source.
- `build_production_coordinator` wires `ManagerPartialSource::from_manager_ref` + 300s / 101s / 64.

### Loops under single `started` CAS

`recover_and_start` still recovers interrupted jobs, registers the live coordinator, then under `compare_exchange(false → true)` spawns **both**:

1. `notification_loop`
2. `deadline_sweep_loop`

Double `recover_and_start` does not spawn extra loops.

### Sweep body

```text
run_deadline_sweep_once → list candidates → partials_for → promote_by_ids
→ if promoted > 0 || any Ready row: notify_ready
→ sleep(sweep_interval)  // loop is promote-then-sleep ⇒ immediate first pass
```

- Errors: `tracing::warn!`, loop continues.
- Lost-wake healing: Ready rows without a wake still get `notify_ready` each pass.

### Test hooks (`test` / `test-utils`)

| Hook | Role |
| --- | --- |
| `sweep_pass_count` | Increments at start of every pass (incl. injected fail) |
| `arm_sweep_fail_once` / `sweep_fail_once` | First body returns `DbError::Validation` without promote |
| `notification_loop_starts` / `sweep_loop_starts` | Loop entry counters for single-spawn proof |

## Tests

| Test | Intent |
| --- | --- |
| `startup_runs_immediate_sweep_before_interval` | 3600s interval; pass_count ≥ 1 without waiting interval |
| `sweep_continues_after_transient_failure` | fail-once then short interval; pass_count increases again |
| `lost_wake_ready_row_is_renotified_and_claimed` | Seed Ready without notify; next sweep re-notifies → claim/finalize |
| `double_recover_and_start_single_notification_and_sweep_loops` | CAS: both start counters == 1 after double recover |
| `sweep_promotes_and_notifies_ready_drain` | `deadline: ZERO` + stub partial → promote → claim → title |

### Note on `start_paused`

Brief preferred `start_paused = true`. On this Windows/sqlx stack, paused time causes `ConnectionAcquire(Timeout)` / `PoolTimedOut` for in-memory SQLite. Liveness is instead proven with **short real intervals** (50ms) or a **long interval** (3600s) so the immediate first pass cannot be confused with a second tick.

## Verification

```powershell
cd src-tauri
cargo test --features test-utils auto_title::coordinator
# 20 passed

cargo check
cargo check --no-default-features --bin codeg-server
# ok (no coordinator warnings)
```

## Concerns

1. **No `start_paused` in CI for these tests** — real short sleeps are slightly timing-sensitive but 50ms + 2s timeouts are generous on this suite.
2. **Sweep always queries Ready** after promote (even when `promoted == 0`) — intentional lost-wake heal; cheap `limit(1)`.
3. **Production interval 101s** starts immediately at recover; empty workspaces pay one list query at startup (same as design).
4. **Existing fixtures** now also spawn the sweep loop via `new` defaults (EmptyPartialSource, 101s); inert partials mean no side effects beyond empty list + Ready re-notify if Ready rows exist during a pass.

---

## Codex review follow-up (liveness test hardening)

### Findings addressed

| Severity | Finding | Fix |
| --- | --- | --- |
| **P2** | `lost_wake_ready_row_is_renotified_and_claimed` could pass from **startup** `notify_ready` / mid-first-pass race if Ready was inserted before end-of-pass + drain settled | Wait for **end-of-pass** (`sweep_pass_count` now increments only after body returns) **and** `drain_idle_count >= 1` **before** seeding Ready; then require a **post-insert** completed pass before claim |
| **P3** | `sweep_continues_after_transient_failure` flaky on 50ms wall window after fail | Deterministic `sweep_fail_observed` hook; wait on fail-observed + next end-of-pass (no fixed post-fail sleep); interval 200ms |

### Test hooks added (`test` / `test-utils` only)

| Hook | Role |
| --- | --- |
| `sweep_pass_count` | **Moved to end-of-pass** (after `run_deadline_sweep_once` returns Ok or Err) |
| `sweep_fail_observed` | Increments when injected fail path runs |
| `drain_idle_count` | Increments when `drain_ready` exits on empty queue (`Ok(None)`) |

No production logic change beyond relocating the existing test counter to end-of-pass and adding test-only atomics.

### Verification

```powershell
cd src-tauri
cargo test --features test-utils auto_title::coordinator
# 20 passed
```
