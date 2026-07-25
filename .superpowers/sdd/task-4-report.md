# Task 4 Report: Canonical arm path, typed bind outcomes, transfer ownership

## Status

**DONE** (FIX wave 7: parent cleanup cancel vs ACK wait + transfer handoff linearize)

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
| `7f1a9dfff40fc8054c8e87b395750d59c3a684cf` | `fix(delegation): split cleanup cancel from ACK wait; linearize handoff` |
| *(docs commits after code fix)* | `docs(sdd): update task-4 report for residual race wave 7` |

**Wave 7 code fix:** `7f1a9dfff40fc8054c8e87b395750d59c3a684cf`  
**Base before this wave:** `0a5aa27f64446e5ff8beb0b54e0a5c964bb92be7` (wave-6 docs tip; code base `21b3f648`)  
**Branch tip:** run `git rev-parse HEAD` (includes docs commits after the code fix).

## Files changed (FIX wave 7)

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/continuation/coordinator.rs` | After control-sent: `context.cancel` aborts with `ArmWorkerDropped` (no hang on ACK); `completion.closed` still awaits ACK / preserves Waiting |
| `src-tauri/src/acp/delegation/listener.rs` | Pre-send coordinator-owner check + transfer handoff gate observe; gated peer-close-before-send regression |
| `src-tauri/src/acp/delegation/wait_cancel.rs` | `deregister_if_owner` + owner-aware `WaitCancelGuard` Drop; transfer handoff gate; unit + residual-window tests |

## FIX wave 7 summary

### Important 1 — parent cleanup cancel regression

**Bug:** Wave 6 made both `context.cancel` and `completion.closed` await the in-flight suspend ACK. Parent cleanup (`cancel_workers_for_parent`) then hung on gated/unreleased suspend, timing out `continuation_cleanup_cancel_fences_direct_claimed_suspension_await` at 2s.

**Fix:** Split paths after `suspend_parent` is in flight:
- **Parent cancel (`context.cancel`):** drop in-flight suspend, send `ArmWorkerDropped`, return. Leave durable row non-terminal for parent stop/exit CAS. Never hang forever on ACK; never invent Failed via `fail_before_suspension`.
- **Waiter close (`completion.closed`):** still prefer ACK / preserve Waiting (wave 6 intent unchanged).

**Regression:**
- `continuation_cleanup_cancel_fences_direct_claimed_suspension_await` — claimed suspend gate + cancel without release → ArmWorkerDropped within 2s, non-terminal durable, worker gone
- `continuation_coordinator_closed_before_ack_after_suspend_requested_preserves_waiting` — closed-only still reaches Waiting
- `continuation_coordinator_ack_ready_beats_completion_closed` — both-ready still prefers ACK

### Important 2 — peer close between Arming and transfer_tx.send

**Bug:** After `transfer_owner` to ContinuationCoordinator but before `transfer_tx.send` / `drop_armed=false`, peer-close Drop could still deregister the wait while the detached arm task continued the handoff.

**Fix:** Handoff linearizable at `transfer_owner`:
1. `WaitCancelGuard` Drop uses `deregister_if_owner(..., Listener)` — post-transfer coordinator rows are not removed.
2. Arm path re-checks owner is ContinuationCoordinator before `transfer_tx.send`; abort without send if missing.
3. Keep post-send `drop_armed=false` as defense-in-depth.
4. Test-only `install_transfer_handoff_gate` parks between transfer and send.

**Regression:**
- `peer_close_between_transfer_owner_and_send_keeps_coordinator_wait` — gate after transfer_owner → abort process_status → registry stays coordinator-owned → release send → Waiting
- `drop_after_transfer_owner_before_send_preserves_coordinator_wait` — unit-level owner-aware Drop
- `peer_close_after_transfer_before_ack_keeps_coordinator_wait` — post-send path still green
- `peer_close_during_bind_deregisters_wait_registration` — pre-transfer Listener Drop still cleans

## FIX wave 6 summary (prior)

Closed-before-ACK after control-sent (await ACK, never pre-fail); transfer disarm on `transfer_tx.send`.

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

## Tests run (FIX wave 7, narrow filters only; 180s job kill)

```powershell
cargo test --features test-utils --lib continuation_cleanup_cancel_fences_direct_claimed_suspension_await -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_closed_before_ack -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_ack_ready_beats -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_post_ack -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_after_transfer_before_ack -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_between_transfer_owner_and_send -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_during_bind_deregisters -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_wait_cancel_after_suspend_control -- --nocapture --test-threads=1
cargo test --features test-utils --lib cancel_during_transfer_oneshot -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_waiter_close -- --nocapture --test-threads=1
cargo test --features test-utils --lib drop_armed_flag_false -- --nocapture --test-threads=1
cargo test --features test-utils --lib drop_after_transfer_owner_before_send -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_peer_close_during_suspend -- --nocapture --test-threads=1
cargo test --features test-utils --lib pre_suspension -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_cleanup_cancel_fences_attempt_zero -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_cleanup_cancel_fences_wake_pending -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_cleanup_cancel_fences_retry -- --nocapture --test-threads=1
```

**Results (all green under the filters above):**

| Filter | Count |
| --- | --- |
| `continuation_cleanup_cancel_fences_direct_claimed_suspension_await` | 1 passed |
| `continuation_coordinator_closed_before_ack*` | 1 passed |
| `continuation_coordinator_ack_ready_beats*` | 1 passed |
| `continuation_coordinator_post_ack*` | 4 passed |
| `peer_close_after_transfer_before_ack*` | 1 passed |
| `peer_close_between_transfer_owner_and_send*` | 1 passed |
| `peer_close_during_bind_deregisters*` | 1 passed |
| `continuation_wait_cancel_after_suspend_control*` | 1 passed |
| `cancel_during_transfer_oneshot*` | 1 passed |
| `continuation_coordinator_waiter_close*` | 2 passed |
| `drop_armed_flag_false*` | 1 passed |
| `drop_after_transfer_owner_before_send*` | 1 passed |
| `continuation_peer_close_during_suspend*` | 1 passed |
| `pre_suspension*` | 3 passed |
| `continuation_cleanup_cancel_fences_attempt_zero*` | 1 passed |
| `continuation_cleanup_cancel_fences_wake_pending*` | 1 passed |
| `continuation_cleanup_cancel_fences_retry*` | 1 passed |

## Self-review

- **Cancel vs closed split:** parent cleanup cancel unblocks without ACK hang; waiter closed-before-ACK still commits Waiting.
- **Pre-control cancel:** transfer-oneshot closed / pre-suspension suite still terminalize Arming as before.
- **Handoff linearize:** ownership flips at `transfer_owner`; Drop cannot reclaim coordinator rows; pre-send owner check aborts if registration vanished; post-send disarm retained.
- **Pre-transfer peer-close:** Listener-owned Drop still deregisters (bind-window test green).
- **No full cargo suite** (process: avoid hang; narrow filters + 180s kill).

## Concerns

1. **Parent cancel after control-sent** drops the in-flight `suspend_parent` future. Parent stop/connection-exit still CAS durable cleanup separately; production ports should tolerate dropped suspend futures during parent teardown. No new ACK timeout was added — cancel is immediate abort, not bounded settle.
2. **Claim-wake errors after control-sent** still return early and drop the suspend future (pre-existing). Out of residual-race scope.
3. **Cancel after durable Waiting** still ends only the MCP status wait; continuation remains Waiting until Task 7/8 wake/cleanup — intentional.
4. **Pre-existing:** `continuation_cleanup_cancel_fences_before_first_suspension_dispatch` expects non-Failed after pre-suspension worker cancel, but wave-2 terminalizes Arming→Failed. Still out of this fix scope (confirmed still failing under that filter).
5. **Bind soft-fail** still parks after tool-id/lease/bind notes (register is the only fail-closed path).
6. **600+600 wait-only** remains supervisor composition (Task 6 for full E2E).
7. **Transfer handoff gate** is test-utils only; production path has no artificial park between transfer and send.

## Out of scope (confirmed not done)

- Task 5 RunStore gate bounds
- Task 6 conversation 1570 full acceptance pack
- Push / PR
