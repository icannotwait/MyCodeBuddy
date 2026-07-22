# Upstream Terminal Reconnect Convergence Design

Date: 2026-07-22

Status: Design approved in conversation; written-spec review pending

## Summary

Keep an upstream-terminated root conversation durably and visibly cancelled.
After the terminal event, the frontend must not automatically reconnect to the
old external session or flush queued prompts. Reconnecting is an explicit user
action; it restores only the ACP connection. A subsequent explicit prompt is
the only action that may transition the conversation back to `in_progress`.

This is separate from `idle-cancel-ui-convergence`, which reasserts an idle
`Connected` ACP state after an explicit user Cancel. That design must not make
an upstream-disconnected session appear usable.

## Incident Evidence

The 2026-07-22 runtime log establishes this sequence for root conversation
`1026` and delegate `1029`:

1. At 09:32:13 local time, the prompting root connection was disconnected and
   the delegate settled as cancelled because its parent disconnected.
2. At 09:32:18, the frontend created a new connection using the root's prior
   external session id.
3. At 09:32:24, the database log reported that conversation `1026` was already
   `cancelled` before a later status write changed it to `pending_review`.
4. At 09:32:28, a further write changed the same row to `in_progress`.

The same upstream termination class cancelled root `1018` and delegate `1030`.
The inconsistent display is therefore not an uncertainty about the upstream
cause: it is a local reconnect/replay path reopening a terminal projection.

## Root Cause

`ConversationSessionSurface` considers every persisted conversation eligible
for automatic ACP connection. Its auto-connect path uses the stored
`external_id`, but does not gate on the persisted conversation status.
`useConnectionLifecycle` consequently reconnects a mounted terminal tab just
as it would an interrupted in-progress tab.

The reconnect can replay historical upstream events. The normal lifecycle
`TurnComplete` transition is already CAS-protected, but a new prompt send
intentionally writes `in_progress`. The frontend queue flush also treats a
newly connected ACP session as eligible work, so it can turn a cancelled
conversation back into an active one without a new user decision.

The child-side broker projection is not the source of this divergence. It
already preserves the delegate's terminal result and error code, such as
`parent_disconnected`. Root status arrives through the authoritative
`conversation://changed` state patch, which the workspace store applies to the
sidebar and child projections.

## Goals

1. Render an upstream-terminated root as `Cancelled` and its ACP connection as
   `Disconnected`.
2. Prevent automatic reconnects and queued-prompt flushes for a cancelled root.
3. Provide a visible, explicit reconnect command.
4. Keep the persisted status `cancelled` after reconnect until the user sends
   a new prompt.
5. Preserve delegated child terminal states and their existing causal reason.
6. Verify the backend root-state patch and frontend convergence independently.

## Non-Goals

- Diagnose or retry the upstream provider failure.
- Add a database column for a terminal reason or a new resume protocol.
- Change the active or idle user-cancel event semantics covered by the
  idle-cancel design.
- Change broker ownership of delegate-row terminal status.
- Automatically resend the interrupted prompt or any queued prompt.
- Refactor connection ownership, desktop batching, or transport reconnects.

## Alternatives Considered

### Frontend Terminal Gate Only

Block automatic ACP connection whenever the persisted conversation status is
`cancelled`. This is narrow, but incomplete without a separate queue gate:
reconnecting manually could otherwise flush stale queued work and reopen the
conversation.

### Durable Backend Termination Metadata

Persist a terminal reason and require a dedicated resume command before any
status transition out of `cancelled`. This is the strongest cross-client
contract, but needs a migration, new API semantics, and careful authorization
of every existing prompt source. It is disproportionate for the observed
frontend reconnect path.

### Selected: Status-Derived Frontend Gate With Backend Regression Coverage

Use the existing authoritative `cancelled` state patch as the terminal gate in
the frontend. Separate automatic connection from explicit reconnect, pause
queued work, and add focused backend tests that prove the root cancellation
patch remains available. This preserves current APIs and keeps a deliberate
new prompt as the existing, intentional way to start a new turn.

## Selected Design

### Authoritative State

For a persisted conversation, `ConversationSessionSurface` will select its
row from the workspace store and derive `isTerminalCancelled` exactly as
`summary.status === "cancelled"`. `pending_review` and `completed` retain their
existing automatic-connection behavior. A missing summary is treated as not
ready for automatic connection, not as permission to reconnect. This avoids a
refresh race in which the ACP hook starts a historical session before the
status patch or reconciliation fetch arrives.

The status patch remains the durable source of truth. The per-connection
`conversation_status_changed` ACP envelope remains a local no-op, as it is
today; the global `conversation://changed` `State` patch drives durable list,
tab, and delegate-card projections.

The terminal ACP `Error` envelope must additionally carry its existing backend
`terminal` classification on the frontend wire. A per-tab
`terminalDisconnectLatch` is armed only while the bound root summary is
`in_progress`, and records that summary's `updated_at` value as its baseline.
It is set by `terminal: true` Error or by a bare
`status_changed: "disconnected"` envelope, which covers a clean transport close
with no preceding Error without turning idle/user-cancelled rows into queue
pauses. Both paths run before the lifecycle worker's database CAS and global
state patch can race with focus. The latch fails automatic connection and focus
retry closed; it survives the stale pre-CAS `in_progress` row and a later
`cancelled` row, and clears only after a newer authoritative non-`cancelled`
state patch (`in_progress`, `pending_review`, or `completed`). It is irrelevant
to the dedicated explicit reconnect command. Recoverable ACP errors continue
to carry `terminal: false` and must not set this latch.

### Connection Lifecycle

`useConnectionLifecycle` will distinguish two policies:

- **Automatic connection**: allowed for an active persisted summary whose
  status is not `cancelled`, under the existing rules and only after its
  summary is available.
- **Explicit reconnect**: allowed only after a user command and may attach to
  the old external session id, but does not change the conversation status.

The hook will receive an explicit automatic-connection permission instead of
overloading `isActive`. Its automatic effect and ordinary focus handler must
not reconnect a cancelled conversation or a tab with
`terminalDisconnectLatch`. The surface will expose a compact Reconnect control
for a cancelled/disconnected session. The control calls a dedicated reconnect
callback, not the auto-connect effect.

The durable row currently has no cancellation-source field. Therefore this
gate applies to every persisted `cancelled` root when it has no live
connection, rather than attempting to infer whether it was upstream or user
initiated. The existing active and idle user-cancel paths are unchanged while
their connection remains live; a later tab mount of any cancelled row requires
the same explicit reconnect decision.

After a successful explicit reconnect, the ACP state can be `connected` while
the persisted conversation remains `cancelled`. This is intentional: the
connection is available for inspection or a new user turn, while the prior
turn remains accurately terminal. The first new user prompt follows the
existing `send_prompt_linked` behavior and transitions the row to
`in_progress`.

### Queue Safety

The same terminal-error or bare-disconnect-in-progress signal that sets
`terminalDisconnectLatch` sets a per-tab `queuePausedByTerminalDisconnect`
latch. The queue remains visible and editable, but it is not drained merely
because a reconnect reaches `connected`. The latch is deliberately not derived
from durable `cancelled` status: the durable row has no cancellation source,
and deriving it there would change the existing active/idle user-cancel
behavior.

Reconnect alone keeps the queue paused. While this latch is set, a direct new
send is an explicit new turn and bypasses paused historical queue items instead
of taking the normal direct-send FIFO tail route. Resuming queued items is a
separate explicit command that clears the latch and restores the existing FIFO
flush behavior. This prevents cancellation from silently becoming a retry while
retaining the user's unsent drafts. Queues not paused by a terminal disconnect
retain the existing FIFO and user-cancel behavior.

### Presentation

The sidebar/root status uses the existing cancelled presentation. The session
surface shows the ACP state as disconnected and exposes the Reconnect command
near the composer. The command must be localized and keyboard-accessible. It
is a clear command, so text plus an appropriate reconnect icon is acceptable.

Delegate cards keep their current terminal state and causal detail, including
`parent_disconnected`; they are not normalized to an invented user-cancel
reason.

## State Transitions

| Event | Root conversation | ACP connection | Delegate card | Automatic work |
| --- | --- | --- | --- | --- |
| Upstream terminal disconnect | `cancelled` | `disconnected` | cancelled with parent cause | stopped |
| Tab remount or transport recovery | `cancelled` | `disconnected` | unchanged | no reconnect or queue flush |
| User clicks Reconnect | `cancelled` | connecting, then connected | unchanged | queue remains paused |
| User sends a new prompt | `in_progress` | prompting | unchanged historical result | new turn only; paused historical queue stays paused |
| User explicitly resumes queue | follows normal send lifecycle | connected/prompting | unchanged historical result | FIFO queue flush enabled |

## Error Handling

If explicit reconnect fails, the conversation remains `cancelled`, the ACP
projection remains disconnected or error, and no retry loop is scheduled. The
Reconnect control remains available for another user attempt.

If global delivery was unavailable while the terminal event occurred, the local
terminal-disconnect latch still blocks reconnect in that mounted tab. A later
workspace reconciliation fetches the persisted cancelled row before automatic
connection is permitted. A transient absence of the row is therefore fail
closed for a persisted tab.

No frontend code may synthesize a completed, pending-review, or in-progress
conversation status to make the composer usable.

## Testing

### Backend

Add or extend a lifecycle regression covering a linked root connection that
receives a terminal disconnect:

1. the terminal error is delivered as `terminal: true`, then the durable row
   transitions from `in_progress` to `cancelled`;
2. the root emits a `conversation://changed` state patch with `cancelled`;
3. a delayed `TurnComplete(end_turn)` cannot overwrite that terminal result;
   and
4. delegate settlement remains broker-owned and reports its parent-disconnect
   cause.

### Frontend

Add focused tests for the lifecycle hook and session surface:

1. an active persisted `cancelled` tab does not call `connect` on mount,
   remount, focus, or transport reconciliation, while `pending_review` and
   `completed` rows retain the existing automatic-connect behavior;
2. a terminal ACP error or bare terminal disconnect prevents focus reconnect
   before the global cancelled state patch arrives, including a disconnect
   observed after `end_turn` that preserves `pending_review`;
3. an explicit Reconnect command calls `connect` once with the existing
   session identity without changing the persisted status;
4. a connected ACP projection does not auto-flush a queue while its
   terminal-disconnect pause latch is set, while an ordinary user cancel does
   not create that pause;
5. a direct user send after reconnect bypasses paused historical queue items
   and transitions through the existing in-progress path; and
6. explicitly resuming the queue restores FIFO flush behavior.

Retain the existing workspace-store state-patch tests so the status update is
observably reflected in the sidebar before a user can choose to reconnect.

## Risks and Mitigations

### Users Expect Reconnect to Retry Work

Reconnect only restores a connection, because replaying a cancelled task is a
meaningful action. The separate queue-resume command makes that choice
explicit while preserving drafts.

### Stale Status During Startup

Treat an unknown persisted status as ineligible for automatic connection.
Existing non-cancelled persisted sessions connect after the status source
resolves, which is a short, safe delay.

### Regression Into the Idle-Cancel Fix

The terminal gate applies only to persisted `cancelled` conversation state. It
does not reinterpret the ACP `Connected` assertion emitted by the separate
idle user-cancel path, and it does not change active-turn cancellation.

## Acceptance Criteria

1. An upstream terminal disconnect leaves the root visibly cancelled and its
   ACP projection disconnected.
2. Refreshing, remounting, or focusing that tab does not reconnect it.
3. Reconnect is explicit, visible, and does not alter the persisted status.
4. No interrupted or queued prompt is silently sent after reconnect.
5. A new user prompt and an explicit queue-resume action continue to work.
6. Child delegate cards retain their terminal parent-disconnect explanation.
7. Focused Rust and frontend tests prove the state transition across the
   backend, workspace store, ACP lifecycle, and queue boundary.
