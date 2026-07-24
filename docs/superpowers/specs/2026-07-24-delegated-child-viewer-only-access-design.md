# Delegated Child Viewer-Only Access Design

- Date: 2026-07-24
- Status: Approved

## Context

Delegated child conversations already have their own durable conversation row,
`external_id`, ACP connection, transcript, and parent relationship. They can be
opened in a normal main tab through `openDelegatedChildSession`.

The current main-tab path does not work for a running child:

- The root workspace query excludes rows whose `parent_id` is set.
- The root workspace store rejects child upserts.
- The conversation surface uses that root store to decide whether auto-connect
  is allowed.
- Connection discovery therefore never runs for the child, even though the
  child's ACP process is still alive.

The result is a tab that reports ACP as disconnected and stops receiving live
messages. Allowing that tab to create a replacement connection would be worse:
two ACP processes could write the same external session and diverge.

## Goals

1. Open a delegated child in a normal main tab and show its complete persisted
   and live content.
2. Keep the child read-only while its delegation task is running.
3. Keep all direct delegate children read-only while their immediate parent
   conversation has any active turn.
4. Let the same child tab start a new turn after both the child task is terminal
   and the parent conversation is idle.
5. Re-lock those children whenever the parent starts a later turn.
6. Preserve `kind = delegate`, `parent_id`, the tab, and the original
   `external_id` throughout the lifecycle.
7. Never spawn a replacement ACP while the child is in observer-only mode.

## Non-Goals

- Promoting a delegated child into a root conversation.
- Giving a child permanent independence from its parent.
- Recording which historical parent turn created a child.
- Adding parent-turn identity or permanent unlock fields to the database.
- Cancelling an already-running child turn when the parent starts a new turn.
- Changing the existing co-controlling semantics of ordinary cross-client
  `isViewer` connections.

## Chosen Semantics

Access is based on current parent ownership, not on the originating parent turn.
For a delegated child:

```text
viewer_only =
  child delegation task is not terminal
  OR immediate parent conversation has an active turn
```

Terminal child task states are `completed`, `failed`, and `canceled`.
`running`, a missing task state, or an undecodable task state is treated as
non-terminal.

A parent is active when its durable conversation status is `in_progress` or its
live ACP state reports `turn_in_flight`. The live state closes the window before
the durable status event reaches clients; the durable status supports
cross-client observation and recovery. If either source says active, the child
is locked. Missing or contradictory state fails closed to `viewer_only`.

When both conditions clear, the child becomes interactive. A later parent turn
re-locks all direct delegate children until that turn ends. Nested delegations
apply the same rule to their immediate parent.

## Access Projection

The backend exposes one lightweight access projection for a conversation:

```ts
type DelegateAccessState = {
  mode: "viewer_only" | "interactive"
  reason: "task_running" | "parent_turn_active" | "state_unknown" | null
  parent_id: number
}
```

If multiple lock reasons apply, `task_running` has display precedence. The mode
does not depend on that precedence.

A shared backend resolver computes this projection from the child row, its
immediate parent row, and `ConnectionManager`. Both the Tauri command and Axum
handler call the same core function. The resolver is also used by interactive
admission guards so the UI and backend do not implement different policies.

No schema migration is required.

## Backend Boundaries

### Access resolver

The resolver has one responsibility: return the effective delegate access mode
for a conversation at the time of the call.

- Regular/root conversations retain their existing behavior and do not enter
  delegate observer mode.
- A malformed delegate row, a missing parent, or a failed status lookup returns
  `viewer_only` with `state_unknown`.
- A stale durable `in_progress` status remains fail-closed until existing
  lifecycle reconciliation heals it.

### Interactive admission

User-originated operations that can acquire execution ownership or mutate a
delegate are checked again by the backend. A locked request returns a typed
`delegate_viewer_only` error containing the current reason.

Restricted operations include:

- owner connect/resume and reconnect;
- sending a new prompt;
- cancel;
- mode and configuration changes;
- fork;
- feedback submission;
- answering an agent question.

Permission approve/reject remains allowed because a running delegated task may
otherwise deadlock waiting for authorization.

The guard applies only to user-facing interactive entry points. Broker-owned
child startup, continuation, cleanup, and terminal settlement must remain able
to operate while the child is `viewer_only`.

An operation admitted just before a parent turn starts is allowed to finish.
Starting the parent turn re-locks the UI and blocks subsequent child mutations;
it does not cancel work already admitted.

## Frontend Boundaries

### Delegate access hook

A focused hook owns access-state loading and refresh. It does not depend on the
root workspace store containing the child.

It refreshes when:

- the child tab opens;
- a `conversation://changed` event targets the child or its returned
  `parent_id`;
- the transport reconnects;
- an interactive command is rejected with `delegate_viewer_only`.

Concurrent refreshes are coalesced. Loading and error states are
`viewer_only`.

### Connection intent

Connection lifecycle gains an explicit intent:

```ts
type ConnectionIntent = "own_or_observe" | "observe_existing"
```

`observe_existing` is used for a locked delegate:

1. Query `acp_find_connection_for_conversation` using both the conversation id
   and `(external_id, agent_type)` fallback.
2. If a live connection exists, attach through the existing
   snapshot/replay/live viewer path.
3. If no connection exists while the child task is running, retry discovery
   with bounded backoff through the spawn window.
4. Stop retrying on terminal task state, tab unmount, intent change, or an
   unrecoverable error.
5. Never fall through to `acpConnect`.

`own_or_observe` retains normal behavior: discover another live owner first,
otherwise resume or create an owner connection using the same `external_id`.

### Viewer identity versus interaction access

Existing `isViewer` means the local tab does not own the ACP process, but the
viewer may ordinarily co-control it. `viewer_only` is a separate interaction
capability and must not be inferred from `isViewer`.

A locked delegate attached to the broker-owned child connection has both
`isViewer = true` and `viewer_only`. A previously interactive child may still
own an idle or in-flight connection when its parent starts a turn; in that case
ownership is retained while the surface becomes `viewer_only`. This avoids
killing an active child process merely to express a UI lock.

## State Transitions

| Child task | Parent turn | Effective mode | Connection behavior |
| --- | --- | --- | --- |
| Running/unknown | Idle | `viewer_only` | Observe existing only |
| Running/unknown | Active/unknown | `viewer_only` | Observe existing only |
| Terminal | Active/unknown | `viewer_only` | Keep existing connection read-only; never reconnect |
| Terminal | Idle | `interactive` | Normal discovery/resume using the same `external_id` |

Transitions are reversible. In particular, `interactive -> viewer_only` is
expected whenever the parent re-enters and starts another turn.

## Open and Live-Update Flow

1. `openDelegatedChildSession` opens or focuses the existing child as a normal
   main tab.
2. The surface loads `DbConversationDetail` directly for the child, even though
   the root workspace store excludes it.
3. The access hook loads `DelegateAccessState`.
4. In `viewer_only`, lifecycle runs only `observe_existing` discovery.
5. A successful attach receives a cold snapshot, any replay, and then live ACP
   events from the one broker-owned connection.
6. Parent and child conversation changes refresh access state.
7. When the child task is terminal and the parent is idle, lifecycle detaches a
   broker viewer if necessary and enters normal `own_or_observe` behavior.
8. A later parent `in_progress` transition immediately re-locks the surface.

## Content Completeness and Consistency

The child transcript is a convergent composition of three sources:

1. Persisted detail provides all history already written by the agent parser.
2. A cold ACP snapshot provides current in-memory state when the tab attaches
   during a turn.
3. Monotonic event sequence replay plus live events provides changes after the
   snapshot.

The UI may briefly be ahead of the on-disk transcript while an agent is
streaming or flushing its session file. That is expected. Terminal state must
trigger authoritative convergence rather than trusting the final live event:

- Re-fetch persisted detail with bounded backoff after `TurnComplete` or a
  terminal child task update.
- Continue while `in_flight_user_turn_id` is present or the persisted tail
  still indicates that the assistant reply has not flushed.
- Reconcile using stable turn ids and the existing normalized user-content
  signature so the live user turn and persisted turn are not duplicated.
- Retire temporary live content only after the persisted transcript contains
  its replacement.

On transport reconnect, discard the stale subscription, refresh persisted
detail and access state, then cold-attach again if a live child connection still
exists. This closes event-loss windows without inventing a second owner.

If the agent process exits before content was either emitted over ACP or written
to its transcript, that content cannot be reconstructed. The surface reports a
sync failure and preserves the last authoritative content; it does not fabricate
turns.

## UI Capability Matrix

| Capability | `viewer_only` | `interactive` |
| --- | --- | --- |
| Read persisted messages | Enabled | Enabled |
| Stream messages/tools/status | Enabled | Enabled |
| Approve/reject permissions | Enabled | Enabled |
| Send prompt | Disabled | Enabled |
| Cancel | Disabled | Enabled |
| Change mode/configuration | Disabled | Enabled |
| Fork | Disabled | Enabled |
| Submit feedback | Disabled | Enabled |
| Reconnect/create owner ACP | Disabled | Enabled |
| Answer agent question | Disabled | Enabled |

An unsent draft remains in local draft storage while locked and is restored
when access returns. It is never sent automatically.

The status area distinguishes:

- waiting for the child ACP to appear;
- observing read-only;
- locked by an active parent turn;
- interactive;
- synchronization failure.

It must not label an observer that is waiting for discovery as a disconnected
owner ACP.

## Failure and Race Handling

- Access lookup failure: fail closed, retain content, retry with backoff.
- Parent missing or state unknown: fail closed with `state_unknown`.
- Child connection not yet registered: keep observer discovery active while the
  task is running; do not spawn.
- Discovery/attach race with teardown: treat the connection as absent and rely
  on persisted terminal convergence.
- Parent starts during a child draft: lock immediately and preserve the draft.
- Parent starts during an admitted child turn: continue streaming that turn,
  disable mutations, and do not cancel it.
- Send races a parent status change: backend returns
  `delegate_viewer_only`; restore the draft and refresh access.
- WebSocket disconnect: refresh detail/access on reconnect and cold-attach.
- Terminal transcript flush lag: bounded polling converges the persisted detail.
- Tab close: detach viewer subscriptions and cancel discovery/reconciliation
  timers without disconnecting the broker-owned ACP.

## Compatibility

- The conversation remains a delegate row and stays grouped under its parent.
- The normal main-tab implementation is reused; no separate viewer window or
  viewer-only tab type is introduced.
- Ordinary root cross-client viewers keep their current co-control behavior.
- Desktop and server modes expose the same DTO and core policy.
- No database migration or historical parent-turn backfill is needed.

## Test Plan

### Rust unit and integration tests

- Access matrix for running/terminal child tasks and active/idle parents.
- A later parent turn re-locks every direct terminal delegate child.
- Parent turn completion unlocks only terminal children; running children stay
  locked.
- Missing child task state, missing parent, and lookup failure fail closed.
- Live `turn_in_flight` locks before a durable status event arrives.
- User connect/resume and prompt admission reject with
  `delegate_viewer_only` while locked.
- Restricted connection mutations reject while permission responses remain
  accepted.
- Broker-owned startup/continuation is not blocked by the user-facing guard.
- Tauri and Web wrappers return the same access projection.

### Frontend unit and component tests

- A child absent from the root workspace store still opens and loads detail.
- `observe_existing` attaches snapshot/replay/live and updates messages.
- Null discovery in observer mode never calls `acpConnect`.
- Delayed child registration is discovered; polling stops on terminal state,
  intent change, and unmount.
- Every control matches the capability matrix.
- Permission approve/reject remains functional in `viewer_only`.
- Parent start/end events re-lock and unlock the child without replacing its
  tab or identity.
- Re-locking preserves a draft and does not cancel an active child turn.
- A raced send restores its draft after `delegate_viewer_only`.
- Terminal persistence lag converges without missing or duplicated turns.
- A missed event and transport reconnect converge through detail refresh plus
  cold snapshot.
- `kind`, `parent_id`, tab identity, and `external_id` remain unchanged.

### Verification

Run focused tests while implementing, then the relevant repository checks:

```bash
pnpm test
pnpm eslint .
pnpm build

cd src-tauri
cargo test --features test-utils
cargo check
cargo check --no-default-features --bin codeg-server
```

Add targeted server-mode tests for the new handler and run clippy for every
changed Rust target before completion.

## Acceptance Criteria

1. Opening a running delegated child in a main tab shows live messages, tools,
   and status instead of a dead/disconnected surface.
2. Observer mode never creates a second ACP connection.
3. The child cannot start or mutate a normal interactive turn while its task is
   running or its parent has an active turn.
4. A terminal child becomes interactive as soon as the parent is idle.
5. Any later parent turn re-locks all direct delegate children.
6. An already-running child turn is allowed to finish under the read-only lock.
7. After terminal reconciliation, the rendered transcript matches the
   authoritative persisted conversation without gaps or duplicates.
8. Parent/child identity, grouping, tab identity, and external session identity
   are preserved.
