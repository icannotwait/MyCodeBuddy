# Task 4 Report: Canonical arm path, typed bind outcomes, transfer ownership

## Status

**DONE** (FIX wave 4: post-ack cancel must use post-suspension ownership)

## Commits

| Hash | Message |
| --- | --- |
| `fd8a3bfcc7ba7c0642a0d69369cca308642d3c50` | `feat(delegation): arm indefinite waits with exact tool id and transferable ownership` |
| `aa12b38e…` | `fix(delegation): terminalize arming when wait transfer oneshot closes` |
| `a1e3d1f0f582516ceb86ccbcce2762e55ab62858` | `fix(delegation): abort arm task on wait cancel before transfer/suspend` |
| `0f413bdc3ff71a67a4572b4d8651194b552d2a7e` | `fix(delegation): fence post-suspend-ack cancel before Waiting CAS` |
| `6d09d69c` | `fix(delegation): post-ack cancel preserves resumable Waiting` |

## Files changed (FIX wave 4)

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/continuation/coordinator.rs` | After successful `suspend_parent` ack, never use `fail_before_suspension` / `fail_cancelled_before_suspension` for cancel/closed; always commit durable Waiting / WakePending+`suspended_at`; true post-suspend failures stay on `fail_after_suspension` |
| `src-tauri/src/acp/delegation/continuation/tests.rs` | Rewrite regression as `continuation_coordinator_post_ack_cancel_preserves_resumable_waiting` |
| `src-tauri/src/acp/delegation/listener.rs` | Fail closed on `wait_cancel.register` error (do not park without cancel handle) |

## FIX wave 4 summary

### Important — post-ack must not treat already-suspended parent as pre-suspension

**Bug:** Wave 3 post-ack fence used `fail_before_suspension` after successful suspend ack. By then the connection has already cleared the parent turn. Terminalizing via the pre-suspension path left **Failed with no resumable continuation** while children kept running.

**Fix (coordinator):**
1. Keep all **pre-ack** cancel fences (before / during `suspend_parent`) that prevent phantom Waiting when ack never lands.
2. Once `Ok(ack)` is accepted, ownership is **post-suspension**: always CAS to durable Waiting (or WakePending+`suspended_at`).
3. Cancel/closed after ack follows Waiting-loop semantics (worker returns on cancel token; wait-cancel does not Broker-cancel children; continuation stays resumable).
4. True post-suspend failures (CAS conflict, identity drift, publish failure) still use `fail_after_suspension`.

**Regression:**
- `continuation_coordinator_post_ack_cancel_preserves_resumable_waiting` — CAS gate after suspend ack, drop completion while gate held, release gate: Waiting + `suspended_at`, active slot retained, children Running, worker owned; not Failed/ArmFailed via pre-suspension.

### Minor — register fail-closed

Failed `wait_cancel.register` returns Unavailable/legacy unknown batch immediately and does not park without a live cancel handle.

## FIX wave 3 summary (prior)

Post-ack cancel fence before Waiting CAS (incorrectly used pre-suspension failure — corrected in wave 4).

## FIX wave 2 summary (prior)

Cancel vs transfer mutual exclusion: listener abort+await arm_task; pre-suspension cancel/`closed` terminalizes Arming.

## Original implementation summary (main Task 4 commit)

1. **Broker preflight** — `Ready` vs `NeedPark { canonical_task_ids }`.
2. **Canonical arm helper** — exact tool id, register canonical task ids, typed bind, cancel-aware park.
3. **Continuation transfer barrier** — oneshot `TransferredWait`; failed transfer drops tx without send.

## Tests run (FIX wave 4, narrow filters only; 180s job kill)

```powershell
cargo test --features test-utils --lib continuation_coordinator_post_ack -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_wait_cancel_after_arming -- --nocapture --test-threads=1
cargo test --features test-utils --lib cancel_during_transfer_oneshot -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_coordinator_waiter_close -- --nocapture --test-threads=1
cargo test --features test-utils --lib pre_suspension -- --nocapture --test-threads=1
cargo test --features test-utils --lib suspend_drain_timeout_stays_owned -- --nocapture --test-threads=1
cargo test --features test-utils --lib parent_connection_loss_stays_owned -- --nocapture --test-threads=1
cargo test --features test-utils --lib parent_stop_rejection_stays_owned -- --nocapture --test-threads=1
cargo test --features test-utils --lib delegation_continuation_e2e_peer_close -- --nocapture --test-threads=1
```

**Results (all green under the filters above):**

| Filter | Count |
| --- | --- |
| `continuation_coordinator_post_ack*` | 4 passed (incl. rewritten preserve-Waiting) |
| `continuation_wait_cancel_after_arming*` | 1 passed |
| `cancel_during_transfer_oneshot*` | 1 passed |
| `continuation_coordinator_waiter_close*` | 2 passed |
| `pre_suspension*` | 3 passed |
| `*_stays_owned*` (drain/connection/stop) | 3 passed |
| `delegation_continuation_e2e_peer_close*` | 2 passed |

## Self-review

- **Post-ack ownership:** cancel/`closed` after suspend ack cannot Failed-terminalize via pre-suspension path.
- **Resumable continuation:** Waiting + `suspended_at` remains so an already-ended parent turn can still be resumed when children finish.
- **Wait-only:** children stay Running; no Broker-cancel on wait-cancel.
- **Pre-ack fences** unchanged (still prevent phantom Waiting before ack).
- **Peer-close / Task-8 retain paths** unchanged.
- **No full cargo suite** (process: avoid hang; narrow filters + 180s kill).

## Concerns

1. **Cancel after durable Waiting** still ends only the MCP status wait; continuation remains Waiting until Task 7/8 wake/cleanup — intentional.
2. **Pre-existing:** `continuation_cleanup_cancel_fences_before_first_suspension_dispatch` expects non-Failed after pre-suspension worker cancel, but wave-2 `fail_cancelled_before_suspension` terminalizes Arming→Failed. Failed on HEAD before wave 4; out of this fix scope.
3. **Bind soft-fail** still parks after tool-id/lease/bind notes (register is the only fail-closed path in this wave).
4. **600+600 wait-only** remains supervisor composition (Task 6 for full E2E).

## Out of scope (confirmed not done)

- Task 5 RunStore gate bounds
- Task 6 conversation 1570 full acceptance pack
- Push / PR
