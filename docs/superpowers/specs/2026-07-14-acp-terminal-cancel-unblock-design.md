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

1. Add a `CancellationToken` and a retained exit-status signal to each
   `TerminalInstance`.
2. `wait_for_exit()` uses `tokio::select!` between `child.wait()` and token
   cancellation. On cancel it **drops the child mutex** so kill can proceed,
   then waits for a published exit status.
3. `kill_command()` **cancels the token first**, then acquires the child mutex,
   kills the process tree (`kill_tree`), waits for exit, drains readers, and
   publishes `exit_status` to the snapshot and retained signal.
4. Mid-prompt cancel in `connection.rs` emits `TurnComplete { cancelled }`
   **before** terminal cleanup so UI recovery is not gated on kill latency.
5. `release_all_for_session()` starts one owned kill task per terminal and waits
   for their handles only until a shared deadline. Timed-out tasks remain
   detached and continue cleanup without blocking connection recovery.

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
exit_status_tx: watch::Sender<Option<TerminalExitStatus>>
```

`cancel` is created per instance. `exit_status_tx` retains the most recently
published status so subscribers cannot miss an update between checking state
and awaiting a change.

### wait_for_exit

1. `refresh_exit_status` / return cached status if present.
2. Loop:
   - If `cancel.is_cancelled()`, await the retained published exit status and
     return.
   - Lock child briefly. If missing, await published status.
   - `select!`:
     - `child.wait()` → clear child, drain readers, publish status, return.
     - `cancel.cancelled()` → drop lock (critical), then await published status.

Dropping a pending `Child::wait()` future must leave `Child` usable for a later
wait in the kill path (Tokio process API guarantee).

### kill_command

1. `cancel.cancel()` **before** taking the child mutex.
2. Fast path if `exit_status` already published.
3. Lock child; if present, `kill_tree(pid)` then `child.wait()`; clear child.
4. Drain readers; publish `exit_status` to the snapshot and retained signal.
5. If child already gone, await / refresh until status is published (waiter may
   have finished natural exit in a race).

### release_all_for_session

Remove matching terminals from the map, spawn one owned `kill_command` task per
terminal, then wait for their handles until one shared deadline. Dropping a
timed-out `JoinHandle` detaches the waiter without cancelling its underlying
task. Suggested session-wide bound: ~2–3 seconds (constant next to
`READER_DRAIN_GRACE`).

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
2. Start `wait_for_terminal_exit` and deterministically observe that it owns the
   child wait before calling `release_all_for_session`.
3. Assert the join completes within a hard timeout (e.g. 5s).
4. Assert wait returns an exit status (or kill completes without hang).
5. Optionally assert the process tree is gone when a PID was captured (best
   effort; hang-prevention is the primary assertion).

Existing create/wait/output tests must keep passing.

## Risks

- Lost wakeup between status publish and wait: use a retained status signal
  (`watch` or equivalent), not an edge-triggered `Notify` registration.
- Double drain_readers: safe if handles vector is taken once.
- Windows process trees via `cmd`/`powershell` shims: keep `kill_tree`.
- Timed-out kill still running in background may race with a new session
  terminal id only if ids collide; UUIDs make this negligible.

## Success Criteria

- Concurrent wait + release never hangs.
- Cancel emits `TurnComplete` without waiting for process death.
- Long-running terminal processes are terminated on release/kill.
- Cancelled permissions disappear immediately from live and snapshot clients.

## Follow-up Hardening (2026-07-15)

Post-implementation review found three cancellation-safety gaps in the first
version. This section supersedes the affected details above while preserving
the same public ACP protocol.

### Cancellation-safe release tasks

`tokio::time::timeout(RELEASE_KILL_BOUND, terminal.kill_command())` is not
cancellation-safe. The timeout drops `kill_command` directly, including after
it has cleared `child` but before reader drain and `exit_status` publication.
A fresh retry then sees no child and can wait forever for a publisher that no
longer exists.

Session release must instead:

1. Remove all matching terminals from the runtime map.
2. Spawn one owned kill task per terminal. The task, not the caller, owns the
   complete kill/wait/drain/publish transition.
3. Await the task handles only until one shared `RELEASE_KILL_BOUND` deadline.
   Dropping a timed-out `JoinHandle` detaches the task; it does not cancel the
   underlying kill operation.
4. Log kill errors inside each owned task so detached failures remain visible.

Starting all tasks before waiting keeps total session-release latency bounded
by approximately one release bound rather than one bound per terminal.

### Retained exit-status signal

`Notify::notify_waiters()` does not retain a permit for a waiter created after
the call. Checking `snapshot.exit_status` before creating `notified()` leaves a
lost-wakeup window that can add the full ten-second fallback delay.

Replace the edge-triggered `Notify` with a retained status signal (`watch` or
an equivalent state-bearing primitive). Publishing an exit status must update
the snapshot and the retained signal. A waiter must observe a status published
before or after it subscribes without relying on timing, while the existing
hard deadline remains a final corruption guard.

### Cancel bookkeeping and permission state

Mid-prompt cancellation order becomes:

1. Send the agent `CancelNotification`.
2. Emit `TurnComplete { cancelled }`.
3. Clear tracked terminal tool calls.
4. Drain pending permission responders, respond with `Cancelled`, and emit a
   matching `PermissionResolved` event for every drained request.
5. Release all session terminals using the bounded owned tasks above.
6. Cascade-cancel delegations and background-drain the prompt response.

The frontend must also clear `pendingPermission` defensively on
`turn_complete`, matching `SessionState::apply_event`. The explicit
`PermissionResolved` event remains necessary for idle cancel, disconnect, and
other drain paths that do not deliver a turn completion to the current client.

### Follow-up regression coverage

- Force the release deadline after `child` is cleared but before reader drain
  completes; assert the detached owned task still publishes exit status.
- Verify the retained exit signal observes publication without a notification
  timing dependency.
- Replace the wait/release test's scheduling sleep with deterministic
  synchronization.
- Verify cancel-drained permissions emit `PermissionResolved` and that a live
  frontend clears a permission card on `turn_complete`.
