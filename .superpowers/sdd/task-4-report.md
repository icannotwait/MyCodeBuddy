# Task 4 Report: Canonical arm path, typed bind outcomes, transfer ownership

## Status

**DONE** (FIX wave 6: closed-before-ACK after control-sent + transfer disarm)

## Commits

| Hash | Message |
| --- | --- |
| `fd8a3bfcc7ba7c0642a0d69369cca308642d3c50` | `feat(delegation): arm indefinite waits with exact tool id and transferable ownership` |
| `aa12b38e…` | `fix(delegation): terminalize arming when wait transfer oneshot closes` |
| `a1e3d1f0f582516ceb86ccbcce2762e55ab62858` | `fix(delegation): abort arm task on wait cancel before transfer/suspend` |
| `0f413bdc3ff71a67a4572b4d8651194b552d2a7e` | `fix(delegation): fence post-suspend-ack cancel before Waiting CAS` |
| `749e99fff13b5db206f49ad413d1094df13a9981` | `fix(delegation): post-ack cancel preserves resumable Waiting` |
| `7acad0107e0e1d4768f7ce736be6ec0900da0125` | `fix(delegation): prefer ACK over closed; guard wait on register` |
| `21b3f6485df7cd021f25f9fd384832846553b78d` | `fix(delegation): await ACK after suspend control; disarm on transfer` |
| *(docs commits after code fix)* | `docs(sdd): update task-4 report for residual race wave 6` |

**Wave 6 code fix:** `21b3f6485df7cd021f25f9fd384832846553b78d`  
**Base before this wave:** `bf55df71abec9fdec5397044db9c3d58244cb8e9` (wave-5 docs tip; code base `7acad010`)  
**Branch tip:** run `git rev-parse HEAD` (includes docs commits after the code fix).

## Files changed (FIX wave 6)

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/continuation/coordinator.rs` | After `suspend_parent` is in flight, cancel/`completion.closed` await ACK (never `fail_before` / `fail_waiter_gone_before`); closed-only while ACK Pending still reaches post-ack Waiting |
| `src-tauri/src/acp/delegation/continuation/tests.rs` | `continuation_coordinator_closed_before_ack_after_suspend_requested_preserves_waiting` |
| `src-tauri/src/acp/delegation/listener.rs` | After successful `transfer_tx.send`, clear guard `drop_armed` immediately; peer-close after transfer regression; wait-cancel after control-sent preserves Waiting |
| `src-tauri/src/acp/delegation/wait_cancel.rs` | `WaitCancelGuard` shared `drop_armed` latch + `drop_armed_flag()`; unit test for skip-Drop-deregister |

## FIX wave 6 summary

### Important 1 — closed before ACK (not only both-ready)

**Bug:** Wave 5 fixed both-ready bias (prefer ACK when both ready). Residual: `completion.closed()` can win alone while suspend is still Pending after real control was sent (wait cancel aborted `arm_task`). Pre-suspension fail then left Failed with no resumable Waiting after the parent turn clear was already in flight.

**Fix:** Once `suspend_parent` is called/pinned, cancel/closed never use `fail_before_suspension` / `fail_waiter_gone_before_suspension`. Await the in-flight suspend future; post-ack path commits durable Waiting (or `fail_after_suspension` on true post-suspend errors).

**Regression:**
- `continuation_coordinator_closed_before_ack_after_suspend_requested_preserves_waiting` — enter suspend gate → drop completion without releasing → still non-terminal → release ACK → Waiting + active slot + children Running
- `continuation_wait_cancel_after_suspend_control_preserves_waiting` — listener cancel after transfer/suspend-entered ends MCP wait/registry clean but commits Waiting (renamed semantics from wave-2 “no suspended”)

### Important 2 — transfer vs peer-close disarm

**Bug:** Listener `WaitCancelGuard` stayed armed until `ArmStatus::Suspended`. Peer-close after successful `transfer_tx.send` Drop-deregistered the coordinator-owned wait.

**Fix:** `WaitCancelGuard` carries a shared `drop_armed` atomic. After `transfer_tx.send` succeeds, arm task stores `false` immediately so listener Drop is a no-op. Coordinator/`TransferredWait` owns cleanup.

**Regression:**
- `peer_close_after_transfer_before_ack_keeps_coordinator_wait` — suspend-entered proves transfer → abort process_status → registry still coordinator-owned → release → Waiting
- `drop_armed_flag_false_skips_drop_deregister` — unit-level Drop skip

## FIX wave 5 summary (prior)

ACK-prefer when both ready; install WaitCancelGuard immediately after register (before bind).

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

## Tests run (FIX wave 6, narrow filters only; 180s job kill)

```powershell
cargo test --features test-utils --lib continuation_coordinator_closed_before_ack_after_suspend_requested_preserves_waiting -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_ack_ready_beats_completion_closed -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_post_ack -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_after_transfer_before_ack_keeps_coordinator_wait -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_during_bind_deregisters_wait_registration -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_wait_cancel_after_suspend_control_preserves_waiting -- --nocapture --test-threads=1
cargo test --features test-utils --lib cancel_during_transfer_oneshot -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_waiter_close -- --nocapture --test-threads=1
cargo test --features test-utils --lib pre_suspension -- --nocapture --test-threads=1
cargo test --features test-utils --lib legacy_indefinite_registers -- --nocapture --test-threads=1
cargo test --features test-utils --lib drop_armed_flag_false_skips_drop_deregister -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_peer_close_during_suspend -- --nocapture --test-threads=1
```

**Results (all green under the filters above):**

| Filter | Count |
| --- | --- |
| `continuation_coordinator_closed_before_ack_after_suspend_requested_preserves_waiting` | 1 passed |
| `continuation_coordinator_ack_ready_beats_completion_closed` | 1 passed |
| `continuation_coordinator_post_ack*` | 4 passed |
| `peer_close_after_transfer_before_ack_keeps_coordinator_wait` | 1 passed |
| `peer_close_during_bind_deregisters_wait_registration` | 1 passed |
| `continuation_wait_cancel_after_suspend_control_preserves_waiting` | 1 passed |
| `cancel_during_transfer_oneshot*` | 1 passed |
| `continuation_coordinator_waiter_close*` | 2 passed |
| `pre_suspension*` | 3 passed |
| `legacy_indefinite_registers*` | 1 passed |
| `drop_armed_flag_false_skips_drop_deregister` | 1 passed |
| `continuation_peer_close_during_suspend*` | 1 passed |

## Self-review

- **Control-sent fence:** closed/cancel after `suspend_parent` await ACK → Waiting, not Failed orphan.
- **Pre-control cancel still works:** transfer-oneshot closed / cancel-before-dispatch paths still terminalize Arming (`cancel_during_transfer_oneshot`, pre-suspension suite green).
- **Transfer disarm:** `drop_armed=false` immediately after `transfer_tx.send`; peer-close Drop cannot deregister coordinator wait; pre-transfer peer-close during bind still Drop-cleans.
- **Wait cancel after control-sent:** MCP status returns timeout batch + registry clean; continuation remains Waiting with active worker.
- **No full cargo suite** (process: avoid hang; narrow filters + 180s kill).

## Concerns

1. **Worker cancel (`context.cancel`) after control-sent** now also awaits suspend rather than pre-failing. Parent stop/connection-exit still cancel workers and CAS the durable row separately; if `suspend_parent` hangs forever, the worker parks on that future until the port completes (same as production drain/timeout paths). No new timeout was added in this wave.
2. **Claim-wake errors after control-sent** still return early and drop the suspend future (pre-existing). Out of residual-race scope.
3. **Cancel after durable Waiting** still ends only the MCP status wait; continuation remains Waiting until Task 7/8 wake/cleanup — intentional.
4. **Pre-existing:** `continuation_cleanup_cancel_fences_before_first_suspension_dispatch` expects non-Failed after pre-suspension worker cancel, but wave-2 terminalizes Arming→Failed. Out of this fix scope.
5. **Bind soft-fail** still parks after tool-id/lease/bind notes (register is the only fail-closed path).
6. **600+600 wait-only** remains supervisor composition (Task 6 for full E2E).

## Out of scope (confirmed not done)

- Task 5 RunStore gate bounds
- Task 6 conversation 1570 full acceptance pack
- Push / PR
