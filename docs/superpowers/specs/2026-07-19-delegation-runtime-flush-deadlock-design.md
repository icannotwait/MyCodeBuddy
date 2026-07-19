# Delegation Runtime Flush Deadlock Fix

## Problem

The coalesced runtime-stats flush can permanently lock
`pending.inner`. In Rust 2021, the temporary Tokio `MutexGuard` created by
the `if let` scrutinee remains alive through the entire `if let` body. The
body later attempts to acquire the same non-reentrant mutex after awaiting
the attention lookup, so the flush worker deadlocks itself.

Once this happens, every operation that needs the shared pending-state lock,
including `complete_call` and parent-turn cancellation, also waits forever.

## Scope

Fix only the guard lifetime in the runtime publication path. Keep the
existing persistence, attention lookup, terminal settlement, logging, and
publication behavior unchanged. Do not refactor the broader delegation lock
protocol or modify wire and database contracts.

## Design

Read the coordination identity in an explicit block:

1. Acquire `pending.inner`.
2. Clone the matching `CoordinationIdentity`.
3. Drop the guard at the end of the block.
4. Await the latest attention request.
5. Reacquire `pending.inner` in a second explicit block and read the running
   task's child conversation id.
6. Publish running metadata only if the task is still present in `running`.

The second lookup deliberately stays after the attention await. Taking both
values in the first snapshot would avoid the deadlock but could publish stale
running metadata after the task reached a terminal state while attention was
being queried.

No new timeout or error path is needed. The existing attention and metadata
operations retain their current behavior; this change only ensures no
pending-state guard survives across either await.

## Regression Test

Add a paused-time broker test using the existing runtime projection fixtures:

1. Start a real running delegation.
2. Project a child tool event so a coalesced runtime flush is scheduled.
3. Advance through `RUNTIME_STATS_FLUSH_INTERVAL` and wait until the runtime
   write is observed.
4. Require `complete_call` to finish within a short Tokio timeout.
5. Assert the persisted task reaches `Completed` and the final runtime
   projection is retained.

Before the production change, the test must fail at the completion timeout
because the flush worker still owns `pending.inner`. After the change it must
pass. Existing focused broker tests and Rust checks provide regression
coverage for settlement and both desktop/server compilation paths.

## Success Criteria

- A successful coalesced runtime write never leaves `pending.inner` locked.
- `complete_call` and cancellation can proceed after runtime publication.
- Running metadata is not emitted after a task has left `running` during the
  attention lookup.
- Existing runtime persistence and terminal settlement tests remain green.
