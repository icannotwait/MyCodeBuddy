# Workflow Graph Expanded Refresh Design

## Status

The design was approved in conversation on 2026-07-29. This written spec is
pending user review; implementation planning follows that review.

## Problem

The workflow graph displayed by a conversation can remain stale after the
durable workflow has advanced. Session 2381 demonstrated the failure: SQLite
and the Rust snapshot projector reported Task 4, while the open frontend still
showed Task 1.

The backend is not the stale-data source. The workflow row had a current
`graph_revision`, and the workflow snapshot endpoint projected the current
phase and nodes correctly. The frontend currently seeds its graph store from
conversation detail and installs transient event listeners, but mounting the
workflow UI does not reconcile with an authoritative snapshot. A
`workflow_graph://changed` event that occurred before listener registration,
during a page refresh, in another webview, or during subscription churn is not
replayed. The stale detail seed can therefore remain indefinitely.

The current overlay effect also combines detail seeding and subscription
mounting. A `workflowGraph` prop change tears down and reinstalls the event
subscription, creating avoidable event gaps.

## Goals

- Never issue workflow snapshot requests while the workflow graph is minimized.
- Fetch an authoritative snapshot immediately when the full workflow graph is
  expanded.
- While expanded, converge through newer `workflow_graph://changed` events and
  a ten-minute fallback refresh.
- Reset the ten-minute fallback after a successful event-driven refresh.
- Preserve the last graph while a refresh is in flight or fails.
- Preserve request-generation and graph-revision protections so stale responses
  cannot overwrite newer state.
- Stop subscription churn caused by conversation-detail updates.
- Keep the design correct under React Strict Mode and multiple mounted workflow
  views in one webview.

## Non-Goals

- Changing Rust workflow projection or persistence.
- Changing the `workflow_graph://changed` payload or backend event protocol.
- Adding a redundant turn-start or `refresh_nudge` event. A mapped continuation
  already emits graph changes when its run is admitted and when its accepted
  prompt promotes the run to `running`.
- Replaying transient workflow events.
- Refreshing unopened conversations or sharing Zustand state across webviews.
- Replacing the existing conversation-detail loading policy.

## Activation Boundary

The frontend treats a workflow graph as active only when all of these conditions
hold:

- the conversation ID is a positive persisted ID;
- the enclosing sub-agent overlay is not collapsed to its compact pill;
- the selected overlay segment is `workflow`; and
- the full graph is expanded through the existing "Expand workflow graph"
  control.

Collapsing the graph, switching to the Sessions segment, collapsing the whole
overlay, closing the tab, or closing the window deactivates the graph.

Codeg currently keeps opened inactive tabs mounted. This design preserves that
behavior: an opened tab whose workflow graph remains expanded stays active even
when another tab is selected. Separate windows have separate stores and manage
their own activation lifecycle.

## Component Responsibilities

`SubAgentOverlay` separates two independent effects:

1. A detail-seeding effect applies `workflowGraph` through `applyFromDetail`.
   Its dependencies may include the detail value, but it does not install or
   remove event listeners.
2. An activation effect derives the activation boundary above. When active, it
   calls a store activation API and returns that API's cleanup function. When
   inactive, it holds no workflow refresh interest.

Detail seeding may still update the cached graph while the UI is minimized.
This is passive reuse of data fetched for the conversation and is not a
workflow-panel snapshot request.

## Store Responsibilities

The workflow graph store owns all live refresh mechanics:

- a reference-counted active-conversation registry;
- one global pair of workflow event listeners per webview while at least one
  workflow graph is active;
- one fallback timer per active conversation;
- authoritative snapshot requests and existing generation/revision gates; and
- cleanup of conversation timers and global listeners.

The existing mounted-conversation semantics are narrowed. Merely having a
cached entry no longer authorizes an event-driven fetch. Only an active
conversation may react to workflow events. This is required so a minimized graph
cannot fetch just because conversation detail previously seeded its entry.

Multiple activations of the same conversation in one webview share the same
entry and timer. Only the zero-to-one activation transition performs the initial
refresh, and only the one-to-zero transition stops the timer. Each webview still
operates independently.

## Refresh Sequence

On the first activation of a conversation:

1. Register the conversation as active.
2. Install the global workflow listeners if they are not already installed.
3. Wait for listener registration to settle.
4. If the same activation is still current, immediately request the
   authoritative workflow snapshot.
5. Continue displaying the previous snapshot throughout the request.

Subscription readiness precedes the initial snapshot request so a mutation that
commits after readiness cannot fall into a listener-before-fetch gap. If
subscription setup fails, the initial request still runs and the graph degrades
to refresh-only operation.

After an accepted successful snapshot response, schedule the next fallback
refresh for ten minutes later. A fallback request follows the same application
and scheduling path.

## Event Handling

While a conversation is active, a `workflow_graph://changed` event with a
revision greater than the applied revision:

1. cancels the pending fallback timer;
2. starts an immediate authoritative snapshot request; and
3. schedules a fresh ten-minute fallback after the new snapshot is successfully
   applied.

Equal or lower revisions remain ignored and do not alter the timer. Events for
inactive conversations are ignored without fetching, even if the store contains
a detail-seeded snapshot.

`workflow_graph://compatibility_nudge` has no durable revision. While the
conversation is active, it starts the same immediate authoritative refresh and
resets the fallback after success. While inactive, it is ignored without
fetching.

The event remains a live clock notification rather than graph data. The
authoritative snapshot request is still the only event-driven source of the full
graph.

## Concurrency And Cleanup

Existing request-generation and graph-revision checks remain authoritative:

- a newer request generation supersedes older requests;
- an older graph revision cannot overwrite a newer applied snapshot; and
- late subscription completions from a disposed Strict Mode mount are disposed
  through the existing event-install generation token.

Deactivation cancels the fallback timer and removes event-fetch eligibility. It
does not attempt to abort an already-dispatched Tauri or HTTP request. A late
response may update the retained cache if it still passes the generation and
revision checks, but it must not schedule another timer while the conversation
is inactive. A later expansion always performs a new immediate refresh.

If a graph is deactivated while listener setup is pending, the activation token
prevents the post-readiness initial refresh. When the final active conversation
deactivates, the global listeners are disposed.

## Failure Handling

A failed snapshot request retains the previous snapshot and records the existing
store error state. It must not clear the graph or enter a tight retry loop.

If the conversation remains active after failure, schedule the next fallback
attempt ten minutes after that failed attempt completes. A newer graph event may
retry sooner. Collapsing and expanding also triggers an immediate retry.

Listener registration failure does not block the initial or periodic requests.
Conversely, a transient event cannot cause any request while the graph is
inactive.

## Testing

Focused store tests use fake timers and controllable subscription/request
promises to verify:

- activation waits for subscription readiness and then fetches immediately;
- inactive or detail-seeded conversations do not fetch on graph events;
- an active conversation fetches every ten minutes;
- a successful event-driven refresh resets the ten-minute timer;
- equal and lower event revisions neither fetch nor reset the timer;
- compatibility nudges fetch only while active and reset the timer on success;
- deactivation clears the timer and prevents late responses from rearming it;
- rapid deactivate/reactivate and Strict Mode subscription completion do not
  leak listeners or run stale activation callbacks;
- request-generation and graph-revision races still reject stale responses;
- failed subscription setup retains initial and periodic refresh behavior; and
- failed requests retain the old graph and retry only at the next allowed time.

Focused overlay tests verify:

- collapsed graph, Sessions segment, and collapsed overlay do not activate;
- expanding the workflow graph activates exactly once;
- switching away or collapsing returns the activation cleanup;
- detail prop updates apply the seed without reinstalling subscriptions; and
- remounting with a new conversation moves activation to the correct ID.

## Acceptance Criteria

- Opening the full workflow graph for session 2381 immediately projects the
  current Task 4 snapshot without requiring a page reload.
- No workflow snapshot request originates from the panel while any defined
  minimized state is active.
- An expanded graph updates promptly on a newer backend event.
- If all events are missed, an expanded graph converges within ten minutes.
- Event-driven success delays the next fallback by a full ten minutes.
- Existing snapshots remain visible through loading and transient failure.
- No Rust source or workflow event contract changes are required.
