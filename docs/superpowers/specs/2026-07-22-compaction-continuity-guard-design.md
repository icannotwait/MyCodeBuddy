# Context Compaction Continuity Guard Design

Date: 2026-07-22

Status: Approved in conversation on 2026-07-22.

## Relationship to Existing Designs

This specification extends:

- docs/superpowers/specs/2026-07-17-event-driven-delegation-join-design.md
- docs/superpowers/specs/2026-07-19-delegation-continuation-design.md
- docs/superpowers/specs/2026-07-14-grok-compact-slash-acp-surfacing-design.md

The existing designs remain authoritative for Broker task ownership, child
lifecycle persistence, explicit Join, suspension fencing, hidden continuation
prompts, parent-decision attention, user cancellation, and connection-loss
cleanup.

This document adds a second continuation trigger: an automatically armed,
durable Join when a parent agent completes context compaction while direct
Codeg delegation work is active. It does not change explicit Join semantics.

## Problem

An asynchronous Codeg delegation can outlive the model context that initiated
it. After an agent compacts its context, the model may no longer preserve the
tool-result route that tells it to collect the child result. If the parent then
ends its turn normally, Codeg maps that end to JoinAbandoned and cancels the
still-running child.

The confirmed Grok reproduction was conversation 800:

1. The parent invoked delegate_to_agent.
2. Grok started automatic context compaction at 241,052 tokens.
3. Grok completed compaction at 8,935 tokens.
4. The delegation result reached the model after compaction.
5. The resumed model continued its original work rather than collecting the
   task result.
6. The parent ended normally and the child was canceled with join_abandoned.

The compaction summary itself retained the pending-task information. The bug is
therefore a timing and tool-result-route failure, not merely a missing-summary
problem.

This is not Grok-specific. Any parent agent can exhibit the same failure if it
can compact context while a Codeg child is active. ACP does not define a
standard context-compaction session update, so Codeg must normalize only
provider signals that are demonstrably reliable.

## Goals

- Preserve direct Codeg children across a verified parent context-compaction
  boundary.
- Suspend the current parent turn before it can continue along a stale
  post-compaction route.
- Resume only from a child terminal snapshot or a child parent-decision
  request.
- Cover a child that is in Broker registration when compaction occurs.
- Keep ordinary explicit Join behavior, including its model-facing
  240-second checkpoint, unchanged.
- Use only structured or provider-private signals that Codeg can identify
  reliably.
- Preserve fail-closed behavior when persistence, suspension, the parent
  connection, or the Codeg process fails.

## Non-Goals

- Inferring compaction from rendered text, summary wording, or a context-usage
  percentage.
- Claiming every ACP provider is protected before it exposes a reliable live
  compaction boundary.
- Changing provider context-window limits or provider compaction policies.
- Resuming the canceled ACP prompt itself. Recovery always starts a new hidden
  continuation turn.
- Transferring children to a new parent ACP connection after disconnect or
  process restart.
- Changing the public explicit Join contract, status-call contract, or child
  lifecycle state machine.
- Applying this guard to platform-native subagents not owned by
  DelegationBroker.

## Evidence and Initial Provider Scope

ACP SessionUpdate has no standard context-compaction variant. The initial
implementation therefore enables the guard only for these normalized sources:

| Source | Reliable live signal | Initial action |
| --- | --- | --- |
| Grok | private auto_compact_completed notification | enable |
| Codex | thread/compacted or completed contextCompaction item | enable after metadata preservation |
| Claude Code | transcript continuation evidence, but no approved live guard signal | do not enable yet |
| Pi | persisted compaction records, but no approved live guard signal | do not enable yet |
| Gemini, OpenCode, Cline, Hermes, CodeBuddy, Kimi, Cursor | no verified live signal in Codeg | do not enable yet |

The first two sources normalize to the same internal event. Later providers
must add a structured adapter and fixtures before they can opt in. A provider
must never opt in through text matching.

## Selected Approach

Introduce an internal event:

~~~text
ContextContinuityBoundary::CompactionCompleted
~~~

The event is producer-neutral. A source adapter reports the boundary; it does
not know about delegation state, persistence, cancellation, or resumption.

The existing DelegationContinuationCoordinator receives a new
compaction-guard entry point. It owns durable state and reuses the existing
parent suspension, waiting projection, wake, hidden-prompt, cleanup, and
failure paths.

The continuation record gains a trigger discriminator:

~~~text
explicit_join
compaction_guard
~~~

The discriminator is durable. It controls checkpoint behavior and prevents
the compaction path from inheriting explicit Join's immediate-ready shortcut.

## Architecture

### Context-continuity normalization

The connection layer converts reliable provider events into
ContextContinuityBoundary::CompactionCompleted before it discards provider
identity:

- Grok emits the boundary only for a recognized
  auto_compact_completed private notification. Existing lifecycle text and
  usage rendering remain unchanged.
- The vendored Codex ACP adapter preserves compaction identity in the
  agent-message update metadata. The Rust connection layer reads that
  metadata, emits the same boundary, and still renders the existing compacted
  text.

A per-parent-turn latch deduplicates the boundary. A repeated signal from the
same provider, or two equivalent Codex signals for the same compaction, cannot
create a second continuation or send a second cancellation request. The latch
resets only when a new parent turn starts.

### Broker capture scope

DelegationBroker exposes a compaction capture operation scoped to one parent
connection and conversation. Under its existing pending-state synchronization,
it:

1. snapshots direct running and settling tasks;
2. records every already-present in-flight start_delegation setup; and
3. returns a capture handle that reports each recorded setup once it acquires a
   task ID or reaches its terminal setup result.

Only setups present at the boundary belong to the capture. A delegation that
starts after the boundary is not added to the guard. Pending ACP tool-call
identity alone is not a child and is not sufficient to arm a guard.

This is required because task IDs are not all available at the instant the
Broker marks a setup in flight. The capture follows that setup through its
registration result rather than taking a one-time running-task snapshot.

### Continuation storage

The continuation table adds a non-null trigger column constrained to:

~~~text
explicit_join | compaction_guard
~~~

Existing rows migrate to explicit_join.

The existing task_ids_json remains the authoritative task set. A
compaction_guard may extend it with a captured in-flight task ID only while
the record is Arming. The extension is a compare-and-swap update that
deduplicates IDs. Once capture closes, task IDs are immutable. The in-memory
capture handle is intentionally not persisted because Codeg fails closed on
connection or process loss.

The one-active-continuation-per-conversation constraint remains unchanged.

### Coordinator responsibilities

The coordinator adds a compaction-specific arming operation. It:

1. obtains a Broker capture;
2. returns without a row when the capture contains neither a direct task nor
   an in-flight setup;
3. snapshots and validates the live parent identity;
4. persists an Arming continuation with trigger compaction_guard;
5. appends captured setup task IDs while the capture remains open;
6. requests fenced parent suspension;
7. after suspension is accepted, waits for every setup captured at the
   boundary to reach task-ID registration or a terminal setup result;
8. closes and freezes the capture; and
9. waits for an actionable Broker condition.

Unlike explicit Join, compaction_guard never returns an immediate result merely
because a captured child becomes terminal during arming. It must complete the
suspension first. The hidden continuation prompt can then immediately carry
that terminal result.

## Compaction Guard Data Flow

~~~text
provider compaction-completed signal
  -> normalize ContextContinuityBoundary
  -> reject duplicate or inactive parent turn
  -> Broker opens parent-scoped capture
  -> coordinator persists compaction_guard Arming row
  -> captured in-flight setups append task IDs by CAS
  -> connection installs suspension lease and cancels active parent prompt
  -> suspension acknowledgement
  -> boundary-era captured setups resolve
  -> capture closes and task set freezes
  -> Broker reports all-terminal or parent attention
  -> coordinator admits one hidden continuation prompt
~~~

The connection control lane gives suspension priority over ordinary commands.
After the lease is installed, inbound activity belongs to draining the old
turn and cannot be treated as the parent model's normal post-compaction
continuation.

The guard waits only on direct tasks owned by the parent connection. Child
descendants retain their existing Broker ownership behavior.

## State and Wake Semantics

The existing continuation states remain:

~~~text
Arming -> Waiting -> WakePending -> Resuming -> Completed
                       \-> Cancelled | Failed
~~~

The trigger changes only the following behavior:

| Condition | explicit_join | compaction_guard |
| --- | --- | --- |
| Initial task snapshot already actionable | return ordinary immediate result | persist and suspend if a boundary capture exists |
| Child terminal during arming | may complete through existing Join flow | retain result, complete suspension, then wake |
| All terminal | hidden prompt | hidden prompt |
| Parent attention requested | hidden prompt | hidden prompt |
| Unavailable | hidden prompt | fail guard and publish diagnostic |
| 240-second checkpoint | hidden prompt | internal liveness check only |

For compaction_guard, a checkpoint re-evaluates Broker state, refreshes the
next internal deadline and waiting projection, and records metrics. It does
not claim WakePending and does not inject a model-visible prompt. This avoids
reintroducing the very stale-route behavior the guard exists to prevent.

Likewise, a compaction_guard does not resume the model for an unavailable
snapshot. It fails closed and publishes a diagnostic. The only model-visible
recovery paths are an all-terminal snapshot and an authenticated
parent-decision request.

## Failure Handling and Priority

### Priority order

1. User cancellation and parent connection disconnect are authoritative.
2. A valid suspension lease owns the current parent turn.
3. A claimed child terminal or parent-attention wake owns resumption.
4. A checkpoint is liveness-only for compaction_guard.
5. Duplicate provider signals are ignored.

### Persistence failure before suspension

If capture or continuation persistence fails before a suspension request is
dispatched, Codeg does not cancel the parent turn. It publishes an explicit
compaction-guard arm diagnostic and leaves the original turn intact. A
non-durable suspension would strand the parent without a safe recovery record.

### Suspension dispatch failure or drain timeout

If a persisted guard cannot suspend the exact parent turn, the coordinator
marks it Failed, clears the waiting projection, and follows the existing
parent-failure cleanup path for direct children. It emits an explicit failure
event rather than leaving a child running without an owner.

### Child terminal during arming

A terminal child is retained in the frozen task set. It never converts the
compaction guard into an ordinary immediate result and never skips the
suspension acknowledgement.

### User cancellation and disconnect

User cancellation cancels the continuation and children through the existing
parent-cancel path. Parent disconnect cancels workers and descendants, then
marks the durable record parent_connection_lost. Neither case emits a hidden
continuation prompt.

### Hidden prompt delivery failure

If the coordinator has claimed a wake but cannot admit the hidden prompt, it
marks the continuation Failed and publishes prompt_delivery_failed. Child
tasks are already terminal or otherwise resolved by the wake condition, so it
does not invent a second child-cancellation path.

### Unavailable task snapshot

An unavailable result for compaction_guard is a failure, not a wake. The
coordinator marks the record Failed and publishes the unavailable diagnostic
without injecting a parent prompt. This preserves the contract that an
automatically compacted parent resumes only for terminal child results or a
parent-decision request.

### Restart

On Codeg restart, active continuations fail closed exactly as the existing
design requires. Codeg does not attempt to restore the old parent connection
or transfer child ownership across process boundaries.

## Compatibility

- Existing explicit Join records and API behavior remain explicit_join.
- Existing continuation UI consumes the same waiting projection and needs no
  new user-facing control.
- Existing compaction lifecycle text remains visible.
- Unsupported providers continue their current behavior until a reliable
  structured signal and regression fixture are added.
- No prompt-language instruction is a correctness dependency.

## Observability

Add structured logs and metrics for:

- normalized compaction boundary by provider;
- duplicate boundary suppression;
- compaction guard armed, suspended, capture-expanded, frozen, resumed,
  canceled, and failed;
- time from boundary to suspension acknowledgement;
- time spent in internal liveness checks;
- terminal wake source and failure code.

Logs must contain provider and opaque IDs only. They must not include delegated
task text, compacted summaries, hidden prompt payloads, or model output.

## Testing Strategy

### Normalization tests

- Grok auto_compact_completed produces the boundary.
- Grok started, failed, cancelled, malformed, and unrelated private
  notifications do not produce it.
- Codex thread-compacted and completed context-compaction metadata produce the
  boundary.
- Codex compaction text still renders normally.
- Plain text that happens to say context compacted never triggers the guard.

### Broker capture tests

- A capture includes running and settling direct tasks.
- A capture tracks an already in-flight setup through task-ID registration.
- An in-flight child that ends during setup is retained as a terminal result.
- A task belonging to another parent, a descendant, or a setup that begins
  after the boundary is excluded.
- Task IDs append exactly once and cannot change after capture freeze.

### Coordinator tests

- The session-800 ordering is reproduced with a deterministic test clock and
  fake parent port: compaction, child terminal, suspension, one hidden prompt,
  and no JoinAbandoned cancellation.
- A child that becomes terminal during arming still causes parent suspension
  acknowledgement before wake.
- Duplicate signals create one row and one suspension request.
- A compaction guard checkpoint never admits a prompt; explicit Join checkpoint
  behavior remains unchanged.
- Persistence failure issues no suspension request.
- Suspension dispatch failure and drain timeout fail and clean up exactly once.
- User cancel, parent disconnect, attention wake, unavailable failure,
  prompt-delivery failure, and concurrent terminal events preserve their
  existing priority rules.

### Connection-loop tests

- Grok private notifications and Codex metadata converge at the same guard
  entry point.
- The suspension control is processed before ordinary command handling after
  the boundary.
- Old-turn terminal messages drain under the lease and cannot produce a normal
  parent-end child cancellation.

### Migration tests

- Upgrade creates the trigger column and defaults legacy rows to explicit_join.
- Invalid trigger values are rejected.
- Task-set extension is conditional on trigger compaction_guard and state
  Arming.

## Acceptance Criteria

- A supported parent agent cannot cancel an active direct Codeg child as
  join_abandoned solely because context compaction made it lose the Join route.
- The full terminal result snapshot reaches the parent in exactly one hidden
  continuation turn.
- A child that races registration at the compaction boundary is included.
- Repeated compaction signals do not create duplicate guards, prompt injections,
  or cancellation requests.
- Explicit Join behavior and its checkpoint semantics remain unchanged.
- User cancellation, disconnect, suspension failure, and restart remain
  fail-closed and leave no orphaned child.
- Only Grok and Codex are enabled initially; no provider is enabled from
  rendered text heuristics.
