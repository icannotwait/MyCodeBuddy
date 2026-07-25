# Task 4 Report: Canonical arm path, typed bind outcomes, transfer ownership

## Status

**DONE** (FIX wave 3: post-suspend_ack cancel race before Waiting CAS)

## Commits

| Hash | Message |
| --- | --- |
| `fd8a3bfcc7ba7c0642a0d69369cca308642d3c50` | `feat(delegation): arm indefinite waits with exact tool id and transferable ownership` |
| `aa12b38e…` | `fix(delegation): terminalize arming when wait transfer oneshot closes` |
| `a1e3d1f0f582516ceb86ccbcce2762e55ab62858` | `fix(delegation): abort arm task on wait cancel before transfer/suspend` |
| `24ab838ac0b3` | `fix(delegation): fence post-suspend-ack cancel before Waiting CAS` |

## Files changed (FIX wave 3)

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/continuation/coordinator.rs` | Post-ack cancel fence before Waiting/WakePending+`suspended_at` CAS; cancel-aware `select!` around those CAS futures so in-flight store awaits (incl. test gates) abort before commit; post-CAS cancel re-check if CAS arm wins with cancel also ready |
| `src-tauri/src/acp/delegation/continuation/tests.rs` | `ObservedStore::install_waiting_cas_gate`; regression `continuation_coordinator_post_ack_cancel_before_waiting_cas_terminalizes` |

## FIX wave 3 summary

### Important — post-suspend_ack cancel before Waiting CAS

**Bug:** After `suspend_parent` resolves `Ok(ack)`, the worker could CAS Arming→Waiting and publish suspended state even when wait-cancel had already aborted `arm_task` (completion receiver gone). Failed `completion.send(Ok(ack))` was ignored, leaving an active Waiting continuation for a canceled wait.

**Fix (coordinator):**
1. Immediate fence after ack identity check: if `context.cancel` or `completion.is_closed()`, `fail_before_suspension` / `fail_waiter_gone_before_suspension` (no Waiting).
2. Waiting and WakePending+`suspended_at` CAS wrapped in `select!` with **CAS result first** (biased): cancel/`closed` only wins while CAS is still pending, so a gated in-flight CAS is dropped before commit.
3. If CAS completes in the same poll as cancel, post-CAS re-check terminalizes the new row (still no stuck active continuation).

**Regression:**
- `continuation_coordinator_post_ack_cancel_before_waiting_cas_terminalizes` — CAS gate after suspend ack, drop completion while gate held: Failed/ArmFailed, no `suspended_at`, slot free, child still Running, worker gone.

## FIX wave 2 summary (prior)

### Important — cancel vs transfer mutual exclusion

**Bug:** Cancel path dropped `JoinHandle` without abort → detached arm could still transfer/suspend.

**Fix:** listener abort+await arm_task; coordinator pre-suspension cancel/`closed` terminalizes Arming.

## Original implementation summary (main Task 4 commit)

1. **Broker preflight** — `Ready` vs `NeedPark { canonical_task_ids }`.
2. **Canonical arm helper** — exact tool id, register canonical task ids, typed bind, cancel-aware park.
3. **Continuation transfer barrier** — oneshot `TransferredWait`; failed transfer drops tx without send.

## Tests run (FIX wave 3, narrow filters only; 180s job kill)

```powershell
cargo test --features test-utils --lib continuation_coordinator_post_ack -- --nocapture --test-threads=1
cargo test --features test-utils --lib cancel_during_transfer_oneshot -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_wait_cancel_after_arming -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_waiter_close -- --nocapture --test-threads=1
cargo test --features test-utils --lib pre_suspension -- --nocapture --test-threads=1
cargo test --features test-utils --lib suspend_drain_timeout_stays_owned -- --nocapture --test-threads=1
cargo test --features test-utils --lib parent_connection_loss_stays_owned -- --nocapture --test-threads=1
cargo test --features test-utils --lib parent_stop_rejection_stays_owned -- --nocapture --test-threads=1
cargo test --features test-utils --lib delegation_continuation_e2e_peer_close -- --nocapture --test-threads=1
```

**Results (all green):**

| Filter | Count |
| --- | --- |
| `continuation_coordinator_post_ack*` | 4 passed (incl. new cancel-before-Waiting) |
| `cancel_during_transfer_oneshot*` | 1 passed |
| `continuation_wait_cancel_after_arming*` | 1 passed |
| `continuation_coordinator_waiter_close*` | 2 passed |
| `pre_suspension*` | 3 passed |
| `*_stays_owned*` (drain/connection/stop) | 3 passed |
| `delegation_continuation_e2e_peer_close*` | 2 passed |

## Self-review

- **Post-ack fence:** cancel/`closed` after suspend ack cannot leave durable Waiting.
- **CAS-gated race:** holding Arming→Waiting store gate + dropping completion is deterministic; select drops in-flight CAS before commit.
- **Wait-only:** terminalize uses `fail_before_suspension` / ArmFailed — no Broker-cancel of children.
- **Peer-close / Task-8 retain paths** unchanged (drain timeout, connection loss, stop rejection still stay owned).
- **No full cargo suite** (process: avoid hang; narrow filters + 180s kill).

## Concerns

1. **Cancel after durable Waiting** still ends only the MCP status wait (Suspended loop); continuation remains Waiting until Task 7/8 wake/cleanup — intentional and out of this fix scope.
2. **Parent already suspended when fence fires:** post-ack terminalize fails the continuation without a dedicated unsuspend path; same class as other post-suspend failures (`fail_after_suspension` for StateConflict still cancels children; wait-cancel path deliberately does not).
3. **Simultaneous CAS-complete + cancel:** post-CAS re-check terminalizes after a brief Waiting row (then Failed); primary CAS-gated path never commits Waiting.
4. **600+600 wait-only** remains supervisor composition (Task 6 for full E2E).

## Out of scope (confirmed not done)

- Task 5 RunStore gate bounds
- Task 6 conversation 1570 full acceptance pack
- Push / PR
