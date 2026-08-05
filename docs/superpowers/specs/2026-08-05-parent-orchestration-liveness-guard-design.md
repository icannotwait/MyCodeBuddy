# Parent Orchestration Liveness Guard Design

## Status and Relationship to Existing Designs

Approved in the 2026-08-05 design discussion.

This is a companion design to
`2026-08-04-platform-generated-completion-evidence-design.md`, reviewed at
commit `1afa90de`. That design remains authoritative for completion protocol
v2 evidence, gate settlement, typed completion attention, final-delivery
validation, and workflow safety. This design adds the missing root-orchestration
liveness boundary after those facts have been persisted.

Where the earlier design says an adjudication event may wake the Parent, this
design makes that wake pass through the same durable liveness coordinator used
for ordinary workflow advancement. It does not create a second completion
protocol or a second gate authority.

The following existing designs remain in force:

- `2026-07-26-brainstorm-to-delivery-workflow-graph-design.md` for the durable
  manifest and graph;
- `2026-07-30-brainstorm-to-delivery-recovery-contract-hardening-design.md`
  for workflow recovery authorization and safety;
- `2026-08-01-continuation-mcp-release-design.md` for child-Join suspension;
  and
- `2026-08-04-platform-generated-completion-evidence-design.md` for all
  protocol-v2 semantic completion facts.

This design is authoritative only for protocol-v2 B2D root-turn liveness,
workflow checkpoint projection, and workflow-root prompt scheduling.

## Incident

Session `codeg://session/3023` stopped after Task 3 even though Tasks 4 through
12 remained.

The durable evidence showed:

- Task 3 and its review-card reruns completed normally;
- the Parent emitted `Tasks 4-12 remaining. Say continue`;
- the Parent then ended with a normal `end_turn` and
  `outcome = completed`;
- there was no `delegation_continuations` row;
- context use was approximately 75 percent; and
- there was no cancellation, compaction, connection failure, or other blocker.

The B2D Skill already required automatic next-task dispatch. The failure was
therefore not missing prompt guidance. The platform accepted a model's clean
turn boundary as if it were a valid workflow waiting point.

## Problem

Codeg currently has two narrower continuation mechanisms:

1. `delegation_continuations` suspends and resumes a Parent around an explicit
   child Join; and
2. completion protocol v2 can emit `completion_decision_resolved` after a user
   adjudicates durable attention.

Neither mechanism covers an ordinary gate transition where:

```text
current Task/gate becomes complete
  -> more manifest work is runnable
  -> Parent prints a progress checkpoint
  -> Parent returns clean end_turn
  -> no child Join and no adjudication event exists
  -> the platform waits for another user message forever
```

The model cannot be the authority for whether a durable workflow is finished,
blocked, waiting on children, or immediately runnable. A sentence such as
`Say continue` is prose, not durable attention.

## Executive Decision

Add a synchronous Parent Orchestration Liveness Guard at the shared root
`end_turn` finalization boundary.

For an applicable workflow, the Guard derives exactly one platform-owned
decision from current durable protocol-v2 state:

```text
Delivered | UserBlocked | WaitingForRuns | RunnableAction
```

The Guard persists that decision before ordinary turn finalization or
workflow-child teardown can occur.

- `Delivered` allows genuine workflow completion.
- `UserBlocked` allows a genuine user waiting point only when a current durable
  typed attention request proves it.
- `WaitingForRuns` parks orchestration, preserves the relevant children, and
  installs a durable event-driven re-evaluation point.
- `RunnableAction` records a visible workflow checkpoint and enqueues an
  immediate, idempotent hidden Parent wake.

The Guard never dispatches a child, settles a gate, chooses an outcome, or
edits Parent prose. A hidden Parent turn must reload durable workflow state and
invoke the existing authorized workflow operations with their normal CAS.

## Core Invariant

For a clean `end_turn` from an applicable protocol-v2 B2D root:

> An incomplete workflow may not become quiescent unless current durable state
> proves either a user blocker or a run that can still make progress. Every
> immediately runnable state must have a durable root wake before the turn is
> finalized.

Equivalently, the following state is forbidden after clean Parent
finalization:

```text
manifest incomplete
AND no current durable user attention
AND no progress-capable active run
AND no pending/admitted workflow root wake
```

Explicit user Stop and abnormal provider/connection termination are outside
this clean-end invariant and retain their existing semantics.

## Goals

- Prevent a protocol-v2 B2D workflow from silently stopping between Task or
  gate transitions.
- Make the liveness decision from the same platform evidence and scope
  validators used by admission, settlement, and projection.
- Preserve Parent output while distinguishing a turn checkpoint from workflow
  delivery.
- Keep active workflow children alive while the Parent is parked.
- Make root wake delivery durable, idempotent, restart-safe, and race-safe
  against user prompts.
- Detect a Parent that repeatedly wakes without changing the workflow frontier
  and stop an infinite self-wake loop.
- Reuse the connection manager's prompt arbitration primitives without
  misusing child-Join continuation state.
- Preserve desktop/server parity and expose a level-triggered UI state.

## Non-Goals

- Applying the Guard to completion protocol v1, `v2_shadow` workflows,
  observed-only graphs, or standalone delegations.
- Replacing the B2D workflow reducer, admission policy, evidence validator, or
  final-delivery artifact guard.
- Letting the Guard directly call `delegate_to_agent`,
  `settle_workflow_gate`, continuation, replacement, or recovery operations.
- Treating arbitrary Parent prose, TODO lists, plans, or claims of completion
  as liveness evidence.
- Reusing `delegation_continuations` for ordinary workflow advancement.
- Automatically recovering refusal, max-token, user cancellation, connection
  loss, or other abnormal turn outcomes.
- Retrospectively waking every historical incomplete workflow during
  migration.
- Generalizing this slice into a scheduler for non-B2D agents.

## Applicability

The Guard runs only when all of the following are true:

1. the terminal source is a provider clean `end_turn` with usable Parent
   output;
2. no child-Join suspension lease owns the turn;
3. the connection is the root conversation, not a delegation child;
4. `delegation_workflows.parent_conversation_id` matches that conversation;
5. `workflow_kind = brainstorm_to_delivery`;
6. `completion_protocol_version = 2`; and
7. an active durable manifest revision exists.

The unique `(parent_conversation_id, workflow_kind)` workflow index makes the
root lookup unambiguous.

The Guard does not run for `cancelled`, `refusal`, `max_tokens`,
`max_turn_requests`, `empty`, `unknown`, transport failure, disconnect, or an
explicit workflow termination/deletion. User Stop also cancels any queued
workflow-root wake through the same prompt-arbitration fence that cancels
continuation admission.

## Architecture

```text
provider clean end_turn
        |
        v
shared Parent finalization boundary
        |
        +-- not applicable -----------------> existing finalization
        |
        v
WorkflowLivenessResolver
        |
        +-- Delivered -------> persist delivered ------> ordinary completion
        |
        +-- UserBlocked -----> persist blocker --------> typed user attention
        |
        +-- WaitingForRuns --> persist parked state ---> child event recheck
        |
        +-- RunnableAction --> persist checkpoint + workflow_root_wake
                                                    |
                                                    v
                                      RootPromptAdmissionPort
                                                    |
                                                    v
                                      hidden Parent turn
                                                    |
                                                    v
                                      reload durable workflow state
```

The component boundaries are explicit:

| Component | Owns | Does not own |
| --- | --- | --- |
| `WorkflowLivenessResolver` | Classifying the current durable frontier | Dispatch, settlement, or prose interpretation |
| `WorkflowLivenessStore` | Decision CAS, wake generation, no-progress state, and checkpoint persistence | Prompt delivery |
| workflow outbox | At-least-once root-wake notification | Workflow truth |
| `WorkflowRootWakeCoordinator` | Retry, deduplication, and re-evaluation triggers | Child-Join suspension |
| `RootPromptAdmissionPort` | Prompt lock, root identity, turn generation, and hidden prompt enqueue | Workflow action selection |
| Parent hidden turn | Reloading state and invoking existing MCP workflow operations | Overriding platform evidence |

## Authoritative Decision Contract

The resolver returns a closed result:

```text
WorkflowLivenessDecisionV1
  Delivered {
    workflow_id,
    graph_revision,
    final_evidence_task_id,
    delivered_artifact_digest
  }
  UserBlocked {
    workflow_id,
    graph_revision,
    attention_refs[]
  }
  WaitingForRuns {
    workflow_id,
    graph_revision,
    progress_capable_runs[]
  }
  RunnableAction {
    workflow_id,
    graph_revision,
    action: WorkflowActionDescriptorV1,
    action_fingerprint
  }
```

All IDs and digests are platform-derived. The result contains no Parent prose,
child prose, model-provided task list, or model-provided status.

### Resolver Inputs

The resolver reads one transactionally consistent snapshot of:

- the workflow header and active immutable manifest;
- current node, gate-state, settlement, and run bindings;
- latest durable run state and protocol-v2 completion evidence;
- the shared bounded evidence/scope validator result;
- current typed attention with full kind-specific CAS validity;
- current final-review evidence and the existing final-delivery guard result;
- active workflow-bound reservations/runs; and
- current liveness state for no-progress comparison.

It does not use `WorkflowGraphSnapshot.overall_state` as authority. That DTO is
a projection. Resolver and projection may share lower-level evaluators, but
the Guard reads the durable facts directly.

### Frontier Rules

The resolver computes the current required frontier and partitions it into:

- platform actions that are enabled now;
- active runs that can still change the frontier;
- current durable attention that blocks a dependency; and
- inconsistent/dead-end facts that require reconciliation.

It then applies these rules:

1. Return `Delivered` only when the full current workflow is complete and the
   existing protocol-v2 final-delivery guard accepts the exact Final evidence
   artifact.
2. If at least one authorized root action is enabled now, return
   `RunnableAction`, even when an independent sibling path is still running or
   blocked.
3. Otherwise, if at least one current run can make progress without user
   input, return `WaitingForRuns`.
4. Otherwise, if current durable typed attention is the sole blocker, return
   `UserBlocked`.
5. Otherwise return `RunnableAction(kind = reconcile_frontier)`. A corrupt or
   unexplained dead end is never silently reclassified as user waiting.

Admission capacity and dependency constraints are part of enabled-action
calculation. A nominally ready node that cannot currently be admitted because
its required sibling is running may therefore produce `WaitingForRuns`.

### Delivered

`Delivered` requires more than every projected node looking completed. It
requires:

- every current required node to have terminal, valid, fresh, passing v2
  evidence;
- every required gate to have a current valid settlement;
- no current completion, artifact, Design self-review, child-question, or
  orchestration-stalled attention blocking delivery;
- no current required run in a non-terminal state; and
- `guard_final_delivery` to confirm that the workspace `HEAD` still equals the
  passing Final evidence artifact with the required clean state.

If final-delivery validation reopens Final review because of drift, the
resolver reloads the resulting revision and returns `RunnableAction`. It never
reports delivery for the pre-drift snapshot.

Implementation must expose the existing final-delivery validation as a
transaction-level operation used by the liveness transaction. Calling the
public wrapper in a nested, separately committed transaction would create a
drift/reload/checkpoint race and is not allowed.

### UserBlocked

`UserBlocked` requires at least one allowlisted, open, current, durable
attention request whose kind-specific CAS still matches the workflow frontier.
The initial allowlist is:

- `child_question` when the child cannot progress without the answer;
- `completion_decision`;
- `completion_artifact_recovery`;
- `design_self_review_decision`; and
- `orchestration_stalled` from this design.

An open request on a retired node, stale scope, superseded run, or old action
fingerprint does not block. A manifest `blocked` flag without corresponding
current durable attention is not enough. Parent text such as `Say continue`,
`Please confirm`, or `Waiting for instructions` is never enough.

If another independent action remains enabled, the resolver returns
`RunnableAction`; the UI may still show the attention, but it is not yet a
global workflow waiting point.

### WaitingForRuns

`WaitingForRuns` contains the ordered platform task IDs and generations of
current workflow-bound runs that can change the frontier. A child parked on a
user question is not progress-capable and instead contributes to
`UserBlocked`.

The decision has two effects:

1. current workflow children are excluded from clean Parent-turn teardown;
2. their existing Broker terminal/attention/graph-change events trigger a
   liveness re-evaluation.

No `delegation_continuations` row is manufactured. If an existing Join
continuation already owns the root turn, that continuation remains the sole
prompt-resume owner and this Guard is not entered for its suspended terminal.

### RunnableAction

`WorkflowActionDescriptorV1` is a bounded platform descriptor:

```text
version
workflow_id
active_manifest_revision
action_kind
phase_id?
gate_id?
gate_lineage?
review_round?
ordered_node_ids[]
ordered_latest_task_ids[]
```

Initial `action_kind` values are:

- `settle_ready_gate`;
- `dispatch_ready_work`;
- `apply_ready_recovery`;
- `finalize_delivery`; and
- `reconcile_frontier`.

The descriptor tells the hidden Parent why it was woken, but it is not an
authorization. The Parent must call `get_workflow_state` and use current
server-owned operation contracts.

`RunnableAction` may coexist with independent active siblings. Those runs are
recorded in the checkpoint's preservation scope even though they are not part
of the action descriptor.

### Action Fingerprint

The action fingerprint is:

```text
sha256("codeg.workflow-next-action.v1\0" + canonical_json(descriptor))
```

Canonical JSON uses sorted object keys, stable enum strings, and explicitly
ordered ID arrays. The fingerprint excludes:

- `graph_revision`;
- liveness/wake generation;
- Parent turn generation;
- prompt IDs and timestamps; and
- all model prose.

`graph_revision` is a separate concurrency CAS. Excluding it lets the Guard
detect logical non-progress even if an audit-only revision changes while the
same action remains required. A new run/task ID, gate lineage, review round,
frontier node, or action kind changes the fingerprint and resets the
no-progress counter.

Golden canonical-JSON vectors are shared by the resolver, store, outbox
dispatcher, prompt admission, and tests.

## Synchronous End-Turn Boundary

The Guard is inserted in the shared `finalize_turn_terminal` natural-end path,
after provider `end_turn` normalization and before both:

- ordinary `TurnComplete` lifecycle finalization; and
- `DelegationBroker::cancel_by_parent_turn`.

It must cover prompt-response terminals and extension/session-update terminals
through that one shared boundary. Adding checks only at an individual provider
adapter is insufficient.

The ordering is normative:

1. acquire the connection's prompt-arbitration lock;
2. verify root conversation, session, turn generation, and applicability;
3. resolve liveness from the current durable workflow snapshot;
4. transactionally CAS the liveness row, open/supersede attention if needed,
   and insert any `workflow_root_wake` outbox row;
5. commit;
6. emit `TurnComplete` with the platform workflow disposition;
7. apply filtered Parent-turn child teardown; and
8. allow the root-wake dispatcher to compete for prompt admission.

No ordinary clean finalization is emitted if steps 3 through 5 fail for an
applicable workflow.

### Turn Disposition

Provider `stop_reason` remains `end_turn`. It is diagnostic provider truth and
must not be overloaded with workflow state.

Add an optional platform-owned `WorkflowCheckpointV1` to `TurnComplete`:

```text
workflow_id
decision
graph_revision
liveness_version
automatic_continuation
```

The same level-triggered fields are projected from durable workflow liveness
for reload. Parent assistant content remains visible and unchanged. The hidden
user prompt is filtered separately.

| Decision | Turn treatment | Awaiting-reply token | Workflow children |
| --- | --- | --- | --- |
| `Delivered` | genuine workflow completion | existing root policy | none should remain |
| `UserBlocked` | durable user-attention checkpoint | typed attention policy | preserve only current blocked workflow runs |
| `WaitingForRuns` | automatic parked checkpoint | no | preserve current workflow runs |
| `RunnableAction` | automatic runnable checkpoint | no | preserve concurrent workflow runs; clean unrelated runs normally |

The existing conversation status enum need not gain a synthetic `automating`
value. A checkpoint may briefly be `pending_review` without an awaiting-reply
token; hidden prompt admission moves it back to `in_progress`. The workflow
liveness projection, not a guessed conversation status, drives the automatic
continuation indicator.

### Filtered Parent Teardown

The current clean `end_turn` path maps to `JoinAbandoned` and calls
`cancel_by_parent_turn`. That operation must accept the Guard's preservation
scope.

- Workflow-bound non-terminal runs named by any incomplete Guard checkpoint
  (`UserBlocked`, `WaitingForRuns`, or `RunnableAction`) are preserved.
- A `RunnableAction` can therefore advance an independent frontier without
  canceling a sibling that was already running when the Parent ended.
- Unrelated children retain existing cleanup semantics.
- A run that becomes terminal between resolution and teardown is harmlessly
  included in the preservation set and then re-evaluated by its terminal
  event.
- User Stop, explicit workflow termination, and abnormal Parent failure do not
  use this preservation exception.

Failure to load or validate the preservation scope fails the Guard closed; it
must not fall back to canceling all children after claiming a workflow
checkpoint.

## Persistence

### Workflow Liveness Row

Add one current row per workflow:

```text
delegation_workflow_liveness
  workflow_id                         TEXT PRIMARY KEY
  parent_conversation_id              INTEGER NOT NULL
  guard_version                       INTEGER NOT NULL
  attention_subject_id                TEXT NOT NULL UNIQUE
  decision                            TEXT NOT NULL CHECK(decision IN (
    'delivered','user_blocked','waiting_for_runs','runnable_action'
  ))
  wake_state                          TEXT NOT NULL CHECK(wake_state IN (
    'none','pending','admitted','preempted'
  ))
  observed_manifest_revision          INTEGER NOT NULL
  observed_graph_revision             INTEGER NOT NULL
  checkpoint_turn_generation          INTEGER NOT NULL
  decision_payload_json               TEXT NOT NULL
  action_kind                         TEXT NULL
  action_fingerprint                  TEXT NULL
  action_descriptor_json              TEXT NULL
  preserved_task_ids_json             TEXT NOT NULL
  wake_generation                     INTEGER NOT NULL DEFAULT 0
  no_progress_count                   INTEGER NOT NULL DEFAULT 0
  current_internal_prompt_id           TEXT NULL
  current_internal_prompt_marker       TEXT NULL
  current_outbox_event_id              TEXT NULL
  admitted_turn_generation             INTEGER NULL
  prompt_admitted_at                   TEXT NULL
  attention_request_id                 TEXT NULL
  version                              INTEGER NOT NULL DEFAULT 0
  created_at                           TEXT NOT NULL
  updated_at                           TEXT NOT NULL
  FOREIGN KEY(workflow_id)
    REFERENCES delegation_workflows(workflow_id) ON DELETE CASCADE
  FOREIGN KEY(parent_conversation_id)
    REFERENCES conversation(id) ON DELETE CASCADE
```

Required checks include:

- `no_progress_count` is between 0 and 2;
- `wake_generation`, revisions, turn generations, and `version` are
  non-negative and bounded by their Rust conversions;
- `runnable_action` requires an action kind, fingerprint, descriptor, and
  `wake_state` of `pending`, `admitted`, or `preempted`;
- non-runnable decisions require `wake_state = none`;
- `decision_payload_json` must match the closed schema for the selected
  decision and contains only the IDs/digests shown in
  `WorkflowLivenessDecisionV1`;
- `admitted` requires prompt ID, marker, admitted generation, and timestamp;
- a current `user_blocked` row names at least one validated attention in its
  bounded decision payload; and
- workflow/conversation ownership is revalidated in every writer rather than
  trusted from duplicated columns.

`decision_payload_json`, `action_descriptor_json`, and
`preserved_task_ids_json` contain platform IDs, digests, and enums only. They
are bounded to the protocol's maximum 100 manifest Tasks and 16 KiB per
serialized field.

The row is current state, not an immutable wake history. Immutable admitted
prompt markers and their admission outcome remain available in corresponding
outbox event rows so history filtering can validate older internal turns.

### Workflow Root Wake Outbox

Reuse `delegation_workflow_outbox_events` with:

```text
event_kind = workflow_root_wake
subject_key = wake:<wake_generation>:<action_fingerprint>
```

Add one nullable root-wake delivery projection to the generic outbox row:

```text
root_prompt_admission_outcome TEXT NULL CHECK(
  root_prompt_admission_outcome IS NULL OR
  root_prompt_admission_outcome IN (
    'admitted','already_admitted','superseded'
  )
)
```

Only `workflow_root_wake` may set this field. Other event kinds leave it null.

The bounded payload is:

```text
WorkflowRootWakeEventV1
  workflow_id
  parent_conversation_id
  expected_graph_revision
  expected_liveness_version
  active_manifest_revision
  action_kind
  action_fingerprint
  wake_generation
  internal_prompt_id
  internal_prompt_marker
```

The payload contains no workflow snapshot, report text, Parent text, child
text, task specification, or user prose.

The existing outbox uniqueness key plus `wake_generation` makes initial wake,
one no-progress retry, redelivery, and later action changes distinct and
idempotent. A dispatcher atomically sets `delivered_at` and the corresponding
admission outcome after prompt admission returns `Admitted`/`AlreadyAdmitted`,
or after a CAS proves the event stale and records `superseded` as a terminal
no-op. `PromptBusy` and parent-unavailable results leave both fields null.

History filtering accepts markers only from rows whose outcome is `admitted`
or `already_admitted`. A `superseded` event was never a hidden user turn and
cannot authorize filtering.

Startup and periodic reconciliation process undelivered root-wake rows. A
missed desktop/WebSocket event cannot lose the wake because the liveness row
and outbox remain level-triggered.

### Orchestration-Stalled Attention

Extend typed attention with `kind = orchestration_stalled`.

The existing attention table keeps its non-null legacy `task_id` column. This
kind stores the liveness row's platform-minted `attention_subject_id` there.
That ID is a workflow-level CAS subject, not a child task, and every delegate,
continue, replace, join, reply-to-child, and run-status API must reject it.

The payload is:

```text
OrchestrationStalledPayloadV1
  workflow_id
  parent_conversation_id
  action_kind
  action_fingerprint
  graph_revision
  liveness_version
  wake_generation
  no_progress_count
  reason_code
  bounded_diagnostics[]
```

It carries no raw prompt or assistant output. Its kind-specific CAS envelope is
the attention ID, subject ID, workflow ID, action fingerprint, graph revision,
wake generation, and liveness version.

Valid resolutions are:

- `user_retry_committed`;
- `superseded`;
- `workflow_terminated`; and
- `workflow_deleted`.

`user_retry_committed` requires authenticated ownership and, in the same
transaction, resolves attention, resets `no_progress_count`, increments wake
generation, records a new pending wake, and inserts its outbox event. Arbitrary
Parent prose cannot resolve this attention. A changed durable action
fingerprint supersedes it automatically.

The attention has no timeout. UI exposes a typed retry action and the existing
explicit workflow termination path.

### Migration Order

After the four `m20260804_...` completion-protocol migrations, add:

1. `m20260805_000001_workflow_liveness_guard`, creating the liveness table and
   indexes and adding the nullable root-prompt admission outcome to the outbox;
   and
2. `m20260805_000002_orchestration_stalled_attention`, transactionally
   rebuilding the attention kind check to include `orchestration_stalled`.

The second migration follows the completion design's explicit attention
rebuild checklist: preserve every prior column, row, check, foreign key, and
index; validate foreign keys; and roll back the entire migration on any copy,
schema, index, or validation failure.

Migration creates no liveness rows and scans no historical workflow. A row is
created only when an enabled Guard observes a new applicable clean end-turn.

## Root Prompt Admission

### Shared Admission Port

Extract a narrow `RootPromptAdmissionPort` from the connection manager's
existing continuation admission infrastructure. Both call sites share:

- the per-connection `prompt_lock`;
- root connection/conversation/session identity checks;
- turn-generation overflow checks;
- channel reservation before the no-await enqueue tail;
- `SessionState.turn_in_flight` arbitration;
- internal prompt origin/marker tracking; and
- exclusion from title capture, user-message capture, and mandatory route
  extraction.

They do not share durable state machines:

- child Join keeps using `delegation_continuations` and suspension leases;
- workflow liveness keeps using `delegation_workflow_liveness` and the workflow
  outbox.

`InternalPromptAdmission` becomes a tagged internal origin so a terminal turn
can be attributed to either a Join continuation or a workflow-root wake.

### Hidden Prompt Envelope

The internal user prompt is:

```text
<!-- codeg-internal-workflow-wake:<workflow_id>:<internal_prompt_id> -->
{"version":1,"workflow_id":"...","expected_graph_revision":42,
 "expected_liveness_version":7,"action_kind":"dispatch_ready_work",
 "action_fingerprint":"sha256:...","wake_generation":3}
```

The actual serialization is compact canonical JSON on one line. The marker and
all values are server-generated. The prompt contains only platform IDs, enum
codes, fingerprints, revisions, generation, and its marker.

The server-owned instruction for this origin requires the Parent to:

1. reload current workflow state;
2. treat embedded revisions as CAS, not evidence;
3. perform the next currently authorized B2D action;
4. stop for the user only when current durable attention exists; and
5. never continue or replace a child solely to repair completion formatting.

The prompt itself is filtered from normal conversation history only when its
exact marker matches an admitted durable root-wake event for that conversation.
A user-authored lookalike remains visible. Parent assistant output and tool
activity remain visible.

### User-Prompt Priority

Foreground user admission and internal root-wake admission use one arbiter.

- A foreground prompt announces intent before waiting on `prompt_lock`.
- An internal wake yields while a foreground waiter exists.
- If the user wins while a wake is still `pending`, admission CAS marks that
  wake `preempted` before admitting the user turn.
- The old outbox delivery then observes a stale liveness version and finishes
  as a no-op.
- The user's turn is Guarded normally at its clean end; if the action remains
  runnable, a fresh wake generation is enqueued.
- Once an internal prompt is durably admitted and `turn_in_flight` is set, a
  later user prompt retains the existing `TurnInProgress` behavior.

User Stop is stronger than foreground prompt priority: it fences pending and
admission-in-progress root wakes and prevents startup replay from reviving the
stopped epoch.

### Existing Wake Sources

`completion_decision_resolved`, Design-decision resolution, artifact recovery,
child terminal events, gate graph changes, and ordinary end-turn checks all
feed one `WorkflowRootWakeCoordinator`.

The adjudication outbox event remains the durable semantic event required by
the completion design. It no longer creates a parallel direct prompt. The
coordinator re-resolves liveness and coalesces all sources by current workflow,
action fingerprint, graph revision, and wake generation.

If a live `delegation_continuations` row already owns Parent resumption, the
workflow wake remains pending or is made stale by the resumed Parent's next
liveness evaluation. Two hidden Parent prompts are never admitted
concurrently.

## State Machine and No-Progress Fence

The principal transitions are:

```text
clean Parent end_turn
  -> Delivered ------------------------------> no wake
  -> UserBlocked ----------------------------> attention remains open
  -> WaitingForRuns -- child event ----------> re-resolve
  -> RunnableAction / pending -- admission --> admitted hidden turn
                                                |
                                                v
                                          clean end_turn
                                                |
                 +------------------------------+-------------------+
                 |                                                  |
          action changed                                    same action
          reset count, wake                         first: retry once
                                                   second: stalled attention
```

No-progress accounting applies only to a clean `end_turn` whose active turn
origin is an admitted `workflow_root_wake`.

- Initial external checkpoint: `no_progress_count = 0`, enqueue wake.
- First hidden clean end with the same action fingerprint: set count to 1 and
  enqueue exactly one retry with a new wake generation.
- Second hidden clean end with the same fingerprint: set count to 2, open one
  `orchestration_stalled` attention, and return `UserBlocked` without a wake.
- Any changed action fingerprint or non-runnable decision resets the counter.
- An external user turn does not consume the hidden no-progress budget.

A graph-revision change alone does not prove progress. Conversely, creating a
new run changes the descriptor's latest task ID and therefore proves a new
frontier even before that run completes.

## Concurrency and Idempotency

- The liveness row's `version` is the local CAS for every decision and wake
  transition.
- Workflow operations retain their existing `graph_revision`, gate lineage,
  review round, task generation, and evidence-scope CAS.
- A root wake must match both expected liveness version and expected graph
  revision before admission. A mismatch triggers re-evaluation, not delivery
  of stale instructions.
- The same `(workflow_id, wake_generation, action_fingerprint)` event is
  idempotent.
- Duplicate outbox delivery of an admitted marker returns
  `AlreadyAdmitted` or observes a stale event.
- Two terminal notifications for the same Parent turn cannot increment the
  no-progress counter twice because checkpoint turn generation is part of the
  liveness CAS.
- Child completion racing Parent finalization either appears in the resolver
  snapshot or bumps workflow state afterward and triggers another
  re-evaluation.
- User attention resolution and root wake creation occur in one transaction.
- Explicit workflow termination/deletion supersedes liveness, attention, and
  pending outbox delivery through one workflow-scoped CAS boundary.

## Crash and Restart Recovery

The relevant crash windows are closed as follows:

| Crash point | Recovery |
| --- | --- |
| Before liveness transaction commit | No checkpoint is claimed; existing terminal/session recovery may replay finalization |
| After liveness/outbox commit, before `TurnComplete` | Startup sees the current row and pending event; it reconciles the ended root turn before admission |
| After `TurnComplete`, before root admission | Undelivered outbox and `wake_state = pending` retry |
| After prompt enqueue, before outbox delivered stamp | Redelivery returns `AlreadyAdmitted` and records an admitted outcome, or proves a stale no-op |
| During uncertain admitted prompt delivery | Reconcile marker, live turn state, and terminal history; if no start is provable, return the same wake generation to pending |
| While `WaitingForRuns` | Startup registers event re-evaluation only for existing waiting liveness rows |
| With open stalled attention | Attention and `UserBlocked` projection remain level-triggered |

Uncertain prompt recovery is at-least-once, not exactly-once. A duplicate
hidden turn is safe because prompt admission is serialized and every workflow
mutation still requires current graph/gate/task CAS. The hidden prompt never
carries authority that could make a stale duplicate pass.

Startup recovery scans only:

- existing non-terminal liveness rows;
- undelivered `workflow_root_wake` outbox events; and
- open `orchestration_stalled` attention.

It does not search all historical manifests for incompleteness and does not
auto-wake a session that never entered the Guard contract.

A `preempted` liveness row is not admitted automatically during startup. Its
old outbox event is closed as `superseded`; a later foreground clean turn or an
authenticated typed retry must create the next wake generation. This preserves
foreground preemption and User Stop across process restart.

## Error Contract

Stable diagnostics include:

| Code | Meaning | Response |
| --- | --- | --- |
| `workflow_liveness_state_invalid` | Durable frontier cannot be consistently reduced | Use `reconcile_frontier`; after the bounded no-progress path, open stalled attention |
| `workflow_liveness_guard_failed` | Resolver or transaction infrastructure failed before checkpoint commit | Do not emit ordinary clean completion; release the turn as a platform error and preserve workflow children |
| `workflow_root_wake_stale` | Revision, fingerprint, generation, or liveness CAS changed | Mark event a terminal no-op and re-evaluate current state |
| `workflow_root_wake_busy` | Another turn currently owns the root | Leave event pending and retry with bounded backoff |
| `workflow_root_unavailable` | Root connection/session cannot currently admit | Leave durable wake pending for reconnect/startup recovery |
| `workflow_orchestration_no_progress` | One hidden turn returned the same action | Enqueue the single allowed retry |
| `workflow_orchestration_stalled` | Two hidden turns returned the same action | Open durable typed attention and stop self-wake |

A resolver, serialization, liveness CAS, or outbox insert failure cannot be
converted into `Delivered`, `UserBlocked`, or an ordinary successful Parent
completion. The platform clears the in-memory turn fence through an explicit
Guard-failure disposition, emits a visible error, preserves current workflow
children, and leaves the durable workflow facts unchanged for manual retry.

Outbox publication or UI event failure after commit does not fail the committed
checkpoint. Recovery is driven by the durable row.

Logs contain IDs, enum codes, revision numbers, retry counts, and digest
prefixes only. They do not contain hidden prompt JSON, Parent prose, child
output, report contents, or user text.

## User Interaction

The workflow overlay adds a bounded liveness projection:

```text
WorkflowLivenessProjectionV1
  state: delivered | needs_input | waiting_for_runs |
         continuing | stalled | guard_error
  automatic_continuation: boolean
  updated_at
  attention_summary?
```

Expected presentation:

- `RunnableAction/pending|admitted`: show an automatic-continuation state;
- `WaitingForRuns`: show the existing waiting-for-agents state;
- `UserBlocked`: show the typed attention action;
- `orchestration_stalled`: show a concise failure state with Retry and
  workflow termination actions; and
- `Delivered`: show normal workflow completion.

The Parent's checkpoint text remains in the conversation. Codeg does not append
instructions such as `say continue`, alter the assistant response, or expose
the hidden prompt. A disconnected UI reconstructs the same state from the
workflow graph/liveness projection rather than relying on a toast.

Desktop and server runtimes expose identical DTOs, events, retry operations,
and attention authorization.

## Security and Bounds

- Only the server can create a liveness row, action descriptor, fingerprint,
  wake event, internal prompt marker, or stalled-attention subject.
- Root ownership is derived from the authenticated connection and workflow
  header, never request fields.
- The hidden wake is not an MCP tool exposed to the Parent or child.
- A marker is filtered only after durable admitted-marker validation; XML-like
  user text is retained.
- The stalled retry operation requires authenticated ownership and its full
  liveness/action CAS.
- Platform-only attention subject IDs are rejected by all child-run APIs.
- Descriptor arrays are bounded by the 100-Task protocol limit and serialized
  descriptor/event payloads are capped at 16 KiB.
- Canonical hashing uses a domain/version separator and shared golden vectors.
- No raw text or absolute path is persisted in liveness or root-wake payloads.

## Rollout and Rollback

Add an independently configurable liveness mode:

| Mode | Behavior |
| --- | --- |
| `off` | Existing behavior; no resolver or writes |
| `shadow` | Resolve and emit metrics only; no checkpoint, preservation, state, or wake writes |
| `enforce` | Full synchronous Guard and wake behavior |

Mode applies only to protocol-v2 B2D clean end-turns. A workflow gets durable
Guard ownership when its first enforce-mode checkpoint row is created. Normal
configuration rollback affects workflows that do not yet have a liveness row;
existing rows continue recovery so rollback cannot strand a committed pending
wake.

An explicit emergency dispatch kill switch may stop new prompt admission for
existing rows, but it does not delete them. UI then shows automatic
continuation paused, and operators can inspect or resume the same durable wake.

Rollout stages are:

1. ship additive migrations and shadow resolver metrics;
2. enable enforce for canary agent/profile cohorts on newly observed clean
   end-turns;
3. expand to all protocol-v2 B2D roots; and
4. remove the ordinary incomplete-clean-end path after invariant counters stay
   at zero.

Expansion stops on any silent-liveness invariant violation, harmful duplicate
workflow action, Guard-caused child cancellation, or sustained root-admission
failure. Migration and shadow mode never wake historical sessions.

## Expected Implementation Boundary

Backend work:

- add focused `workflow/liveness.rs` and `workflow/liveness_store.rs` modules;
- add SeaORM liveness entity and the two ordered migrations;
- extend typed attention with the kind-specific `orchestration_stalled`
  lifecycle and authenticated retry operation;
- add `WorkflowRootWakeCoordinator` and route adjudication/child/graph events
  through it;
- extract `RootPromptAdmissionPort` and tagged internal-prompt origin from the
  existing connection manager/continuation admission code;
- intercept the shared root natural-end path before ordinary finalization and
  Broker teardown;
- add filtered `cancel_by_parent_turn` preservation for current workflow runs;
- extend `TurnComplete` and workflow projection with bounded checkpoint
  metadata;
- generalize internal prompt filtering to admitted workflow-root markers;
- extend desktop, Axum, WebSocket, history, and `codeg-mcp` projections with
  the same liveness truth; and
- add structured metrics and startup reconciliation.

Frontend and Skill work:

- mirror the liveness/checkpoint/attention DTOs in TypeScript;
- render continuing, waiting, needs-input, stalled, guard-error, and delivered
  states in the existing workflow surface;
- add the typed stalled retry action with desktop/server parity;
- update all locale files;
- update the B2D Skill to treat Parent prose as non-authoritative and forbid
  `Say continue` as a workflow waiting contract; and
- add validator fixtures requiring automatic advance unless durable attention
  exists.

The Skill update aligns model behavior but is not the correctness boundary.
The platform Guard remains mandatory even when the Parent ignores the Skill.

Unrelated standalone delegation recovery, workflow graph layout, model choice,
and non-v2 completion behavior are outside this slice.

## Test Strategy

### Resolver Unit Tests

- Golden decision table for `Delivered`, `UserBlocked`, `WaitingForRuns`, and
  every initial `RunnableAction` kind.
- A session-3023 fixture with Task 3 complete and Tasks 4-12 unstarted resolves
  to `dispatch_ready_work`, not user blocking.
- Parent output containing `Say continue` does not affect the decision.
- Stale attention, retired-node attention, and prose-only blockers do not
  produce `UserBlocked`.
- Independent runnable work wins over sibling attention or running work.
- A progress-capable run produces `WaitingForRuns`; a child parked on a current
  question produces `UserBlocked`.
- Coarse UI `overall_state` cannot override invalid/fresh v2 evidence.
- Full completion requires the exact final-delivery artifact; Final drift
  reopens review and resolves runnable.
- Invalid/dead-end state resolves `reconcile_frontier`, never silent waiting.
- Shared canonical action vectors produce identical fingerprints in all
  consumers; graph revision, timestamp, and prose changes do not change them.

### End-Turn Integration Tests

- Every root terminal source reaches the shared Guard exactly once.
- v1, v2-shadow, observed-only, non-B2D, delegate, and suspended-Join turns
  retain existing behavior.
- Clean applicable `end_turn` persists liveness/outbox before `TurnComplete`.
- Inject resolver, transaction, CAS, serialization, and outbox failures and
  prove ordinary clean completion is not emitted.
- `RunnableAction` keeps Parent output visible, emits checkpoint metadata,
  suppresses awaiting-reply generation, and queues one wake.
- `WaitingForRuns` preserves workflow children while unrelated children retain
  existing teardown.
- Current user-blocked children survive checkpoint teardown.
- User Stop, refusal, max-token, max-turn-requests, empty output, disconnect,
  and connection failure retain existing semantics and do not auto-wake.

### Wake and Race Tests

- Duplicate outbox dispatch admits at most one concurrent root turn and is
  otherwise `AlreadyAdmitted`/stale.
- Prompt lock, root identity, session ID, graph revision, liveness version,
  action fingerprint, and wake generation all fence admission.
- A queued foreground user prompt preempts a pending hidden wake; an already
  admitted hidden turn keeps existing `TurnInProgress` behavior.
- User Stop wins against pending, lock-waiting, CAS-in-progress, and
  post-admission wake races.
- `completion_decision_resolved` plus child terminal plus ordinary end-turn
  coalesce to one current action wake.
- An active Join continuation and a workflow wake never admit two Parent
  prompts.
- A child terminal race either changes the first snapshot or causes immediate
  post-commit re-evaluation.
- Forged/unadmitted internal markers remain visible; every historical admitted
  workflow marker is filtered.

### No-Progress Tests

- Initial checkpoint enqueues attempt zero without counting non-progress.
- First same-fingerprint hidden end enqueues exactly one retry.
- Second same-fingerprint hidden end opens exactly one
  `orchestration_stalled` attention and no wake.
- A changed node/task/gate/action fingerprint resets the counter.
- A graph-revision-only change does not evade the fence.
- External user turns do not consume the hidden retry budget.
- Authenticated typed retry resolves attention and creates one new wake;
  stale/different retries conflict.

### Crash and Restart Tests

- Crash after liveness commit and before `TurnComplete`.
- Crash after `TurnComplete` and before outbox dispatch.
- Crash after prompt enqueue and before delivered stamp.
- Restart with pending, admitted-but-unproven, waiting-for-runs, user-blocked,
  stalled, delivered, preempted, and stale wake states.
- Startup only reconciles workflows with liveness rows and never wakes an
  arbitrary historical incomplete manifest.
- Missed desktop/server events recover from the level-triggered DTO.

### Persistence and Migration Tests

- Upgrade every supported completion-v2 schema through both new migrations.
- Preserve every existing attention row/index/check/FK byte-for-byte through
  the attention rebuild.
- Inject copy, schema, index, and foreign-key-check failures and prove full
  rollback.
- Enforce one liveness row per workflow, stable platform attention subject,
  legal decision/wake combinations, bounded JSON, marker uniqueness,
  root-prompt admission-outcome legality, and monotonically increasing CAS
  fields.
- Deleting a workflow/conversation cascades liveness and closes/supersedes
  attention/outbox behavior deterministically.

### End-to-End Tests

- Reproduce session 3023: Task 3 finishes, Parent says Tasks 4-12 remain and
  cleanly ends, a hidden wake is admitted, and Task 4 begins without user
  input.
- Continue the fixture through all remaining Task gates and Final delivery.
- Exercise a current completion decision: no wake until adjudication, then one
  liveness-coordinated root wake.
- Exercise active children, child attention, final artifact drift, one
  no-progress retry, and stalled user recovery.
- Desktop and server show identical checkpoint/liveness states and never show
  hidden prompt text.
- Non-v2 conversations and ordinary chat retain their existing lifecycle.

### Repository Verification

Implementation requires focused suites plus the repository checks from
`AGENTS.md`:

```powershell
pnpm eslint .
pnpm test
pnpm build

Set-Location src-tauri
cargo check
cargo test --features test-utils
cargo clippy --all-targets --features test-utils -- -D warnings

cargo check --no-default-features --features server --bin codeg-server
cargo test --no-default-features --features server --bin codeg-server --lib
cargo clippy --no-default-features --features server --bin codeg-server --lib -- -D warnings

cargo check --no-default-features --bin codeg-mcp
cargo clippy --no-default-features --bin codeg-mcp -- -D warnings
```

## Observability

Add bounded counters/histograms for:

- Guard applicability and decision by action kind;
- incomplete clean end-turns intercepted;
- prose-only waiting claims ignored;
- checkpoint transaction success/failure/latency;
- workflow child preservation count and any preservation failure;
- root wakes enqueued, delivered, already admitted, busy, stale, preempted,
  unavailable, retried, and recovered after restart;
- time from checkpoint to hidden prompt admission and to action-fingerprint
  change;
- WaitingForRuns age and terminal-event re-evaluation latency;
- no-progress first retries and stalled attention opened/resolved;
- pending outbox oldest age and liveness-row oldest non-terminal age;
- shadow/enforce decision differences; and
- the forbidden quiescent-state invariant.

Structured logs include workflow/conversation IDs, decision/action enum,
fingerprint prefix, graph/liveness revisions, wake generation, no-progress
count, and stable failure code.

Primary rollout indicators are:

- number of incomplete v2 clean end-turns caught;
- median checkpoint-to-next-action latency;
- oldest pending root wake;
- root admission failure rate;
- stalled-attention rate by agent/profile;
- duplicate admitted root-turn rate; and
- number of Guard-preserved children later canceled incorrectly.

The forbidden quiescent-state counter and harmful duplicate-action counter must
remain zero in enforce mode.

## Acceptance Criteria

1. The Guard applies only to manifest-backed completion-protocol-v2 B2D root
   clean `end_turn`; all excluded protocols, conversations, suspensions, and
   abnormal terminal reasons retain existing semantics.
2. An applicable clean end-turn cannot complete ordinary finalization until a
   current `Delivered`, `UserBlocked`, `WaitingForRuns`, or `RunnableAction`
   decision is durably committed.
3. `Delivered` requires complete fresh v2 evidence, current settlements, no
   blocker/run, and exact existing Final-delivery artifact validation.
4. Only current allowlisted durable attention can establish `UserBlocked`.
   Parent prose, including `Say continue`, cannot.
5. `WaitingForRuns` preserves current workflow children, cleans unrelated
   children normally, and is re-evaluated from durable child events without
   manufacturing a Join continuation.
6. `RunnableAction` commits a checkpoint and unique `workflow_root_wake`
   before Parent finalization, then admits a hidden Parent turn without user
   input.
7. The Guard and hidden prompt never directly dispatch, settle, continue,
   replace, recover, or adjudicate; the Parent reloads state and uses normal
   authorized operations and CAS.
8. Provider `stop_reason = end_turn` remains intact while the platform exposes
   a distinct bounded workflow-checkpoint disposition.
9. Parent output remains visible, exact admitted hidden prompts remain hidden,
   and forged marker-like user text remains visible.
10. Liveness state is one current row per workflow; immutable outbox rows and
    explicit admitted/already-admitted/superseded outcomes make root wake
    delivery and historical marker validation at-least-once and restart-safe.
11. Action fingerprinting excludes graph revision/prose/timestamps, includes
    logical frontier identity, and passes shared golden vectors.
12. A first same-action hidden clean end retries once; a second opens exactly
    one durable `orchestration_stalled` attention and stops self-wake.
13. Foreground user prompts preempt pending hidden wakes, admitted wakes remain
    serialized, and User Stop prevents later replay of the stopped epoch.
14. Adjudication, child terminal, graph-change, and ordinary end-turn wake
    sources coalesce through one coordinator; active Join continuation never
    races a second root prompt.
15. Resolver/persistence/outbox failure cannot be reported as successful clean
    workflow completion and cannot silently cancel guarded workflow children.
16. Startup recovers existing liveness/outbox/attention rows but never scans
    and wakes arbitrary historical incomplete workflows.
17. Session-3023 reproduction advances from completed Task 3 to Task 4
    automatically and proceeds through Tasks 4-12 without `Say continue`.
18. Desktop, server, WebSocket/history, and `codeg-mcp` expose the same durable
    liveness truth and pass the required verification suites.
