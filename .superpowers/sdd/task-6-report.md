# Task 6 Report — Narrow Cancellation and Guaranteed Convergence

**Status:** DONE (P1 review fixes applied)  
**Branch:** `feat/tool-execution-watchdog`  
**Base commit:** `35900769` — `feat(acp): converge stalled tools through scoped cancellation`  
**Fix commit:** (this commit) — `fix(acp): wire production cancel host and full-stamp wait cancel`  
**Date:** 2026-07-23

## Summary

Task 6 implements host-only scoped cancellation and 10s/10s escalation for
tool-execution leases. Review FAIL (P1) findings are addressed below so the
executor is reachable on real connections, wait stamps are fully validated,
peer-close cleans up, and timeout/user-stop semantics stay distinct.

## Review fix summary (P1 + P2)

### P1 — Production CancelHost + executor entry

- Added `ProductionCancelHost` implementing `CancelHost` via
  `admit_cancel_terminal_if_current`, host Broker cancel, full-stamp wait
  cancel, MCP cancel, timeout-aware turn cancel, and
  `disconnect_if_incarnation`.
- Public entries on `ConnectionManager`:
  - `production_cancel_host()`
  - `escalate_claimed_lease(claim, convergence)`
  - `scan_and_execute_cancellations(at, convergence)` (scan + ClaimCancel
    execute; Task 7 still owns settings/scheduling of the periodic loop)

### P1 — Full WaitStamp validation

- Listener registers real `connection_incarnation`, `turn_generation`, and
  `parent_tool_use_id` from `ParentSessionLookup::parent_wait_context` (plus
  identityless rewrite tool id when available).
- Manager cancel uses `WaitCancelRegistry::cancel(full stamp, cause)` built
  via `wait_stamp_from_lease` — **not** reduced parent match.
- Removed production use of `cancel_for_parent_lease`.

### P1 — Peer-close deregisters waits

- `WaitCancelGuard` Drop spawns async `deregister` when the parking task is
  abandoned (peer-close / serve_one early return). Explicit paths call
  `disarm()` after manual deregister.

### P1 — Distinct timeout vs user cancel

- `ConnectionControl::CancelTurn { turn_generation, cause }` is the
  watchdog turn-cancel path (generation-guarded session/cancel).
- Automatic timeout uses `finalize_active_watchdog_cancel` — **not**
  `finalize_active_user_cancel` — and does **not** cascade
  `cancel_by_parent_turn` (background children survive multi-task wait
  timeout escalation).
- Wait cancel watch channel carries `Option<CancelCause>`; UserStop emits
  `user_cancelled`, AutoTimeout emits `tool_stalled_timeout`.

### P2 — Generation guards

- `admit_cancel_terminal_if_current`, `cancel_delegation_task_if_verified`,
  `cancel_delegation_wait_if_verified`, `cancel_mcp_if_verified`, and
  `cancel_turn_if_current` all require matching active turn generation
  (reject when `None` or mismatched).

### P2 — MCP cancel register path

- `SessionState.mcp_cancel_registry` shares the manager process registry.
- `tool_watchdog_on_tool_event` for `ToolCategory::Mcp` registers a cancel
  token and binds `CancellationCapability::McpRequest`.

## Files changed (fix commit)

| File | Change |
| --- | --- |
| `manager.rs` | ProductionCancelHost, scan/execute, full-stamp wait, gen guards, test |
| `connection.rs` | CancelTurn control + finalize_active_watchdog_cancel; MCP bind |
| `wait_cancel.rs` | Cause on cancel channel; WaitCancelGuard; full-stamp tests |
| `listener.rs` | Full WaitStamp register; guard; cause-aware reports |
| `supervisor.rs` | CancelHost wait/turn take CancelCause |
| `session_state.rs` | mcp_cancel_registry Arc |
| `types.rs` / `registry.rs` / `mod.rs` | CancelCause in types |
| `mcp_cancel.rs` | Debug impl |

## Test summary

```powershell
cargo test --lib --features test-utils tool_watchdog -- --nocapture
cargo test --lib --features test-utils wait_cancel -- --nocapture
cargo test --lib --features test-utils terminal_cancel
cargo test --lib --features test-utils parent_cancel -- --test-threads=1
cargo test --lib --features test-utils production_cancel_host
cargo clippy --lib --features test-utils -- -D warnings
```

| Suite | Result |
| --- | --- |
| `tool_watchdog` | **85 passed** |
| `wait_cancel` | **11 passed** |
| `terminal_cancel` | **5 passed** |
| `parent_cancel` | **12 passed** |
| `production_cancel_host` | **1 passed** |
| clippy `-D warnings` | **clean** |

## Concerns / follow-ups

1. **Periodic scan loop still Task 7** — executor + `scan_and_execute_cancellations`
   are production-callable; process-wide timer / settings wiring is Task 7.
2. **MCP cancel_fn is best-effort** — register path binds a token that returns
   `true` (accepted); real provider cancel plumbing may refine the callback.
3. **DelegationWait capability bind on park** — listener registers the wait
   handle with a full stamp; binding `DelegationWait { wait_id }` onto the
   lease when capability is upgraded can be tightened when attribution owns
   wait leases end-to-end.
4. Design doc under `docs/superpowers/specs/` may still have local unstaged
   edits (out of commit scope).

## Self-review

### Spec compliance
- Host-only cancel; public Timeout no-op preserved
- Production executor reachable after ClaimCancel
- Full WaitStamp cancel path
- Peer-close deregister via Drop guard
- AutoTimeout ≠ user cancel cascade
- Distinct error codes
- Generation guards on admit / task / wait / mcp / turn

### Quality
- ProductionCancelHost keeps escalation unit-testable and production-wired
- Clippy clean under `-D warnings`
