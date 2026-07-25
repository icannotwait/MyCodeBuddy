# Final Review Fix Report — Delegation Wait Watchdog Correlation

- **Date**: 2026-07-25
- **Branch**: `feat/delegation-wait-watchdog-correlation`
- **Worktree**: `D:\MyCodeBuddy\.worktrees\delegation-wait-watchdog-correlation`
- **Status**: **DONE** (Important 1, Important 2, Minor gate-entry bound)

## Commits

| Hash | Message |
| --- | --- |
| `98352577e1d2fc0dbc665943fd1967e71a542b45` | `fix(delegation): align wait tool id bytes and peer-close deregister after transfer` |
| `96f27ec7cebfdf4f9553298447797883ded96e6e` | `docs(sdd): pin final-fix-report commit hash` |

**Base tip before this fix:** `eeca2b9694ddfb75a0c3508ce92cdc3d6e67edad`  
**Code tip:** `98352577e1d2fc0dbc665943fd1967e71a542b45`  
**Branch tip:** `96f27ec7cebfdf4f9553298447797883ded96e6e`  
**Push:** none (local only).

## Files changed

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/wait_cancel.rs` | `exact_match_progress_targets` preserves original wait tool id bytes (trim only for blank); `TransferredWait` watches `cancel_rx` + `waiter_closed` and deregisters without cancelling continuation; regression tests |
| `src-tauri/src/acp/delegation/listener.rs` | Pass `waiter_closed` into `TransferredWait::new`; peer-close-after-transfer tests expect deregister + durable Waiting |
| `src-tauri/src/acp/delegation/continuation/coordinator.rs` | Comment: transferred wait cleanup contract |
| `src-tauri/src/acp/delegation/run_store.rs` | Bound gate-entry `entered_rx` await to `TEST_RUN_STORE_GATE_TIMEOUT` (5s) in continue/replacement race test |

## Important 1 — wait tool id trim vs bind

**Bug:** `exact_match_progress_targets` returned **trimmed** `parent_tool_use_id` while bind/lease lookup uses **raw** host bytes (trim only to reject blank). Whitespace-padded ids could bind a lease then never renew.

**Fix:** Keep original bytes after non-blank check:

```rust
entry.stamp.parent_tool_use_id
    .as_ref()
    .filter(|s| !s.trim().is_empty())
    .cloned()
```

**Regression:** `exact_match_preserves_whitespace_padded_wait_tool_id_bytes`

## Important 2 — peer-close after transfer left registration

**Bug:** After transfer to `ContinuationCoordinator`, status peer-close:
- disarmed `WaitCancelGuard` (owner-aware Drop no-op);
- cancelled `waiter_closed` only (coordinator checks it only pre-insert);
- held `TransferredWait` without consuming cancel/abandonment.

Result: durable continuation survived (correct) but wait registration leaked (incorrect renew/zombie entry).

**Fix:** `TransferredWait` spawns a background cleanup that **deregisters only** when:
1. `waiter_closed` is cancelled (status abandonment / peer-close), or
2. `cancel_rx` observes a cancel cause (host / watchdog wait cancel).

Does **not** cancel Broker children or the continuation worker token. Drop still deregisters if still armed on worker exit. `disarm_cleanup` aborts the watch.

Listener passes the status `waiter_closed` clone into `TransferredWait::new` at transfer send.

**Regressions:**
- `transferred_wait_waiter_closed_deregisters_without_cancel_cause`
- `transferred_wait_cancel_rx_deregisters_registration`
- `peer_close_after_transfer_before_ack_deregisters_keeps_continuation` (renamed/updated)
- `peer_close_between_transfer_owner_and_send_deregisters_keeps_continuation` (renamed/updated)

## Minor — gate-entry bound

`continue_and_replacement_admission_cannot_revive_a_superseded_child` used bare `entered_rx.await`. Wrapped with `tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, entered_rx)` (5s), matching other RunStore gate tests.

## Tests run (narrow filters; ~180s budget)

```powershell
cd src-tauri
cargo test --features test-utils --lib exact_match -- --nocapture --test-threads=1
cargo test --features test-utils --lib wait_cancel -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_after_transfer -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_between_transfer -- --nocapture --test-threads=1
cargo test --features test-utils --lib peer_close_during_bind -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_peer_close -- --nocapture --test-threads=1
cargo test --features test-utils --lib continuation_status_peer_close -- --nocapture --test-threads=1
cargo test --features test-utils --lib incident_1570 -- --nocapture --test-threads=1
cargo test --features test-utils --lib conversation_1570 -- --nocapture --test-threads=1
cargo test --features test-utils --lib attribution_activity -- --nocapture --test-threads=1
cargo test --features test-utils --lib armed_wait_600s -- --nocapture --test-threads=1
cargo test --features test-utils --lib bind_delegation_wait -- --nocapture --test-threads=1
cargo test --features test-utils --lib parent_cancel_while_settling -- --nocapture --test-threads=1
cargo test --features test-utils --lib cannot_revive_a_superseded -- --nocapture --test-threads=1
cargo test --features test-utils --lib settle_gate -- --nocapture --test-threads=1
cargo test --features test-utils --lib continue_admission_gate -- --nocapture --test-threads=1
cargo test --features test-utils --lib drop_after_transfer -- --nocapture --test-threads=1
```

| Filter | Result |
| --- | --- |
| `exact_match` | **6 passed** (incl. whitespace padded id) |
| `wait_cancel` | **25 passed** (incl. TransferredWait cleanup tests) |
| `peer_close_after_transfer*` | **1 passed** |
| `peer_close_between_transfer*` | **1 passed** |
| `peer_close_during_bind*` | **1 passed** |
| `continuation_peer_close*` | **2 passed** |
| `continuation_status_peer_close*` | **1 passed** |
| `incident_1570` | **2 passed** |
| `conversation_1570` | **1 passed** |
| `attribution_activity` | **3 passed** |
| `armed_wait_600s` | **1 passed** |
| `bind_delegation_wait` | **4 passed** |
| `parent_cancel_while_settling` | **1 passed** |
| `cannot_revive_a_superseded` | **1 passed** |
| `settle_gate` | **5 passed** |
| `continue_admission_gate` | **2 passed** |
| `drop_after_transfer*` | **1 passed** |

**No failures.** No push.

## Self-review

- **Important 1:** Renew keys now match bind/lease opaque host bytes; blank still rejected.
- **Important 2:** Peer-close after transfer deregisters wait only; durable continuation still reaches Waiting; no Broker child cancel.
- **Minor:** Gate-entry fail-fast aligned with 5s RunStore gate budget.
- **Scope held:** no frontend/MCP schema/default watchdog duration changes; no push/PR.

## Residual notes

1. Residual transfer→send window still relies on arm_task completing send after peer-close detaches it; once `TransferredWait` is built with an already-cancelled `waiter_closed`, cleanup deregisters immediately. Tests cover that path.
2. Listener Suspended path may still explicit-deregister on host cancel; concurrent with `TransferredWait` cleanup is idempotent (`NotFound` / stamp match).
3. `WaitCancelGuard` remains owner-aware (Listener-only Drop) so residual transfer ownership stays linearizable at `transfer_owner`.
