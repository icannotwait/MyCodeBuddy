# Continuation MCP Release and Transcript Deduplication Design

## Status

Approved in conversation on 2026-08-01.

This document is a corrective amendment to:

- `2026-07-19-delegation-continuation-design.md`; and
- `2026-07-24-delegation-wait-watchdog-correlation-design.md`.

Those designs remain authoritative for continuation persistence, suspension,
checkpoint wakeups, wait ownership, watchdog correlation, user Stop, and child
lifecycle. This amendment restores their requirement that suspension abandons
the foreground Join response without canceling the child.

## Problem

Conversation `2582` exposed a deterministic continuation loop for delegation
task `81cb187d-1473-43bf-be66-43072f554407`:

1. The parent called `get_delegation_status` with
   `return_when=all_terminal_or_attention` and `wait_ms=0`.
2. The continuation coordinator persisted ownership and suspended the parent
   turn.
3. The parent turn became prompt-admissible, but the listener deliberately kept
   the MCP `tools/call` open.
4. The Codex MCP host retained that call for 300 seconds and then reported
   `timed out awaiting tools/call after 300s`.
5. The 600-second checkpoint resumed the parent, which rejoined the same task
   through a new MCP call id.
6. Each timeout became another assistant/tool history record and could surface
   as another status or sub-conversation-like card.

The database contained one generation-1 delegation run and the rollout
contained one `delegate_to_agent` call. The repeated cards therefore represented
repeated wait records for one child, not repeated child creation.

The immediate implementation fault is in the `ArmStatus::Suspended` branch of
`src-tauri/src/acp/delegation/listener.rs`: after suspension ownership has
transferred, it waits on `cancel_rx` instead of releasing the MCP request.

## Goals

- Release the foreground MCP status request immediately after suspension is
  acknowledged.
- Remove the 300-second busy window so the same parent session can be resumed
  as soon as it is suspended.
- Preserve the 600,000 ms continuation checkpoint and its real parent-model
  cache-refresh turn.
- Preserve durable continuation ownership and keep every joined child running.
- Preserve terminal and attention wake behavior.
- Show one delegation card per real run, even when checkpoint rejoins use new
  MCP call ids.
- Keep unknown or unrelated status calls visible for diagnosis.
- Cover the observed rollout shape with deterministic regression tests that do
  not sleep for real checkpoint or host-timeout durations.

## Non-Goals

- Disabling or changing the 600-second checkpoint.
- Changing `wait_ms=0` for legacy, non-continuation callers.
- Canceling, replacing, or restarting a child when its parent is suspended.
- Adding a new Codex-host request-cancellation protocol.
- Shortening the host's generic 300-second MCP timeout.
- Deleting raw rollout audit records.
- Collapsing distinct delegation or continuation runs into one card.

## Considered Approaches

### Release on suspension acknowledgement

After `ArmStatus::Suspended`, return the current task snapshot immediately while
the coordinator continues to own the durable continuation. This is selected
because it stays entirely inside Codeg, matches the original continuation
contract, and removes the busy window at its source.

### Propagate parent-turn cancellation into Codex MCP request cancellation

This would require a reliable cancellation contract from the external Codex
host to the per-launch companion. The current host does not provide that signal
for the observed abort, so this approach expands the integration boundary and
cannot be the primary fix.

### Shorten the MCP or watchdog timeout

This only reduces the duration of the busy window. It still produces timeout
errors, new call ids, repeated checkpoint history, and a period in which resume
can fail. It is rejected.

## Selected Design

### Listener release boundary

The continuation-enabled Join path retains the existing preflight, wait
registration, owner transfer, durable continuation insert, and parent suspension
sequence.

Once the arm worker returns `ArmStatus::Suspended`, the listener must:

1. disarm its listener-owned wait guard;
2. stop parking on `cancel_rx`;
3. return `continuation_release_batch` with no synthetic cancel cause; and
4. allow request-local drop cleanup to notify the transferred waiter that the
   foreground request is gone.

The listener must not explicitly reclaim coordinator-owned continuation state
or cancel Broker tasks. `TransferredWait` remains responsible for exact-stamp
registry cleanup. The durable coordinator remains responsible for Broker
notification, checkpoint timing, wake claiming, and hidden prompt admission.

The release batch uses the existing task snapshot contract and
`DelegationWakeReason::Unavailable`. No new wire enum or public MCP schema is
introduced. A task that is still running returns a structured running snapshot;
one that terminalizes in the suspension-to-snapshot race returns its current
terminal snapshot. In both cases, the coordinator's CAS rules remain the sole
authority for continuation wake and prompt admission.

### Preserved checkpoint flow

`CONTINUATION_CHECKPOINT_MS` remains `600_000`.

When a checkpoint wins, Codeg admits one hidden continuation prompt containing
the authoritative task snapshot. If the model rejoins tasks that are still
running, that new Join creates the next continuation generation and follows the
same fast release path. Rejoining can therefore create a new raw MCP call id,
but it cannot keep the session busy for the host timeout.

### Cancellation and race semantics

- Cancellation before owner transfer retains the existing listener arbitration.
- Cancellation during transfer retains the existing exact-stamp and handoff
  rules.
- Suspension acknowledgement ends only the foreground status request.
- Request-local cleanup after acknowledgement must not fail or cancel the
  durable continuation.
- User Stop remains authoritative and cancels the continuation plus delegation
  tree through the existing parent-stop path.
- Child completion and parent attention continue to race the checkpoint through
  the existing one-winner continuation CAS.
- A terminal result between suspension acknowledgement and release snapshot is
  reported as terminal without creating another child or another wake winner.
- An arm or suspension failure before acknowledgement remains a structured
  `continuation_arm_failed` error and does not use the fast-release path.

### Transcript projection

Raw Codex MCP completions remain reconstructable for audit and parser fidelity.
The user-visible projection must apply these rules across historical, promoted,
and live turns:

- `delegate_to_agent` and `continue_delegation` remain first-class cards.
- A status-only call whose non-empty task ids all belong to known delegation
  runs is folded into those runs and does not render another card.
- The rule is independent of MCP call id, checkpoint generation, intervening
  assistant text, and whether the status completion is a structured snapshot or
  a historical timeout error.
- A status call with no usable identity, or with any unknown task id, remains
  visible.
- Distinct continuation runs remain distinct delegation cards; this amendment
  does not merge real runs by child conversation id alone.

The existing task-aware transcript projection is the implementation boundary.
Implementation must enforce the rules above for live, promoted, and historical
turns and add the exact regression fixture. If the current production path
already satisfies a layer's assertions, that layer needs no behavioral rewrite;
the regression remains required. Broad consecutive-card merging is not allowed.

## Data Flow

```text
parent Join tools/call
  -> listener preflight and exact wait registration
  -> coordinator persists continuation and takes wait ownership
  -> connection suspends the exact parent turn
  -> suspension acknowledgement clears the parent turn gate
  -> listener returns current task snapshot immediately
  -> MCP tools/call ends; parent session is not busy
  -> coordinator waits independently
       -> all terminal / attention / unavailable: hidden resume prompt
       -> 600-second checkpoint: hidden cache-refresh prompt
  -> a still-running checkpoint rejoin repeats the fast release path
```

## Testing Strategy

### Listener RED/GREEN regression

Create a continuation-enabled running-task Join with a test suspension port.
Release the suspension acknowledgement and assert, under a short test timeout,
that the status request returns a running snapshot. The current implementation
must fail this test by remaining parked on `cancel_rx`.

After the production change, assert:

- the request returns without a wait-cancel signal;
- the task remains `running`;
- the continuation row remains active in `waiting` state;
- no child cancel or disconnect is recorded; and
- the exact wait registration is eventually cleaned up without an ownerless
  entry.

### Continuation and checkpoint regression

Use the fake continuation clock to advance exactly 600,000 ms after fast
release. Assert one checkpoint wake, one hidden prompt admission, and a valid
next continuation generation when the parent rejoins. No test sleeps for ten
real minutes.

Retain coverage for terminal and attention wakes before the checkpoint.

### Parser and transcript regression

Build a fixture matching conversation `2582`:

- one reconstructed `delegate_to_agent` completion;
- multiple `get_delegation_status` calls with unique call ids;
- checkpoint assistant text between calls;
- both historical 300-second timeout errors and post-fix structured running
  snapshots; and
- one stable task id and child conversation id.

Assert the parser keeps the audit records while the transcript projection emits
one delegation work-unit card and no residual status row for the known task.
Add separate live and promoted-turn assertions using `task_ids` input. Mixed or
unknown task ids must remain visible.

### Compatibility regression

Retain existing tests for:

- capability-off legacy `wait_ms=0` terminal waits;
- positive bounded legacy waits;
- user Stop and watchdog cancellation attribution;
- peer close during transfer;
- child completion during suspension; and
- continuation state/CAS ownership races.

## Acceptance Criteria

For a reconstructed conversation `2582` scenario:

1. The database contains one delegation run and the child remains running until
   its own terminal outcome or explicit cancellation.
2. Each checkpoint Join reaches suspension and ends its MCP request promptly,
   without a 300-second host timeout.
3. The parent connection is prompt-admissible immediately after suspension and
   can be resumed without a stale busy lock.
4. The 600-second checkpoint continues to wake the same parent agent session.
5. Repeated checkpoint status calls produce no additional user-visible child or
   status cards for the known task.
6. Terminal and attention wakeups still resume exactly once.
7. Unknown status calls and distinct real delegation runs remain visible.

## Verification

Development uses focused RED/GREEN commands for the listener, continuation,
Codex parser, transcript projection, and live transcript tests. Before claiming
completion, run the relevant frontend suite plus desktop Rust checks and tests
for the affected delegation modules. Run strict Clippy on the affected Rust
target and `git diff --check`.
