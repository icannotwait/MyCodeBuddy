# Workflow Refresh Settled-to-Active Convergence Design

## Status

Direction approved on 2026-08-10.

This is a narrow follow-up to
`2026-08-08-workflow-refresh-self-healing-design.md`. It supersedes that
design's workflow-graph timer rules only where they say a discovered,
settled, overlay-only graph owns no timer. Delegation-card reconciliation and
event-subscription recovery remain unchanged.

## Problem

The installed build already contains the previous workflow refresh fixes, but
the window can still show a stale workflow until it is closed and reopened.

The remaining failure requires this sequence:

1. The frontend cache contains a discovered workflow whose state is settled.
2. The workflow chip is collapsed, so `SubAgentOverlay` releases its overlay
   interest lease.
3. The durable workflow later moves from settled to active, but the window
   misses the graph-change event.
4. The cached settled graph owns no fallback timer, so the frontend never
   discovers the newer active revision.
5. Reopening the window reacquires interest and fetches authority, making the
   new state appear.

Inactive conversation tabs stay mounted. `ConversationSessionSurface` passes
their activity state to `MessageListView`, but `MessageListView` currently
discards that value. Keeping every mounted overlay interested would therefore
poll hidden historical tabs as well as the current tab.

## Decision

Keep events as the immediate update path and add a bounded authority fallback
for the active conversation surface.

1. Propagate the existing `isActive` state through `MessageListView` and both
   sub-agent overlay rendering paths.
2. A valid active conversation acquires overlay interest for the lifetime of
   its mounted surface, regardless of whether its workflow chip is expanded.
3. A discovered graph with active overlay interest refreshes from authority
   every 15 seconds, including when the cached graph currently looks settled.
4. Expanded-graph interest remains conditional on the workflow segment and
   full graph being open. It does not keep hidden tabs alive.
5. When a tab becomes inactive or unmounts, it releases its leases. Final
   release clears its timer and, when applicable, shared event listeners.

This bounds the fallback to at most the active tab in each window. Healthy
graph events still refresh immediately; a missed settled-to-active event
converges within 15 seconds.

## Component Contract

### `MessageListView`

Stop discarding `isActive`. Pass it to `LiveAwareSubAgentOverlay` and directly
rendered `SubAgentOverlay` instances.

### `LiveAwareSubAgentOverlay`

Accept `isActive` and forward it to `SubAgentOverlay`. Live transcript
selection and delegation reconciliation remain unchanged.

### `SubAgentOverlay`

Add an optional `isActive` prop with a default of `true` for existing direct
callers and tests.

- Overlay interest: valid positive conversation ID and `isActive`.
- Expanded interest: overlay interest, chip expanded, workflow segment
  selected, and full graph expanded.
- Chip collapse releases only expanded interest. It must not release overlay
  interest while the conversation remains active.
- Becoming inactive releases both interests.

The component may still acquire overlay interest before a workflow exists.
The existing 10-minute undiscovered-graph fallback remains responsible for
that case.

## Store Scheduling

`ActiveConversationRecord.overlayCount` represents active-surface interest
after the component contract above. After each authoritative refresh, choose
the next delay in this order:

1. 15 seconds when the cached graph contains active work;
2. 15 seconds when active overlay interest exists, even if the cached graph is
   discovered and settled;
3. 10 minutes for expanded-only interest or an undiscovered graph; or
4. no timer.

The existing per-conversation single-timer ownership, activation epoch,
request generation, and graph-revision gates remain unchanged. Fetch errors
under active overlay interest retry on the next 15-second interval. A late
fetch after release cannot re-arm the timer.

## Alternatives Considered

### Event recovery only

Retrying listener installation helps startup failures but cannot recover an
event that was emitted during a transient disconnect. It does not guarantee
convergence and is rejected as the sole fix.

### Poll every mounted conversation

This is mechanically simpler, but inactive tabs intentionally remain mounted.
It would make request volume scale with browsing history and is rejected.

### Poll only while cached state looks active

This is the previous behavior. It recovers active-to-settled misses but cannot
discover a settled-to-active transition, so it is the behavior being replaced.

## Error Handling

- Snapshot fetch errors remain on the store entry and do not change backend
  state.
- Event-subscription failures continue through the existing retry path while
  an active lease exists.
- Non-positive or missing conversation IDs never acquire interest.
- Switching tabs releases the old surface before its timer can re-arm; epoch
  guards reject late results.
- Direct overlay callers that omit `isActive` preserve current active behavior
  through the default value.

## Testing

Implementation follows test-driven development.

Component regressions prove:

- a collapsed active workflow chip acquires overlay interest but not expanded
  interest;
- collapsing an open graph releases expanded interest while retaining overlay
  interest;
- changing the surface to inactive releases both interests;
- both `MessageListView` rendering paths pass the real activity state through.

Store regressions prove:

- a settled numbered graph discovers a newer active revision after 15 seconds
  without an event;
- authority polling continues while the active surface remains interested,
  even after a later snapshot settles;
- releasing the final lease cancels the timer; and
- expanded-only and undiscovered 10-minute fallback behavior remains intact.

Targeted verification:

```text
pnpm test -- src/components/chat/workflow-overlay.test.tsx
pnpm test -- src/components/message/message-list-view.test.tsx
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Broader frontend verification:

```text
pnpm test
pnpm eslint .
pnpm build
```

No Rust verification is required because the design changes no Rust code,
backend contract, schema, or persistence behavior.

## Success Criteria

- Collapsing the workflow chip never makes the active conversation stop
  converging.
- With healthy event delivery, workflow changes still appear immediately.
- If a settled-to-active event is missed, the active conversation reflects
  authority within 15 seconds without reopening the window.
- Inactive and closed conversations own no polling timer.
- Existing request-generation and revision ordering protections remain intact.
