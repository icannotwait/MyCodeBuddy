# Task 5 Report: RunStore test gate fail-fast

## Status

**DONE**

## Commits

| Hash | Message |
| --- | --- |
| `6d06783a173ead7799db3d59f5ef309049b36a26` | `test(delegation): bound all RunStore gates to five seconds` |
| *(docs tip)* | `docs(sdd): add task-5 RunStore gate fail-fast report` — run `git rev-parse HEAD` |

**Base:** `a02d3bd2` (Task 4 tip)  
**Code tip:** `6d06783a`  
**Branch tip:** includes docs commit after the code tip (local only until push).

## Files changed

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/run_store.rs` | `TEST_RUN_STORE_GATE_TIMEOUT = 5s`; settle + continue-admission gates use `timeout(...).await` → `TaskStoreError::Permanent("test run_store <settle\|continue_admission> gate timed out"\|"… release dropped")`; four fail-fast unit tests |
| `src-tauri/src/acp/delegation/broker.rs` | Bound `entered_rx` / `complete` joins in `parent_cancel_while_settling_preserves_completion_side_effects` with `TEST_RUN_STORE_GATE_TIMEOUT` |

## Implementation summary

1. **`TEST_RUN_STORE_GATE_TIMEOUT`** — `pub(crate) const` under `cfg(any(test, feature = "test-utils"))`, five seconds.
2. **Settle gate** (`settle_terminal`) — after signaling `entered`, release wait is `tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, rx)`:
   - Elapsed → `Permanent("test run_store settle gate timed out")`
   - RecvError (sender dropped) → `Permanent("test run_store settle gate release dropped")`
3. **Continue-admission gate** (`admit_continue_reserving`, mid-txn after eligibility) — same pattern with `continue_admission` message tags.
4. **Forbidden bare await removed** — no remaining `let _ = rx.await` on either RunStore gate release path.
5. **Harness joins** — `parent_cancel_while_settling_*` no longer unbounded-awaits entered/complete.
6. **RunStore sharing audit** — `parent_cancel_while_settling_preserves_completion_side_effects` already uses `DbDelegationTaskStore::from_run_store(runs.clone())` (shared gated instance). No fixture still splits RunStore for that race.

## TDD

### RED

- Added timeout / drop tests against bare `rx.await`.
- `settle_gate_release_timeout` hung past 60s job kill (oneshot never completes; no timer bound).

### GREEN

- Wrapped both gates with 5s timeout + Permanent errors.
- Tests use real-time SQLite setup, then `tokio::time::pause()` + `advance(5s)` only after the gate has entered (avoids `start_paused` racing sqlx `PoolTimedOut`).

## Tests run (narrow filters; 180s job kill)

```powershell
cd src-tauri
cargo test --features test-utils settle_gate -- --nocapture --test-threads=1
cargo test --features test-utils continue_admission_gate -- --nocapture --test-threads=1
cargo test --features test-utils parent_cancel_while_settling -- --nocapture --test-threads=1
# sanity happy-path for existing continue gate race
cargo test --features test-utils continue_and_replacement_admission -- --nocapture --test-threads=1
```

| Filter | Result |
| --- | --- |
| `settle_gate` | **5 passed** (2 new RunStore + 3 MockTaskStore) |
| `continue_admission_gate` | **2 passed** |
| `parent_cancel_while_settling` | **1 passed** |
| `continue_and_replacement_admission` | **1 passed** (release-path regression) |

## Self-review

- **Scope held:** only test-utils gate bounds + harness joins; no Task 4 production arm/wait paths.
- **Messages match brief:** `test run_store settle gate timed out` / `test run_store continue_admission gate timed out`.
- **Dropped release covered** (optional in brief; implemented).
- **MockTaskStore** already had its own 5s settle gate bound; left unchanged (different message prefix).

## Concerns

1. **`start_paused` incompatible with real SQLite setup** — virtual time races sqlx pool connect. Timeout tests pause only after gate entry. If someone reintroduces `#[tokio::test(start_paused = true)]` around `fresh_in_memory_db()`, expect `PoolTimedOut`.
2. **Gate timeout errors abort mid-txn continue admission** — Permanent rolls back the writer txn (desired for fail-fast tests). Production never installs these gates.
3. **Other broker tests** still use unbounded `entered_rx.await` on various gates; only `parent_cancel_while_settling` was required. Residual hang risk if those miswire release senders remains outside this task.
4. **No push** — local commit only, per instructions.

## Out of scope (confirmed not done)

- Task 6 acceptance pack
- Changing MockTaskStore message strings to the RunStore prefix
- Production (non-test) settle/continue paths
