# Final branch review — wave2 fix report

**Branch:** `feat/popout-close-acp-keepalive`  
**Status:** Critical race fixed  
**Date:** 2026-07-25

## Critical race

**Bug:** Late `record_rebind` close-reserved path committed `ConnectionGone` via `commit_close_reverse` **before** residual stamped rebind. That commit clears `rebind_in_flight`, so the close handler could observe `Done(ConnectionGone)` and emit `conversation-window://closed` with non-reclaimable `ConnectionGone` while residual later moved ownership to main and upgraded to `Reversed`. FE treats `ConnectionGone` as terminal immediately.

**Root cause:** Residual-after-commit ordering in the close-reserved forced-reverse path.

## Fix

1. **Order (late `record_rebind` close-reserved):** Run `residual_reconcile_after_close` **before** `commit_close_reverse`. Keep `rebind_in_flight` set through residual.
2. **Outcome selection:** If residual returns `Some(max_gen)` (`rebound_count > 0`), commit `Reversed { max_gen }` instead of forced-primary `ConnectionGone` / Uncertain / Superseded (`close_reserved_outcome_after_residual`).
3. **Close emit harden:** If about to publish `ConnectionGone`, re-check live status for `Reversed` and scan main for residual with matching op (`max_ownership_generation_for_owner_operation`); upgrade via `commit_close_reverse` when allowed.

**Files:**
- `src-tauri/src/commands/conversation_popout.rs`
- `src-tauri/src/acp/manager.rs`

## Tests added

| Test | Covers |
| --- | --- |
| `close_reserved_outcome_prefers_residual_reversed_over_connection_gone` | Pure rule: residual gen wins over ConnectionGone |
| `late_record_rebind_close_reserved_residual_before_commit_order` | Residual while `rebind_in_flight`; single commit is Reversed; decide_close sees Reversed |
| `upgrade_connection_gone_before_emit_uses_main_residual` | Harden: ConnectionGone + residual already on main → Reversed |
| `upgrade_connection_gone_before_emit_prefers_live_reversed_status` | Harden: stale snapshot yields to live Reversed status |

## Verification

```text
cargo test --features test-utils --lib conversation_popout
# 52 passed; 0 failed
```

## Commit

- `75dcfb68` — `fix(popout): residual before ConnectionGone commit on close-reserved rebind`
