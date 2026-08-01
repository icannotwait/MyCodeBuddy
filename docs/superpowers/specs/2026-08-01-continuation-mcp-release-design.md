# Continuation MCP Release and Transcript Deduplication Design

## Status

Approved in conversation on 2026-08-01.

Amended on 2026-08-01 after independent design review. The selected fast-release
approach and the goals, non-goals, and public protocol surface are unchanged.

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

Once the listener's single exact-stamp arbitration selects
`ArmStatus::Suspended`, the listener must:

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
introduced. Its exact snapshot semantics are defined under
"Release decision, cancellation, and snapshot linearization" below. In every
case, the coordinator's CAS rules remain the sole authority for continuation
wake and prompt admission.

### Foreground MCP-release fence

Suspension acknowledgement is not by itself permission to admit a hidden
prompt. Each continuation Join creates one internal
`ForegroundMcpReleaseFence` before arming. The `serve_one` status request owns
the fence's release end, and the coordinator worker for that continuation
generation owns the wait end. The fence is process-local control state, not a
public MCP wire enum and not a new persistence or recovery boundary.
The listener passes the wait end through the internal Join-arm handoff beside
the transferred wait and retains the release end until `serve_one` finishes the
exact response; neither endpoint is serialized into `BrokerResponse`.

The fence has one pending state and two equivalent release outcomes:

- `FrameFlushed`: `serve_one` successfully completes `write_frame`, including
  its final `flush`, for the exact status `BrokerResponse` on the companion
  connection; or
- `PeerClosed`: `serve_one` observes EOF/read failure before delivery, or its
  response write fails, proving that the request peer is gone.

The owner and linearization point are explicit: the listener-side `serve_one`
task is the sole release owner, and the atomic pending-to-released transition
immediately after the successful frame flush, or when peer closure is
confirmed, is the foreground MCP-release linearization point. Returning from
`process_status`, constructing `continuation_release_batch`, and acknowledging
suspension do not open the fence. A drop guard on the listener-side release end
must publish `PeerClosed` when the exact request connection has failed, so the
coordinator cannot remain parked behind an ownerless fence.

A terminal, attention, unavailable, or checkpoint condition may win the durable
wake CAS while this fence is pending. The row may become `WakePending`, and the
coordinator may refresh its later prompt snapshot, but it must await the fence
before the `WakePending -> Resuming` transition and before the first call to
hidden prompt admission. User Stop remains able to cancel while the coordinator
is waiting on the fence.

The current integration has no acknowledgement from the external Codex host
that it consumed the JSON-RPC result and cleared the host-facing `tools/call`.
The successful listener-to-companion frame flush, or confirmed companion peer
closure, is therefore the nearest enforceable boundary. A short forwarding
race can remain after `FrameFlushed`, so bounded busy retry is an explicit part
of correctness, not incidental resilience. After the fence opens, an explicitly
retryable host-busy prompt-delivery result uses the existing bounded delays of
100 ms, 500 ms, and 2,000 ms. Every attempt retains the same
`internal_prompt_id`. Success admits once; exhaustion ends in the established
`PromptDeliveryFailed` terminal cleanup path. Non-busy failures keep their
existing classification, and no retry may start before the fence opens.

### Preserved checkpoint flow

`CONTINUATION_CHECKPOINT_MS` remains `600_000`.

The persisted deadline remains `wake_at = armed_at + 600_000 ms`. Suspension,
release decision, release-snapshot construction, frame delivery, and any
post-fence busy retry must not rewrite `armed_at`, reset `wake_at`, or extend the
current generation's checkpoint.

When a checkpoint wins, Codeg admits one hidden continuation prompt containing
the authoritative task snapshot. If the model rejoins tasks that are still
running, that new Join creates the next continuation generation and follows the
same fast release path. Rejoining can therefore create a new raw MCP call id,
but it cannot keep the session busy for the host timeout.

### Release decision, cancellation, and snapshot linearization

The foreground response uses one internal one-shot decision, named
`StatusReleaseDecision`, for the exact `WaitStamp`. Its owner is the listener's
existing cancel-versus-arm arbitration site, and its linearization point is the
branch that selects either request-scoped wait cancellation or the successfully
received `ArmStatus::Suspended`. That decision is made exactly once and is not a
public wire enum. The existing biased branch order remains deterministic: if
both inputs are ready in the same arbitration poll,
`ArmStatus::Suspended` and release win.

The following rules extend the July 24 transfer contract:

1. Before the release decision, the existing exact-stamp arbitration chooses
   cancellation or arming once. A cancellation winner may return the existing
   request-scoped cancel report, settles the exact registration, and prevents a
   later arm completion from producing a second response. Cancellation before
   owner transfer and during transfer otherwise retains the established handoff
   rules.
2. Once `ArmStatus::Suspended` wins the release decision, every later
   request-scoped wait cancellation is cleanup-only. It may settle and
   deregister `TransferredWait` by exact stamp, but it cannot synthesize a
   cancellation response, pass a cancel cause into `continuation_release_batch`,
   mutate the durable continuation, or cancel a child. The foreground response
   remains the causeless release snapshot.
3. User Stop is not request-scoped wait cancellation. It remains a separate
   coordinator and parent-tree operation and may CAS the continuation to
   `Cancelled` and cancel the delegation tree even while the observational
   release response is being built or delivered. The already selected release
   response still completes at most once and does not override Stop.
4. `continuation_release_batch` reflects one atomic Broker snapshot of the
   requested task set. The Broker snapshot read is the response-observation
   linearization point. A terminal commit before that read must appear terminal.
   A terminal commit after the release decision can appear as a later terminal
   observation if it precedes the snapshot read, or the response may retain the
   earlier running observation if it follows the read. Response construction
   never performs or competes in a wake CAS; Broker notification and the
   coordinator's existing one-winner CAS independently claim terminal,
   attention, unavailable, or checkpoint wake.

An arm or suspension failure before the release decision remains a structured
`continuation_arm_failed` error and does not use the fast-release path. Across
all orderings, there is one foreground response, exact-stamp cleanup, and one
durable continuation CAS winner. No request-scoped path cancels a child; only
the separate User Stop path does so under this amendment.

### Transcript projection

Raw Codex MCP completions remain reconstructable for audit and parser fidelity.
The user-visible projection must apply one normative predicate identically
across historical, promoted, and live turns:

- `delegate_to_agent` and `continue_delegation` remain first-class cards.
- Distinct continuation runs remain distinct delegation cards; this amendment
  does not merge real runs by child conversation id alone.

For each logical tool call, the predicate is:

1. Normalize only a tool call whose normalized name is exactly
   `get_delegation_status`. Other tool names never enter status folding.
2. Parse identity-bearing input and structured output without scraping free-form
   error or timeout text. Define `request_ids` as the trimmed, de-duplicated
   union of request `task_ids` and legacy request `task_id`. Define
   `report_ids` as the trimmed, de-duplicated union of the task identity from
   every structured report. The candidate identity set is
   `request_ids union report_ids`.
3. Build the identity index over the full parent conversation. Each collected
   id must map by exact task id to exactly one delegation run in that
   conversation. Prefix matching, child-conversation fallback, foreign-parent
   lookup, zero matches, and multiple matches are not acceptable mappings.
4. Fold the complete status call only when the candidate union is non-empty,
   every id has one unambiguous mapping, every identity-bearing structure parsed
   successfully, and, when both `request_ids` and `report_ids` are non-empty,
   those two sets are exactly equal.
5. Fail open on parse failure, an empty union, disagreement between request
   identities and structured reports, an ambiguous mapping, or any unknown id. The
   entire original status call remains visible with all of its rows. In
   particular, a mixed known/unknown call is indivisible: it is not rewritten
   into unknown-only residual rows.

An input-only historical timeout can therefore fold when its request ids are
all known. An output-only structured result can fold when every report id is
known and unambiguous. When both request and report identities are present,
their de-duplicated sets must be equal; even an all-known but mismatched pair
remains fully visible for diagnosis. The rule is independent of MCP call id,
checkpoint generation, and intervening assistant text.

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
  -> listener release decision selects ArmStatus::Suspended
  -> listener reads one atomic current task snapshot
  -> serve_one writes and flushes the response frame, or confirms peer close
  -> listener opens ForegroundMcpReleaseFence
  -> coordinator may wait or already hold one WakePending CAS winner
       -> all terminal / attention / unavailable: hidden resume prompt
       -> wake_at checkpoint: hidden cache-refresh prompt
       -> admission begins only after ForegroundMcpReleaseFence opens
       -> retryable host busy uses the bounded 100/500/2,000 ms schedule
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

### Foreground release-fence regression

Add a deterministic integration test through `serve_one`, not only a direct
`process_status` test. Give the exact status response connection a gated
write/flush so `write_frame` cannot reach its successful delivery boundary.
Release suspension, then make terminal or attention notification win the
continuation CAS while the frame gate remains closed. Assert the row is
`WakePending`, the response has not crossed the gate, and the hidden-admission
port has zero calls. Open the frame gate, let `serve_one` finish the frame and
open `ForegroundMcpReleaseFence`, then assert exactly one prompt admission with
the same continuation generation and no child cancellation.

Cover the integration limitation with fake time as well: after the fence opens,
make admission return retryable host busy, assert retries occur only at the
100 ms, 500 ms, and 2,000 ms boundaries with the same `internal_prompt_id`, and
assert either one eventual admission or `PromptDeliveryFailed` after the final
bounded attempt. No pre-fence retry or admission is permitted.

### Release-linearization regressions

Use explicit gates around the release decision and the atomic Broker snapshot
read for these deterministic cases:

- signal request-scoped wait cancellation before the release decision and
  assert cancellation wins the single response;
- let `ArmStatus::Suspended` win, hold snapshot/frame delivery, then signal the
  same wait cancellation and assert a causeless release response while
  `TransferredWait` only settles and deregisters;
- invoke User Stop after release wins but before response delivery and assert
  it cancels the durable continuation and delegation tree without replacing or
  duplicating the observational response;
- commit terminal state before the Broker snapshot read and assert the release
  response is terminal; and
- let the snapshot read capture `running`, commit terminal state afterward, and
  assert the release response may remain running while the coordinator observes
  terminal notification independently.

Every case asserts one foreground response, exact-stamp registry cleanup, one
continuation CAS winner, and no child cancel call. The User Stop case alone
asserts the established child-cancel cascade.

### Continuation and checkpoint regression

Capture the persisted `armed_at` and `wake_at` under the fake continuation
clock and assert `wake_at - armed_at == 600_000 ms`. Keep the clock controlled
through suspension and response release. Advance it to `wake_at - 1 ms` and
assert zero checkpoint claims and zero hidden prompt admissions. Advance it to
`wake_at` and assert exactly one checkpoint claim and, after the foreground
release fence is open, exactly one hidden prompt admission. A parent rejoin may
then create one valid next continuation generation. The test must not anchor
the deadline to "after fast release" or sleep for ten real minutes.

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
Keep intervening assistant text in the historical fixture so folding proves a
conversation/task-index lookup rather than adjacency.

Run the same predicate matrix against historical, promoted, and live
projection:

| Case | Expected projection |
| --- | --- |
| known request id plus input-only timeout | Fold the whole status call |
| known output-only structured report | Fold the whole status call |
| request/report identity mismatch | Preserve the whole status call |
| empty or unusable identity | Preserve the whole status call |
| all ids known, exact, and agreed | Fold the whole status call |
| mixed known and unknown ids | Preserve the whole call and all rows |
| all ids unknown | Preserve the whole call and all rows |
| parse failure or ambiguous task-to-run mapping | Preserve the whole call |

The promoted and live cases include `task_ids`; compatibility fixtures also
exercise legacy `task_id`. Assertions must prove that no layer implements
unknown-only residual rows for a mixed call.

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
3. A hidden continuation prompt is never admitted before the exact Join's
   `ForegroundMcpReleaseFence` opens. A retryable host-busy race after the
   nearest enforceable frame boundary is bounded and has one documented success
   or failure outcome.
4. The checkpoint remains exactly `armed_at + 600_000 ms` and continues to wake
   the same parent agent session once.
5. Repeated checkpoint status calls produce no additional user-visible child or
   status cards for the known task.
6. Terminal and attention wakeups still resume exactly once.
7. Unknown, mixed, mismatched, ambiguous, and unparseable status calls remain
   wholly visible, while known agreed calls fold identically in historical,
   promoted, and live projection.
8. In the release/cancel race cases, request cancellation on either side of the
   release decision produces one response and exact-stamp cleanup; only the User
   Stop case cancels children.

## Verification

Development uses focused RED/GREEN commands for the listener, continuation,
Codex parser, transcript projection, and live transcript tests. Before claiming
completion, run the relevant frontend suite plus desktop Rust checks and tests
for the affected delegation modules. Run strict Clippy on the affected Rust
target and `git diff --check`.
