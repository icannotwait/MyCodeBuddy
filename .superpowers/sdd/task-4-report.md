# Task 4 Report: Canonical arm path, typed bind outcomes, transfer ownership

## Status

**DONE** (FIX wave 5: residual ACK-vs-closed bias + WaitCancelGuard-after-register)

## Commits

| Hash | Message |
| --- | --- |
| `fd8a3bfcc7ba7c0642a0d69369cca308642d3c50` | `feat(delegation): arm indefinite waits with exact tool id and transferable ownership` |
| `aa12b38e…` | `fix(delegation): terminalize arming when wait transfer oneshot closes` |
| `a1e3d1f0f582516ceb86ccbcce2762e55ab62858` | `fix(delegation): abort arm task on wait cancel before transfer/suspend` |
| `0f413bdc3ff71a67a4572b4d8651194b552d2a7e` | `fix(delegation): fence post-suspend-ack cancel before Waiting CAS` |
| `749e99fff13b5db206f49ad413d1094df13a9981` | `fix(delegation): post-ack cancel preserves resumable Waiting` |
| `7acad0107e0e1d4768f7ce736be6ec0900da0125` | `fix(delegation): prefer ACK over closed; guard wait on register` |
| `6fae4f229ecfe5b5169367f5b0048400a2942e8a` | `docs(sdd): task-4 report for residual race wave 5` |

**Wave 5 code fix HEAD:** `7acad0107e0e1d4768f7ce736be6ec0900da0125`  
**Base before this wave:** `749e99fff13b5db206f49ad413d1094df13a9981`

## Files changed (FIX wave 5)

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/continuation/coordinator.rs` | Biased suspend selects prefer ready ACK over `cancel` / `completion.closed()`; intermediate join/sleep fences `poll!` suspend first so pre-suspension fail cannot run after real suspend ACK is available |
| `src-tauri/src/acp/delegation/continuation/tests.rs` | `continuation_coordinator_ack_ready_beats_completion_closed` — release suspend + drop completion without await between so both are ready on one poll; asserts durable Waiting |
| `src-tauri/src/acp/delegation/listener.rs` | Install `WaitCancelGuard` immediately after successful `wait_cancel.register` (before `bind_delegation_wait`); bind-gated peer-close regression |

## FIX wave 5 summary

### Important 1 — ACK vs `completion.closed` bias

**Bug:** Biased `select!` ranked `completion.closed()` above the suspend future. Wait-cancel aborts `arm_task` (drops the completion receiver) while the connection may already have cleared the parent turn and produced an ACK. When both were ready, the close branch called pre-suspension failure **after real suspension**, leaving Failed with no resumable Waiting.

**Fix:** Prefer `result = &mut suspend` first in all suspend-race selects. Closed/cancel only win when ACK is not yet ready. Intermediate notified/sleep paths also `futures::poll!(suspend)` before pre-suspension fences.

**Regression:**
- `continuation_coordinator_ack_ready_beats_completion_closed` — gated suspend entered → release ACK + drop completion with no yield between → Waiting + `suspended_at`, active slot retained, children Running.

### Important 2 — WaitCancelGuard after register

**Bug:** Guard was installed only after `bind_delegation_wait`. Peer-close dropping `process_status` during bind leaked the wait registration (no Drop cleanup).

**Fix:** Construct `WaitCancelGuard` immediately after successful `register`, then bind. Drop still async-deregisters.

**Regression:**
- `peer_close_during_bind_deregisters_wait_registration` — bind-gated lookup, abort status after register+bind-enter, assert registry clean / cancel NotFound, children untouched.

## FIX wave 4 summary (prior)

Post-ack cancel must use post-suspension ownership (commit Waiting; never `fail_before` after Ok(ack)).

## FIX wave 3 summary (prior)

Post-ack cancel fence before Waiting CAS (incorrectly used pre-suspension failure — corrected in wave 4).

## FIX wave 2 summary (prior)

Cancel vs transfer mutual exclusion: listener abort+await arm_task; pre-suspension cancel/`closed` terminalizes Arming.

## Original implementation summary (main Task 4 commit)

1. **Broker preflight** — `Ready` vs `NeedPark { canonical_task_ids }`.
2. **Canonical arm helper** — exact tool id, register canonical task ids, typed bind, cancel-aware park.
3. **Continuation transfer barrier** — oneshot `TransferredWait`; failed transfer drops tx without send.

## Tests run (FIX wave 5, narrow filters only; 180s job kill)

```powershell
cargo test --features test-utils --lib continuation_coordinator_ack_ready_beats_completion_closed -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_post_ack -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_during_bind_deregisters_wait_registration -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_wait_cancel_after_arming -- --nocapture --test-threads=1
cargo test --features test-utils --lib cancel_during_transfer_oneshot -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_waiter_close -- --nocapture --test-threads=1
cargo test --features test-utils --lib pre_suspension -- --nocapture --test-threads=1
cargo test --features test-utils --lib legacy_indefinite_registers -- --nocapture --test-threads=1
```

**Results (all green under the filters above):**

| Filter | Count |
| --- | --- |
| `continuation_coordinator_ack_ready_beats_completion_closed` | 1 passed |
| `continuation_coordinator_post_ack*` | 4 passed |
| `peer_close_during_bind_deregisters_wait_registration` | 1 passed |
| `continuation_wait_cancel_after_arming*` | 1 passed |
| `cancel_during_transfer_oneshot*` | 1 passed |
| `continuation_coordinator_waiter_close*` | 2 passed |
| `pre_suspension*` | 3 passed |
| `legacy_indefinite_registers*` | 1 passed |

## Self-review

- **ACK-prefer:** when suspend ACK is ready, closed cannot Failed-terminalize via pre-suspension path.
- **Pre-ack cancel still works:** closed alone while suspend Pending still pre-fails (transfer/cancel-after-arming tests green).
- **Register→guard→bind:** peer-close mid-bind deregisters; no ownerless wait stamp.
- **No full cargo suite** (process: avoid hang; narrow filters + 180s kill).

## Concerns

1. **Closed-only while suspend still Pending** still cancels the suspend future (intentional pre-ack fence). The residual both-ready bias is fixed; true “connection cleared turn but ACK future not yet Ready” mid-poll remains a theoretical window only if suspend completion and closed race across separate polls without ACK becoming Ready first — production ACK readiness tracks turn clear closely.
2. **Cancel after durable Waiting** still ends only the MCP status wait; continuation remains Waiting until Task 7/8 wake/cleanup — intentional.
3. **Pre-existing:** `continuation_cleanup_cancel_fences_before_first_suspension_dispatch` expects non-Failed after pre-suspension worker cancel, but wave-2 terminalizes Arming→Failed. Out of this fix scope.
4. **Bind soft-fail** still parks after tool-id/lease/bind notes (register is the only fail-closed path).
5. **600+600 wait-only** remains supervisor composition (Task 6 for full E2E).

## Out of scope (confirmed not done)

- Task 5 RunStore gate bounds
- Task 6 conversation 1570 full acceptance pack
- Push / PR
