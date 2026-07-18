# Event-Driven Delegation Join Design

## Status

Approved in conversation on 2026-07-17.

This specification extends
`docs/superpowers/specs/2026-07-16-delegation-route-reliability-design.md`.
That design remains authoritative for route selection, durable task lifecycle,
soft-watchdog observation, terminal persistence, and restart reconciliation.
This document changes how a parent Codeg agent waits for those tasks, adds the
single V1 child-to-parent attention path, and adds runtime-derived information
to the existing Delegation Card.

No implementation plan has been approved yet. The implementation plan must
preserve the contracts in this document.

## Problem

`delegate_to_agent` starts children asynchronously, which is necessary for
parallel fan-out. The current collection protocol, however, returns a batch
wait when any requested task becomes terminal or actionable. The parent model
must then decide whether and when to call `get_delegation_status` again. In
practice this encourages repeated short waits and consumes model steps merely
to discover that the remaining children are still running.

Completion events already reach `DelegationBroker`, and the Broker already has
a `Notify` used by status waits. The missing contract is a mandatory Join that
keeps the parent model suspended until either every requested child has
finished or a child has an actual decision that only its parent can make.

The UI has a related but separate need. While the parent model is suspended,
the user should still see that a child is active and what observable work it
has performed. Elapsed time, tool-call count, and structured edit metadata can
be derived from runtime events; asking the child model to narrate them would
add cost, inconsistency, and another control channel.

## Goals

- Preserve asynchronous child startup and parallel fan-out.
- Make Join the only supported V1 ownership mode for Codeg Broker tasks.
- Suspend the parent model without short, model-driven polling loops.
- Resume the same parent turn when all joined tasks are terminal, a requested
  child needs a parent decision, or the Broker detects that it cannot safely
  remain parked.
- Keep task lifecycle, observation, and attention as separate concepts.
- Make completion and attention wakeups race-safe and snapshot-recoverable.
- Cancel live children when their parent turn can no longer own the Join.
- Show elapsed time, tool-call count, and detected edit activity in the
  existing Delegation Card without LLM-authored progress messages.
- Preserve the existing tool wire behavior for callers that do not opt into
  the new Join return condition.

## Non-Goals

- A Detach or fire-and-forget mode.
- Generic child-authored progress, warning, log, or heartbeat reports.
- Starting a new parent LLM turn after the original parent turn has ended.
- Waking the parent model for ordinary `active`, `stalled`, or
  `waiting_input` observation changes.
- Changing the durable task states or making `stalled` terminal.
- Replacing the existing permission and `ask_user_question` UI paths.
- Attributing arbitrary shared-worktree changes to a particular child.
- Parsing shell command text to infer file modifications.
- Applying these guarantees to platform-native sub-agents that are not owned
  by `DelegationBroker`.
- Automatically retrying, replacing, reconnecting, or rerouting a child.

## Selected Approach

Codeg uses an event-driven, Broker-owned Join:

1. The parent starts one or more children with `delegate_to_agent`; each call
   still returns a `running` acknowledgement and `task_id` immediately.
2. The parent fans out every independent child before collecting results.
3. The parent issues one batch, infinite Join through
   `get_delegation_status`.
4. The MCP tool call remains open, so the parent model remains suspended in
   its current turn.
5. Child lifecycle and attention events notify the Broker. The Broker
   re-evaluates durable state and completes the tool call only when the Join
   predicate is true.
6. The parent model continues in the same turn with either every final result
   or the open attention requests that require action.

The canonical call is:

```text
get_delegation_status({
  task_ids,
  wait_ms: 0,
  return_when: "all_terminal_or_attention"
})
```

This is intentionally not a direct child-to-parent message injection. A child
completion first settles in the Broker, then releases the already-pending MCP
tool call. No `triggerTurn`-style synthetic parent turn is needed.

## Alternatives Considered

### Repeated bounded status waits

Rejected as the normal coordination path. It preserves compatibility and
remains useful for explicit diagnostics, but every returned running snapshot
requires another model decision. It spends inference on scheduling and makes a
slow child look like a timeout even when no failure occurred.

### Direct child message with parent turn triggering

Rejected. A pushed message can arrive while the parent is already generating,
after it has ended, or concurrently with another child. Correctly deduplicating
and ordering synthetic turns would be harder than releasing the tool call that
already owns the wait.

### Detach plus later notification

Rejected for V1. The product contract is that a parent which delegates work
must collect and incorporate the result before ending its turn. Detach would
require a second ownership model, later-turn routing, orphan retention, and a
separate user-facing result-delivery policy.

### LLM-authored progress and warning reports

Deferred. They can communicate semantic context that runtime events cannot,
but require prompt policy, rate limiting, wake policy, rendering, persistence,
and abuse controls. V1 exposes only a blocking decision request and derives
operational statistics from structured runtime data.

## Core Invariants

### Task lifecycle

The authoritative lifecycle remains:

```text
running -> completed | failed | canceled
```

`unknown` remains a scoped query result rather than a stored task state.

### Observation

The existing running-only observation remains:

```text
active | stalled | waiting_input
```

Observation updates the UI and bounded supervised waits. It does not complete
an `all_terminal_or_attention` Join. Existing permission and user-question
flows continue independently; resolving them lets the child continue without
resuming the parent model.

### Attention

Attention is a persisted overlay on a running task, not a lifecycle or
observation value:

```text
open -> resolved
```

An open request makes a Join actionable. Resolving it never makes the task
terminal; it only lets the blocked child continue.

### Parent ownership

A running Codeg child must have a live parent turn that is executing or blocked
in an owned tool call, normally Join and occasionally the existing user-question
flow while resolving child attention. Page navigation, changing the selected
conversation, or frontend reattachment does not change that ownership. Parent
cancellation, turn failure, clean `end_turn` with live children, connection
teardown, and host restart do.

## Component Responsibilities

### `codeg-mcp` companion

- Advertises the additive Join input field and role-appropriate attention
  tools.
- Converts MCP calls to Broker requests without implementing polling or task
  state locally.
- Keeps the parent `get_delegation_status` call and child
  `request_parent_decision` call open until the Broker replies.
- Treats cancellation of an individual status call as cancellation of that
  call only.

### `DelegationBroker`

- Owns task-to-parent and child-to-task authorization.
- Evaluates Join predicates from authoritative task and attention state.
- Uses notifications only to prompt predicate re-evaluation.
- Persists, resolves, and recovers attention requests.
- Wakes the parent Join and child decision waiters.
- Applies parent-turn cancellation reasons to live child tasks.
- Publishes task, attention, and runtime-stat updates for the existing card.

### ACP lifecycle and `SessionState`

- Routes child terminal events into the existing task settlement path.
- Projects stable child tool-call events into runtime statistics.
- Applies parent turn-end policy before allowing live children to become
  unowned.
- Carries active task, attention, and runtime-stat state in snapshots so a
  frontend can reattach without event replay.

### Frontend Delegation Card

- Uses events for low-latency updates and snapshots or persisted metadata for
  recovery.
- Computes the displayed running duration locally from timestamps.
- Renders operational statistics and attention on the existing card.
- Does not synthesize child chat messages from operational events.

## Tool and Wire Contracts

### `delegate_to_agent`

Startup remains asynchronous. No Join flag is added to the creation call. This
keeps fan-out ergonomic and avoids serializing child launches.

An accepted `running` response means the accepted boundary defined in the
route-reliability design has been crossed. Tool guidance changes to require the
parent to fan out independent work, then Join every still-required task before
ending its turn.

### `get_delegation_status`

The input schema gains:

```text
return_when?: "all_terminal_or_attention"
```

V1 accepts this value only with an explicit `wait_ms=0`. Omitting
`return_when` preserves all current behavior:

- omitted `wait_ms` returns an immediate snapshot;
- positive `wait_ms` remains a bounded supervised wait; and
- `wait_ms=0` returns under the current any-terminal semantics.

With `return_when=all_terminal_or_attention`, the call returns when the first
of these predicates is true:

1. every requested task has a terminal report;
2. at least one requested running task has an open attention request; or
3. the Broker cannot safely continue waiting for at least one requested task,
   such as an unknown task or a persisted `running` task with no live notifier.

The additive response shape is:

```text
{
  tasks: DelegationTaskReport[],
  wake_reason?: "all_terminal" | "attention_required" | "unavailable",
  attention_requests?: DelegationAttentionRequest[]
}
```

`tasks` remains in input order. The two new fields are present for the new Join
mode and absent for legacy calls. `attention_requests` contains every open
request belonging to the requested task set, ordered by creation time.

`unavailable` is a fail-open result for the parent model, not a task lifecycle
state. It prevents an infinite wait when no producer can issue a future
notification. The returned task reports explain which item is unknown,
terminal, or persisted-running without a live owner.

When attention wakes the call, the parent:

1. retains any terminal results included in `tasks`;
2. answers every actionable request it can answer;
3. uses the existing user-question path if a decision genuinely belongs to the
   human; and
4. enters the same Join mode again with only the still-running required task
   ids.

Narrowing the second Join prevents completed result text from being injected
into the parent context repeatedly.

### `request_parent_decision`

The child-facing input is:

```text
request_parent_decision({ message: string })
```

The Broker derives the caller's task and direct parent from authenticated
launch context. The child cannot supply an arbitrary `task_id` or parent id.
Root sessions have no parent and do not receive this tool.

The call:

1. creates or recovers an idempotent attention request;
2. persists it before notifying the parent Join;
3. blocks the child tool call; and
4. returns the parent's reply when the request is resolved normally.

If the task, parent turn, connection, or host ends first, the call returns or
is torn down with the stable closure reason instead of waiting forever.

### `reply_to_delegation`

The parent-facing input is:

```text
reply_to_delegation({ request_id: string, reply: string })
```

The Broker verifies that the caller owns the request's direct child task and
that the request is still open. It persists the resolution before notifying
the child waiter. Repeating the same reply is idempotent. A conflicting reply
to an already-resolved request returns `already_resolved` and never replaces
the original reply.

A non-root child may be both a child and a parent: it can request a decision
from its direct parent and reply to requests from its own direct children.
Authorization follows persisted direct edges only; there is no sibling or
arbitrary ancestor communication.

### Payload bounds

`message` and `reply` are each limited to 16 KiB UTF-8. The Broker rejects an
oversized payload before persistence. A task may have at most one open request
in V1. These bounds keep a blocked control call from becoming an unbounded
transcript or database channel.

## Join Wait Algorithm

`tokio::sync::Notify` is not durable and does not queue a permanent permit for
every waiter. It is therefore a hint, never the condition itself. The Join loop
uses the existing arm-before-snapshot pattern:

```text
loop:
  arm result_notify.notified()
  read authorized task classes and persisted attention state
  assemble reports

  if any requested task cannot be waited on:
    return unavailable

  if any requested running task has open attention:
    return attention_required

  if every requested task is terminal:
    return all_terminal

  await notification
```

Arming before the snapshot prevents a task completion between snapshot and
await from being lost. Re-evaluating the predicate after every wake prevents
spurious or unrelated notifications from completing the MCP call.

A terminal child wakes the internal loop. If a sibling is still running and no
attention is open, the loop parks again without returning a tool result to the
parent model. The final terminal child causes one batch result containing the
reports for all requested tasks.

Only attention requests for task ids in the current Join may complete it. An
unrelated parent's task or a child excluded from the request set can wake the
shared `Notify`, but the predicate fails and the Join parks again.

Canceling the pending MCP status request drops this waiter only. It does not
call `cancel_delegation`, disconnect children, or alter persisted task state.
If that cancellation is part of canceling the whole parent turn, the separate
parent-turn handler performs the child cascade.

## Attention Persistence and Lifecycle

A new `delegation_attention_requests` table stores:

```text
request_id             TEXT PRIMARY KEY
task_id                TEXT NOT NULL
parent_conversation_id INTEGER NOT NULL
child_conversation_id  INTEGER NOT NULL
child_tool_call_id     TEXT NOT NULL
status                 TEXT NOT NULL  -- open | resolved
message                TEXT NOT NULL
reply                   TEXT NULL
resolution_code         TEXT NULL
created_at              DATETIME NOT NULL
resolved_at             DATETIME NULL
```

Required constraints are:

- unique `(task_id, child_tool_call_id)` for replay idempotency;
- at most one `open` row per task;
- `task_id` must resolve to the child conversation's
  `delegation_call_id`; and
- the stored parent must match the child's persisted `parent_id`.

For a normal reply, `resolution_code=parent_reply` and `reply` is non-null.
Non-reply closure uses one of:

```text
task_terminal
parent_canceled
parent_turn_failed
join_abandoned
parent_disconnected
host_restarted
```

Those closures still use `status=resolved`; `reply` remains null. This keeps
the overlay to the approved two states while making teardown auditable.

The order for opening a request is persist, update snapshot/event state, then
notify the parent. The order for resolving it is persist, clear snapshot/event
state, then notify the child. Event loss cannot create a different state from
a later snapshot.

Although a blocked child cannot normally finish cleanly with an open request,
cancellation and teardown can race with replies. A conditional
`open -> resolved` update chooses one winner. Losing paths observe the stored
resolution and do not send a second reply or event.

Startup reconciliation resolves every request still open for a reconciled
running task with `host_restarted`. This occurs in the same startup phase that
settles the task as `failed/host_restarted`.

## Runtime Statistics Projection

Runtime statistics are operational observations, not LLM reports and not
authoritative repository accounting:

```text
started_at
finished_at?
tool_call_count
edit_tool_call_count
touched_files[]
touched_files_truncated
additions?
deletions?
line_counts_complete
```

### Tool-call identity

Each task keeps an in-memory map keyed by the child's stable ACP
`tool_call_id`. The first appearance increments `tool_call_count`. Start,
update, completion, transcript replay, and frontend reconnect events carrying
the same id do not increment it again.

Later updates may enrich that id's contribution with tool kind, locations, or
diff metadata. The projector replaces the previous contribution and applies a
delta to the rollup; it does not add the full contribution again. A tool call
without a stable id is not counted because inventing an ordinal would inflate
counts on replay.

All child-visible tool calls count, including MCP and nested delegation calls.
Edit calls are a subset of this total.

### Edit detection

A call is a detected edit only when structured metadata establishes mutation:

- the normalized tool kind is edit, write, create, delete, move, rename, or
  patch; or
- a structured result reports an applied patch or mutation diff.

`locations` supplies affected paths only after a call is classified as an
edit. A location by itself does not prove mutation because read and search
tools also carry locations.

The projector never:

- parses shell command text;
- assumes every shell call changed files;
- scans filesystem timestamps;
- runs `git diff` to assign shared-worktree edits to a child; or
- claims that a detected edit still exists in the final working tree.

Consequently a file written only through an opaque shell command may not be
listed. The UI wording must remain "detected edits" rather than "files
changed".

### Paths and line counts

Paths are lexically normalized without requiring the file to still exist.
Paths within the child working directory are stored for display relative to
that directory. Outside paths retain a normalized absolute display value and
an outside-workspace marker. Windows deduplication is case-insensitive while
preserving the first observed display casing.

The compact rollup keeps at most 200 unique display paths. Observing another
unique path sets `touched_files_truncated=true`; the projector does not retain
an unbounded overflow set merely to calculate an exact total. The UI renders
the count as `200+` and the expanded card explains that the path list is
truncated.

Additions and deletions are summed from structured patch or diff metadata per
stable tool call. The aggregate is displayed only when at least one edit has
line counts and every detected textual edit has usable counts. Otherwise
`line_counts_complete=false` and the aggregate is omitted rather than shown as
zero or as a misleading partial total. These totals describe observed edit
activity, not the net repository diff.

### Persistence

Delegate conversation rows gain compact rollup fields. They remain null for
non-delegate and pre-feature historical rows; a newly accepted delegate row
initializes counts to zero, the path list to empty, and booleans to false:

```text
delegation_tool_call_count       INTEGER NULL
delegation_edit_tool_call_count  INTEGER NULL
delegation_touched_files_json    TEXT NULL
delegation_touched_files_truncated BOOLEAN NULL
delegation_additions             INTEGER NULL
delegation_deletions             INTEGER NULL
delegation_line_counts_complete  BOOLEAN NULL
```

Codeg does not parse old transcripts to fabricate historical rollups. Cards
for rows with null rollup fields omit the statistics segment.

The in-memory contribution map is needed only while the task is live. A
process restart settles live tasks, so no post-restart event stream can update
an old task. Rollup writes may be coalesced, but terminal settlement must flush
the latest projection before publishing the terminal card event. A statistics
write failure must not block lifecycle settlement; the card may under-report
and the failure is recorded in diagnostics.

## Event and Snapshot Contract

The existing event and snapshot model is extended instead of creating a new
progress feed.

`DelegationStarted` and `ActiveDelegationState` add:

```text
task_id
started_at
runtime_stats
attention_request?
```

Two replay-safe parent-stream events update the active card:

```text
DelegationRuntimeStatsChanged {
  parent_tool_use_id,
  task_id,
  runtime_stats
}

DelegationAttentionChanged {
  parent_tool_use_id,
  task_id,
  attention_request?
}
```

`attention_request=Some` opens or replaces the displayed request;
`attention_request=None` clears it. Applying either event more than once is
idempotent because it replaces the projection for the identified task.

`DelegationCompleted` and persisted `codeg.delegation` metadata carry the
final timestamps and runtime rollup. A live page receives low-latency events;
a cold load or reattachment reconstructs the same card from the connection
snapshot and child conversation metadata.

Events update the UI independently of whether the parent model is blocked.
They do not append chat content and do not themselves trigger an LLM turn.

## Delegation Card UX

No new progress card or chat bubble is introduced. The existing Delegation
Card gains one compact operational line, for example:

```text
Running 3m24s | 18 tool calls | Detected edits in 4 files  +126 -34
```

The actual localized UI uses the application's existing separators and icon
system. The requirements are:

- running duration is `now - started_at`;
- terminal duration is fixed at `finished_at - started_at`;
- one shared one-second frontend ticker updates visible running cards rather
  than one timer per card, and stops when no visible card is running;
- tool and edit counts update from projection events or snapshots;
- when edit calls are detected without usable paths, the card shows the
  detected edit-call count instead of claiming a file count;
- when paths are available, the file count is the unique retained count and is
  rendered as `200+` if the list was truncated;
- unavailable line totals are omitted, not rendered as `+0 -0`;
- the compact card shows counts only;
- the existing expanded card shows retained paths, outside-workspace markers,
  available line details, and a truncation notice; and
- every new label is translated in all ten supported locales.

An open decision request gives the existing card a distinct attention state
and label such as "Waiting for parent decision". It is not a human reply
control. The parent model receives the request through the Join result. If the
parent needs a human-owned decision, it uses the existing question tool and UI.

Existing `stalled`, `waiting_input`, permission, and question treatments
remain visible through their current UI paths. They do not masquerade as a
parent-decision request.

## Parent Turn and Cancellation Policy

Join-only ownership changes the current behavior that allows a normal parent
`end_turn` to leave Codeg children running.

### User cancellation

When the user cancels the parent turn, the parent stop reason remains
`cancelled`. Broker cascade-cancels every live direct or nested child with
`error_code=parent_canceled`, resolves open attention requests with the same
reason, and disconnects child processes through the existing teardown path.

### Model abandons Join

If the parent emits a clean `end_turn` while it still owns any running Codeg
task, lifecycle accepts that the parent turn has ended but classifies the live
children as abandoned. Broker cancels them with
`error_code=join_abandoned`, resolves attention requests, emits terminal card
updates, and records an abandonment diagnostic.

Codeg does not automatically trigger another parent turn. There is no pending
model computation to resume, and silently spending another inference turn
would violate the Join ownership contract.

If every child is already terminal when `end_turn` is processed, there is no
live child to abandon and no cancellation occurs. Codeg can enforce live-task
ownership but cannot prove that a model semantically incorporated a result it
obtained earlier.

### Parent turn failure

Refusal, max-token, max-turn, and other non-user failure stop reasons cancel
live children with `error_code=parent_turn_failed`. This is separate from a
user cancellation and from a clean but premature `end_turn`.

### Connection teardown and restart

Connection teardown continues through `cancel_by_parent`; its stable closure
reason is `parent_disconnected` where the teardown can be distinguished.
Application restart uses the existing `failed/host_restarted` task
reconciliation and resolves open attention requests with `host_restarted`.

### Page and session navigation

Changing the selected page, tile, or conversation does not send a parent
cancel signal and has no Broker effect. The parent MCP call, child processes,
events, and snapshot state continue independently of frontend selection.

## Race Handling

### Completion versus cancellation

The existing conditional `running -> terminal` persistence remains the sole
winner. If completion wins, a later parent cancellation returns the completed
report. If cancellation wins, a late child completion cannot replace it or
emit a second terminal event.

### Attention versus task completion

Opening attention requires the task still to be running. Resolving attention
and settling the task use conditional writes. A terminal winner closes any
open request; a late reply observes `already_resolved` or `task_terminal`.

### Reply versus parent cancellation

Both attempt `open -> resolved`. The first durable transition wins. If the
reply wins just before cancellation, the child tool may receive it but the
subsequent task cancellation still stops the child. No second resolution is
published.

### Event before wait registration

Join evaluates persisted state after arming its notification future. An
already-terminal task or already-open attention request returns immediately,
so no event history is required to enter the correct state.

### Multiple child completions

Every terminal transition may wake the shared internal notifier. The Join
predicate returns only after all requested reports are terminal. Notifications
can be coalesced without losing correctness.

### Multiple attention requests

Each task has at most one open request, but several joined tasks can request
attention concurrently. The response includes every open request visible in
the same snapshot. A request created after that snapshot is returned
immediately when the parent re-enters Join.

## Security and Isolation

- Every status, Join, cancel, request, and reply operation is scoped to the
  authenticated connection and persisted parent-child relationship.
- Unknown and foreign task ids both return `unknown`; the response does not
  reveal whether another parent owns the id.
- `request_parent_decision` derives ownership from companion launch context and
  accepts no destination id.
- `reply_to_delegation` cannot address siblings, grandchildren, or another
  root's children.
- Attention payload limits are enforced before database writes and event
  emission.
- Runtime statistics inspect only already-normalized ACP event metadata. They
  do not read files merely to populate UI statistics.
- File paths are visible only wherever the existing child conversation and
  Delegation Card are already visible.

## Observability

Existing delegation metrics gain low-cardinality counters and histograms for:

```text
join_started
join_returned{reason=all_terminal|attention_required|unavailable|canceled}
join_duration_ms
join_abandoned
attention_opened
attention_resolved{reason}
attention_open_duration_ms
runtime_stats_projection_error{kind}
```

Structured diagnostic logs may include task, request, parent connection, and
child connection ids. Metric labels must not include those high-cardinality
ids, file paths, prompts, replies, or agent output.

The Broker records spurious notification wakes only in debug or sampled trace
output; they are expected under a shared notifier and are not failures.

## Compatibility and Rollout

The rollout is additive:

1. Add the database migration for attention and runtime rollups.
2. Add internal attention, Join predicate, and runtime projection support.
3. Extend Broker/companion wire types and role-specific tool schemas.
4. Update tool guidance so Codeg parents use one batch Join.
5. Extend events, snapshots, persisted delegation metadata, and TypeScript
   mirrors.
6. Extend the existing Delegation Card and translations.
7. Enable the new guidance after the host and per-launch companion advertise
   the same capability version.

Old callers that omit `return_when` retain current snapshot, bounded wait, and
any-terminal behavior. New fields are optional on the wire during the rollout.
A new companion must not advertise decision tools or Join guidance when the
host capability handshake does not support them.

Rollback can stop advertising the new tools and mode without rewriting task
rows. Unknown additive metadata remains inert. Capability removal follows the
existing immutable connection-boundary rule: a live connection that accepted
tasks keeps its launch-time capability until normal teardown, which resolves
open requests. Codeg does not hot-downgrade an active connection and strand its
waiters.

## Testing Strategy

### Broker unit tests

- Join returns immediately when every requested task is already terminal.
- One completed child does not return while a requested sibling is running.
- Children completing out of order produce one final batch in input order.
- Attention before wait registration returns immediately.
- Attention after parking releases the Join.
- Observation and unrelated-task notifications do not release the Join.
- Unknown or persisted-running-without-notifier returns `unavailable`.
- Canceling the status waiter does not cancel a task.
- Completion, cancellation, reply, and attention races produce one durable
  winner and one terminal or resolution event.

### Attention tests

- Root sessions cannot call `request_parent_decision`.
- A child cannot choose a different parent or task.
- A parent cannot reply to a foreign request.
- Duplicate child tool-call replay returns the same request.
- Repeating the same reply is idempotent; a conflicting reply is rejected.
- One-open-request-per-task and payload limits are enforced.
- Nested direct-parent routing works without sibling or ancestor leakage.
- Terminal, cancel, disconnect, and restart paths unblock both waiters.

### Runtime projection tests

- A stable tool id counts once across start, update, completion, and replay.
- Later structured metadata enriches rather than double-counts a call.
- Missing tool ids are not counted.
- Read locations alone do not classify an edit.
- Structured edit/write/patch metadata classifies edits and extracts paths.
- Shell command text is never parsed for edits.
- Windows path casing and duplicate paths collapse correctly.
- Path truncation and outside-workspace markers are deterministic.
- Complete line metadata displays totals; partial metadata suppresses them.
- Terminal settlement flushes the latest rollup.
- Pre-feature and non-delegate rows do not display fabricated zero counts.

### Lifecycle integration tests

- Two parallel children suspend the parent until the second terminal result.
- A child decision wakes the parent, a reply resumes the child, and the parent
  re-enters Join for the final result.
- User cancellation produces `parent_canceled` throughout the task tree.
- Premature parent `end_turn` produces `join_abandoned`.
- Parent failure produces `parent_turn_failed`.
- Page or conversation switching does not cancel or release Join.
- Host restart reconciles tasks and attention without an infinite waiter.
- Desktop and WebOnly emitters produce equivalent event and snapshot state.

### Frontend tests

- Running and terminal duration calculations use the correct timestamps.
- A shared ticker updates visible running cards without changing layout.
- Compact counts and expanded paths recover from live events and snapshots.
- Attention opens and clears idempotently on the existing card.
- Missing line counts and path truncation render without false precision.
- All localized strings exist and compact content fits supported viewports.

## Acceptance Criteria

- A parent can fan out multiple Codeg children and make one infinite batch Join
  without a model-driven polling loop.
- The parent model receives no tool result when only a subset of joined tasks
  completes.
- The final child completion releases the pending tool call and the same parent
  turn continues with all final reports.
- A persisted child decision request releases Join early, can be answered only
  by the direct parent, and resumes the blocked child exactly once.
- Observation and runtime-stat events update UI but do not wake the parent
  model; V1 exposes no generic progress or warning report channel.
- No live Codeg child survives user cancellation, parent turn failure, or a
  premature parent `end_turn`.
- Frontend navigation alone never cancels a child.
- The existing Delegation Card shows runtime, deduplicated tool count, and
  explicitly non-authoritative detected edit activity without an LLM progress
  report.
- Snapshot reattachment and cold metadata load recover the same visible state
  after a live event is missed.
- Legacy status callers retain their current wire semantics.

## Reference Semantics and Evidence

The selected semantics follow the useful parts of `pi-subagents` as inspected
at commit `315e1eb`:

- `subagent_wait` supports waiting for all children and waking for attention;
- `contact_supervisor` provides correlated `need_decision` requests and blocks
  the child until reply; and
- Pi core can trigger a turn when delivering a message.

Codeg deliberately does not copy Pi's temporary-file and polling mechanics.
Codeg already has a central Broker, durable task rows, connection snapshots,
and a notification primitive, so the same semantics can be implemented as a
state predicate over Broker-owned data.

An analysis of 96 Codex rollouts found 1,273 `wait_agent` returns, each followed
by a model-generated item. Of 579 `trigger_turn=false` child deliveries, 458
were messages and 121 were final answers; 167 observed `trigger_turn=true`
events were child task starts. This supports the distinction used here: a
child result does not bypass parent inference, and Codeg should resume the
already-suspended parent computation by completing its Join tool call rather
than manufacture a separate parent turn.

## Deferred Extensions

The Broker event model can later support semantic `progress` or `warning`
reports, but that work requires a separate design covering report schema,
rate limits, persistence, UI presentation, and whether any severity may wake a
parent. V1 does not reserve visible cards or chat messages for it.

Detach also remains a separate product decision. Adding it would require an
explicit creation-time ownership mode, a durable destination for results after
the originating turn, notification policy, cancellation ownership, retention,
and restart behavior. None of those semantics are implicit in this Join-only
design.
