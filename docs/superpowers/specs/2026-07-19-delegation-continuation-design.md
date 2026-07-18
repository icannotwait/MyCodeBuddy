# Delegation Continuation and Parent Turn Suspension Design

## Status

Approved in conversation on 2026-07-19. Reviewed against the current ACP
connection and delegation lifecycle on 2026-07-19; the mandatory amendments in
the next section are part of the approved design.

This specification amends
`docs/superpowers/specs/2026-07-17-event-driven-delegation-join-design.md`.
That document remains authoritative for the Broker Join predicate, durable task
lifecycle, parent-decision attention path, runtime statistics, Delegation Card,
and legacy status-call compatibility. This document supersedes its assumptions
that an infinite Join can keep every parent model suspended in the same turn,
that starting a later parent turn is a non-goal, and that a live child must
always be owned by a currently executing parent turn or open MCP call.

No implementation plan has been approved yet. The implementation plan must
preserve the contracts in this document and the unaffected contracts in the
2026-07-17 design.

## Problem

Codeg's Broker Join is event-driven and can keep a `get_delegation_status`
request open until all requested children are terminal, parent attention is
required, or the wait becomes unavailable. That is sufficient for MCP clients
that expose the pending tool call directly to the model runtime.

It does not suspend Codex for the same duration when the MCP call is nested in
`functions.exec`. Official Codex code mode gives both the initial
`functions.exec` observation and later `functions.wait` observations a hardcoded
default yield of 10,000 milliseconds when the model does not provide
`yield_time_ms`. A running cell is returned to the model after that observation
window and remains alive, so the model may spend another inference step calling
`functions.wait`. A Codeg-side 240-second Join response cannot prevent those
outer 10-second observation boundaries.

Prompting Codex to set a 240-second `yield_time_ms` is useful as an optimization,
but it is not a correctness mechanism. The value is selected per invocation by
the model, has no documented global Code mode configuration, and is outside
Codeg's enforcement boundary.

Codeg therefore needs to end the current parent turn intentionally, preserve
ownership of its children outside that turn, wait without an LLM in the loop,
and start a hidden continuation turn when there is actionable work or before a
typical five-minute provider cache expires.

## Goals

- Eliminate repeated parent-model wakeups caused solely by Codex's default
  `functions.exec` and `functions.wait` yields.
- Preserve asynchronous child execution while the parent turn is suspended.
- Wake the parent at the first of all-terminal, parent-attention, unavailable,
  or a 240-second checkpoint.
- Make parent suspension distinct from user cancellation.
- Keep correctness entirely inside Codeg without modifying Codex, an official
  ACP adapter, or an upstream model backend.
- Keep the active conversation protected from concurrent user prompts while
  allowing the rest of the application to remain usable.
- Preserve typed user input as a local draft without queueing or automatically
  sending it later.
- Hide internal continuation prompts from the public transcript while still
  delivering them to the agent and retaining an auditable continuation record.
- Recover waiting state after frontend reattachment while the parent ACP
  connection remains live, and fail closed when that connection or the Codeg
  process is actually lost.
- Make every wake, cancellation, and recovery path idempotent and race-safe.

## Non-Goals

- Changing child execution timeouts or provider cache behavior.
- Guaranteeing that every provider keeps a cache warm for exactly five minutes.
- Configuring Codex's default code-mode yield from Codeg.
- Resuming the same canceled ACP request. A continuation always starts a new
  parent turn in the same agent session.
- Transferring live children to a replacement ACP connection after the original
  parent connection or Codeg process has exited. That needs a separately
  designed cross-connection ownership handoff.
- Queueing user prompts behind a continuation.
- Locking navigation, other conversations, child inspection, or the entire app.
- Applying continuation ownership to platform-native subagents not represented
  by Codeg's `DelegationBroker`.
- Adding a user-facing checkpoint duration setting in V1.
- Replacing the existing child attention, task lifecycle, or Delegation Card
  contracts.

## Selected Approach

Codeg introduces a durable `DelegationContinuationCoordinator` and a new
internal parent-turn operation, `SuspendForDelegation`.

The canonical sequence is:

```text
parent calls canonical Broker Join
  -> Broker evaluates the current task and attention snapshot
  -> immediately actionable: return the ordinary Join result
  -> still waiting:
       persist and arm a continuation
       request internal parent-turn suspension
       cancel only the current parent turn and status waiter
       keep every joined child running
       lock external prompt admission for this conversation
       await Broker events or the 240-second deadline
  -> atomically claim one wake reason
  -> admit one hidden continuation prompt into the same agent session
  -> parent continues in a new turn
```

The wake deadline is:

```text
min(
  every required task is terminal,
  an open parent-attention request exists,
  a required task or coordination producer is unavailable,
  continuation armed_at + 240 seconds
)
```

The 240-second checkpoint intentionally leaves margin below the common
five-minute provider-cache window. It causes an actual parent model turn; an
internal status refresh without a model request would not refresh that cache.

## Alternatives Considered

### Keep the current Join open for 240 seconds

Rejected as the primary path. Broker waiting would remain efficient, but Codex
would still yield its outer code-mode cell on the hardcoded observation window
and invite repeated model-generated `functions.wait` calls.

### Require a long `yield_time_ms` through prompt guidance

Retained only as an optional optimization. The model may omit, shorten, or
misapply the pragma, and Codeg cannot validate it before Codex starts the cell.
Correctness cannot depend on prompt adherence.

### Patch or pin the official Codex runtime

Rejected. It would fork an upstream execution runtime, complicate official npm
updates, and solve only Codex while leaving other ACP agents with different
turn-lifecycle behavior.

## Review Amendments

Two independent reviews converged on the same implementation risk, and the
current code confirms it: the unsafe boundary is not Broker Join. It is the
existing rule that every parent-turn end drains the complete live delegation
tree. `src-tauri/src/acp/connection.rs:4761` maps `end_turn` to
`JoinAbandoned` and all other terminal paths to a `ParentTurnEndReason`.
The extension completion, stop-reason, prompt-response, and explicit Cancel
branches then call `DelegationBroker::cancel_by_parent_turn`
(`connection.rs:5194`, `5365`, `5447`, `5638`, and `5793`). That Broker method
recursively drains descendants before asynchronously tearing their sessions
down (`delegation/broker.rs:4695`).

The following amendments remove that conflict without weakening explicit user
Stop semantics.

### Suspension is a third turn disposition, not a new cancel reason

`SuspendForDelegation` must not be represented by a new
`ParentTurnEndReason`. Every current value of that enum feeds the destructive
parent-tree drain. Instead, the connection loop must use a private, exhaustive
turn-finalization disposition such as:

```text
NaturalEnd(ParentTurnEndReason)
UserCancelled
DelegationSuspended(SuspensionLease)
```

`NaturalEnd` and `UserCancelled` retain the existing terminal behavior.
`DelegationSuspended` is the only disposition that:

- sends the ACP cancel notification for the current parent turn;
- clears that turn's in-flight gate through an internal suspended-turn event;
- abandons the pending `get_delegation_status` response; and
- leaves Broker task ownership, attention, and the live child tree untouched.

It must not emit the public `TurnComplete { stop_reason: "cancelled" }`, call
`DelegationBroker::cancel_by_parent_turn`, invoke the manager's user-cancel
conversation-status write, clear the continuation's task ownership, or resolve
attention as `parent_canceled`.

The existing `ConnectionCommand::Cancel` behavior remains reserved for a user
Stop. It currently emits the public cancelled completion and cascades to the
Broker (`connection.rs:5540` and `5745`); changing that path would regress the
normal cancellation contract.

### One fenced suspension path covers every terminal source

Add `ConnectionCommand::SuspendForDelegation {
continuation_id, parent_turn_generation, reply }`. A private
`SuspensionLease` records the same identifiers plus the current connection and
session. The connection loop accepts it only while that exact parent turn is
in flight. It records the lease before sending the upstream cancel
notification and acknowledges only after the loop has made the connection safe
for another prompt.

All current terminal producers must call one internal finalization helper:

- extension `turn_completed` handling;
- `SessionMessage::StopReason` handling;
- `session/prompt` response completion; and
- both active-turn and idle `ConnectionCommand::Cancel` branches.

That helper consumes a matching `SuspensionLease` exactly once. A matching
cancelled completion is classified as `DelegationSuspended`; a non-matching or
late completion follows the existing natural/error/cancel rules. No new prompt
may be admitted until the old turn's suspension acknowledgement has been
persisted, so a delayed terminal event cannot be mistaken for the resumed
turn. If a normal terminal event wins before the lease is committed, normal
parent-tree cancellation wins and the continuation is failed rather than
silently adopting ambiguous children.

### V1 ownership transfer stops at a live parent connection

The durable continuation transfers ownership across parent *turns*, not across
an ACP connection or Codeg-process teardown. The current connection cleanup
uses `cancel_by_parent(...ParentDisconnected)` and therefore cancels all live
descendants (`delegation/broker.rs:4652`). V1 preserves that fail-closed
behavior:

- frontend reattachment may reconstruct a waiting projection while the backend
  and parent connection are still alive;
- an actual parent connection exit changes an active continuation to `Failed`
  with `parent_connection_lost`, runs the existing parent-disconnected cascade,
  and releases the prompt lock after durable cleanup; and
- application restart follows the same policy instead of attempting an
  unsupported session/child handoff.

Automatic continuation on a replacement connection requires durable child
control handles, session-resume guarantees for every ACP provider, and an
atomic ownership handoff. It is intentionally deferred rather than implied by
this design's persistence table.

### Waiting is a distinct admission result, never a busy retry

Add a stable `AcpError::ContinuationInProgress` with web error code
`conversation_waiting_for_subagents` and HTTP 409 mapping. The common manager
admission check must run before conversation creation, optimistic user-message
projection, or prompt enqueue for every external producer.

This error must not be translated to `TurnBusyError`. Today
`src/lib/api.ts:208` converts `turn_in_progress` into `TurnBusyError`, and
`src/hooks/use-connection-lifecycle.ts:433` re-queues that draft for automatic
send. A continuation rejection instead leaves the editor's text only in its
local draft and displays the waiting state; it is neither queued nor replayed
when the lock ends.

The frontend must receive a redacted continuation-lock projection and pass it
to `ChatInput`/`MessageInput`. The present `ChatInput` disabled expression
(`src/components/chat/chat-input.tsx:154`) considers only connection status and
selector loading, so it is insufficient once `turn_in_flight` is deliberately
false between turns. Stop, navigation, task cards, and editing remain enabled;
only external prompt submission is locked.

### Closing a status call remains non-destructive

No Broker cancellation behavior is required merely because suspension closes
the pending Join. `delegation/companion.rs:934` intentionally gives
`get_delegation_status` no external handle, so an MCP cancellation suppresses
only that response and does not cancel a task. Preserve this property with a
regression test; it is what lets the coordinator, rather than the parent MCP
call, own the live child set during the wait.

## Core Invariants

### Suspension is not cancellation

`SuspendForDelegation` may stop the current parent turn and its current Join
request. It must not:

- mark the conversation `Cancelled`;
- call `DelegationBroker::cancel_by_parent_turn`;
- settle a running child as canceled or failed;
- resolve an open child attention request as parent-canceled; or
- expose a user-cancel event to clients.

The existing explicit Cancel path retains its current semantics and cancels the
entire delegation tree.

The lifecycle must classify the internal suspended-turn boundary before it
applies the generic parent `end_turn`, failure, or cancellation policy from the
2026-07-17 design. An internal suspension is not `join_abandoned` and cannot
trigger that policy's child cascade.

### Continuation owns live children between turns

A durable, non-terminal continuation is a valid owner of its exact requested
task set while no parent turn is running. Parent connection identity is used
for routing and authorization, but a normal turn need not remain in flight.

A running child must have exactly one of these owners:

```text
active parent turn or its pending Broker tool call
active delegation continuation
```

No child may be silently transferred to a different conversation, root, or
continuation generation.

### One active continuation per conversation

At most one continuation for a parent conversation may be in a non-terminal
state. Each new Join after a wake creates a new generation. Old timers and
events can never wake a newer generation.

### The database is authoritative

In-memory watchers and timers are delivery mechanisms. The durable continuation
row, durable task reports, and durable attention rows determine whether a wake
is valid. Every event wake re-reads the authoritative predicate before claiming
the continuation.

### User input is never silently queued

While a continuation owns the conversation, external prompt admission fails
with a typed `conversation_waiting_for_subagents` result. Text already in a
composer remains local. Codeg does not create a user-message row, queue a send,
or submit the draft after unlock.

### Internal prompts are server-authored

Only the coordinator may bypass the waiting gate. A client-provided
`internal=true`, marker string, or continuation id never grants admission or
hides a public message.

## Component Responsibilities

### `DelegationContinuationCoordinator`

- Validates and persists continuation ownership.
- Arms Broker task/attention observation and the persisted deadline.
- Requests parent-turn suspension only after the durable row exists.
- Re-evaluates the wake predicate after every notification.
- Uses conditional state transitions to select one wake winner.
- Waits for the parent connection to become prompt-admissible.
- Builds and submits the hidden continuation prompt.
- Cancels watchers and prevents future wakes on explicit user Stop.
- Reconciles non-terminal rows to safe cleanup at process startup; V1 does not
  resume them on a replacement connection.
- Publishes runtime waiting state and low-cardinality metrics.

The coordinator should live in shared backend state so desktop and server modes
use the same implementation.

### `DelegationBroker`

- Remains authoritative for task ownership, reports, attention, and notifier
  availability.
- Exposes a coordinator-facing snapshot/wait API with the same
  `all_terminal_or_attention` predicate as Join.
- Treats abandonment of a status waiter as abandonment of that waiter only.
- Preserves children while their parent conversation has a valid continuation.
- Applies the existing cancellation cascade when the user explicitly stops the
  waiting conversation.

### ACP connection lifecycle

- Adds `ConnectionCommand::SuspendForDelegation` instead of reusing
  `ConnectionCommand::Cancel`.
- Registers and consumes a one-shot `SuspensionLease` keyed by continuation id,
  connection/session identity, and parent-turn generation.
- Routes every terminal producer through one disposition classifier so a
  matching suspension cannot fall through to `cancel_by_parent_turn`.
- Sends the ACP cancel notification needed to stop the current parent turn.
- Clears turn-scoped tool, permission, question, and terminal bookkeeping.
- Emits an internal suspended-turn completion that clears `turn_in_flight`
  without changing the conversation to `Cancelled` or canceling children.
- Fences late output and late terminal events from the suspended turn.
- Acknowledges suspension to the coordinator only after the connection is safe
  for a later prompt.

### Prompt admission

- Enforces the waiting lock in the backend's common prompt-admission boundary.
- Covers Tauri, HTTP, chat-channel, automation, and any other external producer.
- Returns `conversation_waiting_for_subagents`, rather than
  `turn_in_progress`, so clients retain a local draft instead of auto-queuing
  it for retry.
- Accepts an internal continuation only through an in-process capability that
  public request types cannot construct.
- Preserves ordinary `turn_in_flight` rejection while the continuation turn is
  running.

### Frontend

- Projects a conversation-level `waiting_for_subagents` state.
- Disables sending in the affected conversation while leaving editing enabled.
- Applies that lock independently of `ConnectionStatus` and
  `turn_in_flight`, which are intentionally clear between turns.
- Keeps the draft local and unchanged across wait, wake, and cancellation.
- Shows waiting state and the existing Delegation Cards, but no synthetic user
  bubble for the continuation prompt.
- Keeps navigation, other conversations, child inspection, and Stop available.

## Trigger and Join Contract

V1 arms a continuation only for the canonical coordination call:

```text
get_delegation_status({
  task_ids,
  wait_ms: 0,
  return_when: "all_terminal_or_attention"
})
```

The capability must be enabled for the connection and every requested task
must be an authorized Codeg Broker task owned by the parent conversation.

Before arming, the listener evaluates the ordinary Join predicate:

- `all_terminal`: return the normal result in the current turn;
- `attention_required`: return the normal result in the current turn;
- `unavailable`: return the normal result in the current turn; or
- still waiting: arm a continuation and suspend the parent.

Snapshot, positive bounded waits, and legacy `wait_ms=0` behavior are unchanged.
No `deferred` tool result is delivered to the model. Once arming succeeds, the
pending status socket remains open only until parent suspension closes it.

If durable arming or suspension dispatch fails, the continuation is moved to a
terminal failure state and the current Join receives an explicit error. Codeg
must not leave the conversation locked without a durable owner.

## Persistence Model

Add a `delegation_continuations` table with the following logical fields:

```text
continuation_id          TEXT PRIMARY KEY
parent_conversation_id   INTEGER NOT NULL
parent_session_id        TEXT NOT NULL
parent_connection_id     TEXT NULL
generation               INTEGER NOT NULL
parent_turn_generation   INTEGER NOT NULL
task_ids_json             TEXT NOT NULL
state                     TEXT NOT NULL
wake_reason               TEXT NULL
armed_at                  DATETIME NOT NULL
wake_at                   DATETIME NOT NULL
suspend_requested_at      DATETIME NULL
suspended_at              DATETIME NULL
wake_claimed_at           DATETIME NULL
prompt_admitted_at        DATETIME NULL
finished_at               DATETIME NULL
internal_prompt_id        TEXT NOT NULL
internal_prompt_marker    TEXT NOT NULL
failure_code              TEXT NULL
version                   INTEGER NOT NULL
created_at                DATETIME NOT NULL
updated_at                DATETIME NOT NULL
```

`task_ids_json` is serialized and parsed through a typed JSON representation;
string concatenation is not used. Input order is retained for prompt snapshots.

Required constraints are:

- unique `(parent_conversation_id, generation)`;
- unique `internal_prompt_id`;
- at most one non-terminal row per parent conversation; and
- every task id must resolve to the same authorized parent at arm time.

SQLite partial-index support may enforce the active-row constraint. A
transactional conditional insert remains required because concurrent Join
requests can race before either observes the other.

The internal prompt body does not need a second full copy in this table. The
row stores its stable marker, reason, task set, timestamps, and delivery id;
the agent session remains the authoritative model-history copy.

## State Machine

```text
Arming
  -> Waiting
  -> WakePending
  -> Resuming
  -> Completed

Arming | Waiting | WakePending | Resuming
  -> Cancelled
  -> Failed
```

### `Arming`

The row is durable and watchers may already observe task changes, but parent
suspension has not yet been acknowledged. A wake condition in this state is
recorded as `WakePending`; it is not delivered concurrently with the old turn.

### `Waiting`

The old parent turn is fenced and external prompts are locked. Broker events
and the persisted deadline may attempt the wake CAS.

### `WakePending`

Exactly one wake reason has been persisted. The coordinator waits until the
same parent agent session can accept its internal prompt. New task events may
enrich the eventual snapshot but cannot replace the winning wake reason.

### `Resuming`

The coordinator has claimed prompt admission and is submitting the internal
prompt. The row stays active until ACP confirms acceptance so external prompts
cannot enter the gap.

### `Completed`

The hidden prompt was accepted. The continuation lock is released. Ordinary
turn-in-flight admission rules protect the new parent turn. If that turn calls
Join again, it creates the next generation.

### `Cancelled`

The user explicitly stopped the wait or the whole conversation. Watchers and
pending delivery are disabled before the child cancellation cascade begins.

### `Failed`

Codeg could not establish or deliver a safe continuation. The stable failure
code is surfaced as conversation state, children are not left ownerless, and
the input lock is released only after cleanup is complete.

## Arm and Suspension Algorithm

Arming uses this order:

```text
1. authorize parent and task set
2. capture and validate the current parent-turn generation
3. evaluate the current Join predicate
4. transactionally insert Arming with that parent-turn generation and
   wake_at = now + 240 seconds; this insert immediately activates the backend
   prompt gate
5. arm the Broker notifier/watcher
6. re-evaluate the predicate after arming
7. send SuspendForDelegation with continuation id and parent-turn generation
8. await the connection-loop acknowledgement that its SuspensionLease was
   consumed and the old turn can no longer reach generic finalization
9. transition Arming -> Waiting unless a wake already claimed WakePending
10. publish the frontend waiting projection; backend enforcement is already live
```

The snapshot after notifier registration closes the event-before-registration
race. The continuation row exists before suspension, so a successful turn stop
can never leave children without a durable owner.

The pending Join socket is expected to close when the agent cancels the current
tool call. The existing status-listener peer-close behavior is retained: it
drops only the Join waiter and does not mutate task state.

Suspension uses an acknowledgement channel. If the connection command cannot
be delivered, Codeg conditionally fails the continuation and returns an error
through the still-open Join. If the command was delivered but the agent is slow
to acknowledge cancellation, the connection loop may publish its immediate
internal suspended-turn boundary, then drain the matching upstream terminal
event under the lease. That boundary is not a public cancelled completion and
must not use user-cancel side effects.

## Wake Algorithm

Each non-terminal continuation has one in-memory task observer and one timer.
They are recreated only while the same backend process and parent connection
remain live; process startup reconciles persisted rows to cleanup instead.

```text
loop:
  arm Broker notification
  read authorized task reports, attention, and availability

  if attention is open:
    try CAS -> WakePending(attention_required)
  else if every requested task is terminal:
    try CAS -> WakePending(all_terminal)
  else if any required task cannot produce a future update:
    try CAS -> WakePending(unavailable)
  else if persisted wake_at <= now:
    try CAS -> WakePending(checkpoint)
  else:
    await notification or wake_at

  stop after this coordinator instance wins or observes a terminal/newer state
```

The deterministic predicate priority is:

```text
attention_required
all_terminal
unavailable
checkpoint
```

This priority affects simultaneous snapshots only. The first successfully
persisted CAS remains authoritative if a different condition arrives later.

At a checkpoint the coordinator still includes the latest terminal results,
running reports, and attention state. If a condition became actionable while
prompt admission was pending, the prompt contains that newer snapshot even
though its stable wake reason remains `checkpoint`.

## Hidden Continuation Prompt

The coordinator constructs one internal prompt with a versioned envelope:

```text
<codeg_internal_continuation
  id="<continuation_id>"
  generation="<generation>"
  reason="<wake_reason>">

The delegated tasks you were waiting for have been re-evaluated.
Use the authoritative task and attention snapshot below to continue the
original user request. If required tasks are still running after you have
handled actionable work, enter the canonical Join again.

<typed task and attention snapshot>
</codeg_internal_continuation>
```

The envelope is agent-facing control context, not user-authored content. It
contains no instructions to expose the continuation mechanism to the user.

Delivery uses a server-only origin:

```text
PromptOrigin::DelegationContinuation {
  continuation_id,
  generation,
  wake_reason,
  internal_prompt_id,
}
```

The public Tauri/HTTP prompt types cannot construct this origin. The marker
contains the unpredictable persisted continuation id, and transcript filtering
suppresses it only when that id and prompt delivery match a durable internal
record. A user who types a similar XML tag still creates a visible user message.

The hidden prompt:

- is sent to the same agent session;
- is present in the agent's own model history;
- does not emit a public `UserMessage` event;
- does not create a visible user row in Codeg's conversation transcript;
- is omitted by cold transcript parsing using the persisted internal marker;
  and
- retains continuation id, reason, and delivery timestamps for audit without
  duplicating the full child output in a second database field.

## Conversation Lock and UX

Waiting state is separate from conversation completion status. The conversation
remains logically in progress while `turn_in_flight` is false between parent
turns.

The backend prompt-admission gate checks for an active continuation before it
accepts every external source:

- desktop/Tauri prompt;
- server HTTP prompt;
- chat-channel prompt;
- automation prompt; and
- any future external producer using the common manager API.

A rejected request receives a typed response containing the conversation id and
continuation state, but not hidden prompt content or foreign task information.

Every non-terminal state, including `Arming`, activates the authoritative
backend gate as soon as the continuation insert commits. The later suspension
acknowledgement controls when the frontend projects `Waiting`; it does not
control enforcement. This closes the interval between clearing the old
`turn_in_flight` value and publishing the waiting UI state.

The frontend mirrors this authoritative state for ergonomics:

- sending is disabled only in the affected conversation;
- the editor remains enabled;
- its text remains a local draft;
- no queued-message indicator is shown;
- navigation and other conversations remain interactive;
- child conversations and Delegation Cards remain inspectable; and
- Stop remains enabled.

If a stale client attempts to send despite the UI lock, the backend rejects it.
Frontend state is never the enforcement boundary.

When the hidden prompt is accepted, the continuation lock ends atomically with
prompt admission. The ordinary active-turn gate then prevents a user prompt
from racing the resumed parent turn. When that turn finishes without another
Join, normal prompt admission resumes and the local draft is still unsent.

## Explicit Stop Semantics

User Stop cancels the entire delegation operation:

```text
1. CAS active continuation -> Cancelled
2. cancel its watcher, timer, and unadmitted hidden prompt
3. invoke the existing explicit parent Cancel path
4. cascade cancel every live direct and nested child
5. resolve open attention with parent_canceled
6. publish terminal task/card state
7. release the waiting lock after cleanup ownership is established
8. keep any editor text as an unsent local draft
```

The user cancel path continues to mark the conversation `Cancelled`. This is
the only path in this design that intentionally uses
`cancel_by_parent_turn(...ParentCanceled)`.

If Stop wins before internal prompt admission, no later watcher or timer may
wake the parent. If prompt admission wins first, Stop cancels the newly running
parent turn and the same delegation tree. Both orders produce one durable
cancel outcome and no orphan children.

## Turn and Event Fencing

Suspension introduces a parent turn generation that is captured when the
continuation arms. The connection loop records that generation as suspended.

After the internal suspended-turn boundary:

- content, tool, permission, and completion events belonging to that old turn
  cannot change the new continuation turn;
- a late upstream `TurnComplete(cancelled)` is deduplicated;
- an old Join result cannot be projected after the hidden prompt starts; and
- a stale suspension acknowledgement cannot unlock or wake a newer generation.

Fencing is scoped to the turn, not the whole ACP session. Session state, model
history, and the connection remain available for the hidden continuation.

## Recovery and Failure Handling

### Frontend reattachment

Active continuation state is included in the existing connection/conversation
snapshot path. Reattachment reconstructs the waiting lock and status without
event replay. Draft recovery remains a frontend concern and never becomes a
backend prompt queue.

### Parent connection interruption

An actual parent connection exit is not an in-turn suspension. The coordinator
atomically changes an active continuation to `Failed` with
`parent_connection_lost`, disables its watcher/timer, and lets the existing
`ParentDisconnected` parent-tree cascade settle live children. It then releases
the prompt lock and surfaces a visible system error. It does not fabricate a
user message or attempt a hidden prompt on another connection.

### Application restart

Process startup reconciles `Arming`, `Waiting`, `WakePending`, and `Resuming`
rows as interrupted ownership, not as resumable work. It marks each row failed
with `parent_connection_lost`, reconciles any durable child reports through the
existing teardown policy, releases the lock, and preserves the audit record.
The database remains authoritative for cleanup and observability, but V1 never
starts a hidden continuation prompt after a process restart.

### Arming failure

If persistence fails, the parent remains in its current turn and receives a
Join error. If suspension dispatch fails after persistence, the row is
conditionally failed and the same still-open Join receives the error when
possible. The input lock is never published from an unowned or terminal row.

### Prompt delivery failure

Retryable delivery failures use bounded exponential backoff while state remains
`WakePending`. The coordinator rechecks user cancellation and prompt admission
before every attempt. Permanent failures follow the cleanup policy above.

## Race Handling

### Task completion during arming

The post-registration snapshot observes it. The continuation moves directly to
`WakePending` but waits for the old parent turn's suspension boundary before
submitting the hidden prompt.

### Deadline and Broker event together

Both attempt the same state/version CAS. One reason wins. The prompt snapshot is
fresh, so no terminal result or attention request is lost.

### User Stop and wake claim

Stop wins if it changes the active row before prompt admission. If the wake has
already admitted a prompt, the normal active-turn Cancel path wins afterward.
Neither order can submit two hidden prompts.

### Two coordinator instances after in-process reconciliation

Every claim and prompt-admission transition is conditional on id, generation,
state, and version. A losing instance stops when it observes the newer version.

### Parent attempts another prompt from a stale client

The backend continuation gate rejects it. No user row is persisted, so a later
unlock cannot accidentally replay the request.

### Hidden prompt accepted but acknowledgement lost

Before releasing the lock, the coordinator correlates `internal_prompt_id` with
the parent session/turn record. It marks the continuation completed when
acceptance is already durable and does not resubmit the prompt.

## Security and Isolation

- Every task in a continuation is authorized against the authenticated parent
  conversation before persistence and again before wake delivery.
- Internal prompt bypass uses an in-process capability, not a public flag.
- Transcript hiding requires a persisted internal prompt identity; arbitrary
  user content cannot opt itself out of history.
- Error responses do not reveal whether a foreign task id exists.
- Metric labels contain no prompts, child output, file paths, continuation ids,
  or other high-cardinality data.
- Hidden prompts do not bypass model sandbox, permission, or tool policies.
- A continuation grants no new ability to message siblings or arbitrary
  ancestors; existing direct parent-child attention authorization remains.

## Observability

Add low-cardinality counters and histograms:

```text
continuation_armed
continuation_suspended
continuation_wake_claimed{reason=all_terminal|attention_required|unavailable|checkpoint}
continuation_prompt_admitted
continuation_cancelled{phase}
continuation_failed{phase,code}
continuation_reconciled{state}
continuation_duplicate_claim_suppressed
continuation_wait_duration_ms{reason}
continuation_suspend_duration_ms
continuation_prompt_delivery_retry
prompt_rejected{reason=waiting_for_subagents,source}
```

Structured logs may include continuation, conversation, connection, session,
and task ids for diagnostics. Metrics must not use those ids as labels.

Tracing should distinguish:

- internal suspension from user cancellation;
- status-waiter peer close from task cancellation;
- wake claim from prompt admission; and
- hidden prompt submission from ordinary user prompt submission.

## Compatibility and Rollout

The behavior is guarded by a connection-bound
`delegation_continuation_v1` capability.

When disabled, the 2026-07-17 event-driven Join behavior remains unchanged.
When enabled, only the canonical `wait_ms=0` plus
`all_terminal_or_attention` call may suspend the parent. Legacy status calls
retain their existing response semantics.

Rollout order:

1. Add persistence and coordinator state without enabling suspension.
2. Add internal prompt origin, backend gate, snapshot state, and hidden
   transcript projection.
3. Add `SuspendForDelegation` with turn fencing and no child cascade.
4. Add Broker/coordinator integration and startup cleanup reconciliation.
5. Enable for official Codex ACP sessions and verify that repeated code-mode
   waits disappear.
6. Enable per additional ACP agent only after cancel-and-resume conformance
   tests pass.
7. Make the capability the default after telemetry shows no orphan tasks,
   duplicate wakes, hidden-message leaks, or stuck locks.

V1 uses a named backend constant of `240_000` milliseconds. It is not exposed
as a UI setting. A future configuration design may make it adjustable after
provider behavior and operational data justify the added surface.

Rollback disables new arming. Existing active rows must still be reconciled,
canceled, or completed by code that understands their schema; rollback must not
strand a persisted lock or delete an active continuation blindly.

## Testing Strategy

### Coordinator unit tests

- Already-terminal, attention, and unavailable snapshots return normally and
  do not create a continuation.
- A running snapshot creates exactly one continuation generation.
- Post-arm recheck catches completion between initial snapshot and watcher arm.
- Task completion, attention, unavailable, and checkpoint each claim one wake.
- Deadline/event and multiple-event races have one CAS winner.
- A stale coordinator instance cannot wake a newer generation.
- Prompt delivery is idempotent by `internal_prompt_id`.
- Frontend reattachment restores an active lock without replaying a prompt.
- Parent connection loss and process restart fail active ownership closed and
  release the lock after the established descendant cleanup path.

### ACP lifecycle tests

- `SuspendForDelegation` clears the parent turn without marking the conversation
  canceled.
- Suspension never calls `cancel_by_parent_turn` and leaves children running.
- Each of extension completion, stop-reason handling, prompt-response handling,
  and the active-turn Cancel branch consumes a matching suspension lease instead
  of falling through to the generic parent-end cascade.
- Explicit user Cancel still cascades through all direct and nested children.
- Late output and a late upstream TurnComplete from the suspended generation are
  fenced and deduplicated.
- A wake that occurs during suspension waits until the parent connection is
  prompt-admissible.
- Prompt acceptance followed by a lost acknowledgement does not duplicate the
  continuation turn.

### Prompt admission and transcript tests

- Tauri, HTTP, chat-channel, and automation prompts are rejected while waiting.
- The rejection has code `conversation_waiting_for_subagents` and is never
  converted into `TurnBusyError` or an automatic queue entry.
- A server-authored continuation prompt passes the same gate.
- A client cannot forge the internal origin or hide its message with marker
  text.
- Live events, database projection, and cold transcript parsing all omit the
  hidden prompt.
- The agent session receives the hidden prompt with the expected task snapshot.
- A local draft survives wait, wake, completion, and cancellation without being
  sent automatically.

### Broker and cancellation tests

- Closing the suspended Join socket drops only the status waiter.
- Children remain live while the continuation is active.
- User Stop cancels the continuation and the complete delegation tree.
- Stop before wake, after wake claim, and after prompt admission each have one
  deterministic outcome and no orphan tasks.
- Unknown or foreign tasks cannot be adopted by a continuation.

### Codex integration tests

- A canonical Join nested in `functions.exec` triggers suspension before a
  repeated `functions.wait` model loop develops.
- No parent model request occurs while children are merely running before the
  240-second checkpoint.
- Child completion before 240 seconds starts one hidden parent turn.
- Parent attention before 240 seconds starts one hidden parent turn.
- A 240-second checkpoint starts one hidden parent turn with current snapshots.
- If the parent rejoins, the next generation uses a new 240-second deadline.

Tests use paused/fake time; the suite must not sleep for four real minutes.

### Recovery and frontend tests

- Reattachment to a still-live backend restores the waiting lock before external
  prompt admission opens.
- Parent connection loss and process restart cancel or reconcile owned children,
  unlock the conversation, and surface `parent_connection_lost`.
- Waiting state remains scoped to one conversation across desktop and server
  clients.
- Navigation, other conversations, child inspection, and Stop remain enabled.
- Waiting and error labels exist in all ten supported locales and do not resize
  or overlap compact conversation controls.

## Acceptance Criteria

- A Codeg parent can enter canonical Join without relying on the model to set
  `yield_time_ms`.
- Once the continuation is armed, no periodic parent LLM request is required to
  discover that children are still running.
- Parent suspension closes the current Join and turn without canceling any
  joined child.
- The parent is awakened exactly once by all-terminal, parent-attention,
  unavailable, or a 240-second checkpoint.
- Wake starts a new turn in the same agent session and includes an authoritative
  task/attention snapshot.
- The continuation prompt is available to the model but absent from every
  user-visible live and cold transcript projection.
- External prompts cannot enter the waiting conversation through any backend
  path, return `conversation_waiting_for_subagents`, and leave typed text
  neither queued nor automatically submitted.
- The user can navigate and inspect other work while waiting.
- User Stop cancels the active continuation and the entire delegation tree.
- Event, timer, cancellation, and terminal-source races cannot create duplicate
  parent turns, orphan children, or a permanently stuck input lock.
- Parent connection loss or process restart fails the continuation closed rather
  than attempting an unsupported ownership transfer.
- Disabling the capability restores the existing event-driven Join behavior for
  new waits without corrupting already-persisted continuations.

## Upstream Evidence

The default-yield conclusion is based on official Codex source at commit
`b8b61bc692517adcd18622df260f2ddd80635122`:

- `codex-rs/code-mode-protocol/src/runtime.rs` defines
  `DEFAULT_EXEC_YIELD_TIME_MS` and `DEFAULT_WAIT_YIELD_TIME_MS` as `10_000`.
- `codex-rs/code-mode/src/service.rs` uses
  `request.yield_time_ms.unwrap_or(DEFAULT_EXEC_YIELD_TIME_MS)`.
- `codex-rs/core/src/tools/code_mode/wait_spec.rs` documents the wait default as
  10,000 milliseconds.
- `codex-rs/features/src/feature_configs.rs` exposes no code-mode default-yield
  configuration.
- The current public Codex configuration reference documents only
  `features.code_mode.enabled`, `excluded_tool_namespaces`, and
  `direct_only_tool_namespaces`; it exposes no default-yield setting.

Relevant source links:

- <https://github.com/openai/codex/blob/b8b61bc692517adcd18622df260f2ddd80635122/codex-rs/code-mode-protocol/src/runtime.rs#L11-L12>
- <https://github.com/openai/codex/blob/b8b61bc692517adcd18622df260f2ddd80635122/codex-rs/code-mode/src/service.rs#L114-L120>
- <https://github.com/openai/codex/blob/b8b61bc692517adcd18622df260f2ddd80635122/codex-rs/core/src/tools/code_mode/wait_spec.rs#L13-L16>
- <https://github.com/openai/codex/blob/b8b61bc692517adcd18622df260f2ddd80635122/codex-rs/features/src/feature_configs.rs#L7-L20>
- <https://learn.chatgpt.com/docs/config-file/config-reference#configtoml>

These values explain why a Broker-side 240-second long poll alone cannot
suppress Codex's outer model wakeups. They do not require Codeg to fork Codex;
the continuation architecture moves the waiting guarantee into Codeg's own
durable control plane.
