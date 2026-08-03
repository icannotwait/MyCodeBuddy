# Workflow Graph Live Refresh Design

Date: 2026-08-03
Status: Approved approach, pending implementation

## Context

The workflow overlay releases its graph interest when it is closed, switched
away, or unmounted. Events emitted during that inactive period are not
replayed. When the overlay becomes active again, a cached numbered
`graph_revision` currently suppresses the initial fetch, so the UI can retain
an old workflow state indefinitely.

Workflow cards now also display live delegation runtime statistics. Those
statistics are persisted without changing the durable workflow graph revision.
The existing `delegation_runtime_stats_changed` ACP event updates delegation
cards, but it is not projected into cached workflow nodes.

## Goals

- Reconcile a cached workflow graph immediately whenever overlay interest is
  reacquired after a fully inactive period.
- Update visible workflow node runtime statistics from existing ACP runtime
  events within the event delivery latency.
- Preserve task-generation isolation: an event may update only a node whose
  `latest_task_id` exactly matches the event `task_id`.
- Keep durable `graph_revision` semantics limited to durable workflow changes.
- Avoid periodic high-frequency graph projection and database reads.

## Non-goals

- Adding a new backend event or changing the ACP wire payload.
- Incrementing `graph_revision` for runtime counters.
- Replacing terminal, admission, gate, or manifest graph-change handling.
- Persisting frontend-derived runtime statistics.

## Design

### Activation reconciliation

`activateInterest` will fetch after the workflow event listeners become ready
whenever the conversation transitions from no active leases to at least one
active lease. This applies to both overlay and expanded interest, including
when a numbered snapshot is already cached.

Multiple concurrent leases in the same activation epoch continue to share the
single initial reconciliation. Moving from overlay-only to expanded interest
keeps the existing one-time expanded refresh behavior. Epoch checks continue
to prevent a late request from an old activation from mutating or scheduling
work for a reactivated conversation.

The ten-minute fallback policy remains unchanged. It is a safety net for
expanded graphs and undiscovered overlay graphs, not the mechanism for normal
reactivation convergence.

### Runtime event projection

The workflow graph store will expose an action that accepts a `task_id` and a
`DelegationRuntimeStats` snapshot. It will scan cached workflow snapshots and
replace runtime display fields only on nodes whose `latest_task_id` equals the
event task id:

- `tool_call_count`
- `edit_tool_call_count`
- `touched_file_count`
- `touched_files_truncated`
- `additions`
- `deletions`
- `line_counts_complete`

The action is replacement-based and idempotent. It does not alter
`graph_revision`, request generations, loading state, lifecycle status, or any
unrelated node. If no cached node matches, it is a no-op; a later activation
fetch supplies the durable snapshot.

`DelegationProvider` already receives the parent-stream
`delegation_runtime_stats_changed` event. After normal delegation reduction,
it will forward that event's task id and runtime snapshot to the workflow graph
store. This reuses the existing desktop/server event fanout and its current
runtime update cadence.

### Authoritative convergence

Live runtime projection is an optimistic display update over an already
persisted broker snapshot. Durable terminal and workflow events continue to
trigger graph refetches and remain authoritative. If the ACP event is missed,
closing and reopening the overlay reconciles from the backend immediately.

## Error Handling

- Listener installation failure still falls through the existing readiness
  timeout and performs the activation fetch.
- A failed activation fetch retains the cached snapshot and records the
  existing store error state.
- Unknown or stale task ids are ignored rather than applied by conversation or
  node position.
- Runtime events received before a graph is cached are ignored; initial or
  reactivation fetch recovers the current data.

## Tests

Frontend regression tests will cover:

1. A numbered graph is cached, all interest is released, the backend advances
   while inactive, and reacquiring overlay interest performs exactly one fetch
   and applies the newer snapshot.
2. Duplicate overlay leases in one activation epoch do not cause duplicate
   reconciliation requests.
3. A runtime event replaces all projected runtime fields for the exact latest
   task id without changing graph revision or request generation.
4. A stale or unknown task id cannot mutate a node from a newer generation.
5. `DelegationProvider` forwards runtime events to the workflow store while
   preserving the existing delegation binding reduction.

## Acceptance Criteria

- Reopening the workflow overlay shows backend changes made while it was
  inactive without waiting for another event or fallback timer.
- While the overlay remains open, tool calls, edit calls, touched-file count,
  and line totals update when the existing runtime event arrives.
- No new polling loop, backend event, or runtime-driven graph revision is added.
- Existing workflow lifecycle, event-ordering, and Strict Mode tests continue
  to pass.
