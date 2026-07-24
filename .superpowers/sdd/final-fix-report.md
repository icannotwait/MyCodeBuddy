# Final branch review — fix report

**Branch:** `feat/popout-close-acp-keepalive`  
**Status:** REQUEST_CHANGES findings fixed  
**Date:** 2026-07-25

## Summary

All Critical / Important / Minor findings from the final branch review are fixed. Tests cover the Critical and Important behaviors. No FE reclaim change required (upgrade lands before `conversation-window://closed` emit, or via `commit_close_reverse` for late residual paths).

## Findings

### Critical 1 — Close-fenced cold connect hard-kill

**Problem:** `acp_connect` post-spawn fence path used `disconnect_if_owner`, which unconditionally tears down a busy agent if spawn finished after close registration wait / residual.

**Fix:** Route A residual via `close_fence_late_connect_reconcile` → `residual_reconcile_after_close` (stamped reverse-to-main + idle-only disconnect + terminal rebind). Never `disconnect_if_owner` on this path. Successful residual reverse upgrades stored outcome to `Reversed { gen }` when possible.

**Files:** `src-tauri/src/commands/acp.rs`, `src-tauri/src/commands/conversation_popout.rs`

### Important 2 — Idle residual permanent tool-lease fence

**Problem:** `disconnect_idle_by_owner_window_and_operation` called `clear_tool_leases` (fences admission) before the exclusive phase-3 idle pass. If phase 3 skipped (busy / no write lock), the surviving connection was permanently fenced and watchdog tool admission broke.

**Fix:** Clear/fence leases only after the exclusive idle pass decides to remove and the map entry is dropped. Skips leave admission open.

**Files:** `src-tauri/src/acp/manager.rs`

### Important 3 — Superseded before late reverse

**Problem:** Rebind-timeout / CAS race could commit `Superseded` first; late residual reverse success could not upgrade, so FE stayed non-reclaimable.

**Fix:**
1. `commit_close_reverse` upgrades `Superseded` → `Reversed { gen }` (same as `ReverseUncertain`).
2. `rebind_stamped_connections_owner_window` returns `(count, max_gen)`.
3. Residual returns max post-rebind gen; close handler and close-reserved forced-reverse path upgrade before publish / after residual.

**Files:** `src-tauri/src/commands/conversation_popout.rs`, `src-tauri/src/acp/manager.rs`

### Minor 4 — Design doc trailing whitespace

**Fix:** Stripped trailing spaces on design doc lines ~39, 125, 133, 141, 149 (and any other trailing whitespace in that file).

**Files:** `docs/superpowers/specs/2026-07-24-popout-close-acp-keepalive-design.md`

## Tests added/adjusted

| Test | Covers |
| --- | --- |
| `close_fence_late_connect_reconcile_keeps_busy_reverses_to_main` | Critical: busy late connect not hard-killed; reverse to main |
| `close_fence_late_connect_reconcile_moves_idle_off_closed_label` | Critical: idle leaves closed label via residual |
| `disconnect_idle_skip_does_not_permanently_fence_survivor` | Important: busy skip does not fence |
| `disconnect_idle_write_lock_skip_does_not_fence` | Important: write-lock skip does not fence |
| `disconnect_idle_success_fences_removed_connection` | Important: successful reap still fences |
| `commit_close_reverse_upgrades_superseded_to_reversed_with_gen` | Important: Superseded → Reversed |
| `residual_stamped_rebind_upgrades_superseded_outcome` | Important: residual reverse upgrades outcome |

## Verification

```text
cargo test --features test-utils --lib conversation_popout
# 46 passed

cargo test --features test-utils --lib disconnect_idle
# 6 passed
```

FE reclaim tests: not required — outcome upgrade happens server-side before closed emit / via existing `commit_close_reverse` upgrade path already covered by ReverseUncertain FE handling for `Reversed`.

## Commits

| Hash | Message |
| --- | --- |
| `2e649cbe` | `fix(popout): Route A close-fence residual, idle lease fence, Superseded upgrade` |
| `8d8f8f01` | `docs(sdd): final-fix-report for popout close review fixes` |

## Residual risk

- Idle connections reverse to `main` with op stamp retained (v1 design); subject to existing idle sweep, not close residual after reverse.
- Permanent fence is still applied for successful removes only; intentional for disconnect paths that commit teardown.
