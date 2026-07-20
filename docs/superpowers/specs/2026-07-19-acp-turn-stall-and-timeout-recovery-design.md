# ACP Turn Stall and Timeout Recovery Design

## Status

Approved in conversation on 2026-07-19.

This specification defines product-level protection for every ACP prompt turn,
including root conversations, Broker-backed Codeg delegations, and
platform-native sub-agents such as Grok native sub-agents. It complements:

- `2026-07-14-acp-terminal-cancel-unblock-design.md`, which made terminal
  cancellation bounded and cancellation-safe;
- `2026-07-17-event-driven-delegation-join-design.md`, which defines Codeg
  delegation observation and ownership; and
- `2026-07-19-delegation-continuation-design.md`, which defines intentional
  parent-turn suspension while Codeg children continue.

No implementation plan has been approved yet. The implementation plan must
preserve the contracts in this document.

## Problem

An ACP agent can keep a prompt turn open indefinitely while it waits for a
model response, native sub-agent, terminal process, or tool call. Today the
user can cancel manually, but there is no backend-enforced maximum duration
for a prompt that never reaches a terminal ACP frame.

The incident that motivated this design had four compounding conditions:

- Grok launched foreground `tsc`, test, and ESLint commands without a shell
  timeout.
- The parent asked `get_command_or_subagent_output` to wait with
  `timeout_ms=1200000`, but that model-supplied value was not an authoritative
  Codeg turn deadline and did not terminate the underlying child work.
- Native sub-agent UUIDs were repeatedly used as terminal IDs. There were 523
  `terminal not found` errors; 520 carried native sub-agent IDs and only three
  carried real `term_*` IDs.
- The parent turn remained open for more than two hours, leaving the
  conversation in progress and providing no product-level recovery boundary.

The affected root session was
`019f76f1-a9f9-7fe3-9e4c-0503ef1a352c`; the native Grok sub-agent was
`019f7877-b1c0-7761-aacf-4cbdbe3e79d1`.

The long polling value explains part of the observed delay, but it was not a
safety mechanism. Prompt guidance, shell timeout arguments, and model-chosen
poll intervals are useful efficiency hints only. The application must retain
an enforcement boundary that does not depend on the model following them.

There is also a performance amplifier in the terminal runtime. Output is read
in 4 KiB pieces, appended to a `String`, and capped at 1 MB by repeatedly
draining its prefix. Once a noisy command reaches the cap, every small append
can move almost the entire retained buffer. Large lint output therefore adds
avoidable CPU and lock contention while the turn is already under stress.

## Goals

- Warn when an in-flight turn has produced no real model or terminal progress
  for a configurable period.
- Keep `stalled` as an ephemeral observation, never a persisted conversation
  or delegation lifecycle state.
- Enforce a configurable, backend-owned absolute maximum turn duration, with a
  product default of 30 minutes.
- Make user cancellation and hard-timeout recovery share one race-safe turn
  finalization path.
- Send ACP cancellation, stop turn-owned terminal processes, close pending
  input responders, and settle all root and Broker-owned state exactly once.
- Guarantee that a timed-out or canceled root conversation never transitions
  to `pending_review`.
- Cover Grok native sub-agents and other agent-native work that is invisible to
  `DelegationBroker` by protecting the owning root ACP turn.
- Preserve the existing ability to run legitimate long commands by making the
  hard limit configurable and allowing it to be disabled explicitly.
- Make terminal output retention scale linearly with emitted output rather
  than repeatedly shifting the 1 MB retained window.
- Add useful terminal and turn diagnostics without logging commands, output,
  paths, environment variables, or secrets.
- Reduce repeated invalid-terminal noise without weakening RPC error behavior.
- Surface the difference between "no observable progress" and "finished" in
  the UI, with an immediate Stop action.

## Non-Goals

- Inferring that a silent process is dead. A compiler may legitimately emit no
  output for several minutes; soft stall is an observation only.
- Automatically completing, approving, retrying, or replacing a stalled turn.
- Relying on prompt text, model-selected timeouts, or background-polling style
  for correctness.
- Adding a universal per-command execution timeout. Command duration varies;
  the authoritative safety boundary is the owning turn.
- Persisting `stalled` in `conversation.status` or
  `conversation.delegation_task_status`.
- Adding a new durable root conversation status such as `timed_out`. Root
  timeout settles to the existing `cancelled` status and carries a distinct
  live error code.
- Individually canceling one Grok native sub-agent. Codeg does not own a stable
  per-native-child lifecycle or cancellation handle. V1 cancels its owning
  root turn and all terminals created by that turn.
- Applying the turn deadline to explicit out-of-turn background work after its
  owning prompt has already ended normally. Existing background-work ownership
  and keepalive rules remain authoritative.
- Changing the 240-second delegation continuation checkpoint. A deliberate
  continuation suspension ends its current turn and therefore retires that
  turn's hard timer normally.
- Sending commands, output, working directories, environment variables, or
  raw invalid IDs to telemetry or logs.

## Selected Approach

Codeg adds a backend-owned `TurnGuard` to every admitted ACP prompt. Each turn
gets an opaque generation token, an activity clock, a soft observation timer,
and an immutable hard deadline captured at admission.

The connection loop remains the only component allowed to finalize a turn.
`TurnGuard` produces signals; it does not directly cancel an agent or write
conversation state. The loop feeds real model and terminal progress into the
guard and selects the hard deadline alongside ACP updates, the prompt result,
frontend commands, and terminal polling.

At a high level:

```text
prompt admitted
  -> allocate turn generation and capture hard-timeout policy
  -> mark active turn in SessionState and TerminalRuntime
  -> observe ACP model/tool progress and terminal lifecycle/output
  -> emit active/stalled/waiting_input observation transitions
  -> first terminal source claims the turn-finalization fence
       agent completion       -> existing success/failure mapping
       user Stop              -> cancelled
       absolute deadline      -> timeout
       disconnect             -> disconnected
       delegation suspension  -> suspended, not cancelled
  -> detach and cancel turn-owned resources when required
  -> emit exactly one TurnComplete
  -> converge root DB state or Broker child task state
  -> acknowledge/drain cancellation before admitting another turn
```

Prompt instructions also tell agents to start expected long commands in the
background and use bounded polling with the returned `term_*` ID. This reduces
unnecessary model blocking, but no invariant in this design depends on that
instruction being followed.

## Alternatives Considered

### Prompt-only background execution and bounded polling

Rejected as the safety mechanism. It improves normal behavior, but the model
can omit a timeout, choose an excessive wait, use the wrong identifier, or
stop making tool calls. The incident demonstrated all of those failure modes.
Prompt guidance remains an optimization layered over backend enforcement.

### Per-terminal timeout only

Rejected as the primary boundary. A turn can hang without any terminal, and a
terminal timeout does not guarantee that the model, native sub-agent, pending
permission, prompt RPC, or conversation state will finish. Different commands
also have materially different legitimate durations. Turn ownership is the
only boundary that covers all causes.

### Backend turn guard with soft observation and hard enforcement

Selected. It covers every prompt source, can reuse the existing ACP cancel and
terminal cleanup paths, distinguishes observation from lifecycle, and remains
effective when an agent ignores all prompt guidance.

## Core Invariants

### One active generation

Every admitted prompt receives a backend-generated `turn_id` that is unique
for the connection lifetime. It is allocated for all prompt purposes,
including user conversations, chat channels, automations, internal prompts,
Broker children, and hidden delegation continuation prompts.

The new token must not reuse `ActiveTurnContext.token`: that token exists only
for selected persistence/title paths, while timeout protection must cover
every prompt. A `PromptCommand` carries the new token, admission time, and
captured hard-timeout value into the connection loop.

At most one generation is active on a connection. Activity or cleanup tagged
with an older generation can never mutate a later turn.

### Observation is not lifecycle

An in-flight turn has one ephemeral observation:

```text
active | stalled | waiting_input
```

`stalled` never ends a turn, changes `conversation.status`, settles a Broker
task, wakes a terminal-only Join, or changes route selection. Activity after a
stall returns the observation to `active`.

`waiting_input` has precedence over `stalled` while an ACP permission or
`ask_user_question` request is open. Resolving the input does not fabricate
agent progress; if the last real progress is already older than the threshold,
the next observation can immediately be `stalled`.

### Hard deadline is absolute

The hard deadline is:

```text
monotonic_admission_instant + captured_hard_timeout
```

It does not reset on model output, terminal output, tool progress, user input,
permission waiting, stall recovery, or configuration changes. A value of zero
disables hard enforcement for turns admitted under that policy.

The timer uses `tokio::time::Instant`; wall-clock timestamps are retained only
for display and diagnostics. System clock changes therefore cannot extend or
shorten enforcement. Resume after machine sleep fires an overdue deadline as
soon as the runtime is scheduled.

### Exactly one terminal winner

All completion sources claim a generation-scoped finalization fence before
performing side effects. The first successful claim moves the turn from
`running` to `finishing`; every losing path becomes a no-op for lifecycle,
database, and user-visible events.

At the exact scheduling boundary, agent completion and the hard timer may
race. Either may claim first, but the result must be internally consistent and
there must be exactly one `TurnComplete`, one database transition, one Broker
settlement, and one cancellation cascade at most. Tests exercise both orders.

### Cancellation is fenced before reuse

ACP session updates do not carry a Codeg turn generation. After Stop or hard
timeout, Codeg cannot safely attribute a late terminal frame to a newly
admitted prompt on the same session.

The UI-facing turn is completed immediately, but the connection enters an
internal cancellation quarantine. New prompts remain queued until either:

- the canceled prompt RPC or a terminal stop frame acknowledges cancellation;
  or
- a five-second cancellation grace expires, after which Codeg disconnects the
  agent process and lets the normal reconnect path create a clean session.

Non-terminal updates received during quarantine are drained and do not update
the retired turn or a future turn. The quarantine is an internal admission
gate, not a new persisted conversation status.

`SessionState` carries a backend-only `turn_recovery_in_flight` flag separate
from `turn_in_flight`. The finalizer sets it before emitting `TurnComplete`;
prompt admission treats either flag as busy and returns the existing busy
result so the frontend message queue retains the draft. Cancellation
acknowledgement or connection teardown clears it. It is never serialized and
does not keep the old live transcript visible.

### Timeout never means reviewable completion

The stable stop reason for backend enforcement is `timeout`.

- Root conversation: `InProgress -> Cancelled` by compare-and-set.
- Broker child turn: task becomes `Failed` with `child_timeout`; its child
  conversation becomes `Cancelled` through Broker-owned settlement.
- Parent root timeout with live Codeg descendants: descendants become
  `Canceled` with `parent_timeout`.
- Grok native children: no independent durable task row exists; the root turn
  becomes `Cancelled`, its active tool UI closes, and its turn-owned terminals
  are stopped.

Neither `timeout` nor `cancelled` can enter the `end_turn` branch that writes
`PendingReview`.

## Settings and Defaults

Turn protection is an ACP runtime concern, not a delegation-only concern. Add
a focused `AcpTurnPolicySettings` service and a live watch-backed
`AcpTurnPolicyRuntime` in `AppState`.

The canonical persisted keys are:

```text
acp.turn.stalled_after_seconds
acp.turn.hard_timeout_seconds
```

The product defaults and accepted ranges are:

| Setting | Default | Accepted values | Semantics |
| --- | ---: | --- | --- |
| `stalled_after_seconds` | 300 | 60 through 3600 | Live soft observation threshold |
| `hard_timeout_seconds` | 1800 | 0, or 60 through 86400 | Absolute per-turn maximum; 0 disables |

There is deliberately no cross-field constraint. A user may choose a hard
limit shorter than the soft threshold, in which case enforcement occurs
without a prior stall warning.

### Compatibility migration

The existing `delegation.stalled_after_seconds` value remains a compatibility
alias; there must not be two independent soft thresholds.

Startup resolution is:

1. use `acp.turn.stalled_after_seconds` when present and valid;
2. otherwise use the clamped legacy `delegation.stalled_after_seconds` value;
3. otherwise use 300 seconds.

Both the new turn-protection save path and the legacy delegation-settings save
path update the canonical and compatibility values in one transaction, then
publish one runtime snapshot. Existing clients can continue reading and
writing `DelegationSettings.stalled_after_seconds`; new UI reads the canonical
turn-policy endpoint. The legacy field remains an API alias through at least
one compatibility cycle and may remain indefinitely because it is cheap.

The existing delegation supervisor subscribes to the centralized soft
threshold watch. `DelegationRuntimeSettings` retains route, enablement, and
other delegation-owned values but no longer owns the authoritative threshold.

### Live update semantics

- A soft-threshold change is applied to every in-flight observation. Lowering
  it may immediately emit `stalled`; raising it may immediately emit `active`.
- A hard-timeout change applies only to turns admitted after the successful
  settings transaction. An in-flight absolute deadline never jumps forward,
  backward, on, or off.
- A failed persistence transaction publishes no runtime change.
- Missing or malformed persisted values fall back as described above and are
  reported in structured diagnostics without logging raw values.

### Settings UI

Move the inactivity threshold out of the multi-agent-only presentation and
place both controls under an ACP "Turn protection" section:

- an inactivity warning duration input, in minutes; and
- an enable toggle plus maximum turn duration input, in minutes.

The hard-timeout toggle stores zero when disabled and restores the last valid
nonzero draft value when re-enabled. The default visible value is 30 minutes.
The settings copy must call the soft state "no observable activity", not
"hung", and the hard state "maximum turn duration", not a shell timeout.

## Activity Rules

`TurnActivityHandle` is a cheap, cloneable, generation-scoped producer. It
updates a shared monotonic `TurnProgressClock` atomically and sends a coalesced
wake to the guard. `SessionState` keeps a reference to that clock while the
generation is active, and snapshot materialization reads the current value
directly; no event or state-lock acquisition is required for every output
chunk. A burst of terminal bytes or model chunks must not enqueue one message
per chunk.

The following events advance real progress:

- a non-empty agent text delta;
- a non-empty agent thought/reasoning delta;
- the first appearance of a tool call;
- a tool update that changes observable status, progress, content, result,
  locations, or structured metadata;
- a changed plan update;
- successful terminal creation for the active turn;
- one or more newly decoded terminal output bytes, whether or not the terminal
  could be associated with a visible tool call;
- first publication of a terminal exit status; and
- a recognized agent extension notification that carries actual model or tool
  progress.

The following do not advance progress:

- frontend keepalive or touch calls;
- user messages, feedback submission, permission answers, or question answers;
- usage, mode, configuration, selector, session-info, or status-only updates;
- repeated identical tool or plan updates;
- terminal output polling that returns no new byte and no new exit status;
- a pending `waitForExit` call by itself;
- successful lookup, wait, output, kill, or release RPC traffic with no new
  process state; and
- an invalid terminal-ID request or its error log.

This distinction prevents a tight polling loop, a noisy keepalive, or repeated
invalid calls from hiding a genuinely silent turn.

### Unified activity clock

`SessionState.last_agent_activity_at` currently feeds Broker child observation.
Its semantics are broadened to include terminal progress and should be renamed
internally to `last_turn_progress_at` where practical. Wire compatibility for
existing delegation observation fields can retain
`last_agent_activity_at` while sourcing it from the unified clock.

For a Broker child, the child connection's unified progress clock continues to
drive the parent Delegation Card. For a root turn, the same clock drives the new
turn-health snapshot and banner.

For Grok native sub-agents, every native child shares the root ACP connection.
Model/tool events and terminal output from any child advance the root clock.
This means V1 detects root-level silence, not silence of one child while a
sibling remains active. The absolute root deadline still guarantees a final
boundary.

## Architecture and State Machine

The lifecycle state is backend-internal:

```text
idle
  -> running(turn_id)
       -> finishing(success | agent_failure | user_cancel | timeout |
                    disconnect | delegation_suspend)
            -> quarantine(user_cancel | timeout only)
                 -> reusable | disconnected
            -> idle(other terminal causes)
```

The observation state exists only inside `running`:

```text
active --silence threshold--> stalled
stalled --real progress-----> active
active/stalled --input open-> waiting_input
waiting_input --input close-> active or stalled, derived from last progress
```

The hard deadline is a separate transition from every running observation:

```text
running(active | stalled | waiting_input) --absolute deadline--> finishing(timeout)
```

It is not implemented as "stalled for another N minutes". A continuously
chatty but runaway turn still reaches its absolute deadline.

### Component boundaries

#### `TurnGuard`

- Owns turn start instants, the soft-threshold watch, immutable hard deadline,
  current observation, and activity wake.
- Derives observation using a shared pure function also usable by the existing
  delegation supervisor.
- Emits `ObservationChanged` and `HardDeadlineElapsed` signals only.
- Has no database, agent connection, terminal kill, Broker, or event-emitter
  capability.

#### ACP connection loop

- Creates and retires the guard for each prompt command.
- Feeds normalized ACP progress into the generation-scoped activity handle.
- Selects guard signals alongside prompt/session/command events.
- Owns the finalization fence and invokes the single finalizer.
- Runs cancellation quarantine and decides whether the connection is reusable.

#### `SessionState`

- Stores the current generation, immutable start/deadline values, observation,
  and shared progress clock for attach/reconnect recovery.
- Applies turn-health events idempotently by `turn_id`.
- Materializes a current `TurnHealthSnapshot` from the shared progress clock
  when serving a snapshot, even if no observation transition was emitted.
- Clears health, live message, active tools, pending input, and
  `turn_in_flight` on the winning `TurnComplete`.
- Keeps the backend-only `turn_recovery_in_flight` admission fence set until
  cancellation acknowledgement or connection teardown.
- Never writes durable conversation lifecycle directly.

#### `TerminalRuntime`

- Snapshots the active turn generation on terminal creation.
- Emits coalesced terminal progress to that generation's activity handle.
- Detaches and cancels only terminals owned by a specified turn during Stop or
  timeout.
- Retains session-wide cleanup for disconnect and process teardown.

#### ACP lifecycle subscriber

- Maps `TurnComplete{stop_reason: "timeout"}` to the root cancellation CAS.
- Maps a Broker child timeout to `DelegationError::ChildTimeout` and lets the
  Broker own task/conversation settlement.
- Treats late or duplicate terminal events as CAS losers.

#### `DelegationBroker`

- Adds `ParentTurnEndReason::ParentTimedOut` with stable `parent_timeout`
  task and attention-resolution codes.
- Cancels live descendants when their parent turn times out.
- Continues to treat `stalled` as observation only.

## Event and Snapshot Contract

Add an operational ACP event:

```text
TurnHealthChanged {
  turn_id: string,
  observation: "active" | "stalled" | "waiting_input",
  started_at: datetime,
  last_progress_at: datetime,
  stalled_since?: datetime,
  deadline_at?: datetime
}
```

`deadline_at` is absent when hard enforcement was disabled for this turn.
`stalled_since` is exactly `last_progress_at + stalled_after_seconds`, not the
time a scan happened to run.

The event is emitted:

- once when a turn becomes active;
- when the observation enum changes; and
- when a live soft-threshold change changes the derived observation.

It is not emitted for every activity pulse. `SessionState` still updates its
snapshot view through the shared progress clock so a later snapshot is current.

`LiveSessionSnapshot` gains an optional `turn_health` field. It is present only
while a turn is running and is cleared by the winning `TurnComplete`. The short
cancellation quarantine is an internal admission fence and is not represented
as turn health. The frontend replaces its local value from snapshots and
events; it never derives backend truth from a browser timer alone.

Hard timeout emits, in order:

1. `AcpEvent::Error` with `code="turn_hard_timeout"`, `terminal=false`, and a
   localized-display fallback message;
2. exactly one `AcpEvent::TurnComplete` with `stop_reason="timeout"`; and
3. `ConversationStatusChanged{Cancelled}` when the lifecycle CAS wins.

The error explains why work stopped. `TurnComplete` closes live rendering and
releases ordinary visible turn state; the separate internal cancellation
quarantine can still defer admission briefly. The status event converges the
sidebar and other clients. No event claims the agent completed its work.

Mixed-version compatibility is additive: old clients ignore the new event and
optional snapshot field but still process `Error`, `TurnComplete`, and the
existing cancelled conversation status.

## Timeout and Cancel Ordering

User Stop and hard timeout call the same `finalize_cancelled_turn` function
with different causes. The required ordering is:

1. claim the generation-scoped finalization fence;
2. mark the generation retired so later activity cannot touch another turn;
3. send ACP `CancelNotification` best-effort;
4. atomically detach turn-owned terminals from the public runtime map and
   signal their cancellation tokens;
5. emit timeout `Error` when the cause is hard timeout;
6. emit the single `TurnComplete` immediately, without waiting for process
   teardown;
7. cancel pending permission/question responders;
8. start bounded terminal process-tree kill, pipe drain, and exit publication;
9. cascade Broker descendants with `ParentCanceled` or `ParentTimedOut`;
10. converge durable root or child state through idempotent CAS; and
11. drain for cancellation acknowledgement, disconnecting after the five-second
    grace if none arrives.

Detaching the terminal entries before `TurnComplete` ensures the user-visible
turn cannot remain logically dependent on them. Awaiting process-tree cleanup
after `TurnComplete` preserves responsive UI recovery. The existing three-second
session cleanup bound remains the maximum synchronous wait; owned cleanup tasks
continue safely after that bound.

If terminal detach, kill, Broker cascade, logging, or status broadcast fails,
the winning `TurnComplete` is not rolled back. Each cleanup path records its
own structured error and remains independently retryable or reconcilable.

### Normal and failure completion

Normal `end_turn` and agent failure reasons use the same finalization fence but
do not kill turn terminals automatically unless existing cleanup policy already
requires it. They retire the terminal activity handle so late output cannot
advance a future turn.

Deliberate delegation suspension is a distinct final disposition. It retires
the current guard without sending Cancel to Broker children and transfers
ownership under the continuation design. The continuation's next hidden prompt
receives a new turn ID and a fresh hard deadline.

### Startup and disconnect convergence

Prompt turns do not survive process restart. Startup reconciliation changes a
root `InProgress` row with no recoverable live connection/turn to `Cancelled`.
Broker reconciliation continues to settle orphaned `running` children through
its existing host-restart path.

Disconnect while a turn is running remains terminal cleanup, not hard timeout.
It uses session-wide terminal release because the entire connection is gone.
The conversation CAS still prevents a previously completed or canceled row
from being overwritten.

## Native Grok Behavior

Grok native sub-agents are scheduled inside the Grok agent process and do not
create Codeg Broker task rows. Codeg therefore cannot safely treat a native
sub-agent UUID as a terminal ID or address one child with Broker cancellation.

The V1 guarantee is at the owning ACP turn:

- all Grok model/tool updates contribute to one root turn activity clock;
- every ACP terminal created during that turn is tagged with its root turn ID,
  even when terminal-to-tool association is ambiguous;
- root Stop or timeout sends Cancel to Grok and kills all terminals tagged to
  that root turn;
- the root emits `TurnComplete{timeout}` and becomes `Cancelled`; and
- late native child output is fenced during cancellation quarantine.

This is stronger than relying on Grok to choose a shell timeout but intentionally
coarser than Broker-backed delegation. Per-native-child stall and cancel would
require Grok to expose stable child lifecycle and cancellation handles in its
ACP extension; that is a future integration, not a heuristic based on UUID
shape.

## Long Command Policy

The terminal prompt context and relevant tool descriptions should recommend:

- start a command in background mode when it may exceed a normal foreground
  observation window;
- retain the returned `term_*` terminal ID separately from native sub-agent or
  tool-call IDs;
- poll output/status in bounded intervals rather than issuing one very long
  model-blocking wait; and
- release the terminal after observing a terminal exit status.

Recommended polling intervals are 10 to 30 seconds. A single model-selected
wait must not exceed 60 seconds in Codeg-authored guidance. These are context
efficiency recommendations, not execution limits.

`wait_for_terminal_exit` may continue to wait naturally without a per-call
timeout because the owning turn guard and cancellation token provide the
authoritative outer bound. The terminal runtime must remain cancellation-safe
when that wait and a kill race.

## Terminal Ownership and Buffer Redesign

### Turn ownership

`TerminalInstance` gains its creation `turn_id` and an optional activity
handle. `TerminalRuntime` maintains an active-turn context per session while a
prompt is running.

On create:

1. validate the session and request;
2. snapshot the current active turn context;
3. spawn the process;
4. insert the terminal with that immutable owner; and
5. record one terminal-create progress pulse.

On timeout or Stop, `release_all_for_turn(session_id, turn_id)` removes only
matching entries, signals them, and starts owned cleanup tasks. On connection
disconnect, `release_all_for_session` remains authoritative and removes every
entry regardless of generation.

During cancellation quarantine, new create requests for the retired session
are rejected with `terminal_turn_retired` until the agent acknowledges cancel
or the connection is replaced. A terminal created during an active turn keeps
that turn ID even if it intentionally outlives a normal turn. A terminal
created outside any active turn after normal completion is session-owned with
no turn ID and is cleaned only by explicit release or session teardown. The
quarantine rejection is narrow and temporary.

### Chunked retained output

Replace the capped `String` with a bounded chunk deque:

```text
TerminalOutputBuffer {
  chunks: VecDeque<OutputChunk>,
  retained_bytes: usize,
  base_offset: u64,
  end_offset: u64,
  truncated: bool
}
```

The reader uses a 32 KiB buffer and an incremental UTF-8 decoder. Decoded text
is coalesced into chunks up to 64 KiB. When retained output exceeds the limit,
the buffer drops complete head chunks and trims at most one partial head at a
UTF-8 boundary. It never drains the prefix of a 1 MB `String` on each 4 KiB
append.

The existing wire semantics remain:

- default retained limit is 1,000,000 bytes;
- `truncated` becomes true after the first eviction and never returns to false;
- `base_offset` advances by the exact number of normalized UTF-8 bytes evicted;
- `terminal_output_delta` returns `had_gap=true` when the requested offset is
  older than `base_offset`;
- exit status is not exposed until reader drain completes or the existing
  bounded drain path gives up; and
- output and delta snapshots are assembled only from retained chunks.

Appending and eviction are amortized O(new bytes). Full `terminal/output`
still costs O(retained bytes), which is expected and bounded. Delta output costs
O(returned bytes plus the small number of intersected chunks).

Each successful append updates the terminal's total-output counter and emits
one coalesced activity wake for its immutable turn owner. Output need not be
associated with a visible tool call to count.

## Invalid Terminal IDs and Release Semantics

Terminal IDs created by Codeg have the stable `term_` shape. Native sub-agent
UUIDs, ACP tool-call IDs, and arbitrary strings are not interchangeable.

Unknown or invalid IDs remain RPC errors for:

- output;
- wait;
- kill; and
- release when the ID was never issued to that session.

The error response gains stable detail codes that distinguish invalid shape,
unknown ID, wrong session, and already released. This lets agent adapters
correct behavior without parsing English strings.

Release is idempotent only for a terminal known to have been released in the
same session. `TerminalRuntime` keeps a bounded, expiring tombstone cache of
released `(session_id, terminal_id)` pairs: at most 1,024 entries per runtime,
each retained for ten minutes, with oldest-expiry eviction. A duplicate release
returns success; an arbitrary UUID or a terminal belonging to another session
remains an error. Output, wait, and kill against a tombstone remain errors
because pretending that state or output still exists would hide bugs.

Invalid-ID logging uses a token bucket keyed by connection, operation, and ID
shape, not by raw ID. Each bucket has capacity five and refills one token per
minute. Suppressed counts are summarized once per minute while nonzero. This
prevents an unbounded key set and ensures a repeated native sub-agent UUID
cannot flood logs. The raw invalid ID is never logged.

## UI Behavior

### Root turn

When the current root turn becomes `stalled`, show a restrained full-width
status banner above the conversation stream:

```text
No agent or terminal activity for 5 minutes. The turn is still running.
[Stop turn]
```

When a hard deadline is enabled, the banner may also show the absolute stop
time or remaining duration derived from `deadline_at`. Browser timers update
display only; backend events remain authoritative.

The existing composer Stop action remains available throughout prompting. The
stalled banner calls the same `acp_cancel` action and changes to a short
"Stopping" state while the request is submitted. It does not mark the turn
complete optimistically.

Real progress removes the stalled banner immediately on the `active`
transition. `waiting_input` uses the existing permission or question UI and
does not show a competing stall warning. A reconnect or second client restores
the same state from `turn_health`.

### Broker delegation child

The existing Delegation Card continues to render `active`, `stalled`, and
`waiting_input` from Broker observation. It must not convert `stalled` into a
completed or failed badge. Opening the child conversation exposes that
connection's root-level stalled banner and Stop action.

### Native Grok sub-agent

Native child tool cards remain agent-authored views. Because there is no safe
per-child cancel handle, the product exposes the owning root turn Stop action
rather than a fake per-child button. The root stalled banner remains visible
regardless of which native child caused the silence.

### Hard timeout result

On hard timeout:

- stop streaming and clear running tool state through `TurnComplete`;
- show a localized error explaining the configured maximum duration was
  reached;
- leave the durable conversation status as `cancelled`;
- never show review, success, or completion affordances; and
- allow a new queued prompt after cancellation acknowledgement or reconnect.

All ten supported locales receive the error, banner, action, and setting copy
in the same change. The backend error code, not the English fallback message,
selects localized text.

## Durable State Mapping

The terminal cause mapping is explicit:

| Cause | `TurnComplete.stop_reason` | Root conversation | Broker child task | Descendant cascade |
| --- | --- | --- | --- | --- |
| Clean agent end | `end_turn` | `pending_review` | `completed` | Join policy |
| User Stop | `cancelled` | `cancelled` | `canceled` | `parent_canceled` |
| Hard deadline | `timeout` | `cancelled` | `failed` / `child_timeout` | `parent_timeout` |
| Agent refusal/budget/empty | existing reason | `cancelled` | `failed` / specific code | `parent_turn_failed` |
| Disconnect | no successful turn | `cancelled` by terminal reconciliation | canceled/failed by Broker ownership | `parent_disconnected` |
| Delegation suspension | internal suspension disposition | remains in progress under continuation lock | children keep running | none |

Every root write is `InProgress -> target` compare-and-set. Every Broker task
write uses the existing durable task CAS. A terminal row never moves backward,
and a late drained `end_turn` cannot change a timed-out row to `pending_review`.

## Observability

### Turn diagnostics

Emit structured records and process-local counters for:

- turn admitted, with agent type, purpose, configured soft threshold, and
  whether a hard deadline is enabled;
- observation transitions and stall duration;
- hard deadline reached;
- finalization winner and losing duplicate sources;
- Cancel notification send outcome;
- cancellation acknowledgement versus grace-expiry disconnect;
- terminal detach count and bounded cleanup outcome;
- root status CAS outcome; and
- Broker descendant cascade outcome.

Records may include connection ID, opaque turn ID, agent type, purpose, elapsed
milliseconds, counts, stable reason codes, and booleans. They must not include
prompt text, model text, tool arguments, command text, paths, terminal output,
environment values, or tokens.

### Terminal diagnostics

For create, wait, output, kill, and release, record:

- operation and stable outcome code;
- connection/session-scoped opaque identifiers;
- turn ownership presence, never command content;
- elapsed milliseconds;
- retained and total output byte counts;
- truncation/gap flags;
- exit-status presence, not raw output; and
- rate-limited invalid-ID shape and suppression count.

Recommended counters include active terminals, creates, natural exits, kills,
release cleanup timeouts, output bytes, evicted bytes, invalid IDs by operation
and shape, duplicate releases, turn stalls, stall recoveries, hard timeouts,
and cancellation-grace disconnects.

## Race and Failure Handling

### Agent completion versus deadline

Both attempt the finalization fence. The winner owns all lifecycle side
effects. A losing timeout emits no error; a losing agent completion cannot
write reviewable state.

### Stop versus deadline

Both are cancellation causes. The first claim chooses the diagnostic cause;
cleanup remains idempotent. If the user clicked before the deadline but the
deadline task won scheduling, the result may be `timeout`; it still settles to
`cancelled` and never duplicates cleanup.

### Terminal exit versus kill

The existing cancellation token and retained exit-status watch remain the
coordination mechanism. Natural exit or kill publishes exactly one exit status.
Turn detach is map-level and does not cancel the owned cleanup future.

### Terminal output versus turn retirement

An output append may race retirement. It may update the terminal's own retained
buffer, but its generation-scoped activity pulse is ignored after retirement
and cannot revive or reset another turn.

### Settings change versus deadline

Soft changes re-derive observation. Hard changes cannot mutate an admitted
turn's captured deadline. The successful settings transaction is the boundary
for future turns.

### Status persistence failure

`TurnComplete` still releases UI state. A transient root status-CAS failure is
retried at 100 ms, 500 ms, and 2 seconds, with each attempt using the same
idempotent expected status and target. Connection teardown and startup
reconciliation provide a second convergence path if all attempts fail. A
failed status broadcast does not change the database winner.

### Cleanup timeout

Terminal cleanup tasks are owned and continue after the three-second caller
bound. Cancellation quarantine has its own five-second acknowledgement bound.
Neither bound can interrupt a process state mutation halfway or leave the
conversation in progress.

## Security and Isolation

- Turn IDs are backend-generated opaque values and are never accepted from an
  agent request.
- Terminal cleanup matches both session ownership and immutable turn ownership.
- Wrong-session IDs never reveal whether another session owns the terminal.
- Invalid-ID logs omit raw IDs and request contents.
- Activity cannot be advanced by frontend keepalives or arbitrary user input.
- A child connection cannot alter its parent's guard or deadline.
- Broker cascade authorization continues to follow persisted direct parent
  edges.
- Settings writes use the existing authenticated desktop/server command
  surfaces and publish only after a successful transaction.

## Testing Strategy

### Turn guard unit tests

- active becomes stalled exactly at 300 seconds of silence;
- real model activity and terminal output recover stalled to active;
- repeated identical updates and empty polls do not recover it;
- waiting input takes precedence without pausing the hard deadline;
- lowering and raising the live soft threshold re-derives observation;
- hard timeout fires at the captured 1800 seconds despite continuous progress;
- hard timeout fires while stalled and while waiting for input;
- zero disables hard enforcement;
- a later settings change does not alter an admitted deadline; and
- an old generation's activity is ignored after a new turn starts.

Tests use paused Tokio time and a fake wall clock; they never sleep for real
minutes.

### Connection integration tests

- a mock prompt RPC that never returns is canceled at the hard deadline;
- Cancel notification precedes terminal detach and `TurnComplete` emission;
- timeout emits one coded Error and one `TurnComplete{timeout}`;
- user Stop uses the same cleanup path without the timeout Error;
- prompt completion racing the deadline has exactly one winner in both orders;
- late completion after timeout cannot emit a second TurnComplete or write
  `pending_review`;
- pending permission and question responders are canceled;
- cancellation acknowledgement reuses the connection only after drain;
- missing acknowledgement disconnects after five seconds;
- a queued next prompt runs on a clean reusable or reconnected session; and
- deliberate delegation suspension retires the guard without canceling
  children.

### Native Grok tests

- two ambiguous native shell tool calls still create turn-owned terminals;
- output from an unbound terminal advances root turn progress;
- a native sub-agent UUID passed to output/wait/kill remains an RPC error;
- repeated invalid UUID-shaped IDs produce bounded logs and aggregate counts;
- root hard timeout kills every terminal created by that turn; and
- a terminal from an older generation is not killed by a later turn's scoped
  cleanup.

These tests use a deterministic ACP fixture or recorded protocol sequence, not
the live Grok service in the required CI path.

### Terminal buffer tests

- output below the limit round-trips exactly;
- multi-byte UTF-8 split across reader boundaries is preserved;
- eviction never splits a UTF-8 code point;
- absolute offsets, `had_gap`, and truncation remain compatible;
- concurrent output and exit publication preserve tail output;
- duplicate same-session release succeeds through a tombstone;
- arbitrary, wrong-session, and tombstoned output/wait/kill requests fail;
- tombstone size and age are bounded; and
- a 100 MB synthetic stream retains at most the configured 1 MB plus bounded
  chunk overhead and performs amortized linear append/eviction work.

### Lifecycle and Broker tests

- root `timeout` CASes `InProgress -> Cancelled`;
- a timed-out row cannot later become `PendingReview`;
- child `timeout` settles `Failed` with `child_timeout`;
- parent timeout settles live descendants `Canceled` with `parent_timeout`;
- attention waits close with the matching parent-timeout resolution;
- repeated timeout/cancel/disconnect settlement is idempotent; and
- startup reconciliation cancels an orphaned root in-progress turn.

### Frontend tests

- live and snapshot turn-health paths render the same stalled banner;
- active recovery removes the banner;
- waiting input does not render a competing stall warning;
- Stop invokes the existing ACP cancel action once and shows a bounded pending
  state;
- timeout displays localized error copy and cancelled status;
- timeout never renders completion/review affordances;
- mixed-version snapshots without `turn_health` remain valid; and
- all ten locale files contain the new keys.

### Required verification

The implementation plan must include focused Rust tests, frontend Vitest tests,
full `pnpm test`, frontend lint/build checks appropriate to touched files, and
desktop, server, and `codeg-mcp` Rust check/test/clippy commands required by
`AGENTS.md`.

## Rollout and Compatibility

1. Land the generation/activity tracker, chunked output buffer, structured
   diagnostics, and soft UI with hard enforcement available in a test/canary
   mode.
2. Exercise mocked hung turns and real canary agents while recording would-fire
   hard deadlines, cleanup latency, false-stall rates, and cancellation-grace
   disconnects.
3. Enable enforcement for all prompt purposes with the 1800-second default.
   Existing installations without the new key receive the product default;
   users with exceptional workloads can raise it or set zero.
4. Keep the compatibility soft-threshold alias and additive wire fields during
   mixed desktop/server version operation.
5. Remove only the canary gate after telemetry shows no duplicate completion,
   accidental review transition, terminal leakage, or material false timeout.

Rollback sets hard enforcement to zero for newly admitted turns while leaving
soft observation, generation fencing, terminal ownership, buffer improvements,
and reconciliation active. Already admitted deadlines keep their captured
policy; emergency process rollback still follows normal connection teardown.

## Acceptance Criteria

- With default settings, a turn with no real progress is visibly stalled after
  five minutes and remains explicitly running.
- The user can Stop a stalled root turn without opening terminal or sub-agent
  diagnostics.
- With default settings, any still-running ACP prompt reaches a backend-owned
  terminal outcome no later than 30 minutes plus scheduler/cleanup dispatch
  latency, regardless of model-supplied timeout values.
- Hard timeout sends ACP cancellation, detaches and stops turn-owned terminals,
  clears pending input, and emits exactly one terminal event sequence.
- A timed-out or canceled root conversation durably settles as `cancelled` and
  never as `pending_review`.
- A timed-out Broker child reports `child_timeout`; a timed-out parent cancels
  live descendants with `parent_timeout`.
- Grok native sub-agent work is covered by the owning root deadline even when
  terminal-to-tool association is ambiguous.
- Model, tool, and terminal output progress recover a soft stall, while empty
  polling and invalid terminal requests do not.
- Large terminal output no longer performs repeated prefix shifts of the full
  retained 1 MB window.
- Invalid terminal IDs remain explicit RPC errors, duplicate legitimate
  release is idempotent, and repeated noise is rate-limited without raw IDs.
- Desktop and server clients recover the same turn health from snapshots and
  events.
- Commands, output, paths, environments, prompts, and secrets never appear in
  the new diagnostic records.

## Deferred Extensions

- Per-native-sub-agent health and cancel, after an agent exposes stable child
  lifecycle and cancellation handles.
- Per-agent or per-profile hard-timeout overrides, after operational data shows
  one global policy is insufficient.
- A durable root last-turn error reason if product UX later needs timeout cause
  after full application restart rather than only live events and logs.
- User-facing per-terminal controls for intentionally detached background jobs.
- Adaptive soft thresholds based on command type. V1 deliberately uses one
  understandable threshold and never parses command text.
