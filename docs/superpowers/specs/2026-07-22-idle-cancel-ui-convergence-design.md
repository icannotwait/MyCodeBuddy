# Idle Cancel UI Convergence Design

Date: 2026-07-22

Status: Design approved in conversation; written-spec review pending

## Summary

Make an explicit cancel converge the main-window composer to the backend's
authoritative idle state even when the frontend missed the original terminal
event. The active-turn cancellation path remains unchanged. The idle
`ConnectionControl::Cancel` path will emit `StatusChanged(Connected)` after
local cancellation cleanup so a stale frontend projection cannot remain
`prompting` indefinitely.

## Incident Evidence

Conversation 898 reproduced the split state:

- the user clicked Stop in the main window;
- the persisted conversation status became `cancelled`;
- no delegation continuation existed for the conversation; and
- the composer continued rendering the `prompting` UI.

The composer derives Stop visibility and send behavior from the frontend ACP
connection projection. It does not derive them from the persisted conversation
row. The active cancel path emits `TurnComplete(cancelled)` and then restores
`Connected`, but the idle cancel branch sends the upstream cancel notification
and performs cleanup without emitting any connection-state event. If the
frontend still projects `prompting` when that branch runs, no later event is
guaranteed to repair it.

## Goals

1. Make Cancel an idempotent state-convergence operation for the composer.
2. Keep the backend connection state authoritative.
3. Preserve active-turn cancellation behavior and delegation cleanup.
4. Apply the same behavior to desktop and server transports.
5. Add a regression test for the idle cancellation branch.

## Non-Goals

- Optimistically unlocking the composer before the backend handles Cancel.
- Changing the desktop event batcher or connection ownership model.
- Synthesizing a second `TurnComplete` when no turn is active.
- Changing conversation persistence or cancellation status transitions.
- Expanding `ConnectionControl` with an acknowledgement protocol.
- Refactoring unrelated ACP lifecycle code.

## Alternatives Considered

### Backend State Convergence

Selected. Emit the authoritative idle status from the idle cancel branch. This
is transport-neutral, preserves the backend as source of truth, and directly
closes the missing state transition with a narrow change.

### Frontend Optimistic Unlock

The frontend could set the local status to `connected` when `acp_cancel`
returns. That would respond quickly, but the command currently confirms queue
acceptance rather than completed cancellation. It could therefore display an
idle composer while the backend still owns an active turn or while cleanup
fails.

### Cancel Acknowledgement and Snapshot Handshake

The control command could carry an acknowledgement and make the API return a
fresh session snapshot. This provides a stronger request/response contract but
changes the control protocol and all cancellation call sites. It is unnecessary
for the observed missing idle transition.

## Selected Design

### Active Turn

No behavior changes. A Cancel consumed by the active prompt loop continues to:

1. notify the agent;
2. finalize the turn as `cancelled`;
3. emit `TurnComplete(cancelled)`, which clears `turn_in_flight` and restores
   the session state to `Connected`;
4. cancel turn-scoped delegation work; and
5. release pending permissions and terminal resources.

The existing post-loop `StatusChanged(Connected)` remains in place.

### Idle Connection

The idle `ConversationInput::Control(ConnectionControl::Cancel)` branch keeps
its existing cleanup. Once the branch has accepted the cancel notification and
released local pending permission and terminal state, it emits:

```text
StatusChanged { status: Connected }
```

The event is deliberately emitted even when the backend session state already
equals `Connected`. Its purpose is convergence: the event sequence advances and
all attached clients receive an authoritative state assertion.

The branch does not emit `TurnComplete`. There is no active turn to complete,
and a synthetic completion could duplicate lifecycle persistence, awaiting
reply handling, or transcript finalization.

Turn-scoped delegation cancellation remains intact. The connected-state event
must not wait on slow child teardown; it should be emitted before the awaited
broker cascade, matching the active path's user-visible convergence ordering.

## Data Flow

```text
Stop click
  -> acp_cancel
  -> ConnectionControl::Cancel
  -> idle connection branch
  -> agent cancel notification
  -> local permission/terminal cleanup
  -> StatusChanged(Connected)
  -> SessionState event sequence and snapshot update
  -> desktop batch or per-connection stream
  -> frontend reducer
  -> composer replaces Stop with Send
  -> delegation cascade continues
```

## Error Handling

Sending the upstream cancel notification remains best effort, as it is today.
An idle session is already locally non-prompting, so a notification failure must
not prevent the authoritative `Connected` assertion.

The new event uses `emit_with_state`, preserving event sequencing, replay,
snapshot state, desktop batching, and server attach behavior. No frontend
timeout or polling fallback is added.

## Testing

Add a focused Rust regression test around the real idle connection-control
path or its smallest existing harness. The test must fail before the fix and
prove that:

1. an idle `ConnectionControl::Cancel` emits `StatusChanged(Connected)`;
2. it emits no `TurnComplete`;
3. the session remains connected and usable after the command; and
4. the event is applied to `SessionState`, not only sent to a test mock.

Retain existing active cancellation and manager persistence tests. Run the
focused test first, followed by the relevant Rust library tests, `cargo check`,
and Clippy for the affected desktop and server build surfaces.

## Risks and Mitigations

### Duplicate Connected Events

The idle backend state may already be `Connected`. Status events and frontend
reducer updates are idempotent; the extra event occurs only on an explicit user
Cancel and is intentionally the convergence signal.

### Duplicate Turn Finalization

The idle branch must not emit `TurnComplete`. The regression test explicitly
asserts its absence.

### Delayed UI Recovery

Awaiting delegation teardown before emitting the status could preserve the
visible lock during slow child cleanup. Emit the status before that cascade.

## Acceptance Criteria

1. Clicking Stop on a stale main-window composer causes it to leave the
   `prompting` UI after the backend handles the command.
2. Active-turn cancellation behavior is unchanged.
3. Idle cancellation emits one authoritative connected-state assertion and no
   synthetic turn completion.
4. Desktop and server consumers receive the event through existing delivery
   paths.
5. Focused regression tests, relevant Rust tests, checks, and Clippy pass.
