# ACP Terminal Cancel Unblock

## Goal

Ensure user cancel (and session terminal release) never deadlocks behind a
long-running `terminal/waitForExit`, never blocks `TurnComplete { cancelled }`
behind process cleanup, and still terminates the spawned process tree.

## Background

ACP agents often call `terminal/create` and then block on
`terminal/waitForExit` for shell commands. Codeg's cancel path calls
`TerminalRuntime::release_all_for_session()`, which invokes
`TerminalInstance::kill_command()`.

Today both `wait_for_exit()` and `kill_command()` acquire the same
`child: Mutex<Option<Child>>` and hold it across `child.wait().await`.

Race that hangs cancel:

1. Agent `waitForExit` holds the child mutex while awaiting process exit.
2. User cancel → `release_all_for_session` → `kill_command` waits on the same mutex.
3. Process is never killed; cancel handler never reaches `TurnComplete`.
4. UI stays in "prompting" / animation; reconnect may also stall on the same cleanup.

A frontend-only "stop animation when DB says cancelled" path would hide the
stuck UI without fixing the process leak. That is explicitly out of scope as a
standalone fix.

## Approved Approach

**CancellationToken + early TurnComplete + bounded cleanup.**

1. Add a `CancellationToken` (and an exit `Notify`) to each `TerminalInstance`.
2. `wait_for_exit()` uses `tokio::select!` between `child.wait()` and token
   cancellation. On cancel it **drops the child mutex** so kill can proceed,
   then waits for a published exit status.
3. `kill_command()` **cancels the token first**, then acquires the child mutex,
   kills the process tree (`kill_tree`), waits for exit, drains readers, and
   publishes `exit_status` + notifies waiters.
4. Mid-prompt cancel in `connection.rs` emits `TurnComplete { cancelled }`
   **before** terminal cleanup so UI recovery is not gated on kill latency.
5. `release_all_for_session()` applies a bounded wait per terminal; on timeout
   it logs and continues cleanup on a background task so connection state
   recovery is not blocked forever.

## Non-Goals

- Full per-terminal supervisor task owning `Child` via channels (larger rewrite).
- Frontend-only forced end of prompting animation after DB cancelled.
- Changing the ACP protocol schema or agent-facing terminal RPC shapes.
- Interactive PTY tabs in `terminal/manager.rs` (separate runtime).

## Alternatives Considered

### Per-terminal supervisor task

A dedicated task owns `Child` and accepts wait/kill commands over a channel.
Most thorough isolation, but larger ownership refactor and more test surface.
Deferred unless CancellationToken proves insufficient.

### Frontend cancelled hot-fix only

Stop animation when lifecycle writes cancelled. Masks process leaks and still
leaves kill/wait deadlocks affecting reconnect. Rejected as sole fix.

## Detailed Design

### TerminalInstance fields

```text
session_id, output_limit, child, snapshot, reader_handles  (existing)
cancel: CancellationToken
exit_notify: Notify
```

`cancel` is created per instance. `exit_notify` wakes waiters after
`exit_status` is published (kill path or natural exit).

### wait_for_exit

1. `refresh_exit_status` / return cached status if present.
2. Loop:
   - If `cancel.is_cancelled()`, await published exit status (with notify +
     periodic refresh) and return.
   - Lock child briefly. If missing, await published status.
   - `select!`:
     - `child.wait()` → clear child, drain readers, publish status, notify, return.
     - `cancel.cancelled()` → drop lock (critical), then await published status.

Dropping a pending `Child::wait()` future must leave `Child` usable for a later
wait in the kill path (Tokio process API guarantee).

### kill_command

1. `cancel.cancel()` **before** taking the child mutex.
2. Fast path if `exit_status` already published.
3. Lock child; if present, `kill_tree(pid)` then `child.wait()`; clear child.
4. Drain readers; publish `exit_status`; `exit_notify.notify_waiters()`.
5. If child already gone, await / refresh until status is published (waiter may
   have finished natural exit in a race).

### release_all_for_session

Remove matching terminals from the map, then for each:

- `timeout(RELEASE_KILL_BOUND, kill_command())`
- On timeout: log error, `tokio::spawn` continued `kill_command` (token already
  cancelled from the timed-out attempt if it reached cancel; spawn a fresh call
  which is idempotent once status is published).

Suggested bound: ~2–3 seconds per terminal (constant next to `READER_DRAIN_GRACE`).

### connection cancel ordering

Mid-prompt `ConnectionCommand::Cancel`:

1. Send agent `CancelNotification`.
2. Emit `TurnComplete { cancelled }` (UI unblocks).
3. Cancel permissions / clear tracked terminal tool calls.
4. `release_all_for_session` (bounded).
5. Cascade-cancel delegations; background-drain prompt response.

Disconnect paths may keep terminal cleanup before teardown; they still benefit
from non-blocking kill via the token + bound.

Idle cancel (between prompts) has no in-flight turn; emit order is less
critical, but `release_all_for_session` still uses the same bounded kill.

## Testing

Regression (unit, `terminal_runtime` tests):

1. Create a long-running terminal command (`Start-Sleep` / `sleep`).
2. Concurrently:
   - `wait_for_terminal_exit`
   - after a short delay, `release_all_for_session`
3. Assert the join completes within a hard timeout (e.g. 5s).
4. Assert wait returns an exit status (or kill completes without hang).
5. Optionally assert the process tree is gone when a PID was captured (best
   effort; hang-prevention is the primary assertion).

Existing create/wait/output tests must keep passing.

## Risks

- Lost wakeup between status publish and notify: always re-check
  `exit_status` after `notified()` and before waiting.
- Double drain_readers: safe if handles vector is taken once.
- Windows process trees via `cmd`/`powershell` shims: keep `kill_tree`.
- Timed-out kill still running in background may race with a new session
  terminal id only if ids collide; UUIDs make this negligible.

## Success Criteria

- Concurrent wait + release never hangs.
- Cancel emits `TurnComplete` without waiting for process death.
- Long-running terminal processes are terminated on release/kill.
- No frontend change required for correctness.
