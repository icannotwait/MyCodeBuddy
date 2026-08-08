# Workflow Refresh Self-Healing Design

## Status

Direction approved in the 2026-08-08 design discussion. The selected approach
adds durable-state reconciliation for delegation cards and workflow graphs
without changing broker or workflow persistence protocols.

## Incident

A delegated run can finish successfully and commit its terminal state while
the currently open window continues to render the run, or its workflow node,
as running. Closing and reopening the window fixes the display.

The observed incident establishes that:

- the child runs reached terminal state;
- the delegation run rows and workflow graph revision were committed;
- the parent orchestration consumed the child results and continued; and
- a cold window load rendered the durable terminal state correctly.

The failure is therefore in frontend convergence, not task execution or
database settlement.

Two frontend behaviors make a transient event miss permanent:

1. A live `DelegationBinding` or live delegation meta with status `running`
   has unconditional precedence over an immutable terminal run snapshot.
2. An overlay-only workflow graph with a known `graph_revision` disables its
   fallback refresh and depends exclusively on transient graph events.

Workflow event subscription failures are also swallowed, so the window has no
observable signal that it has entered refresh-only operation.

## Decision

Use events for low-latency updates and durable snapshots for convergence.

The implementation has three parts:

1. A terminal delegation run snapshot for the exact same task may close a
   stale running binding or running live meta source.
2. A visible workflow graph with active work receives a 15-second authority
   refresh until its active work settles, even when it already has a numbered
   graph revision.
3. Failed required workflow event subscriptions are logged and retried while
   at least one workflow graph interest lease remains active.

No backend schema, event payload, API, or persistence change is required.

## Goals

- Make a missed delegation completion event converge from the existing
  immutable run-snapshot query within 15 seconds.
- Make a missed workflow graph event converge from the authoritative graph
  snapshot query within 15 seconds while work is active.
- Preserve immediate event-driven refresh when delivery is healthy.
- Retry failed graph-changed and compatibility-nudge subscriptions without
  duplicate listeners.
- Stop timers and listeners when the final graph interest lease is released.
- Preserve run identity isolation across later continuations that share a
  child conversation.

## Non-Goals

- Replacing ACP event delivery or the Tauri event plugin.
- Adding durable event replay to the backend.
- Polling every historical or closed conversation.
- Changing workflow settlement, graph revisions, or delegation run storage.
- Letting a snapshot for one task change another task's card.
- Resolving contradictory terminal outcomes from two sources. Existing live
  terminal precedence remains unchanged.

## Considered Approaches

### 1. Durable reconciliation plus subscription retry

Keep healthy events as the fast path, allow exact-run durable terminal data to
close stale card state, poll only active visible workflows, and retry failed
required listeners. This bounds stale display time while retaining current
event efficiency. This is the selected approach.

### 2. Polling only

Polling every visible workflow and delegation card would converge without
listener changes, but it would hide subscription failures and create needless
requests after work settles. This approach is rejected.

### 3. Durable event replay protocol

A cursor-backed replay stream could guarantee event delivery, but it requires
new frontend and backend protocol state and migration across desktop, server,
and remote transports. It is disproportionate to this frontend convergence
bug and is deferred.

## Delegation Card Convergence

### Identity gate

The durable snapshot may replace a higher running source only when all
available run identities agree:

- binding path: `binding.taskId === runSnapshot.task_id`;
- meta path: `parsedMeta.taskId === runSnapshot.task_id`; and
- snapshot status is `completed`, `failed`, or `canceled`.

A missing or mismatched task ID fails closed. The existing
`scopeDelegationBindingForCard` gate remains in place so a later continuation
cannot attach its live binding to an earlier card.

### Precedence

The effective source rules become:

1. live terminal binding;
2. live terminal meta;
3. exact-run durable terminal snapshot over a running binding/meta;
4. live running binding or meta;
5. non-terminal durable snapshot;
6. existing child projection and tool-call fallbacks.

The implementation should derive effective binding and meta inputs before
field merging. When an exact terminal snapshot closes a stale running source,
the stale source is omitted for all run-scoped fields, not just the badge.
This lets status, runtime stats, finish time, error code, attention clear, and
card summary converge as one coherent terminal record.

Conversation-scoped identity such as the child conversation remains available
from the run snapshot. A terminal card does not need the stale live child
connection ID.

### Cache behavior

The existing `useDelegationRunSnapshot` 15-second refresh remains unchanged.
It already continues querying until a terminal snapshot is installed. The fix
changes how that terminal record participates in the card merge.

## Workflow Graph Convergence

### Fast reconciliation eligibility

A graph needs the 15-second authority refresh while any node is in one of
these active states:

- `reserving`;
- `running`;
- `waiting_review`; or
- `waiting_adjudication`.

`overall_state === "in_progress"` also qualifies, protecting skeleton or
partially projected graphs whose node list temporarily lacks the active row.

When no active state remains:

- overlay-only interest does not keep the fast timer;
- undiscovered graphs retain the existing 10-minute discovery fallback; and
- expanded-graph interest retains the existing 10-minute fallback behavior.

The normal `workflow_graph://changed` and compatibility-nudge handlers remain
the immediate path. Each refresh continues to apply snapshots through the
existing request-generation and `graph_revision` gates.

### Timer ownership

Each conversation continues to own at most one fallback timer through its
`ActiveConversationRecord`. The delay is selected after every authoritative
refresh:

- 15 seconds for active workflow state;
- 10 minutes for the existing discovery/expanded fallback; or
- no timer when overlay-only state is settled and already discovered.

Releasing the final interest lease clears the timer. Late fetch completion is
still rejected by the activation epoch guard and cannot re-arm a released
conversation.

## Event Subscription Recovery

The graph-changed and compatibility-nudge subscriptions are required for the
event fast path. Each channel tracks whether it is subscribed or currently
subscribing.

On failure:

- emit one frontend warning per channel per install generation, containing the
  channel name and normalized error;
- retain any successfully installed sibling listener;
- schedule a retry after 5 seconds if graph interest still exists; and
- retry only missing, non-pending channels.

One shared retry timer prevents duplicate retry loops when both subscriptions
fail together. Repeated retry failures do not repeat the warning; a successful
subscription resets that channel's warning latch. A successful subscription
stores exactly one dispose handle. The existing install-generation token
rejects and disposes late results from a previous mount. Final lease release
clears the retry timer, warning latches, and pending state in addition to
disposing listeners.

The optional completion-decision listener retains its existing compatibility
behavior and does not keep the required-listener retry loop alive.

## Error Handling

- Snapshot fetch errors remain stored on the graph entry and are retried by
  the next eligible fallback or event.
- A listener retry failure never disables durable polling.
- A late subscription success after disposal immediately invokes its returned
  unsubscribe function.
- Conflicting or mismatched task snapshots never close a live card.
- Terminal live sources are never reopened or replaced by a lower source.
- No timer or listener failure changes backend task state.

## Testing

Implementation follows test-driven development.

Delegation card regression tests prove:

- a matching completed snapshot closes a stale running binding and supplies
  coherent terminal fields;
- a matching failed snapshot closes stale running live meta as an error;
- a mismatched terminal snapshot cannot close a running binding; and
- live terminal state continues to outrank lower running data.

Workflow graph store regression tests prove:

- an active numbered overlay graph refetches after 15 seconds without an
  event and applies the newer terminal revision;
- the fast fallback stops after the fetched graph settles;
- a settled numbered overlay graph does not start a fast timer;
- expanded and undiscovered 10-minute fallback behavior remains intact;
- failed required subscriptions retry after 5 seconds;
- successful sibling listeners are not duplicated during retry; and
- final lease release cancels pending retry and refresh timers.

Targeted verification:

```text
pnpm test -- src/hooks/use-delegation-card-model.test.ts
pnpm test -- src/lib/workflow-graph-store.test.ts
```

Broader frontend verification:

```text
pnpm test
pnpm eslint .
pnpm build
```

No Rust command is required because the selected design does not modify Rust
code or backend contracts.

## Success Criteria

- Missing one completion or graph event no longer requires a window restart.
- A running delegation card converges within one existing 15-second snapshot
  interval after durable settlement.
- A visible active workflow graph converges within one 15-second authority
  interval after durable settlement.
- Healthy event delivery remains immediate.
- Settled overlay-only workflows do not poll continuously.
- No cross-task or cross-generation state contamination is introduced.
