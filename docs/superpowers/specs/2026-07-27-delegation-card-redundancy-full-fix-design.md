# Delegation Card Redundancy Full-Fix Design

## Status

Approved on 2026-07-27. This design extends and supersedes the implementation
boundary of
`2026-07-27-delegation-work-unit-sticky-runtime-ui-design.md`: sticky runtime
continuity remains required, and this slice also fixes the backend false
`tool_stalled_timeout` result and folds redundant transcript cards by work
unit.

## Problem

Long B2D / Join workflows are one user-visible unit of work, but the transcript
currently exposes the orchestration mechanics as many separate cards. Session
2075 demonstrated the amplification:

- 14 child rows, 13 visible;
- 25 durable delegation runs;
- at least 114 `get_delegation_status` calls;
- at least 46 continuation turns, including 36 checkpoint wakes.

Two independent problems compound:

1. A continuation worker normally drops its `TransferredWait` when it finishes
   waking the parent. Deregistration then closes the watch sender without a
   cancel cause. The listener treats that cause-less closure as
   `CancelCause::AutoTimeout`, returning `canceled / tool_stalled_timeout` even
   though the Broker still reports the real task as running, completed, or
   waiting for input.
2. The frontend only merges adjacent status polls. Interleaved text,
   continuation calls, checkpoint turns, and non-adjacent polls therefore
   produce repeated cards for the same logical work unit.

The global tool watchdog was disabled in the observed failure, so changing its
timeout would not address the root cause.

## Goals

- Never convert a cause-less continuation wait release into an automatic
  timeout.
- Preserve explicit `AutoTimeout` and `UserStop` cancellation semantics.
- Render one canonical inline delegation card per work unit across initial
  dispatch, continuation, replacement, and mapped status polls.
- Preserve all raw transcript events and durable run rows for audit and
  diagnostics.
- Improve existing history, including session 2075, on reload without a schema
  migration or database rewrite.
- Keep elapsed time, tool counts, edit rollups, and generating chrome continuous
  while a work unit remains sticky-active.
- Suppress the exact Codex `Conversation interrupted` assistant marker in
  delegated child sessions while leaving standalone sessions unchanged.

## Non-Goals

- Replacing Broker Join ownership or the continuation coordinator protocol.
- Removing the 600-second continuation checkpoint.
- Coalescing or deleting persisted tool calls.
- Hiding status calls that cannot be mapped to a known work unit.
- Inferring tool counts that were never observed.
- Changing the semantics of a real watchdog timeout or explicit user stop.

## Selected Approach

Use a layered minimal-contract fix:

```text
Continuation coordinator
    | explicit cause                 | sender closes without cause
    v                                v
cancel report                 Broker snapshot (non-blocking)
    |                                |
    +---------------+----------------+
                    v
             raw transcript events
                    |
                    v
       pure work-unit transcript projection
                    |
        +-----------+------------+
        |                        |
 canonical delegation card   unmapped status cards
        |
 sticky runtime aggregation
```

The backend change is intentionally narrow. The frontend projection is pure and
display-only, so live and historical data use the same deterministic rules.

## Backend Release Contract

### Current defect

`ContinuationJoin` transfers wait ownership to the continuation coordinator.
After suspension, the listener waits on the transferred watch receiver. A
normal worker exit drops `TransferredWait`, which deregisters the wait and drops
the sender without writing a `CancelCause`. The listener currently applies
`unwrap_or(AutoTimeout)` after the channel closes.

### Required behavior

The listener distinguishes two outcomes:

1. `cancel_cause_of(cancel_rx) == Some(cause)`: return the existing synthetic
   cancel reports. `AutoTimeout` remains `tool_stalled_timeout`; `UserStop`
   remains `user_cancelled`.
2. The receiver closes and no cause is present: deregister idempotently, then
   call `broker.get_tasks_status(..., StatusWait::Snapshot)` for the canonical
   task ids. Return those reports in a Join-shaped batch with
   `wake_reason: unavailable` and no fabricated error.

The snapshot is non-blocking and parent-scoped. It can return running,
completed, failed, canceled, unknown, stalled, or waiting-input state exactly as
the Broker currently sees it. The listener does not infer why the coordinator
released ownership.

Explicit-cause branches before suspension keep their current behavior. This
change does not add a second coordinator-to-listener result channel and does not
alter worker lifetime.

### Failure handling

- Deregistration remains idempotent and best-effort, as today.
- A closed sender with no cause is a release signal, never a cancellation
  signal.
- Snapshot lookup already degrades to parent-scoped `unknown`; it must not be
  replaced with a timeout report.

## Work-Unit Identity

The frontend derives a stable key using the first available identity:

1. explicit `work_unit_key` parsed from the dispatch tool input;
2. `(parentConversationId, childConversationId)` when both are known;
3. continuation linkage through `continued_from_task_id`, target `task_id`, or
   replacement linkage to a previously indexed run;
4. the run's own `task_id` as a single-run fallback;
5. the parent tool-use id only when no durable identity exists.

Every discovered identity is indexed to the same internal unit. Exact
`work_unit_key` wins when identities later converge. Parent conversation id is
part of every fallback namespace, so two parents cannot merge accidentally.

`work_unit_key` is read from agent-facing tool input only. It is not added to
the redacted workflow graph DTO.

## Transcript Projection

Add a pure projector over the session's adapted render items. It runs after
normal tool adaptation and assistant-turn merging but before rows are rendered.
It performs two passes:

1. Collect every `delegate_to_agent` and `continue_delegation` source, parse its
   identities, and build work units independent of transcript order.
2. Attribute `get_delegation_status` reports and requested task ids to those
   units, then rewrite display parts.

For each unit:

- the first dispatch position becomes the canonical card position;
- the card receives all correlated run sources in chronological order;
- later dispatch / continuation cards for the same unit are removed from the
  display projection;
- mapped status rows are folded into the unit and no longer render separately;
- assistant text, reasoning, unrelated tools, user turns, and raw source turns
  retain their original order;
- an entirely unmapped status call remains visible;
- for a mixed batch, mapped task rows fold into their units while unmapped rows
  remain in a residual status group.

The projector never mutates persisted `MessageTurn` data. It returns new render
items only for groups that change, preserving existing memoization for all
unaffected history.

### Canonical card source

The card is positioned at the first run but uses the newest correlated run as
its live binding and lifecycle source. Earlier sources remain attached as
read-only aggregation inputs. This lets the newest `parentToolUseId` subscribe
to live state without remounting the visible card at every continuation.

The sub-agent overlay consumes the same work-unit grouping helper, so its row
count and identity agree with the inline transcript.

## Sticky Runtime

For each projected work unit, derive runtime state from all unique run task ids:

- `anchorStartedAt`: earliest valid observed start;
- `toolCallCount`: sum of the maximum observed `tool_call_count` for each run;
- edit counts: the same per-run peak fold, summed across runs;
- touched files: stable union by path, retaining the latest observed detail;
- current lifecycle: newest authoritative run/binding/projection state.

Repeated observations of one task id use a maximum, never a sum. Invalid or
missing observations are omitted rather than converted to zero.

### Sticky phase

A work unit is `active_sticky` while the current run is running or while a
recoverable orchestration transition is awaiting continuation. Recoverable
intermediate codes include `parent_turn_failed`, `join_abandoned`, and
`parent_disconnected`; `parent_canceled` is recoverable only when it was not an
explicit delegation stop.

While sticky-active:

- lifecycle chrome renders as running/generating;
- elapsed time advances from `anchorStartedAt`;
- the last observed tool and edit totals remain visible through re-seed gaps;
- the operational line is `Generating | elapsed | N tools | edits`, omitting
  unavailable segments and using existing localized labels.

True completion, business failure, and explicit user cancellation release the
generating chrome. A 15-minute frontend orphan guard also releases a recoverable
intermediate state when there is no live binding, continuation/replacement
evidence, or newer observation. The guard remains longer than the 600-second
checkpoint.

A later legal continuation of the same unit re-enters `active_sticky` without
resetting the original elapsed anchor or prior per-run peaks.

## Delegated Child Interrupt Marker

The conversation surface passes whether the displayed conversation has a
non-null `parent_id`. For delegated children only, a pure display filter removes
an assistant text part whose trimmed content normalizes exactly to
`Conversation interrupted` with optional surrounding Markdown emphasis.

The filter does not remove partial matches or messages containing additional
content. Standalone conversations are unchanged. Filtering at display
projection preserves raw history and makes live and historical rendering
converge without rewriting stored transcript parts.

## Testing

### Rust

- A cause-less post-suspension sender close returns Broker snapshot reports and
  never `tool_stalled_timeout`.
- Cause-less release preserves running, completed, and waiting-input snapshots.
- Explicit `AutoTimeout` still produces `tool_stalled_timeout`.
- Explicit `UserStop` still produces `user_cancelled`.
- Parent scoping and unknown fallback remain intact.

### Frontend pure projection

- A 2075-like sequence with initial dispatch, many non-adjacent polls,
  interleaved assistant text, checkpoints, and continuations renders one card
  per work unit.
- Raw input arrays are not mutated.
- Two parallel work units remain separate.
- Continuation and replacement task-id links join the correct unit.
- Unmapped polls remain visible; mixed polls retain only their unmapped rows.
- Live and historical adapted inputs produce the same projection.

### Sticky model and UI

- Elapsed time and per-run peak tool totals remain continuous across task-id
  changes and re-seed gaps.
- Recoverable orchestration cancellation stays generating.
- Completed, business-failed, user-canceled, and orphaned units become terminal.
- The operational line includes the localized generating prefix.
- Inline and overlay grouping agree.

### Interrupt marker

- Exact delegated-child marker is hidden in live and historical rendering.
- Partial/additional text remains.
- The exact marker remains visible in standalone conversations.

## Acceptance Criteria

- Reloading session 2075 shows one canonical card per correlated work unit
  instead of repeated dispatch, continuation, and mapped status cards.
- Existing database rows and transcript events are unchanged.
- A normal continuation wake never yields a synthetic
  `tool_stalled_timeout`.
- Real automatic timeout and explicit user stop keep their existing wire codes.
- Sticky-active cards do not flash terminal styling or reset elapsed/tool
  totals across continuation and checkpoint re-entry.
- Delegated child transcripts do not display the exact interruption marker;
  standalone Codex transcripts still do.
- Frontend typecheck, lint, targeted tests, full Vitest suite, and relevant Rust
  tests pass.

## Rollout and Compatibility

No schema migration is required. The backend behavior changes only for a watch
channel closing without a cancel cause. The frontend derives its projection
from fields already present in tool input, output, metadata, and durable run
snapshots, so old sessions improve immediately when reopened.

If a historical run lacks enough identity to correlate safely, it remains a
separate card. The design prefers a visible duplicate over merging unrelated
work.
