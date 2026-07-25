# Final Review Fix Report — Delegation Wait Watchdog Correlation

- **Date**: 2026-07-25
- **Branch**: `feat/delegation-wait-watchdog-correlation`
- **Worktree**: `D:\MyCodeBuddy\.worktrees\delegation-wait-watchdog-correlation`
- **Status**: **DONE** (Important 1–3, Minor gate-entry bound; residual rewrite-id trim closed)

## Commits

| Hash | Message |
| --- | --- |
| `98352577e1d2fc0dbc665943fd1967e71a542b45` | `fix(delegation): align wait tool id bytes and peer-close deregister after transfer` |
| `f886a00bdf60941664a31ffddc5eb90f11192a2b` | `fix(delegation): preserve rewrite wait tool id bytes for lease align` |
| `a975afa0810f3e4d666340c8ff5bca08f2e73e77` | `docs(sdd): update final-fix-report for rewrite tool id byte preserve` |

**Base tip before final residual fix:** `feb59afa0b9580934b1ffd7acc1d51791f7bc78f`  
**Code tip:** `f886a00bdf60941664a31ffddc5eb90f11192a2b`  
**Push:** none (local only).

## Files changed (residual Important 3)

| File | Change |
| --- | --- |
| `src-tauri/src/acp/delegation/listener.rs` | `resolve_wait_tool_id` no longer trims rewrite/fallback ids; trim only rejects blank; unit coverage for padded rewrite |
| `src-tauri/src/acp/manager.rs` | `padded_rewrite_tool_id_bind_and_renewal_align_lease_keys` regression (rewrite → bind → renew) |

## Important 1 — wait tool id trim vs bind (prior)

**Bug:** `exact_match_progress_targets` returned **trimmed** `parent_tool_use_id` while bind/lease lookup uses **raw** host bytes (trim only to reject blank). Whitespace-padded ids could bind a lease then never renew.

**Fix:** Keep original bytes after non-blank check:

```rust
entry.stamp.parent_tool_use_id
    .as_ref()
    .filter(|s| !s.trim().is_empty())
    .cloned()
```

**Regression:** `exact_match_preserves_whitespace_padded_wait_tool_id_bytes`

## Important 2 — peer-close after transfer left registration (prior)

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

## Important 3 — resolve_wait_tool_id trimmed rewrite/fallback ids (this fix)

**Bug:** Request-carried `_meta` ids already preserved original bytes, but identity-less **rewrite/fallback** ids went through `.map(str::trim)` before becoming the wait stamp `parent_tool_use_id`. Host leases are keyed by **raw** rewrite id bytes (`tool_lease_key`). A padded rewrite id would:

1. Register lease under `"  rewrite-id  "`
2. Resolve to trimmed `"rewrite-id"` in the wait stamp
3. `bind_delegation_wait` lookup miss → `WaitToolLeaseMismatch`
4. Even if bound somehow, renew `exact_match` / progress keys would diverge

**Fix:** Trim only to reject blank / whitespace-only rewrite ids; keep original bytes otherwise:

```rust
// Nonblank request id: original bytes
if !req.parent_tool_use_id.trim().is_empty() {
    return Some(req.parent_tool_use_id.clone());
}
// Rewrite/fallback: reject blank only; preserve original bytes
rewritten_status_tool_id
    .filter(|s| !s.trim().is_empty())
    .map(|s| s.to_string())
```

**Regressions:**
- `resolve_wait_tool_id_request_over_rewrite` (extended: padded rewrite preserve, blank reject, ws-only request falls through)
- `padded_rewrite_tool_id_bind_and_renewal_align_lease_keys` (padded rewrite → Bound on raw key; trimmed → LeaseMismatch; exact_match + renew keep Running)

## Minor — gate-entry bound (prior)

`continue_and_replacement_admission_cannot_revive_a_superseded_child` used bare `entered_rx.await`. Wrapped with `tokio::time::timeout(TEST_RUN_STORE_GATE_TIMEOUT, entered_rx)` (5s), matching other RunStore gate tests.

## Tests run (narrow filters; ~180s budget)

```powershell
cd src-tauri
cargo test --features test-utils --lib resolve_wait_tool_id -- --test-threads=1
cargo test --features test-utils --lib padded_rewrite_tool_id -- --test-threads=1
cargo test --features test-utils --lib exact_match -- --test-threads=1
cargo test --features test-utils --lib bind_delegation_wait -- --test-threads=1
cargo test --features test-utils --lib wait_cancel -- --test-threads=1
cargo test --features test-utils --lib peer_close_after_transfer -- --test-threads=1
cargo test --features test-utils --lib peer_close_between_transfer -- --test-threads=1
cargo test --features test-utils --lib peer_close_during_bind -- --test-threads=1
cargo test --features test-utils --lib continuation_peer_close -- --test-threads=1
cargo test --features test-utils --lib continuation_status_peer_close -- --test-threads=1
cargo test --features test-utils --lib incident_1570 -- --test-threads=1
cargo test --features test-utils --lib conversation_1570 -- --test-threads=1
cargo test --features test-utils --lib attribution_activity -- --test-threads=1
cargo test --features test-utils --lib armed_wait_600s -- --test-threads=1
```

| Filter | Result |
| --- | --- |
| `resolve_wait_tool_id` | **1 passed** (padded rewrite preserve) |
| `padded_rewrite_tool_id` | **1 passed** (rewrite→bind→renew) |
| `exact_match` | **6 passed** (incl. whitespace padded id) |
| `bind_delegation_wait` | **4 passed** |
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

**No failures.** No push.

## Self-review

- **Important 1:** Renew keys match bind/lease opaque host bytes; blank still rejected.
- **Important 2:** Peer-close after transfer deregisters wait only; durable continuation still reaches Waiting; no Broker child cancel.
- **Important 3:** Rewrite/fallback wait tool ids keep original bytes end-to-end with lease keys; blank-only reject; bind+renew regression proves padded vs trimmed divergence.
- **Minor:** Gate-entry fail-fast aligned with 5s RunStore gate budget.
- **Scope held:** no frontend/MCP schema/default watchdog duration changes; no push/PR.

## Residual notes

1. Residual transfer→send window still relies on arm_task completing send after peer-close detaches it; once `TransferredWait` is built with an already-cancelled `waiter_closed`, cleanup deregisters immediately. Tests cover that path.
2. Listener Suspended path may still explicit-deregister on host cancel; concurrent with `TransferredWait` cleanup is idempotent (`NotFound` / stamp match).
3. `WaitCancelGuard` remains owner-aware (Listener-only Drop) so residual transfer ownership stays linearizable at `transfer_owner`.
4. `manager::bind_delegation_wait` still trims only for blank rejection and capability equality check after exact key lookup — lease index remains raw-byte keyed.
