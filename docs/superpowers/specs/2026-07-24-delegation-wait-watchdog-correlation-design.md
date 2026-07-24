# Delegation Wait Watchdog Correlation and RunStore Convergence Design

## Status

Approved in conversation on 2026-07-24.

This specification is a corrective addendum to
`2026-07-22-tool-execution-watchdog-design.md`. That design already requires
real delegated-child activity to renew the exact foreground parent lease that
is waiting for the child. This document closes an implementation and test
coverage gap in that contract. It also specifies an independent `RunStore`
ownership correction exposed while diagnosing the same class of apparent
session hangs.

## Incidents and Evidence

### Active child falsely times out its parent wait

Conversation 1570 (`Windows taskbar awaiting reply badge design worktree`)
started Task 4 at `2026-07-24T07:18:05Z`. The parent then blocked in
`get_delegation_status` at `07:18:10Z`.

The durable task edge retained the tool id of the earlier
`delegate_to_agent` call, ending in `-168`. The live foreground wait used the
next tool id, ending in `-169`. Child observation events renewed `-168`, while
the execution watchdog was timing `-169`.

The child remained active throughout the wait. It started or completed tools
at `07:29:06Z`, `07:34:36Z`, `07:36:53Z`, and later. Nevertheless, the parent
turn was cancelled at `07:38:11Z`, approximately 1,201 seconds after the wait
started. The child was not stalled and completed normally at `07:40:44Z`
after 39 tool calls.

The exact 600-second warning plus 600-second grace interval, combined with
the distinct launch and wait tool ids, proves that semantic progress reached
the wrong lease.

### RunStore gate waits forever

The test `parent_cancel_while_settling_preserves_completion_side_effects`
installs a settlement gate on one `RunStore`. `DbDelegationTaskStore::settle`
creates a different temporary `RunStore`, so the gated instance is never
entered and the test waits forever on `entered_rx.await`.

Production assembly also constructs the task store and Broker run store
separately. Production does not install test gates, but the duplicate
ownership obscures which `RunStore` is authoritative and allows tests to
exercise a different instance from the one used by settlement.

## Goals

- Renew a live foreground delegation wait only from semantic activity by a
  task explicitly included in that wait.
- Support both singleton and multi-task indefinite waits.
- Preserve the existing 600-second semantic-silence warning and full
  600-second grace period.
- Preserve exact connection incarnation and turn-generation fencing.
- Make a normally correlated automatic timeout cancel the request-scoped wait
  without invoking Broker child cancellation.
- Clear an actionable Warning or Grace projection when matching child activity
  resumes.
- Use one shared `Arc<RunStore>` for Broker and database task-store operations.
- Bound test-only gates and joins so a fixture error fails quickly instead of
  hanging the complete Rust test process.

## Non-Goals

- Adding an absolute wall-clock timeout to foreground terminal commands.
- Treating `TaskStatus::Running` or process existence as semantic progress.
- Pausing the watchdog for the entire lifetime of a delegated task.
- Changing the default watchdog settings or the delegation soft-watchdog
  threshold.
- Persisting live wait registrations in SQLite.
- Cancelling a child task when a parent status wait times out.
- Changing MCP schemas, delegation card rendering, or frontend watchdog UI.
- Refactoring unrelated delegation settlement or continuation behavior.

## Selected Approach

### Request-scoped exact wait registration

Extend the existing in-memory wait-cancel registration with the normalized set
of task ids that the request is waiting for. A live registration has these
logical fields:

```text
wait_id
connection_id
connection_incarnation
turn_generation
parent_conversation_id
wait_tool_call_id
task_ids
owner
cancel_sender
settled
```

`wait_tool_call_id` is the current `get_delegation_status` tool id, represented
by `WaitStamp.parent_tool_use_id`. It is not the historical tool id that
started a child.

`task_ids` has set semantics: trimmed, de-duplicated ids only. The companion
and listener already validate ownership before parking; the registry stores
only the listener-validated ids. Registrations remain process-local and are
removed on normal completion, cancellation, peer close, or abandoned wait.

The registry exposes a read-only exact-match operation. A child activity event
matches a wait only when all of the following are true:

- the registration is live and not settled;
- the task id is a member of `task_ids`;
- connection id and connection incarnation match the current parent;
- turn generation matches the current parent turn; and
- the registration has a concrete wait tool call id.

The lookup returns immutable progress targets. It never returns cancellation
senders and never guesses a tool id.

### Which waits register

Every indefinite foreground status wait registers before it parks:

- legacy terminal-only `wait_ms: 0`; and
- coordination Join with `wait_ms: 0` and
  `return_when: all_terminal_or_attention`.

This applies to both the continuation-capable Join path and the compatibility
Join path used when `delegation_continuation_v1` is unavailable.

Both singleton and multi-task waits bind their exact tool lease to
`CancellationCapability::DelegationWait { wait_id }`.

Immediate snapshots do not register. Positive legacy waits remain bounded to
at most 60 seconds and do not need an execution-watchdog wait registration.

Registration and capability binding happen before the request blocks. If the
host cannot provide a reliable current wait tool id, the listener does not
invent one. The status request still runs, and the existing MCP/turn fallback
remains available for cancellation.

### Semantic progress flow

The child observation source continues to derive activity from the child
connection's `SessionState.last_agent_activity_at`. New agent messages,
thoughts, plan updates, tool starts, and tool updates advance this timestamp.
Repeated metadata and keepalive traffic do not.

The progress flow is:

```text
child ACP semantic activity
  -> child SessionState.last_agent_activity_at
  -> delegation observation snapshot with changed activity timestamp
  -> verify durable/live parent_tool_use_id -> task_id launch edge
  -> exact-match active wait registrations containing task_id
  -> validate current parent connection incarnation and turn generation
  -> record DelegationActivity on each matching wait_tool_call_id lease
```

The historical launch edge remains the authority proving that the child
belongs to this parent. It is not used as the destination lease for a later
status wait.

If the original launch tool still has a live foreground lease, activity may
renew that exact lease too. A completed launch tool must not be re-registered
merely because its delegation card remains active.

The progress fingerprint uses the child activity timestamp, so duplicate
observation snapshots do not renew. A newer timestamp is semantic progress
even when the observation enum remains `Active`.

### Warning and grace recovery

The existing lease state machine remains authoritative. Matching progress:

- updates `last_progress_at` while Running;
- returns Warning or Grace to Running;
- increments the lease version; and
- emits one `Cleared` projection when an actionable warning is demoted.

Activity from a sibling tool, a child outside the registered task set, a stale
turn, or a stale connection incarnation cannot clear or renew the lease.

### Cancellation scope

When an indefinite status-wait lease expires, the execution watchdog uses its
`DelegationWait` capability to wake only the matching request-scoped wait. The
wait returns `tool_stalled_timeout`, settles its foreground tool lifecycle,
and deregisters its handle.

The child task remains Running. A parent can query it later, and normal Broker
completion still owns the child result. If the specific wait cancel does not
make the parent turn converge, the existing generation-guarded `CancelTurn`
path also avoids the user-cancel parent-tree cascade, so acknowledged children
survive it.

The existing incarnation-guarded connection disconnect remains the final
fallback only if both specific wait cancellation and `CancelTurn` cannot make
the parent loop converge. Connection teardown may reclaim the parent-owned
delegation tree. This is an infrastructure-failure fallback, not the normal
wait-timeout contract. Correctly registered wait cancellation must be tested
to converge before this fallback and must record no Broker child cancel or
connection disconnect.

## Race and Failure Semantics

### Activity versus normal wait completion

If activity wins first, it renews the live wait lease and normal completion
then removes the lease. If completion wins first, deregistration or lease
completion makes the late activity a no-op.

### Activity versus warning scan

The existing registry lock and lease version decide the winner. Progress that
wins against Warning or Grace emits `Cleared`. A stale scan action cannot claim
the newer lease version.

### Wait cancellation versus child completion

The wait-cancel registry settles only the wait handle. Child completion may
wake the same request concurrently. Existing exact stamp and deregistration
logic select one request outcome; neither path rewrites an already terminal
child result.

### Continuation ownership transfer

Canonical Join may transfer a registration from the listener to the
continuation coordinator. The task-id set and exact wait stamp transfer
unchanged. Once the foreground parent turn is suspended and its lease is gone,
later child progress finds no live lease and is harmless. Existing continuation
wake behavior remains authoritative.

### Missing or stale correlation

A missing wait tool id, missing active parent turn, stale incarnation, stale
generation, or absent lease produces no renewal. Registration or capability
binding failure emits one structured debug record per wait with a stable reason
label. It must not include task prompts, tool arguments, companion tokens, or
environment values. Per-activity unmatched events do not log, preventing an
unbounded noisy path.

## Shared RunStore Ownership

`DbDelegationTaskStore` will own `Arc<RunStore>` rather than constructing a new
store in `load`, `settle`, or related operations.

It keeps a compatibility constructor:

```text
DbDelegationTaskStore::new(db)
```

which creates one internal shared store, and adds an explicit production/test
constructor:

```text
DbDelegationTaskStore::from_run_store(Arc<RunStore>)
```

Application assembly creates one `Arc<RunStore>` and passes clones to the
Broker and `DbDelegationTaskStore`. Broker test helpers do the same. All task
store operations use the held instance.

This change does not add process-global state and does not change the durable
schema. It makes the already authoritative store identity explicit and lets
test gates observe the actual settlement path.

## Test Fail-Fast Boundaries

Test synchronization must never introduce an unbounded process-wide hang.

- Gate-entry receivers use the repository's bounded async test duration.
- Spawned completion tasks are joined through the same bounded duration.
- A test-only RunStore gate waits no longer than five seconds for release and
  returns a bounded test error when release is absent.
- Timeout failures identify the missing phase, task id where safe, and expected
  gate transition.
- No production terminal, Broker, or database operation receives these test
  deadlines.

## Testing

### Wait registration unit tests

- Singleton and multi-task registrations return an exact match for a member.
- A task outside the set does not match.
- Settled and deregistered waits do not match.
- Old turn generations and connection incarnations do not match.
- Owner transfer preserves task membership and the wait stamp.
- Duplicate task ids do not create duplicate renewal targets.

### Watchdog attribution tests

- Use distinct launch and wait tool ids. Child activity renews the wait lease,
  not a completed launch lease or sibling lease.
- Activity at `t+590s` prevents a warning at `t+600s`.
- A new 600-second silence window starts from the last matching activity.
- Activity while in Grace returns the wait lease to Running and emits exactly
  one `Cleared` projection.
- An unrelated active child cannot renew the wait.
- A stale wait registration cannot renew a reused tool id in a newer turn.

### Listener and cancellation integration tests

- Singleton indefinite waits bind `DelegationWait`, matching multi-task waits.
- Legacy terminal-only, compatibility Join, and continuation-capable Join
  waits register before parking.
- Automatic timeout wakes only the wait, leaves the child task Running, and
  does not invoke connection-disconnect fallback in the healthy path.
- Peer close and normal completion deregister before later child activity.
- A continuation ownership transfer retains exact task membership.

### Conversation 1570 regression shape

Use a controlled clock rather than a real 20-minute sleep:

- start a child from launch tool A;
- park the parent on status tool B for that child;
- publish multiple child activity timestamps beyond the original 1,200-second
  total duration;
- scan after every deadline and assert that B remains Running while activity
  remains newer than 600 seconds;
- stop publishing activity and assert Warning then wait-only timeout at the
  normal boundaries; and
- assert that the child can still complete afterward.

### RunStore regression tests

- The settlement gate and `DbDelegationTaskStore::settle` share the same
  `Arc<RunStore>`.
- `parent_cancel_while_settling_preserves_completion_side_effects` reaches and
  releases the gate within the test bound.
- Relevant parent-cancel, settle, and RunStore gate tests remain green.
- A deliberately unreleased gate returns its asserted test-only timeout
  outcome within five seconds.

## Alternatives Rejected

### Pause every wait while any child is Running

Rejected because a status value is not semantic progress. A genuinely stalled
child would disable the execution watchdog indefinitely.

### Periodic synthetic heartbeat

Rejected because it fabricates progress and hides dead children, broken event
bridges, and stale wait registrations.

### Lower the global watchdog duration

Rejected as a correctness fix. It would make the false timeout happen sooner
and increase failures for valid long-running work.

### Renew the historical launch tool id only

Rejected because foreground waiting occurs in a later tool call with a distinct
lease. This is the defect observed in conversation 1570.

### Add only async test timeouts

Rejected as the RunStore fix. Timeouts would turn the permanent hang into a
failure but leave the settlement gate attached to the wrong store instance.

### Use a gated task-store wrapper only in the test

Rejected because it preserves ambiguous production ownership between the task
store and Broker run store.

## Acceptance Criteria

- A foreground status wait remains live beyond 20 minutes when at least one
  explicitly awaited child produces semantic activity within each configured
  600-second window.
- The same wait warns after 600 seconds of real child silence and receives a
  full 600-second grace period before cancellation.
- Only tasks listed in the active wait can renew it.
- Singleton and multi-task indefinite waits use request-scoped
  `DelegationWait` cancellation.
- A correctly registered wait timeout invokes neither Broker child cancel nor
  connection disconnect; the acknowledged child remains Running.
- Warning/Grace recovery emits `Cleared` and cannot be replayed as actionable
  after progress.
- Stale turns, incarnations, waits, and tool ids cannot mutate a newer lease.
- `DbDelegationTaskStore` and Broker use the same `Arc<RunStore>` in production
  and relevant tests.
- The previously hanging settlement test either completes normally or fails
  at a named bounded wait; it cannot hang the full Rust test process.
- No terminal absolute-runtime timeout, database migration, frontend change,
  or watchdog default change is introduced.
